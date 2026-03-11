// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use crate::error::{NodeError, NodeResult};
use crate::traits::PlatformStorage;
use sonde_protocol::{MapDef, ProgramImage, Sha256Provider};

/// Loaded program ready for execution.
#[derive(Debug, Clone)]
pub struct LoadedProgram {
    /// Raw bytecode with LDDW src=1 instructions still present.
    /// The BPF interpreter resolves them at load time using `map_pointers`.
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

    /// Load the currently active resident program from flash.
    /// Returns `None` if no program is installed.
    pub fn load_active(&self, sha: &impl Sha256Provider) -> Option<LoadedProgram> {
        let (_interval, active_partition) = self.storage.read_schedule();
        let image_bytes = self.storage.read_program(active_partition)?;
        let hash = sha.hash(&image_bytes).to_vec();
        let image = ProgramImage::decode(&image_bytes).ok()?;
        Some(LoadedProgram {
            bytecode: image.bytecode,
            map_defs: image.maps,
            hash,
            is_ephemeral: false,
        })
    }

    /// Get the hash of the currently active resident program, or an empty
    /// vec if no program is installed.
    pub fn active_program_hash(&self, sha: &impl Sha256Provider) -> Vec<u8> {
        let (_interval, active_partition) = self.storage.read_schedule();
        match self.storage.read_program(active_partition) {
            Some(image_bytes) => sha.hash(&image_bytes).to_vec(),
            None => Vec::new(),
        }
    }

    /// Install a new resident program via chunked transfer.
    ///
    /// 1. Write to the **inactive** partition.
    /// 2. Verify the SHA-256 hash against `expected_hash`.
    /// 3. Flip the active partition flag.
    ///
    /// Returns the decoded program on success. On failure, the existing
    /// active program is untouched (A/B atomicity).
    pub fn install_resident(
        &mut self,
        image_bytes: &[u8],
        expected_hash: &[u8],
        sha: &impl Sha256Provider,
    ) -> NodeResult<LoadedProgram> {
        // Verify hash
        let actual_hash = sha.hash(image_bytes);
        if actual_hash.as_slice() != expected_hash {
            return Err(NodeError::ProgramHashMismatch);
        }

        // Decode the CBOR program image
        let image = ProgramImage::decode(image_bytes)
            .map_err(|e| NodeError::ProgramDecodeFailed(format!("{}", e)))?;

        // Write to the inactive partition
        let (_interval, active_partition) = self.storage.read_schedule();
        let inactive_partition = 1 - active_partition;
        self.storage
            .write_program(inactive_partition, image_bytes)?;

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
    /// Verifies the hash and decodes the image, but does not write
    /// to flash or change the active partition.
    pub fn load_ephemeral(
        &self,
        image_bytes: &[u8],
        expected_hash: &[u8],
        sha: &impl Sha256Provider,
    ) -> NodeResult<LoadedProgram> {
        let actual_hash = sha.hash(image_bytes);
        if actual_hash.as_slice() != expected_hash {
            return Err(NodeError::ProgramHashMismatch);
        }

        let image = ProgramImage::decode(image_bytes)
            .map_err(|e| NodeError::ProgramDecodeFailed(format!("{}", e)))?;

        Ok(LoadedProgram {
            bytecode: image.bytecode,
            map_defs: image.maps,
            hash: actual_hash.to_vec(),
            is_ephemeral: true,
        })
    }
}

/// Resolve LDDW src=1 map references in bytecode.
///
/// BPF `LDDW` instructions are 16 bytes (two 8-byte slots). When `src=1`,
/// the `imm` field (bytes 4..8 of the first slot) contains a map index.
/// This function replaces the immediate with the runtime pointer to the
/// map's storage, split across the two 8-byte slots:
///   slot 0 imm (bytes 4..8) = lower 32 bits of pointer
///   slot 1 imm (bytes 4..8) = upper 32 bits of pointer
pub fn resolve_map_references(bytecode: &mut [u8], map_pointers: &[u64]) -> NodeResult<()> {
    if !bytecode.len().is_multiple_of(8) {
        return Err(NodeError::ProgramDecodeFailed(
            "bytecode length not a multiple of 8".into(),
        ));
    }

    let mut i = 0;
    while i + 16 <= bytecode.len() {
        let opcode = bytecode[i];
        let src_reg = (bytecode[i + 1] >> 4) & 0x0F;

        // LDDW opcode = 0x18, src=1 means map reference
        if opcode == 0x18 && src_reg == 1 {
            let map_index = u32::from_le_bytes([
                bytecode[i + 4],
                bytecode[i + 5],
                bytecode[i + 6],
                bytecode[i + 7],
            ]) as usize;

            if map_index >= map_pointers.len() {
                return Err(NodeError::ProgramDecodeFailed(format!(
                    "LDDW references map index {} but only {} maps defined",
                    map_index,
                    map_pointers.len()
                )));
            }

            let ptr = map_pointers[map_index];
            let lo = (ptr & 0xFFFF_FFFF) as u32;
            let hi = ((ptr >> 32) & 0xFFFF_FFFF) as u32;

            // Clear the src field (set src=0 after relocation)
            bytecode[i + 1] &= 0x0F;

            // Write lower 32 bits into slot 0 imm
            bytecode[i + 4..i + 8].copy_from_slice(&lo.to_le_bytes());
            // Write upper 32 bits into slot 1 imm
            bytecode[i + 12..i + 16].copy_from_slice(&hi.to_le_bytes());

            i += 16; // Skip both slots of the LDDW
        } else if opcode == 0x18 {
            i += 16; // LDDW with src!=1, skip both slots
        } else {
            i += 8; // Normal instruction
        }
    }

    Ok(())
}

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
        fn take_program_updated_flag(&mut self) -> bool {
            false
        }
        fn set_program_updated_flag(&mut self) -> NodeResult<()> {
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
            let loaded = store.install_resident(&cbor, &hash, &TestSha256).unwrap();
            assert!(!loaded.is_ephemeral);
        }
        // Active partition should now be 1
        assert_eq!(storage.read_schedule().1, 1);
        assert!(storage.read_program(1).is_some());
    }

    #[test]
    fn test_resolve_map_references() {
        // Build a minimal LDDW src=1, imm=0 instruction (16 bytes)
        let mut bytecode = vec![0u8; 16];
        bytecode[0] = 0x18; // LDDW opcode
        bytecode[1] = 0x10; // src=1, dst=0
                            // imm = 0 (map index 0)
        bytecode[4..8].copy_from_slice(&0u32.to_le_bytes());

        let map_pointers = vec![0xDEAD_BEEF_CAFE_BABEu64];
        resolve_map_references(&mut bytecode, &map_pointers).unwrap();

        // Verify src was cleared to 0
        assert_eq!((bytecode[1] >> 4) & 0x0F, 0);
        // Verify lower 32 bits
        let lo = u32::from_le_bytes([bytecode[4], bytecode[5], bytecode[6], bytecode[7]]);
        assert_eq!(lo, 0xCAFE_BABE);
        // Verify upper 32 bits
        let hi = u32::from_le_bytes([bytecode[12], bytecode[13], bytecode[14], bytecode[15]]);
        assert_eq!(hi, 0xDEAD_BEEF);
    }

    #[test]
    fn test_resolve_map_references_out_of_bounds() {
        let mut bytecode = vec![0u8; 16];
        bytecode[0] = 0x18;
        bytecode[1] = 0x10; // src=1
        bytecode[4..8].copy_from_slice(&5u32.to_le_bytes()); // map index 5

        let map_pointers = vec![0x1234u64]; // only 1 map
        let result = resolve_map_references(&mut bytecode, &map_pointers);
        assert!(matches!(result, Err(NodeError::ProgramDecodeFailed(_))));
    }
}
