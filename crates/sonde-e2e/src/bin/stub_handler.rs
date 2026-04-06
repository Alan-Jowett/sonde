// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Minimal stub handler for E2E testing.
//!
//! Reads length-prefixed CBOR `HandlerMessage::Data` from stdin, echoes
//! back a `HandlerMessage::DataReply` with the same `request_id` and a
//! fixed payload `[0xCC, 0xDD]`.
//!
//! Flags:
//! - `--no-reply`  Return an empty `data` vec (suppresses `APP_DATA_REPLY`).
//! - `--receipt-dir <DIR>`  Write received blob bytes to `<DIR>/receipt.bin`
//!   so the test can verify the handler actually received the correct data.

use sonde_gateway::handler::HandlerMessage;
use std::io::{Read, Write};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let no_reply = args.iter().any(|a| a == "--no-reply");
    let receipt_dir: Option<PathBuf> = args
        .windows(2)
        .find(|w| w[0] == "--receipt-dir")
        .map(|w| PathBuf::from(&w[1]));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    loop {
        // Read 4-byte big-endian length prefix.
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).is_err() {
            break; // EOF or pipe closed
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        // Read CBOR payload.
        let mut buf = vec![0u8; len];
        if stdin.read_exact(&mut buf).is_err() {
            break;
        }

        // Decode the message.
        let msg = match HandlerMessage::decode(&buf) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Only reply to Data messages.
        let (request_id, data_blob) = match &msg {
            HandlerMessage::Data {
                request_id, data, ..
            } => (*request_id, data.clone()),
            _ => continue,
        };

        // Write receipt file so the test can verify handler invocation.
        if let Some(ref dir) = receipt_dir {
            let _ = std::fs::write(dir.join("receipt.bin"), &data_blob);
        }

        // Build a DataReply with a fixed echo payload (or empty for --no-reply).
        let reply = HandlerMessage::DataReply {
            request_id,
            data: if no_reply { vec![] } else { vec![0xCC, 0xDD] },
        };

        let payload = match reply.encode() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Write length-prefixed reply.
        let reply_len = (payload.len() as u32).to_be_bytes();
        if stdout.write_all(&reply_len).is_err() {
            break;
        }
        if stdout.write_all(&payload).is_err() {
            break;
        }
        if stdout.flush().is_err() {
            break;
        }
    }
}
