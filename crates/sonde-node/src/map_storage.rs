// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#[cfg(feature = "esp")]
use core::marker::PhantomData;

use crate::error::{NodeError, NodeResult};
use sonde_protocol::MapDef;

/// The only supported map type. Other types are rejected at allocation time.
const BPF_MAP_TYPE_ARRAY: u32 = 1;

/// Expected key size for array maps (u32 index).
const ARRAY_MAP_KEY_SIZE: u32 = 4;

/// Total bytes reserved for BPF map data.
///
/// On ESP32 this maps directly to the size of `MAP_BACKING` in RTC slow SRAM.
/// The ESP32-C3 has 8 KB of RTC slow SRAM; ~4 KB is used by firmware state,
/// flags, and the `RtcMapLayout` record, leaving ~4 KB for map data
/// (bpf-environment.md §10, node-design.md §13).
pub const MAP_BUDGET: usize = 4 * 1024;

/// Maximum number of BPF maps a single program may define.
///
/// Re-exported from [`bpf_dispatch`](crate::bpf_dispatch) to ensure the RTC
/// layout record and the dispatch-time pointer index share one source of truth.
pub use crate::bpf_dispatch::MAX_MAPS;

/// Static backing buffer for all BPF map data.
///
/// Placed in RTC slow SRAM (`.rtc.data`) on ESP32 so that map contents
/// survive deep sleep (ND-0603).  Every call to `MapStorage::allocate()`
/// carves non-overlapping regions from this buffer rather than heap-
/// allocating, so data written in one wake cycle is still present after
/// the chip wakes from deep sleep — provided the same program is running
/// and `layout_matches()` is true (no re-allocation).
#[cfg(feature = "esp")]
#[link_section = ".rtc.data"]
static mut MAP_BACKING: [u8; MAP_BUDGET] = [0u8; MAP_BUDGET];

/// Layout record stored in RTC slow SRAM alongside `MAP_BACKING`.
///
/// Records the `MapDef` entries for the currently allocated program so that,
/// after a deep-sleep wake, `MapStorage::from_rtc()` can rebuild the
/// `MapStorage` metadata (map definitions, offsets, pointers) and set
/// `layout_matches()` to return `true` — thereby preventing `allocate()`
/// from being called and zeroing out the preserved map data.
///
/// `map_count == 0` means no program is installed / layout is invalid.
#[cfg(feature = "esp")]
#[repr(C)]
struct RtcMapLayout {
    map_count: u32,
    defs: [MapDef; MAX_MAPS],
}

#[cfg(feature = "esp")]
impl RtcMapLayout {
    const fn zero() -> Self {
        Self {
            map_count: 0,
            defs: [MapDef {
                map_type: 0,
                key_size: 0,
                value_size: 0,
                max_entries: 0,
            }; MAX_MAPS],
        }
    }
}

#[cfg(feature = "esp")]
#[link_section = ".rtc.data"]
static mut MAP_LAYOUT: RtcMapLayout = RtcMapLayout::zero();

// ---------------------------------------------------------------------------
// Platform-specific map data backing
// ---------------------------------------------------------------------------

/// On host/test builds map data lives in a heap-allocated `Vec<u8>`.
#[cfg(not(feature = "esp"))]
type MapData = Vec<u8>;

/// On ESP firmware map data lives in a raw slice into `MAP_BACKING`.
#[cfg(feature = "esp")]
type MapData = RtcSlice;

/// A thin view of a contiguous region inside `MAP_BACKING`.
///
/// Holds a raw pointer and length instead of `&'static mut [u8]` to allow
/// the region to be "re-claimed" by a subsequent `allocate()` call without
/// violating Rust's aliasing rules (raw pointers carry no borrow obligations).
///
/// # Safety
///
/// All instances are created by `make_map_data` (called from `allocate()`)
/// or by `from_rtc()` during deep-sleep recovery. Both paths guarantee
/// that each `RtcSlice` covers a unique, non-overlapping range of
/// `MAP_BACKING`. Safe code outside this module cannot create arbitrary
/// slices into `MAP_BACKING`.
///
/// The singleton invariant — exactly one live `MapStorage` on ESP builds —
/// ensures that only one set of `RtcSlice` values exists at a time.
/// The wake-cycle engine is single-threaded, so no concurrent access
/// can occur. `RtcSlice` must not be moved across threads; the
/// `PhantomData<*const ()>` marker documents this intent.
#[cfg(feature = "esp")]
pub(crate) struct RtcSlice {
    ptr: *mut u8,
    len: usize,
    /// Marker field documenting that `RtcSlice` should not be transferred
    /// across thread boundaries — the backing `MAP_BACKING` buffer has no
    /// synchronisation.
    _not_send_sync: PhantomData<*const ()>,
}

#[cfg(feature = "esp")]
impl RtcSlice {
    fn len(&self) -> usize {
        self.len
    }

    fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    fn fill(&mut self, val: u8) {
        // SAFETY: ptr..ptr+len is a valid, uniquely-owned range inside MAP_BACKING.
        unsafe { core::ptr::write_bytes(self.ptr, val, self.len) }
    }
}

#[cfg(feature = "esp")]
impl core::ops::Index<core::ops::Range<usize>> for RtcSlice {
    type Output = [u8];
    fn index(&self, range: core::ops::Range<usize>) -> &[u8] {
        assert!(
            range.start <= range.end && range.end <= self.len,
            "RtcSlice index out of bounds"
        );
        // SAFETY: range is within bounds; ptr is valid for the slice lifetime.
        unsafe { core::slice::from_raw_parts(self.ptr.add(range.start), range.end - range.start) }
    }
}

#[cfg(feature = "esp")]
impl core::ops::IndexMut<core::ops::Range<usize>> for RtcSlice {
    fn index_mut(&mut self, range: core::ops::Range<usize>) -> &mut [u8] {
        assert!(
            range.start <= range.end && range.end <= self.len,
            "RtcSlice index_mut out of bounds"
        );
        // SAFETY: range is within bounds; ptr is valid and uniquely owned.
        unsafe {
            core::slice::from_raw_parts_mut(self.ptr.add(range.start), range.end - range.start)
        }
    }
}

#[cfg(feature = "esp")]
impl core::fmt::Debug for RtcSlice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RtcSlice(len={})", self.len)
    }
}

// ---------------------------------------------------------------------------
// Map data construction helper
// ---------------------------------------------------------------------------

/// Create the backing storage for a map of `size` bytes.
///
/// * **Non-ESP**: allocates a zero-filled heap `Vec<u8>`.
/// * **ESP**: returns an `RtcSlice` pointing at `MAP_BACKING[offset..offset+size]`
///   after zero-initialising that region.
///
/// `offset` is ignored on non-ESP builds.
#[cfg(not(feature = "esp"))]
fn make_map_data(size: usize, _offset: usize) -> MapData {
    vec![0u8; size]
}

#[cfg(feature = "esp")]
fn make_map_data(size: usize, offset: usize) -> MapData {
    // SAFETY: `allocate()` guarantees that `offset + size <= MAP_BUDGET` and
    // that every call to `make_map_data` receives a unique, non-overlapping
    // `[offset, offset+size)` range within `MAP_BACKING`.
    unsafe {
        let ptr = MAP_BACKING.as_mut_ptr().add(offset);
        core::ptr::write_bytes(ptr, 0, size);
        RtcSlice {
            ptr,
            len: size,
            _not_send_sync: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------

/// A single map instance allocated in sleep-persistent memory.
#[derive(Debug)]
pub struct MapInstance {
    pub def: MapDef,
    /// Backing storage: `max_entries * (key_size + value_size)` bytes.
    data: MapData,
    /// Size of one entry (key_size + value_size).
    entry_size: usize,
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
/// **Persistence contract (ND-0603):** Map data survives deep sleep because
/// `MapInstance` backing storage lives in `MAP_BACKING`, which is placed in
/// RTC slow SRAM (`.rtc.data`) on ESP32 firmware builds.  On host/test
/// builds each map is heap-backed (`Vec<u8>`), which is sufficient for
/// unit testing.
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

    /// Reconstruct `MapStorage` from the RTC slow SRAM layout record.
    ///
    /// On ESP32, the layout record (`MAP_LAYOUT`) is written by `allocate()`
    /// and survives deep sleep.  This method reads that record and builds
    /// the `maps` metadata (definitions + `RtcSlice` pointers) **without
    /// zero-initialising `MAP_BACKING`**, so the map data accumulated in the
    /// previous wake cycle is preserved (ND-0603).
    ///
    /// Returns `None` when:
    /// - The layout record is empty (`map_count == 0`) — cold boot / no program.
    /// - The record is corrupt (count > `MAX_MAPS`).
    /// - The stored layout exceeds `budget_bytes`.
    ///
    /// The caller should fall back to `MapStorage::new(budget_bytes)` on
    /// `None` and let the normal `allocate()`-on-mismatch path handle
    /// initialisation.
    #[cfg(feature = "esp")]
    pub fn from_rtc(budget_bytes: usize) -> Option<Self> {
        // SAFETY: MAP_LAYOUT is only written by `allocate()` in this
        // single-threaded wake-cycle engine. Volatile reads pair with the
        // volatile writes in write_rtc_layout() to prevent the compiler
        // from reordering or eliding accesses across the commit boundary.
        let map_count =
            unsafe { core::ptr::read_volatile(&raw const MAP_LAYOUT.map_count) } as usize;
        // map_count == 0: cold boot / no program installed.
        // map_count > MAX_MAPS: corrupt record (MAX_MAPS is the inclusive upper bound).
        if map_count == 0 || map_count > MAX_MAPS {
            return None;
        }

        // Re-derive total bytes to validate the record is sane.
        // Use checked arithmetic so a corrupt record with huge field values
        // is rejected explicitly rather than relying on the budget check.
        let mut total_bytes: usize = 0;
        for i in 0..map_count {
            // Volatile read to pair with volatile writes in write_rtc_layout().
            let def = unsafe { core::ptr::read_volatile(&raw const MAP_LAYOUT.defs[i]) };
            let entry_size = (def.key_size as usize).checked_add(def.value_size as usize)?;
            let map_size = entry_size.checked_mul(def.max_entries as usize)?;
            total_bytes = total_bytes.checked_add(map_size)?;
        }
        if total_bytes > budget_bytes || total_bytes > MAP_BUDGET {
            return None;
        }

        // Re-validate recovered MapDef semantics (map_type, key_size, etc.)
        // so a corrupt RTC record doesn't produce MapStorage with invalid defs.
        let recovered_defs: Vec<MapDef> = (0..map_count)
            .map(|i| unsafe { core::ptr::read_volatile(&raw const MAP_LAYOUT.defs[i]) })
            .collect();
        if Self::validate_map_defs(&recovered_defs).is_err() {
            return None;
        }

        let mut maps = Vec::with_capacity(map_count);
        let mut offset: usize = 0;
        for i in 0..map_count {
            // SAFETY: index is within [0, map_count) which is ≤ MAX_MAPS.
            let def = unsafe { core::ptr::read_volatile(&raw const MAP_LAYOUT.defs[i]) };
            let entry_size = def.key_size as usize + def.value_size as usize;
            let total_size = entry_size * def.max_entries as usize;
            // Build RtcSlice without zero-filling — data from the previous
            // wake cycle is preserved in MAP_BACKING.
            // SAFETY: offset + total_size ≤ total_bytes ≤ MAP_BUDGET.
            let data = unsafe {
                RtcSlice {
                    ptr: MAP_BACKING.as_mut_ptr().add(offset),
                    len: total_size,
                    _not_send_sync: PhantomData,
                }
            };
            maps.push(MapInstance {
                def,
                data,
                entry_size,
            });
            offset += total_size;
        }

        let cached_ptrs = maps.iter().map(|m| m.data_ptr()).collect();
        Some(Self {
            maps,
            cached_ptrs,
            budget_bytes,
        })
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

    /// Validate map definitions: checks type, key_size, entry counts, and arithmetic.
    ///
    /// Call this before committing program installs to ensure the maps
    /// are compatible with this platform. Rejects zero-entry maps (which
    /// produce duplicate `data_ptr()` values and break map indexing) and
    /// zero-value-size maps (which are semantically invalid).
    pub fn validate_map_defs(map_defs: &[MapDef]) -> NodeResult<()> {
        if map_defs.len() > MAX_MAPS {
            return Err(NodeError::ProgramDecodeFailed(
                "program defines too many maps (exceeds MAX_MAPS)",
            ));
        }
        for def in map_defs {
            if def.map_type != BPF_MAP_TYPE_ARRAY {
                return Err(NodeError::ProgramDecodeFailed(
                    "unsupported map type: only BPF_MAP_TYPE_ARRAY (1) is supported",
                ));
            }
            if def.key_size != ARRAY_MAP_KEY_SIZE {
                return Err(NodeError::ProgramDecodeFailed(
                    "array map key_size must be 4 (u32)",
                ));
            }
            if def.max_entries == 0 {
                return Err(NodeError::ProgramDecodeFailed(
                    "map max_entries must be > 0: zero-entry maps produce \
                     duplicate data_ptr() values and break map indexing",
                ));
            }
            if def.value_size == 0 {
                return Err(NodeError::ProgramDecodeFailed(
                    "map value_size must be > 0: zero-byte values are not supported",
                ));
            }
        }
        // Also verify arithmetic doesn't overflow
        if Self::required_bytes_checked(map_defs).is_none() {
            return Err(NodeError::ProgramDecodeFailed(
                "invalid map definitions: size calculation overflowed",
            ));
        }
        Ok(())
    }

    /// Allocate map storage from map definitions.
    ///
    /// Returns an error if map definitions are invalid (unsupported type,
    /// wrong key_size, arithmetic overflow) or exceed the budget.
    /// On success, all maps are zero-initialized.
    ///
    /// On ESP firmware builds:
    /// - Backing storage is carved from `MAP_BACKING` in RTC slow SRAM.
    /// - The layout record (`MAP_LAYOUT`) is updated so that
    ///   `MapStorage::from_rtc()` can reconstruct the metadata on the next
    ///   wake **without** zeroing the preserved data.
    pub fn allocate(&mut self, map_defs: &[MapDef]) -> NodeResult<()> {
        Self::validate_map_defs(map_defs)?;

        let required = Self::required_bytes_checked(map_defs).ok_or(
            NodeError::ProgramDecodeFailed("invalid map definitions: size calculation overflowed"),
        )?;
        if required > self.budget_bytes {
            return Err(NodeError::MapBudgetExceeded {
                required,
                available: self.budget_bytes,
            });
        }
        // On ESP builds, also reject if required exceeds the fixed-size
        // MAP_BACKING buffer to prevent out-of-bounds writes.
        #[cfg(feature = "esp")]
        if required > MAP_BUDGET {
            return Err(NodeError::MapBudgetExceeded {
                required,
                available: MAP_BUDGET,
            });
        }

        let mut maps = Vec::with_capacity(map_defs.len());
        let mut offset: usize = 0;
        for def in map_defs {
            let entry_size = (def.key_size as usize)
                .checked_add(def.value_size as usize)
                .expect("overflow already checked");
            let total_size = entry_size
                .checked_mul(def.max_entries as usize)
                .expect("overflow already checked");
            maps.push(MapInstance {
                def: *def,
                data: make_map_data(total_size, offset),
                entry_size,
            });
            offset += total_size;
        }
        self.maps = maps;
        self.cached_ptrs = self.maps.iter().map(|m| m.data_ptr()).collect();

        // Persist the layout in RTC SRAM so from_rtc() can reconstruct
        // MapStorage after deep sleep without zeroing the map data.
        #[cfg(feature = "esp")]
        Self::write_rtc_layout(map_defs);

        Ok(())
    }

    /// Pre-populate maps with initial data from the program image.
    ///
    /// Called after `allocate()` when a new program is installed. For each
    /// map, if `initial_data[i]` is non-empty and matches `value_size`,
    /// the data is written as the value of entry 0 (the only entry in
    /// global variable maps). Entries without initial data remain
    /// zero-filled from allocation.
    pub fn apply_initial_data(&mut self, initial_data: &[Vec<u8>]) {
        for (i, data) in initial_data.iter().enumerate() {
            if data.is_empty() {
                continue;
            }
            if let Some(map) = self.maps.get_mut(i) {
                if data.len() == map.def.value_size as usize {
                    let _ = map.update(0, data);
                }
            }
        }
    }

    /// Write the current map definitions to the RTC layout record.
    ///
    /// Called by `allocate()` after successfully setting up maps.
    /// `validate_map_defs()` rejects programs with more than `MAX_MAPS`
    /// maps, so truncation cannot occur in practice.
    ///
    /// Uses an invalidate-write-commit pattern with compiler fences so
    /// that a reset mid-write never leaves `from_rtc()` with a
    /// valid-looking but inconsistent record:
    ///   1. Volatile-write `map_count = 0` (invalidate — from_rtc returns None)
    ///   2. Hardware fence (ensure invalidate is visible before defs writes)
    ///   3. Volatile-write all defs
    ///   4. Hardware fence (ensure all defs are visible before commit)
    ///   5. Volatile-write `map_count = count` (commit)
    #[cfg(feature = "esp")]
    fn write_rtc_layout(map_defs: &[MapDef]) {
        use core::sync::atomic::{fence, Ordering};
        unsafe {
            let count = map_defs.len().min(MAX_MAPS);
            // Invalidate: ensures from_rtc() returns None if we reset
            // between here and the final commit below.
            core::ptr::write_volatile(&raw mut MAP_LAYOUT.map_count, 0);
            fence(Ordering::SeqCst);
            for (i, def) in map_defs.iter().enumerate().take(count) {
                core::ptr::write_volatile(&raw mut MAP_LAYOUT.defs[i], *def);
            }
            fence(Ordering::SeqCst);
            // Commit: volatile-write map_count last.
            core::ptr::write_volatile(&raw mut MAP_LAYOUT.map_count, count as u32);
        }
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

    /// Clear all map data to zero (used on program change and factory reset).
    ///
    /// On ESP builds, also invalidates the RTC layout record so that
    /// `from_rtc()` returns `None` on the next boot. Without this, a
    /// factory reset followed by a reboot would reconstruct a stale
    /// map layout even though no resident program is installed.
    pub fn clear_all(&mut self) {
        for map in &mut self.maps {
            map.data.fill(0);
        }
        #[cfg(feature = "esp")]
        Self::invalidate_rtc_layout();
    }

    /// Set `MAP_LAYOUT.map_count = 0` so `from_rtc()` treats the record
    /// as empty on the next boot. Called by `clear_all()` during factory
    /// reset and program erase.
    #[cfg(feature = "esp")]
    fn invalidate_rtc_layout() {
        unsafe {
            core::ptr::write_volatile(&raw mut MAP_LAYOUT.map_count, 0);
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

    #[test]
    fn test_map_budget_constant() {
        // MAP_BUDGET must be positive and large enough for at least one
        // small map (1 entry of 8 bytes = 12 bytes with 4-byte key).
        const { assert!(MAP_BUDGET > 0, "MAP_BUDGET must be positive") };
        const { assert!(MAP_BUDGET >= 12, "MAP_BUDGET too small for a minimal map") };
    }

    #[test]
    fn test_max_maps_constant() {
        const { assert!(MAX_MAPS >= 1, "MAX_MAPS must allow at least one map") };
    }

    #[test]
    fn test_validate_rejects_zero_max_entries() {
        let defs = vec![array_map_def(8, 0)];
        let result = MapStorage::validate_map_defs(&defs);
        assert!(matches!(result, Err(NodeError::ProgramDecodeFailed(_))));
    }

    #[test]
    fn test_validate_rejects_zero_value_size() {
        let defs = vec![array_map_def(0, 4)];
        let result = MapStorage::validate_map_defs(&defs);
        assert!(matches!(result, Err(NodeError::ProgramDecodeFailed(_))));
    }

    /// Verify that when the same program runs again (layout_matches == true)
    /// and allocate() is *not* called, the map data from the "previous wake
    /// cycle" is intact.  On real hardware the data survives because
    /// MAP_BACKING is in RTC SRAM; here we simulate it with a Vec<u8> that
    /// stays in memory between the two simulated "wake cycles".
    #[test]
    fn test_data_preserved_when_layout_matches() {
        let defs = vec![array_map_def(4, 4)];
        let mut ms = MapStorage::new(MAP_BUDGET);
        ms.allocate(&defs).unwrap();

        // Write data in "wake cycle 1".
        ms.get_mut(0).unwrap().update(0, &[1, 2, 3, 4]).unwrap();

        // Simulate "wake cycle 2": layout matches, so allocate() is not
        // called.  The existing MapStorage (and its heap-backed data on the
        // host) is reused directly, just as the RTC-backed data on real
        // hardware would be.
        assert!(ms.layout_matches(&defs));
        // Data is preserved without calling allocate().
        let val = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(val, &[1, 2, 3, 4]);
    }

    /// Verify that allocate() zero-initialises maps (new-program path).
    #[test]
    fn test_allocate_zeroes_data() {
        let defs = vec![array_map_def(4, 4)];
        let mut ms = MapStorage::new(MAP_BUDGET);
        ms.allocate(&defs).unwrap();
        ms.get_mut(0)
            .unwrap()
            .update(0, &[0xFF, 0xFF, 0xFF, 0xFF])
            .unwrap();

        // Allocate again (simulates a new program being installed with the
        // same layout).
        ms.allocate(&defs).unwrap();
        let val = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(val, &[0, 0, 0, 0]);
    }

    #[test]
    fn test_validate_too_many_maps() {
        let defs: Vec<MapDef> = (0..MAX_MAPS + 1).map(|_| array_map_def(4, 1)).collect();
        let result = MapStorage::validate_map_defs(&defs);
        match result {
            Err(NodeError::ProgramDecodeFailed(msg)) => {
                assert!(
                    msg.contains("too many maps"),
                    "error message should mention too many maps: {msg}"
                );
            }
            Err(other) => panic!("expected ProgramDecodeFailed, got: {other}"),
            Ok(()) => panic!("expected error for too many maps"),
        }
    }

    #[test]
    fn test_validate_exactly_max_maps() {
        let defs: Vec<MapDef> = (0..MAX_MAPS).map(|_| array_map_def(4, 1)).collect();
        assert!(MapStorage::validate_map_defs(&defs).is_ok());
    }

    #[test]
    fn test_apply_initial_data_populates_map() {
        let mut ms = MapStorage::new(4096);
        // Single-entry array map with value_size=4.
        let defs = vec![array_map_def(4, 1)];
        ms.allocate(&defs).unwrap();

        let initial = vec![0xAA, 0xBB, 0xCC, 0xDD];
        ms.apply_initial_data(std::slice::from_ref(&initial));

        let stored = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(stored, &initial[..]);
    }

    #[test]
    fn test_apply_initial_data_skips_empty() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 1)];
        ms.allocate(&defs).unwrap();

        // Empty initial data — map should remain zero-filled.
        ms.apply_initial_data(&[vec![]]);

        let stored = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(
            stored,
            &[0, 0, 0, 0],
            "empty initial_data must leave map zero-filled"
        );
    }

    #[test]
    fn test_apply_initial_data_size_mismatch_ignored() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 1)];
        ms.allocate(&defs).unwrap();

        // Wrong size — should be silently ignored.
        ms.apply_initial_data(&[vec![0xFF, 0xFF]]);

        let stored = ms.get(0).unwrap().lookup(0).unwrap();
        assert_eq!(
            stored,
            &[0, 0, 0, 0],
            "mismatched initial_data must be ignored"
        );
    }

    #[test]
    fn test_apply_initial_data_multiple_maps() {
        let mut ms = MapStorage::new(4096);
        let defs = vec![array_map_def(4, 1), array_map_def(2, 1)];
        ms.allocate(&defs).unwrap();

        let data0 = vec![0x01, 0x02, 0x03, 0x04];
        let data1 = vec![0xAA, 0xBB];
        ms.apply_initial_data(&[data0.clone(), data1.clone()]);

        assert_eq!(ms.get(0).unwrap().lookup(0).unwrap(), &data0);
        assert_eq!(ms.get(1).unwrap().lookup(0).unwrap(), &data1);
    }
}
