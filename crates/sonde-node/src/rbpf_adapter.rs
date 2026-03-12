// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Adapter wrapping the [`rbpf`] crate as a [`BpfInterpreter`] backend.
//!
//! This is the default interpreter for host-side testing and the
//! reference implementation for embedded targets. BPF-to-BPF calls
//! are not yet supported by upstream `rbpf`; programs requiring them
//! will fail verification on the gateway (Prevail) and never reach
//! the node.

use crate::bpf_helpers::SondeContext;
use crate::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};

/// rbpf-backed BPF interpreter.
///
/// Wraps [`rbpf::EbpfVmRaw`] and adapts it to the [`BpfInterpreter`]
/// trait used by the wake cycle engine.
pub struct RbpfInterpreter {
    /// Registered helpers, keyed by BPF call number.
    helpers: std::collections::HashMap<u32, HelperFn>,
    /// Raw bytecode loaded via [`load`].
    bytecode: Option<Vec<u8>>,
    /// Memory ranges the VM is allowed to access (map backing stores).
    allowed_ranges: Vec<std::ops::Range<u64>>,
}

impl RbpfInterpreter {
    /// Create a new interpreter with no program loaded.
    pub fn new() -> Self {
        Self {
            helpers: std::collections::HashMap::new(),
            bytecode: None,
            allowed_ranges: Vec::new(),
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
        self.helpers.insert(id, func);
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

        // Register map backing memory as allowed ranges so the VM
        // permits load/store into map values.
        self.allowed_ranges.clear();
        for &ptr in map_ptrs {
            if ptr != 0 {
                // Allow a generous range from the pointer. The exact
                // size isn't known here, but Prevail has already
                // verified all accesses are in-bounds.
                self.allowed_ranges
                    .push(ptr..ptr.saturating_add(1024 * 1024));
            }
        }

        Ok(())
    }

    fn execute(&mut self, ctx_ptr: u64, _instruction_budget: u64) -> Result<u64, BpfError> {
        // NOTE: rbpf does not expose an instruction-counting / step-limit
        // mechanism. We rely on Prevail verification on the gateway to
        // guarantee termination (bounded loops, no infinite recursion).
        // A future upstream rbpf patch could add metering, at which point
        // _instruction_budget should be wired in here.

        let bytecode = self
            .bytecode
            .as_ref()
            .ok_or_else(|| BpfError::LoadError("no program loaded".into()))?;

        let mut vm = rbpf::EbpfVmRaw::new(Some(bytecode))
            .map_err(|e| BpfError::LoadError(format!("{}", e)))?;

        // Disable the default verifier — Prevail on the gateway has
        // already verified the program. rbpf's built-in verifier is
        // too restrictive (rejects valid programs with map accesses).
        vm.set_verifier(|_| Ok(()))
            .map_err(|e| BpfError::LoadError(format!("{}", e)))?;

        // Register helpers.
        for (&id, &func) in &self.helpers {
            vm.register_helper(id, func)
                .map_err(|e| BpfError::LoadError(format!("helper {}: {}", id, e)))?;
        }

        // Allow access to the SondeContext memory region.
        if ctx_ptr != 0 {
            let ctx_end = ctx_ptr
                .checked_add(SondeContext::SIZE as u64)
                .ok_or_else(|| BpfError::LoadError("context pointer overflow".into()))?;
            vm.register_allowed_memory(ctx_ptr..ctx_end);
        }

        // Allow access to map memory regions.
        for range in &self.allowed_ranges {
            vm.register_allowed_memory(range.clone());
        }

        // Copy the context into a temporary mutable buffer so that
        // rbpf's execute_program (which requires &mut [u8]) cannot
        // corrupt the caller's real SondeContext. The BPF spec defines
        // the context as read-only (bpf-environment.md §4).
        let mut ctx_buf = [0u8; SondeContext::SIZE];
        if ctx_ptr != 0 {
            // SAFETY: ctx_ptr points to a SondeContext on the caller's
            // stack, which is alive for this call's duration.
            unsafe {
                let src = core::slice::from_raw_parts(ctx_ptr as *const u8, SondeContext::SIZE);
                ctx_buf.copy_from_slice(src);
            }
        }
        let ctx_slice: &mut [u8] = if ctx_ptr != 0 { &mut ctx_buf } else { &mut [] };

        vm.execute_program(ctx_slice).map_err(|e| {
            // rbpf uses a flat Error type with a string message (no
            // structured variants). Match on known substrings to map
            // to the appropriate BpfError variant.
            let msg = e.to_string();
            if msg.contains("call depth") || msg.contains("stack overflow") {
                BpfError::CallDepthExceeded
            } else {
                BpfError::InvalidBytecode(msg)
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
    fn prog_call_helper(id: u32, arg: u64) -> Vec<u8> {
        let mut bytecode = Vec::new();
        // BPF_MOV64_IMM(R1, arg as u32): pass arg in R1
        bytecode.extend_from_slice(&[0xb7, 0x01, 0x00, 0x00]);
        bytecode.extend_from_slice(&(arg as u32).to_le_bytes());
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
        let prog = prog_call_helper(1, 0);
        interp.load(&prog, &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn test_unregistered_helper_fails() {
        let mut interp = RbpfInterpreter::new();
        // Call helper 42 without registering it
        let prog = prog_call_helper(42, 0);
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
        // Program that ignores R1 (ctx pointer) and simply returns 0; this
        // test ensures that passing a valid non-zero context pointer to
        // `execute` succeeds and yields the expected return value.
        let prog = prog_return(0);
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
        assert_eq!(result, 0);
    }
}
