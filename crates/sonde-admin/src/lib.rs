// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use chrono::{DateTime, Utc};

pub mod pb {
    tonic::include_proto!("sonde.admin");
}

pub mod grpc_client;

/// Format a millisecond Unix epoch timestamp as a human-readable UTC date.
/// Returns `"<invalid timestamp: {ms}>"` for out-of-range values so that CLI
/// output never falls back to raw milliseconds (per GW-0806 criterion #3).
pub fn format_epoch_ms(ms: u64) -> String {
    let Ok(ms_i64) = i64::try_from(ms) else {
        return format!("<invalid timestamp: {ms}>");
    };

    DateTime::<Utc>::from_timestamp_millis(ms_i64)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("<invalid timestamp: {ms}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_known_timestamp() {
        // 2026-03-28 04:03:15 UTC = 1774670595000 ms
        assert_eq!(
            format_epoch_ms(1_774_670_595_000),
            "2026-03-28 04:03:15 UTC"
        );
    }

    #[test]
    fn test_format_epoch_zero() {
        assert_eq!(format_epoch_ms(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn test_format_out_of_range() {
        // u64::MAX cannot fit in i64, should show invalid marker
        assert_eq!(
            format_epoch_ms(u64::MAX),
            format!("<invalid timestamp: {}>", u64::MAX)
        );
    }
}
