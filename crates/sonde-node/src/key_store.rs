// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::{NodeError, NodeResult};
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

    /// Provision a new PSK via USB pairing.
    /// Fails if the node is already paired (factory reset required first).
    pub fn pair(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
        if self.storage.read_key().is_some() {
            return Err(NodeError::StorageError(
                "already paired; factory reset required".into(),
            ));
        }
        self.storage.write_key(key_hint, psk)
    }

    /// Factory reset: erase PSK, programs, map data, and schedule.
    ///
    /// Per security.md §2.6 and node-design.md §6.2, this erases:
    /// 1. Key partition (PSK + key_hint + magic)
    /// 2. Both program partitions
    /// 3. All map data in sleep-persistent memory (zeroed)
    /// 4. Schedule partition (reset to default interval)
    ///
    /// After this, the node is inert until re-paired via USB.
    pub fn factory_reset(&mut self, map_storage: &mut MapStorage) -> NodeResult<()> {
        self.storage.erase_key()?;
        self.storage.erase_program(0)?;
        self.storage.erase_program(1)?;
        map_storage.clear_all();
        self.storage.reset_schedule()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::PlatformStorage;

    /// In-memory mock implementation of PlatformStorage for testing.
    struct MockStorage {
        key: Option<(u16, [u8; 32])>,
        schedule_interval: u32,
        active_partition: u8,
        programs: [Option<Vec<u8>>; 2],
        early_wake_flag: bool,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                key: None,
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
                early_wake_flag: false,
            }
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            self.key
        }

        fn write_key(&mut self, key_hint: u16, psk: &[u8; 32]) -> NodeResult<()> {
            if self.key.is_some() {
                return Err(NodeError::StorageError("already paired".into()));
            }
            self.key = Some((key_hint, *psk));
            Ok(())
        }

        fn erase_key(&mut self) -> NodeResult<()> {
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
    }

    #[test]
    fn test_load_identity_unpaired() {
        let mut storage = MockStorage::new();
        let ks = KeyStore::new(&mut storage);
        assert!(ks.load_identity().is_none());
    }

    #[test]
    fn test_pair_and_load() {
        let mut storage = MockStorage::new();
        let psk = [0xAA; 32];
        {
            let mut ks = KeyStore::new(&mut storage);
            ks.pair(42, &psk).unwrap();
        }
        let ks = KeyStore::new(&mut storage);
        let id = ks.load_identity().expect("should be paired");
        assert_eq!(id.key_hint, 42);
        assert_eq!(id.psk, psk);
    }

    #[test]
    fn test_pair_rejects_already_paired() {
        let mut storage = MockStorage::new();
        let psk = [0xBB; 32];
        {
            let mut ks = KeyStore::new(&mut storage);
            ks.pair(1, &psk).unwrap();
        }
        let mut ks = KeyStore::new(&mut storage);
        let result = ks.pair(2, &[0xCC; 32]);
        assert!(result.is_err());
    }

    #[test]
    fn test_factory_reset() {
        let mut storage = MockStorage::new();
        let psk = [0xDD; 32];
        storage.key = Some((10, psk));
        storage.programs[0] = Some(vec![1, 2, 3]);
        storage.programs[1] = Some(vec![4, 5, 6]);
        storage.schedule_interval = 300;
        storage.active_partition = 1;

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
                                                 // Map data should be zeroed
        assert_eq!(
            map_storage.get(0).unwrap().lookup(0).unwrap(),
            &[0, 0, 0, 0]
        );
    }
}
