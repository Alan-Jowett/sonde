// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TEST_CERT_PEM: &str = concat!(
    "-----BEGIN CERTIFICATE-----\n",
    "MIIBWDCB/6ADAgECAggbYn85Il496TAKBggqhkjOPQQDAjAaMRgwFgYDVQQDEw9z\n",
    "b25kZS10ZXN0LWNlcnQwHhcNMjYwNDI4MTczNDAzWhcNMzYwNDI5MTczNDAzWjAa\n",
    "MRgwFgYDVQQDEw9zb25kZS10ZXN0LWNlcnQwWTATBgcqhkjOPQIBBggqhkjOPQMB\n",
    "BwNCAASvz+sAGz7/92glvERlQlom5OFgseIgMgvGZM04KsqOD+D/hwG3tzmpOu4U\n",
    "AZyhAdrkAqvHWmfQkK5D8jdhgv33oy8wLTAMBgNVHRMBAf8EAjAAMB0GA1UdDgQW\n",
    "BBQ4+jYZ/ddAOO7/msNIHh9f61IeFjAKBggqhkjOPQQDAgNIADBFAiBmBB/wP94s\n",
    "DdBiCaUetVSkrk484rSijsJqpqnlJ/0H+QIhAMYgtEuZ8LcCsScdbwsFArve4TVN\n",
    "yfVpQffskcauwpb9\n",
    "-----END CERTIFICATE-----\n"
);

const TEST_KEY_PEM: &str = concat!(
    "-----BEGIN PRIVATE KEY-----\n",
    "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgor2vT3esA5xTV1E4\n",
    "IWCpH+V2pudlqDwiS4+LKEKy3X6hRANCAASvz+sAGz7/92glvERlQlom5OFgseIg\n",
    "MgvGZM04KsqOD+D/hwG3tzmpOu4UAZyhAdrkAqvHWmfQkK5D8jdhgv33\n",
    "-----END PRIVATE KEY-----\n"
);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn bootstrap_script_path() -> PathBuf {
    repo_root()
        .join("deploy")
        .join("azure-companion")
        .join("bootstrap.sh")
}

fn prepare_path_dir(temp: &TempDir) -> PathBuf {
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    bin_dir
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn write_runtime_wrapper(bin_dir: &Path, wrapper_log: &Path) {
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nadmin_socket=\"\"\nconnector_socket=\"\"\nstate_dir=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --admin-socket)\n      admin_socket=\"$2\"\n      shift 2\n      ;;\n    --connector-socket)\n      connector_socket=\"$2\"\n      shift 2\n      ;;\n    --state-dir)\n      state_dir=\"$2\"\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\ncase \"$1\" in\n  run)\n    printf 'run %s %s %s\\n' \"$admin_socket\" \"$connector_socket\" \"$state_dir\" >> \"{}\"\n    exit 0\n    ;;\n  bootstrap)\n    printf 'bootstrap %s %s %s\\n' \"$admin_socket\" \"$connector_socket\" \"$state_dir\" >> \"{}\"\n    if [ \"${{SONDE_TEST_FAIL_BOOTSTRAP:-0}}\" = \"1\" ]; then\n      exit 17\n    fi\n    if [ \"${{SONDE_TEST_WRITE_RUNTIME_STATE:-0}}\" = \"1\" ]; then\n      mkdir -p \"$state_dir\"\n      cat > \"$state_dir/cert.pem\" <<'EOF'\n{}\nEOF\n      cat > \"$state_dir/key.pem\" <<'EOF'\n{}\nEOF\n      cat > \"$state_dir/service-principal.json\" <<'EOF'\n{{\"tenant_id\":\"11111111-1111-1111-1111-111111111111\",\"client_id\":\"22222222-2222-2222-2222-222222222222\",\"certificate_path\":\"cert.pem\",\"private_key_path\":\"key.pem\"}}\nEOF\n      cat > \"$state_dir/service-bus.json\" <<'EOF'\n{{\"namespace\":\"example.servicebus.windows.net\",\"upstream_queue\":\"upstream\",\"downstream_queue\":\"downstream\"}}\nEOF\n    fi\n    exit 0\n    ;;\n  *)\n    exec \"{}\" --admin-socket \"$admin_socket\" --connector-socket \"$connector_socket\" --state-dir \"$state_dir\" \"$@\"\n    ;;\nesac\n",
            wrapper_log.display(),
            wrapper_log.display(),
            TEST_CERT_PEM,
            TEST_KEY_PEM,
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
}

fn write_runtime_ready_state(state_dir: &Path) {
    fs::create_dir_all(state_dir).unwrap();
    fs::write(state_dir.join("cert.pem"), TEST_CERT_PEM).unwrap();
    fs::write(state_dir.join("key.pem"), TEST_KEY_PEM).unwrap();
    fs::write(
        state_dir.join("service-principal.json"),
        br#"{"tenant_id":"11111111-1111-1111-1111-111111111111","client_id":"22222222-2222-2222-2222-222222222222","certificate_path":"cert.pem","private_key_path":"key.pem"}"#,
    )
    .unwrap();
    fs::write(
        state_dir.join("service-bus.json"),
        br#"{"namespace":"example.servicebus.windows.net","upstream_queue":"upstream","downstream_queue":"downstream"}"#,
    )
    .unwrap();
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {}", path.display());
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn container_ready_state_skips_bootstrap_and_runs() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    write_runtime_ready_state(&state_dir);
    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("SONDE_AZURE_COMPANION_IN_CONTAINER", "1");
    cmd.env("SONDE_AZURE_COMPANION_STATE_DIR", &state_dir);
    cmd.env("SONDE_GATEWAY_ADMIN_SOCKET", temp.path().join("admin.sock"));
    cmd.env(
        "SONDE_GATEWAY_CONNECTOR_SOCKET",
        temp.path().join("connector.sock"),
    );

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "runtime start failed: {output:?}");
    assert_eq!(
        fs::read_to_string(&wrapper_log).unwrap().lines().collect::<Vec<_>>(),
        vec![format!(
            "run {} {} {}",
            temp.path().join("admin.sock").display(),
            temp.path().join("connector.sock").display(),
            state_dir.display()
        )]
    );
}

#[test]
fn container_bootstrap_runs_and_then_starts_runtime() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("SONDE_AZURE_COMPANION_IN_CONTAINER", "1");
    cmd.env("SONDE_AZURE_COMPANION_STATE_DIR", &state_dir);
    cmd.env("SONDE_GATEWAY_ADMIN_SOCKET", temp.path().join("admin.sock"));
    cmd.env(
        "SONDE_GATEWAY_CONNECTOR_SOCKET",
        temp.path().join("connector.sock"),
    );
    cmd.env("SONDE_TEST_WRITE_RUNTIME_STATE", "1");

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "bootstrap failed: {output:?}");
    let lines: Vec<_> = fs::read_to_string(&wrapper_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("bootstrap "));
    assert!(lines[1].starts_with("run "));
}

#[test]
fn host_bootstrap_invokes_docker_with_expected_mounts() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    let runtime_dir = temp.path().join("runtime");
    let admin_socket_path = runtime_dir.join("admin.sock");
    let connector_socket_path = runtime_dir.join("connector.sock");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let _admin_socket = std::os::unix::net::UnixListener::bind(&admin_socket_path).unwrap();
    let _connector_socket = std::os::unix::net::UnixListener::bind(&connector_socket_path).unwrap();
    let docker_log = temp.path().join("docker.log");

    write_executable(
        &bin_dir.join("docker"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"{}\"\n",
            docker_log.display()
        ),
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("SONDE_AZURE_COMPANION_IMAGE", "sonde-azure-companion:test");
    cmd.env("SONDE_AZURE_COMPANION_STATE_DIR", &state_dir);
    cmd.env("SONDE_GATEWAY_RUNTIME_DIR", &runtime_dir);
    cmd.env("SONDE_AZURE_LOCATION", "westus2");
    cmd.env("SONDE_AZURE_PROJECT_NAME", "sonde-test");
    cmd.env("SONDE_AZURE_SUBSCRIPTION_ID", "sub-123");

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "host bootstrap failed: {output:?}");
    let logged = fs::read_to_string(docker_log).unwrap();
    assert!(logged.contains("run --rm"));
    assert!(logged.contains("sonde-azure-companion:test"));
    assert!(logged.contains("-e SONDE_AZURE_LOCATION"));
    assert!(logged.contains("-e SONDE_AZURE_PROJECT_NAME"));
    assert!(logged.contains("-e SONDE_AZURE_SUBSCRIPTION_ID"));
    assert!(!logged.contains("SONDE_AZURE_DEVICE_CLIENT_ID"));
    assert!(!logged.contains("SONDE_AZURE_DEVICE_SCOPES"));
    assert!(logged.contains("-v /var/run/docker.sock:/var/run/docker.sock"));
    assert!(logged.contains(&format!(
        "-v {}:/var/lib/sonde-azure-companion",
        state_dir.display()
    )));
    assert!(logged.contains(&format!(
        "-v {}:/var/run/sonde/admin.sock",
        admin_socket_path.display()
    )));
    assert!(logged.contains(&format!(
        "-v {}:/var/run/sonde/connector.sock",
        connector_socket_path.display()
    )));
}

#[test]
fn host_bootstrap_does_not_require_removed_device_env() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    let runtime_dir = temp.path().join("runtime");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let _admin_socket =
        std::os::unix::net::UnixListener::bind(runtime_dir.join("admin.sock")).unwrap();
    let _connector_socket =
        std::os::unix::net::UnixListener::bind(runtime_dir.join("connector.sock")).unwrap();
    let docker_log = temp.path().join("docker.log");

    write_executable(
        &bin_dir.join("docker"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"{}\"\n",
            docker_log.display()
        ),
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("SONDE_AZURE_COMPANION_IMAGE", "sonde-azure-companion:test");
    cmd.env("SONDE_AZURE_COMPANION_STATE_DIR", &state_dir);
    cmd.env("SONDE_GATEWAY_RUNTIME_DIR", &runtime_dir);

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "host bootstrap failed: {output:?}");
    assert!(docker_log.exists(), "docker should still be invoked by the host wrapper");
}

#[test]
fn t_azc_0100_container_image_smoke() {
    if !docker_available() {
        eprintln!("skipping Docker smoke test because docker is unavailable");
        return;
    }

    let repo = repo_root();
    let status = Command::new("docker")
        .current_dir(&repo)
        .args([
            "build",
            "-f",
            ".github/docker/Dockerfile.azure-companion",
            "-t",
            "sonde-azure-companion:test",
            ".",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "docker build failed");

    let status = Command::new("docker")
        .args([
            "run",
            "--rm",
            "sonde-azure-companion:test",
            "sonde-azure-companion",
            "--help",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "binary smoke test failed");

    let status = Command::new("docker")
        .args([
            "run",
            "--rm",
            "sonde-azure-companion:test",
            "sonde-azure-companion",
            "bootstrap",
            "--help",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "bootstrap smoke test failed");
}

#[test]
fn container_bootstrap_forwards_sigterm_to_bootstrap_child() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let pid_file = temp.path().join("bootstrap.pid");
    let signal_log = temp.path().join("signal.log");
    let run_log = temp.path().join("run.log");

    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --admin-socket|--connector-socket|--state-dir)\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\ncase \"$1\" in\n  bootstrap)\n    printf '%s\\n' \"$$\" > \"{}\"\n    trap 'printf \"%s\\n\" TERM >> \"{}\"; exit 143' TERM\n    trap 'printf \"%s\\n\" INT >> \"{}\"; exit 130' INT\n    while :; do\n      sleep 1\n    done\n    ;;\n  run)\n    printf 'run\\n' >> \"{}\"\n    exit 0\n    ;;\n  *)\n    exit 64\n    ;;\nesac\n",
            pid_file.display(),
            signal_log.display(),
            signal_log.display(),
            run_log.display(),
        ),
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    cmd.env("SONDE_AZURE_COMPANION_IN_CONTAINER", "1");
    cmd.env("SONDE_AZURE_COMPANION_STATE_DIR", &state_dir);
    cmd.env("SONDE_GATEWAY_ADMIN_SOCKET", temp.path().join("admin.sock"));
    cmd.env(
        "SONDE_GATEWAY_CONNECTOR_SOCKET",
        temp.path().join("connector.sock"),
    );

    let mut child = cmd.spawn().unwrap();
    wait_for_path(&pid_file);

    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "failed to signal bootstrap script");

    let exit_status = child.wait().unwrap();
    assert_eq!(exit_status.code(), Some(143));
    wait_for_path(&signal_log);
    assert_eq!(fs::read_to_string(&signal_log).unwrap(), "TERM\n");
    assert!(!run_log.exists(), "run should not start after SIGTERM");
}
