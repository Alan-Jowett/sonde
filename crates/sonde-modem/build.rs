// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

fn main() {
    #[cfg(feature = "esp")]
    embuild::espidf::sysenv::output();

    // Inject the git commit SHA so firmware can log it at boot.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SONDE_GIT_COMMIT={commit}");

    // Re-run if HEAD changes (new commit).
    if let Some(git_dir) = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
}
