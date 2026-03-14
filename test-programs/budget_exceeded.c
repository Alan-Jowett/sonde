// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * budget_exceeded — a program whose instruction count exceeds the runtime limit.
 *
 * The loop is bounded by a compile-time constant so the Prevail verifier
 * accepts it as semantically safe, but the iteration count is large enough
 * that the sonde-bpf interpreter will terminate the program early when its
 * instruction budget is exhausted.
 *
 * This tests that the node firmware correctly enforces the runtime instruction
 * budget and reports a budget-exceeded termination rather than running forever.
 *
 * Corresponds to budget_exceeded_program in docs/node-validation.md §2.4.
 * Used by test T-N614 (execution constraint — instruction budget).
 */

#include "include/sonde_helpers.h"

/**
 * Number of loop iterations.
 *
 * Each iteration executes several BPF instructions (add, compare, branch).
 * One million iterations produces well over 3 000 000 instructions — far
 * above any reasonable platform budget.
 */
#define ITERATIONS 1000000  /* 1,000,000 */

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    /*
     * Accumulate a sum to prevent the compiler from optimising the loop away.
     * Return the sum so the result register is live and the loop body cannot
     * be elided.
     */
    int sum = 0;
    for (int i = 0; i < ITERATIONS; i++) {
        sum += i;
    }
    return sum;
}
