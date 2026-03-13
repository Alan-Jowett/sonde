// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! USB pairing handler for the node firmware.
//!
//! Handles pairing commands received over a serial transport. Dispatches
//! to [`KeyStore`] operations. Reuses `sonde-protocol`'s modem codec framing.

use sonde_protocol::modem::{
    encode_modem_frame, ModemMessage, PairAck, PairingReady, ResetAck,
    PAIRING_STATUS_STORAGE_ERROR, PAIRING_STATUS_SUCCESS, PAIR_ACK_ALREADY_PAIRED,
};

use crate::key_store::KeyStore;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;
use crate::FIRMWARE_ABI_VERSION;

/// Result of handling one pairing message.
pub enum PairingAction {
    /// Continue listening for more messages.
    Continue,
}

/// Handle a single received pairing message. Returns bytes to send back
/// and the action to take.
pub fn handle_pairing_message<S: PlatformStorage>(
    msg: &ModemMessage,
    storage: &mut S,
    map_storage: &mut MapStorage,
) -> (Option<Vec<u8>>, PairingAction) {
    match msg {
        ModemMessage::PairRequest(req) => {
            let mut ks = KeyStore::new(storage);
            let status = match ks.pair(req.key_hint, &req.psk) {
                Ok(()) => PAIRING_STATUS_SUCCESS,
                Err(crate::error::NodeError::StorageError(ref msg))
                    if msg.contains("already paired") =>
                {
                    PAIR_ACK_ALREADY_PAIRED
                }
                Err(_) => PAIRING_STATUS_STORAGE_ERROR,
            };
            let ack = ModemMessage::PairAck(PairAck { status });
            let frame = encode_modem_frame(&ack).ok();
            (frame, PairingAction::Continue)
        }
        ModemMessage::ResetRequest => {
            let mut ks = KeyStore::new(storage);
            let status = match ks.factory_reset(map_storage) {
                Ok(()) => PAIRING_STATUS_SUCCESS,
                Err(_) => PAIRING_STATUS_STORAGE_ERROR,
            };
            let ack = ModemMessage::ResetAck(ResetAck { status });
            let frame = encode_modem_frame(&ack).ok();
            (frame, PairingAction::Continue)
        }
        ModemMessage::IdentityRequest => {
            let identity = storage.read_key();
            let resp = match identity {
                Some((key_hint, _)) => ModemMessage::IdentityResponse(
                    sonde_protocol::modem::IdentityResponse::Paired { key_hint },
                ),
                None => ModemMessage::IdentityResponse(
                    sonde_protocol::modem::IdentityResponse::Unpaired,
                ),
            };
            let frame = encode_modem_frame(&resp).ok();
            (frame, PairingAction::Continue)
        }
        _ => {
            // Unknown message — silently discard (forward compatibility).
            (None, PairingAction::Continue)
        }
    }
}

/// Build a `PAIRING_READY` frame.
pub fn pairing_ready_frame() -> Vec<u8> {
    let msg = ModemMessage::PairingReady(PairingReady {
        firmware_version: FIRMWARE_ABI_VERSION,
    });
    encode_modem_frame(&msg).expect("PairingReady encode cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{NodeError, NodeResult};
    use crate::traits::PlatformStorage;
    use sonde_protocol::modem::{
        FrameDecoder, IdentityResponse, PairRequest, ResetAck, PAIRING_STATUS_STORAGE_ERROR,
        PAIRING_STATUS_SUCCESS, PAIR_ACK_ALREADY_PAIRED, PSK_SIZE,
    };

    /// In-memory storage for testing.
    struct FakeStorage {
        key: Option<(u16, [u8; 32])>,
    }

    impl FakeStorage {
        fn new() -> Self {
            Self { key: None }
        }

        fn paired(key_hint: u16, psk: [u8; 32]) -> Self {
            Self {
                key: Some((key_hint, psk)),
            }
        }
    }

    impl PlatformStorage for FakeStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }
        fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
            if self.key.is_some() {
                return Err(crate::error::NodeError::StorageError(
                    "already paired; factory reset required".into(),
                ));
            }
            self.key = Some((key_hint, *psk));
            Ok(())
        }
        fn erase_key(&mut self) -> NodeResult<()> {
            self.key = None;
            Ok(())
        }
        fn read_schedule(&self) -> (u32, u8) {
            (60, 0)
        }
        fn write_schedule_interval(&mut self, _interval_s: u32) -> NodeResult<()> {
            Ok(())
        }
        fn write_active_partition(&mut self, _partition: u8) -> NodeResult<()> {
            Ok(())
        }
        fn reset_schedule(&mut self) -> NodeResult<()> {
            Ok(())
        }
        fn read_program(&self, _partition: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_program(&mut self, _partition: u8, _image: &[u8]) -> NodeResult<()> {
            Ok(())
        }
        fn erase_program(&mut self, _partition: u8) -> NodeResult<()> {
            Ok(())
        }
        fn take_early_wake_flag(&mut self) -> bool {
            false
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            Ok(())
        }
    }

    fn decode_response(frame: &[u8]) -> ModemMessage {
        let mut decoder = FrameDecoder::new();
        decoder.push(frame);
        decoder.decode().unwrap().unwrap()
    }

    #[test]
    fn pair_unpaired_node_succeeds() {
        let mut storage = FakeStorage::new();
        let mut maps = MapStorage::new(1024);
        let psk = [0xAA; PSK_SIZE];
        let msg = ModemMessage::PairRequest(PairRequest {
            key_hint: 0x1234,
            psk,
        });

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::PairAck(PairAck {
                status: PAIRING_STATUS_SUCCESS
            })
        );
        assert_eq!(storage.read_key(), Some((0x1234, psk)));
    }

    #[test]
    fn pair_already_paired_returns_already_paired() {
        let mut storage = FakeStorage::paired(0x0001, [0xBB; 32]);
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::PairRequest(PairRequest {
            key_hint: 0x9999,
            psk: [0xCC; PSK_SIZE],
        });

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::PairAck(PairAck {
                status: PAIR_ACK_ALREADY_PAIRED,
            })
        );
    }

    #[test]
    fn factory_reset_clears_key() {
        let mut storage = FakeStorage::paired(0x0001, [0xBB; 32]);
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::ResetRequest;

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::ResetAck(ResetAck {
                status: PAIRING_STATUS_SUCCESS,
            })
        );
        assert_eq!(storage.read_key(), None);
    }

    #[test]
    fn identity_paired() {
        let mut storage = FakeStorage::paired(0x00FF, [0x11; 32]);
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::IdentityRequest;

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::IdentityResponse(IdentityResponse::Paired { key_hint: 0x00FF })
        );
    }

    #[test]
    fn identity_unpaired() {
        let mut storage = FakeStorage::new();
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::IdentityRequest;

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::IdentityResponse(IdentityResponse::Unpaired)
        );
    }

    #[test]
    fn unknown_message_returns_none() {
        let mut storage = FakeStorage::new();
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::Unknown {
            msg_type: 0xFE,
            body: vec![],
        };

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        assert!(frame.is_none());
    }

    #[test]
    fn pairing_ready_frame_round_trips() {
        let frame = pairing_ready_frame();
        let msg = decode_response(&frame);
        assert_eq!(
            msg,
            ModemMessage::PairingReady(PairingReady {
                firmware_version: FIRMWARE_ABI_VERSION,
            })
        );
    }

    /// Storage mock that fails on write operations.
    struct FailingStorage;

    impl PlatformStorage for FailingStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            None
        }
        fn write_key(&mut self, _key_hint: u16, _psk: &[u8; 32]) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
        fn erase_key(&mut self) -> NodeResult<()> {
            Err(NodeError::StorageError("erase failed".into()))
        }
        fn read_schedule(&self) -> (u32, u8) {
            (60, 0)
        }
        fn write_schedule_interval(&mut self, _interval_s: u32) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
        fn write_active_partition(&mut self, _partition: u8) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
        fn reset_schedule(&mut self) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
        fn read_program(&self, _partition: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_program(&mut self, _partition: u8, _image: &[u8]) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
        fn erase_program(&mut self, _partition: u8) -> NodeResult<()> {
            Err(NodeError::StorageError("erase failed".into()))
        }
        fn take_early_wake_flag(&mut self) -> bool {
            false
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            Err(NodeError::StorageError("write failed".into()))
        }
    }

    #[test]
    fn pair_storage_error_returns_storage_error_status() {
        let mut storage = FailingStorage;
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::PairRequest(PairRequest {
            key_hint: 0x1234,
            psk: [0xAA; PSK_SIZE],
        });

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::PairAck(PairAck {
                status: PAIRING_STATUS_STORAGE_ERROR,
            })
        );
    }

    #[test]
    fn factory_reset_storage_error_returns_storage_error_status() {
        let mut storage = FailingStorage;
        let mut maps = MapStorage::new(1024);
        let msg = ModemMessage::ResetRequest;

        let (frame, _action) = handle_pairing_message(&msg, &mut storage, &mut maps);
        let resp = decode_response(frame.as_ref().unwrap());
        assert_eq!(
            resp,
            ModemMessage::ResetAck(ResetAck {
                status: PAIRING_STATUS_STORAGE_ERROR,
            })
        );
    }
}
