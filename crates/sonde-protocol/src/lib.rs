#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod chunk;
pub mod codec;
pub mod constants;
pub mod error;
pub mod header;
pub mod messages;
pub mod program_image;
pub mod traits;

pub use chunk::{chunk_count, get_chunk};
pub use codec::{decode_frame, encode_frame, verify_frame, DecodedFrame};
pub use constants::*;
pub use error::{DecodeError, EncodeError};
pub use header::FrameHeader;
pub use messages::{CommandPayload, GatewayMessage, NodeMessage};
pub use program_image::{program_hash, MapDef, ProgramImage};
pub use traits::{HmacProvider, Sha256Provider};
