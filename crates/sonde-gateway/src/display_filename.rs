// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Normalize an optional operator-supplied filename for human-facing display.
///
/// Strips any path components and trailing separators so callers can safely
/// store/render only the basename, never a full path.
pub fn normalize_display_filename(source_filename: &Option<String>) -> Option<String> {
    let trimmed = source_filename
        .as_deref()?
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()?;
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
