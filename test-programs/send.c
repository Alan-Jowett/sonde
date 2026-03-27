// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * send — sends a fixed, known blob to the gateway via send().
 *
 * Uses a constant byte sequence so tests can assert on the exact payload.
 * Corresponds to send_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

/** Fixed payload — on stack to avoid .rodata global map (prevail-rust#1). */
SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;
    __u8 blob[] = { 0xAA, 0xBB };
    send(blob, sizeof(blob));
    return 0;
}
