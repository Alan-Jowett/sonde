// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/admin.proto")?;
    Ok(())
}
