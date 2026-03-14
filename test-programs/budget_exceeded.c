// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * budget_exceeded — a program whose instruction count would exceed the runtime
 * instruction budget once metering is implemented.
 *
 * The loop is bounded by a compile-time constant so the Prevail verifier
 * accepts it when using the Ephemeral profile (where termination checking
 * is enabled, `check_for_termination = true`).  When ingested as a
 * Resident program, Prevail's termination check is disabled
 * (`check_for_termination = false`), so the Resident profile accepts any
 * bounded loop regardless.  The iteration count is large enough that,
 * once runtime instruction metering is added to sonde-bpf, the interpreter
 * will terminate the program early regardless of profile.
 *
 * NOTE: sonde-bpf does not yet enforce the instruction_budget parameter at
 * runtime.  Termination today relies on Prevail verification: bounded loops
 * and no infinite recursion.  Under the Resident profile
 * (check_for_termination = false) loop bounds are still structurally
 * enforced by BPF constraints; the Ephemeral profile
 * (check_for_termination = true) additionally proves termination.  Once
 * runtime metering is added, this program will exercise the budget-exceeded
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
    /* Return the computed sum (not 0) to keep the accumulator live and
     * prevent clang from discarding the loop as dead code.  The program
     * return value is currently unused by the node firmware (per
     * bpf-environment.md §4), so the non-zero value is harmless. */
    return (__s32)sum;
}
