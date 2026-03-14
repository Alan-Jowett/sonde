// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/**
 * oversized_map — declares a map that exceeds the RTC SRAM budget.
 *
 * The ESP32-C3 provides approximately 4 KB of usable sleep-persistent map
 * storage.  This program declares a single ARRAY map with 1 024 uint32_t
 * entries, requiring 1 024 × (4 + 4) = 8 192 bytes — more than double the
 * available budget.
 *
 * The gateway's ingestion pipeline (or the node firmware on install) must
 * reject this program because its map footprint exceeds the platform limit.
 *
 * Corresponds to oversized_map_program in docs/node-validation.md §2.4.
 * Used by test T-N616 (map memory budget enforcement).
 */

#include "include/sonde_helpers.h"

/**
 * Oversized map: 1 024 entries × (4-byte key + 4-byte value) = 8 192 bytes.
 * This exceeds the ~4 KB RTC SRAM budget on the ESP32-C3 reference platform.
 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1024);
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
