// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

//! Archive creation, extraction, and inspection for `.sondeapp` bundles.

use crate::error::BundleError;
use crate::manifest::Manifest;
use crate::validate::{self, ValidationResult};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

/// Information about a file in the bundle archive.
#[derive(Debug)]
pub struct BundleFile {
    pub path: String,
    pub size: u64,
}

/// Structured information about a bundle.
#[derive(Debug)]
pub struct BundleInfo {
    pub manifest: Manifest,
    pub files: Vec<BundleFile>,
    pub archive_size: u64,
}

/// Maximum total extracted size (100 MB) to prevent decompression bombs.
const MAX_EXTRACT_SIZE: u64 = 100 * 1024 * 1024;

/// Maximum number of entries in an archive to prevent DoS via entry flooding.
const MAX_ENTRY_COUNT: usize = 10_000;

/// Maximum manifest size when reading from an archive (1 MB).
const MAX_MANIFEST_SIZE: u64 = 1_048_576;

/// Check an archive entry path for safety.
fn check_path_safety(path: &Path) -> Result<(), BundleError> {
    if crate::validate::is_path_unsafe(path) {
        return Err(BundleError::PathTraversal(path.display().to_string()));
    }
    Ok(())
}

/// Extract a `.sondeapp` archive to a target directory.
///
/// Extraction uses a staging directory to ensure `target_dir` is not left
/// in a partially-extracted state if a later entry fails validation.
pub fn extract_bundle(bundle_path: &Path, target_dir: &Path) -> Result<Manifest, BundleError> {
    let file = std::fs::File::open(bundle_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    let mut total_size: u64 = 0;
    let mut entry_count: usize = 0;

    // Extract into a staging directory to avoid partial extraction on error.
    std::fs::create_dir_all(target_dir)?;
    let staging_dir = tempfile::tempdir_in(target_dir)?;

    for entry_result in archive
        .entries()
        .map_err(|e| BundleError::InvalidArchive(format!("failed to read archive entries: {e}")))?
    {
        let mut entry = entry_result.map_err(|e| {
            BundleError::InvalidArchive(format!("failed to read archive entry: {e}"))
        })?;

        entry_count += 1;
        if entry_count > MAX_ENTRY_COUNT {
            return Err(BundleError::InvalidArchive(format!(
                "archive exceeds maximum entry count ({})",
                MAX_ENTRY_COUNT
            )));
        }

        // Check cumulative extracted size
        total_size = total_size.saturating_add(entry.size());
        if total_size > MAX_EXTRACT_SIZE {
            return Err(BundleError::InvalidArchive(format!(
                "archive exceeds maximum extracted size ({} bytes)",
                MAX_EXTRACT_SIZE
            )));
        }

        let path = entry
            .path()
            .map_err(|e| BundleError::InvalidArchive(format!("invalid path in archive: {e}")))?;

        // Safety checks on the original Path (not lossy string) to prevent
        // non-UTF8 bypass of string-based validation.
        check_path_safety(&path)?;

        let path_str = path.to_string_lossy().to_string();

        // Only allow regular files and directories; reject all special types.
        let entry_type = entry.header().entry_type();
        match entry_type {
            tar::EntryType::Regular | tar::EntryType::Directory => {
                entry
                    .unpack_in(staging_dir.path())
                    .map_err(BundleError::Io)?;
            }
            // Internal tar metadata — consumed by the reader, skip unpacking
            tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink
            | tar::EntryType::XHeader
            | tar::EntryType::XGlobalHeader => {}
            tar::EntryType::Symlink | tar::EntryType::Link => {
                return Err(BundleError::SymlinkNotAllowed(path_str));
            }
            _ => {
                return Err(BundleError::InvalidArchive(format!(
                    "unsupported entry type for path: {}",
                    path_str
                )));
            }
        }
    }

    // All entries validated. Move contents from staging to target_dir.
    // Verify target_dir is empty to avoid partial updates from conflicting paths.
    let existing: Vec<_> = std::fs::read_dir(target_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path() != staging_dir.path())
        .collect();
    if !existing.is_empty() {
        return Err(BundleError::InvalidArchive(
            "target directory is not empty; cannot extract safely".to_string(),
        ));
    }
    for item in std::fs::read_dir(staging_dir.path())? {
        let item = item?;
        let dest = target_dir.join(item.file_name());
        std::fs::rename(item.path(), &dest)?;
    }

    // Parse manifest (enforce size limit consistent with inspect_bundle)
    let manifest_path = target_dir.join("app.yaml");
    if !manifest_path.exists() {
        return Err(BundleError::MissingManifest);
    }
    let manifest_size = std::fs::metadata(&manifest_path)?.len();
    if manifest_size > MAX_MANIFEST_SIZE {
        return Err(BundleError::InvalidArchive(format!(
            "app.yaml exceeds maximum manifest size ({} bytes)",
            MAX_MANIFEST_SIZE
        )));
    }
    let yaml = std::fs::read_to_string(&manifest_path)?;
    Manifest::from_yaml(&yaml)
}

/// Create a `.sondeapp` archive from a source directory.
pub fn create_bundle(source_dir: &Path, output_path: &Path) -> Result<BundleInfo, BundleError> {
    // Parse manifest
    let manifest_path = source_dir.join("app.yaml");
    if !manifest_path.exists() {
        return Err(BundleError::MissingManifest);
    }
    let yaml = std::fs::read_to_string(&manifest_path)?;
    let manifest = Manifest::from_yaml(&yaml)?;

    // Validate
    let result = validate::validate_manifest(&manifest, source_dir);
    if !result.is_valid() {
        return Err(BundleError::ValidationFailed(result));
    }

    // Collect files to include
    let mut files_to_include = HashSet::new();
    files_to_include.insert("app.yaml".to_string());
    for prog in &manifest.programs {
        files_to_include.insert(prog.path.clone());
    }
    for handler in &manifest.handlers {
        // Include handler working_dir contents if specified
        if let Some(ref wd) = handler.working_dir {
            if check_path_safety(Path::new(wd)).is_ok() {
                let wd_path = source_dir.join(wd);
                if wd_path.is_dir() && wd_path.starts_with(source_dir) {
                    collect_dir_files(source_dir, &wd_path, &mut files_to_include)?;
                }
            }
        }
        // Include handler command if it is a safe relative path to a file
        // inside the source directory (e.g., "handler/script.py")
        if check_path_safety(Path::new(&handler.command)).is_ok() {
            let cmd_path = source_dir.join(&handler.command);
            if cmd_path.is_file() && cmd_path.starts_with(source_dir) {
                files_to_include.insert(handler.command.clone());
            }
        }
        // Include args that are existing files within the source directory
        for arg in &handler.args {
            if check_path_safety(Path::new(arg)).is_err() {
                continue;
            }
            let arg_path = source_dir.join(arg);
            if arg_path.exists() && arg_path.starts_with(source_dir) {
                files_to_include.insert(arg.clone());
            }
        }
    }

    // Create archive in a temporary file for atomic writes
    let output_dir = output_path.parent().unwrap_or(Path::new("."));
    let tmp_file = tempfile::NamedTempFile::new_in(output_dir)?;
    let file = tmp_file.as_file().try_clone()?;
    let gz = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(gz);

    // Sort files for deterministic archive ordering
    let mut sorted_files: Vec<&String> = files_to_include.iter().collect();
    sorted_files.sort();

    let mut bundle_files = Vec::new();

    for rel_path in sorted_files {
        let full_path = source_dir.join(rel_path);
        // Reject symlinks to prevent bundling symlink targets
        if std::fs::symlink_metadata(&full_path)?.is_symlink() {
            return Err(BundleError::SymlinkNotAllowed(rel_path.clone()));
        }
        if full_path.is_file() {
            let size = full_path.metadata()?.len();
            builder
                .append_path_with_name(&full_path, rel_path)
                .map_err(|e| {
                    BundleError::Io(std::io::Error::other(format!(
                        "failed to add {rel_path} to archive: {e}"
                    )))
                })?;
            bundle_files.push(BundleFile {
                path: rel_path.clone(),
                size,
            });
        }
    }

    let gz = builder.into_inner().map_err(|e| {
        BundleError::Io(std::io::Error::other(format!(
            "failed to finish archive: {e}"
        )))
    })?;
    gz.finish()?;

    // Atomically move temp file to final output path
    tmp_file
        .persist(output_path)
        .map_err(|e| BundleError::Io(e.error))?;

    let archive_size = std::fs::metadata(output_path)?.len();

    Ok(BundleInfo {
        manifest,
        files: bundle_files,
        archive_size,
    })
}

/// Inspect a bundle without extracting to disk.
pub fn inspect_bundle(bundle_path: &Path) -> Result<BundleInfo, BundleError> {
    let archive_size = std::fs::metadata(bundle_path)?.len();
    let file = std::fs::File::open(bundle_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut files = Vec::new();
    let mut manifest_yaml = None;
    let mut entry_count: usize = 0;
    let mut total_size: u64 = 0;

    for entry_result in archive
        .entries()
        .map_err(|e| BundleError::InvalidArchive(format!("failed to read archive: {e}")))?
    {
        let mut entry = entry_result
            .map_err(|e| BundleError::InvalidArchive(format!("failed to read entry: {e}")))?;

        entry_count += 1;
        if entry_count > MAX_ENTRY_COUNT {
            return Err(BundleError::InvalidArchive(format!(
                "archive exceeds maximum entry count ({})",
                MAX_ENTRY_COUNT
            )));
        }

        // Enforce cumulative size limit to prevent decompression bomb DoS
        total_size = total_size.saturating_add(entry.size());
        if total_size > MAX_EXTRACT_SIZE {
            return Err(BundleError::InvalidArchive(format!(
                "archive exceeds maximum extracted size ({} bytes)",
                MAX_EXTRACT_SIZE
            )));
        }

        let entry_path = entry
            .path()
            .map_err(|e| BundleError::InvalidArchive(format!("invalid path: {e}")))?;

        check_path_safety(&entry_path)?;

        // Mirror extract_bundle entry-type filtering
        let entry_type = entry.header().entry_type();
        match entry_type {
            tar::EntryType::Regular | tar::EntryType::Directory => {}
            tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink
            | tar::EntryType::XHeader
            | tar::EntryType::XGlobalHeader => continue,
            tar::EntryType::Symlink | tar::EntryType::Link => {
                return Err(BundleError::SymlinkNotAllowed(
                    entry_path.to_string_lossy().to_string(),
                ));
            }
            _ => {
                return Err(BundleError::InvalidArchive(format!(
                    "unsupported entry type for path: {}",
                    entry_path.to_string_lossy()
                )));
            }
        }

        // Normalize path: strip leading "./" for consistent matching
        let raw_path = entry_path.to_string_lossy().to_string();
        let path = raw_path.strip_prefix("./").unwrap_or(&raw_path).to_string();

        let size = entry.size();
        files.push(BundleFile {
            path: path.clone(),
            size,
        });

        if path == "app.yaml" {
            if size > MAX_MANIFEST_SIZE {
                return Err(BundleError::InvalidArchive(format!(
                    "app.yaml exceeds maximum manifest size ({} bytes)",
                    MAX_MANIFEST_SIZE
                )));
            }
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            manifest_yaml = Some(content);
        }
    }

    let yaml = manifest_yaml.ok_or(BundleError::MissingManifest)?;
    let manifest = Manifest::from_yaml(&yaml)?;

    Ok(BundleInfo {
        manifest,
        files,
        archive_size,
    })
}

/// Validate a bundle archive.
pub fn validate_bundle(bundle_path: &Path) -> Result<ValidationResult, BundleError> {
    let dir = tempfile::tempdir()?;
    let manifest = extract_bundle(bundle_path, dir.path())?;
    Ok(validate::validate_manifest(&manifest, dir.path()))
}

/// Recursively collect files in a directory.
fn collect_dir_files(
    base: &Path,
    dir: &Path,
    files: &mut HashSet<String>,
) -> Result<(), BundleError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        // Use symlink_metadata to avoid following symlinks
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_dir_files(base, &path, files)?;
        } else if metadata.is_file() {
            if let Ok(rel) = path.strip_prefix(base) {
                files.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_bundle_dir(dir: &Path) {
        let manifest = r#"
schema_version: 1
name: "test-app"
version: "0.1.0"
programs:
  - name: "test-prog"
    path: "bpf/test.elf"
    profile: "resident"
nodes:
  - name: "node-1"
    program: "test-prog"
"#;
        std::fs::write(dir.join("app.yaml"), manifest).unwrap();
        std::fs::create_dir_all(dir.join("bpf")).unwrap();
        let mut elf = vec![0x7f, b'E', b'L', b'F'];
        elf.extend_from_slice(&[0u8; 12]);
        std::fs::write(dir.join("bpf").join("test.elf"), &elf).unwrap();
    }

    /// T-SB-0800: Create valid bundle + T-SB-0200: Valid archive extraction.
    #[test]
    fn test_create_and_extract_round_trip() {
        let src = tempfile::tempdir().unwrap();
        write_test_bundle_dir(src.path());

        let out = tempfile::tempdir().unwrap();
        let bundle_path = out.path().join("test.sondeapp");
        let info = create_bundle(src.path(), &bundle_path).unwrap();

        assert_eq!(info.manifest.name, "test-app");
        assert!(info.archive_size > 0);

        // Extract and verify
        let extract_dir = tempfile::tempdir().unwrap();
        let manifest = extract_bundle(&bundle_path, extract_dir.path()).unwrap();
        assert_eq!(manifest.name, "test-app");
        assert!(extract_dir.path().join("app.yaml").exists());
        assert!(extract_dir.path().join("bpf").join("test.elf").exists());
    }

    /// T-SB-0801: Create fails on invalid manifest.
    #[test]
    fn test_create_fails_on_invalid_manifest() {
        let src = tempfile::tempdir().unwrap();
        // Missing name
        std::fs::write(
            src.path().join("app.yaml"),
            "schema_version: 1\nversion: '0.1.0'\nprograms: []\nnodes: []\n",
        )
        .unwrap();
        let out = tempfile::tempdir().unwrap();
        let bundle_path = out.path().join("test.sondeapp");
        let result = create_bundle(src.path(), &bundle_path);
        assert!(result.is_err());
        assert!(!bundle_path.exists());
    }

    /// T-SB-0900: Inspect valid bundle.
    #[test]
    fn test_inspect_bundle() {
        let src = tempfile::tempdir().unwrap();
        write_test_bundle_dir(src.path());

        let out = tempfile::tempdir().unwrap();
        let bundle_path = out.path().join("test.sondeapp");
        create_bundle(src.path(), &bundle_path).unwrap();

        let info = inspect_bundle(&bundle_path).unwrap();
        assert_eq!(info.manifest.name, "test-app");
        assert!(!info.files.is_empty());
    }

    /// T-SB-1002: CLI validate — valid bundle + T-SB-1003: CLI validate — invalid bundle.
    #[test]
    fn test_validate_bundle() {
        let src = tempfile::tempdir().unwrap();
        write_test_bundle_dir(src.path());

        let out = tempfile::tempdir().unwrap();
        let bundle_path = out.path().join("test.sondeapp");
        create_bundle(src.path(), &bundle_path).unwrap();

        let result = validate_bundle(&bundle_path).unwrap();
        assert!(result.is_valid(), "expected valid: {:?}", result.errors);
    }

    /// T-SB-0204: Archive missing app.yaml.
    #[test]
    fn test_missing_manifest() {
        let src = tempfile::tempdir().unwrap();
        // Create an empty tgz
        let bundle_path = src.path().join("empty.sondeapp");
        let file = std::fs::File::create(&bundle_path).unwrap();
        let gz = GzEncoder::new(file, Compression::default());
        let builder = tar::Builder::new(gz);
        let gz = builder.into_inner().unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = extract_bundle(&bundle_path, extract_dir.path());
        assert!(matches!(result, Err(BundleError::MissingManifest)));
    }

    /// T-SB-0203: Non-gzip file rejection.
    #[test]
    fn test_non_gzip_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-gzip.sondeapp");
        std::fs::write(&path, "this is not gzip").unwrap();
        let result = validate_bundle(&path);
        assert!(result.is_err());
    }

    /// T-SB-0802: Create excludes unreferenced files.
    #[test]
    fn test_create_excludes_unreferenced_files() {
        let src = tempfile::tempdir().unwrap();
        write_test_bundle_dir(src.path());

        // Add unreferenced file
        std::fs::write(
            src.path().join("bpf").join("unreferenced.elf"),
            [0x7f, b'E', b'L', b'F', 0, 0, 0, 0],
        )
        .unwrap();

        let out = tempfile::tempdir().unwrap();
        let bundle_path = out.path().join("test.sondeapp");
        let info = create_bundle(src.path(), &bundle_path).unwrap();

        // Verify unreferenced file is NOT in the archive
        assert!(!info.files.iter().any(|f| f.path.contains("unreferenced")));

        // Double-check by inspecting
        let inspected = inspect_bundle(&bundle_path).unwrap();
        assert!(!inspected
            .files
            .iter()
            .any(|f| f.path.contains("unreferenced")));
    }

    /// T-SB-0201: Archive with path traversal.
    #[test]
    fn test_path_traversal_rejected() {
        // Build a tgz with a path-traversal entry by writing raw tar headers
        // (the tar crate's `append_data` rejects ".." itself, so we bypass it).
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("evil.sondeapp");

        // Manually construct a tar entry with ".." in the path
        let mut tar_bytes = Vec::new();
        {
            let mut header = tar::Header::new_gnu();
            let data = b"malicious";
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            // Set path raw to bypass tar crate's safety check
            header.as_gnu_mut().unwrap().name[..14].copy_from_slice(b"../etc/passwd\0");
            header.set_cksum();

            // Write header
            tar_bytes.extend_from_slice(header.as_bytes());
            // Write data block (padded to 512 bytes)
            let mut block = [0u8; 512];
            block[..data.len()].copy_from_slice(data);
            tar_bytes.extend_from_slice(&block);
            // Two empty blocks to signal end of archive
            tar_bytes.extend_from_slice(&[0u8; 1024]);
        }

        // Compress
        let file = std::fs::File::create(&bundle_path).unwrap();
        let mut gz = GzEncoder::new(file, Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = extract_bundle(&bundle_path, extract_dir.path());
        assert!(
            matches!(result, Err(BundleError::PathTraversal(_))),
            "expected PathTraversal error, got: {:?}",
            result
        );
    }

    /// T-SB-0202: Archive with symlink.
    #[test]
    fn test_symlink_rejected() {
        // Create a tgz with a symlink entry
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("symlink.sondeapp");
        let file = std::fs::File::create(&bundle_path).unwrap();
        let gz = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(gz);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_link(&mut header, "evil-link", "/etc/shadow")
            .unwrap();

        let gz = builder.into_inner().unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = extract_bundle(&bundle_path, extract_dir.path());
        assert!(
            matches!(result, Err(BundleError::SymlinkNotAllowed(_))),
            "expected SymlinkNotAllowed error, got: {:?}",
            result
        );
    }

    /// T-SB-0201/T-SB-0202: Archive safety — hardlink rejected.
    #[test]
    fn test_hardlink_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("hardlink.sondeapp");
        let file = std::fs::File::create(&bundle_path).unwrap();
        let gz = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(gz);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_link(&mut header, "evil-hardlink", "../../etc/passwd")
            .unwrap();

        let gz = builder.into_inner().unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = extract_bundle(&bundle_path, extract_dir.path());
        assert!(
            matches!(result, Err(BundleError::SymlinkNotAllowed(_))),
            "expected SymlinkNotAllowed error for hardlink, got: {:?}",
            result
        );
    }

    /// T-SB-0201/T-SB-0202: Archive safety — FIFO entry rejected.
    #[test]
    fn test_fifo_entry_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("fifo.sondeapp");

        let mut tar_bytes = Vec::new();
        {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Fifo);
            header.set_size(0);
            header.set_mode(0o644);
            header.as_gnu_mut().unwrap().name[..9].copy_from_slice(b"evil-fifo");
            header.set_cksum();
            tar_bytes.extend_from_slice(header.as_bytes());
            tar_bytes.extend_from_slice(&[0u8; 1024]);
        }

        let file = std::fs::File::create(&bundle_path).unwrap();
        let mut gz = GzEncoder::new(file, Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = extract_bundle(&bundle_path, extract_dir.path());
        assert!(
            matches!(result, Err(BundleError::InvalidArchive(_))),
            "expected InvalidArchive error for FIFO entry, got: {:?}",
            result
        );
    }
}
