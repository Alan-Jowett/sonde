// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * veml7700_sensor — read a Vishay VEML7700 ambient light sensor over I2C.
 *
 * The VEML7700 exposes 16-bit word registers over I2C at fixed 7-bit address
 * 0x10. Per the datasheet (`84286`), register words are transferred little-
 * endian on the bus (low byte first, then high byte).
 *
 * This program uses two sensitivity profiles with hysteresis:
 *   - Normal profile: ALS gain x1, ALS integration time 100 ms
 *   - Low-light profile: ALS gain x2, ALS integration time 800 ms
 *
 * A single-entry array map keeps the three most recent lux readings and the
 * currently selected profile across wake cycles. Before each measurement, the
 * program averages the stored readings:
 *   - if avg < 200 mLux, switch to the low-light profile
 *   - if avg > 1000 mLux, switch back to the normal profile
 *
 * This adds hysteresis so the program does not flap around the threshold, and
 * it uses the high-sensitivity profile only when recent readings justify it.
 *
 * Each wake cycle:
 *   1. Choose the profile from the rolling average of the last three readings.
 *   2. Write ALS_CONF_0 for the chosen profile.
 *   3. Wait slightly longer than the selected integration time.
 *   4. Read the ALS and WHITE 16-bit result registers.
 *   5. Convert the ALS count to millilux using the selected profile:
 *        x1 / 100 ms: lux_ml = als_counts * 57.6 mLux/count
 *        x2 / 800 ms: lux_ml = als_counts * 3.6 mLux/count
 *   6. Clamp the reported lux value to a minimum of 1 mLux.
 *   7. Store the new reading in the three-sample rolling history.
 *
 * Payload (18 bytes, queued with send_async):
 *   [0..7]   timestamp (little-endian u64, ms since epoch)
 *   [8..9]   ALS_CONF_0 word written to the device (little-endian u16)
 *   [10..11] ALS counts from register 0x04 (little-endian u16)
 *   [12..13] WHITE counts from register 0x05 (little-endian u16)
 *   [14..17] lux_ml (little-endian u32)
 */

#include "include/sonde_helpers.h"

/* Bus 0, fixed VEML7700 7-bit I2C address 0x10. */
#define VEML7700_HANDLE I2C_HANDLE(0, 0x10)

#define VEML7700_REG_ALS_CONF_0 0x00u
#define VEML7700_REG_ALS        0x04u
#define VEML7700_REG_WHITE      0x05u

/* gain x1, integration time 100 ms, interrupt disabled, ALS power on */
#define VEML7700_ALS_CONF_0_DEFAULT  0x0000u
/* gain x2, integration time 800 ms, interrupt disabled, ALS power on */
#define VEML7700_ALS_CONF_0_LOWLIGHT 0x08C0u
#define VEML7700_ALS_CONF_0_SHUTDOWN 0x0001u

#define VEML7700_LUX_LOW_ML  200u
#define VEML7700_LUX_HIGH_ML 1000u

/* Wait slightly longer than the selected integration time. */
#define VEML7700_CONVERSION_US_DEFAULT  120000u
#define VEML7700_CONVERSION_US_LOWLIGHT 900000u

struct veml7700_state {
    __u32 recent_lux_ml[3];
    __u16 current_conf;
    __u8 recent_count;
    __u8 recent_index;
};

typedef char veml7700_state_size_must_be_16[(sizeof(struct veml7700_state) == 16u) ? 1 : -1];

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct veml7700_state);
} state_map SEC(".maps");

/* Set to 1 while debugging sensor bring-up to enable error trace logs. */
#ifndef VEML7700_ENABLE_TRACE
#define VEML7700_ENABLE_TRACE 0
#endif

#if VEML7700_ENABLE_TRACE
#define VEML7700_TRACE(msg)                                                    \
    do {                                                                       \
        char _trace[] = msg;                                                   \
        bpf_trace_printk(_trace, (__u32)(sizeof(_trace) - 1));                 \
    } while (0)
#else
#define VEML7700_TRACE(msg) do { } while (0)
#endif

static int veml7700_write_word(__u8 reg, __u16 value)
{
    __u8 buf[3];

    buf[0] = reg;
    buf[1] = (__u8)value;
    buf[2] = (__u8)(value >> 8);
    return i2c_write(VEML7700_HANDLE, buf, sizeof(buf));
}

static int veml7700_read_word(__u8 reg, __u16 *value_out)
{
    __u8 raw[2];
    int rc = i2c_write_read(VEML7700_HANDLE, &reg, 1, raw, sizeof(raw));

    if (rc < 0)
        return rc;

    *value_out = (__u16)((__u16)raw[0] | ((__u16)raw[1] << 8));
    return 0;
}

static __u32 veml7700_conversion_us(__u16 conf)
{
    if (conf == VEML7700_ALS_CONF_0_LOWLIGHT)
        return VEML7700_CONVERSION_US_LOWLIGHT;

    return VEML7700_CONVERSION_US_DEFAULT;
}

static __u32 veml7700_counts_to_lux_ml(__u16 conf, __u16 als_counts)
{
    __u32 lux_ml;

    if (conf == VEML7700_ALS_CONF_0_LOWLIGHT)
        lux_ml = (__u32)als_counts * 36u / 10u;
    else
        lux_ml = (__u32)als_counts * 576u / 10u;

    if (lux_ml == 0u)
        return 1u;

    return lux_ml;
}

static __u16 veml7700_select_conf(const struct veml7700_state *state)
{
    __u16 conf = state->current_conf;

    if (state->recent_count == 0) {
        if (conf == VEML7700_ALS_CONF_0_LOWLIGHT)
            return conf;
        return VEML7700_ALS_CONF_0_DEFAULT;
    }

    __u32 sum = state->recent_lux_ml[0];
    if (state->recent_count >= 2u)
        sum += state->recent_lux_ml[1];
    if (state->recent_count >= 3u)
        sum += state->recent_lux_ml[2];

    __u32 avg = sum / (__u32)state->recent_count;
    if (avg < VEML7700_LUX_LOW_ML)
        conf = VEML7700_ALS_CONF_0_LOWLIGHT;
    else if (avg > VEML7700_LUX_HIGH_ML)
        conf = VEML7700_ALS_CONF_0_DEFAULT;
    else if (conf != VEML7700_ALS_CONF_0_LOWLIGHT)
        conf = VEML7700_ALS_CONF_0_DEFAULT;

    return conf;
}

static void veml7700_record_lux_ml(struct veml7700_state *state, __u32 lux_ml)
{
    __u8 index = state->recent_index;

    if (index >= 3u)
        index = 0u;
    state->recent_index = index;

    state->recent_lux_ml[index] = lux_ml;
    if (index >= 2u)
        state->recent_index = 0u;
    else
        state->recent_index = index + 1u;
    if (state->recent_count < 3u)
        state->recent_count += 1u;
}

SEC("sonde")
int program(struct sonde_context *ctx)
{
    __u32 state_key = 0;
    struct veml7700_state *state = map_lookup_elem(&state_map, &state_key);
    __u16 conf;
    int rc;

    if (!state) {
        VEML7700_TRACE("veml7700: state map lookup failed\n");
        return 0;
    }

    conf = veml7700_select_conf(state);
    state->current_conf = conf;

    rc = veml7700_write_word(VEML7700_REG_ALS_CONF_0, conf);
    if (rc < 0) {
        VEML7700_TRACE("veml7700: config write failed\n");
        return 0;
    }

    rc = delay_us(veml7700_conversion_us(conf));
    if (rc < 0) {
        VEML7700_TRACE("veml7700: delay failed\n");
        return 0;
    }

    __u16 als_counts = 0;
    rc = veml7700_read_word(VEML7700_REG_ALS, &als_counts);
    if (rc < 0) {
        VEML7700_TRACE("veml7700: als read failed\n");
        return 0;
    }

    __u16 white_counts = 0;
    rc = veml7700_read_word(VEML7700_REG_WHITE, &white_counts);
    if (rc < 0) {
        VEML7700_TRACE("veml7700: white read failed\n");
        return 0;
    }

    __u32 lux_ml = veml7700_counts_to_lux_ml(conf, als_counts);
    veml7700_record_lux_ml(state, lux_ml);
    __u8 payload[18];
    __u64 ts = ctx->timestamp;

    payload[0]  = (__u8)(ts);
    payload[1]  = (__u8)(ts >> 8);
    payload[2]  = (__u8)(ts >> 16);
    payload[3]  = (__u8)(ts >> 24);
    payload[4]  = (__u8)(ts >> 32);
    payload[5]  = (__u8)(ts >> 40);
    payload[6]  = (__u8)(ts >> 48);
    payload[7]  = (__u8)(ts >> 56);

    payload[8]  = (__u8)(conf);
    payload[9]  = (__u8)(conf >> 8);
    payload[10] = (__u8)(als_counts);
    payload[11] = (__u8)(als_counts >> 8);
    payload[12] = (__u8)(white_counts);
    payload[13] = (__u8)(white_counts >> 8);
    payload[14] = (__u8)(lux_ml);
    payload[15] = (__u8)(lux_ml >> 8);
    payload[16] = (__u8)(lux_ml >> 16);
    payload[17] = (__u8)(lux_ml >> 24);
    rc = veml7700_write_word(VEML7700_REG_ALS_CONF_0, VEML7700_ALS_CONF_0_SHUTDOWN);
    if (rc < 0) {
        VEML7700_TRACE("veml7700: shutdown failed\n");
    }

    rc = send_async(payload, sizeof(payload));
    if (rc == -1)
        send(payload, sizeof(payload));

    return 0;
}

/**
 * Decoder: parse the 18-byte VEML7700 APP_DATA payload and emit named readings.
 *
 * Payload layout (from the sonde program above):
 *   [0..7]   timestamp (little-endian u64, ms since epoch)
 *   [8..9]   ALS_CONF_0 word written to the device (little-endian u16)
 *   [10..11] ALS counts (little-endian u16)
 *   [12..13] WHITE counts (little-endian u16)
 *   [14..17] lux_ml (little-endian u32)
 */
/*
 * NOTE: The ctx->input_data dereference pattern below requires sonde-bpf
 * data/data_end pointer tagging (not yet implemented). Until then, decoders
 * must access the blob via R1 bounded offsets at runtime. This source file
 * documents the intended ABI; hand-crafted bytecode is used for gateway tests.
 */
#ifndef SONDE_DISABLE_DECODER
SEC("decoder")
int decode(struct decoder_context *ctx)
{
    const __u8 *data = (const __u8 *)(__u64)ctx->input_data;
    const __u8 *data_end = (const __u8 *)(__u64)ctx->input_end;

    if (data + 18 > data_end)
        return 0;

    __u16 als_conf = (__u16)(
        (__u16)data[8] |
        ((__u16)data[9] << 8)
    );
    __u16 als_counts = (__u16)(
        (__u16)data[10] |
        ((__u16)data[11] << 8)
    );
    __u16 white_counts = (__u16)(
        (__u16)data[12] |
        ((__u16)data[13] << 8)
    );
    __u32 lux_ml = (__u32)(
        (__u32)data[14] |
        ((__u32)data[15] << 8) |
        ((__u32)data[16] << 16) |
        ((__u32)data[17] << 24)
    );

    char name_conf[] = "als_conf";
    emit_reading(name_conf, 8, (__s64)als_conf);

    char name_als[] = "als_counts";
    emit_reading(name_als, 10, (__s64)als_counts);

    char name_white[] = "white_counts";
    emit_reading(name_white, 12, (__s64)white_counts);

    char name_lux[] = "lux_ml";
    emit_reading(name_lux, 6, (__s64)lux_ml);

    return 0;
}
#endif
