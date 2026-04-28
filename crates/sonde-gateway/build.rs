// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    println!("cargo:rerun-if-changed=proto/admin.proto");
    tonic_prost_build::configure().compile_protos(&["proto/admin.proto"], &["proto"])?;

    // Inject the git commit SHA so the binary can display it at runtime (GW-1303).
    // Prefer an explicit SONDE_GIT_COMMIT env var (set by CI) over running git.
    let raw = std::env::var("SONDE_GIT_COMMIT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        });
    let commit: String = raw.chars().take(7).collect();
    println!("cargo:rustc-env=SONDE_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=SONDE_GIT_COMMIT");

    if let Some(git_dir) = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = std::process::Command::new("git")
            .args(["symbolic-ref", "-q", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        {
            let ref_path = format!("{git_dir}/{head_ref}");
            println!("cargo:rerun-if-changed={ref_path}");
        }
        println!("cargo:rerun-if-changed={git_dir}/packed-refs");
    }

    Ok(())
}
