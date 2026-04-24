// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;

use futures::stream;
use hyper_util::rt::TokioIo;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

use sonde_gateway::companion::pb::gateway_companion_server::{
    GatewayCompanion, GatewayCompanionServer,
};
use sonde_gateway::companion::pb::*;

type EventStream = Pin<Box<dyn futures::Stream<Item = Result<CompanionEvent, Status>> + Send>>;

#[derive(Clone)]
struct TestCompanionServer {
    display_requests: Arc<Mutex<Vec<Vec<String>>>>,
    display_error: Option<tonic::Code>,
}

#[tonic::async_trait]
impl GatewayCompanion for TestCompanionServer {
    type StreamEventsStream = EventStream;

    async fn stream_events(
        &self,
        _request: Request<CompanionStreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        Ok(Response::new(Box::pin(stream::empty())))
    }

    async fn list_nodes(
        &self,
        _request: Request<CompanionListNodesRequest>,
    ) -> Result<Response<CompanionListNodesResponse>, Status> {
        Ok(Response::new(CompanionListNodesResponse {
            nodes: Vec::new(),
        }))
    }

    async fn get_node(
        &self,
        _request: Request<CompanionGetNodeRequest>,
    ) -> Result<Response<CompanionNodeInfo>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn assign_program(
        &self,
        _request: Request<CompanionAssignProgramRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn set_schedule(
        &self,
        _request: Request<CompanionSetScheduleRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn queue_reboot(
        &self,
        _request: Request<CompanionQueueRebootRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn queue_ephemeral(
        &self,
        _request: Request<CompanionQueueEphemeralRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn get_node_status(
        &self,
        _request: Request<CompanionGetNodeStatusRequest>,
    ) -> Result<Response<CompanionNodeStatus>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn show_modem_display_message(
        &self,
        request: Request<CompanionShowModemDisplayMessageRequest>,
    ) -> Result<Response<CompanionEmpty>, Status> {
        self.display_requests
            .lock()
            .await
            .push(request.into_inner().lines);
        if let Some(code) = self.display_error {
            return Err(Status::new(code, "injected display failure"));
        }
        Ok(Response::new(CompanionEmpty {}))
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

async fn spawn_companion_server(
    socket_path: &Path,
    display_error: Option<tonic::Code>,
) -> Arc<Mutex<Vec<Vec<String>>>> {
    let display_requests = Arc::new(Mutex::new(Vec::new()));
    let service = TestCompanionServer {
        display_requests: Arc::clone(&display_requests),
        display_error,
    };
    let listener = UnixListener::bind(socket_path).unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(GatewayCompanionServer::new(service))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });
    display_requests
}

fn prepare_path_dir(temp: &TempDir) -> PathBuf {
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    bin_dir
}

fn bootstrap_env(bin_dir: &Path, state_dir: &Path, socket_path: &Path) -> Vec<(String, String)> {
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{}", bin_dir.display(), path);
    vec![
        ("PATH".to_string(), path),
        (
            "SONDE_AZURE_COMPANION_IN_CONTAINER".to_string(),
            "1".to_string(),
        ),
        (
            "SONDE_AZURE_COMPANION_STATE_DIR".to_string(),
            state_dir.display().to_string(),
        ),
        (
            "SONDE_GATEWAY_COMPANION_SOCKET".to_string(),
            socket_path.display().to_string(),
        ),
    ]
}

fn bootstrap_script_path() -> PathBuf {
    repo_root().join("deploy/azure-companion/bootstrap.sh")
}

#[tokio::test]
async fn t_azc_0101_0102_0200_0201_0202_bootstrap_success_path() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let socket_path = temp.path().join("companion.sock");
    let display_requests = spawn_companion_server(&socket_path, None).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nsocket=\"\"\nif [ \"$1\" = \"--companion-socket\" ]; then\n  socket=\"$2\"\n  shift 2\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'run %s\\n' \"$socket\" >> \"{}\"\n  exit 0\nfi\nexec \"{}\" --companion-socket \"$socket\" \"$@\"\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
    write_executable(
        &bin_dir.join("az"),
        "#!/bin/sh\nset -eu\nprintf 'To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD-EFGH to authenticate.\\n'\ntouch \"$AZURE_CONFIG_DIR/msal_token_cache.json\"\n",
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in bootstrap_env(&bin_dir, &state_dir, &socket_path) {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "bootstrap failed: {output:?}");

    let requests = display_requests.lock().await.clone();
    assert_eq!(
        requests,
        vec![vec!["Azure login".to_string(), "ABCD-EFGH".to_string()]]
    );
    assert!(wrapper_log.exists());
    assert!(fs::read_to_string(&wrapper_log).unwrap().contains("run "));
    assert!(state_dir.join("azure/msal_token_cache.json").exists());
}

#[tokio::test]
async fn t_azc_0203_display_failure_aborts_bootstrap() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let socket_path = temp.path().join("companion.sock");
    let display_requests =
        spawn_companion_server(&socket_path, Some(tonic::Code::FailedPrecondition)).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nsocket=\"\"\nif [ \"$1\" = \"--companion-socket\" ]; then\n  socket=\"$2\"\n  shift 2\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'run %s\\n' \"$socket\" >> \"{}\"\n  exit 0\nfi\nexec \"{}\" --companion-socket \"$socket\" \"$@\"\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
    write_executable(
        &bin_dir.join("az"),
        "#!/bin/sh\nset -eu\nprintf 'To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ZXCV-1234 to authenticate.\\n'\nsleep 1\n",
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in bootstrap_env(&bin_dir, &state_dir, &socket_path) {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert!(!output.status.success());
    assert!(
        wrapper_log.exists() == false
            || fs::read_to_string(&wrapper_log)
                .unwrap_or_default()
                .is_empty()
    );
    assert_eq!(
        display_requests.lock().await.clone(),
        vec![vec!["Azure login".to_string(), "ZXCV-1234".to_string()]]
    );
}

#[tokio::test]
async fn t_azc_0104_persisted_state_skips_login_and_display() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(state_dir.join("azure")).unwrap();
    fs::write(state_dir.join("azure/msal_token_cache.json"), b"cached").unwrap();
    let socket_path = temp.path().join("companion.sock");
    let display_requests = spawn_companion_server(&socket_path, None).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nsocket=\"\"\nif [ \"$1\" = \"--companion-socket\" ]; then\n  socket=\"$2\"\n  shift 2\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'run %s\\n' \"$socket\" >> \"{}\"\n  exit 0\nfi\nexec \"{}\" --companion-socket \"$socket\" \"$@\"\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
    write_executable(
        &bin_dir.join("az"),
        "#!/bin/sh\nset -eu\nprintf 'az should not be invoked\\n' >&2\nexit 9\n",
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in bootstrap_env(&bin_dir, &state_dir, &socket_path) {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "bootstrap failed: {output:?}");
    assert_eq!(display_requests.lock().await.len(), 0);
    assert!(fs::read_to_string(&wrapper_log).unwrap().contains("run "));
}

#[tokio::test]
async fn t_azc_0105_missing_state_reenters_login() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(state_dir.join("azure")).unwrap();
    let marker = state_dir.join("az-invoked");
    let socket_path = temp.path().join("companion.sock");
    let _display_requests = spawn_companion_server(&socket_path, None).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nsocket=\"\"\nif [ \"$1\" = \"--companion-socket\" ]; then\n  socket=\"$2\"\n  shift 2\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'run %s\\n' \"$socket\" >> \"{}\"\n  exit 0\nfi\nexec \"{}\" --companion-socket \"$socket\" \"$@\"\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
    write_executable(
        &bin_dir.join("az"),
        &format!(
            "#!/bin/sh\nset -eu\nprintf 'To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code QWER-5678 to authenticate.\\n'\ntouch \"{}\"\ntouch \"$AZURE_CONFIG_DIR/msal_token_cache.json\"\n",
            marker.display()
        ),
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in bootstrap_env(&bin_dir, &state_dir, &socket_path) {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "bootstrap failed: {output:?}");
    assert!(marker.exists());
}

#[tokio::test]
async fn t_azc_0106_login_failure_aborts_bootstrap() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let socket_path = temp.path().join("companion.sock");
    let _display_requests = spawn_companion_server(&socket_path, None).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nsocket=\"\"\nif [ \"$1\" = \"--companion-socket\" ]; then\n  socket=\"$2\"\n  shift 2\nfi\nif [ \"$1\" = \"run\" ]; then\n  printf 'run %s\\n' \"$socket\" >> \"{}\"\n  exit 0\nfi\nexec \"{}\" --companion-socket \"$socket\" \"$@\"\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
    write_executable(
        &bin_dir.join("az"),
        "#!/bin/sh\nset -eu\nprintf 'To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code FAIL-0001 to authenticate.\\n'\nexit 42\n",
    );

    let mut cmd = Command::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in bootstrap_env(&bin_dir, &state_dir, &socket_path) {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert!(!wrapper_log.exists());
}

#[test]
fn host_bootstrap_invokes_docker_with_expected_mounts() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    let runtime_dir = temp.path().join("runtime");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
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
    let logged = fs::read_to_string(docker_log).unwrap();
    assert!(logged.contains("run --rm"));
    assert!(logged.contains("sonde-azure-companion:test"));
    assert!(logged.contains(&format!(
        "-v {}:/var/lib/sonde-azure-companion",
        state_dir.display()
    )));
    assert!(logged.contains(&format!("-v {}:/var/run/sonde", runtime_dir.display())));
}

#[test]
fn t_azc_0100_container_image_smoke() {
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
        .args(["run", "--rm", "sonde-azure-companion:test", "az", "version"])
        .status()
        .unwrap();
    assert!(status.success(), "azure-cli smoke test failed");
}
