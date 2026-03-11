// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Peer table with auto-registration and LRU eviction.
//!
//! ESP-NOW supports ~20 peers. This module manages them transparently,
//! adding peers on first send and evicting the least-recently-used when full.

use sonde_protocol::modem::MAC_SIZE;

const MAX_PEERS: usize = 20;

#[derive(Clone)]
struct PeerEntry {
    mac: [u8; MAC_SIZE],
    last_used: u32,
}

/// Fixed-capacity peer table with LRU eviction.
pub struct PeerTable {
    entries: Vec<PeerEntry>,
    tick: u32,
}

impl Default for PeerTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(MAX_PEERS),
            tick: 0,
        }
    }

    /// Ensure the given MAC is in the peer table.
    ///
    /// If already present, updates `last_used` and returns `None`.
    /// If not present and the table is not full, inserts and returns `None`.
    /// If not present and the table is full, evicts the LRU peer,
    /// inserts the new one, and returns `Some(evicted_mac)`.
    pub fn ensure_peer(&mut self, mac: &[u8; MAC_SIZE]) -> Option<[u8; MAC_SIZE]> {
        self.tick = self.tick.wrapping_add(1);

        // Check if already present.
        for entry in &mut self.entries {
            if entry.mac == *mac {
                entry.last_used = self.tick;
                return None;
            }
        }

        // Not present — need to insert.
        let evicted = if self.entries.len() >= MAX_PEERS {
            // Find the LRU entry.
            let lru_idx = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i)
                .unwrap();
            let evicted_mac = self.entries[lru_idx].mac;
            self.entries[lru_idx] = PeerEntry {
                mac: *mac,
                last_used: self.tick,
            };
            Some(evicted_mac)
        } else {
            self.entries.push(PeerEntry {
                mac: *mac,
                last_used: self.tick,
            });
            None
        };

        evicted
    }

    /// Remove all peers from the table.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.tick = 0;
    }

    /// Number of peers currently in the table.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns true if the given MAC is in the table.
    pub fn contains(&self, mac: &[u8; MAC_SIZE]) -> bool {
        self.entries.iter().any(|e| e.mac == *mac)
    }

    /// Returns all MAC addresses currently in the table.
    pub fn all_macs(&self) -> Vec<[u8; MAC_SIZE]> {
        self.entries.iter().map(|e| e.mac).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_find() {
        let mut table = PeerTable::new();
        let mac = [1, 2, 3, 4, 5, 6];
        assert_eq!(table.ensure_peer(&mac), None);
        assert!(table.contains(&mac));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn duplicate_insert_no_eviction() {
        let mut table = PeerTable::new();
        let mac = [1, 2, 3, 4, 5, 6];
        table.ensure_peer(&mac);
        table.ensure_peer(&mac);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn lru_eviction() {
        let mut table = PeerTable::new();

        // Fill the table.
        for i in 0..MAX_PEERS {
            let mac = [i as u8, 0, 0, 0, 0, 0];
            assert_eq!(table.ensure_peer(&mac), None);
        }
        assert_eq!(table.len(), MAX_PEERS);

        // Insert one more — should evict the first (LRU).
        let new_mac = [0xFF, 0, 0, 0, 0, 0];
        let evicted = table.ensure_peer(&new_mac);
        assert_eq!(evicted, Some([0, 0, 0, 0, 0, 0]));
        assert!(table.contains(&new_mac));
        assert!(!table.contains(&[0, 0, 0, 0, 0, 0]));
        assert_eq!(table.len(), MAX_PEERS);
    }

    #[test]
    fn lru_respects_access_order() {
        let mut table = PeerTable::new();

        // Fill the table.
        for i in 0..MAX_PEERS {
            let mac = [i as u8, 0, 0, 0, 0, 0];
            table.ensure_peer(&mac);
        }

        // Touch the first entry to make it recently used.
        table.ensure_peer(&[0, 0, 0, 0, 0, 0]);

        // Insert a new peer — should evict the second entry (now LRU).
        let new_mac = [0xFE, 0, 0, 0, 0, 0];
        let evicted = table.ensure_peer(&new_mac);
        assert_eq!(evicted, Some([1, 0, 0, 0, 0, 0]));
        assert!(table.contains(&[0, 0, 0, 0, 0, 0])); // first still there
    }

    #[test]
    fn clear_empties_table() {
        let mut table = PeerTable::new();
        for i in 0..5 {
            table.ensure_peer(&[i, 0, 0, 0, 0, 0]);
        }
        table.clear();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn evicted_peer_can_be_readded() {
        let mut table = PeerTable::new();
        for i in 0..MAX_PEERS {
            table.ensure_peer(&[i as u8, 0, 0, 0, 0, 0]);
        }

        // Evict peer 0 by adding a new one.
        table.ensure_peer(&[0xFF, 0, 0, 0, 0, 0]);
        assert!(!table.contains(&[0, 0, 0, 0, 0, 0]));

        // Re-add peer 0 — should evict peer 1 (now LRU).
        let evicted = table.ensure_peer(&[0, 0, 0, 0, 0, 0]);
        assert_eq!(evicted, Some([1, 0, 0, 0, 0, 0]));
        assert!(table.contains(&[0, 0, 0, 0, 0, 0]));
    }
}
