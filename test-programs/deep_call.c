// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * deep_call — exercises BPF-to-BPF calls at the maximum supported depth.
 *
 * BPF supports up to 8 call frames (including the entry point).  This
 * program uses all 8 frames — depth_2 through depth_8 plus the entry point
 * program() — to verify that both the Prevail verifier and the sonde-bpf
 * interpreter handle the maximum call depth correctly.
 *
 * Each function is marked __noinline so the compiler emits a real BPF_CALL
 * instruction rather than inlining the body.  A runtime value (battery_mv)
 * is threaded through the call chain to prevent constant folding.
 *
 * Corresponds to deep_call_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

/* Leaf function at call depth 8. */
static __noinline int depth_8(int v) { return v + 8; }

/* Functions at depths 7 down to 2 each call the one below. */
static __noinline int depth_7(int v) { return depth_8(v) + 7; }
static __noinline int depth_6(int v) { return depth_7(v) + 6; }
static __noinline int depth_5(int v) { return depth_6(v) + 5; }
static __noinline int depth_4(int v) { return depth_5(v) + 4; }
static __noinline int depth_3(int v) { return depth_4(v) + 3; }
static __noinline int depth_2(int v) { return depth_3(v) + 2; }

/**
 * Entry point at depth 1.  Passes the runtime battery voltage through the
 * call chain to depth 8 and back, producing a call stack that is 8 frames
 * deep.  The result is sent to the gateway as a single byte.
 */
SEC("sonde")
int program(struct sonde_context *ctx)
{
    /* Use a runtime value so the compiler cannot constant-fold the chain. */
    int seed = (int)((__u32)ctx->battery_mv);
    int result = depth_2(seed) + 1;
    __u8 blob[1] = { (__u8)((__u32)result & 0xFFu) };
    send(blob, sizeof(blob));
    return 0;
}
