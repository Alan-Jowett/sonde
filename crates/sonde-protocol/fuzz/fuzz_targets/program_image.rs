// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Fuzz target: ProgramImage::decode with arbitrary CBOR bytes.
//! The CBOR decoder must never panic on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sonde_protocol::ProgramImage;

fuzz_target!(|data: &[u8]| {
    let _ = ProgramImage::decode(data);
});
