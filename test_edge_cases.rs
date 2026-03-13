// Test file to check edge cases in sonde-bpf interpreter
use sonde_bpf::ebpf;
use sonde_bpf::interpreter::{execute_program, BpfError};

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let regs = (src << 4) | (dst & 0x0f);
    let off_bytes = off.to_le_bytes();
    let imm_bytes = imm.to_le_bytes();
    [
        opc,
        regs,
        off_bytes[0],
        off_bytes[1],
        imm_bytes[0],
        imm_bytes[1],
        imm_bytes[2],
        imm_bytes[3],
    ]
}

fn prog_from(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.iter().copied()).collect()
}

fn main() {
    println!("Testing edge cases...\n");

    // Test 1: SDIV with i32::MIN / -1 (overflow case)
    println!("Test 1: SDIV32 i32::MIN / -1 (overflow)");
    let prog = prog_from(&[
        insn(ebpf::MOV32_IMM, 0, 0, 0, i32::MIN),
        insn(ebpf::DIV32_IMM, 0, 0, 1, -1), // SDIV32, off=1
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => {
            // wrapping_div returns i32::MIN on overflow per Rust semantics
            println!("  Result: {:#x} (expected {:#x} from wrapping)", result, i32::MIN as u32 as u64);
        }
        Err(e) => println!("  Error: {:?}", e),
    }

    // Test 2: SDIV64 with i64::MIN / -1
    println!("\nTest 2: SDIV64 i64::MIN / -1 (overflow)");
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 0, 0, 0, 0),
        insn(0x00, 0, 0, 0, -2147483648), // i64::MIN split
        insn(ebpf::DIV64_IMM, 0, 0, 1, -1), // SDIV64, off=1
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => {
            println!("  Result: {:#x}", result);
        }
        Err(e) => println!("  Error: {:?}", e),
    }

    // Test 3: Shift by >= bitwidth (should be masked)
    println!("\nTest 3: LSH64 by 64+ (should mask to 0)");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 0xff),
        insn(ebpf::LSH64_IMM, 0, 0, 0, 64), // shift by 64
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => {
            // With & 0x3f mask, 64 becomes 0, so no shift
            println!("  Result: {:#x} (expected {:#x})", result, 0xff);
        }
        Err(e) => println!("  Error: {:?}", e),
    }

    // Test 4: Memory bounds at edge
    println!("\nTest 4: Store at last valid byte");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 2, 0, 0, 15), // r2 = 15 (last offset in 16-byte mem)
        insn(ebpf::ADD64_REG, 2, 1, 0, 0),  // r2 += r1 (mem base)
        insn(ebpf::ST_B_IMM, 2, 0, 0, 0xAB),
        insn(ebpf::LD_B_REG, 0, 2, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => println!("  Result: {:#x} (expected 0xab)", result),
        Err(e) => println!("  Error: {:?}", e),
    }

    // Test 5: Memory access one byte past end
    println!("\nTest 5: Load from one byte past end (should fail)");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 2, 0, 0, 16), // r2 = 16 (one past end)
        insn(ebpf::ADD64_REG, 2, 1, 0, 0),  // r2 += r1
        insn(ebpf::LD_B_REG, 0, 2, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded when should fail"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }

    // Test 6: check_mem with addr that would overflow on addition
    println!("\nTest 6: Memory access with large addr (overflow check)");
    let prog = prog_from(&[
        insn(ebpf::LD_DW_IMM, 2, 0, 0, u32::MAX as i32),
        insn(0x00, 0, 0, 0, u32::MAX as i32), // r2 = u64::MAX
        insn(ebpf::LD_DW_REG, 0, 2, 0, 0), // try to load from u64::MAX
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded when should fail"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }

    // Test 7: Negative offset wrapping to very large address
    println!("\nTest 7: Load with negative offset from 0");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 2, 0, 0, 0), // r2 = 0
        insn(ebpf::LD_B_REG, 0, 2, -1, 0), // load from [0 + (-1)]
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded when should fail"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }

    // Test 8: Jump offset computation edge case
    println!("\nTest 8: Jump forward to last instruction");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),
        insn(ebpf::JA, 0, 0, 1, 0), // jump to insn 2 (exit)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => println!("  Result: {} (expected 42)", result),
        Err(e) => println!("  Error: {:?}", e),
    }

    // Test 9: Jump with large negative offset
    println!("\nTest 9: Jump backward beyond start (should fail)");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 1),
        insn(ebpf::JA, 0, 0, -10, 0), // jump to -9 (out of bounds)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded when should fail"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }

    // Test 10: Invalid register index
    println!("\nTest 10: Instruction with dst=11 (invalid register)");
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 11, 0, 0, 42), // dst=11 is invalid (max is 10)
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded when should fail"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }

    println!("\nAll edge case tests completed.");
}
