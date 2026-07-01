// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * veml7700_sensor — read a Vishay VEML7700 ambient light sensor over I2C.
 *
 * The VEML7700 exposes 16-bit word registers over I2C at fixed 7-bit address
 * 0x10. Per the datasheet (`84286`), register words are transferred little-
 * endian on the bus (low byte first, then high byte).
 *
 * This program uses a ten-band autoranging ladder. Each band is defined by a
 * gain/integration-time pair, ordered from most sensitive to least sensitive:
 *   - x2 / 800 ms  -> 3.6 mLux/count
 *   - x2 / 400 ms  -> 7.2 mLux/count
 *   - x2 / 200 ms  -> 14.4 mLux/count
 *   - x2 / 100 ms  -> 28.8 mLux/count
 *   - x2 / 50 ms   -> 57.6 mLux/count
 *   - x2 / 25 ms   -> 115.2 mLux/count
 *   - x1 / 25 ms   -> 230.4 mLux/count
 *   - x1/4 / 50 ms -> 460.8 mLux/count
 *   - x1/4 / 25 ms -> 921.6 mLux/count
 *   - x1/8 / 25 ms -> 1843.2 mLux/count
 *
 * That spans the datasheet endpoints from 3.6 mLux/count (x2 / 800 ms) up to
 * 1843.2 mLux/count (x1/8 / 25 ms), for an effective full-scale range of about
 * 0.0036 lux to 120.8 klux.
 *
 * A single-entry array map keeps the current band across wake cycles. After
 * each measurement, the program uses the larger of ALS and WHITE counts to
 * choose the band for the next wake cycle:
 *   - if either count exceeds 75% of full scale, switch one band up
 *   - if both counts are below 25% of full scale, switch one band down
 *
 * Each wake cycle:
 *   1. Select the current autorange band.
 *   2. Write ALS_CONF_0 for that band.
 *   3. Wait slightly longer than the selected integration time.
 *   4. Read the ALS and WHITE 16-bit result registers.
 *   5. Convert the ALS count to millilux using the selected band.
 *   6. Clamp the reported lux value to a minimum of 1 mLux.
 *   7. Persist the selected band for the next wake cycle.
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

#define VEML7700_ALS_CONF_0_SHUTDOWN 0x0001u

/* Raw-count hysteresis thresholds. */
#define VEML7700_COUNTS_LOW_MAX   0x3FFFu
#define VEML7700_COUNTS_HIGH_MIN  0xC000u

enum {
    VEML7700_BAND_X2_800 = 0,
    VEML7700_BAND_X2_400 = 1,
    VEML7700_BAND_X2_200 = 2,
    VEML7700_BAND_X2_100 = 3,
    VEML7700_BAND_X2_50  = 4,
    VEML7700_BAND_X2_25  = 5,
    VEML7700_BAND_X1_25  = 6,
    VEML7700_BAND_Q4_50  = 7,
    VEML7700_BAND_Q4_25  = 8,
    VEML7700_BAND_Q8_25  = 9,
    VEML7700_BAND_COUNT  = 10,
    VEML7700_BAND_DEFAULT = VEML7700_BAND_X2_50,
};

struct veml7700_state {
    __u8 current_band;
    __u8 initialized;
    __u16 reserved;
};

typedef char veml7700_state_size_must_be_4[(sizeof(struct veml7700_state) == 4u) ? 1 : -1];

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

static __u8 veml7700_select_band(const struct veml7700_state *state)
{
    if (state->initialized == 0u)
        return VEML7700_BAND_DEFAULT;
    if (state->current_band >= VEML7700_BAND_COUNT)
        return VEML7700_BAND_DEFAULT;

    return state->current_band;
}

static __u16 veml7700_band_conf(__u8 band)
{
    switch (band) {
    case VEML7700_BAND_X2_800:
        return 0x08C0u;
    case VEML7700_BAND_X2_400:
        return 0x0880u;
    case VEML7700_BAND_X2_200:
        return 0x0840u;
    case VEML7700_BAND_X2_100:
        return 0x0800u;
    case VEML7700_BAND_X2_50:
        return 0x0A00u;
    case VEML7700_BAND_X2_25:
        return 0x0B00u;
    case VEML7700_BAND_X1_25:
        return 0x0300u;
    case VEML7700_BAND_Q4_50:
        return 0x1A00u;
    case VEML7700_BAND_Q4_25:
        return 0x1B00u;
    default:
        return 0x1300u;
    }
}

static __u32 veml7700_conversion_us(__u8 band)
{
    switch (band) {
    case VEML7700_BAND_X2_800:
        return 900000u;
    case VEML7700_BAND_X2_400:
        return 500000u;
    case VEML7700_BAND_X2_200:
        return 250000u;
    case VEML7700_BAND_X2_100:
        return 120000u;
    case VEML7700_BAND_X2_50:
    case VEML7700_BAND_Q4_50:
        return 70000u;
    default:
        return 40000u;
    }
}

static __u32 veml7700_counts_to_lux_ml(__u8 band, __u16 als_counts)
{
    __u32 lux_ml = (__u32)als_counts * 36u;

    lux_ml <<= band;
    lux_ml /= 10u;

    if (lux_ml == 0u)
        return 1u;

    return lux_ml;
}

static __u8 veml7700_adjust_band(__u8 band, __u16 als_counts, __u16 white_counts)
{
    __u16 peak_counts = als_counts;

    if (white_counts > peak_counts)
        peak_counts = white_counts;

    if (peak_counts >= VEML7700_COUNTS_HIGH_MIN) {
        if (band + 1u < VEML7700_BAND_COUNT)
            return band + 1u;
        return band;
    }
    if (peak_counts <= VEML7700_COUNTS_LOW_MAX) {
        if (band > 0u)
            return band - 1u;
    }

    return band;
}

SEC("sonde")
int program(struct sonde_context *ctx)
{
    __u32 state_key = 0;
    struct veml7700_state *state = map_lookup_elem(&state_map, &state_key);
    __u8 band;
    __u16 conf;
    int rc;

    if (!state) {
        VEML7700_TRACE("veml7700: state map lookup failed\n");
        return 0;
    }

    band = veml7700_select_band(state);
    conf = veml7700_band_conf(band);

    rc = veml7700_write_word(VEML7700_REG_ALS_CONF_0, conf);
    if (rc < 0) {
        VEML7700_TRACE("veml7700: config write failed\n");
        return 0;
    }

    rc = delay_us(veml7700_conversion_us(band));
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

    __u32 lux_ml = veml7700_counts_to_lux_ml(band, als_counts);
    state->current_band = veml7700_adjust_band(band, als_counts, white_counts);
    state->initialized = 1u;
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
