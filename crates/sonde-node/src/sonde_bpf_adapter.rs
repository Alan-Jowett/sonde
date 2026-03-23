// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Adapter wrapping [`sonde_bpf`] as a [`BpfInterpreter`] backend.

use crate::bpf_helpers::SondeContext;
use crate::bpf_runtime::{BpfError, BpfInterpreter, HelperFn};
use sonde_bpf::interpreter::{HelperDescriptor, HelperReturn, MapRegion};

/// sonde-bpf-backed BPF interpreter.
pub struct SondeBpfInterpreter {
    helpers: Vec<HelperDescriptor>,
    bytecode: Option<Vec<u8>>,
    map_regions: Vec<MapRegion>,
}

impl SondeBpfInterpreter {
    pub fn new() -> Self {
        Self {
            helpers: Vec::new(),
            bytecode: None,
            map_regions: Vec::new(),
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
        // Determine return type: map_lookup_elem returns a pointer into map values.
        let ret = if id == crate::bpf_helpers::helper_ids::MAP_LOOKUP_ELEM {
            HelperReturn::MapValueOrNull { map_arg: 1 }
        } else {
            HelperReturn::Scalar
        };

        // Check if already registered (update in place).
        if let Some(desc) = self.helpers.iter_mut().find(|d| d.id == id) {
            desc.func = func;
            desc.ret = ret;
            return Ok(());
        }

        self.helpers.push(HelperDescriptor { id, func, ret });
        Ok(())
    }

    fn load(
        &mut self,
        bytecode: &[u8],
        map_ptrs: &[u64],
        map_defs: &[sonde_protocol::MapDef],
    ) -> Result<(), BpfError> {
        if bytecode.is_empty() {
            return Err(BpfError::InvalidBytecode("empty bytecode"));
        }
        if !bytecode.len().is_multiple_of(8) {
            return Err(BpfError::InvalidBytecode(
                "bytecode length must be a multiple of 8",
            ));
        }

        if map_ptrs.len() != map_defs.len() {
            return Err(BpfError::LoadError("map_ptrs and map_defs length mismatch"));
        }

        // Build MapRegion descriptors from map_ptrs + map_defs.
        // Use a temporary vec so self.map_regions is only updated on success.
        let mut new_regions = Vec::with_capacity(map_ptrs.len());
        for (i, (&ptr, def)) in map_ptrs.iter().zip(map_defs.iter()).enumerate() {
            if ptr == 0 {
                return Err(BpfError::MapLoadError {
                    index: i,
                    kind: "null pointer",
                });
            }
            let entry_size = (def.key_size as u64)
                .checked_add(def.value_size as u64)
                .ok_or(BpfError::MapLoadError {
                    index: i,
                    kind: "entry size overflow (key_size + value_size)",
                })?;
            let total_bytes =
                entry_size
                    .checked_mul(def.max_entries as u64)
                    .ok_or(BpfError::MapLoadError {
                        index: i,
                        kind: "total size overflow (entry_size * max_entries)",
                    })?;
            new_regions.push(MapRegion {
                relocated_ptr: ptr,
                value_size: def.value_size,
                data_start: ptr,
                data_end: ptr.checked_add(total_bytes).ok_or(BpfError::MapLoadError {
                    index: i,
                    kind: "pointer + size overflow",
                })?,
            });
        }

        // Commit only after all validation passes.
        self.bytecode = Some(bytecode.to_vec());
        self.map_regions = new_regions;

        Ok(())
    }

    /// Execute the loaded program with instruction budget enforcement.
    fn execute(&mut self, ctx_ptr: u64, instruction_budget: u64) -> Result<u64, BpfError> {
        let bytecode = self
            .bytecode
            .as_ref()
            .ok_or(BpfError::LoadError("no program loaded"))?;

        // Copy context into a local buffer (read-only for the interpreter).
        let mut ctx_buf = [0u8; SondeContext::SIZE];
        if ctx_ptr != 0 {
            // SAFETY: The caller (run_wake_cycle) passes a pointer to a
            // stack-allocated SondeContext that is alive for this call.
            // SondeContext is repr(C) and 8-byte aligned. The pointer is
            // obtained via `&ctx as *const SondeContext as u64` — alignment
            // and validity are guaranteed by the Rust reference.
            unsafe {
                let src = core::slice::from_raw_parts(ctx_ptr as *const u8, SondeContext::SIZE);
                ctx_buf.copy_from_slice(src);
            }
        }

        // SAFETY: The caller (run_wake_cycle) must ensure:
        // 1. MapStorage is not dropped or reallocated between load() and
        //    execute() — guaranteed by the borrow structure of run_wake_cycle.
        // 2. data_start..data_end ranges (from map_ptrs + map_defs) cover
        //    actual live MapStorage allocations for this call's duration.
        // 3. No concurrent mutation of map backing memory outside of BPF
        //    helper calls (single-threaded BPF execution).
        let result = unsafe {
            sonde_bpf::interpreter::execute_program(
                bytecode,
                &mut ctx_buf,
                &self.helpers,
                &self.map_regions,
                true, // read_only_ctx
                instruction_budget,
            )
        };

        result.map_err(|e| {
            use sonde_bpf::interpreter::BpfError as SbErr;
            match e {
                SbErr::InstructionBudgetExceeded { .. } => BpfError::InstructionBudgetExceeded,
                SbErr::CallDepthExceeded { .. } => BpfError::CallDepthExceeded,
                SbErr::UnknownHelper { id, .. } => BpfError::HelperNotRegistered(id),
                SbErr::OutOfBounds { .. } | SbErr::UnknownOpcode { .. } => {
                    BpfError::InvalidBytecode("invalid or out-of-bounds bytecode")
                }
                SbErr::InvalidMapIndex { .. } | SbErr::InvalidHelperArgument { .. } => {
                    BpfError::LoadError("invalid map index or helper argument")
                }
                _ => BpfError::RuntimeError("BPF runtime error"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpf_runtime::BpfInterpreter;

    fn prog_return(value: u32) -> Vec<u8> {
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&value.to_le_bytes());
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode
    }

    fn prog_call_helper(id: u32, arg: u32) -> Vec<u8> {
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0xb7, 0x01, 0x00, 0x00]);
        bytecode.extend_from_slice(&arg.to_le_bytes());
        bytecode.extend_from_slice(&[0x85, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&id.to_le_bytes());
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode
    }

    #[test]
    fn test_execute_return_42() {
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(42);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_register_arithmetic() {
        let mut bytecode = Vec::new();
        bytecode.extend_from_slice(&[0xb7, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&[0xb7, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&[0x0f, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&[0x0f, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut interp = SondeBpfInterpreter::new();
        interp.load(&bytecode, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 3);
    }

    #[test]
    fn test_execute_return_0() {
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(0);
        interp.load(&prog, &[], &[]).unwrap();
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
        let prog = prog_call_helper(1, 0);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn test_unregistered_helper_fails() {
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_call_helper(42, 0);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_bytecode_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let result = interp.load(&[], &[], &[]);
        assert!(matches!(result, Err(BpfError::InvalidBytecode(_))));
    }

    #[test]
    fn test_misaligned_bytecode_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let result = interp.load(&[0x95, 0x00, 0x00], &[], &[]);
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
        let mut prog = Vec::new();
        prog.extend_from_slice(&[0x61, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        prog.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let mut interp = SondeBpfInterpreter::new();
        interp.load(&prog, &[], &[]).unwrap();
        let ctx = crate::bpf_helpers::SondeContext {
            timestamp: 1710000000000,
            battery_mv: 3300,
            firmware_abi_version: 1,
            wake_reason: 0,
            _padding: [0; 3],
        };
        let ctx_ptr = &ctx as *const _ as u64;
        let result = interp.execute(ctx_ptr, 100_000).unwrap();
        assert_eq!(result, ctx.timestamp as u32 as u64);
    }

    #[test]
    fn test_mismatched_map_counts_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(0);
        let def = sonde_protocol::MapDef {
            map_type: 1,
            key_size: 4,
            value_size: 8,
            max_entries: 1,
        };
        // 2 pointers but only 1 def
        let result = interp.load(&prog, &[0x1000, 0x2000], &[def]);
        assert!(matches!(result, Err(BpfError::LoadError(_))));
    }

    #[test]
    fn test_map_size_overflow_rejected() {
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(0);
        let def = sonde_protocol::MapDef {
            map_type: 1,
            key_size: u32::MAX,
            value_size: u32::MAX,
            max_entries: u32::MAX,
        };
        let result = interp.load(&prog, &[0x1000], &[def]);
        assert!(matches!(result, Err(BpfError::MapLoadError { .. })));
    }

    #[test]
    fn test_helper_reregistration_updates() {
        fn helper_a(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
            1
        }
        fn helper_b(_: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
            2
        }
        let mut interp = SondeBpfInterpreter::new();
        interp.register_helper(1, helper_a).unwrap();
        interp.register_helper(1, helper_b).unwrap();
        // Should still have only one helper entry
        let prog = prog_call_helper(1, 0);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000).unwrap();
        assert_eq!(result, 2); // helper_b's return value
    }

    #[test]
    fn test_instruction_budget_exceeded() {
        // prog_return(42) has 2 instructions; a budget of 1 allows only the
        // first instruction before the budget is exhausted on the second.
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(42);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 1);
        assert!(matches!(result, Err(BpfError::InstructionBudgetExceeded)));
    }

    #[test]
    fn test_instruction_budget_exact() {
        // prog_return(42) has 2 instructions; a budget of exactly 2 should
        // succeed.
        let mut interp = SondeBpfInterpreter::new();
        let prog = prog_return(42);
        interp.load(&prog, &[], &[]).unwrap();
        let result = interp.execute(0, 2).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_stack_frame_overflow() {
        // ND-0605 AC3: A BPF program that stores beyond the total stack
        // boundary must trigger a MemoryAccessViolation → RuntimeError.
        //
        // The interpreter allocates STACK_SIZE_PER_FRAME * MAX_CALL_DEPTH
        // bytes. r10 starts at the stack top. Accessing 8 bytes past the
        // bottom of the stack must fail.
        //
        // Bytecode:
        //   mov64 r0, 0x42
        //   stxdw [r10 - (STACK_SIZE + 8)], r0   ← past stack bottom
        //   exit
        use sonde_bpf::ebpf::{MAX_CALL_DEPTH, STACK_SIZE_PER_FRAME};

        let total_stack = STACK_SIZE_PER_FRAME * MAX_CALL_DEPTH;
        let overflow_offset = -(total_stack as i16 + 8);

        let mut bytecode = Vec::new();
        // mov64 r0, 0x42
        bytecode.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0x42, 0x00, 0x00, 0x00]);
        // stxdw [r10 + overflow_offset], r0
        let offset_bytes = overflow_offset.to_le_bytes();
        bytecode.extend_from_slice(&[0x7b, 0x0a, offset_bytes[0], offset_bytes[1]]);
        bytecode.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        // exit
        bytecode.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        let mut interp = SondeBpfInterpreter::new();
        interp.load(&bytecode, &[], &[]).unwrap();
        let result = interp.execute(0, 100_000);
        assert!(
            result.is_err(),
            "stack access beyond frame boundary must fail"
        );
        assert!(
            matches!(result, Err(BpfError::RuntimeError(_))),
            "expected RuntimeError for stack overflow, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // T-N929: Write to read-only sonde_context rejected (ND-0505 AC5/AC6)
    // -----------------------------------------------------------------------

    /// T-N929: A BPF program that attempts to write to the `sonde_context`
    /// memory region MUST be rejected, and the original context MUST remain
    /// unchanged.
    #[test]
    fn t_n929_write_to_read_only_context_rejected() {
        let mut interp = SondeBpfInterpreter::new();

        // BPF program:
        //   r2 = 0xDEAD            (mov64 imm)
        //   *(u32*)(r1 + 0) = r2   (STX W — write to context)
        //   r0 = 0                 (mov64 imm)
        //   exit
        let mut prog = Vec::new();
        // r2 = 0xDEAD
        prog.extend_from_slice(&[0xb7, 0x02, 0x00, 0x00, 0xAD, 0xDE, 0x00, 0x00]);
        // *(u32*)(r1 + 0) = r2  — attempt to write to context
        prog.extend_from_slice(&[0x63, 0x21, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // r0 = 0
        prog.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        // exit
        prog.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        // A verifier improvement may legitimately reject this program at
        // load time. Treat that as an acceptable outcome, but require it to
        // fail with an appropriate "invalid/unsafe program" error.
        let load_result = interp.load(&prog, &[], &[]);
        match load_result {
            Ok(()) => {
                // Program loaded — verify it's rejected at execution time.
            }
            Err(err) => {
                assert!(
                    matches!(
                        err,
                        BpfError::InvalidBytecode(_) | BpfError::LoadError(_)
                    ),
                    "context-write program must be rejected at load() as invalid/unsafe, got: {err:?}"
                );
                // Load-time rejection is acceptable; the context is trivially unchanged.
                return;
            }
        }

        let ctx = SondeContext {
            timestamp: 1710000000000,
            battery_mv: 3300,
            firmware_abi_version: 1,
            wake_reason: 0,
            _padding: [0; 3],
        };
        let ctx_ptr = &ctx as *const _ as u64;

        // Execute — the interpreter rejects the write to the read-only
        // context region.
        let result = interp.execute(ctx_ptr, 100_000);
        assert!(
            matches!(result, Err(BpfError::RuntimeError(_))),
            "write to read-only context must produce a RuntimeError, got: {result:?}"
        );

        // The original SondeContext on the caller's stack is unchanged
        // because SondeBpfInterpreter copies the context into a local
        // buffer before execution (defense-in-depth).
        assert_eq!(ctx.timestamp, 1710000000000);
        assert_eq!(ctx.battery_mv, 3300);
        assert_eq!(ctx.firmware_abi_version, 1);
        assert_eq!(ctx.wake_reason, 0);
    }

    // -----------------------------------------------------------------------
    // T-N934: Stack overflow terminates BPF program (ND-0605 AC4)
    // -----------------------------------------------------------------------

    /// T-N934: A BPF program that writes beyond the total stack boundary
    /// MUST be terminated with a stack violation error.
    #[test]
    fn t_n934_stack_overflow_terminates_program() {
        let mut interp = SondeBpfInterpreter::new();

        // BPF program:
        //   r0 = 0xDEAD                  (mov64 imm)
        //   *(u32*)(r10 - 4100) = r0     (STX W — beyond 4 KB total stack)
        //   exit
        //
        // R10 starts at stack_base + 4096. Writing at offset -4100 accesses
        // stack_base - 4, which is below the stack region boundary.
        let mut prog = Vec::new();
        // r0 = 0xDEAD
        prog.extend_from_slice(&[0xb7, 0x00, 0x00, 0x00, 0xAD, 0xDE, 0x00, 0x00]);
        // *(u32*)(r10 - 4100) = r0  — offset -4100 = 0xEFFC as i16 LE
        prog.extend_from_slice(&[0x63, 0x0A, 0xFC, 0xEF, 0x00, 0x00, 0x00, 0x00]);
        // exit
        prog.extend_from_slice(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        // A verifier improvement may legitimately reject this program at
        // load time. Treat that as an acceptable outcome, but in that case
        // we still require it to fail with an appropriate "invalid/unsafe
        // program" error.
        let load_result = interp.load(&prog, &[], &[]);
        match load_result {
            Ok(()) => {
                // After successful load, stack boundary violations are
                // runtime memory errors — not bytecode decoding errors.
                let result = interp.execute(0, 100_000);
                assert!(
                    matches!(result, Err(BpfError::RuntimeError(_))),
                    "stack overflow must terminate the program with a RuntimeError, got: {result:?}"
                );
            }
            Err(err) => {
                assert!(
                    matches!(
                        err,
                        BpfError::InvalidBytecode(_) | BpfError::LoadError(_)
                    ),
                    "stack overflow program must be rejected at load() as invalid/unsafe, got: {err:?}"
                );
            }
        }
    }
}
