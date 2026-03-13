// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Adapter wrapping the [`sonde_bpf`] crate as a [`BpfInterpreter`] backend.
//!
//! This is the default interpreter for host-side testing and the reference
//! implementation for embedded targets. It uses the zero-allocation
//! `sonde_bpf::interpreter::execute_program_ex` function, which keeps all
//! interpreter state (registers, call stack, BPF stack) on the Rust stack.

use crate::bpf_helpers::SondeContext;
use crate::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};

/// sonde-bpf-backed BPF interpreter.
///
/// Wraps [`sonde_bpf::interpreter::execute_program_ex`] and adapts it to the
/// [`BpfInterpreter`] trait used by the wake cycle engine.
pub struct SondeBpfInterpreter {
    /// Registered helpers, keyed by BPF call number.
    helpers: Vec<(u32, HelperFn)>,
    /// Raw bytecode loaded via [`load`].
    bytecode: Option<Vec<u8>>,
    /// Memory regions (start address, length) the VM is allowed to access,
    /// in addition to the BPF stack and the context region. Populated with
    /// map backing store addresses from [`load`].
    extra_regions: Vec<(u64, usize)>,
}

impl SondeBpfInterpreter {
    /// Create a new interpreter with no program loaded.
    pub fn new() -> Self {
        Self {
            helpers: Vec::new(),
            bytecode: None,
            extra_regions: Vec::new(),
        }
    }
}

impl Default for SondeBpfInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl BpfInterpreter for SondeBpfInterpreter {
    fn register_helper(&mut self, id: u32, func: HelperFn) -> Result<(), BpfError> {
        self.helpers.push((id, func));
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

        // Register map backing memory as additional allowed regions so the VM
        // permits load/store into map values (e.g. after map_lookup_elem
        // returns a pointer that the BPF program then dereferences).
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
                self.extra_regions.push((ptr, MAX_MAP_REGION_SIZE));
            }
        }

        Ok(())
    }

    fn execute(&mut self, ctx_ptr: u64, instruction_budget: u64) -> Result<u64, BpfError> {
        let _ = instruction_budget;
        // sonde-bpf does not yet enforce an instruction counter. Termination
        // is guaranteed by the Prevail static verifier on the gateway, which
        // rejects programs with unbounded loops before they reach the node.
        // Programs that have not been Prevail-verified MUST NOT be loaded.

        let bytecode = self
            .bytecode
            .as_ref()
            .ok_or_else(|| BpfError::LoadError("no program loaded".into()))?;

        // Copy the context into a temporary mutable buffer so that the
        // interpreter's `mem` region (r1/r2) covers the context bytes.
        // The BPF spec defines the context as read-only (bpf-environment.md
        // §4); copying prevents the program from mutating the caller's
        // real SondeContext even if a verifier bug slips through.
        let mut ctx_buf = [0u8; SondeContext::SIZE];
        if ctx_ptr != 0 {
            // SAFETY: ctx_ptr points to a SondeContext on the caller's
            // stack, which is alive for this call's duration.
            unsafe {
                let src = core::slice::from_raw_parts(ctx_ptr as *const u8, SondeContext::SIZE);
                ctx_buf.copy_from_slice(src);
            }
        }

        // Build the helpers slice expected by sonde_bpf (same fn-pointer
        // type; just reinterpret the HelperFn slice).
        let helpers: &[(u32, sonde_bpf::ebpf::Helper)] =
            // SAFETY: HelperFn and sonde_bpf::ebpf::Helper are both
            // `fn(u64,u64,u64,u64,u64)->u64`; they are the same ABI.
            unsafe {
                core::slice::from_raw_parts(
                    self.helpers.as_ptr() as *const (u32, sonde_bpf::ebpf::Helper),
                    self.helpers.len(),
                )
            };

        sonde_bpf::interpreter::execute_program_ex(bytecode, &mut ctx_buf, helpers, &self.extra_regions)
            .map_err(|e| match e {
                sonde_bpf::interpreter::BpfError::CallDepthExceeded { .. } => {
                    BpfError::CallDepthExceeded
                }
                sonde_bpf::interpreter::BpfError::UnknownHelper { id, .. } => {
                    BpfError::HelperNotRegistered(id)
                }
                sonde_bpf::interpreter::BpfError::OutOfBounds { pc } => {
                    BpfError::InvalidBytecode(format!("PC out of bounds at insn #{pc}"))
                }
                sonde_bpf::interpreter::BpfError::UnknownOpcode { pc, opc } => {
                    BpfError::InvalidBytecode(format!("unknown opcode {opc:#04x} at insn #{pc}"))
                }
                sonde_bpf::interpreter::BpfError::MemoryAccessViolation { pc, addr, len } => {
                    BpfError::InvalidBytecode(format!(
                        "memory access violation at insn #{pc}: addr={addr:#x} len={len}"
                    ))
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
        let mut interp = SondeBpfInterpreter::new();
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

        let mut interp = SondeBpfInterpreter::new();
        interp.load(&bytecode, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 3);
    }

    #[test]
    fn test_execute_return_0() {
        let mut interp = SondeBpfInterpreter::new();
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

        let mut interp = SondeBpfInterpreter::new();
        interp.register_helper(1, my_helper).unwrap();
        let prog = prog_call_helper(1, 0u32);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn test_unregistered_helper_fails() {
        let mut interp = SondeBpfInterpreter::new();
        // Call helper 42 without registering it
        let prog = prog_call_helper(42, 0u32);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_bytecode_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let result = interp.load(&[], &[]);
        assert!(matches!(result, Err(BpfError::InvalidBytecode(_))));
    }

    #[test]
    fn test_misaligned_bytecode_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let result = interp.load(&[0x95, 0x00, 0x00], &[]);
        assert!(matches!(result, Err(BpfError::InvalidBytecode(_))));
    }

    #[test]
    fn test_no_program_loaded() {
        let mut interp = SondeBpfInterpreter::new();
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

        let mut interp = SondeBpfInterpreter::new();
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
