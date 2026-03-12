// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    println!("cargo:rerun-if-changed=proto/admin.proto");
    tonic_build::compile_protos("proto/admin.proto")?;
    Ok(())
}
