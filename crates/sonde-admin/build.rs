// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../sonde-gateway/proto");
    println!("cargo:rerun-if-changed=../sonde-gateway/proto/admin.proto");
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(
            &["../sonde-gateway/proto/admin.proto"],
            &["../sonde-gateway/proto"],
        )?;
    Ok(())
}
