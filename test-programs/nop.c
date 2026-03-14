// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * nop — the simplest possible sonde BPF program.
 *
 * Returns 0 immediately without calling any helpers or accessing maps.
 * Used as a baseline for ingestion, verification, and execution tests.
 *
 * Corresponds to nop_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;
    return 0;
}
