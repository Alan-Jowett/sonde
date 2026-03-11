// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Fuzz target: GatewayMessage::decode with arbitrary msg_type and CBOR bytes.
//! The CBOR decoder must never panic on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sonde_protocol::GatewayMessage;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let msg_type = data[0];
    let cbor = &data[1..];
    let _ = GatewayMessage::decode(msg_type, cbor);
});
