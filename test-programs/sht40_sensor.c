// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors


/**
 * sht40_sensor — read Sensirion SHT40 (SHT4x family) temperature + humidity.
 *
 * SHT4x devices are command-based (not register-mapped):
 *   1) i2c_write(cmd)
 *   2) delay_us(conversion_time)
 *   3) i2c_read(6-byte frame)
 *
 * Result frame (6 bytes):
 *   [0] T_msb
 *   [1] T_lsb
 *   [2] CRC(T_msb,T_lsb)
 *   [3] RH_msb
 *   [4] RH_lsb
 *   [5] CRC(RH_msb,RH_lsb)
 *
 * Conversions (datasheet):
 *   T_C  = -45 + 175 * (T_raw / 65535)
 *   RH%  = -6  + 125 * (RH_raw / 65535)
 *
 * Payload (14 bytes):
 *   [0..5]   raw frame (T + CRC + RH + CRC)
 *   [6..9]   temp_mC (little-endian i32)
 *   [10..13] rh_mpermille (little-endian i32)  // milli-%RH
 */

#include "include/sonde_helpers.h"

/* Bus 0, typical SHT4x address 0x44 (some variants use 0x45). */
#define SHT40_HANDLE I2C_HANDLE(0, 0x44)

/* Measurement commands (SHT4x family). */
#define SHT4X_CMD_MEASURE_HIGH   0xFDu  /* high precision */
#define SHT4X_CMD_MEASURE_MEDIUM 0xF6u  /* medium precision */
#define SHT4X_CMD_MEASURE_LOW    0xE0u  /* low precision */

/* Typical conversion delays (microseconds). Use margin over typical values. */
#define SHT4X_DELAY_HIGH_US   10000u  /* ~8.3ms typical -> 10ms */
#define SHT4X_DELAY_MED_US     6000u  /* ~4.5ms typical -> 6ms  */
#define SHT4X_DELAY_LOW_US     2500u  /* ~1.6ms typical -> 2.5ms */

/* CRC-8 per Sensirion SHT4x: polynomial 0x31, init 0xFF. */
static __u8
crc8_sensirion_2bytes(const __u8 *data)
{
    __u8 crc = 0xFFu;
    for (int i = 0; i < 2; i++) {
        crc ^= data[i];
        for (int b = 0; b < 8; b++) {
            if (crc & 0x80u)
                crc = (__u8)((crc << 1) ^ 0x31u);
            else
                crc = (__u8)(crc << 1);
        }
    }
    return crc;
}

SEC("sonde")
int program(struct sonde_context *ctx)
{
    /* 1) Send measurement command */
    __u8 cmd = SHT4X_CMD_MEASURE_HIGH;
    int rc = i2c_write(SHT40_HANDLE, &cmd, 1);
    if (rc < 0) {
        char err[] = "sht40: write failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    /* 2) Wait for conversion */
    rc = delay_us(SHT4X_DELAY_HIGH_US);
    if (rc < 0) {
        char err[] = "sht40: delay failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    /* 3) Read 6-byte result frame */
    __u8 buf[6];
    rc = i2c_read(SHT40_HANDLE, buf, sizeof(buf));
    if (rc < 0) {
        char err[] = "sht40: read failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    /* Optional CRC validation */
    __u8 t_crc  = crc8_sensirion_2bytes(&buf[0]);
    __u8 rh_crc = crc8_sensirion_2bytes(&buf[3]);
    if (t_crc != buf[2] || rh_crc != buf[5]) {
        char err[] = "sht40: crc mismatch\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    /* Parse raw values (big-endian) */
    __u16 t_raw  = (__u16)(((__u16)buf[0] << 8) | (__u16)buf[1]);
    __u16 rh_raw = (__u16)(((__u16)buf[3] << 8) | (__u16)buf[4]);

    /* Convert to milli-units using integer math.
     * temp_mC = (-45 + 175 * t_raw/65535) * 1000
     *         = -45000 + (175000 * t_raw)/65535
     */
    __s32 temp_mC = -45000;
    temp_mC += (__s32)(((__u64)175000u * (__u64)t_raw) / 65535u);

    /* rh_mpermille = (-6 + 125 * rh_raw/65535) * 1000
     *              = -6000 + (125000 * rh_raw)/65535
     */
    __s32 rh_mpermille = -6000;
    rh_mpermille += (__s32)(((__u64)125000u * (__u64)rh_raw) / 65535u);

    /* Build payload (22 bytes), avoiding unaligned access.
     *
     * Layout:
     *   [0..7]   timestamp (little-endian u64, ms since epoch)
     *   [8..13]  raw frame (T_msb, T_lsb, CRC_T, RH_msb, RH_lsb, CRC_RH)
     *   [14..17] temp_mC (little-endian i32)
     *   [18..21] rh_mpermille (little-endian i32)
     */
    __u8 payload[22];

    /* Embed ctx->timestamp per bpf-environment.md best practice:
     * async delivery may be delayed; handlers need collection time. */
    __u64 ts = ctx->timestamp;
    payload[0]  = (__u8)(ts);
    payload[1]  = (__u8)(ts >> 8);
    payload[2]  = (__u8)(ts >> 16);
    payload[3]  = (__u8)(ts >> 24);
    payload[4]  = (__u8)(ts >> 32);
    payload[5]  = (__u8)(ts >> 40);
    payload[6]  = (__u8)(ts >> 48);
    payload[7]  = (__u8)(ts >> 56);

    /* Raw frame */
    payload[8]  = buf[0];
    payload[9]  = buf[1];
    payload[10] = buf[2];
    payload[11] = buf[3];
    payload[12] = buf[4];
    payload[13] = buf[5];

    /* temp_mC little-endian i32 */
    __u32 t_bits = (__u32)temp_mC;
    payload[14] = (__u8)(t_bits);
    payload[15] = (__u8)(t_bits >> 8);
    payload[16] = (__u8)(t_bits >> 16);
    payload[17] = (__u8)(t_bits >> 24);

    /* rh_mpermille little-endian i32 */
    __u32 rh_bits = (__u32)rh_mpermille;
    payload[18] = (__u8)(rh_bits);
    payload[19] = (__u8)(rh_bits >> 8);
    payload[20] = (__u8)(rh_bits >> 16);
    payload[21] = (__u8)(rh_bits >> 24);

    /* Store-and-forward: queue for delivery on next wake cycle.
     * Falls back to send() only if the async queue is full. */
    rc = send_async(payload, sizeof(payload));
    if (rc == -1) {
        send(payload, sizeof(payload));
    }
    return 0;
}
