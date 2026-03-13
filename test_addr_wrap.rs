// Check for potential issues with wrapping_add on address calculation
use sonde_bpf::ebpf;
use sonde_bpf::interpreter::execute_program;

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let regs = (src << 4) | (dst & 0x0f);
    let off_bytes = off.to_le_bytes();
    let imm_bytes = imm.to_le_bytes();
    [opc, regs, off_bytes[0], off_bytes[1], imm_bytes[0], imm_bytes[1], imm_bytes[2], imm_bytes[3]]
}

fn prog_from(insns: &[[u8; 8]]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.iter().copied()).collect()
}

fn main() {
    println!("Testing wrapping_add behavior in address calculation...\n");
    
    // In the code: let addr = (reg[src] as i64).wrapping_add(insn.off as i64) as u64;
    // This converts to i64, does wrapping signed add, then back to u64.
    // Let's test if we can trick it into accessing out-of-bounds memory.
    
    // Test: reg[src] = u64::MAX, off = 1
    // As i64: -1 + 1 = 0
    // As u64: 0
    println!("Test: reg=u64::MAX, off=1 => addr should be 0");
    let addr_val = (u64::MAX as i64).wrapping_add(1i64) as u64;
    println!("  Computed addr: {}", addr_val);
    
    // Test: reg[src] = mem_base, off = i16::MAX
    // This should add a large positive offset
    println!("\nTest: reg=mem_base, off=i16::MAX");
    let mem_base = 0x1000u64;
    let offset = i16::MAX;
    let addr_val = (mem_base as i64).wrapping_add(offset as i64) as u64;
    println!("  mem_base: {:#x}, offset: {}, addr: {:#x}", mem_base, offset, addr_val);
    
    // Test: reg[src] = mem_base, off = i16::MIN
    // This should subtract, potentially wrapping
    println!("\nTest: reg=mem_base, off=i16::MIN");
    let addr_val = (mem_base as i64).wrapping_add(i16::MIN as i64) as u64;
    println!("  mem_base: {:#x}, offset: {}, addr: {:#x}", mem_base, i16::MIN, addr_val);
    
    // Real test: Try to load with a carefully crafted offset
    println!("\nActual interpreter test: Load with large positive offset beyond bounds");
    let prog = prog_from(&[
        insn(ebpf::LD_B_REG, 0, 1, i16::MAX, 0), // load from [mem_base + 32767]
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let mut mem = [0u8; 16]; // Only 16 bytes
    match execute_program(&prog, &mut mem, &[]) {
        Ok(_) => println!("  UNEXPECTED: Succeeded"),
        Err(e) => println!("  Error (expected): {:?}", e),
    }
}
