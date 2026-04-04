// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Fuzz target: decode_frame with arbitrary bytes.
//! The frame parser must never panic on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sonde_protocol::decode_frame;

fuzz_target!(|data: &[u8]| {
    let _ = decode_frame(data);
});
