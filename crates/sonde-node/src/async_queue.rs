// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! RAM-only async send queue for store-and-forward.
//!
//! BPF programs call `send_async` (helper #17) to enqueue data blobs
//! that are transmitted after BPF execution completes — either
//! piggybacked on the next WAKE or sent as individual APP_DATA frames.
//! The queue does not survive deep sleep (RAM-only).

/// Maximum number of queued messages per wake cycle.
const MAX_MESSAGES: usize = 10;

/// RAM-only queue of data blobs destined for the gateway.
pub struct AsyncQueue {
    messages: Vec<Vec<u8>>,
}

impl AsyncQueue {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Enqueue a blob for deferred transmission.
    ///
    /// Returns `0` on success, `-1` if the queue is full, or `-2` if
    /// the blob exceeds the APP_DATA payload budget.
    pub fn push(&mut self, blob: Vec<u8>) -> i64 {
        if self.messages.len() >= MAX_MESSAGES {
            return -1;
        }
        if blob.len() > sonde_protocol::MAX_PAYLOAD_SIZE {
            return -2;
        }
        self.messages.push(blob);
        0
    }

    /// Drain all queued messages, returning them and leaving the queue empty.
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.messages)
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// If exactly one message is queued and it fits within `wake_budget`
    /// bytes, return a reference to it for WAKE piggybacking.
    pub fn single_for_piggyback(&self, wake_budget: usize) -> Option<&Vec<u8>> {
        if self.messages.len() == 1 && self.messages[0].len() <= wake_budget {
            Some(&self.messages[0])
        } else {
            None
        }
    }
}

impl Default for AsyncQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_drain() {
        let mut q = AsyncQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);

        assert_eq!(q.push(vec![1, 2, 3]), 0);
        assert!(!q.is_empty());
        assert_eq!(q.len(), 1);

        let msgs = q.drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], vec![1, 2, 3]);
        assert!(q.is_empty());
    }

    #[test]
    fn queue_full_returns_neg1() {
        let mut q = AsyncQueue::new();
        for i in 0..10 {
            assert_eq!(q.push(vec![i]), 0);
        }
        assert_eq!(q.push(vec![99]), -1);
        assert_eq!(q.len(), 10);
    }

    #[test]
    fn oversized_blob_returns_neg2() {
        let mut q = AsyncQueue::new();
        let big = vec![0u8; sonde_protocol::MAX_PAYLOAD_SIZE + 1];
        assert_eq!(q.push(big), -2);
        assert!(q.is_empty());
    }

    #[test]
    fn max_size_blob_accepted() {
        let mut q = AsyncQueue::new();
        let blob = vec![0x42u8; sonde_protocol::MAX_PAYLOAD_SIZE];
        assert_eq!(q.push(blob), 0);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn single_for_piggyback_one_fits() {
        let mut q = AsyncQueue::new();
        let _ = q.push(vec![1, 2, 3]);
        assert!(q.single_for_piggyback(100).is_some());
        assert_eq!(q.single_for_piggyback(100).unwrap(), &vec![1, 2, 3]);
    }

    #[test]
    fn single_for_piggyback_one_too_large() {
        let mut q = AsyncQueue::new();
        let _ = q.push(vec![1, 2, 3]);
        assert!(q.single_for_piggyback(2).is_none());
    }

    #[test]
    fn single_for_piggyback_multiple() {
        let mut q = AsyncQueue::new();
        let _ = q.push(vec![1]);
        let _ = q.push(vec![2]);
        assert!(q.single_for_piggyback(100).is_none());
    }

    #[test]
    fn single_for_piggyback_empty() {
        let q = AsyncQueue::new();
        assert!(q.single_for_piggyback(100).is_none());
    }
}
