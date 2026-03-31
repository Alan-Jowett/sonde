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

/// Check an archive entry path for safety.
fn check_path_safety(path_str: &str) -> Result<(), BundleError> {
    let path = std::path::Path::new(path_str);
    // Reject absolute paths
    if path.is_absolute() || path_str.starts_with('/') || path_str.starts_with('\\') {
        return Err(BundleError::PathTraversal(path_str.to_string()));
    }
    // Reject paths with ".." components
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            return Err(BundleError::PathTraversal(path_str.to_string()));
        }
    }
    Ok(())
}

/// Extract a `.sondeapp` archive to a target directory.
pub fn extract_bundle(bundle_path: &Path, target_dir: &Path) -> Result<Manifest, BundleError> {
    let file = std::fs::File::open(bundle_path)?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    let mut total_size: u64 = 0;

    for entry_result in archive
        .entries()
        .map_err(|e| BundleError::InvalidArchive(format!("failed to read archive entries: {e}")))?
    {
        let mut entry = entry_result.map_err(|e| {
            BundleError::InvalidArchive(format!("failed to read archive entry: {e}"))
        })?;

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
            .map_err(|e| BundleError::InvalidArchive(format!("invalid path in archive: {e}")))?
            .to_path_buf();
        let path_str = path.to_string_lossy().to_string();

        check_path_safety(&path_str)?;

        // Reject symlinks and hardlinks
        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
            return Err(BundleError::SymlinkNotAllowed(path_str));
        }

        entry.unpack_in(target_dir).map_err(BundleError::Io)?;
    }

    // Parse manifest
    let manifest_path = target_dir.join("app.yaml");
    if !manifest_path.exists() {
        return Err(BundleError::MissingManifest);
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
            let wd_path = source_dir.join(wd);
            if wd_path.is_dir() {
                collect_dir_files(source_dir, &wd_path, &mut files_to_include)?;
            }
        }
        // Include args that are existing files within the source directory
        for arg in &handler.args {
            let arg_path = source_dir.join(arg);
            if arg_path.exists() && arg_path.starts_with(source_dir) {
                files_to_include.insert(arg.clone());
            }
        }
    }

    // Create archive
    let output_file = std::fs::File::create(output_path)?;
    let gz = GzEncoder::new(output_file, Compression::default());
    let mut builder = tar::Builder::new(gz);

    let mut bundle_files = Vec::new();

    for rel_path in &files_to_include {
        let full_path = source_dir.join(rel_path);
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

    for entry_result in archive
        .entries()
        .map_err(|e| BundleError::InvalidArchive(format!("failed to read archive: {e}")))?
    {
        let mut entry = entry_result
            .map_err(|e| BundleError::InvalidArchive(format!("failed to read entry: {e}")))?;

        let path = entry
            .path()
            .map_err(|e| BundleError::InvalidArchive(format!("invalid path: {e}")))?
            .to_string_lossy()
            .to_string();

        let size = entry.size();
        files.push(BundleFile {
            path: path.clone(),
            size,
        });

        if path == "app.yaml" {
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

    #[test]
    fn test_non_gzip_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-gzip.sondeapp");
        std::fs::write(&path, "this is not gzip").unwrap();
        let result = validate_bundle(&path);
        assert!(result.is_err());
    }

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
}
