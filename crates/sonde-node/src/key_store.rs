// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::NodeResult;
use crate::map_storage::MapStorage;
use crate::traits::PlatformStorage;

/// Key store operations for PSK management, pairing, and factory reset.
pub struct KeyStore<'a, S: PlatformStorage> {
    storage: &'a mut S,
}

/// The node's identity material loaded from the key partition.
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub key_hint: u16,
    pub psk: [u8; 32],
}

impl<'a, S: PlatformStorage> KeyStore<'a, S> {
    pub fn new(storage: &'a mut S) -> Self {
        Self { storage }
    }

    /// Load the node's identity from the key partition.
    /// Returns `None` if the node is unpaired.
    pub fn load_identity(&self) -> Option<NodeIdentity> {
        self.storage
            .read_key()
            .map(|(key_hint, psk)| NodeIdentity { key_hint, psk })
    }

    /// Factory reset: erase PSK, programs, map data, schedule, and channel.
    ///
    /// Per security.md §2.6 and node-design.md §6.2, this erases:
    /// 1. Key partition (PSK + key_hint + magic)
    /// 2. Both program partitions
    /// 3. All map data in sleep-persistent memory (zeroed)
    /// 4. Schedule partition (reset to default interval)
    /// 5. Stored WiFi channel (reset to default)
    /// 6. BLE pairing artifacts: peer_payload erased, reg_complete cleared (ND-0917)
    ///
    /// After this, the node is inert until re-paired via BLE.
    pub fn factory_reset(&mut self, map_storage: &mut MapStorage) -> NodeResult<()> {
        self.storage.erase_key()?;
        self.storage.erase_program(0)?;
        self.storage.erase_program(1)?;
        map_storage.clear_all();
        self.storage.reset_schedule()?;
        // Clear stored WiFi channel so re-pairing with a different gateway
        // on a different channel is not broken by a stale channel value.
        self.storage.write_channel(1)?;
        // Clear BLE pairing artifacts (ND-0917): peer_payload may or may not
        // exist depending on whether BLE provisioning was previously done.
        self.storage.erase_peer_payload()?;
        // Always reset the reg_complete flag so the next boot does not skip
        // the PEER_REQUEST phase.
        self.storage.write_reg_complete(false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NodeError;
    use crate::traits::PlatformStorage;

    /// In-memory mock implementation of PlatformStorage for testing.
    struct MockStorage {
        key: Option<(u16, [u8; 32])>,
        schedule_interval: u32,
        active_partition: u8,
        programs: [Option<Vec<u8>>; 2],
        early_wake_flag: bool,
        channel: Option<u8>,
        peer_payload: Option<Vec<u8>>,
        reg_complete: bool,
        // Failure-injection flags for error-path testing.
        fail_erase_key: bool,
        fail_erase_program: bool,
        fail_reset_schedule: bool,
        fail_write_channel: bool,
        fail_erase_peer_payload: bool,
        fail_write_reg_complete: bool,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                key: None,
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
                early_wake_flag: false,
                channel: None,
                peer_payload: None,
                reg_complete: false,
                fail_erase_key: false,
                fail_erase_program: false,
                fail_reset_schedule: false,
                fail_write_channel: false,
                fail_erase_peer_payload: false,
                fail_write_reg_complete: false,
            }
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }

        fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
            if self.key.is_some() {
                return Err(NodeError::StorageError("already paired"));
            }
            self.key = Some((key_hint, *psk));
            Ok(())
        }

        fn erase_key(&mut self) -> NodeResult<()> {
            if self.fail_erase_key {
                return Err(NodeError::StorageError("injected erase_key failure"));
            }
            self.key = None;
            Ok(())
        }

        fn read_schedule(&self) -> (u32, u8) {
            (self.schedule_interval, self.active_partition)
        }

        fn write_schedule_interval(&mut self, interval_s: u32) -> NodeResult<()> {
            self.schedule_interval = interval_s;
            Ok(())
        }

        fn write_active_partition(&mut self, partition: u8) -> NodeResult<()> {
            self.active_partition = partition;
            Ok(())
        }

        fn reset_schedule(&mut self) -> NodeResult<()> {
            if self.fail_reset_schedule {
                return Err(NodeError::StorageError("injected reset_schedule failure"));
            }
            self.schedule_interval = 60;
            self.active_partition = 0;
            Ok(())
        }

        fn read_program(&self, partition: u8) -> Option<Vec<u8>> {
            self.programs[partition as usize].clone()
        }

        fn write_program(&mut self, partition: u8, image: &[u8]) -> NodeResult<()> {
            self.programs[partition as usize] = Some(image.to_vec());
            Ok(())
        }

        fn erase_program(&mut self, partition: u8) -> NodeResult<()> {
            if self.fail_erase_program {
                return Err(NodeError::StorageError("injected erase_program failure"));
            }
            self.programs[partition as usize] = None;
            Ok(())
        }

        fn take_early_wake_flag(&mut self) -> bool {
            let v = self.early_wake_flag;
            self.early_wake_flag = false;
            v
        }

        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            self.early_wake_flag = true;
            Ok(())
        }

        fn read_channel(&self) -> Option<u8> {
            self.channel
        }

        fn write_channel(&mut self, channel: u8) -> NodeResult<()> {
            if self.fail_write_channel {
                return Err(NodeError::StorageError("injected write_channel failure"));
            }
            self.channel = Some(channel);
            Ok(())
        }

        fn read_peer_payload(&self) -> Option<Vec<u8>> {
            self.peer_payload.clone()
        }

        fn write_peer_payload(&mut self, payload: &[u8]) -> NodeResult<()> {
            self.peer_payload = Some(payload.to_vec());
            Ok(())
        }

        fn erase_peer_payload(&mut self) -> NodeResult<()> {
            if self.fail_erase_peer_payload {
                return Err(NodeError::StorageError(
                    "injected erase_peer_payload failure",
                ));
            }
            self.peer_payload = None;
            Ok(())
        }

        fn read_reg_complete(&self) -> bool {
            self.reg_complete
        }

        fn write_reg_complete(&mut self, complete: bool) -> NodeResult<()> {
            if self.fail_write_reg_complete {
                return Err(NodeError::StorageError(
                    "injected write_reg_complete failure",
                ));
            }
            self.reg_complete = complete;
            Ok(())
        }
    }

    #[test]
    fn test_load_identity_unpaired() {
        // T-N400, T-N401: No stored PSK → load_identity returns None (unpaired).
        let mut storage = MockStorage::new();
        let ks = KeyStore::new(&mut storage);
        assert!(ks.load_identity().is_none());
    }

    #[test]
    fn test_factory_reset() {
        // T-N404: Factory reset erases all persistent state (key, programs,
        // schedule, channel, BLE artifacts, map data).
        let mut storage = MockStorage::new();
        let psk = [0xDD; 32];
        storage.key = Some((10, psk));
        storage.programs[0] = Some(vec![1, 2, 3]);
        storage.programs[1] = Some(vec![4, 5, 6]);
        storage.schedule_interval = 300;
        storage.active_partition = 1;
        storage.channel = Some(6);
        storage.peer_payload = Some(vec![0xAB; 64]);
        storage.reg_complete = true;

        let mut map_storage = MapStorage::new(4096);
        // Allocate some maps to verify they get cleared
        use sonde_protocol::MapDef;
        map_storage
            .allocate(&[MapDef {
                map_type: 1,
                key_size: 4,
                value_size: 4,
                max_entries: 4,
            }])
            .unwrap();
        map_storage
            .get_mut(0)
            .unwrap()
            .update(0, &[1, 2, 3, 4])
            .unwrap();

        {
            let mut ks = KeyStore::new(&mut storage);
            ks.factory_reset(&mut map_storage).unwrap();
        }

        assert!(storage.key.is_none());
        assert!(storage.programs[0].is_none());
        assert!(storage.programs[1].is_none());
        assert_eq!(storage.schedule_interval, 60); // reset to default
        assert_eq!(storage.active_partition, 0); // reset to default
        assert_eq!(storage.channel, Some(1)); // reset to default channel
                                              // BLE pairing artifacts must be cleared (ND-0917)
        assert!(storage.peer_payload.is_none());
        assert!(!storage.reg_complete);
        // Map data should be zeroed
        assert_eq!(
            map_storage.get(0).unwrap().lookup(0).unwrap(),
            &[0, 0, 0, 0]
        );
    }

    #[test]
    fn test_load_identity_paired() {
        // T-N402: Paired node with stored PSK → load_identity returns the
        // correct key_hint and PSK.
        let mut storage = MockStorage::new();
        let psk = [0x42; 32];
        storage.key = Some((0xBEEF, psk));

        let ks = KeyStore::new(&mut storage);
        let id = ks.load_identity().expect("should return identity");
        assert_eq!(id.key_hint, 0xBEEF);
        assert_eq!(id.psk, psk);
    }

    // -- factory_reset error-path tests ------------------------------------
    // Each test injects a failure at one step and asserts that factory_reset
    // propagates the StorageError.

    /// Helper: build a fully-populated MockStorage suitable for factory_reset.
    fn populated_storage() -> MockStorage {
        let mut s = MockStorage::new();
        s.key = Some((10, [0xDD; 32]));
        s.programs[0] = Some(vec![1, 2, 3]);
        s.programs[1] = Some(vec![4, 5, 6]);
        s.schedule_interval = 300;
        s.active_partition = 1;
        s.channel = Some(6);
        s.peer_payload = Some(vec![0xAB; 64]);
        s.reg_complete = true;
        s
    }

    #[test]
    fn test_factory_reset_erase_key_fails() {
        // T-N405: erase_key failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_erase_key = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(err, NodeError::StorageError("injected erase_key failure"));
    }

    #[test]
    fn test_factory_reset_erase_program_fails() {
        // T-N406: erase_program failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_erase_program = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(
            err,
            NodeError::StorageError("injected erase_program failure")
        );
    }

    #[test]
    fn test_factory_reset_reset_schedule_fails() {
        // T-N407: reset_schedule failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_reset_schedule = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(
            err,
            NodeError::StorageError("injected reset_schedule failure")
        );
    }

    #[test]
    fn test_factory_reset_write_channel_fails() {
        // T-N408: write_channel failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_write_channel = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(
            err,
            NodeError::StorageError("injected write_channel failure")
        );
    }

    #[test]
    fn test_factory_reset_erase_peer_payload_fails() {
        // T-N409: erase_peer_payload failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_erase_peer_payload = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(
            err,
            NodeError::StorageError("injected erase_peer_payload failure")
        );
    }

    #[test]
    fn test_factory_reset_write_reg_complete_fails() {
        // T-N410: write_reg_complete failure propagates from factory_reset.
        let mut storage = populated_storage();
        storage.fail_write_reg_complete = true;
        let mut map_storage = MapStorage::new(4096);
        let mut ks = KeyStore::new(&mut storage);
        let err = ks.factory_reset(&mut map_storage).unwrap_err();
        assert_eq!(
            err,
            NodeError::StorageError("injected write_reg_complete failure")
        );
    }
}
