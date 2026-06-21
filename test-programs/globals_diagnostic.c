// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * globals_diagnostic — verify `.rodata`, `.data`, and `.bss` behavior.
 *
 * This resident program emits one compact 24-byte APP_DATA payload per wake:
 *
 *   [0..3]   wake_index     (little-endian u32)
 *   [4..7]   rodata_value   (little-endian u32)
 *   [8..11]  data_before    (little-endian u32)
 *   [12..15] data_after     (little-endian u32)
 *   [16..19] bss_before     (little-endian u32)
 *   [20..23] bss_after      (little-endian u32)
 *
 * Semantics:
 *   - `.rodata` is checked by reporting a compile-time constant value.
 *   - `.data` is checked by reporting the initialized value, then incrementing it.
 *   - `.bss` is checked by reporting its value before and after incrementing it.
 *
 * The paired `SEC("decoder")` section emits named readings so a gateway-side
 * handler can log an operator-friendly pass/fail summary.
 */

#include "include/sonde_helpers.h"

#define GLOBALS_DIAG_REPORT_LEN 24u
#define GLOBALS_DIAG_RODATA_INIT 0x13579BDFu
#define GLOBALS_DIAG_DATA_INIT   0x2468ACE0u

static const __u32 globals_diag_rodata = GLOBALS_DIAG_RODATA_INIT;
static __u32 globals_diag_data = GLOBALS_DIAG_DATA_INIT;
static __u32 globals_diag_bss;

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    __u8 report[GLOBALS_DIAG_REPORT_LEN];
    __u32 wake_index = globals_diag_bss;
    __u32 rodata_value = globals_diag_rodata;
    __u32 data_before = globals_diag_data;
    __u32 data_after = data_before + 1u;
    __u32 bss_before = globals_diag_bss;
    __u32 bss_after = bss_before + 1u;

    globals_diag_data = data_after;
    globals_diag_bss = bss_after;

    report[0] = (__u8)wake_index;
    report[1] = (__u8)(wake_index >> 8);
    report[2] = (__u8)(wake_index >> 16);
    report[3] = (__u8)(wake_index >> 24);

    report[4] = (__u8)rodata_value;
    report[5] = (__u8)(rodata_value >> 8);
    report[6] = (__u8)(rodata_value >> 16);
    report[7] = (__u8)(rodata_value >> 24);

    report[8] = (__u8)data_before;
    report[9] = (__u8)(data_before >> 8);
    report[10] = (__u8)(data_before >> 16);
    report[11] = (__u8)(data_before >> 24);

    report[12] = (__u8)data_after;
    report[13] = (__u8)(data_after >> 8);
    report[14] = (__u8)(data_after >> 16);
    report[15] = (__u8)(data_after >> 24);

    report[16] = (__u8)bss_before;
    report[17] = (__u8)(bss_before >> 8);
    report[18] = (__u8)(bss_before >> 16);
    report[19] = (__u8)(bss_before >> 24);

    report[20] = (__u8)bss_after;
    report[21] = (__u8)(bss_after >> 8);
    report[22] = (__u8)(bss_after >> 16);
    report[23] = (__u8)(bss_after >> 24);

    send(report, GLOBALS_DIAG_REPORT_LEN);
    return 0;
}

SEC("decoder")
int decode(struct decoder_context *ctx)
{
    const __u8 *data = (const __u8 *)(__u64)ctx->input_data;
    const __u8 *data_end = (const __u8 *)(__u64)ctx->input_end;

    if (data + GLOBALS_DIAG_REPORT_LEN > data_end)
        return 0;

    __u32 wake_index = (__u32)(
        (__u32)data[0] |
        ((__u32)data[1] << 8) |
        ((__u32)data[2] << 16) |
        ((__u32)data[3] << 24)
    );
    __u32 rodata_value = (__u32)(
        (__u32)data[4] |
        ((__u32)data[5] << 8) |
        ((__u32)data[6] << 16) |
        ((__u32)data[7] << 24)
    );
    __u32 data_before = (__u32)(
        (__u32)data[8] |
        ((__u32)data[9] << 8) |
        ((__u32)data[10] << 16) |
        ((__u32)data[11] << 24)
    );
    __u32 data_after = (__u32)(
        (__u32)data[12] |
        ((__u32)data[13] << 8) |
        ((__u32)data[14] << 16) |
        ((__u32)data[15] << 24)
    );
    __u32 bss_before = (__u32)(
        (__u32)data[16] |
        ((__u32)data[17] << 8) |
        ((__u32)data[18] << 16) |
        ((__u32)data[19] << 24)
    );
    __u32 bss_after = (__u32)(
        (__u32)data[20] |
        ((__u32)data[21] << 8) |
        ((__u32)data[22] << 16) |
        ((__u32)data[23] << 24)
    );

    char wake_name[] = "wake_index";
    emit_reading(wake_name, 10, (__s64)wake_index);

    char ro_name[] = "rodata_value";
    emit_reading(ro_name, 12, (__s64)rodata_value);

    char data_before_name[] = "data_before";
    emit_reading(data_before_name, 11, (__s64)data_before);

    char data_after_name[] = "data_after";
    emit_reading(data_after_name, 10, (__s64)data_after);

    char bss_before_name[] = "bss_before";
    emit_reading(bss_before_name, 10, (__s64)bss_before);

    char bss_after_name[] = "bss_after";
    emit_reading(bss_after_name, 9, (__s64)bss_after);

    return 0;
}
