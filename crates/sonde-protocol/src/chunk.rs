// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

/// Calculate the number of chunks needed to transfer an image.
/// Returns `None` if `chunk_size` is 0 (invalid).
/// Returns `Some(0)` if `image_size` is 0.
pub fn chunk_count(image_size: usize, chunk_size: usize) -> Option<u32> {
    if chunk_size == 0 {
        return None;
    }
    if image_size == 0 {
        return Some(0);
    }
    Some(image_size.div_ceil(chunk_size) as u32)
}

/// Get the bytes for a specific chunk from a program image.
/// Returns `None` if `chunk_index` is out of range or `chunk_size` is 0.
pub fn get_chunk(image: &[u8], chunk_index: u32, chunk_size: u32) -> Option<&[u8]> {
    if chunk_size == 0 {
        return None;
    }
    let start = (chunk_index as usize) * (chunk_size as usize);
    if start >= image.len() {
        return None;
    }
    let end = core::cmp::min(start + chunk_size as usize, image.len());
    Some(&image[start..end])
}
