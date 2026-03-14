// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::{NodeError, NodeResult};
use crate::traits::PlatformStorage;
use sonde_protocol::{MapDef, ProgramImage, Sha256Provider};

/// Contains raw BPF bytecode as stored in the program image. Map reference
/// relocation (LDDW `src=1` map indices) is **not** performed by `ProgramStore`;
/// each `BpfInterpreter` backend is responsible for handling unrelocated
/// references (either by pre-relocating in `load()` or at runtime).
#[derive(Debug, Clone)]
pub struct LoadedProgram {
    /// Raw BPF bytecode with LDDW `src=1` map references not yet relocated.
    pub bytecode: Vec<u8>,
    /// Map definitions from the program image.
    pub map_defs: Vec<MapDef>,
    /// SHA-256 hash of the CBOR program image.
    pub hash: Vec<u8>,
    /// Whether this is an ephemeral program (stored in RAM, run once).
    pub is_ephemeral: bool,
}

/// Manages A/B program partitions and program image lifecycle.
pub struct ProgramStore<'a, S: PlatformStorage> {
    storage: &'a mut S,
}

impl<'a, S: PlatformStorage> ProgramStore<'a, S> {
    pub fn new(storage: &'a mut S) -> Self {
        Self { storage }
    }

    /// Load the hash and raw bytes of the currently active resident program.
    ///
    /// Returns `(hash, raw_bytes)`.  The caller is responsible for decoding
    /// the CBOR image only when BPF execution is needed — this avoids
    /// unnecessary CPU/heap work in cycles that return early (Reboot,
    /// transport failures, transfer failures).
    ///
    /// Returns an empty hash and `None` bytes when no program is installed
    /// or when the active partition index is invalid (> 1).
    pub fn load_active_raw(&self, sha: &dyn Sha256Provider) -> (Vec<u8>, Option<Vec<u8>>) {
        let (_interval, active_partition) = self.storage.read_schedule();
        if active_partition > 1 {
            return (Vec::new(), None);
        }
        match self.storage.read_program(active_partition) {
            Some(image_bytes) => {
                let hash = sha.hash(&image_bytes).to_vec();
                (hash, Some(image_bytes))
            }
            None => (Vec::new(), None),
        }
    }

    /// Decode raw CBOR image bytes into a [`LoadedProgram`] for a **resident**
    /// program (sets `is_ephemeral: false`).
    ///
    /// Called in step 9 of the wake cycle when BPF execution is needed.
    /// Separated from [`load_active_raw`](Self::load_active_raw) so that
    /// decode is deferred until we know the program will actually execute.
    ///
    /// Returns `None` if CBOR decoding fails.
    pub(crate) fn decode_image(image_bytes: &[u8], hash: Vec<u8>) -> Option<LoadedProgram> {
        ProgramImage::decode(image_bytes)
            .ok()
            .map(|image| LoadedProgram {
                bytecode: image.bytecode,
                map_defs: image.maps,
                hash,
                is_ephemeral: false,
            })
    }

    /// Install a new resident program via chunked transfer.
    ///
    /// 1. Verify the SHA-256 hash against `expected_hash`.
    /// 2. Decode the CBOR program image.
    /// 3. Validate that map definitions fit within `map_budget`.
    /// 4. Write to the **inactive** partition.
    /// 5. Flip the active partition flag.
    ///
    /// Map budget is validated *before* the A/B swap so the old program
    /// remains active if the new program's maps don't fit.
    ///
    /// Returns the decoded program on success. On failure, the existing
    /// active program is untouched (A/B atomicity).
    pub fn install_resident(
        &mut self,
        image_bytes: &[u8],
        expected_hash: &[u8],
        sha: &(impl Sha256Provider + ?Sized),
        map_budget: usize,
    ) -> NodeResult<LoadedProgram> {
        // Verify hash
        let actual_hash = sha.hash(image_bytes);
        if actual_hash.as_slice() != expected_hash {
            return Err(NodeError::ProgramHashMismatch);
        }

        // Decode the CBOR program image
        let image = ProgramImage::decode(image_bytes)
            .map_err(|e| NodeError::ProgramDecodeFailed(format!("{}", e)))?;

        // Validate map definitions (type, key_size, overflow) and budget
        // before committing the A/B swap so a bad program never becomes active.
        crate::map_storage::MapStorage::validate_map_defs(&image.maps)?;
        let required = crate::map_storage::MapStorage::required_bytes(&image.maps);
        if required > map_budget {
            return Err(NodeError::MapBudgetExceeded {
                required,
                available: map_budget,
            });
        }

        // Write to the inactive partition
        let (_interval, active_partition) = self.storage.read_schedule();
        if active_partition > 1 {
            return Err(NodeError::StorageError(
                "invalid active partition index".into(),
            ));
        }
        let inactive_partition = 1 - active_partition;
        self.storage
            .write_program(inactive_partition, image_bytes)?;

        // Re-read the written program and verify its hash to detect
        // flash write corruption or partial writes before committing
        // the A/B swap.
        let written_bytes = self
            .storage
            .read_program(inactive_partition)
            .ok_or_else(|| NodeError::StorageError("failed to re-read written program".into()))?;
        let written_hash = sha.hash(&written_bytes);
        if written_hash.as_slice() != expected_hash {
            return Err(NodeError::ProgramHashMismatch);
        }

        // Flip active partition
        self.storage.write_active_partition(inactive_partition)?;

        Ok(LoadedProgram {
            bytecode: image.bytecode,
            map_defs: image.maps,
            hash: actual_hash.to_vec(),
            is_ephemeral: false,
        })
    }

    /// Load an ephemeral program (stored in RAM, not flash).
    ///
    /// Verifies the hash, decodes the image, and validates map definitions
    /// (type, key_size, budget). Does not write to flash or change the
    /// active partition. Validation happens before the caller sends
    /// `PROGRAM_ACK`, so an unrunnable ephemeral program is never ACK'd.
    pub fn load_ephemeral(
        &self,
        image_bytes: &[u8],
        expected_hash: &[u8],
        sha: &(impl Sha256Provider + ?Sized),
    ) -> NodeResult<LoadedProgram> {
        let actual_hash = sha.hash(image_bytes);
        if actual_hash.as_slice() != expected_hash {
            return Err(NodeError::ProgramHashMismatch);
        }

        let image = ProgramImage::decode(image_bytes)
            .map_err(|e| NodeError::ProgramDecodeFailed(format!("{}", e)))?;

        // Validate map definitions before returning Ok, so the caller
        // won't send PROGRAM_ACK for an unrunnable program.
        //
        // Ephemeral programs must not declare maps (ND-0503: "resident
        // program is unaffected by ephemeral execution"). Re-allocating
        // maps would destroy the resident program's sleep-persistent state.
        if !image.maps.is_empty() {
            return Err(NodeError::ProgramDecodeFailed(
                "ephemeral programs must not declare maps".into(),
            ));
        }

        Ok(LoadedProgram {
            bytecode: image.bytecode,
            map_defs: image.maps,
            hash: actual_hash.to_vec(),
            is_ephemeral: true,
        })
    }
}

// NOTE: `resolve_map_references` was removed in the sonde-bpf migration.
// LDDW `src=1` map reference relocation is now handled at runtime by the
// `sonde_bpf` interpreter backend.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NodeError;

    struct TestSha256;
    impl Sha256Provider for TestSha256 {
        fn hash(&self, data: &[u8]) -> [u8; 32] {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(data);
            hasher.finalize().into()
        }
    }

    /// Local mock storage for program_store tests.
    struct MockStorage {
        schedule_interval: u32,
        active_partition: u8,
        programs: [Option<Vec<u8>>; 2],
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                schedule_interval: 60,
                active_partition: 0,
                programs: [None, None],
            }
        }
    }

    impl PlatformStorage for MockStorage {
        fn read_key(&self) -> Option<(u16, [u8; 32])> {
            None
        }
        fn write_key(&mut self, _kh: u16, _psk: &[u8; 32]) -> NodeResult<()> {
            Ok(())
        }
        fn erase_key(&mut self) -> NodeResult<()> {
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
            false
        }
        fn set_early_wake_flag(&mut self) -> NodeResult<()> {
            Ok(())
        }
    }

    fn make_test_image(bytecode: &[u8], maps: &[MapDef]) -> (Vec<u8>, Vec<u8>) {
        let image = ProgramImage {
            bytecode: bytecode.to_vec(),
            maps: maps.to_vec(),
        };
        let cbor = image.encode_deterministic().unwrap();
        let hash = TestSha256.hash(&cbor).to_vec();
        (cbor, hash)
    }

    #[test]
    fn test_load_ephemeral_valid() {
        let (cbor, hash) = make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let mut storage = MockStorage::new();
        let store = ProgramStore::new(&mut storage);
        let loaded = store.load_ephemeral(&cbor, &hash, &TestSha256).unwrap();
        assert!(loaded.is_ephemeral);
        assert_eq!(loaded.hash, hash);
    }

    #[test]
    fn test_load_ephemeral_hash_mismatch() {
        let (cbor, _hash) = make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let wrong_hash = vec![0xFF; 32];
        let mut storage = MockStorage::new();
        let store = ProgramStore::new(&mut storage);
        let result = store.load_ephemeral(&cbor, &wrong_hash, &TestSha256);
        assert!(matches!(result, Err(NodeError::ProgramHashMismatch)));
    }

    #[test]
    fn test_install_resident_ab_swap() {
        let (cbor, hash) = make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let mut storage = MockStorage::new();
        // Active partition is 0, so install should write to partition 1
        {
            let mut store = ProgramStore::new(&mut storage);
            let loaded = store
                .install_resident(&cbor, &hash, &TestSha256, 4096)
                .unwrap();
            assert!(!loaded.is_ephemeral);
        }
        // Active partition should now be 1
        assert_eq!(storage.read_schedule().1, 1);
        assert!(storage.read_program(1).is_some());
    }

    #[test]
    fn test_install_resident_invalid_active_partition() {
        let (cbor, hash) = make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let mut storage = MockStorage::new();
        storage.active_partition = 5; // invalid
        let mut store = ProgramStore::new(&mut storage);
        let result = store.install_resident(&cbor, &hash, &TestSha256, 4096);
        assert!(matches!(result, Err(NodeError::StorageError(_))));
    }

    // ---- load_active_raw tests ----

    #[test]
    fn test_load_active_raw_no_program() {
        let mut storage = MockStorage::new();
        let store = ProgramStore::new(&mut storage);
        let (hash, bytes) = store.load_active_raw(&TestSha256);
        assert!(hash.is_empty());
        assert!(bytes.is_none());
    }

    #[test]
    fn test_load_active_raw_invalid_partition() {
        let mut storage = MockStorage::new();
        storage.active_partition = 5;
        let store = ProgramStore::new(&mut storage);
        let (hash, bytes) = store.load_active_raw(&TestSha256);
        assert!(hash.is_empty());
        assert!(bytes.is_none());
    }

    #[test]
    fn test_load_active_raw_valid_program() {
        let (cbor, expected_hash) =
            make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let mut storage = MockStorage::new();
        storage.programs[0] = Some(cbor.clone());
        let store = ProgramStore::new(&mut storage);
        let (hash, bytes) = store.load_active_raw(&TestSha256);
        assert_eq!(hash, expected_hash);
        assert_eq!(bytes.unwrap(), cbor);
    }

    // ---- decode_image tests ----

    #[test]
    fn test_decode_image_valid() {
        let (cbor, hash) = make_test_image(&[0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], &[]);
        let loaded = ProgramStore::<MockStorage>::decode_image(&cbor, hash.clone()).unwrap();
        assert_eq!(loaded.hash, hash);
        assert!(!loaded.is_ephemeral);
        assert_eq!(
            loaded.bytecode,
            vec![0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn test_decode_image_invalid_cbor() {
        let bad_bytes = vec![0xFF, 0xFE, 0xFD];
        let hash = vec![0x42; 32];
        let result = ProgramStore::<MockStorage>::decode_image(&bad_bytes, hash);
        assert!(result.is_none());
    }
}
