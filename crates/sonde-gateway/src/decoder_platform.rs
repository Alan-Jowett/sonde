// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Decoder-specific Prevail verifier platform (GW-1901).
//!
//! Defines helper prototypes for the restricted decoder BPF helper set
//! (IDs 10, 11, 16, 18) so that the Prevail verifier rejects decoder
//! programs that reference hardware or network helpers.

use prevail::elf_loader::UnmarshalError;
use prevail::linux::linux_platform::LinuxPlatform;
use prevail::linux::spec_prototypes::HelperPrototype;
use prevail::platform::EbpfPlatform;
use prevail::spec::config::EbpfVerifierOptions;
use prevail::spec::ebpf_base::{EbpfArgumentType, EbpfContextDescriptor, EbpfReturnType};
use prevail::spec::type_descriptors::{
    EbpfMapDescriptor, EbpfMapType, EbpfMapValueType, EbpfProgramType,
};

use EbpfArgumentType as Arg;
use EbpfReturnType as Ret;

/// Context descriptor for `struct decoder_context` (16 bytes).
///
/// Layout: `{ input_data: u64 (8B), input_end: u64 (8B) }`.
/// `input_data` is a read-only pointer to the raw APP_DATA blob start;
/// `input_end` points past the last byte. The Prevail verifier uses the
/// `data`/`end` offsets to track packet-style pointer bounds.
static DECODER_CONTEXT: EbpfContextDescriptor = EbpfContextDescriptor {
    size: 16,
    data: 0, // input_data at offset 0
    end: 8,  // input_end at offset 8
    meta: -1,
};

/// Sentinel prototype for unsupported helper IDs.
static UNSUPPORTED_HELPER: HelperPrototype = HelperPrototype {
    name: "unsupported",
    return_type: Ret::Unsupported,
    argument_type: [Arg::DontCare; 5],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: true,
};

// ── Permitted decoder helpers ───────────────────────────────────────────

/// Helper 10: map_lookup_elem(*map, *key) -> *value or null
static DECODER_MAP_LOOKUP: HelperPrototype = HelperPrototype {
    name: "map_lookup_elem",
    return_type: Ret::PtrToMapValueOrNull,
    argument_type: [
        Arg::PtrToMap,
        Arg::PtrToMapKey,
        Arg::DontCare,
        Arg::DontCare,
        Arg::DontCare,
    ],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: false,
};

/// Helper 11: map_update_elem(*map, *key, *value) -> i32
static DECODER_MAP_UPDATE: HelperPrototype = HelperPrototype {
    name: "map_update_elem",
    return_type: Ret::Integer,
    argument_type: [
        Arg::PtrToMap,
        Arg::PtrToMapKey,
        Arg::PtrToMapValue,
        Arg::DontCare,
        Arg::DontCare,
    ],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: false,
};

/// Helper 16: bpf_trace_printk(*fmt, fmt_len) -> i32
static DECODER_TRACE_PRINTK: HelperPrototype = HelperPrototype {
    name: "bpf_trace_printk",
    return_type: Ret::Integer,
    argument_type: [
        Arg::PtrToReadableMem,
        Arg::ConstSize,
        Arg::DontCare,
        Arg::DontCare,
        Arg::DontCare,
    ],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: false,
};

/// Helper 18: emit_reading(*name, name_len, value_i64) -> i32
static DECODER_EMIT_READING: HelperPrototype = HelperPrototype {
    name: "emit_reading",
    return_type: Ret::Integer,
    argument_type: [
        Arg::PtrToReadableMem,
        Arg::ConstSize,
        Arg::Anything,
        Arg::DontCare,
        Arg::DontCare,
    ],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: false,
};

/// Decoder BPF verifier platform (GW-1901).
///
/// Wraps `LinuxPlatform` for ELF/map parsing and overrides helper prototypes
/// and program type resolution with decoder-specific definitions.
///
/// Only helpers 10 (`map_lookup_elem`), 11 (`map_update_elem`),
/// 16 (`bpf_trace_printk`), and 18 (`emit_reading`) are permitted.
/// All other helper IDs are rejected by the verifier.
pub struct DecoderPlatform {
    inner: LinuxPlatform,
    /// Mirror of map descriptors populated via `sync_map_descriptors`.
    map_descriptors: Vec<EbpfMapDescriptor>,
}

impl DecoderPlatform {
    pub fn new() -> Self {
        Self {
            inner: LinuxPlatform::new(),
            map_descriptors: Vec::new(),
        }
    }

    /// Mirror the full set of map descriptors from the ELF loader into this
    /// platform, replacing any previously stored descriptors.
    ///
    /// Same pattern as [`SondePlatform::sync_map_descriptors`].
    pub fn sync_map_descriptors(&mut self, descriptors: &[EbpfMapDescriptor]) {
        self.map_descriptors = descriptors.to_vec();
    }
}

impl Default for DecoderPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl EbpfPlatform for DecoderPlatform {
    fn get_program_type(&self, _section: &str, _path: &str) -> EbpfProgramType {
        EbpfProgramType {
            name: "decoder".to_string(),
            context_descriptor: Some(&DECODER_CONTEXT),
            platform_specific_data: 0,
            section_prefixes: vec!["decoder".to_string()],
            is_privileged: false,
        }
    }

    fn get_helper_prototype(&self, n: i32) -> &HelperPrototype {
        match n {
            10 => &DECODER_MAP_LOOKUP,
            11 => &DECODER_MAP_UPDATE,
            16 => &DECODER_TRACE_PRINTK,
            18 => &DECODER_EMIT_READING,
            _ => &UNSUPPORTED_HELPER,
        }
    }

    fn is_helper_usable(&self, n: i32) -> bool {
        matches!(n, 10 | 11 | 16 | 18)
    }

    fn map_record_size(&self) -> usize {
        self.inner.map_record_size()
    }

    fn parse_maps_section(
        &mut self,
        descriptors: &mut Vec<EbpfMapDescriptor>,
        data: &[u8],
        record_size: usize,
        count: usize,
        options: &EbpfVerifierOptions,
    ) {
        self.inner
            .parse_maps_section(descriptors, data, record_size, count, options);
    }

    fn resolve_inner_map_references(
        &self,
        descriptors: &mut Vec<EbpfMapDescriptor>,
    ) -> Result<(), UnmarshalError> {
        self.inner.resolve_inner_map_references(descriptors)
    }

    fn get_map_descriptor(&self, map_fd: i32) -> Option<&EbpfMapDescriptor> {
        if let Some(desc) = self
            .map_descriptors
            .iter()
            .find(|d| d.original_fd == map_fd)
        {
            return Some(desc);
        }
        self.inner.get_map_descriptor(map_fd)
    }

    fn get_map_type(&self, platform_specific_type: u32) -> EbpfMapType {
        match platform_specific_type {
            0 => EbpfMapType {
                platform_specific_type: 0,
                name: "global".to_string(),
                is_array: true,
                value_type: EbpfMapValueType::Any,
            },
            1 => EbpfMapType {
                platform_specific_type: 1,
                name: "array".to_string(),
                is_array: true,
                value_type: EbpfMapValueType::Any,
            },
            other => EbpfMapType {
                platform_specific_type: other,
                name: format!("map_type_{other}"),
                is_array: false,
                value_type: EbpfMapValueType::Any,
            },
        }
    }

    fn supported_conformance_groups(&self) -> u32 {
        self.inner.supported_conformance_groups()
    }
}
