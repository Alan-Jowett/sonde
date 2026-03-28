// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Sonde-specific Prevail verifier platform (GW-0404).
//!
//! Defines helper prototypes for sonde BPF helpers (IDs 1–16) so that the
//! Prevail verifier understands the call signatures used by sonde programs.
//! Without this, the gateway would use `LinuxPlatform` which assigns
//! different semantics to the same helper IDs.

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

/// Context descriptor for `struct sonde_context` (16 bytes, no packet data/end pointers).
static SONDE_CONTEXT: EbpfContextDescriptor = EbpfContextDescriptor {
    size: 16,
    data: -1,
    end: -1,
    meta: -1,
};

/// Sentinel prototype for helper ID 0 (unused).
static UNSUPPORTED_HELPER: HelperPrototype = HelperPrototype {
    name: "unsupported",
    return_type: Ret::Unsupported,
    argument_type: [Arg::DontCare; 5],
    reallocate_packet: false,
    context_descriptor: None,
    unsupported: true,
};

/// Helper prototypes for sonde BPF helpers 1–16.
/// Signatures match `test-programs/include/sonde_helpers.h`.
static SONDE_HELPERS: [HelperPrototype; 16] = [
    // 1: i2c_read(handle, *buf, buf_len) -> i32
    HelperPrototype {
        name: "i2c_read",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::PtrToWritableMem,
            Arg::ConstSize,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 2: i2c_write(handle, *data, data_len) -> i32
    HelperPrototype {
        name: "i2c_write",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::PtrToReadableMem,
            Arg::ConstSize,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 3: i2c_write_read(handle, *write_ptr, write_len, *read_ptr, read_len) -> i32
    HelperPrototype {
        name: "i2c_write_read",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::PtrToReadableMem,
            Arg::ConstSize,
            Arg::PtrToWritableMem,
            Arg::ConstSize,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 4: spi_transfer(handle, *tx, *rx, len) -> i32
    HelperPrototype {
        name: "spi_transfer",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::PtrToReadableMemOrNull,
            Arg::PtrToWritableMemOrNull,
            Arg::ConstSize,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 5: gpio_read(pin) -> i32
    HelperPrototype {
        name: "gpio_read",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 6: gpio_write(pin, value) -> i32
    HelperPrototype {
        name: "gpio_write",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::Anything,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 7: adc_read(channel) -> i32
    HelperPrototype {
        name: "adc_read",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 8: send(*ptr, len) -> i32
    HelperPrototype {
        name: "send",
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
    },
    // 9: send_recv(*ptr, len, *reply_buf, reply_len, timeout_ms) -> i32
    HelperPrototype {
        name: "send_recv",
        return_type: Ret::Integer,
        argument_type: [
            Arg::PtrToReadableMem,
            Arg::ConstSize,
            Arg::PtrToWritableMem,
            Arg::ConstSize,
            Arg::Anything,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 10: map_lookup_elem(*map, *key) -> *value or null
    HelperPrototype {
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
    },
    // 11: map_update_elem(*map, *key, *value) -> i32
    HelperPrototype {
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
    },
    // 12: get_time() -> u64
    HelperPrototype {
        name: "get_time",
        return_type: Ret::Integer,
        argument_type: [Arg::DontCare; 5],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 13: get_battery_mv() -> u16
    HelperPrototype {
        name: "get_battery_mv",
        return_type: Ret::Integer,
        argument_type: [Arg::DontCare; 5],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 14: delay_us(microseconds) -> i32
    HelperPrototype {
        name: "delay_us",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 15: set_next_wake(seconds) -> i32
    HelperPrototype {
        name: "set_next_wake",
        return_type: Ret::Integer,
        argument_type: [
            Arg::Anything,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
            Arg::DontCare,
        ],
        reallocate_packet: false,
        context_descriptor: None,
        unsupported: false,
    },
    // 16: bpf_trace_printk(*fmt, fmt_len) -> i32
    HelperPrototype {
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
    },
];

/// Sonde BPF verifier platform.
///
/// Wraps `LinuxPlatform` for ELF/map parsing and overrides helper prototypes
/// and program type resolution with sonde-specific definitions.
///
/// Also maintains a mirror of all map descriptors (including global variable
/// maps from .rodata/.data) so that `get_map_descriptor` can find them.
/// This works around a prevail-rust issue where global variable map
/// descriptors are added to the ELF loader's state but never propagated
/// to the platform's internal map store.
pub struct SondePlatform {
    inner: LinuxPlatform,
    /// Mirror of map descriptors populated via `sync_map_descriptors` after
    /// program/ELF parsing (including global variable maps from .rodata/.data).
    map_descriptors: Vec<EbpfMapDescriptor>,
}

impl SondePlatform {
    pub fn new() -> Self {
        Self {
            inner: LinuxPlatform::new(),
            map_descriptors: Vec::new(),
        }
    }

    /// Mirror the full set of map descriptors from the ELF loader into this
    /// platform, replacing any previously stored descriptors.
    ///
    /// This is needed because `prevail-rust` adds global variable map
    /// descriptors (`.rodata`, `.data`, `.bss`) to the ELF loader's internal
    /// state but does not propagate them through `parse_maps_section`.
    /// Call this after `ElfObject::get_programs` with the descriptors from
    /// `RawProgram.info.map_descriptors`.
    pub fn sync_map_descriptors(&mut self, descriptors: &[EbpfMapDescriptor]) {
        self.map_descriptors = descriptors.to_vec();
    }
}

impl Default for SondePlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl EbpfPlatform for SondePlatform {
    fn get_program_type(&self, _section: &str, _path: &str) -> EbpfProgramType {
        EbpfProgramType {
            name: "sonde".to_string(),
            context_descriptor: Some(&SONDE_CONTEXT),
            platform_specific_data: 0,
            section_prefixes: vec!["sonde".to_string(), ".text".to_string()],
            is_privileged: false,
        }
    }

    fn get_helper_prototype(&self, n: i32) -> &HelperPrototype {
        if (1..=16).contains(&n) {
            &SONDE_HELPERS[(n - 1) as usize]
        } else {
            &UNSUPPORTED_HELPER
        }
    }

    fn is_helper_usable(&self, n: i32) -> bool {
        (1..=16).contains(&n)
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
        // First check our mirror (includes global variable maps).
        if let Some(desc) = self
            .map_descriptors
            .iter()
            .find(|d| d.original_fd == map_fd)
        {
            return Some(desc);
        }
        // Fall back to the inner platform for maps parsed via parse_maps_section.
        self.inner.get_map_descriptor(map_fd)
    }

    fn get_map_type(&self, platform_specific_type: u32) -> EbpfMapType {
        match platform_specific_type {
            // Map type 0: global variable maps (.rodata, .data, .bss).
            // Prevail promotes ELF data sections to map descriptors with
            // map_type == 0. These must be array-typed so that LDDW
            // references produce shared-typed value pointers.
            0 => EbpfMapType {
                platform_specific_type: 0,
                name: "global".to_string(),
                is_array: true,
                value_type: EbpfMapValueType::Any,
            },
            // Sonde BPF_MAP_TYPE_ARRAY = 1 (differs from Linux's value of 2).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_16_helpers_are_usable() {
        let platform = SondePlatform::new();
        for id in 1..=16 {
            assert!(
                platform.is_helper_usable(id),
                "helper {id} should be usable"
            );
            let proto = platform.get_helper_prototype(id);
            assert!(!proto.unsupported, "helper {id} should not be unsupported");
            assert!(!proto.name.is_empty(), "helper {id} should have a name");
        }
    }

    #[test]
    fn helper_0_is_not_usable() {
        let platform = SondePlatform::new();
        assert!(!platform.is_helper_usable(0));
        assert!(platform.get_helper_prototype(0).unsupported);
    }

    #[test]
    fn helper_17_is_not_usable() {
        let platform = SondePlatform::new();
        assert!(!platform.is_helper_usable(17));
        assert!(platform.get_helper_prototype(17).unsupported);
    }

    #[test]
    fn map_lookup_returns_nullable_pointer() {
        let platform = SondePlatform::new();
        let proto = platform.get_helper_prototype(10);
        assert_eq!(proto.name, "map_lookup_elem");
        assert_eq!(proto.return_type, Ret::PtrToMapValueOrNull);
    }

    #[test]
    fn sonde_array_map_type_is_1() {
        let platform = SondePlatform::new();
        let mt = platform.get_map_type(1);
        assert!(mt.is_array);
        assert_eq!(mt.platform_specific_type, 1);
    }

    #[test]
    fn program_type_has_sonde_context() {
        let platform = SondePlatform::new();
        let pt = platform.get_program_type("", "");
        assert_eq!(pt.name, "sonde");
        assert!(
            pt.section_prefixes.contains(&"sonde".to_string()),
            "section_prefixes should include \"sonde\" for SEC(\"sonde\") programs"
        );
        let ctx = pt
            .context_descriptor
            .expect("should have context descriptor");
        assert_eq!(ctx.size, 16);
    }

    #[test]
    fn unknown_map_type_is_not_array() {
        let platform = SondePlatform::new();
        let mt = platform.get_map_type(99);
        assert!(!mt.is_array);
        assert_eq!(mt.platform_specific_type, 99);
    }

    /// GW-0404 criterion 5: map_type 0 (global variable maps) is array-typed.
    #[test]
    fn global_variable_map_type_0_is_array() {
        let platform = SondePlatform::new();
        let mt = platform.get_map_type(0);
        assert!(mt.is_array, "map_type 0 must be array-typed for LDDW");
        assert_eq!(mt.platform_specific_type, 0);
    }

    /// GW-0404 criterion 6: `sync_map_descriptors` makes descriptors visible
    /// via `get_map_descriptor`, and they take precedence over `inner`.
    #[test]
    fn sync_map_descriptors_returns_synced_descriptor() {
        let mut platform = SondePlatform::new();
        assert!(
            platform.get_map_descriptor(42).is_none(),
            "descriptor should not exist before sync"
        );

        let desc = EbpfMapDescriptor {
            original_fd: 42,
            map_type: 0,
            key_size: 4,
            value_size: 128,
            max_entries: 1,
            inner_map_fd: 0,
        };
        platform.sync_map_descriptors(std::slice::from_ref(&desc));

        let found = platform
            .get_map_descriptor(42)
            .expect("descriptor should exist after sync");
        assert_eq!(found.original_fd, 42);
        assert_eq!(found.value_size, 128);
        assert_eq!(found.map_type, 0);
    }
}
