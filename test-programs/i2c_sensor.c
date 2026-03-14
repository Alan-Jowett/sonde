// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * i2c_sensor — reads a BME280 environmental sensor over I2C and sends data.
 *
 * Demonstrates the typical I2C sensor interaction pattern:
 *   1. Read the chip-ID register to verify the device is present.
 *   2. Trigger a forced-mode measurement.
 *   3. Wait for conversion to complete using delay_us().
 *   4. Burst-read the raw measurement registers via i2c_write_read().
 *   5. Transmit the raw bytes to the gateway via send().
 *
 * The BME280 is a popular temperature / humidity / pressure sensor from
 * Bosch.  This program uses its simplest operating mode (forced mode, no
 * oversampling) to minimise complexity.
 *
 * Connection: I2C bus 0, address 0x76 (SDO pin tied low).
 */

#include "include/sonde_helpers.h"

/* I2C bus 0, BME280 at 7-bit address 0x76. */
#define BME280_HANDLE  I2C_HANDLE(0, 0x76)

/* BME280 register addresses (from the Bosch datasheet). */
#define BME280_REG_CHIP_ID    0xD0u  /**< Always reads 0x60 on genuine parts. */
#define BME280_REG_CTRL_HUM   0xF2u  /**< Humidity oversampling control.       */
#define BME280_REG_CTRL_MEAS  0xF4u  /**< Temp/pressure oversampling + mode.   */
#define BME280_REG_PRESS_MSB  0xF7u  /**< Start of the 8-byte raw data block.  */

/* Expected chip-ID value. */
#define BME280_CHIP_ID        0x60u

/*
 * ctrl_meas value: osrs_t=1 (×1), osrs_p=1 (×1), mode=01 (forced).
 * Binary: 001 001 01 = 0x25.
 */
#define BME280_CTRL_MEAS_FORCED 0x25u

/* Forced-mode conversion time for ×1 oversampling is at most ~10 ms. */
#define BME280_CONVERSION_US 10000u

/* Size of the raw output data block (3 bytes pressure + 3 temp + 2 hum). */
#define BME280_RAW_DATA_LEN  8u

/**
 * bme280_read_reg — read a single register from the BME280.
 *
 * Uses the i2c_write_read pattern: write the register address byte, then
 * read one byte back in a single I2C transaction (repeated start).
 *
 * Returns 0 on success, negative on error.
 */
static __noinline int bme280_read_reg(__u8 reg, __u8 *out)
{
    return i2c_write_read(BME280_HANDLE, &reg, 1, out, 1);
}

/**
 * bme280_trigger_and_read — start a forced measurement and return raw bytes.
 *
 * Writes the ctrl_meas register to trigger one measurement, waits for
 * conversion, then burst-reads all eight raw output bytes into @raw.
 *
 * Returns 0 on success, negative on error.
 */
static __noinline int bme280_trigger_and_read(__u8 *raw)
{
    /* Enable humidity oversampling ×1 (must be set before ctrl_meas). */
    __u8 ctrl_hum_write[2] = { BME280_REG_CTRL_HUM, 0x01u };
    int rc = i2c_write(BME280_HANDLE, ctrl_hum_write, sizeof(ctrl_hum_write));
    if (rc < 0)
        return rc;

    /* Trigger forced-mode measurement. */
    __u8 ctrl_meas_write[2] = { BME280_REG_CTRL_MEAS, BME280_CTRL_MEAS_FORCED };
    rc = i2c_write(BME280_HANDLE, ctrl_meas_write, sizeof(ctrl_meas_write));
    if (rc < 0)
        return rc;

    /* Wait for conversion to complete. */
    delay_us(BME280_CONVERSION_US);

    /* Burst-read 8 raw bytes starting at the pressure MSB register. */
    __u8 start_reg = BME280_REG_PRESS_MSB;
    return i2c_write_read(BME280_HANDLE,
                          &start_reg, 1,
                          raw, BME280_RAW_DATA_LEN);
}

/* Error message strings for bpf_trace_printk. */
static const char err_no_device[] = "bme280: device not found\n";
static const char err_read_fail[] = "bme280: measurement failed\n";

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    /* Step 1: verify device identity. */
    __u8 chip_id = 0;
    int rc = bme280_read_reg(BME280_REG_CHIP_ID, &chip_id);
    if (rc < 0 || chip_id != BME280_CHIP_ID) {
        bpf_trace_printk(err_no_device, sizeof(err_no_device));
        return 0;
    }

    /* Step 2: trigger measurement and read raw data. */
    __u8 raw[BME280_RAW_DATA_LEN];
    rc = bme280_trigger_and_read(raw);
    if (rc < 0) {
        bpf_trace_printk(err_read_fail, sizeof(err_read_fail));
        return 0;
    }

    /* Step 3: transmit raw bytes to the gateway for decoding. */
    send(raw, sizeof(raw));
    return 0;
}
