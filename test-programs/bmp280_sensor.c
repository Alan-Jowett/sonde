// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * bmp280_sensor — read a Bosch BMP280 pressure/temperature sensor over I2C.
 *
 * The BMP280 stores factory calibration words in registers 0x88..0x9F and
 * returns raw 20-bit pressure/temperature ADC readings in a 6-byte burst at
 * 0xF7..0xFC. This program follows the Bosch `BST-BMP280-DS001` datasheet:
 *   1. Read and verify the chip ID (0x58).
 *   2. Read the 24-byte calibration block.
 *   3. Trigger a forced measurement with pressure/temp oversampling x1.
 *   4. Wait longer than the datasheet maximum conversion time.
 *   5. Burst-read raw pressure + temperature registers.
 *   6. Compensate temperature and pressure using the integer formulas from
 *      datasheet section 3.11.3 / appendix 8.2.
 *
 * Payload (22 bytes, queued with send_async):
 *   [0..7]   timestamp (little-endian u64, ms since epoch)
 *   [8..13]  raw register burst [press_msb..temp_xlsb]
 *   [14..17] temp_mC (little-endian i32)
 *   [18..21] pressure_pa (little-endian u32)
 */

#include "include/sonde_helpers.h"

/* Bus 0, BMP280 at 0x76 (SDO tied low). 0x77 is also valid on some boards. */
#define BMP280_HANDLE I2C_HANDLE(0, 0x76)

#define BMP280_REG_CALIB_START 0x88u
#define BMP280_REG_CHIP_ID     0xD0u
#define BMP280_REG_CTRL_MEAS   0xF4u
#define BMP280_REG_PRESS_MSB   0xF7u

#define BMP280_CHIP_ID         0x58u
#define BMP280_CALIB_LEN       24u
#define BMP280_RAW_LEN         6u

/* osrs_t=1, osrs_p=1, mode=forced => 0b00100101 = 0x25. */
#define BMP280_CTRL_MEAS_FORCED 0x25u

/* Datasheet Table 13: x1/x1 forced conversion max is 6.4 ms; use margin. */
#define BMP280_CONVERSION_US   10000u

static __noinline int bmp280_read_bytes(__u8 reg, __u8 *out, __u32 len)
{
    return i2c_write_read(BMP280_HANDLE, &reg, 1, out, len);
}

static __noinline __u16 read_le_u16(const __u8 *buf)
{
    return (__u16)((__u16)buf[0] | ((__u16)buf[1] << 8));
}

static __noinline __s32 read_le_s16(const __u8 *buf)
{
    __u32 bits = (__u32)read_le_u16(buf);

    if (bits & 0x8000u)
        bits |= 0xFFFF0000u;

    return (__s32)bits;
}

static __noinline __s32 bmp280_compensate_temp_mc(__s32 adc_t,
                                                  const __u8 *calib,
                                                  __s32 *t_fine_out)
{
    __u16 dig_t1 = read_le_u16(&calib[0]);
    __s32 dig_t2 = read_le_s16(&calib[2]);
    __s32 dig_t3 = read_le_s16(&calib[4]);

    __s64 adc_t_s64 = (__s64)adc_t;
    __s64 var1 = (((adc_t_s64 >> 3) - ((__s64)dig_t1 << 1)) * (__s64)dig_t2) >> 11;
    __s64 delta = (adc_t_s64 >> 4) - (__s64)dig_t1;
    __s64 var2 = (((delta * delta) >> 12) * (__s64)dig_t3) >> 14;
    __s32 t_fine = (__s32)(var1 + var2);
    __s32 temp_centi_c = (__s32)((((__s64)t_fine) * 5 + 128) >> 8);

    *t_fine_out = t_fine;
    return temp_centi_c * 10;
}

static __noinline __u32 bmp280_compensate_pressure_pa(__s32 adc_p,
                                                       const __u8 *calib,
                                                       __s32 t_fine)
{
    __u16 dig_p1 = read_le_u16(&calib[6]);
    __s32 dig_p2 = read_le_s16(&calib[8]);
    __s32 dig_p3 = read_le_s16(&calib[10]);
    __s32 dig_p4 = read_le_s16(&calib[12]);
    __s32 dig_p5 = read_le_s16(&calib[14]);
    __s32 dig_p6 = read_le_s16(&calib[16]);
    __s32 dig_p7 = read_le_s16(&calib[18]);
    __s32 dig_p8 = read_le_s16(&calib[20]);
    __s32 dig_p9 = read_le_s16(&calib[22]);

    __s64 var1 = ((__s64)t_fine >> 1) - 64000;
    __s64 var2 = ((((var1 >> 2) * (var1 >> 2)) >> 11) * (__s64)dig_p6);
    var2 += (var1 * (__s64)dig_p5) << 1;
    var2 = (var2 >> 2) + ((__s64)dig_p4 << 16);
    var1 = ((((__s64)dig_p3 * (((var1 >> 2) * (var1 >> 2)) >> 13)) >> 3) +
            (((__s64)dig_p2 * var1) >> 1)) >> 18;
    var1 = (((__s64)32768 + var1) * (__s64)dig_p1) >> 15;
    if (var1 <= 0)
        return 0;

    __s64 delta = ((__s64)1048576 - (__s64)adc_p) - (var2 >> 12);
    if (delta <= 0)
        return 0;

    __u64 p = (__u64)delta * 3125u;
    if (p < 0x80000000ULL)
        p = (p << 1) / (__u64)var1;
    else
        p = (p / (__u64)var1) * 2u;

    __s64 corr1 = (((__s64)dig_p9) * (__s64)(((p >> 3) * (p >> 3)) >> 13)) >> 12;
    __s64 corr2 = (((__s64)(p >> 2)) * (__s64)dig_p8) >> 13;
    __s64 corrected = (__s64)p + ((corr1 + corr2 + (__s64)dig_p7) >> 4);

    if (corrected <= 0)
        return 0;

    return (__u32)corrected;
}

SEC("sonde")
int program(struct sonde_context *ctx)
{
    __u8 chip_id = 0;
    int rc = bmp280_read_bytes(BMP280_REG_CHIP_ID, &chip_id, 1);
    if (rc < 0 || chip_id != BMP280_CHIP_ID) {
        char err[] = "bmp280: device not found\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    __u8 calib[BMP280_CALIB_LEN];
    rc = bmp280_read_bytes(BMP280_REG_CALIB_START, calib, sizeof(calib));
    if (rc < 0) {
        char err[] = "bmp280: calib read failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    __u8 ctrl_meas_write[2] = { BMP280_REG_CTRL_MEAS, BMP280_CTRL_MEAS_FORCED };
    rc = i2c_write(BMP280_HANDLE, ctrl_meas_write, sizeof(ctrl_meas_write));
    if (rc < 0) {
        char err[] = "bmp280: trigger failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    rc = delay_us(BMP280_CONVERSION_US);
    if (rc < 0) {
        char err[] = "bmp280: delay failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    __u8 raw[BMP280_RAW_LEN];
    rc = bmp280_read_bytes(BMP280_REG_PRESS_MSB, raw, sizeof(raw));
    if (rc < 0) {
        char err[] = "bmp280: raw read failed\n";
        bpf_trace_printk(err, (__u32)(sizeof(err) - 1));
        return 0;
    }

    __s32 adc_p = (__s32)(((__u32)raw[0] << 12) |
                          ((__u32)raw[1] << 4) |
                          ((__u32)raw[2] >> 4));
    __s32 adc_t = (__s32)(((__u32)raw[3] << 12) |
                          ((__u32)raw[4] << 4) |
                          ((__u32)raw[5] >> 4));

    __s32 t_fine = 0;
    __s32 temp_mc = bmp280_compensate_temp_mc(adc_t, calib, &t_fine);
    __u32 pressure_pa = bmp280_compensate_pressure_pa(adc_p, calib, t_fine);

    __u8 payload[22];
    __u64 ts = ctx->timestamp;
    __u32 temp_bits = (__u32)temp_mc;

    payload[0]  = (__u8)(ts);
    payload[1]  = (__u8)(ts >> 8);
    payload[2]  = (__u8)(ts >> 16);
    payload[3]  = (__u8)(ts >> 24);
    payload[4]  = (__u8)(ts >> 32);
    payload[5]  = (__u8)(ts >> 40);
    payload[6]  = (__u8)(ts >> 48);
    payload[7]  = (__u8)(ts >> 56);

    payload[8]  = raw[0];
    payload[9]  = raw[1];
    payload[10] = raw[2];
    payload[11] = raw[3];
    payload[12] = raw[4];
    payload[13] = raw[5];

    payload[14] = (__u8)(temp_bits);
    payload[15] = (__u8)(temp_bits >> 8);
    payload[16] = (__u8)(temp_bits >> 16);
    payload[17] = (__u8)(temp_bits >> 24);

    payload[18] = (__u8)(pressure_pa);
    payload[19] = (__u8)(pressure_pa >> 8);
    payload[20] = (__u8)(pressure_pa >> 16);
    payload[21] = (__u8)(pressure_pa >> 24);

    rc = send_async(payload, sizeof(payload));
    if (rc == -1)
        send(payload, sizeof(payload));

    return 0;
}

/**
 * Decoder: parse the 22-byte BMP280 APP_DATA payload and emit named readings.
 *
 * Payload layout:
 *   [0..7]   timestamp (little-endian u64, ms since epoch)
 *   [8..13]  raw register burst [press_msb..temp_xlsb]
 *   [14..17] temp_mC (little-endian i32)
 *   [18..21] pressure_pa (little-endian u32)
 */
/*
 * NOTE: The ctx->input_data dereference pattern below requires sonde-bpf
 * data/data_end pointer tagging (not yet implemented). Until then, decoders
 * must access the blob via R1 bounded offsets at runtime. This source file
 * documents the intended ABI; hand-crafted bytecode is used for gateway tests.
 */
SEC("decoder")
int decode(struct decoder_context *ctx)
{
    const __u8 *data = (const __u8 *)(__u64)ctx->input_data;
    const __u8 *data_end = (const __u8 *)(__u64)ctx->input_end;

    if (data + 22 > data_end)
        return 0;

    __s32 temp_mc = (__s32)(
        (__u32)data[14] |
        ((__u32)data[15] << 8) |
        ((__u32)data[16] << 16) |
        ((__u32)data[17] << 24)
    );
    __u32 pressure_pa = (__u32)(
        (__u32)data[18] |
        ((__u32)data[19] << 8) |
        ((__u32)data[20] << 16) |
        ((__u32)data[21] << 24)
    );

    __u32 abs_mc = (temp_mc < 0) ? (__u32)(-temp_mc) : (__u32)temp_mc;
    __u32 abs_mf = abs_mc * 9u / 5u;
    __s32 signed_mf = (temp_mc < 0) ? -(__s32)abs_mf : (__s32)abs_mf;
    __s32 temp_mf = signed_mf + 32000;

    char name_temp[] = "temp_mc";
    emit_reading(name_temp, 7, (__s64)temp_mc);

    char name_tempf[] = "temp_mf";
    emit_reading(name_tempf, 7, (__s64)temp_mf);

    char name_pressure[] = "pressure_pa";
    emit_reading(name_pressure, 11, (__s64)pressure_pa);

    return 0;
}
