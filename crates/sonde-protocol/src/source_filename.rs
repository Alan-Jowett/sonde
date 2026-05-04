// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use alloc::string::{String, ToString};

/// Normalize an optional operator-supplied filename for storage/display.
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
        return None;
    }

    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let basename = &trimmed[2..];
        if basename.is_empty() {
            return None;
        }
        return Some(basename.to_string());
    }

    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_components() {
        assert_eq!(
            normalize_display_filename(&Some(r"C:\captures\temp-reader.o".to_string())).as_deref(),
            Some("temp-reader.o")
        );
        assert_eq!(
            normalize_display_filename(&Some("/tmp/temp-reader.o".to_string())).as_deref(),
            Some("temp-reader.o")
        );
        assert_eq!(
            normalize_display_filename(&Some("C:temp-reader.o".to_string())).as_deref(),
            Some("temp-reader.o")
        );
    }

    #[test]
    fn rejects_root_only_inputs() {
        assert!(normalize_display_filename(&Some(r"C:\".to_string())).is_none());
        assert!(normalize_display_filename(&Some("/".to_string())).is_none());
        assert!(normalize_display_filename(&Some(r"\\".to_string())).is_none());
    }
}
