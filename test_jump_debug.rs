// Debug test for jump issue
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
    // Program has 3 instructions (indices 0, 1, 2)
    // Insn 0: MOV r0, 42
    // Insn 1: JA +1 (after fetch, pc=2, target = 2+1 = 3, which is >= num_insns=3)
    // Insn 2: EXIT
    
    let prog = prog_from(&[
        insn(ebpf::MOV64_IMM, 0, 0, 0, 42),  // insn 0
        insn(ebpf::JA, 0, 0, 1, 0),          // insn 1: jump +1 instruction
        insn(ebpf::EXIT, 0, 0, 0, 0),        // insn 2
    ]);
    
    println!("Program has {} bytes, {} instructions", prog.len(), prog.len() / 8);
    println!("Insn 0: MOV64_IMM");
    println!("Insn 1: JA +1");  
    println!("Insn 2: EXIT");
    println!("After fetching insn 1, pc=2. Target = 2+1 = 3.");
    println!("num_insns = 3, so target >= num_insns, which fails the check.");
    println!("This means JA +1 from insn 1 can never reach the EXIT at insn 2!");
    
    let mut mem = [];
    match execute_program(&prog, &mut mem, &[]) {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {:?}", e),
    }
}
