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
    let count = image_size.div_ceil(chunk_size);
    u32::try_from(count).ok()
}

/// Get the bytes for a specific chunk from a program image.
/// Returns `None` if `chunk_index` is out of range, `chunk_size` is 0, or arithmetic overflows.
pub fn get_chunk(image: &[u8], chunk_index: u32, chunk_size: u32) -> Option<&[u8]> {
    if chunk_size == 0 {
        return None;
    }
    let start = (chunk_index as usize).checked_mul(chunk_size as usize)?;
    if start >= image.len() {
        return None;
    }
    let end_unclamped = start.checked_add(chunk_size as usize)?;
    let end = core::cmp::min(end_unclamped, image.len());
    Some(&image[start..end])
}
