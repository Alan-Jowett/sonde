// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Why the node woke up this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WakeReason {
    /// Normal scheduled wake.
    Scheduled = 0x00,
    /// Woke early due to a prior `set_next_wake()` call.
    Early = 0x01,
    /// New program was just installed. First execution.
    ProgramUpdate = 0x02,
}

/// Minimum sleep interval in seconds. Prevents a tight wake-sleep loop
/// that would drain the battery in hours if the gateway sets interval=0
/// or a BPF program calls set_next_wake(0).
pub const MIN_SLEEP_INTERVAL_S: u32 = 1;

/// Manages wake intervals and wake reason tracking.
///
/// The sleep manager tracks the base interval (set by `UPDATE_SCHEDULE`),
/// an optional one-shot early wake request (from `set_next_wake()`), and
/// the wake reason for the current cycle.
pub struct SleepManager {
    /// Base wake interval in seconds, persisted across deep sleep via
    /// the schedule partition.
    base_interval_s: u32,
    /// One-shot early wake request from the BPF program.
    /// Only applies to the next sleep; does NOT modify `base_interval_s`.
    next_wake_override_s: Option<u32>,
    /// Wake reason for the current cycle.
    wake_reason: WakeReason,
}

impl SleepManager {
    /// Create a new SleepManager.
    ///
    /// `base_interval_s` is loaded from the schedule partition.
    /// `wake_reason` is determined by checking RTC flags at boot.
    pub fn new(base_interval_s: u32, wake_reason: WakeReason) -> Self {
        Self {
            base_interval_s,
            next_wake_override_s: None,
            wake_reason,
        }
    }

    /// Get the wake reason for the current cycle.
    pub fn wake_reason(&self) -> WakeReason {
        self.wake_reason
    }

    /// Get the base interval in seconds.
    pub fn base_interval_s(&self) -> u32 {
        self.base_interval_s
    }

    /// Update the base interval (called when processing UPDATE_SCHEDULE).
    pub fn set_base_interval(&mut self, interval_s: u32) {
        self.base_interval_s = interval_s;
    }

    /// Request an earlier next wake (BPF `set_next_wake()` helper).
    ///
    /// The effective sleep duration is `min(requested, base_interval_s)`.
    /// This does NOT modify the base interval.
    /// Only the most recent call takes effect (last writer wins).
    pub fn set_next_wake(&mut self, seconds: u32) {
        self.next_wake_override_s = Some(seconds);
    }

    /// Compute the effective sleep duration in seconds.
    ///
    /// Returns `min(next_wake_override, base_interval_s)` if an override
    /// was requested, otherwise `base_interval_s`. The result is clamped
    /// to a minimum of 1 second to prevent a tight wake-sleep loop that
    /// would drain the battery.
    pub fn effective_sleep_s(&self) -> u32 {
        let raw = match self.next_wake_override_s {
            Some(override_s) => core::cmp::min(override_s, self.base_interval_s),
            None => self.base_interval_s,
        };
        raw.max(MIN_SLEEP_INTERVAL_S)
    }

    /// Returns true if `set_next_wake()` was called during this cycle
    /// and the override is less than the base interval (i.e., the node
    /// will actually wake early).
    pub fn will_wake_early(&self) -> bool {
        match self.next_wake_override_s {
            Some(override_s) => override_s < self.base_interval_s,
            None => false,
        }
    }

    /// Update the wake reason for the current cycle.
    ///
    /// Typically called after a successful program installation to set
    /// `WakeReason::ProgramUpdate`, which is observed immediately in the
    /// `SondeContext` passed to the BPF program executing in this cycle.
    pub fn set_wake_reason(&mut self, reason: WakeReason) {
        self.wake_reason = reason;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_sleep_uses_base_interval() {
        // T-N200: Default sleep uses the configured base interval.
        let sm = SleepManager::new(300, WakeReason::Scheduled);
        assert_eq!(sm.effective_sleep_s(), 300);
        assert!(!sm.will_wake_early());
    }

    #[test]
    fn test_set_next_wake_shorter() {
        // T-N208: set_next_wake with a shorter interval is accepted.
        let mut sm = SleepManager::new(300, WakeReason::Scheduled);
        sm.set_next_wake(10);
        assert_eq!(sm.effective_sleep_s(), 10);
        assert!(sm.will_wake_early());
    }

    #[test]
    fn test_set_next_wake_longer_clamped() {
        // T-N209: set_next_wake cannot extend beyond the configured base interval.
        let mut sm = SleepManager::new(60, WakeReason::Scheduled);
        sm.set_next_wake(600);
        // min(600, 60) = 60
        assert_eq!(sm.effective_sleep_s(), 60);
        assert!(!sm.will_wake_early());
    }

    #[test]
    fn test_set_next_wake_equal() {
        // T-N208: set_next_wake equal to the base interval is a no-op (will_wake_early stays false).
        let mut sm = SleepManager::new(60, WakeReason::Scheduled);
        sm.set_next_wake(60);
        assert_eq!(sm.effective_sleep_s(), 60);
        assert!(!sm.will_wake_early());
    }

    #[test]
    fn test_update_schedule() {
        // T-N205: set_base_interval updates the effective sleep duration.
        let mut sm = SleepManager::new(60, WakeReason::Scheduled);
        sm.set_base_interval(120);
        assert_eq!(sm.effective_sleep_s(), 120);
    }

    #[test]
    fn test_wake_reason() {
        // T-N508, T-N509: wake_reason is preserved from construction.
        let sm = SleepManager::new(60, WakeReason::Early);
        assert_eq!(sm.wake_reason(), WakeReason::Early);

        let sm2 = SleepManager::new(60, WakeReason::ProgramUpdate);
        assert_eq!(sm2.wake_reason(), WakeReason::ProgramUpdate);
    }

    #[test]
    fn test_zero_interval_clamped_to_minimum() {
        // Defensive clamp: a zero base interval is floored to 1 second
        // to prevent the node sleeping for 0 seconds on misconfiguration.
        // (Not directly described in the spec; this is an implementation invariant.)
        let sm = SleepManager::new(0, WakeReason::Scheduled);
        assert_eq!(sm.effective_sleep_s(), 1);
    }

    #[test]
    fn test_zero_next_wake_clamped_to_minimum() {
        // Defensive clamp: set_next_wake(0) is floored to 1 second.
        // (Not directly described in the spec; this is an implementation invariant.)
        let mut sm = SleepManager::new(300, WakeReason::Scheduled);
        sm.set_next_wake(0);
        assert_eq!(sm.effective_sleep_s(), 1);
    }
}
