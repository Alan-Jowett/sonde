// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

pub mod admin;
pub mod ble_pairing;
pub mod crypto;
pub mod engine;
pub mod gateway_identity;
pub mod handler;
pub mod key_provider;
pub mod modem;
pub mod phone_trust;
pub mod program;
pub mod registry;
pub mod session;
pub mod sqlite_storage;
pub mod state_bundle;
pub mod storage;
pub mod transport;

pub use admin::AdminService;
pub use crypto::{RustCryptoHmac, RustCryptoSha256};
pub use engine::{Gateway, PendingCommand};
pub use gateway_identity::{GatewayIdentity, IdentityError};
pub use handler::{
    load_handler_configs, HandlerConfig, HandlerConfigError, HandlerMessage, HandlerRouter,
    ProgramMatcher,
};
pub use phone_trust::{PhonePskRecord, PhonePskStatus};
pub use program::{ProgramLibrary, ProgramRecord, VerificationProfile};
pub use registry::{BatteryReading, NodeRecord, SensorDescriptor};
pub use session::{Session, SessionManager, SessionState};
pub use sqlite_storage::SqliteStorage;
pub use storage::{InMemoryStorage, Storage, StorageError};
pub use transport::{MockTransport, PeerAddress, Transport, TransportError};
