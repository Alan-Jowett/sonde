// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod pb {
    tonic::include_proto!("sonde.admin");
}

pub mod grpc_client;
