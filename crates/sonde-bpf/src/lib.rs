// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! `sonde_bpf` — A simple, zero-allocation BPF interpreter (RFC 9669).
//!
//! This crate provides a BPF bytecode interpreter that:
//! - Allocates **no heap memory** during program execution
//! - Supports the full RFC 9669 instruction set (ALU32/64, JMP/JMP32, memory, atomics)
//! - Uses **tagged registers** to track pointer provenance and enforce memory safety
//! - Registers helpers via [`interpreter::HelperDescriptor`] with return-type metadata
//! - Is `#![no_std]`-compatible (disable the default `std` feature)
//!
//! # Example
//! ```
//! use sonde_bpf::{ebpf, interpreter};
//!
//! // mov64 r0, 42; exit
//! let prog: &[u8] = &[
//!     0xb7, 0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, // r0 = 42
//!     0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // exit
//! ];
//! let mut mem = [];
//! let result = interpreter::execute_program_no_maps(prog, &mut mem, &[], false, u64::MAX).unwrap();
//! assert_eq!(result, 42);
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

pub mod ebpf;
pub mod interpreter;
