// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Sonde-owned helpers for Prevail legacy map-section parsing.
//!
//! The gateway verifier integration needs stable abstract map identities and
//! accurate map descriptors during ELF ingestion, but it never executes BPF
//! map operations or talks to a kernel verifier. We therefore assign synthetic
//! map FDs deterministically from map equivalence keys rather than delegating
//! to Linux-specific map creation code.

use std::collections::BTreeMap;

use prevail::elf_loader::UnmarshalError;
use prevail::spec::type_descriptors::{
    EbpfMapDescriptor, EbpfMapType, EbpfMapValueType, EquivalenceKey,
};

const CONFORMANCE_BASE32: u32 = 0x01;
const CONFORMANCE_BASE64: u32 = 0x02;
const CONFORMANCE_ATOMIC32: u32 = 0x04;
const CONFORMANCE_ATOMIC64: u32 = 0x08;
const CONFORMANCE_DIVMUL32: u32 = 0x10;
const CONFORMANCE_DIVMUL64: u32 = 0x20;
const CONFORMANCE_PACKET: u32 = 0x40;
const NO_INNER_MAP_FD: i32 = -1;

/// Supported instruction-conformance groups for sonde verification.
pub(crate) const SUPPORTED_CONFORMANCE_GROUPS: u32 = CONFORMANCE_BASE32
    | CONFORMANCE_BASE64
    | CONFORMANCE_ATOMIC32
    | CONFORMANCE_ATOMIC64
    | CONFORMANCE_DIVMUL32
    | CONFORMANCE_DIVMUL64
    | CONFORMANCE_PACKET;

/// Legacy map-definition record layout stored in ELF `.maps` sections.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct LegacyMapDef {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
    inner_map_idx: u32,
    numa_node: u32,
}

/// Return the legacy `.maps` record size expected by Prevail.
pub(crate) fn legacy_map_record_size() -> usize {
    std::mem::size_of::<LegacyMapDef>()
}

/// Parse the raw `.maps` section into verifier map descriptors.
pub(crate) fn parse_legacy_maps_section<F>(
    map_descriptors: &mut Vec<EbpfMapDescriptor>,
    data: &[u8],
    record_size: usize,
    count: usize,
    cache: &mut BTreeMap<EquivalenceKey, i32>,
    map_type_for: F,
) where
    F: Fn(u32) -> EbpfMapType,
{
    let mut mapdefs = Vec::with_capacity(count);
    for i in 0..count {
        let src_offset = i * record_size;
        let def = parse_map_def_record(&data[src_offset..], record_size);
        mapdefs.push(def);
    }

    for def in &mapdefs {
        let map_type = map_type_for(def.map_type);
        let inner_map_fd = if map_type.value_type == EbpfMapValueType::Map {
            def.inner_map_idx as i32
        } else {
            NO_INNER_MAP_FD
        };
        let original_fd = create_synthetic_map_fd(
            &map_type,
            def.key_size,
            def.value_size,
            def.max_entries,
            cache,
        );
        map_descriptors.push(EbpfMapDescriptor {
            original_fd,
            map_type: def.map_type,
            key_size: def.key_size,
            value_size: def.value_size,
            max_entries: def.max_entries,
            // For map-of-maps, this holds the referenced map index until the
            // resolve pass runs. Ordinary maps carry no inner-map reference.
            inner_map_fd,
        });
    }
}

/// Resolve inner-map references after all `.maps` records are parsed.
pub(crate) fn resolve_inner_map_references(
    map_descriptors: &mut [EbpfMapDescriptor],
) -> Result<(), UnmarshalError> {
    let len = map_descriptors.len();
    for i in 0..len {
        let inner = map_descriptors[i].inner_map_fd;
        if inner == NO_INNER_MAP_FD {
            continue;
        }
        if inner < 0 || (inner as usize) >= len {
            return Err(UnmarshalError(format!(
                "bad inner map index {} for map {}",
                inner, i
            )));
        }
        map_descriptors[i].inner_map_fd = map_descriptors[inner as usize].original_fd;
    }
    Ok(())
}

/// Find a map descriptor by synthetic original FD.
pub(crate) fn find_map_descriptor(
    map_descriptors: &[EbpfMapDescriptor],
    map_fd: i32,
) -> Option<&EbpfMapDescriptor> {
    map_descriptors
        .iter()
        .find(|desc| desc.original_fd == map_fd)
}

fn parse_map_def_record(record: &[u8], record_size: usize) -> LegacyMapDef {
    let mut padded = [0u8; std::mem::size_of::<LegacyMapDef>()];
    let copy_len = record_size.min(padded.len()).min(record.len());
    padded[..copy_len].copy_from_slice(&record[..copy_len]);

    let mut fields = [0u32; 7];
    for (idx, chunk) in padded.chunks_exact(4).take(7).enumerate() {
        fields[idx] = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }

    LegacyMapDef {
        map_type: fields[0],
        key_size: fields[1],
        value_size: fields[2],
        max_entries: fields[3],
        map_flags: fields[4],
        inner_map_idx: fields[5],
        numa_node: fields[6],
    }
}

fn create_synthetic_map_fd(
    map_type: &EbpfMapType,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    cache: &mut BTreeMap<EquivalenceKey, i32>,
) -> i32 {
    let equiv = EquivalenceKey {
        value_type: map_type.value_type,
        key_size,
        value_size,
        max_entries: if map_type.is_array { max_entries } else { 0 },
    };
    let next_fd = cache.len() as i32 + 1;
    *cache.entry(equiv).or_insert(next_fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prevail::spec::type_descriptors::{EbpfMapType, EbpfMapValueType};

    fn array_map_type(platform_specific_type: u32) -> EbpfMapType {
        EbpfMapType {
            platform_specific_type,
            name: "array".to_string(),
            is_array: true,
            value_type: EbpfMapValueType::Any,
        }
    }

    #[test]
    fn parse_legacy_maps_section_builds_descriptor() {
        let mut descriptors = Vec::new();
        let mut cache = BTreeMap::new();
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u32.to_ne_bytes());
        raw.extend_from_slice(&4u32.to_ne_bytes());
        raw.extend_from_slice(&16u32.to_ne_bytes());
        raw.extend_from_slice(&2u32.to_ne_bytes());
        raw.extend_from_slice(&0u32.to_ne_bytes());
        raw.extend_from_slice(&0u32.to_ne_bytes());
        raw.extend_from_slice(&0u32.to_ne_bytes());

        parse_legacy_maps_section(
            &mut descriptors,
            &raw,
            legacy_map_record_size(),
            1,
            &mut cache,
            array_map_type,
        );

        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].map_type, 1);
        assert_eq!(descriptors[0].key_size, 4);
        assert_eq!(descriptors[0].value_size, 16);
        assert_eq!(descriptors[0].max_entries, 2);
        assert_eq!(descriptors[0].original_fd, 1);
        assert_eq!(descriptors[0].inner_map_fd, NO_INNER_MAP_FD);
    }

    #[test]
    fn resolve_inner_map_references_leaves_ordinary_maps_without_inner_map() {
        let mut descriptors = vec![EbpfMapDescriptor {
            original_fd: 7,
            map_type: 1,
            key_size: 4,
            value_size: 16,
            max_entries: 1,
            inner_map_fd: NO_INNER_MAP_FD,
        }];

        resolve_inner_map_references(&mut descriptors).unwrap();

        assert_eq!(descriptors[0].inner_map_fd, NO_INNER_MAP_FD);
    }
}
