// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Adapter wrapping the [`sonde_bpf`] interpreter as a [`BpfInterpreter`] backend.
//!
//! Uses the zero-allocation [`sonde_bpf::interpreter::execute_program_with_extra_mem`]
//! function, which keeps all interpreter state on the Rust call stack and requires
//! no heap allocation during execution.

use crate::bpf_helpers::SondeContext;
use crate::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_bpf::ebpf::Helper;

/// `sonde_bpf`-backed BPF interpreter.
///
/// Wraps [`sonde_bpf::interpreter::execute_program_with_extra_mem`] and adapts
/// it to the [`BpfInterpreter`] trait used by the wake cycle engine.
pub struct RbpfInterpreter {
    /// Registered helpers, keyed by BPF call number.
    helpers: Vec<(u32, Helper)>,
    /// Raw bytecode loaded via [`load`].
    bytecode: Option<Vec<u8>>,
    /// Extra memory regions the VM is allowed to access (map backing stores).
    /// Each entry is a `(*const u8, len)` pair.
    extra_regions: Vec<(*const u8, usize)>,
}

// SAFETY: `*const u8` raw pointers stored in `extra_regions` point to map
// data owned by `MapStorage`. `MapStorage` outlives the wake cycle, and
// `RbpfInterpreter` is always used on a single thread within that cycle.
// The pointers are only used for bounds-checking inside the interpreter
// and are never sent across thread boundaries in normal operation.
unsafe impl Send for RbpfInterpreter {}

impl RbpfInterpreter {
    /// Create a new interpreter with no program loaded.
    pub fn new() -> Self {
        Self {
            helpers: Vec::new(),
            bytecode: None,
            extra_regions: Vec::new(),
        }
    }
}

impl Default for RbpfInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl BpfInterpreter for RbpfInterpreter {
    fn register_helper(&mut self, id: u32, func: HelperFn) -> Result<(), BpfError> {
        // HelperFn and sonde_bpf::ebpf::Helper have identical signatures.
        self.helpers.push((id, func as Helper));
        Ok(())
    }

    fn load(&mut self, bytecode: &[u8], map_ptrs: &[u64]) -> Result<(), BpfError> {
        if bytecode.is_empty() {
            return Err(BpfError::InvalidBytecode("empty bytecode".into()));
        }
        if !bytecode.len().is_multiple_of(8) {
            return Err(BpfError::InvalidBytecode(
                "bytecode length must be a multiple of 8".into(),
            ));
        }

        self.bytecode = Some(bytecode.to_vec());

        // Register map backing memory as extra allowed regions so the VM
        // permits load/store into map values that BPF programs access via
        // pointers returned by the map_lookup_elem helper.
        self.extra_regions.clear();
        for &ptr in map_ptrs {
            if ptr != 0 {
                // Defence-in-depth: allow access around each map pointer.
                // The BpfInterpreter::load trait only receives pointers
                // (not sizes), so we use a fixed cap. Prevail has already
                // verified all accesses are in-bounds; this just prevents
                // the VM from wandering far off if a verifier bug exists.
                // Actual map sizes are typically < 4 KB (RTC SRAM budget).
                const MAX_MAP_REGION_SIZE: usize = 64 * 1024;
                self.extra_regions
                    .push((ptr as *const u8, MAX_MAP_REGION_SIZE));
            }
        }

        Ok(())
    }

    fn execute(&mut self, ctx_ptr: u64, _instruction_budget: u64) -> Result<u64, BpfError> {
        let bytecode = self
            .bytecode
            .as_ref()
            .ok_or_else(|| BpfError::LoadError("no program loaded".into()))?;

        // Copy the context into a temporary mutable buffer so that the
        // interpreter cannot corrupt the caller's real SondeContext. The BPF
        // spec defines the context as read-only (bpf-environment.md §4).
        let mut ctx_buf = [0u8; SondeContext::SIZE];
        if ctx_ptr != 0 {
            // SAFETY: ctx_ptr points to a SondeContext on the caller's
            // stack, which is alive for this call's duration.
            unsafe {
                let src = core::slice::from_raw_parts(ctx_ptr as *const u8, SondeContext::SIZE);
                ctx_buf.copy_from_slice(src);
            }
        }

        sonde_bpf::interpreter::execute_program_with_extra_mem(
            bytecode,
            &mut ctx_buf,
            &self.helpers,
            &self.extra_regions,
        )
        .map_err(|e| match e {
            sonde_bpf::interpreter::BpfError::CallDepthExceeded { .. } => {
                BpfError::CallDepthExceeded
            }
            sonde_bpf::interpreter::BpfError::UnknownHelper { id, .. } => {
                BpfError::HelperNotRegistered(id)
            }
            sonde_bpf::interpreter::BpfError::OutOfBounds { .. }
            | sonde_bpf::interpreter::BpfError::UnknownOpcode { .. }
            | sonde_bpf::interpreter::BpfError::MemoryAccessViolation { .. } => {
                BpfError::InvalidBytecode(e.to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpf_runtime::BpfInterpreter;

    /// Build a minimal BPF program: `mov r0, <imm>; exit`
    fn prog_return(value: u32) -> Vec<u8> {
        let mut bytecode = Vec::new();
        // BPF_MOV64_IMM(R0, value): opcode=0xb7, dst=0, src=0, off=0, imm=value
        bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&value.to_le_bytes());
        // BPF_EXIT: opcode=0x95
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode
    }

    /// Build a BPF program that calls helper `id` and exits with R0.
    /// `mov r1, <arg>; call <id>; exit`
    fn prog_call_helper(id: u32, arg: u32) -> Vec<u8> {
        let mut bytecode = Vec::new();
        // BPF_MOV64_IMM(R1, arg): pass arg in R1
        bytecode.extend_from_slice(&[0xb7, 0x01, 0x00, 0x00]);
        bytecode.extend_from_slice(&arg.to_le_bytes());
        // BPF_CALL(id): opcode=0x85, imm=id
        bytecode.extend_from_slice(&[0x85, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&id.to_le_bytes());
        // BPF_EXIT
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode
    }

    #[test]
    fn test_execute_return_42() {
        let mut interp = RbpfInterpreter::new();
        let prog = prog_return(42);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_register_arithmetic() {
        // mov r1, 1; mov r2, 2; mov r0, 0; add r0, r1; add r0, r2; exit
        // Expected: r0 = 0 + 1 + 2 = 3
        let mut bytecode = Vec::new();
        // BPF_MOV64_IMM(R1, 1)
        bytecode.extend_from_slice(&[0xb7, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        // BPF_MOV64_IMM(R2, 2)
        bytecode.extend_from_slice(&[0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
        // BPF_MOV64_IMM(R0, 0)
        bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // BPF_ADD64_REG(R0, R1): opcode=0x0f, dst=0, src=1
        bytecode.extend_from_slice(&[0x0f, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // BPF_ADD64_REG(R0, R2): opcode=0x0f, dst=0, src=2
        bytecode.extend_from_slice(&[0x0f, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // BPF_EXIT
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut interp = RbpfInterpreter::new();
        interp.load(&bytecode, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 3);
    }

    #[test]
    fn test_execute_return_0() {
        let mut interp = RbpfInterpreter::new();
        let prog = prog_return(0);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn test_helper_call() {
        fn my_helper(_r1: u64, _r2: u64, _r3: u64, _r4: u64, _r5: u64) -> u64 {
            99
        }

        let mut interp = RbpfInterpreter::new();
        interp.register_helper(1, my_helper).unwrap();
        let prog = prog_call_helper(1, 0u32);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn test_unregistered_helper_fails() {
        let mut interp = RbpfInterpreter::new();
        // Call helper 42 without registering it
        let prog = prog_call_helper(42, 0u32);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_bytecode_rejected() {
        let mut interp = RbpfInterpreter::new();
        let result = interp.load(&[], &[]);
        assert!(matches!(result, Err(BpfError::InvalidBytecode(_))));
    }

    #[test]
    fn test_misaligned_bytecode_rejected() {
        let mut interp = RbpfInterpreter::new();
        let result = interp.load(&[0x95, 0x00, 0x00], &[]);
        assert!(matches!(result, Err(BpfError::InvalidBytecode(_))));
    }

    #[test]
    fn test_no_program_loaded() {
        let mut interp = RbpfInterpreter::new();
        let result = interp.execute(0, 100_000);
        assert!(matches!(result, Err(BpfError::LoadError(_))));
    }

    #[test]
    fn test_with_context_ptr() {
        // Program that loads the first 4 bytes of context (low 32 bits of
        // timestamp at offset 0) into R0 and returns it.
        // BPF_LDXW r0, [r1+0]: opcode=0x61, dst=0, src=1, off=0, imm=0
        let mut prog = Vec::new();
        prog.extend_from_slice(&[0x61, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // BPF_EXIT
        prog.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut interp = RbpfInterpreter::new();
        interp.load(&prog, &[]).unwrap();

        let ctx = SondeContext {
            timestamp: 1710000000000,
            battery_mv: 3300,
            firmware_abi_version: 1,
            wake_reason: 0,
            _padding: [0; 3],
        };
        let ctx_ptr = &ctx as *const SondeContext as u64;
        let result = interp.execute(ctx_ptr, 100_000).unwrap();
        assert_eq!(result, ctx.timestamp as u32 as u64);
    }
}
