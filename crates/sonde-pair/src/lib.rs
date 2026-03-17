// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod cbor;
pub mod crypto;
pub mod discovery;
#[cfg(all(windows, feature = "dpapi"))]
pub mod dpapi;
pub mod envelope;
pub mod error;
#[cfg(feature = "file-store")]
pub mod file_store;
pub mod phase1;
pub mod phase2;
pub mod rng;
#[cfg(all(target_os = "linux", feature = "secret-service-store"))]
pub mod secret_service_store;
pub mod store;
pub mod transport;
pub mod types;
pub mod validation;

#[cfg(feature = "btleplug")]
pub mod btleplug_transport;

#[cfg(feature = "android")]
pub mod android_store;
#[cfg(feature = "android")]
pub mod android_transport;
