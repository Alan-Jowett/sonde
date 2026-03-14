// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * budget_exceeded — a program whose instruction count would exceed the runtime
 * instruction budget once metering is implemented.
 *
 * The loop is bounded by a compile-time constant so the Prevail verifier
 * accepts it as semantically safe (termination is guaranteed).  The
 * iteration count is large enough that, when runtime instruction metering
 * is added to sonde-bpf, the interpreter will terminate the program early.
 *
 * NOTE: sonde-bpf does not yet enforce the instruction_budget parameter at
 * runtime.  Termination is currently guaranteed solely by Prevail
 * verification on the gateway (bounded loops, no infinite recursion).  Once
 * metering support is added this program will exercise the budget-exceeded
 * path.  See crates/sonde-node/src/sonde_bpf_adapter.rs for the current
 * limitation.
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
 * above any reasonable platform budget once metering is enforced.
 */
#define ITERATIONS 1000000  /* 1,000,000 */

SEC("sonde")
int program(struct sonde_context *ctx)
{
    /*
     * Seed the accumulator from a runtime value (ctx->timestamp) so
     * the compiler cannot optimise the loop into a closed-form constant.
     * Without this, clang may legally compute the sum at compile time
     * and emit a single-instruction program, defeating the purpose of
     * exercising the runtime instruction budget.
     */
    __s64 sum = (__s64)ctx->timestamp;
    for (__s64 i = 0; i < ITERATIONS; i++) {
        sum += i;
    }
    return (__s32)sum;
}
