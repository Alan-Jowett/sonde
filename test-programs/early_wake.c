// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * early_wake — requests an earlier next wake via set_next_wake().
 *
 * Calls set_next_wake(10) to schedule the next wake in 10 seconds, overriding
 * the gateway-configured interval.  The node firmware honours the shorter of
 * the two intervals (BPF request vs gateway schedule).
 *
 * Corresponds to early_wake_program in docs/node-validation.md §2.4.
 * Used by test T-N224 (early-wake scheduling).
 */

#include "include/sonde_helpers.h"

/** Seconds until the next requested wake. */
#define NEXT_WAKE_SECONDS 10u

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;
    set_next_wake(NEXT_WAKE_SECONDS);
    return 0;
}
