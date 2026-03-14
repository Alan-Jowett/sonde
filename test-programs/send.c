// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * send — sends a fixed, known blob to the gateway via send().
 *
 * Uses a constant byte sequence so tests can assert on the exact payload.
 * Corresponds to send_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

/** Fixed payload — deterministic so tests can match on content. */
static const __u8 SEND_BLOB[] = { 0xAA, 0xBB };

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;
    send(SEND_BLOB, sizeof(SEND_BLOB));
    return 0;
}
