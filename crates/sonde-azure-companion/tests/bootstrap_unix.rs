// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use futures::stream;
use tempfile::TempDir;
use tokio::net::UnixListener;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use sonde_gateway::admin::pb::gateway_admin_server::{GatewayAdmin, GatewayAdminServer};
use sonde_gateway::admin::pb::*;

type BlePairingStream =
    Pin<Box<dyn futures::Stream<Item = Result<BlePairingEvent, Status>> + Send>>;

const TEST_CERT_PEM: &str = concat!(
    "-----BEGIN CERTIFICATE-----\n",
    "MIIBszCCAVmgAwIBAgIUAlA4D2+fMZ5I2mv8VLK0sgM4nWkwCgYIKoZIzj0EAwIw\n",
    "GDEWMBQGA1UEAwwNc29uZGUtdGVzdC1jZXJ0MB4XDTI2MDEwMTAwMDAwMFoXDTM2\n",
    "MDEwMTAwMDAwMFowGDEWMBQGA1UEAwwNc29uZGUtdGVzdC1jZXJ0MFkwEwYHKoZI\n",
    "zj0CAQYIKoZIzj0DAQcDQgAErTVS8gkGqkT1vqe8LTTlYF+XNfL7+uJ+9fwbH3P9\n",
    "SiJrjN4J1wzqP8cP6lP0wtD+u2E4b4W0QW+E3ajQe8rW+6NTMFEwHQYDVR0OBBYE\n",
    "FCn5Pw3Ozl7pJ1mJtqQv5Xz6vbALMB8GA1UdIwQYMBaAFCn5Pw3Ozl7pJ1mJtqQv\n",
    "5Xz6vbALMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDSAAwRQIhAJNL5l3C\n",
    "tI6X5x4c4x6pI0vA6PfXzL5K5ll4D7OQyZcAAiA1dQXJk0v6qY+Mi8XGcX6Z7J5u\n",
    "gW4Y8d+4T2oD7j9m0Q==\n",
    "-----END CERTIFICATE-----\n"
);

const TEST_KEY_PEM: &str = concat!(
    "-----BEGIN PRIVATE KEY-----\n",
    "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg2X8i4lE4hM2t0b5Y\n",
    "fI7xW0ZzM3ZrY4L3s67qG8R0uYWhRANCAAStNVLyCQaqRPW+p7wtNOVgX5c18vv6\n",
    "4n71/Bsfc/1KImuM3gnXDOo/xw/qU/TC0P67YThvhbRBb4TdqNB7ytb7\n",
    "-----END PRIVATE KEY-----\n"
);

#[derive(Clone)]
struct TestAdminServer {
    display_requests: Arc<Mutex<Vec<Vec<String>>>>,
    display_error: Option<tonic::Code>,
}

#[tonic::async_trait]
impl GatewayAdmin for TestAdminServer {
    type OpenBlePairingStream = BlePairingStream;

    async fn list_nodes(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn get_node(
        &self,
        _request: Request<GetNodeRequest>,
    ) -> Result<Response<NodeInfo>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn register_node(
        &self,
        _request: Request<RegisterNodeRequest>,
    ) -> Result<Response<RegisterNodeResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn remove_node(
        &self,
        _request: Request<RemoveNodeRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn factory_reset(
        &self,
        _request: Request<FactoryResetRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn ingest_program(
        &self,
        _request: Request<IngestProgramRequest>,
    ) -> Result<Response<IngestProgramResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn list_programs(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListProgramsResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn assign_program(
        &self,
        _request: Request<AssignProgramRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn remove_program(
        &self,
        _request: Request<RemoveProgramRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn set_schedule(
        &self,
        _request: Request<SetScheduleRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn queue_reboot(
        &self,
        _request: Request<QueueRebootRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn queue_ephemeral(
        &self,
        _request: Request<QueueEphemeralRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn get_node_status(
        &self,
        _request: Request<GetNodeStatusRequest>,
    ) -> Result<Response<NodeStatus>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn export_state(
        &self,
        _request: Request<ExportStateRequest>,
    ) -> Result<Response<ExportStateResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn import_state(
        &self,
        _request: Request<ImportStateRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn get_modem_status(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ModemStatus>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn set_modem_channel(
        &self,
        _request: Request<SetModemChannelRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn scan_modem_channels(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ScanModemChannelsResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn show_modem_display_message(
        &self,
        request: Request<ShowModemDisplayMessageRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.display_requests
            .lock()
            .await
            .push(request.into_inner().lines);
        if let Some(code) = self.display_error {
            return Err(Status::new(code, "injected display failure"));
        }
        Ok(Response::new(Empty {}))
    }

    async fn open_ble_pairing(
        &self,
        _request: Request<OpenBlePairingRequest>,
    ) -> Result<Response<Self::OpenBlePairingStream>, Status> {
        Ok(Response::new(Box::pin(stream::empty())))
    }

    async fn close_ble_pairing(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn confirm_ble_pairing(
        &self,
        _request: Request<ConfirmBlePairingRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn list_phones(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListPhonesResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn revoke_phone(
        &self,
        _request: Request<RevokePhoneRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn add_handler(
        &self,
        _request: Request<AddHandlerRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn remove_handler(
        &self,
        _request: Request<RemoveHandlerRequest>,
    ) -> Result<Response<Empty>, Status> {
        Err(Status::unimplemented("not used in test"))
    }

    async fn list_handlers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListHandlersResponse>, Status> {
        Err(Status::unimplemented("not used in test"))
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

async fn spawn_admin_server(
    socket_path: &Path,
    display_error: Option<tonic::Code>,
) -> Arc<Mutex<Vec<Vec<String>>>> {
    let display_requests = Arc::new(Mutex::new(Vec::new()));
    let service = TestAdminServer {
        display_requests: Arc::clone(&display_requests),
        display_error,
    };
    let listener = UnixListener::bind(socket_path).unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(GatewayAdminServer::new(service))
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

fn bootstrap_script_path() -> PathBuf {
    repo_root().join("deploy/azure-companion/bootstrap.sh")
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

fn write_runtime_wrapper(bin_dir: &Path, wrapper_log: &Path) {
    write_executable(
        &bin_dir.join("sonde-azure-companion"),
        &format!(
            "#!/bin/sh\nset -eu\nadmin_socket=\"\"\nconnector_socket=\"\"\nstate_dir=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --admin-socket)\n      admin_socket=\"$2\"\n      shift 2\n      ;;\n    --connector-socket)\n      connector_socket=\"$2\"\n      shift 2\n      ;;\n    --state-dir)\n      state_dir=\"$2\"\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\ncase \"$1\" in\n  run)\n    printf 'run %s %s %s\\n' \"$admin_socket\" \"$connector_socket\" \"$state_dir\" >> \"{}\"\n    exit 0\n    ;;\n  bootstrap-auth)\n    \"{}\" --admin-socket \"$admin_socket\" --connector-socket \"$connector_socket\" --state-dir \"$state_dir\" \"$@\"\n    status=$?\n    if [ \"$status\" -eq 0 ] && [ \"${{SONDE_TEST_WRITE_RUNTIME_STATE:-0}}\" = \"1\" ]; then\n      mkdir -p \"$state_dir\"\n      cat > \"$state_dir/client-cert.pem\" <<'EOF'\n{}\nEOF\n      cat > \"$state_dir/client-key.pem\" <<'EOF'\n{}\nEOF\n      cat > \"$state_dir/service-principal.json\" <<'EOF'\n{{\"tenant_id\":\"11111111-1111-1111-1111-111111111111\",\"client_id\":\"22222222-2222-2222-2222-222222222222\",\"certificate_path\":\"client-cert.pem\",\"private_key_path\":\"client-key.pem\"}}\nEOF\n    fi\n    exit \"$status\"\n    ;;\n  *)\n    exec \"{}\" --admin-socket \"$admin_socket\" --connector-socket \"$connector_socket\" --state-dir \"$state_dir\" \"$@\"\n    ;;\nesac\n",
            wrapper_log.display(),
            env!("CARGO_BIN_EXE_sonde-azure-companion"),
            TEST_CERT_PEM,
            TEST_KEY_PEM,
            env!("CARGO_BIN_EXE_sonde-azure-companion")
        ),
    );
}

fn write_runtime_ready_state(state_dir: &Path) {
    fs::create_dir_all(state_dir).unwrap();
    fs::write(state_dir.join("client-cert.pem"), TEST_CERT_PEM).unwrap();
    fs::write(state_dir.join("client-key.pem"), TEST_KEY_PEM).unwrap();
    fs::write(
        state_dir.join("service-principal.json"),
        br#"{"tenant_id":"11111111-1111-1111-1111-111111111111","client_id":"22222222-2222-2222-2222-222222222222","certificate_path":"client-cert.pem","private_key_path":"client-key.pem"}"#,
    )
    .unwrap();
}

fn bootstrap_env(
    bin_dir: &Path,
    state_dir: &Path,
    admin_socket_path: &Path,
    connector_socket_path: &Path,
    oauth_server: &MockServer,
) -> Vec<(String, String)> {
    let mut path_value = std::env::var("PATH").unwrap_or_default();
    path_value = format!("{}:{}", bin_dir.display(), path_value);
    vec![
        ("PATH".to_string(), path_value),
        (
            "SONDE_AZURE_COMPANION_IN_CONTAINER".to_string(),
            "1".to_string(),
        ),
        (
            "SONDE_AZURE_COMPANION_STATE_DIR".to_string(),
            state_dir.display().to_string(),
        ),
        (
            "SONDE_GATEWAY_ADMIN_SOCKET".to_string(),
            admin_socket_path.display().to_string(),
        ),
        (
            "SONDE_GATEWAY_CONNECTOR_SOCKET".to_string(),
            connector_socket_path.display().to_string(),
        ),
        (
            "SONDE_AZURE_DEVICE_CLIENT_ID".to_string(),
            "test-client-id".to_string(),
        ),
        (
            "SONDE_AZURE_DEVICE_SCOPES".to_string(),
            "https://management.azure.com/.default".to_string(),
        ),
        (
            "SONDE_AZURE_DEVICE_AUTH_URL".to_string(),
            format!("{}/device", oauth_server.uri()),
        ),
        (
            "SONDE_AZURE_DEVICE_TOKEN_URL".to_string(),
            format!("{}/token", oauth_server.uri()),
        ),
        (
            "SONDE_AZURE_SERVICEBUS_NAMESPACE".to_string(),
            "example.servicebus.windows.net".to_string(),
        ),
        (
            "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE".to_string(),
            "upstream".to_string(),
        ),
        (
            "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE".to_string(),
            "downstream".to_string(),
        ),
    ]
}

async fn mount_successful_device_flow(
    oauth_server: &MockServer,
    user_code: &str,
    expected_count: u64,
) {
    mount_device_code_request(oauth_server, user_code, expected_count).await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/json")
                .set_body_string(
                    "{\"access_token\":\"temporary-token\",\"token_type\":\"Bearer\",\"expires_in\":300}",
                ),
        )
        .expect(expected_count)
        .mount(oauth_server)
        .await;
}

async fn mount_device_code_request(
    oauth_server: &MockServer,
    user_code: &str,
    expected_count: u64,
) {
    Mock::given(method("POST"))
        .and(path("/device"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/json")
                .set_body_string(format!(
                    "{{\"device_code\":\"device-code-{user_code}\",\"user_code\":\"{user_code}\",\"verification_uri\":\"https://microsoft.com/devicelogin\",\"verification_uri_complete\":\"https://microsoft.com/devicelogin?code={user_code}\",\"expires_in\":900,\"interval\":1}}"
                )),
        )
        .expect(expected_count)
        .mount(oauth_server)
        .await;
}

async fn mount_failed_token_poll(oauth_server: &MockServer, user_code: &str) {
    mount_device_code_request(oauth_server, user_code, 1).await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .append_header("content-type", "application/json")
                .set_body_string(
                    "{\"error\":\"expired_token\",\"error_description\":\"expired for test\"}",
                ),
        )
        .expect(1)
        .mount(oauth_server)
        .await;
}

async fn run_bootstrap_with_env(env: &[(String, String)]) -> std::process::Output {
    let mut cmd = TokioCommand::new("sh");
    cmd.arg(bootstrap_script_path());
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().await.unwrap()
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn t_azc_0101_0102_0200_0201_0202_bootstrap_success_path() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let admin_socket_path = temp.path().join("admin.sock");
    let connector_socket_path = temp.path().join("connector.sock");
    let display_requests = spawn_admin_server(&admin_socket_path, None).await;
    let oauth_server = MockServer::start().await;
    mount_successful_device_flow(&oauth_server, "ABCD-EFGH", 1).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);
    let mut env = bootstrap_env(
        &bin_dir,
        &state_dir,
        &admin_socket_path,
        &connector_socket_path,
        &oauth_server,
    );
    env.push((
        "SONDE_TEST_WRITE_RUNTIME_STATE".to_string(),
        "1".to_string(),
    ));

    let output = run_bootstrap_with_env(&env).await;
    assert!(output.status.success(), "bootstrap failed: {output:?}");

    let requests = display_requests.lock().await.clone();
    assert_eq!(
        requests,
        vec![vec!["Azure login".to_string(), "ABCD-EFGH".to_string()]]
    );
    let wrapper_contents = fs::read_to_string(&wrapper_log).unwrap();
    assert!(wrapper_contents.contains("run "));
    assert!(wrapper_contents.contains(&admin_socket_path.display().to_string()));
    assert!(wrapper_contents.contains(&connector_socket_path.display().to_string()));
    assert!(wrapper_contents.contains(&state_dir.display().to_string()));
}

#[tokio::test]
async fn t_azc_0203_display_failure_aborts_bootstrap() {
    for code in [tonic::Code::FailedPrecondition, tonic::Code::Unavailable] {
        let temp = TempDir::new().unwrap();
        let bin_dir = prepare_path_dir(&temp);
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let admin_socket_path = temp.path().join("admin.sock");
        let connector_socket_path = temp.path().join("connector.sock");
        let display_requests = spawn_admin_server(&admin_socket_path, Some(code)).await;
        let oauth_server = MockServer::start().await;
        mount_device_code_request(&oauth_server, "ZXCV-1234", 1).await;

        let wrapper_log = temp.path().join("wrapper.log");
        write_runtime_wrapper(&bin_dir, &wrapper_log);

        let output = run_bootstrap_with_env(&bootstrap_env(
            &bin_dir,
            &state_dir,
            &admin_socket_path,
            &connector_socket_path,
            &oauth_server,
        ))
        .await;
        assert!(!output.status.success());
        assert!(!wrapper_log.exists() || fs::read_to_string(&wrapper_log).unwrap().is_empty());
        assert_eq!(
            display_requests.lock().await.clone(),
            vec![vec!["Azure login".to_string(), "ZXCV-1234".to_string()]]
        );
    }
}

#[tokio::test]
async fn t_azc_0104_ready_state_skips_device_auth() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    write_runtime_ready_state(&state_dir);
    let admin_socket_path = temp.path().join("admin.sock");
    let connector_socket_path = temp.path().join("connector.sock");
    let display_requests = spawn_admin_server(&admin_socket_path, None).await;
    let oauth_server = MockServer::start().await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);

    let output = run_bootstrap_with_env(&bootstrap_env(
        &bin_dir,
        &state_dir,
        &admin_socket_path,
        &connector_socket_path,
        &oauth_server,
    ))
    .await;
    assert!(output.status.success(), "runtime start failed: {output:?}");
    assert!(display_requests.lock().await.is_empty());
    assert!(fs::read_to_string(&wrapper_log).unwrap().contains("run "));
}

#[tokio::test]
async fn t_azc_0105_repeated_starts_reuse_ready_state() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    write_runtime_ready_state(&state_dir);
    let admin_socket_path = temp.path().join("admin.sock");
    let connector_socket_path = temp.path().join("connector.sock");
    let display_requests = spawn_admin_server(&admin_socket_path, None).await;
    let oauth_server = MockServer::start().await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);
    let env = bootstrap_env(
        &bin_dir,
        &state_dir,
        &admin_socket_path,
        &connector_socket_path,
        &oauth_server,
    );

    let first = run_bootstrap_with_env(&env).await;
    assert!(first.status.success(), "first bootstrap failed: {first:?}");

    let second = run_bootstrap_with_env(&env).await;
    assert!(
        second.status.success(),
        "second runtime start failed: {second:?}"
    );

    assert!(display_requests.lock().await.is_empty());
    let wrapper_contents = fs::read_to_string(&wrapper_log).unwrap();
    assert_eq!(wrapper_contents.lines().count(), 2);
}

#[tokio::test]
async fn t_azc_0106_login_failure_aborts_bootstrap() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let admin_socket_path = temp.path().join("admin.sock");
    let connector_socket_path = temp.path().join("connector.sock");
    let _display_requests = spawn_admin_server(&admin_socket_path, None).await;
    let oauth_server = MockServer::start().await;
    mount_failed_token_poll(&oauth_server, "FAIL-0001").await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);

    let output = run_bootstrap_with_env(&bootstrap_env(
        &bin_dir,
        &state_dir,
        &admin_socket_path,
        &connector_socket_path,
        &oauth_server,
    ))
    .await;
    assert!(!output.status.success());
    assert!(!wrapper_log.exists());
}

#[tokio::test]
async fn t_azc_0107_bootstrap_without_runtime_state_fails_closed() {
    let temp = TempDir::new().unwrap();
    let bin_dir = prepare_path_dir(&temp);
    let state_dir = temp.path().join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let admin_socket_path = temp.path().join("admin.sock");
    let connector_socket_path = temp.path().join("connector.sock");
    let display_requests = spawn_admin_server(&admin_socket_path, None).await;
    let oauth_server = MockServer::start().await;
    mount_successful_device_flow(&oauth_server, "POST-BOOT", 1).await;

    let wrapper_log = temp.path().join("wrapper.log");
    write_runtime_wrapper(&bin_dir, &wrapper_log);

    let output = run_bootstrap_with_env(&bootstrap_env(
        &bin_dir,
        &state_dir,
        &admin_socket_path,
        &connector_socket_path,
        &oauth_server,
    ))
    .await;
    assert!(!output.status.success());
    assert_eq!(
        display_requests.lock().await.clone(),
        vec![vec!["Azure login".to_string(), "POST-BOOT".to_string()]]
    );
    assert!(!wrapper_log.exists() || fs::read_to_string(&wrapper_log).unwrap().is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("runtime state is still incomplete"));
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
    cmd.env("SONDE_AZURE_DEVICE_CLIENT_ID", "test-client-id");
    cmd.env(
        "SONDE_AZURE_DEVICE_SCOPES",
        "https://management.azure.com/.default",
    );

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "host bootstrap failed: {output:?}");
    let logged = fs::read_to_string(docker_log).unwrap();
    assert!(logged.contains("run --rm"));
    assert!(logged.contains("sonde-azure-companion:test"));
    assert!(logged.contains("-e SONDE_AZURE_DEVICE_CLIENT_ID"));
    assert!(logged.contains("-e SONDE_AZURE_DEVICE_SCOPES"));
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
    assert!(!logged.contains(&format!("-v {}:/var/run/sonde", runtime_dir.display())));
}

#[test]
fn host_bootstrap_defers_bootstrap_env_validation_to_container() {
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
    cmd.env("SONDE_AZURE_DEVICE_CLIENT_ID", "");
    cmd.env("SONDE_AZURE_DEVICE_SCOPES", "");

    let output = cmd.output().unwrap();
    assert!(output.status.success(), "host bootstrap failed: {output:?}");
    assert!(
        docker_log.exists(),
        "docker should still be invoked by the host wrapper"
    );
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
            "bootstrap-auth",
            "--help",
        ])
        .status()
        .unwrap();
    assert!(status.success(), "bootstrap-auth smoke test failed");
}

#[test]
fn container_bootstrap_forwards_sigterm_to_bootstrap_auth_child() {
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
            "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --admin-socket|--connector-socket|--state-dir)\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\ncase \"$1\" in\n  bootstrap-auth)\n    printf '%s\\n' \"$$\" > \"{}\"\n    trap 'printf \"%s\\n\" TERM >> \"{}\"; exit 143' TERM\n    trap 'printf \"%s\\n\" INT >> \"{}\"; exit 130' INT\n    while :; do\n      sleep 1\n    done\n    ;;\n  run)\n    printf 'run\\n' >> \"{}\"\n    exit 0\n    ;;\n  *)\n    exit 64\n    ;;\nesac\n",
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
    cmd.env("SONDE_AZURE_DEVICE_CLIENT_ID", "test-client-id");
    cmd.env(
        "SONDE_AZURE_DEVICE_SCOPES",
        "https://management.azure.com/.default",
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
