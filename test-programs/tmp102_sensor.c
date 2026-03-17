// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * tmp102_sensor — reads a SparkFun TMP102 temperature sensor over I2C.
 *
 * The TMP102 is a simple 12-bit digital temperature sensor from Texas
 * Instruments.  It starts measuring on power-up with no initialization
 * required (continuous conversion mode, 4 Hz default).
 *
 * Each wake cycle:
 *   1. Read 2 bytes from the temperature register (0x00).
 *   2. Convert the raw 12-bit value to millidegrees Celsius.
 *   3. Send a 6-byte payload to the gateway:
 *        [temp_raw_hi, temp_raw_lo, temp_mC as 4-byte little-endian i32]
 *
 * The gateway handler can decode the payload or use the raw bytes directly.
 *
 * Connection: I2C bus 0, address 0x48 (ADD0 tied to GND).
 *             SDA = GPIO 0, SCL = GPIO 1 (as configured in esp_hal.rs).
 *
 * Datasheet: https://www.ti.com/lit/ds/symlink/tmp102.pdf
 */

#include "include/sonde_helpers.h"

/* I2C bus 0, TMP102 at 7-bit address 0x48 (ADD0 = GND). */
#define TMP102_HANDLE       I2C_HANDLE(0, 0x48)

/* TMP102 register addresses. */
#define TMP102_REG_TEMP     0x00u  /**< Temperature register (read-only, 2 bytes). */

/* TMP102 resolution: 0.0625 °C per LSB (12-bit mode).
 * To avoid floating point in BPF, we compute millidegrees:
 *   temp_mC = raw_12bit * 625 / 10
 * This gives integer millidegrees Celsius (e.g., 25125 = 25.125 °C). */

/* Error messages for trace output. */
static const char err_read[] = "tmp102: read failed\n";

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    /* Read 2 bytes from the temperature register. */
    __u8 reg = TMP102_REG_TEMP;
    __u8 raw[2];
    int rc = i2c_write_read(TMP102_HANDLE, &reg, 1, raw, 2);
    if (rc < 0) {
        bpf_trace_printk(err_read, (__u32)(sizeof(err_read) - 1));
        return 0;
    }

    /*
     * TMP102 temperature format (12-bit mode, default):
     *   raw[0] = [D11 D10 D9 D8 D7 D6 D5 D4]
     *   raw[1] = [D3  D2  D1 D0 0  0  0  0 ]
     *
     * Combine and shift: raw_12bit = (raw[0] << 4) | (raw[1] >> 4)
     * This is a signed 12-bit value in two's complement.
     */
    __s32 raw_12bit = ((__s32)raw[0] << 4) | ((__s32)raw[1] >> 4);

    /* Sign-extend from 12 bits to 32 bits. */
    if (raw_12bit & 0x800)
        raw_12bit |= (__s32)-4096;  /* -4096 == 0xFFFFF000 as signed */

    /* Convert to millidegrees Celsius: raw * 0.0625 * 1000 = raw * 625 / 10
     * BPF doesn't support signed division, so handle sign separately. */
    __u32 abs_raw = (raw_12bit < 0) ? (__u32)(-raw_12bit) : (__u32)raw_12bit;
    __u32 abs_mc = abs_raw * 625u / 10u;
    __s32 temp_mc = (raw_12bit < 0) ? -(__s32)abs_mc : (__s32)abs_mc;

    /* Build 6-byte payload manually to avoid unaligned access:
     *   [0]   raw_hi
     *   [1]   raw_lo
     *   [2:5] temp_mc as little-endian i32
     * Cast to __u32 for bit-shifting to avoid implementation-defined
     * behavior on signed right-shift. */
    __u32 temp_bits = (__u32)temp_mc;
    __u8 payload[6];
    payload[0] = raw[0];
    payload[1] = raw[1];
    payload[2] = (__u8)(temp_bits);
    payload[3] = (__u8)(temp_bits >> 8);
    payload[4] = (__u8)(temp_bits >> 16);
    payload[5] = (__u8)(temp_bits >> 24);

    send(payload, sizeof(payload));
    return 0;
}
