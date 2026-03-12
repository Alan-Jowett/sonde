// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::{NodeError, NodeResult};
use sonde_protocol::MapDef;

/// The only supported map type. Other types are rejected at allocation time.
const BPF_MAP_TYPE_ARRAY: u32 = 1;

/// Expected key size for array maps (u32 index).
const ARRAY_MAP_KEY_SIZE: u32 = 4;

/// A single map instance allocated in sleep-persistent memory.
#[derive(Debug)]
pub struct MapInstance {
    pub def: MapDef,
    /// Backing storage: `max_entries * (key_size + value_size)` bytes.
    /// Guaranteed 8-byte aligned so BPF programs can safely cast value
    /// pointers to u32/u64 types.
    data: Vec<u8>,
    /// Size of one entry (key_size + value_size).
    entry_size: usize,
}

/// Allocate a zero-initialized byte buffer with 8-byte alignment.
///
/// Standard `Vec<u8>` only guarantees 1-byte alignment. BPF programs
/// may cast map value pointers to wider types (u32, u64), so we
/// allocate as `Vec<u64>` and reinterpret as bytes.
fn allocate_aligned(size: usize) -> Vec<u8> {
    if size == 0 {
        return Vec::new();
    }
    let u64_count = size.div_ceil(8);
    let mut v: Vec<u64> = vec![0u64; u64_count];
    let ptr = v.as_mut_ptr() as *mut u8;
    let cap_bytes = v.capacity() * 8;
    core::mem::forget(v);
    // SAFETY: Vec<u64> guarantees 8-byte alignment. We reconstruct a
    // Vec<u8> with the same allocation but byte-granularity length.
    // The allocator will free with the correct layout since the
    // capacity preserves the original u64 allocation.
    unsafe { Vec::from_raw_parts(ptr, size, cap_bytes) }
}

impl MapInstance {
    /// Look up a value by key index. Returns a slice pointing to the value,
    /// or `None` if the key is out of bounds.
    pub fn lookup(&self, key: u32) -> Option<&[u8]> {
        if key >= self.def.max_entries {
            return None;
        }
        let offset = (key as usize) * self.entry_size + self.def.key_size as usize;
        let end = offset + self.def.value_size as usize;
        if end > self.data.len() {
            return None;
        }
        Some(&self.data[offset..end])
    }

    /// Update a value by key index. Returns `Ok(())` on success.
    pub fn update(&mut self, key: u32, value: &[u8]) -> NodeResult<()> {
        if key >= self.def.max_entries {
            return Err(NodeError::MapKeyOutOfBounds {
                key,
                max_entries: self.def.max_entries,
            });
        }
        if value.len() != self.def.value_size as usize {
            return Err(NodeError::MapValueSizeMismatch {
                expected: self.def.value_size,
                actual: value.len(),
            });
        }
        let offset = (key as usize) * self.entry_size + self.def.key_size as usize;
        let end = offset + self.def.value_size as usize;
        if end > self.data.len() {
            return Err(NodeError::MapKeyOutOfBounds {
                key,
                max_entries: self.def.max_entries,
            });
        }
        self.data[offset..end].copy_from_slice(value);
        Ok(())
    }

    /// Get a raw pointer to this map's data for BPF LDDW relocation.
    pub fn data_ptr(&self) -> u64 {
        self.data.as_ptr() as u64
    }

    /// Total bytes used by this map instance.
    pub fn storage_bytes(&self) -> usize {
        self.data.len()
    }
}

/// Manages all map instances for the current program.
///
/// **Persistence contract:** On real hardware the caller must keep the
/// `MapStorage` instance in RTC slow SRAM (or an equivalent sleep-
/// persistent region) so that map contents survive deep sleep (ND-0603).
/// The current implementation uses heap-backed `Vec` storage, which is
/// suitable for host-based testing. The ESP-IDF platform layer will
/// replace this with a fixed RTC SRAM buffer at integration time.
///
/// `run_wake_cycle()` accepts `&mut MapStorage` from the caller and only
/// re-allocates maps when a new program is installed (not every cycle),
/// preserving map data across normal wake/sleep transitions.
pub struct MapStorage {
    maps: Vec<MapInstance>,
    /// Cached runtime pointers, updated on `allocate()`.
    cached_ptrs: Vec<u64>,
    budget_bytes: usize,
}

impl MapStorage {
    /// Create a new MapStorage with the given memory budget.
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            maps: Vec::new(),
            cached_ptrs: Vec::new(),
            budget_bytes,
        }
    }

    /// Get the configured memory budget in bytes.
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Calculate the total storage required for a set of map definitions.
    /// Returns `None` if arithmetic overflows (malformed map definitions).
    fn required_bytes_checked(map_defs: &[MapDef]) -> Option<usize> {
        let mut total: usize = 0;
        for def in map_defs {
            let entry_size = (def.key_size as usize).checked_add(def.value_size as usize)?;
            let map_size = entry_size.checked_mul(def.max_entries as usize)?;
            total = total.checked_add(map_size)?;
        }
        Some(total)
    }

    /// Calculate the total storage required for a set of map definitions.
    /// Saturates to `usize::MAX` on overflow.
    pub fn required_bytes(map_defs: &[MapDef]) -> usize {
        Self::required_bytes_checked(map_defs).unwrap_or(usize::MAX)
    }

    /// Validate map definitions: checks type, key_size, and arithmetic.
    ///
    /// Call this before committing program installs to ensure the maps
    /// are compatible with this platform.
    pub fn validate_map_defs(map_defs: &[MapDef]) -> NodeResult<()> {
        for def in map_defs {
            if def.map_type != BPF_MAP_TYPE_ARRAY {
                return Err(NodeError::ProgramDecodeFailed(format!(
                    "unsupported map_type {}: only BPF_MAP_TYPE_ARRAY (1) is supported",
                    def.map_type
                )));
            }
            if def.key_size != ARRAY_MAP_KEY_SIZE {
                return Err(NodeError::ProgramDecodeFailed(format!(
                    "array map key_size must be 4 (u32), got {}",
                    def.key_size
                )));
            }
        }
        // Also verify arithmetic doesn't overflow
        if Self::required_bytes_checked(map_defs).is_none() {
            return Err(NodeError::ProgramDecodeFailed(
                "invalid map definitions: size calculation overflowed".into(),
            ));
        }
        Ok(())
    }

    /// Allocate map storage from map definitions.
    ///
    /// Returns an error if map definitions are invalid (unsupported type,
    /// wrong key_size, arithmetic overflow) or exceed the budget.
    /// On success, all maps are zero-initialized.
    pub fn allocate(&mut self, map_defs: &[MapDef]) -> NodeResult<()> {
        Self::validate_map_defs(map_defs)?;

        let required = Self::required_bytes_checked(map_defs).ok_or_else(|| {
            NodeError::ProgramDecodeFailed(
                "invalid map definitions: size calculation overflowed".into(),
            )
        })?;
        if required > self.budget_bytes {
            return Err(NodeError::MapBudgetExceeded {
                required,
                available: self.budget_bytes,
            });
        }

        let mut maps = Vec::with_capacity(map_defs.len());
        for def in map_defs {
            let entry_size = (def.key_size as usize)
                .checked_add(def.value_size as usize)
                .expect("overflow already checked");
            let total_size = entry_size
                .checked_mul(def.max_entries as usize)
                .expect("overflow already checked");
            maps.push(MapInstance {
                def: def.clone(),
                data: allocate_aligned(total_size),
                entry_size,
            });
        }
        self.maps = maps;
        self.cached_ptrs = self.maps.iter().map(|m| m.data_ptr()).collect();
        Ok(())
    }

    /// Get a reference to a map by index.
    pub fn get(&self, index: usize) -> Option<&MapInstance> {
        self.maps.get(index)
    }

    /// Get a mutable reference to a map by index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut MapInstance> {
        self.maps.get_mut(index)
    }

    /// Get the runtime pointers for all maps (for LDDW relocation).
    /// Returns a cached slice — no allocation per call.
    pub fn map_pointers(&self) -> &[u64] {
        &self.cached_ptrs
    }

    /// Number of allocated maps.
    pub fn map_count(&self) -> usize {
        self.maps.len()
    }

    /// Check if the current map layout matches the given definitions.
    ///
    /// Returns `true` if the number of maps and each map's definition
    /// (type, key_size, value_size, max_entries) match exactly. Used
    /// to detect when re-allocation is needed (e.g. after an ephemeral
    /// program ran with different map definitions).
    pub fn layout_matches(&self, map_defs: &[MapDef]) -> bool {
        if self.maps.len() != map_defs.len() {
            return false;
        }
        self.maps
            .iter()
            .zip(map_defs.iter())
            .all(|(instance, def)| instance.def == *def)
    }

    /// Clear all map data to zero (used on program change).
    pub fn clear_all(&mut self) {
        for map in &mut self.maps {
            map.data.fill(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn array_map_def(value_size: u32, max_entries: u32) -> MapDef {
        MapDef {
            map_type: 1, // BPF_MAP_TYPE_ARRAY
            key_size: 4,
            value_size,
            max_entries,
        }
    }

    #[test]
    fn test_allocate_single_map() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(8, 16)]; // 16 entries * (4+8) = 192 bytes
        ms.allocate(&defs).unwrap();
        assert_eq!(ms.map_count(), 1);
        assert_eq!(ms.get(0).unwrap().storage_bytes(), 192);
    }

    #[test]
    fn test_allocate_exceeds_budget() {
        let mut ms = MapStorage::new(100);
        let defs = vec![array_map_def(8, 16)]; // needs 192 bytes
        let result = ms.allocate(&defs);
        assert!(matches!(
            result,
            Err(NodeError::MapBudgetExceeded {
                required: 192,
                available: 100
            })
        ));
    }

    #[test]
    fn test_lookup_and_update() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 4)]; // 4 entries of 4-byte values
        ms.allocate(&defs).unwrap();

        // Initially zero
        let val = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(val, &[0, 0, 0, 0]);

        // Update key 2
        ms.get_mut(0)
            .unwrap()
            .update(2, &[0xDE, 0xAD, 0xBE, 0xEF])
            .unwrap();

        // Read back
        let val = ms.get(0).unwrap().lookup(2).unwrap();
        assert_eq!(val, &[0xDE, 0xAD, 0xBE, 0xEF]);

        // Other keys still zero
        let val = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(val, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_lookup_out_of_bounds() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 4)];
        ms.allocate(&defs).unwrap();
        assert!(ms.get(0).unwrap().lookup(4).is_none());
        assert!(ms.get(0).unwrap().lookup(100).is_none());
    }

    #[test]
    fn test_update_out_of_bounds() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 4)];
        ms.allocate(&defs).unwrap();
        let result = ms.get_mut(0).unwrap().update(4, &[1, 2, 3, 4]);
        assert!(matches!(result, Err(NodeError::MapKeyOutOfBounds { .. })));
    }

    #[test]
    fn test_multiple_maps() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![
            array_map_def(4, 4),  // 4 * (4+4) = 32 bytes
            array_map_def(16, 2), // 2 * (4+16) = 40 bytes
        ];
        ms.allocate(&defs).unwrap();
        assert_eq!(ms.map_count(), 2);
        assert_eq!(ms.map_pointers().len(), 2);
    }

    #[test]
    fn test_clear_all() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 4)];
        ms.allocate(&defs).unwrap();
        ms.get_mut(0).unwrap().update(0, &[1, 2, 3, 4]).unwrap();
        ms.clear_all();
        let val = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(val, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_required_bytes() {
        let defs = vec![
            array_map_def(8, 16), // 16 * (4+8) = 192
            array_map_def(32, 4), // 4 * (4+32) = 144
        ];
        assert_eq!(MapStorage::required_bytes(&defs), 336);
    }
}
