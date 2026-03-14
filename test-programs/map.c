// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * map — demonstrates BPF map usage: map_update_elem and map_lookup_elem.
 *
 * Maintains a wake-cycle counter in a single-entry ARRAY map.  On each
 * invocation the counter is incremented, written back, and the new value is
 * sent to the gateway.
 *
 * Corresponds to map_program in docs/node-validation.md §2.4.
 */

#include "include/sonde_helpers.h"

/**
 * Single-entry array map that persists across sleep cycles.
 *
 * Storage: 1 entry × (4-byte key + 4-byte value) = 8 bytes of RTC SRAM.
 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u32);
} counter_map SEC(".maps");

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    __u32 key = 0;

    /* Read the current counter value (or treat missing entry as 0). */
    __u32 *ptr = map_lookup_elem(&counter_map, &key);
    __u32  val = ptr ? *ptr : 0;

    /* Increment and write the counter back. */
    val += 1;
    map_update_elem(&counter_map, &key, &val);

    /* Send the updated counter value to the gateway. */
    send(&val, sizeof(val));

    return 0;
}
