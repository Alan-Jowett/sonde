<!-- SPDX-License-Identifier: MIT
  Copyright (c) 2026 sonde contributors -->
# Safe BPF Interpreter Validation Specification (`sonde-bpf`)

> **Document status:** Draft  
> **Scope:** Test plan for the tagged-register safety model in the `sonde-bpf` interpreter crate.  
> **Audience:** Implementers (human or LLM agent) writing `sonde-bpf` crate tests.  
> **Related:** [safe-bpf-interpreter.md](safe-bpf-interpreter.md), [bpf-environment.md](bpf-environment.md), [node-requirements.md](node-requirements.md)

---

## 1  Overview

All tests in this document are pure Rust `#[test]` cases unless noted otherwise.  The interpreter is fully testable in isolation — construct bytecode in-memory, allocate context/stack/map buffers, and call `execute_program(...)`.  There are **33 test cases** total, organized into nine categories that cover the tagged-register safety model end-to-end.

**Notation:** Each test references the relevant section of [safe-bpf-interpreter.md](safe-bpf-interpreter.md) (abbreviated **§N.M**) or [bpf-environment.md](bpf-environment.md).

### Test infrastructure

Tests use the same `execute_program` entry point described in safe-bpf-interpreter.md §10.1.  Helper functions are registered via `HelperDescriptor` structs with explicit `HelperReturn` types.  Map memory is allocated on the host heap and passed as `MapRegion` descriptors.

**Safety:** `execute_program(...)` is `unsafe` when maps are involved (caller must guarantee `MapRegion` pointers are valid).  For tests that do not use maps, prefer the safe wrapper `execute_program_no_maps(...)`.  Tests requiring maps must use `unsafe { execute_program(...) }` with host-heap-allocated map buffers.

Use clearly non-zero test keys (e.g., `[0x42u8; 32]`) and non-trivial buffer contents to avoid masking bugs with zero-initialized memory.

---

## 2  Pointer dereference tests

### T-BPF-001  Load via scalar register → `NonDereferenceableAccess`

**Validates:** safe-bpf-interpreter.md §3.1

**Procedure:**
1. Construct bytecode that does `LDX_DW r0, [r3 + 0]` where R3 is scalar (default zero-initialized, no pointer tag).
2. Execute with `execute_program(...)`.
3. Assert: result is `Err(BpfError::NonDereferenceableAccess { .. })`.

---

### T-BPF-002  Store via MapDescriptor register → `NonDereferenceableAccess`

**Validates:** safe-bpf-interpreter.md §3.2

**Procedure:**
1. Construct bytecode that does `LD_DW_IMM r1, src=1, imm=0` (loads MapDescriptor for map 0) followed by `STX_DW [r1 + 0], r0`.
2. Provide one valid map definition.
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::NonDereferenceableAccess { .. })`.

---

### T-BPF-003  Load with `addr + offset` wrapping past u64::MAX → `MemoryAccessViolation`

**Validates:** safe-bpf-interpreter.md §3.1

**Procedure:**
1. In the test harness, obtain the context base address `ctx_base` and compute a 64-bit delta such that `ctx_base + delta + 128` overflows `u64::MAX`. Encode this delta into the program using `LD_DW_IMM r2, src=0` (64-bit immediate load), then construct bytecode: `ADD r1, r2` followed by `LDX_B r0, [r1 + 127]`.
2. Execute with `execute_program_no_maps(...)`.
3. Assert: result is `Err(BpfError::MemoryAccessViolation { .. })` — the `checked_add` in `mem_load` detects the overflow.

---

### T-BPF-004  Atomic op on Context (read-only) region → silently ignored

**Validates:** safe-bpf-interpreter.md §3.3, ND-0505 AC6

**Procedure:**
1. Seed the context buffer with a known non-zero pattern (e.g., `[0xAA; 8]` in the first 8 bytes).
2. Construct bytecode that loads a non-zero value into R0 (`MOV64_IMM r0, 0x42`), then executes an atomic ADD on R1 (Context pointer) at offset 0: `ATOMIC_DW r1, r0, ADD`, followed by `EXIT`.
3. Execute with `execute_program_no_maps(...)` with `read_only_ctx = true`.
4. Assert: result is `Ok(0)` — the write is silently ignored per ND-0505 AC6, the context buffer retains its original pattern, and the program continues to completion.

---

### T-BPF-005  Atomic op via scalar register → `NonDereferenceableAccess`

**Validates:** safe-bpf-interpreter.md §3.3

**Procedure:**
1. Construct bytecode that executes an atomic ADD on R3 (scalar, never assigned a pointer): `ATOMIC_DW r3, r0, ADD`.
2. Execute with `execute_program(...)`.
3. Assert: result is `Err(BpfError::NonDereferenceableAccess { .. })`.

---

## 3  Pointer arithmetic tests

### T-BPF-006  pointer + pointer → `InvalidPointerArithmetic`

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (copies Context pointer to R2), then `ADD r1, r2`.
2. Execute with `execute_program(...)`.
3. Assert: result is `Err(BpfError::InvalidPointerArithmetic { .. })`.

---

### T-BPF-007  scalar − pointer → `InvalidPointerArithmetic`

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r3, 42` (scalar), `MOV r4, r1` (copies Context pointer), then `SUB r3, r4`.
2. Execute with `execute_program(...)`.
3. Assert: result is `Err(BpfError::InvalidPointerArithmetic { .. })`.

---

### T-BPF-008  AND/OR/XOR on pointer → `InvalidPointerArithmetic`

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. For each operation (AND, OR, XOR): construct bytecode that applies the bitwise op with R1 (Context pointer) as `dst` and a scalar immediate or register as `src`.
2. Execute with `execute_program(...)`.
3. Assert: each returns `Err(BpfError::InvalidPointerArithmetic { .. })`.

---

### T-BPF-009  MapDescriptor in non-MOV arithmetic → `InvalidPointerArithmetic`

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `LD_DW_IMM r2, src=1, imm=0` (R2 = MapDescriptor), then `ADD r2, 1`.
2. Provide one valid map definition.
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::InvalidPointerArithmetic { .. })`.

---

### T-BPF-010  MUL/DIV on pointer → result is scalar (tag cleared)

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (R2 = Context pointer), `MUL r2, 1`, then `LDX_DW r0, [r2 + 0]` (attempt to dereference).
2. Execute with `execute_program(...)`.
3. Assert: the MUL succeeds (no error at that instruction), but the subsequent load fails with `Err(BpfError::NonDereferenceableAccess { .. })` because R2 was cleared to scalar by MUL.

---

### T-BPF-011  NEG on pointer → result is scalar

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (R2 = Context pointer), `NEG r2`, then `LDX_DW r0, [r2 + 0]`.
2. Execute with `execute_program(...)`.
3. Assert: the NEG succeeds, but the subsequent load fails with `Err(BpfError::NonDereferenceableAccess { .. })`.

---

### T-BPF-012  ALU32 with pointer input → always scalar (truncation)

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (R2 = Context pointer), `ADD32 r2, 0` (32-bit ADD), then `LDX_DW r0, [r2 + 0]`.
2. Execute with `execute_program(...)`.
3. Assert: the ADD32 succeeds, but the subsequent load fails with `Err(BpfError::NonDereferenceableAccess { .. })` — 32-bit ALU always clears the pointer tag.

---

### T-BPF-013  pointer − pointer (same region) → scalar result

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (R2 = Context pointer copy), `ADD r2, 4` (advance within context), `SUB r2, r1`.
2. Execute with `execute_program(...)`.
3. Assert: R2 now holds the scalar value 4. Verify by storing R2 to the stack and confirming the value equals 4. The subtraction does not produce an error.

---

### T-BPF-014  pointer − pointer (different regions) → `InvalidPointerArithmetic`

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r10` (R2 = Stack pointer), `SUB r2, r1` (subtract Context pointer from Stack pointer).
2. Execute with `execute_program(...)`.
3. Assert: result is `Err(BpfError::InvalidPointerArithmetic { .. })`.

---

## 4  Tag propagation tests

### T-BPF-015  MOV reg-to-reg inherits source pointer tag

**Validates:** safe-bpf-interpreter.md §4.3

**Procedure:**
1. Construct bytecode: `MOV r2, r1` (R1 = Context pointer), then `LDX_B r0, [r2 + 0]`.
2. Execute with `execute_program(...)`.
3. Assert: the load succeeds — R2 inherited the Context pointer tag from R1, making it dereferenceable.

---

### T-BPF-016  Helper call clobbers R1–R5 tags to scalar

**Validates:** safe-bpf-interpreter.md §4.6

**Procedure:**
1. Construct bytecode: `MOV r3, r1` (save Context pointer in R3), then `CALL helper_id` (any Scalar-returning helper), then `LDX_B r0, [r3 + 0]`.
2. Register one helper that returns `HelperReturn::Scalar`.
3. Execute with `execute_program(...)`.
4. Assert: the load via R3 fails with `Err(BpfError::NonDereferenceableAccess { .. })` because R3 was clobbered to scalar by the CALL.

---

### T-BPF-017  Initial register state: R1=Context, R10=Stack, R0/R2–R9=scalar

**Validates:** safe-bpf-interpreter.md §4.1

**Procedure:**
1. Construct bytecode that:
   - Loads one byte from R1 at offset 0 (`LDX_B r0, [r1 + 0]`) — should succeed (R1 = Context pointer).
   - Stores one byte to the stack via R10 at offset −1 (`STX_B [r10 − 1], r0`) — should succeed (R10 = Stack pointer).
   - Loads from R3 at offset 0 (`LDX_B r0, [r3 + 0]`) — should fail (R3 = scalar).
2. Execute with `execute_program(...)`.
3. Assert: the program fails at the R3 load with `Err(BpfError::NonDereferenceableAccess { .. })`, confirming R1 and R10 are correctly tagged and R3 is scalar.

---

## 5  Spill tracking tests

### T-BPF-018  STX_DW pointer to stack sets spill bitmap bit

**Validates:** safe-bpf-interpreter.md §6.3

**Procedure:**
1. Construct bytecode: `STX_DW [r10 − 8], r1` (spill Context pointer to stack), then `LDX_DW r2, [r10 − 8]` (reload), then `LDX_B r0, [r2 + 0]` (dereference reloaded pointer).
2. Execute with `execute_program(...)`.
3. Assert: the program succeeds — the spill tracker recorded the pointer tag for the stack slot, and LDX_DW restored it to R2.

---

### T-BPF-019  LDX_DW from spill slot restores pointer tag

**Validates:** safe-bpf-interpreter.md §6.3

**Procedure:**
1. Construct bytecode: `STX_DW [r10 − 8], r1` (spill Context pointer), `MOV r1, 0` (clobber R1 to scalar), `LDX_DW r1, [r10 − 8]` (reload from spill), `LDX_B r0, [r1 + 0]` (dereference R1).
2. Execute with `execute_program(...)`.
3. Assert: the program succeeds — R1 regains its Context pointer tag from the spill slot after being clobbered and reloaded.

---

### T-BPF-020  Partial overwrite (STX_B/H/W) clears spill bitmap bit

**Validates:** safe-bpf-interpreter.md §6.3

**Procedure:**
1. Construct bytecode: `STX_DW [r10 − 8], r1` (spill Context pointer), then `STX_B [r10 − 8], r0` (partial overwrite — 1 byte), then `LDX_DW r2, [r10 − 8]` (reload), then `LDX_B r0, [r2 + 0]` (attempt to dereference).
2. Execute with `execute_program(...)`.
3. Assert: the dereference fails with `Err(BpfError::NonDereferenceableAccess { .. })` because the partial overwrite cleared the spill bitmap bit, so LDX_DW returns a scalar.

---

### T-BPF-021  Spill table overflow (>32 slots) falls back to scalar on reload

**Validates:** safe-bpf-interpreter.md §6.3, §6.4

**Procedure:**
1. Construct bytecode that spills the same Context pointer to 33 distinct 8-byte-aligned stack slots (slots 0 through 32), exceeding `MAX_SPILL_SLOTS`. Then reload slot 32 with `LDX_DW` and attempt to dereference.
2. Execute with `execute_program(...)`.
3. Assert: the dereference fails with `Err(BpfError::NonDereferenceableAccess { .. })` — the 33rd spill exceeded the table capacity, so the reload produced a scalar.

---

## 6  Call frame tests

### T-BPF-022  R6–R9 pointer tags saved/restored across BPF-to-BPF calls

**Validates:** safe-bpf-interpreter.md §7.1

**Procedure:**
1. Construct bytecode with two functions (caller + callee):
   - Caller: `MOV r6, r1` (save Context pointer in callee-saved R6), `CALL callee` (BPF-to-BPF call, src=1), then `LDX_B r0, [r6 + 0]` (dereference R6 after return).
   - Callee: `MOV r6, 0` (clobber R6 to scalar in callee scope), `EXIT`.
2. Execute with `execute_program(...)`.
3. Assert: the load succeeds — R6's Context tag was saved on CALL and restored on EXIT, even though the callee overwrote R6 in its own scope.

---

### T-BPF-023  R10 Stack tag invariant maintained across call frames

**Validates:** safe-bpf-interpreter.md §7.1

**Procedure:**
1. Construct bytecode with caller + callee:
   - Caller: `STX_DW [r10 − 8], r0` (store to caller stack frame), `CALL callee`, `EXIT`.
   - Callee: `STX_DW [r10 − 8], r0` (store to callee stack frame — different R10 offset), `EXIT`.
2. Execute with `execute_program(...)`.
3. Assert: the program succeeds — R10 retains the Stack tag in both frames. Both stores are valid because the Stack region spans the entire stack allocation.

---

## 7  Helper return validation tests

### T-BPF-024  `MapValueOrNull` helper returns 0 → R0 tagged scalar

**Validates:** safe-bpf-interpreter.md §5.2

**Procedure:**
1. Register a helper with `HelperReturn::MapValueOrNull { map_arg: 1 }` that returns 0 (NULL / not found).
2. Construct bytecode: `LD_DW_IMM r1, src=1, imm=0` (R1 = MapDescriptor for map 0), `CALL helper_id`, `LDX_B r0, [r0 + 0]` (attempt to dereference R0).
3. Execute with `execute_program(...)`.
4. Assert: the dereference fails with `Err(BpfError::NonDereferenceableAccess { .. })` — R0 was tagged scalar because the helper returned NULL.

---

### T-BPF-025  `MapValueOrNull` helper returns valid pointer → R0 tagged MapValue with validated bounds

**Validates:** safe-bpf-interpreter.md §5.2

**Procedure:**
1. Allocate a map buffer (e.g., 64 bytes). Register a helper with `HelperReturn::MapValueOrNull { map_arg: 1 }` that returns a pointer into the map's data region.
2. Construct bytecode: `LD_DW_IMM r1, src=1, imm=0`, `CALL helper_id`, `LDX_B r0, [r0 + 0]` (dereference returned pointer).
3. Execute with `execute_program(...)`.
4. Assert: the program succeeds — R0 was tagged `MapValue` with valid bounds, and the load is within the map region.

---

### T-BPF-026  Helper returns out-of-bounds pointer → `MemoryAccessViolation`

**Validates:** safe-bpf-interpreter.md §5.2

**Procedure:**
1. Register a helper with `HelperReturn::MapValueOrNull { map_arg: 1 }` that returns a pointer **outside** the map's data region (e.g., map base − 1).
2. Construct bytecode: `LD_DW_IMM r1, src=1, imm=0`, `CALL helper_id`.
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::MemoryAccessViolation { .. })` — the interpreter rejects the helper's return value during validation before tagging R0.

---

## 8  Helper argument validation tests

### T-BPF-027  Helper expects MapDescriptor but receives scalar → `InvalidHelperArgument`

**Validates:** safe-bpf-interpreter.md §5.2

**Procedure:**
1. Register a helper with `HelperReturn::MapValueOrNull { map_arg: 1 }`.
2. Construct bytecode: `MOV r1, 42` (R1 = scalar, not a MapDescriptor), `CALL helper_id`.
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::InvalidHelperArgument { .. })`.

---

## 9  LD_DW_IMM map relocation tests

### T-BPF-028  LD_DW_IMM src=1 with negative imm → `InvalidMapIndex`

**Validates:** safe-bpf-interpreter.md §4.2

**Procedure:**
1. Construct bytecode: `LD_DW_IMM r1, src=1, imm=-1`.
2. Provide one valid map definition.
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::InvalidMapIndex { pc: .., index: -1 })`.

---

### T-BPF-029  LD_DW_IMM src=1 with imm ≥ maps.len() → `InvalidMapIndex`

**Validates:** safe-bpf-interpreter.md §4.2

**Procedure:**
1. Construct bytecode: `LD_DW_IMM r1, src=1, imm=5`.
2. Provide only 2 map definitions (indices 0 and 1).
3. Execute with `execute_program(...)`.
4. Assert: result is `Err(BpfError::InvalidMapIndex { pc: .., index: 5 })`.

---

### T-BPF-030  LD_DW_IMM src=1 happy path — R0 tagged MapDescriptor

**Validates:** safe-bpf-interpreter.md §4.2

**Procedure:**
1. Construct bytecode: `LD_DW_IMM r1, src=1, imm=0`, then call a helper registered with `HelperReturn::MapValueOrNull { map_arg: 1 }` (which expects R1 to carry a MapDescriptor tag), then `EXIT`.
2. Provide one valid map definition at index 0 and register the helper.
3. Execute with `execute_program(...)`.
4. Assert: the program completes without error — the helper call succeeds, confirming R1 was tagged as MapDescriptor (if the tag were missing, the helper call would fail with `InvalidHelperArgument`).

---

## 10  End-to-end / integration tests

> **Note:** These tests exercise the interpreter within the broader node firmware stack. They run in `crates/sonde-e2e/tests/` or `crates/sonde-node/` integration tests (not `sonde-bpf` unit tests) and require the mock gateway, mock HAL, and test program library described in [node-validation.md](node-validation.md) §2.

### T-BPF-031  E2E map read/write through full gateway → node → BPF stack

**Validates:** bpf-environment.md §5.3, ND-0504

**E2E coverage:** Partially covered by T-E2E-081 (`t_e2e_081_ephemeral_restrictions`) which deploys a resident program with maps and verifies map state after BPF execution. Direct unit coverage for persistence across cycles exists in `crates/sonde-node/src/map_storage.rs` (`test_data_preserved_when_layout_matches`), which validates that map data is preserved when the RTC layout matches.

**Procedure:**
1. Deploy a BPF program (via mock gateway) that calls `map_lookup_elem` on a defined map, writes a value via `map_update_elem`, then reads it back.
2. Execute a full wake cycle (WAKE → COMMAND → BPF execution).
3. Assert: the value written by `map_update_elem` is read back correctly by `map_lookup_elem` in a subsequent invocation. Map data persists across wake cycles (sleep-persistent memory).

---

### T-BPF-032  Map memory budget exceeded → program load rejected

**Validates:** bpf-environment.md §5.3, ND-0606

**E2E coverage:** Unit coverage for map budget enforcement exists in `crates/sonde-node/src/map_storage.rs` (`test_allocate_exceeds_budget`) and program-load rejection is checked in `crates/sonde-node/src/program_store.rs` via `NodeError::MapBudgetExceeded`. E2E-level coverage via T-E2E-083 (`t_e2e_083_instruction_budget_enforcement`) exercises the budget mechanism end-to-end for instruction budgets; map memory budgets are structurally similar.

**Procedure:**
1. Deploy a BPF program (via mock gateway) that declares map definitions exceeding the node's memory budget.
2. Attempt to load the program.
3. Assert: the firmware rejects the program at load time. The previously installed program remains active and unaffected.

---

### T-BPF-033  Context write from BPF program → silently ignored

**Validates:** bpf-environment.md §4, ND-0505 AC6

**E2E coverage:** Context write rejection is unit-tested in `crates/sonde-node/src/sonde_bpf_adapter.rs` (`t_n929_write_to_read_only_context_silently_ignored`), which validates that writes to the read-only Context region are silently ignored. T-E2E-081 (`t_e2e_081_ephemeral_restrictions`) exercises related ephemeral-program restrictions (map writes, `set_next_wake`) at the E2E level.

**Procedure:**
1. Populate `sonde_context` with known non-zero field values (e.g., `timestamp = 1710000000000`, `battery_mv = 3300`).
2. Deploy a BPF program that loads a distinct non-zero value into R0 (`MOV64_IMM r0, 0xFF`), then attempts to write to the `sonde_context` structure via `STX_DW [r1 + 0], r0` (R1 = Context pointer), followed by `EXIT`.
3. Execute a full wake cycle with `read_only_ctx = true`.
4. Assert: the program completes successfully — the write to the read-only Context region is silently ignored per ND-0505 AC6, and the `sonde_context` fields retain their original values.

---

## Appendix A  Traceability matrix

| Spec section | Test IDs | Category |
|---|---|---|
| safe-bpf-interpreter.md §3.1 (`mem_load`) | T-BPF-001, T-BPF-003 | Pointer dereference |
| safe-bpf-interpreter.md §3.2 (`mem_store`) | T-BPF-002 | Pointer dereference |
| safe-bpf-interpreter.md §3.3 (`mem_atomic`) | T-BPF-004, T-BPF-005 | Pointer dereference |
| safe-bpf-interpreter.md §4.1 (Initialization) | T-BPF-017 | Tag propagation |
| safe-bpf-interpreter.md §4.2 (LD_DW_IMM) | T-BPF-028, T-BPF-029, T-BPF-030 | Map relocation |
| safe-bpf-interpreter.md §4.3 (ALU / pointer arithmetic) | T-BPF-006 – T-BPF-014, T-BPF-015 | Pointer arithmetic, tag propagation |
| safe-bpf-interpreter.md §4.6 (CALL / EXIT) | T-BPF-016 | Tag propagation |
| safe-bpf-interpreter.md §5.2 (Helper return) | T-BPF-024, T-BPF-025, T-BPF-026, T-BPF-027 | Helper validation |
| safe-bpf-interpreter.md §6.3 (Spill operations) | T-BPF-018, T-BPF-019, T-BPF-020 | Spill tracking |
| safe-bpf-interpreter.md §6.3, §6.4 (Spill overflow) | T-BPF-021 | Spill tracking |
| safe-bpf-interpreter.md §7.1 (Call frames) | T-BPF-022, T-BPF-023 | Call frames |
| bpf-environment.md §4 (Context) | T-BPF-033 | E2E |
| bpf-environment.md §5.3 (Maps) | T-BPF-031, T-BPF-032 | E2E |
| ND-0504 (BPF execution) | T-BPF-031 | E2E |
| ND-0505 (Execution context) | T-BPF-033 | E2E |
| ND-0606 (Map memory budget) | T-BPF-032 | E2E |
