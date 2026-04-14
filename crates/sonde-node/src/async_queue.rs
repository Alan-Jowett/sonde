// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Sleep-retained async send queue for store-and-forward.
//!
//! BPF programs call `send_async` (helper #17) to enqueue data blobs
//! that are transmitted after BPF execution completes — either
//! piggybacked on the next WAKE or sent as individual APP_DATA frames.
//!
//! On ESP32, the queue is backed by RTC slow SRAM (`.rtc.data`) and
//! survives deep sleep (ND-0609). On host/test builds, a heap-allocated
//! buffer provides the same fixed-size layout for unit testing.

/// Maximum number of queued messages per wake cycle.
const MAX_MESSAGES: usize = 10;

/// Maximum size of a single blob (mirrors [`sonde_protocol::MAX_APP_DATA_BLOB_SIZE`]).
const MAX_BLOB_SIZE: usize = sonde_protocol::MAX_APP_DATA_BLOB_SIZE;

/// Magic value to validate that the RTC region was initialized by this
/// firmware version. ASCII "QUEU".
const QUEUE_MAGIC: u32 = 0x5155_4555;

// ---------------------------------------------------------------------------
// Fixed-size RTC layout
// ---------------------------------------------------------------------------

/// A single slot in the RTC queue layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct RtcQueueItem {
    len: u32,
    data: [u8; MAX_BLOB_SIZE],
}

/// Fixed-size layout stored in RTC slow SRAM (ESP) or on the heap (tests).
///
/// Uses a magic/count commit pattern identical to
/// [`MapStorage::write_rtc_layout`](crate::map_storage) — `count` is
/// written last so that a reset mid-write never leaves [`from_rtc()`]
/// with a valid-looking but inconsistent record.
#[repr(C)]
struct RtcQueueLayout {
    magic: u32,
    count: u32,
    items: [RtcQueueItem; MAX_MESSAGES],
}

impl RtcQueueLayout {
    const fn zero() -> Self {
        Self {
            magic: 0,
            count: 0,
            items: [RtcQueueItem {
                len: 0,
                data: [0u8; MAX_BLOB_SIZE],
            }; MAX_MESSAGES],
        }
    }
}

#[cfg(feature = "esp")]
#[link_section = ".rtc.data"]
static mut QUEUE_LAYOUT: RtcQueueLayout = RtcQueueLayout::zero();

// ---------------------------------------------------------------------------
// AsyncQueue
// ---------------------------------------------------------------------------

/// Sleep-retained queue of data blobs destined for the gateway.
///
/// **Persistence contract (ND-0609):** On ESP32, the queue is backed by
/// a static in RTC slow SRAM (`.rtc.data`). Data survives deep sleep
/// so that blobs queued in cycle N are available for piggybacking in
/// cycle N+1's WAKE. Data is lost on reboot (RTC SRAM is cleared on
/// power-on reset).
///
/// On host/test builds, the queue is backed by a heap-allocated
/// [`RtcQueueLayout`], which is sufficient for unit testing.
pub struct AsyncQueue {
    #[cfg(not(feature = "esp"))]
    backing: Box<RtcQueueLayout>,
}

impl AsyncQueue {
    /// Create a fresh empty queue.
    ///
    /// On ESP: writes the magic value and sets count to 0 in RTC SRAM.
    /// On host: allocates a zeroed layout on the heap and sets the magic.
    pub fn new() -> Self {
        #[cfg(not(feature = "esp"))]
        {
            let mut backing = Box::new(RtcQueueLayout::zero());
            backing.magic = QUEUE_MAGIC;
            Self { backing }
        }
        #[cfg(feature = "esp")]
        {
            use core::sync::atomic::{fence, Ordering};
            unsafe {
                // Invalidate first so from_rtc() returns None on partial init.
                core::ptr::write_volatile(&raw mut QUEUE_LAYOUT.count, 0);
                fence(Ordering::SeqCst);
                core::ptr::write_volatile(&raw mut QUEUE_LAYOUT.magic, QUEUE_MAGIC);
                fence(Ordering::SeqCst);
                // count stays 0 — empty queue
            }
            Self {}
        }
    }

    /// Recover the queue from RTC slow SRAM after deep sleep.
    ///
    /// On ESP: validates the magic and count in the RTC layout. Returns
    /// a queue reflecting the persisted state if valid, or a fresh empty
    /// queue on cold boot or corruption.
    ///
    /// On host: always returns a fresh empty queue (no RTC SRAM to
    /// recover from).
    pub fn from_rtc() -> Self {
        #[cfg(feature = "esp")]
        {
            let magic = unsafe { core::ptr::read_volatile(&raw const QUEUE_LAYOUT.magic) };
            if magic != QUEUE_MAGIC {
                return Self::new();
            }
            let count = unsafe { core::ptr::read_volatile(&raw const QUEUE_LAYOUT.count) } as usize;
            if count > MAX_MESSAGES {
                return Self::new();
            }
            // Validate item lengths to catch corruption.
            for i in 0..count {
                let len = unsafe { core::ptr::read_volatile(&raw const QUEUE_LAYOUT.items[i].len) }
                    as usize;
                if len > MAX_BLOB_SIZE {
                    return Self::new();
                }
            }
            Self {}
        }
        #[cfg(not(feature = "esp"))]
        {
            Self::new()
        }
    }

    // ------- Low-level accessors -------

    fn read_count(&self) -> usize {
        #[cfg(feature = "esp")]
        {
            unsafe { core::ptr::read_volatile(&raw const QUEUE_LAYOUT.count) as usize }
        }
        #[cfg(not(feature = "esp"))]
        {
            self.backing.count as usize
        }
    }

    fn read_item_len(&self, index: usize) -> usize {
        #[cfg(feature = "esp")]
        {
            unsafe { core::ptr::read_volatile(&raw const QUEUE_LAYOUT.items[index].len) as usize }
        }
        #[cfg(not(feature = "esp"))]
        {
            self.backing.items[index].len as usize
        }
    }

    /// Return a reference to the data in slot `index`.
    ///
    /// The caller must ensure `index < read_count()` and that the item
    /// length has been validated (≤ [`MAX_BLOB_SIZE`]).
    fn read_item_data(&self, index: usize) -> &[u8] {
        let len = self.read_item_len(index);
        #[cfg(feature = "esp")]
        {
            unsafe {
                let data_ptr = &raw const QUEUE_LAYOUT.items[index].data as *const u8;
                core::slice::from_raw_parts(data_ptr, len)
            }
        }
        #[cfg(not(feature = "esp"))]
        {
            &self.backing.items[index].data[..len]
        }
    }

    /// Write a blob into slot `index`.
    fn write_item(&mut self, index: usize, data: &[u8]) {
        #[cfg(feature = "esp")]
        {
            unsafe {
                let data_dst = &raw mut QUEUE_LAYOUT.items[index].data as *mut u8;
                core::ptr::copy_nonoverlapping(data.as_ptr(), data_dst, data.len());
                core::ptr::write_volatile(
                    &raw mut QUEUE_LAYOUT.items[index].len,
                    data.len() as u32,
                );
            }
        }
        #[cfg(not(feature = "esp"))]
        {
            self.backing.items[index].data[..data.len()].copy_from_slice(data);
            self.backing.items[index].len = data.len() as u32;
        }
    }

    /// Commit `count` to the RTC layout.
    ///
    /// On ESP, a fence ensures all preceding item writes are visible
    /// before the count is updated (matching the invalidate-write-commit
    /// pattern in [`MapStorage::write_rtc_layout`](crate::map_storage)).
    fn commit_count(&mut self, count: usize) {
        #[cfg(feature = "esp")]
        {
            use core::sync::atomic::{fence, Ordering};
            unsafe {
                fence(Ordering::SeqCst);
                core::ptr::write_volatile(&raw mut QUEUE_LAYOUT.count, count as u32);
            }
        }
        #[cfg(not(feature = "esp"))]
        {
            self.backing.count = count as u32;
        }
    }

    // ------- Public API -------

    /// Enqueue a blob for deferred transmission.
    ///
    /// Returns `0` on success, `-1` if the queue is full, or `-2` if
    /// the blob exceeds the APP_DATA payload budget.
    pub fn push(&mut self, blob: Vec<u8>) -> i64 {
        if blob.len() > MAX_BLOB_SIZE {
            return -2;
        }
        let count = self.read_count();
        if count >= MAX_MESSAGES {
            return -1;
        }
        self.write_item(count, &blob);
        self.commit_count(count + 1);
        0
    }

    /// Drain all queued messages, returning them and leaving the queue empty.
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        let count = self.read_count();
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            result.push(self.read_item_data(i).to_vec());
        }
        self.commit_count(0);
        result
    }

    pub fn is_empty(&self) -> bool {
        self.read_count() == 0
    }

    pub fn len(&self) -> usize {
        self.read_count()
    }

    /// If exactly one message is queued and it fits within `wake_budget`
    /// bytes, return a reference to it for WAKE piggybacking.
    pub fn single_for_piggyback(&self, wake_budget: usize) -> Option<&[u8]> {
        if self.read_count() == 1 {
            let len = self.read_item_len(0);
            if len <= wake_budget {
                return Some(self.read_item_data(0));
            }
        }
        None
    }

    /// Clear the queue without returning messages.
    pub fn clear(&mut self) {
        self.commit_count(0);
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
        let big = vec![0x42u8; sonde_protocol::MAX_APP_DATA_BLOB_SIZE + 1];
        assert_eq!(q.push(big), -2);
        assert!(q.is_empty());
    }

    #[test]
    fn max_size_blob_accepted() {
        let mut q = AsyncQueue::new();
        let blob = vec![0x42u8; sonde_protocol::MAX_APP_DATA_BLOB_SIZE];
        assert_eq!(q.push(blob), 0);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn single_for_piggyback_one_fits() {
        let mut q = AsyncQueue::new();
        let _ = q.push(vec![1, 2, 3]);
        assert!(q.single_for_piggyback(100).is_some());
        assert_eq!(q.single_for_piggyback(100).unwrap(), &[1, 2, 3]);
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

    #[test]
    fn from_rtc_returns_empty_on_host() {
        let q = AsyncQueue::from_rtc();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn clear_empties_queue() {
        let mut q = AsyncQueue::new();
        assert_eq!(q.push(vec![0x42, 0x43]), 0);
        assert_eq!(q.push(vec![0x44, 0x45]), 0);
        assert_eq!(q.len(), 2);

        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn drain_preserves_data_fidelity() {
        let mut q = AsyncQueue::new();
        let blob1 = vec![0x42u8; 100];
        let blob2 = vec![0x43u8; 50];
        assert_eq!(q.push(blob1.clone()), 0);
        assert_eq!(q.push(blob2.clone()), 0);

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0], blob1);
        assert_eq!(drained[1], blob2);
        assert!(q.is_empty());
    }

    #[test]
    fn layout_size_within_budget() {
        // RtcQueueItem may include alignment padding from the u32 len field.
        let item_size = core::mem::size_of::<RtcQueueItem>();
        let size = core::mem::size_of::<RtcQueueLayout>();
        assert_eq!(size, 8 + MAX_MESSAGES * item_size);
        assert!(size <= 2300, "queue layout exceeds 2.3 KB: {size}");
    }

    #[test]
    fn push_after_clear_reuses_slots() {
        let mut q = AsyncQueue::new();
        for _ in 0..MAX_MESSAGES {
            assert_eq!(q.push(vec![0x42]), 0);
        }
        assert_eq!(q.push(vec![0x42]), -1);

        q.clear();
        assert_eq!(q.push(vec![0x43]), 0);
        assert_eq!(q.len(), 1);

        let drained = q.drain();
        assert_eq!(drained[0], vec![0x43]);
    }
}
