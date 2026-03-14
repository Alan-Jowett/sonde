// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Function signature for BPF helper calls.
/// All arguments and return values are u64 per the BPF calling convention.
pub type HelperFn = fn(r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64;

/// Errors during BPF program execution.
#[derive(Debug, Clone, PartialEq)]
pub enum BpfError {
    /// The program exceeded the instruction budget.
    InstructionBudgetExceeded,
    /// The program exceeded the maximum call depth (8 frames).
    CallDepthExceeded,
    /// The bytecode is invalid or malformed.
    InvalidBytecode(&'static str),
    /// A helper was called that is not registered.
    HelperNotRegistered(u32),
    /// Error during program loading.
    LoadError(&'static str),
    /// Runtime error during BPF execution (memory violation, pointer
    /// arithmetic error, etc.).
    RuntimeError(&'static str),
}

impl core::fmt::Display for BpfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BpfError::InstructionBudgetExceeded => write!(f, "instruction budget exceeded"),
            BpfError::CallDepthExceeded => write!(f, "call depth exceeded"),
            BpfError::InvalidBytecode(msg) => write!(f, "invalid bytecode: {}", msg),
            BpfError::HelperNotRegistered(id) => write!(f, "helper {} not registered", id),
            BpfError::LoadError(msg) => write!(f, "load error: {}", msg),
            BpfError::RuntimeError(msg) => write!(f, "runtime error: {}", msg),
        }
    }
}

impl std::error::Error for BpfError {}

/// BPF interpreter abstraction.
///
/// Both sonde-bpf and uBPF can implement this trait. The choice of interpreter
/// backend does not affect the rest of the firmware design.
pub trait BpfInterpreter {
    /// Register a helper function by call number.
    ///
    /// Helper IDs are part of the firmware ABI and MUST NOT change:
    ///   1=i2c_read, 2=i2c_write, 3=i2c_write_read, 4=spi_transfer,
    ///   5=gpio_read, 6=gpio_write, 7=adc_read, 8=send, 9=send_recv,
    ///   10=map_lookup_elem, 11=map_update_elem, 12=get_time,
    ///   13=get_battery_mv, 14=delay_us, 15=set_next_wake,
    ///   16=bpf_trace_printk
    fn register_helper(&mut self, id: u32, func: HelperFn) -> Result<(), BpfError>;

    /// Load BPF bytecode with map metadata.
    ///
    /// The bytecode may contain unrelocated LDDW `src=1` map reference
    /// instructions. The implementation is responsible for handling these
    /// (either by pre-relocating or by supporting them at runtime).
    ///
    /// `map_ptrs` maps `map_index → runtime pointer` for the backing
    /// storage of each map.
    ///
    /// `map_defs` carries the corresponding [`sonde_protocol::MapDef`]
    /// entries so the backend can compute region sizes for bounds checking.
    fn load(
        &mut self,
        bytecode: &[u8],
        map_ptrs: &[u64],
        map_defs: &[sonde_protocol::MapDef],
    ) -> Result<(), BpfError>;

    /// Execute the loaded program.
    ///
    /// `ctx_ptr` is a pointer to the `SondeContext` struct, passed as R1.
    /// `instruction_budget` is a hint for limiting execution.  Not all
    /// backends enforce this — see implementation docs.  Returns the
    /// program's return value (R0) or an error.
    fn execute(&mut self, ctx_ptr: u64, instruction_budget: u64) -> Result<u64, BpfError>;
}
