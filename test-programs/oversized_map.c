// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * oversized_map — declares a map that exceeds the RTC SRAM budget.
 *
 * The ESP32-C3 provides approximately 4 KB of usable sleep-persistent map
 * storage.  This program declares a single ARRAY map with 2 048 uint32_t
 * entries.  Sonde's firmware sizes each entry as (key_size + value_size),
 * so: 2 048 × (4 + 4) = 16 384 bytes — well above the ~4 KB budget.
 *
 * Even under a value-only sizing model (2 048 × 4 = 8 192 bytes) the map
 * still clearly exceeds the budget, making this test robust regardless of
 * the exact allocation strategy.
 *
 * The gateway's ingestion pipeline (or the node firmware on install) must
 * reject this program because its map footprint exceeds the platform limit.
 *
 * Corresponds to oversized_map_program in docs/node-validation.md §2.4.
 * Used by test T-N616 (map memory budget enforcement).
 */

#include "include/sonde_helpers.h"

/**
 * Oversized map: 2 048 entries × (4-byte key + 4-byte value) = 16 384 bytes.
 * This far exceeds the ~4 KB RTC SRAM budget on the ESP32-C3 reference platform.
 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 2048);
    __type(key, __u32);
    __type(value, __u32);
} big_map SEC(".maps");

SEC("sonde")
int program(struct sonde_context *ctx)
{
    (void)ctx;

    __u32  key = 0;
    __u32 *val = map_lookup_elem(&big_map, &key);
    (void)val;

    return 0;
}
