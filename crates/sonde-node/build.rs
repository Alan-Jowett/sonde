// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() {
    #[cfg(feature = "esp")]
    embuild::espidf::sysenv::output();

    // Inject the git commit SHA so firmware can log it at boot.
    // Prefer an explicit SONDE_GIT_COMMIT env var (set by CI) over running
    // git — the git binary may not have access to the repository metadata
    // inside Docker containers (e.g. safe.directory ownership mismatch).
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
    // Normalise to a short hash (7 chars) for readable log output and
    // consistency between CI (full SHA from github.sha) and local builds
    // (short SHA from `git rev-parse --short`).  Also guards against
    // newlines corrupting the `cargo:rustc-env` directive.
    let commit: String = raw.chars().take(7).collect();
    println!("cargo:rustc-env=SONDE_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=SONDE_GIT_COMMIT");

    // Re-run if HEAD changes (new commit).
    if let Some(git_dir) = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        // Watch HEAD itself (detached HEAD, bare repos, etc.).
        println!("cargo:rerun-if-changed={git_dir}/HEAD");

        // Also watch the resolved branch ref (e.g., refs/heads/main) so we
        // rerun when the branch tip moves in a normal checkout.
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

        // Watch packed-refs for repositories that pack refs.
        println!("cargo:rerun-if-changed={git_dir}/packed-refs");
    }
}
