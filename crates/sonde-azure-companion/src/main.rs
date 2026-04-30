// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use azservicebus::{
    ServiceBusClient, ServiceBusClientOptions, ServiceBusMessage, ServiceBusReceiver,
    ServiceBusReceiverOptions, ServiceBusSender, ServiceBusSenderOptions,
};
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use azure_core::date::OffsetDateTime;
use azure_core::error::ErrorKind;
use azure_core::Uuid;
use base64::Engine as _;
use bollard::container::LogOutput;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use bollard::{body_full, Docker};
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::pkcs8::DecodePrivateKey as Ed25519DecodePrivateKey;
use futures_util::StreamExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use regex::Regex;
use rsa::pkcs1::DecodeRsaPrivateKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spki::EncodePublicKey;
use thiserror::Error;
use time::Duration as TimeDuration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint, Uri};
use x509_cert::der::{Decode, Encode};
use x509_cert::Certificate;

use sonde_gateway::admin::pb::gateway_admin_client::GatewayAdminClient;
use sonde_gateway::admin::pb::ShowModemDisplayMessageRequest;

#[cfg(unix)]
const DEFAULT_ADMIN_SOCKET: &str = "/var/run/sonde/admin.sock";
#[cfg(windows)]
const DEFAULT_ADMIN_SOCKET: &str = r"\\.\pipe\sonde-admin";
#[cfg(unix)]
const DEFAULT_CONNECTOR_SOCKET: &str = "/var/run/sonde/connector.sock";
#[cfg(windows)]
const DEFAULT_CONNECTOR_SOCKET: &str = r"\\.\pipe\sonde-connector";
#[cfg(unix)]
const DEFAULT_STATE_DIR: &str = "/var/lib/sonde-azure-companion";
#[cfg(windows)]
const DEFAULT_STATE_DIR: &str = r"C:\ProgramData\sonde-azure-companion";

const SERVICE_PRINCIPAL_STATE_FILENAME: &str = "service-principal.json";
/// Path to bundled Bicep files inside the companion container image.
const BUNDLED_BICEP_PATH: &str = "/opt/sonde/deploy/bicep";
/// Pinned Azure CLI container image digest for reproducible bootstrap.
const AZURE_CLI_IMAGE: &str =
    "mcr.microsoft.com/azure-cli@sha256:7f9ca8e6bf1c72e5fafefb6925546272776d635fb428538455c5c79bb77e2aa7";
const CERT_PEM_FILENAME: &str = "cert.pem";
const KEY_PEM_FILENAME: &str = "key.pem";
const SERVICE_BUS_CONFIG_FILENAME: &str = "service-bus.json";
const STAGING_DIR_NAME: &str = ".staging";
const DEFAULT_DOWNSTREAM_WAIT_SECS: u64 = 1;
const CONNECTOR_MAX_FRAME_LENGTH: usize =
    sonde_gateway::connector::DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE;
const ACCESS_TOKEN_REFRESH_MARGIN_SECS: i64 = 300;
const CLIENT_ASSERTION_LIFETIME_SECS: i64 = 600;
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const TOKEN_HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;
const TOKEN_HTTP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
enum CompanionError {
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    TonicTransport(#[from] tonic::transport::Error),
    #[error(transparent)]
    TonicStatus(#[from] tonic::Status),
    #[error(transparent)]
    AzureCore(#[from] azure_core::Error),
    #[error(transparent)]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

#[derive(Debug, Parser)]
#[command(name = "sonde-azure-companion")]
struct Cli {
    /// Gateway admin socket path (UDS on Unix, named pipe on Windows).
    #[arg(long, env = "SONDE_GATEWAY_ADMIN_SOCKET", default_value = DEFAULT_ADMIN_SOCKET)]
    admin_socket: String,

    /// Gateway connector socket path (UDS on Unix, named pipe on Windows).
    #[arg(long, env = "SONDE_GATEWAY_CONNECTOR_SOCKET", default_value = DEFAULT_CONNECTOR_SOCKET)]
    connector_socket: String,

    /// Mounted state directory reserved for bootstrap output and runtime auth material.
    #[arg(long, env = "SONDE_AZURE_COMPANION_STATE_DIR", default_value = DEFAULT_STATE_DIR)]
    state_dir: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the long-running Azure connector runtime.
    Run,
    /// Perform bootstrap deployment and display the device code on the modem.
    Bootstrap(BootstrapArgs),
    /// Ask the gateway admin API to render a transient modem display message.
    DisplayMessage {
        /// Between 1 and 4 text lines to render.
        lines: Vec<String>,
    },
    /// Check whether the persisted runtime state and runtime configuration are present.
    #[command(hide = true)]
    CheckRuntimeReady,
}

#[derive(Debug, Args)]
struct BootstrapArgs {
    /// Azure region for Bicep deployment.
    #[arg(long, env = "SONDE_AZURE_LOCATION", default_value = "eastus")]
    azure_location: String,

    /// Project name for Bicep deployment.
    #[arg(long, env = "SONDE_AZURE_PROJECT_NAME", default_value = "sonde")]
    azure_project_name: String,

    /// Optional Azure subscription ID override.
    #[arg(long, env = "SONDE_AZURE_SUBSCRIPTION_ID")]
    azure_subscription_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeConfig {
    namespace: String,
    upstream_queue: String,
    downstream_queue: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct ServicePrincipalStateFile {
    tenant_id: String,
    client_id: String,
    certificate_path: String,
    private_key_path: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct ServiceBusConfigFile {
    namespace: String,
    upstream_queue: String,
    downstream_queue: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeCredentialState {
    tenant_id: String,
    client_id: String,
    certificate_path: PathBuf,
    private_key_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct ClientAssertionClaims {
    aud: String,
    iss: String,
    sub: String,
    jti: String,
    nbf: i64,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct BicepBootstrapValues {
    #[serde(rename = "tenantId")]
    tenant_id: BicepOutputValue,
    #[serde(rename = "clientId")]
    client_id: BicepOutputValue,
    #[serde(rename = "serviceBusNamespace")]
    service_bus_namespace: BicepOutputValue,
    #[serde(rename = "upstreamQueue")]
    upstream_queue: BicepOutputValue,
    #[serde(rename = "downstreamQueue")]
    downstream_queue: BicepOutputValue,
}

#[derive(Debug, Deserialize)]
struct BicepOutputValue {
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct BicepOutputs {
    #[serde(rename = "companionBootstrapValues")]
    companion_bootstrap_values: BicepOutputValue,
}

struct ClientAssertionCredential {
    client_id: String,
    token_endpoint: String,
    signing_algorithm: Algorithm,
    signing_key: EncodingKey,
    certificate_thumbprint: String,
    http_client: reqwest::Client,
    cached_token: Mutex<Option<CachedAccessToken>>,
}

struct CachedAccessToken {
    scope: String,
    token: AccessToken,
}

impl std::fmt::Debug for ClientAssertionCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientAssertionCredential")
            .field("client_id", &self.client_id)
            .field("token_endpoint", &self.token_endpoint)
            .field("signing_algorithm", &self.signing_algorithm)
            .finish()
    }
}

#[tonic::async_trait]
trait UpstreamPublisher: Send {
    async fn publish(&mut self, payload: Vec<u8>) -> Result<(), CompanionError>;
}

#[tonic::async_trait]
trait DownstreamConsumer: Send {
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError>;
    async fn complete(&mut self) -> Result<(), CompanionError>;
    async fn abandon(&mut self) -> Result<(), CompanionError>;
    async fn abandon_inflight(&mut self) -> Result<(), CompanionError>;
}

#[tonic::async_trait]
trait BrokerTransportFactory {
    type Publisher: UpstreamPublisher;
    type Consumer: DownstreamConsumer;

    async fn connect(
        &self,
        runtime_config: &RuntimeConfig,
        runtime_state: &RuntimeCredentialState,
    ) -> Result<(Self::Publisher, Self::Consumer), CompanionError>;
}

struct AzServiceBusTransportFactory;

struct AzServiceBusPublisher {
    _client: ServiceBusClient<azservicebus::core::BasicRetryPolicy>,
    sender: ServiceBusSender,
}

struct AzServiceBusConsumer {
    _client: ServiceBusClient<azservicebus::core::BasicRetryPolicy>,
    receiver: ServiceBusReceiver,
    inflight:
        Option<azservicebus::primitives::service_bus_received_message::ServiceBusReceivedMessage>,
}

#[cfg(unix)]
async fn connect_admin(socket_path: &str) -> Result<GatewayAdminClient<Channel>, CompanionError> {
    use hyper_util::rt::TokioIo;

    let socket_path = socket_path.to_owned();
    let channel = Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await?;
    Ok(GatewayAdminClient::new(channel))
}

#[cfg(windows)]
async fn connect_admin(pipe_name: &str) -> Result<GatewayAdminClient<Channel>, CompanionError> {
    use hyper_util::rt::TokioIo;
    use tokio::net::windows::named_pipe::ClientOptions;

    let pipe_name = pipe_name.to_owned();
    let channel = Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let name = pipe_name.clone();
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                let client = loop {
                    match ClientOptions::new().open(&name) {
                        Ok(client) => break client,
                        Err(err) if err.raw_os_error() == Some(231) => {}
                        Err(err) => return Err(err),
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "named pipe busy — timed out after 5s",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                };
                Ok::<_, std::io::Error>(TokioIo::new(client))
            }
        }))
        .await?;
    Ok(GatewayAdminClient::new(channel))
}

#[cfg(unix)]
async fn connect_connector(socket_path: &str) -> Result<Box<dyn AsyncIo>, CompanionError> {
    Ok(Box::new(
        tokio::net::UnixStream::connect(socket_path).await?,
    ))
}

#[cfg(windows)]
async fn connect_connector(pipe_name: &str) -> Result<Box<dyn AsyncIo>, CompanionError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return Ok(Box::new(client)),
            Err(err) if err.raw_os_error() == Some(231) => {}
            Err(err) => return Err(err.into()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CompanionError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "named pipe busy — timed out after 5s",
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!(
    "sonde-azure-companion requires Unix (UDS) or Windows (named pipes) — this platform is not supported"
);

fn validate_display_lines(lines: &[String]) -> Result<(), CompanionError> {
    if (1..=4).contains(&lines.len()) {
        Ok(())
    } else {
        Err(CompanionError::Config(
            "display-message requires between 1 and 4 lines".to_string(),
        ))
    }
}

fn require_non_empty(value: String, env_name: &str) -> Result<String, CompanionError> {
    if value.trim().is_empty() {
        Err(CompanionError::Config(format!(
            "{env_name} must be set and non-empty"
        )))
    } else {
        Ok(value.trim().to_string())
    }
}

fn load_runtime_config(state_dir: &Path) -> Result<RuntimeConfig, CompanionError> {
    let namespace_env = std::env::var("SONDE_AZURE_SERVICEBUS_NAMESPACE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let upstream_env = std::env::var("SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let downstream_env = std::env::var("SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let file_config = match load_service_bus_config_file(state_dir) {
        Ok(config) => Some(config),
        Err(CompanionError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };

    let namespace = if let Some(value) = namespace_env {
        require_non_empty(value, "SONDE_AZURE_SERVICEBUS_NAMESPACE")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(config.namespace.clone(), "service-bus.json namespace")?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_SERVICEBUS_NAMESPACE must be set and non-empty (or service-bus.json must exist in state dir)"
                .into(),
        ));
    };
    let upstream_queue = if let Some(value) = upstream_env {
        require_non_empty(value, "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(
            config.upstream_queue.clone(),
            "service-bus.json upstream_queue",
        )?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE must be set and non-empty (or service-bus.json must exist in state dir)"
                .into(),
        ));
    };
    let downstream_queue = if let Some(value) = downstream_env {
        require_non_empty(value, "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(
            config.downstream_queue.clone(),
            "service-bus.json downstream_queue",
        )?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE must be set and non-empty (or service-bus.json must exist in state dir)"
                .into(),
        ));
    };

    Ok(RuntimeConfig {
        namespace,
        upstream_queue,
        downstream_queue,
    })
}

fn load_service_bus_config_file(state_dir: &Path) -> Result<ServiceBusConfigFile, CompanionError> {
    let config_path = state_dir.join(SERVICE_BUS_CONFIG_FILENAME);
    let bytes = std::fs::read(&config_path)?;
    let config: ServiceBusConfigFile = serde_json::from_slice(&bytes)?;
    Ok(config)
}

fn prepare_staging_dir(state_dir: &Path) -> Result<PathBuf, CompanionError> {
    let staging_dir = state_dir.join(STAGING_DIR_NAME);
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir)?;
    }
    std::fs::create_dir_all(&staging_dir)?;
    Ok(staging_dir)
}

fn commit_staging(staging_dir: &Path, state_dir: &Path) -> Result<(), CompanionError> {
    let backup_dir = state_dir.join(format!("{STAGING_DIR_NAME}-backup"));
    if backup_dir.exists() {
        std::fs::remove_dir_all(&backup_dir)?;
    }
    std::fs::create_dir_all(&backup_dir)?;

    let staged_files: Vec<(PathBuf, PathBuf)> = std::fs::read_dir(staging_dir)?
        .map(|entry| {
            let entry = entry?;
            Ok((entry.path(), state_dir.join(entry.file_name())))
        })
        .collect::<Result<_, std::io::Error>>()?;

    let mut backed_up: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut committed: Vec<PathBuf> = Vec::new();

    let commit_result: Result<(), CompanionError> = (|| {
        for (_, dest) in &staged_files {
            if dest.exists() {
                let backup_path = backup_dir.join(dest.file_name().ok_or_else(|| {
                    CompanionError::Config(format!(
                        "staged destination had no file name: {}",
                        dest.display()
                    ))
                })?);
                std::fs::rename(dest, &backup_path)?;
                backed_up.push((backup_path, dest.clone()));
            }
        }

        for (src, dest) in &staged_files {
            std::fs::rename(src, dest)?;
            committed.push(dest.clone());
        }
        Ok(())
    })();

    if let Err(err) = commit_result {
        for dest in &committed {
            if dest.exists() {
                let _ = std::fs::remove_file(dest);
            }
        }
        for (backup_path, dest) in backed_up.iter().rev() {
            if backup_path.exists() {
                let _ = std::fs::rename(backup_path, dest);
            }
        }
        let _ = std::fs::remove_dir_all(&backup_dir);
        return Err(err);
    }

    let _ = std::fs::remove_dir_all(&backup_dir);
    let _ = std::fs::remove_dir(staging_dir);
    Ok(())
}

fn cleanup_staging(staging_dir: &Path) {
    let _ = std::fs::remove_dir_all(staging_dir);
}

fn generate_certificate(staging_dir: &Path) -> Result<(PathBuf, PathBuf, String), CompanionError> {
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};

    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| {
        CompanionError::Config(format!("failed to generate ECDSA P-256 key pair: {e}"))
    })?;

    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| CompanionError::Config(format!("failed to create certificate params: {e}")))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "sonde-azure-companion");
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(730);

    let cert = params.self_signed(&key_pair).map_err(|e| {
        CompanionError::Config(format!("failed to generate self-signed certificate: {e}"))
    })?;

    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();

    let cert_path = staging_dir.join(CERT_PEM_FILENAME);
    let key_path = staging_dir.join(KEY_PEM_FILENAME);

    std::fs::write(&cert_path, cert_pem.as_bytes())?;
    write_private_key_pem(&key_path, &key_pem)?;

    let cert_der = cert.der().to_vec();
    let cert_base64 = base64::engine::general_purpose::STANDARD.encode(&cert_der);

    Ok((cert_path, key_path, cert_base64))
}

fn write_private_key_pem(path: &Path, pem: &str) -> Result<(), CompanionError> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(pem.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, pem.as_bytes())?;
        Ok(())
    }
}

fn service_principal_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME)
}

fn resolve_state_relative_path(state_dir: &Path, value: &str) -> Result<PathBuf, CompanionError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Err(CompanionError::Config(format!(
            "service principal path `{value}` must be relative to the state directory"
        )));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(CompanionError::Config(format!(
            "service principal path `{value}` must stay within the state directory"
        )));
    }
    Ok(state_dir.join(path))
}

fn canonicalize_state_file_path(
    state_dir: &Path,
    path: &Path,
    value: &str,
) -> Result<PathBuf, CompanionError> {
    let canonical_state_dir = state_dir.canonicalize()?;
    let canonical_path = path.canonicalize()?;
    if !canonical_path.starts_with(&canonical_state_dir) {
        return Err(CompanionError::Config(format!(
            "service principal path `{value}` resolved outside the state directory"
        )));
    }
    Ok(canonical_path)
}

fn load_runtime_credential_state(
    state_dir: &Path,
) -> Result<RuntimeCredentialState, CompanionError> {
    let state_path = service_principal_state_path(state_dir);
    let state_bytes = match std::fs::read(&state_path) {
        Ok(state_bytes) => state_bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CompanionError::Config(format!(
                "service principal state file not found: {}",
                state_path.display()
            )));
        }
        Err(err) => return Err(err.into()),
    };
    let state: ServicePrincipalStateFile = serde_json::from_slice(&state_bytes)?;
    let tenant_id = require_non_empty(state.tenant_id, "service principal tenant_id")?;
    let client_id = require_non_empty(state.client_id, "service principal client_id")?;
    let certificate_path_value =
        require_non_empty(state.certificate_path, "service principal certificate_path")?;
    let certificate_path = resolve_state_relative_path(state_dir, &certificate_path_value)?;
    if !certificate_path.is_file() {
        return Err(CompanionError::Config(format!(
            "service principal certificate file not found: {}",
            certificate_path.display()
        )));
    }
    let certificate_path =
        canonicalize_state_file_path(state_dir, &certificate_path, &certificate_path_value)?;
    let private_key_path_value =
        require_non_empty(state.private_key_path, "service principal private_key_path")?;
    let private_key_path = resolve_state_relative_path(state_dir, &private_key_path_value)?;
    if !private_key_path.is_file() {
        return Err(CompanionError::Config(format!(
            "service principal private key file not found: {}",
            private_key_path.display()
        )));
    }
    let private_key_path =
        canonicalize_state_file_path(state_dir, &private_key_path, &private_key_path_value)?;
    Ok(RuntimeCredentialState {
        tenant_id,
        client_id,
        certificate_path,
        private_key_path,
    })
}

fn check_runtime_ready(
    state_dir: &Path,
) -> Result<(RuntimeConfig, RuntimeCredentialState), CompanionError> {
    let runtime_config = load_runtime_config(state_dir)?;
    let runtime_state = load_runtime_credential_state(state_dir)?;
    let _ = load_certificate_thumbprint(&runtime_state.certificate_path)?;
    let _ = load_signing_key(&runtime_state.private_key_path)?;
    validate_certificate_matches_private_key(
        &runtime_state.certificate_path,
        &runtime_state.private_key_path,
    )?;
    Ok((runtime_config, runtime_state))
}

fn load_certificate_thumbprint(certificate_path: &Path) -> Result<String, CompanionError> {
    let certificate_file = std::fs::File::open(certificate_path)?;
    let mut reader = std::io::BufReader::new(certificate_file);
    let certificate = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()?
        .ok_or_else(|| {
            CompanionError::Config(format!(
                "service principal certificate file did not contain a PEM certificate: {}",
                certificate_path.display()
            ))
        })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(certificate.as_ref())))
}

fn load_certificate_subject_public_key_info(
    certificate_path: &Path,
) -> Result<Vec<u8>, CompanionError> {
    let certificate_file = std::fs::File::open(certificate_path)?;
    let mut reader = std::io::BufReader::new(certificate_file);
    let certificate = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()?
        .ok_or_else(|| {
            CompanionError::Config(format!(
                "service principal certificate file did not contain a PEM certificate: {}",
                certificate_path.display()
            ))
        })?;
    let certificate = Certificate::from_der(certificate.as_ref()).map_err(|err| {
        CompanionError::Config(format!(
            "service principal certificate file did not contain a parseable X.509 certificate: {} ({err})",
            certificate_path.display()
        ))
    })?;
    certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|err| {
            CompanionError::Config(format!(
                "failed to encode service principal certificate public key: {} ({err})",
                certificate_path.display()
            ))
        })
}

fn load_signing_key(private_key_path: &Path) -> Result<(Algorithm, EncodingKey), CompanionError> {
    let private_key_pem = std::fs::read(private_key_path)?;

    if let Ok(key) = EncodingKey::from_rsa_pem(&private_key_pem) {
        return Ok((Algorithm::RS256, key));
    }
    if let Ok(key) = EncodingKey::from_ec_pem(&private_key_pem) {
        ensure_p256_private_key(&private_key_pem, private_key_path)?;
        return Ok((Algorithm::ES256, key));
    }
    if let Ok(key) = EncodingKey::from_ed_pem(&private_key_pem) {
        return Ok((Algorithm::EdDSA, key));
    }

    Err(CompanionError::Config(format!(
        "service principal private key file must contain a PEM-encoded RSA, EC, or EdDSA private key: {}",
        private_key_path.display()
    )))
}

fn ensure_p256_private_key(
    private_key_pem: &[u8],
    private_key_path: &Path,
) -> Result<(), CompanionError> {
    let mut reader = std::io::BufReader::new(private_key_pem);
    let private_key = rustls_pemfile::read_one(&mut reader)?.ok_or_else(|| {
        CompanionError::Config(format!(
            "service principal private key file did not contain a PEM private key: {}",
            private_key_path.display()
        ))
    })?;

    match private_key {
        rustls_pemfile::Item::Pkcs8Key(key) => {
            p256::SecretKey::from_pkcs8_der(key.secret_pkcs8_der()).map_err(|_| {
                CompanionError::Config(format!(
                    "service principal EC private key must use the P-256 curve for ES256 assertions: {}",
                    private_key_path.display()
                ))
            })?;
        }
        rustls_pemfile::Item::Sec1Key(key) => {
            p256::SecretKey::from_sec1_der(key.secret_sec1_der()).map_err(|_| {
                CompanionError::Config(format!(
                    "service principal EC private key must use the P-256 curve for ES256 assertions: {}",
                    private_key_path.display()
                ))
            })?;
        }
        _ => {
            return Err(CompanionError::Config(format!(
                "service principal EC private key must be encoded as PKCS#8 or SEC1 PEM: {}",
                private_key_path.display()
            )));
        }
    }

    Ok(())
}

fn encode_public_key_der<T>(
    public_key: &T,
    private_key_path: &Path,
) -> Result<Vec<u8>, CompanionError>
where
    T: EncodePublicKey,
{
    public_key
        .to_public_key_der()
        .map(|der| der.as_ref().to_vec())
        .map_err(|err| {
            CompanionError::Config(format!(
                "failed to encode service principal public key from private key: {} ({err})",
                private_key_path.display()
            ))
        })
}

fn load_private_key_subject_public_key_info(
    private_key_path: &Path,
) -> Result<Vec<u8>, CompanionError> {
    let private_key_pem = std::fs::read(private_key_path)?;
    let mut reader = std::io::BufReader::new(private_key_pem.as_slice());
    let private_key = rustls_pemfile::read_one(&mut reader)?.ok_or_else(|| {
        CompanionError::Config(format!(
            "service principal private key file did not contain a PEM private key: {}",
            private_key_path.display()
        ))
    })?;

    match private_key {
        rustls_pemfile::Item::Pkcs1Key(key) => {
            let private_key =
                rsa::RsaPrivateKey::from_pkcs1_der(key.secret_pkcs1_der()).map_err(|err| {
                    CompanionError::Config(format!(
                        "service principal RSA private key could not be parsed: {} ({err})",
                        private_key_path.display()
                    ))
                })?;
            encode_public_key_der(&private_key.to_public_key(), private_key_path)
        }
        rustls_pemfile::Item::Pkcs8Key(key) => {
            let der = key.secret_pkcs8_der();
            if let Ok(private_key) = rsa::RsaPrivateKey::from_pkcs8_der(der) {
                return encode_public_key_der(&private_key.to_public_key(), private_key_path);
            }
            if let Ok(private_key) = p256::SecretKey::from_pkcs8_der(der) {
                return encode_public_key_der(&private_key.public_key(), private_key_path);
            }
            if let Ok(private_key) = ed25519_dalek::SigningKey::from_pkcs8_der(der) {
                return encode_public_key_der(&private_key.verifying_key(), private_key_path);
            }
            Err(CompanionError::Config(format!(
                "service principal private key file must contain a PEM-encoded RSA, EC, or EdDSA private key: {}",
                private_key_path.display()
            )))
        }
        rustls_pemfile::Item::Sec1Key(key) => {
            let private_key =
                p256::SecretKey::from_sec1_der(key.secret_sec1_der()).map_err(|err| {
                    CompanionError::Config(format!(
                        "service principal EC private key could not be parsed: {} ({err})",
                        private_key_path.display()
                    ))
                })?;
            encode_public_key_der(&private_key.public_key(), private_key_path)
        }
        _ => Err(CompanionError::Config(format!(
            "service principal private key file must contain a PEM private key: {}",
            private_key_path.display()
        ))),
    }
}

fn validate_certificate_matches_private_key(
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<(), CompanionError> {
    let certificate_public_key = load_certificate_subject_public_key_info(certificate_path)?;
    let private_key_public_key = load_private_key_subject_public_key_info(private_key_path)?;
    if certificate_public_key != private_key_public_key {
        return Err(CompanionError::Config(format!(
            "service principal certificate public key does not match private key: {} / {}",
            certificate_path.display(),
            private_key_path.display()
        )));
    }
    Ok(())
}

fn parse_bicep_outputs(
    json: &str,
) -> Result<(ServicePrincipalStateFile, ServiceBusConfigFile), CompanionError> {
    let outputs: BicepOutputs = serde_json::from_str(json).map_err(|e| {
        CompanionError::Config(format!("failed to parse Bicep deployment outputs: {e}"))
    })?;

    let bootstrap_values: BicepBootstrapValues =
        serde_json::from_value(outputs.companion_bootstrap_values.value).map_err(|e| {
            CompanionError::Config(format!("failed to parse companionBootstrapValues: {e}"))
        })?;

    let sp = ServicePrincipalStateFile {
        tenant_id: bootstrap_values
            .tenant_id
            .value
            .as_str()
            .ok_or_else(|| CompanionError::Config("tenantId must be a string".into()))?
            .to_string(),
        client_id: bootstrap_values
            .client_id
            .value
            .as_str()
            .ok_or_else(|| CompanionError::Config("clientId must be a string".into()))?
            .to_string(),
        certificate_path: CERT_PEM_FILENAME.to_string(),
        private_key_path: KEY_PEM_FILENAME.to_string(),
    };

    let sb = ServiceBusConfigFile {
        namespace: bootstrap_values
            .service_bus_namespace
            .value
            .as_str()
            .ok_or_else(|| CompanionError::Config("serviceBusNamespace must be a string".into()))?
            .to_string(),
        upstream_queue: bootstrap_values
            .upstream_queue
            .value
            .as_str()
            .ok_or_else(|| CompanionError::Config("upstreamQueue must be a string".into()))?
            .to_string(),
        downstream_queue: bootstrap_values
            .downstream_queue
            .value
            .as_str()
            .ok_or_else(|| CompanionError::Config("downstreamQueue must be a string".into()))?
            .to_string(),
    };

    Ok((sp, sb))
}

fn build_az_bootstrap_script() -> String {
    r#"set -eu
az login --use-device-code --output none >&2
if [ -n "${SONDE_AZURE_SUBSCRIPTION_ID:-}" ]; then
    az account set --subscription "$SONDE_AZURE_SUBSCRIPTION_ID" >&2
fi
az deployment sub create \
    --location "$SONDE_AZURE_LOCATION" \
    --template-file /bicep/main.bicep \
    --parameters companionCertificateBase64="$COMPANION_CERT_BASE64" \
    --parameters location="$SONDE_AZURE_LOCATION" \
    --parameters project_name="$SONDE_AZURE_PROJECT_NAME" \
    --query 'properties.outputs' \
    --output json"#
        .to_string()
}

fn device_code_regex() -> &'static Regex {
    static DEVICE_CODE_RE: OnceLock<Regex> = OnceLock::new();
    DEVICE_CODE_RE.get_or_init(|| {
        Regex::new(r"enter the code ([A-Z0-9-]+) to authenticate").expect("valid device code regex")
    })
}

fn extract_device_code(stderr_buffer: &str) -> Option<String> {
    device_code_regex()
        .captures(stderr_buffer)
        .and_then(|captures| captures.get(1))
        .map(|code| code.as_str().to_string())
}

fn build_container_env(cert_base64: &str, args: &BootstrapArgs) -> Vec<String> {
    let mut env = vec![
        format!("SONDE_AZURE_LOCATION={}", args.azure_location),
        format!("SONDE_AZURE_PROJECT_NAME={}", args.azure_project_name),
        format!("COMPANION_CERT_BASE64={cert_base64}"),
    ];
    if let Some(sub_id) = &args.azure_subscription_id {
        env.push(format!("SONDE_AZURE_SUBSCRIPTION_ID={sub_id}"));
    }
    env
}

fn downstream_body_to_connector_payload(body: &[u8]) -> Result<Vec<u8>, CompanionError> {
    if body.len() > CONNECTOR_MAX_FRAME_LENGTH {
        return Err(CompanionError::Config(format!(
            "downstream Service Bus message body length {} exceeds connector max frame length {}",
            body.len(),
            CONNECTOR_MAX_FRAME_LENGTH
        )));
    }
    Ok(body.to_vec())
}

fn build_service_bus_credential(
    runtime_state: &RuntimeCredentialState,
) -> Result<Arc<dyn TokenCredential>, CompanionError> {
    let certificate_thumbprint = load_certificate_thumbprint(&runtime_state.certificate_path)?;
    let (signing_algorithm, signing_key) = load_signing_key(&runtime_state.private_key_path)?;
    let token_endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        runtime_state.tenant_id
    );
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(TOKEN_HTTP_CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(TOKEN_HTTP_TIMEOUT_SECS))
        .build()?;
    Ok(Arc::new(ClientAssertionCredential {
        client_id: runtime_state.client_id.clone(),
        token_endpoint,
        signing_algorithm,
        signing_key,
        certificate_thumbprint,
        http_client,
        cached_token: Mutex::new(None),
    }))
}

impl ClientAssertionCredential {
    fn build_client_assertion(&self) -> Result<String, CompanionError> {
        let now = OffsetDateTime::now_utc();
        let now_unix = now.unix_timestamp();
        let claims = ClientAssertionClaims {
            aud: self.token_endpoint.clone(),
            iss: self.client_id.clone(),
            sub: self.client_id.clone(),
            jti: Uuid::new_v4().to_string(),
            nbf: now_unix,
            iat: now_unix,
            exp: (now + TimeDuration::seconds(CLIENT_ASSERTION_LIFETIME_SECS)).unix_timestamp(),
        };
        let mut header = Header::new(self.signing_algorithm);
        header.x5t_s256 = Some(self.certificate_thumbprint.clone());
        Ok(jsonwebtoken::encode(&header, &claims, &self.signing_key)?)
    }

    async fn fetch_token(&self, scope: &str) -> azure_core::Result<AccessToken> {
        let client_assertion = self
            .build_client_assertion()
            .map_err(|err| azure_core::Error::new(ErrorKind::Credential, err))?;
        let response = self
            .http_client
            .post(&self.token_endpoint)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", scope),
                ("grant_type", "client_credentials"),
                ("client_assertion_type", CLIENT_ASSERTION_TYPE),
                ("client_assertion", client_assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|err| azure_core::Error::new(ErrorKind::Credential, err))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|err| format!("<failed to read token error response body: {err}>"));
            return Err(azure_core::Error::new(
                ErrorKind::Credential,
                std::io::Error::other(format!("token endpoint returned {status}: {body}")),
            ));
        }
        let response: OAuthTokenResponse = response
            .json()
            .await
            .map_err(|err| azure_core::Error::new(ErrorKind::Credential, err))?;
        let expires_on = OffsetDateTime::now_utc() + TimeDuration::seconds(response.expires_in);
        Ok(AccessToken::new(response.access_token, expires_on))
    }
}

#[tonic::async_trait]
impl TokenCredential for ClientAssertionCredential {
    async fn get_token(
        &self,
        scopes: &[&str],
        _options: Option<TokenRequestOptions>,
    ) -> azure_core::Result<AccessToken> {
        if scopes.is_empty() {
            return Err(azure_core::Error::message(
                ErrorKind::Credential,
                "missing Azure token scope",
            ));
        }
        let scope = scopes.join(" ");
        let refresh_after =
            OffsetDateTime::now_utc() + TimeDuration::seconds(ACCESS_TOKEN_REFRESH_MARGIN_SECS);
        {
            let cached_token = self.cached_token.lock().await;
            if let Some(cached_token) = cached_token.as_ref() {
                if cached_token.scope == scope && cached_token.token.expires_on > refresh_after {
                    return Ok(cached_token.token.clone());
                }
            }
        }

        let token = self.fetch_token(&scope).await?;
        let mut cached_token = self.cached_token.lock().await;
        *cached_token = Some(CachedAccessToken {
            scope,
            token: token.clone(),
        });
        Ok(token)
    }
}

#[tonic::async_trait]
impl UpstreamPublisher for AzServiceBusPublisher {
    async fn publish(&mut self, payload: Vec<u8>) -> Result<(), CompanionError> {
        let message = ServiceBusMessage::new(payload);
        self.sender.send_message(message).await?;
        Ok(())
    }
}

#[tonic::async_trait]
impl DownstreamConsumer for AzServiceBusConsumer {
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
        if self.inflight.is_some() {
            return Err(CompanionError::Config(
                "cannot receive a new downstream message while another message is still inflight"
                    .to_string(),
            ));
        }
        let message = self
            .receiver
            .receive_message_with_max_wait_time(Some(Duration::from_secs(
                DEFAULT_DOWNSTREAM_WAIT_SECS,
            )))
            .await?;
        if let Some(message) = message {
            self.inflight = Some(message);
            let payload = self
                .inflight
                .as_ref()
                .expect("inflight message must exist immediately after receive")
                .body()
                .map_err(|err| {
                    CompanionError::Config(format!(
                        "downstream Service Bus message body was not raw binary data: {err}"
                    ))
                })
                .and_then(downstream_body_to_connector_payload);
            match payload {
                Ok(payload) => Ok(Some(payload)),
                Err(err) => {
                    if let Err(abandon_err) = self.abandon().await {
                        eprintln!(
                            "failed to abandon downstream Service Bus message after body decode error: {abandon_err}"
                        );
                    }
                    Err(err)
                }
            }
        } else {
            Ok(None)
        }
    }

    async fn complete(&mut self) -> Result<(), CompanionError> {
        let inflight = self.inflight.as_ref().ok_or_else(|| {
            CompanionError::Config("no inflight downstream message to complete".to_string())
        })?;
        self.receiver.complete_message(inflight).await?;
        self.inflight = None;
        Ok(())
    }

    async fn abandon(&mut self) -> Result<(), CompanionError> {
        let inflight = self.inflight.as_ref().ok_or_else(|| {
            CompanionError::Config("no inflight downstream message to abandon".to_string())
        })?;
        self.receiver.abandon_message(inflight, None).await?;
        self.inflight = None;
        Ok(())
    }

    async fn abandon_inflight(&mut self) -> Result<(), CompanionError> {
        if self.inflight.is_none() {
            return Ok(());
        }
        self.abandon().await
    }
}

#[tonic::async_trait]
impl BrokerTransportFactory for AzServiceBusTransportFactory {
    type Publisher = AzServiceBusPublisher;
    type Consumer = AzServiceBusConsumer;

    async fn connect(
        &self,
        runtime_config: &RuntimeConfig,
        runtime_state: &RuntimeCredentialState,
    ) -> Result<(Self::Publisher, Self::Consumer), CompanionError> {
        let credential = build_service_bus_credential(runtime_state)?;

        let mut sender_client = ServiceBusClient::new_from_token_credential(
            runtime_config.namespace.clone(),
            Arc::clone(&credential),
            ServiceBusClientOptions::default(),
        )
        .await?;
        let sender = sender_client
            .create_sender(
                runtime_config.upstream_queue.clone(),
                ServiceBusSenderOptions::default(),
            )
            .await?;

        let mut receiver_client = ServiceBusClient::new_from_token_credential(
            runtime_config.namespace.clone(),
            credential,
            ServiceBusClientOptions::default(),
        )
        .await?;
        let receiver = receiver_client
            .create_receiver_for_queue(
                runtime_config.downstream_queue.clone(),
                ServiceBusReceiverOptions::default(),
            )
            .await?;

        Ok((
            AzServiceBusPublisher {
                _client: sender_client,
                sender,
            },
            AzServiceBusConsumer {
                _client: receiver_client,
                receiver,
                inflight: None,
            },
        ))
    }
}

async fn read_framed<T>(reader: &mut T) -> Result<Option<Vec<u8>>, CompanionError>
where
    T: AsyncRead + Unpin,
{
    let mut len = [0u8; 4];
    let mut read_len = 0usize;
    while read_len < len.len() {
        match reader.read(&mut len[read_len..]).await {
            Ok(0) if read_len == 0 => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connector EOF while reading frame length prefix",
                )
                .into())
            }
            Ok(n) => read_len += n,
            Err(err) => return Err(err.into()),
        }
    }
    let len = usize::try_from(u32::from_be_bytes(len)).map_err(|_| {
        CompanionError::Config("connector frame length did not fit in usize".to_string())
    })?;
    if len > CONNECTOR_MAX_FRAME_LENGTH {
        return Err(CompanionError::Config(format!(
            "connector frame length {len} exceeds max {}",
            CONNECTOR_MAX_FRAME_LENGTH
        )));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_framed<T>(writer: &mut T, payload: &[u8]) -> Result<(), CompanionError>
where
    T: AsyncWrite + Unpin,
{
    if payload.len() > CONNECTOR_MAX_FRAME_LENGTH {
        return Err(CompanionError::Config(format!(
            "connector payload length {} exceeds max {}",
            payload.len(),
            CONNECTOR_MAX_FRAME_LENGTH
        )));
    }
    let len = u32::try_from(payload.len()).map_err(|_| {
        CompanionError::Config("connector payload exceeded 32-bit framed length".to_string())
    })?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn pump_upstream_once<T, P>(reader: &mut T, publisher: &mut P) -> Result<bool, CompanionError>
where
    T: AsyncRead + Unpin,
    P: UpstreamPublisher,
{
    match read_framed(reader).await? {
        Some(payload) => {
            publisher.publish(payload).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

async fn pump_downstream_once<T, C>(writer: &mut T, consumer: &mut C) -> Result<(), CompanionError>
where
    T: AsyncWrite + Unpin,
    C: DownstreamConsumer,
{
    let Some(payload) = consumer.receive().await? else {
        return Ok(());
    };

    if let Err(err) = write_framed(writer, &payload).await {
        if let Err(abandon_err) = consumer.abandon().await {
            eprintln!("failed to abandon downstream Service Bus message after connector write error: {abandon_err}");
        }
        return Err(err);
    }

    if let Err(err) = consumer.complete().await {
        if let Err(abandon_err) = consumer.abandon().await {
            eprintln!(
                "failed to abandon downstream Service Bus message after completion error: {abandon_err}"
            );
        }
        return Err(err);
    }
    Ok(())
}

async fn bridge_runtime<T, P, C>(
    stream: T,
    mut publisher: P,
    mut consumer: C,
) -> Result<(), CompanionError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    P: UpstreamPublisher + 'static,
    C: DownstreamConsumer + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let upstream = async move {
        while pump_upstream_once(&mut reader, &mut publisher).await? {}
        Ok::<(), CompanionError>(())
    };
    tokio::pin!(upstream);

    loop {
        tokio::select! {
            result = &mut upstream => {
                if let Err(abandon_err) = consumer.abandon_inflight().await {
                    eprintln!("failed to abandon downstream Service Bus message during bridge shutdown: {abandon_err}");
                }
                return result;
            }
            result = pump_downstream_once(&mut writer, &mut consumer) => {
                if let Err(err) = result {
                    if let Err(abandon_err) = consumer.abandon_inflight().await {
                        eprintln!("failed to abandon downstream Service Bus message after downstream error: {abandon_err}");
                    }
                    return Err(err);
                }
            }
        }
    }
}

async fn run_with_factory<F>(
    connector_socket: &str,
    state_dir: &Path,
    factory: &F,
) -> Result<(), CompanionError>
where
    F: BrokerTransportFactory,
    F::Publisher: 'static,
    F::Consumer: 'static,
{
    let (runtime_config, runtime_state) = check_runtime_ready(state_dir)?;
    run_checked_with_factory(connector_socket, &runtime_config, &runtime_state, factory).await
}

async fn run_checked_with_factory<F>(
    connector_socket: &str,
    runtime_config: &RuntimeConfig,
    runtime_state: &RuntimeCredentialState,
    factory: &F,
) -> Result<(), CompanionError>
where
    F: BrokerTransportFactory,
    F::Publisher: 'static,
    F::Consumer: 'static,
{
    let (publisher, consumer) = factory.connect(runtime_config, runtime_state).await?;
    let stream = connect_connector(connector_socket).await?;
    eprintln!(
        "connected to gateway connector at {connector_socket} and Azure Service Bus namespace {}",
        runtime_config.namespace
    );
    bridge_runtime(stream, publisher, consumer).await
}

async fn run(connector_socket: &str, state_dir: &Path) -> Result<(), CompanionError> {
    run_with_factory(connector_socket, state_dir, &AzServiceBusTransportFactory).await
}

async fn copy_files_to_container(
    docker: &Docker,
    container_id: &str,
    staging_dir: &Path,
) -> Result<(), CompanionError> {
    let mut archive = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut archive);
        let bicep_path = Path::new(BUNDLED_BICEP_PATH);
        if !bicep_path.is_dir() {
            return Err(CompanionError::Config(format!(
                "bundled Bicep path not found: {}",
                bicep_path.display()
            )));
        }
        builder.append_dir_all("bicep", bicep_path).map_err(|e| {
            CompanionError::Config(format!("failed to add Bicep files to archive: {e}"))
        })?;

        let cert_path = staging_dir.join(CERT_PEM_FILENAME);
        if !cert_path.is_file() {
            return Err(CompanionError::Config(format!(
                "generated certificate file not found: {}",
                cert_path.display()
            )));
        }
        builder
            .append_path_with_name(&cert_path, format!("cert/{CERT_PEM_FILENAME}"))
            .map_err(|e| {
                CompanionError::Config(format!("failed to add certificate to archive: {e}"))
            })?;
        builder
            .finish()
            .map_err(|e| CompanionError::Config(format!("failed to finalize tar archive: {e}")))?;
    }

    let upload_options = UploadToContainerOptionsBuilder::default().path("/").build();

    docker
        .upload_to_container(
            container_id,
            Some(upload_options),
            body_full(archive.into()),
        )
        .await
        .map_err(|e| CompanionError::Config(format!("failed to copy files to container: {e}")))?;

    Ok(())
}

async fn stream_container_output(
    docker: &Docker,
    container_id: &str,
    admin_socket: &str,
) -> Result<String, CompanionError> {
    let log_opts = LogsOptionsBuilder::default()
        .follow(true)
        .stdout(true)
        .stderr(true)
        .build();

    let mut logs = docker.logs(container_id, Some(log_opts));
    let mut stdout_buffer = String::new();
    let mut stderr_buffer = String::new();
    let mut device_code_displayed = false;

    while let Some(result) = logs.next().await {
        match result {
            Ok(LogOutput::StdOut { message }) => {
                let text = String::from_utf8_lossy(&message);
                stdout_buffer.push_str(&text);
            }
            Ok(LogOutput::StdErr { message }) => {
                let text = String::from_utf8_lossy(&message);
                eprint!("{text}");
                stderr_buffer.push_str(&text);
                const MAX_STDERR_BUFFER_LEN: usize = 4096;
                if stderr_buffer.len() > MAX_STDERR_BUFFER_LEN {
                    let trim_start = stderr_buffer.len() - MAX_STDERR_BUFFER_LEN;
                    stderr_buffer.drain(..trim_start);
                }

                if !device_code_displayed {
                    if let Some(device_code) = extract_device_code(&stderr_buffer) {
                        eprintln!("Detected device code: {device_code}");
                        if let Err(e) = display_message(
                            admin_socket,
                            vec!["Azure login".to_string(), device_code],
                        )
                        .await
                        {
                            return Err(CompanionError::Config(format!(
                                "failed to display device code on modem: {e}"
                            )));
                        }
                        device_code_displayed = true;
                    }
                }
            }
            Ok(LogOutput::Console { message }) => {
                let text = String::from_utf8_lossy(&message);
                stdout_buffer.push_str(&text);
            }
            Ok(LogOutput::StdIn { .. }) => {}
            Err(e) => {
                return Err(CompanionError::Config(format!(
                    "failed to read container output: {e}"
                )));
            }
        }
    }

    Ok(stdout_buffer)
}

async fn run_bootstrap_deployment(
    admin_socket: &str,
    staging_dir: &Path,
    cert_base64: &str,
    args: &BootstrapArgs,
) -> Result<(ServicePrincipalStateFile, ServiceBusConfigFile), CompanionError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| CompanionError::Config(format!("failed to connect to Docker daemon: {e}")))?;

    eprintln!("Pulling Azure CLI container image...");
    let pull_opts = CreateImageOptionsBuilder::default()
        .from_image(AZURE_CLI_IMAGE)
        .build();
    let mut pull_stream = docker.create_image(Some(pull_opts), None, None);
    while let Some(result) = pull_stream.next().await {
        result
            .map_err(|e| CompanionError::Config(format!("failed to pull Azure CLI image: {e}")))?;
    }

    let bootstrap_script = build_az_bootstrap_script();
    let env_vars = build_container_env(cert_base64, args);
    let container_name = format!("sonde-bootstrap-{}", Uuid::new_v4());
    let container = docker
        .create_container(
            Some(
                CreateContainerOptionsBuilder::default()
                    .name(&container_name)
                    .build(),
            ),
            ContainerCreateBody {
                image: Some(AZURE_CLI_IMAGE.to_string()),
                cmd: Some(vec!["sh".to_string(), "-c".to_string(), bootstrap_script]),
                env: Some(env_vars),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            CompanionError::Config(format!("failed to create Azure CLI container: {e}"))
        })?;
    let container_id = container.id;

    let bootstrap_result = async {
        copy_files_to_container(&docker, &container_id, staging_dir).await?;
        docker
            .start_container(&container_id, None)
            .await
            .map_err(|e| {
                CompanionError::Config(format!("failed to start Azure CLI container: {e}"))
            })?;

        let stdout_output = stream_container_output(&docker, &container_id, admin_socket).await?;
        let wait_options = WaitContainerOptionsBuilder::default()
            .condition("not-running")
            .build();
        let mut wait_stream = docker.wait_container(&container_id, Some(wait_options));
        let exit_result = wait_stream.next().await;

        match exit_result {
            Some(Ok(result)) if result.status_code == 0 => {}
            Some(Ok(result)) => {
                return Err(CompanionError::Config(format!(
                    "Azure CLI container exited with non-zero status: {}",
                    result.status_code
                )));
            }
            Some(Err(e)) => {
                return Err(CompanionError::Config(format!(
                    "failed to wait for Azure CLI container: {e}"
                )));
            }
            None => {
                return Err(CompanionError::Config(
                    "Azure CLI container wait stream ended unexpectedly".into(),
                ));
            }
        }

        parse_bicep_outputs(&stdout_output)
    }
    .await;

    let remove_options = RemoveContainerOptionsBuilder::default().force(true).build();
    let _ = docker
        .remove_container(&container_id, Some(remove_options))
        .await;

    bootstrap_result
}

async fn display_message(admin_socket: &str, lines: Vec<String>) -> Result<(), CompanionError> {
    validate_display_lines(&lines)?;
    let mut client = connect_admin(admin_socket).await?;
    client
        .show_modem_display_message(ShowModemDisplayMessageRequest { lines })
        .await?;
    Ok(())
}

async fn display_progress(admin_socket: &str, msg: &str) -> Result<(), CompanionError> {
    display_message(admin_socket, vec![msg.to_string()]).await
}

async fn report_bootstrap_failure(
    admin_socket: &str,
    staging_dir: &Path,
    err: CompanionError,
) -> Result<(), CompanionError> {
    cleanup_staging(staging_dir);
    display_progress(admin_socket, "Bootstrap failed").await?;
    Err(err)
}

async fn bootstrap(
    admin_socket: &str,
    state_dir: &Path,
    args: BootstrapArgs,
) -> Result<(), CompanionError> {
    std::fs::create_dir_all(state_dir)?;

    let staging_dir = prepare_staging_dir(state_dir)?;

    if let Err(err) = display_progress(admin_socket, "Generating cert...").await {
        cleanup_staging(&staging_dir);
        return Err(err);
    }
    eprintln!("Generating ECDSA P-256 self-signed certificate");
    let (_cert_path, _key_path, cert_base64) = match generate_certificate(&staging_dir) {
        Ok(result) => result,
        Err(e) => return report_bootstrap_failure(admin_socket, &staging_dir, e).await,
    };

    display_progress(admin_socket, "Authenticating...").await?;
    eprintln!("Starting Azure CLI container for device-code auth and Bicep deployment");
    let (sp_state, sb_config) =
        match run_bootstrap_deployment(admin_socket, &staging_dir, &cert_base64, &args).await {
            Ok(result) => result,
            Err(e) => return report_bootstrap_failure(admin_socket, &staging_dir, e).await,
        };

    display_progress(admin_socket, "Writing config...").await?;
    eprintln!("Writing runtime artifacts to state volume");

    let sp_path = staging_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME);
    let sp_json = serde_json::to_string_pretty(&sp_state)?;
    std::fs::write(&sp_path, sp_json.as_bytes())?;

    let sb_path = staging_dir.join(SERVICE_BUS_CONFIG_FILENAME);
    let sb_json = serde_json::to_string_pretty(&sb_config)?;
    std::fs::write(&sb_path, sb_json.as_bytes())?;

    if let Err(e) = commit_staging(&staging_dir, state_dir) {
        return report_bootstrap_failure(admin_socket, &staging_dir, e).await;
    }

    display_progress(admin_socket, "Bootstrap complete").await?;
    eprintln!("Bootstrap completed successfully");

    Ok(())
}

async fn run_cli() -> Result<(), CompanionError> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(&cli.connector_socket, &cli.state_dir).await?,
        Command::Bootstrap(args) => bootstrap(&cli.admin_socket, &cli.state_dir, args).await?,
        Command::DisplayMessage { lines } => display_message(&cli.admin_socket, lines).await?,
        Command::CheckRuntimeReady => {
            check_runtime_ready(&cli.state_dir)?;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(err) = run_cli().await {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_az_bootstrap_script, check_runtime_ready, cleanup_staging, commit_staging,
        downstream_body_to_connector_payload, extract_device_code, generate_certificate,
        load_runtime_config, load_signing_key, parse_bicep_outputs, prepare_staging_dir,
        pump_downstream_once, pump_upstream_once, read_framed, resolve_state_relative_path,
        validate_certificate_matches_private_key, validate_display_lines, write_framed,
        ClientAssertionCredential, CompanionError, DownstreamConsumer, RuntimeConfig,
        RuntimeCredentialState, ServiceBusConfigFile, ServicePrincipalStateFile, UpstreamPublisher,
        CERT_PEM_FILENAME, CONNECTOR_MAX_FRAME_LENGTH, KEY_PEM_FILENAME,
        SERVICE_BUS_CONFIG_FILENAME,
    };
    use azure_core::credentials::TokenCredential;
    use base64::Engine as _;
    use jsonwebtoken::{Algorithm, EncodingKey};
    use std::collections::VecDeque;
    use std::path::Path;
    use std::path::PathBuf;
    #[cfg(unix)]
    use std::pin::Pin;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::sync::Mutex as StdMutex;
    #[cfg(unix)]
    use std::task::{Context, Poll, Waker};
    use tempfile::TempDir;
    use tokio::io::duplex;
    #[cfg(unix)]
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[cfg(unix)]
    use tokio::sync::{Mutex, Notify};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use x509_cert::der::Decode;
    use x509_cert::Certificate;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn write_service_principal_state(temp: &TempDir) -> PathBuf {
        let cert_path = temp.path().join("client-cert.pem");
        let key_path = temp.path().join("client-key.pem");
        std::fs::write(
            &cert_path,
            concat!(
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
            ),
        )
        .unwrap();
        std::fs::write(
            &key_path,
            concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgor2vT3esA5xTV1E4\n",
                "IWCpH+V2pudlqDwiS4+LKEKy3X6hRANCAASvz+sAGz7/92glvERlQlom5OFgseIg\n",
                "MgvGZM04KsqOD+D/hwG3tzmpOu4UAZyhAdrkAqvHWmfQkK5D8jdhgv33\n",
                "-----END PRIVATE KEY-----\n"
            ),
        )
        .unwrap();
        let state_path = temp.path().join("service-principal.json");
        let state = ServicePrincipalStateFile {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            certificate_path: "client-cert.pem".to_string(),
            private_key_path: "client-key.pem".to_string(),
        };
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        state_path
    }

    fn write_invalid_service_principal_state(temp: &TempDir) {
        std::fs::write(temp.path().join("client-cert.pem"), b"not-a-certificate").unwrap();
        std::fs::write(temp.path().join("client-key.pem"), b"not-a-key").unwrap();
        let state = ServicePrincipalStateFile {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            certificate_path: "client-cert.pem".to_string(),
            private_key_path: "client-key.pem".to_string(),
        };
        std::fs::write(
            temp.path().join("service-principal.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
    }

    fn write_mismatched_service_principal_state(temp: &TempDir) {
        write_service_principal_state(temp);
        std::fs::write(
            temp.path().join("client-key.pem"),
            concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgwA92gZP+jHsasiyN\n",
                "7EXLuqVG2dtgLXuEaEdJqKI9ueOhRANCAAQ3nLx4zRkZlPBKa53AJ9tc8SJeY6MI\n",
                "f2Nv/cxwiGclvIa/mG/Rz9WYK+tAWhhjZnPJyRJ4YoiYPSvkPJGBYVD8\n",
                "-----END PRIVATE KEY-----\n"
            ),
        )
        .unwrap();
    }

    fn with_runtime_env(test: impl FnOnce()) {
        temp_env::with_vars(
            [
                (
                    "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                    Some("example.servicebus.windows.net"),
                ),
                ("SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE", Some("upstream")),
                (
                    "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
                    Some("downstream"),
                ),
            ],
            test,
        );
    }

    fn write_service_bus_config(temp: &TempDir, namespace: &str, upstream: &str, downstream: &str) {
        let config = ServiceBusConfigFile {
            namespace: namespace.to_string(),
            upstream_queue: upstream.to_string(),
            downstream_queue: downstream.to_string(),
        };
        std::fs::write(
            temp.path().join(SERVICE_BUS_CONFIG_FILENAME),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
    }

    #[derive(Default)]
    struct FakePublisher {
        published: Vec<Vec<u8>>,
    }

    #[tonic::async_trait]
    impl UpstreamPublisher for FakePublisher {
        async fn publish(&mut self, payload: Vec<u8>) -> Result<(), CompanionError> {
            self.published.push(payload);
            Ok(())
        }
    }

    struct FakeConsumer {
        queued: VecDeque<Vec<u8>>,
        inflight: Option<Vec<u8>>,
        completes: usize,
        abandons: usize,
        fail_complete: bool,
    }

    impl FakeConsumer {
        fn new(payloads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                queued: payloads.into_iter().collect(),
                inflight: None,
                completes: 0,
                abandons: 0,
                fail_complete: false,
            }
        }

        fn with_complete_error(payloads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                queued: payloads.into_iter().collect(),
                inflight: None,
                completes: 0,
                abandons: 0,
                fail_complete: true,
            }
        }
    }

    #[tonic::async_trait]
    impl DownstreamConsumer for FakeConsumer {
        async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
            let payload = self.queued.pop_front();
            self.inflight = payload.clone();
            Ok(payload)
        }

        async fn complete(&mut self) -> Result<(), CompanionError> {
            if self.fail_complete {
                return Err(CompanionError::Config(
                    "injected downstream completion failure".to_string(),
                ));
            }
            self.inflight.take();
            self.completes += 1;
            Ok(())
        }

        async fn abandon(&mut self) -> Result<(), CompanionError> {
            self.inflight.take();
            self.abandons += 1;
            Ok(())
        }

        async fn abandon_inflight(&mut self) -> Result<(), CompanionError> {
            if self.inflight.is_some() {
                self.abandon().await?;
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    struct BlockingConsumer {
        queued: VecDeque<Vec<u8>>,
        inflight: Option<Vec<u8>>,
    }

    #[cfg(unix)]
    impl BlockingConsumer {
        fn new(payloads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                queued: payloads.into_iter().collect(),
                inflight: None,
            }
        }
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl DownstreamConsumer for BlockingConsumer {
        async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
            if let Some(payload) = self.queued.pop_front() {
                self.inflight = Some(payload.clone());
                Ok(Some(payload))
            } else {
                std::future::pending::<Result<Option<Vec<u8>>, CompanionError>>().await
            }
        }

        async fn complete(&mut self) -> Result<(), CompanionError> {
            self.inflight.take();
            Ok(())
        }

        async fn abandon(&mut self) -> Result<(), CompanionError> {
            self.inflight.take();
            Ok(())
        }

        async fn abandon_inflight(&mut self) -> Result<(), CompanionError> {
            if self.inflight.is_some() {
                self.abandon().await?;
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    struct SharedPublisher {
        published: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl UpstreamPublisher for SharedPublisher {
        async fn publish(&mut self, payload: Vec<u8>) -> Result<(), CompanionError> {
            self.published.lock().await.push(payload);
            Ok(())
        }
    }

    #[cfg(unix)]
    struct TestBrokerTransportFactory {
        connect_started: Arc<Notify>,
        release_connect: Arc<Notify>,
        connect_calls: Arc<AtomicUsize>,
        published: Arc<Mutex<Vec<Vec<u8>>>>,
        downstream_payloads: Vec<Vec<u8>>,
        allow_return: Arc<AtomicBool>,
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl super::BrokerTransportFactory for TestBrokerTransportFactory {
        type Publisher = SharedPublisher;
        type Consumer = BlockingConsumer;

        async fn connect(
            &self,
            _runtime_config: &RuntimeConfig,
            _runtime_state: &RuntimeCredentialState,
        ) -> Result<(Self::Publisher, Self::Consumer), CompanionError> {
            self.connect_calls.fetch_add(1, Ordering::SeqCst);
            self.connect_started.notify_waiters();
            if !self.allow_return.load(Ordering::SeqCst) {
                self.release_connect.notified().await;
                self.allow_return.store(true, Ordering::SeqCst);
            }
            Ok((
                SharedPublisher {
                    published: Arc::clone(&self.published),
                },
                BlockingConsumer::new(self.downstream_payloads.clone()),
            ))
        }
    }

    #[cfg(unix)]
    struct ShutdownAwareConsumer {
        payload: Option<Vec<u8>>,
        inflight: bool,
        inflight_set: Arc<Notify>,
        abandons: Arc<AtomicUsize>,
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl DownstreamConsumer for ShutdownAwareConsumer {
        async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
            let payload = self.payload.take();
            if let Some(payload) = payload {
                self.inflight = true;
                self.inflight_set.notify_waiters();
                Ok(Some(payload))
            } else {
                std::future::pending::<Result<Option<Vec<u8>>, CompanionError>>().await
            }
        }

        async fn complete(&mut self) -> Result<(), CompanionError> {
            self.inflight = false;
            Ok(())
        }

        async fn abandon(&mut self) -> Result<(), CompanionError> {
            if self.inflight {
                self.inflight = false;
                self.abandons.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }

        async fn abandon_inflight(&mut self) -> Result<(), CompanionError> {
            self.abandon().await
        }
    }

    #[cfg(unix)]
    struct DownstreamErrorCleanupConsumer {
        payload: Option<Vec<u8>>,
        inflight: bool,
        first_abandon_fails: bool,
        abandon_calls: Arc<AtomicUsize>,
        abandon_inflight_calls: Arc<AtomicUsize>,
    }

    #[cfg(unix)]
    #[tonic::async_trait]
    impl DownstreamConsumer for DownstreamErrorCleanupConsumer {
        async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
            let payload = self.payload.take();
            if let Some(payload) = payload {
                self.inflight = true;
                Ok(Some(payload))
            } else {
                std::future::pending::<Result<Option<Vec<u8>>, CompanionError>>().await
            }
        }

        async fn complete(&mut self) -> Result<(), CompanionError> {
            self.inflight = false;
            Ok(())
        }

        async fn abandon(&mut self) -> Result<(), CompanionError> {
            self.abandon_calls.fetch_add(1, Ordering::SeqCst);
            if self.first_abandon_fails {
                self.first_abandon_fails = false;
                return Err(CompanionError::Config(
                    "injected downstream abandon failure".to_string(),
                ));
            }
            self.inflight = false;
            Ok(())
        }

        async fn abandon_inflight(&mut self) -> Result<(), CompanionError> {
            self.abandon_inflight_calls.fetch_add(1, Ordering::SeqCst);
            if self.inflight {
                self.abandon().await?;
            }
            Ok(())
        }
    }

    #[cfg(unix)]
    struct ReaderState {
        eof: AtomicBool,
        waker: StdMutex<Option<Waker>>,
    }

    #[cfg(unix)]
    impl ReaderState {
        fn new() -> Self {
            Self {
                eof: AtomicBool::new(false),
                waker: StdMutex::new(None),
            }
        }

        fn finish(&self) {
            self.eof.store(true, Ordering::SeqCst);
            if let Some(waker) = self.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    #[cfg(unix)]
    struct ShutdownTestStream {
        reader_state: Arc<ReaderState>,
        write_started: Arc<Notify>,
    }

    #[cfg(unix)]
    impl AsyncRead for ShutdownTestStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.reader_state.eof.load(Ordering::SeqCst) {
                Poll::Ready(Ok(()))
            } else {
                *self.reader_state.waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    #[cfg(unix)]
    impl AsyncWrite for ShutdownTestStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.write_started.notify_waiters();
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[cfg(unix)]
    struct FailingWriteStream;

    #[cfg(unix)]
    impl AsyncRead for FailingWriteStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    #[cfg(unix)]
    impl AsyncWrite for FailingWriteStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected downstream write failure",
            )))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn display_lines_accept_one_to_four_entries() {
        for line_count in 1..=4 {
            let values = (0..line_count).map(|_| "line").collect::<Vec<_>>();
            assert!(validate_display_lines(&lines(&values)).is_ok());
        }
    }

    #[test]
    fn display_lines_reject_zero_or_more_than_four_entries() {
        assert!(validate_display_lines(&lines(&[])).is_err());
        assert!(validate_display_lines(&lines(&["1", "2", "3", "4", "5"])).is_err());
    }

    #[test]
    fn test_generate_certificate() {
        let temp = TempDir::new().unwrap();
        let (cert_path, key_path, cert_base64) = generate_certificate(temp.path()).unwrap();

        assert_eq!(cert_path, temp.path().join(CERT_PEM_FILENAME));
        assert_eq!(key_path, temp.path().join(KEY_PEM_FILENAME));
        assert!(cert_path.is_file());
        assert!(key_path.is_file());

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(cert_base64)
            .unwrap();
        let cert_file = std::fs::File::open(&cert_path).unwrap();
        let mut reader = std::io::BufReader::new(cert_file);
        let cert_der = rustls_pemfile::certs(&mut reader)
            .next()
            .transpose()
            .unwrap()
            .unwrap();
        assert_eq!(decoded, cert_der.as_ref());

        let certificate = Certificate::from_der(cert_der.as_ref()).unwrap();
        assert_ne!(
            certificate.tbs_certificate.validity.not_before,
            certificate.tbs_certificate.validity.not_after
        );
        let (algorithm, _) = load_signing_key(&key_path).unwrap();
        assert_eq!(algorithm, Algorithm::ES256);
        validate_certificate_matches_private_key(&cert_path, &key_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let key_mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(key_mode, 0o600);
        }
    }

    #[test]
    fn test_load_runtime_config_from_file() {
        let temp = TempDir::new().unwrap();
        write_service_bus_config(&temp, "file.servicebus.windows.net", "file-up", "file-down");

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
                "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        namespace: "file.servicebus.windows.net".to_string(),
                        upstream_queue: "file-up".to_string(),
                        downstream_queue: "file-down".to_string(),
                    }
                );
            },
        );
    }

    #[test]
    fn test_load_runtime_config_env_overrides_file() {
        let temp = TempDir::new().unwrap();
        write_service_bus_config(&temp, "file.servicebus.windows.net", "file-up", "file-down");

        temp_env::with_vars(
            [
                (
                    "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                    Some("env.servicebus.windows.net"),
                ),
                ("SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE", Some("env-up")),
                ("SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE", Some("env-down")),
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        namespace: "env.servicebus.windows.net".to_string(),
                        upstream_queue: "env-up".to_string(),
                        downstream_queue: "env-down".to_string(),
                    }
                );
            },
        );
    }

    #[test]
    fn test_load_runtime_config_trims_selected_values() {
        let temp = TempDir::new().unwrap();
        write_service_bus_config(
            &temp,
            "  file.servicebus.windows.net  ",
            "  file-up  ",
            "  file-down  ",
        );

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
                "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        namespace: "file.servicebus.windows.net".to_string(),
                        upstream_queue: "file-up".to_string(),
                        downstream_queue: "file-down".to_string(),
                    }
                );
            },
        );
    }

    #[test]
    fn test_load_runtime_config_rejects_blank_service_bus_file_values() {
        let temp = TempDir::new().unwrap();
        write_service_bus_config(&temp, "example.servicebus.windows.net", "  ", "downstream");

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
                "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
            ],
            || {
                let err = load_runtime_config(temp.path()).unwrap_err();
                assert!(err
                    .to_string()
                    .contains("service-bus.json upstream_queue must be set and non-empty"));
            },
        );
    }

    #[test]
    fn test_load_runtime_config_surfaces_invalid_service_bus_json() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join(SERVICE_BUS_CONFIG_FILENAME),
            b"{not valid json",
        )
        .unwrap();

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
                "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
            ],
            || {
                let err = load_runtime_config(temp.path()).unwrap_err();
                assert!(matches!(err, CompanionError::Json(_)));
            },
        );
    }

    #[test]
    fn build_az_bootstrap_script_passes_location_parameter_to_template() {
        let script = build_az_bootstrap_script();
        assert!(script.contains(r#"--location "$SONDE_AZURE_LOCATION""#));
        assert!(script.contains(r#"--parameters location="$SONDE_AZURE_LOCATION""#));
    }

    #[test]
    fn extract_device_code_handles_split_prompt_buffer() {
        let buffer = concat!(
            "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and ",
            "enter the code ABCD-EFGH to authenticate.\n",
        );
        assert_eq!(extract_device_code(buffer).as_deref(), Some("ABCD-EFGH"));
    }

    #[test]
    fn test_parse_bicep_outputs() {
        let json = r#"{
            "companionBootstrapValues": {
                "value": {
                    "tenantId": { "value": "11111111-1111-1111-1111-111111111111" },
                    "clientId": { "value": "22222222-2222-2222-2222-222222222222" },
                    "serviceBusNamespace": { "value": "example.servicebus.windows.net" },
                    "upstreamQueue": { "value": "upstream" },
                    "downstreamQueue": { "value": "downstream" }
                }
            }
        }"#;

        let (sp, sb) = parse_bicep_outputs(json).unwrap();
        assert_eq!(
            sp,
            ServicePrincipalStateFile {
                tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                certificate_path: CERT_PEM_FILENAME.to_string(),
                private_key_path: KEY_PEM_FILENAME.to_string(),
            }
        );
        assert_eq!(
            sb,
            ServiceBusConfigFile {
                namespace: "example.servicebus.windows.net".to_string(),
                upstream_queue: "upstream".to_string(),
                downstream_queue: "downstream".to_string(),
            }
        );
    }

    #[test]
    fn test_staged_commit() {
        let temp = TempDir::new().unwrap();
        let staging_dir = prepare_staging_dir(temp.path()).unwrap();
        std::fs::write(staging_dir.join(CERT_PEM_FILENAME), b"new-cert").unwrap();
        std::fs::write(staging_dir.join(KEY_PEM_FILENAME), b"new-key").unwrap();
        std::fs::write(temp.path().join(CERT_PEM_FILENAME), b"old-cert").unwrap();

        commit_staging(&staging_dir, temp.path()).unwrap();

        assert_eq!(
            std::fs::read(temp.path().join(CERT_PEM_FILENAME)).unwrap(),
            b"new-cert"
        );
        assert_eq!(
            std::fs::read(temp.path().join(KEY_PEM_FILENAME)).unwrap(),
            b"new-key"
        );
        assert!(!staging_dir.exists());
    }

    #[test]
    fn test_staged_cleanup() {
        let temp = TempDir::new().unwrap();
        let staging_dir = prepare_staging_dir(temp.path()).unwrap();
        std::fs::write(staging_dir.join(CERT_PEM_FILENAME), b"temp-cert").unwrap();

        cleanup_staging(&staging_dir);

        assert!(!staging_dir.exists());
    }

    #[test]
    fn runtime_ready_requires_namespace_and_queue_config() {
        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_SERVICEBUS_NAMESPACE",
                "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
                "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
            ],
            || {
                let temp = TempDir::new().unwrap();
                let err = load_runtime_config(temp.path()).unwrap_err();
                assert!(matches!(err, CompanionError::Config(_)));
            },
        );
    }

    #[test]
    fn runtime_ready_uses_service_principal_state_file() {
        let temp = TempDir::new().unwrap();
        write_service_principal_state(&temp);
        with_runtime_env(|| {
            let (config, state) = check_runtime_ready(temp.path()).unwrap();
            assert_eq!(
                config,
                RuntimeConfig {
                    namespace: "example.servicebus.windows.net".to_string(),
                    upstream_queue: "upstream".to_string(),
                    downstream_queue: "downstream".to_string(),
                }
            );
            assert_eq!(
                state,
                RuntimeCredentialState {
                    tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                    client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                    certificate_path: temp.path().join("client-cert.pem").canonicalize().unwrap(),
                    private_key_path: temp.path().join("client-key.pem").canonicalize().unwrap(),
                }
            );
        });
    }

    #[test]
    fn runtime_ready_rejects_blank_state_paths() {
        let temp = TempDir::new().unwrap();
        let state = ServicePrincipalStateFile {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            certificate_path: " ".to_string(),
            private_key_path: "client-key.pem".to_string(),
        };
        std::fs::write(
            temp.path().join("service-principal.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        std::fs::write(temp.path().join("client-key.pem"), b"dummy").unwrap();
        with_runtime_env(|| {
            let err = check_runtime_ready(temp.path()).unwrap_err();
            assert!(err
                .to_string()
                .contains("service principal certificate_path must be set and non-empty"));
        });
    }

    #[test]
    fn runtime_ready_rejects_unparseable_pem_material() {
        let temp = TempDir::new().unwrap();
        write_invalid_service_principal_state(&temp);
        with_runtime_env(|| {
            assert!(check_runtime_ready(temp.path()).is_err());
        });
    }

    #[test]
    fn runtime_ready_rejects_mismatched_certificate_private_key() {
        let temp = TempDir::new().unwrap();
        write_mismatched_service_principal_state(&temp);
        with_runtime_env(|| {
            let err = check_runtime_ready(temp.path()).unwrap_err();
            assert!(err
                .to_string()
                .contains("service principal certificate public key does not match private key"));
        });
    }

    #[test]
    fn runtime_ready_reports_missing_state_file_clearly() {
        let temp = TempDir::new().unwrap();
        with_runtime_env(|| {
            let err = check_runtime_ready(temp.path()).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "service principal state file not found: {}",
                    temp.path().join("service-principal.json").display()
                )
            );
        });
    }

    #[test]
    fn load_signing_key_rejects_non_p256_ec_private_keys() {
        let temp = TempDir::new().unwrap();
        let private_key_path = temp.path().join("client-key.pem");
        std::fs::write(
            &private_key_path,
            concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDD6GGUh9wwgHc1R0MYl\n",
                "xZfpPwMaBFTrBgVlM+BwH5lDYPlcsiyN1yQxjtNvBGY9HRChZANiAARjYBFs2Isx\n",
                "DAL8I6WJrqUHfWv3iFNkGaNXrJJSf2q5Qe1pmV4qURhQ9bvqcE/fjyRNui4vO9vZ\n",
                "YpU8DwOw4WRFViavnnT+S7gi+MPx9LgM0Ol80YC4eaFWfPc1D11V0zs=\n",
                "-----END PRIVATE KEY-----\n"
            ),
        )
        .unwrap();

        let err = match load_signing_key(&private_key_path) {
            Ok(_) => panic!("expected non-P-256 EC private key to be rejected"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("service principal EC private key must use the P-256 curve"));
    }

    #[test]
    fn downstream_body_to_connector_payload_rejects_oversized_messages() {
        let body = vec![0u8; CONNECTOR_MAX_FRAME_LENGTH + 1];
        let err = downstream_body_to_connector_payload(&body).unwrap_err();
        assert!(err
            .to_string()
            .contains("exceeds connector max frame length"));
    }

    #[tokio::test]
    async fn client_assertion_credential_joins_scopes_and_caches_by_scope_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "application/json")
                    .set_body_string("{\"access_token\":\"cached-token\",\"expires_in\":3600}"),
            )
            .mount(&server)
            .await;

        let credential = ClientAssertionCredential {
            client_id: "test-client-id".to_string(),
            token_endpoint: format!("{}/token", server.uri()),
            signing_algorithm: Algorithm::HS256,
            signing_key: EncodingKey::from_secret(b"test-secret"),
            certificate_thumbprint: "thumbprint".to_string(),
            http_client: reqwest::Client::builder().build().unwrap(),
            cached_token: tokio::sync::Mutex::new(None),
        };

        credential
            .get_token(&["scope-a", "scope-b"], None)
            .await
            .unwrap();
        credential
            .get_token(&["scope-a", "scope-b"], None)
            .await
            .unwrap();
        credential.get_token(&["scope-c"], None).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let request_bodies = requests
            .iter()
            .map(|request| String::from_utf8(request.body.clone()).unwrap())
            .collect::<Vec<_>>();
        assert!(request_bodies
            .iter()
            .any(|body| body.contains("scope=scope-a+scope-b")));
        assert!(request_bodies
            .iter()
            .any(|body| body.contains("scope=scope-c")));
    }

    #[tokio::test]
    async fn client_assertion_credential_surfaces_token_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .append_header("content-type", "application/json")
                    .set_body_string(
                        "{\"error\":\"invalid_scope\",\"error_description\":\"scope rejected for test\"}",
                    ),
            )
            .mount(&server)
            .await;

        let credential = ClientAssertionCredential {
            client_id: "test-client-id".to_string(),
            token_endpoint: format!("{}/token", server.uri()),
            signing_algorithm: Algorithm::HS256,
            signing_key: EncodingKey::from_secret(b"test-secret"),
            certificate_thumbprint: "thumbprint".to_string(),
            http_client: reqwest::Client::builder().build().unwrap(),
            cached_token: tokio::sync::Mutex::new(None),
        };

        let err = credential
            .get_token(&["scope-a"], None)
            .await
            .expect_err("expected token request to fail");
        let err_text = err.to_string();
        assert!(err_text.contains("400 Bad Request"));
        assert!(err_text.contains("invalid_scope"));
        assert!(err_text.contains("scope rejected for test"));
    }

    #[tokio::test]
    async fn upstream_pump_publishes_one_framed_payload() {
        let (mut client, server) = duplex(64);
        let mut publisher = FakePublisher::default();
        let payload = vec![1u8, 2, 3, 4];

        tokio::spawn(async move {
            let len = u32::try_from(payload.len()).unwrap().to_be_bytes();
            let mut server = server;
            server.write_all(&len).await.unwrap();
            server.write_all(&payload).await.unwrap();
            server.flush().await.unwrap();
        });

        assert!(pump_upstream_once(&mut client, &mut publisher)
            .await
            .unwrap());
        assert_eq!(publisher.published, vec![vec![1u8, 2, 3, 4]]);
    }

    #[tokio::test]
    async fn downstream_pump_completes_after_successful_local_write() {
        let (client, mut server) = duplex(64);
        let mut consumer = FakeConsumer::new([vec![9u8, 8, 7]]);
        let mut client = client;

        pump_downstream_once(&mut client, &mut consumer)
            .await
            .unwrap();

        let mut len = [0u8; 4];
        server.read_exact(&mut len).await.unwrap();
        let frame_len = usize::try_from(u32::from_be_bytes(len)).unwrap();
        let mut payload = vec![0u8; frame_len];
        server.read_exact(&mut payload).await.unwrap();

        assert_eq!(payload, vec![9u8, 8, 7]);
        assert_eq!(consumer.completes, 1);
        assert_eq!(consumer.abandons, 0);
    }

    #[tokio::test]
    async fn downstream_pump_abandons_after_local_write_failure() {
        let (client, server) = duplex(64);
        drop(server);
        let mut client = client;
        let mut consumer = FakeConsumer::new([vec![1u8, 2, 3]]);

        assert!(pump_downstream_once(&mut client, &mut consumer)
            .await
            .is_err());
        assert_eq!(consumer.completes, 0);
        assert_eq!(consumer.abandons, 1);
    }

    #[tokio::test]
    async fn downstream_pump_abandons_after_completion_failure() {
        let (client, mut server) = duplex(64);
        let mut client = client;
        let mut consumer = FakeConsumer::with_complete_error([vec![4u8, 5, 6]]);

        let err = pump_downstream_once(&mut client, &mut consumer)
            .await
            .unwrap_err();
        let mut len = [0u8; 4];
        server.read_exact(&mut len).await.unwrap();
        let frame_len = usize::try_from(u32::from_be_bytes(len)).unwrap();
        let mut payload = vec![0u8; frame_len];
        server.read_exact(&mut payload).await.unwrap();

        assert_eq!(payload, vec![4u8, 5, 6]);
        assert!(err.to_string().contains("completion failure"));
        assert_eq!(consumer.completes, 0);
        assert_eq!(consumer.abandons, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bootstrap_fails_closed_when_progress_display_fails() {
        let temp = TempDir::new().unwrap();
        let args = super::BootstrapArgs {
            azure_location: "westus2".to_string(),
            azure_project_name: "sonde".to_string(),
            azure_subscription_id: None,
        };

        let err = super::bootstrap("/tmp/sonde-missing-admin.sock", temp.path(), args)
            .await
            .unwrap_err();
        assert!(!temp.path().join(".staging").exists());
        assert!(matches!(err, CompanionError::TonicTransport(_)));
    }

    #[tokio::test]
    async fn read_framed_rejects_payloads_over_connector_limit() {
        let oversized_len = u32::try_from(CONNECTOR_MAX_FRAME_LENGTH + 1)
            .unwrap()
            .to_be_bytes();
        let (mut client, mut server) = duplex(16);

        tokio::spawn(async move {
            server.write_all(&oversized_len).await.unwrap();
            server.flush().await.unwrap();
        });

        let err = read_framed(&mut client).await.unwrap_err();
        assert!(err.to_string().contains("exceeds max"));
    }

    #[tokio::test]
    async fn read_framed_returns_none_on_clean_eof() {
        let (mut client, server) = duplex(16);
        drop(server);

        assert!(read_framed(&mut client).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_framed_rejects_truncated_length_prefix() {
        let (mut client, mut server) = duplex(16);

        tokio::spawn(async move {
            server.write_all(&[0u8, 0u8]).await.unwrap();
            server.shutdown().await.unwrap();
        });

        let err = read_framed(&mut client).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("connector EOF while reading frame length prefix"));
    }

    #[tokio::test]
    async fn write_framed_rejects_payloads_over_connector_limit() {
        let (mut client, _server) = duplex(16);
        let payload = vec![0u8; CONNECTOR_MAX_FRAME_LENGTH + 1];

        let err = write_framed(&mut client, &payload).await.unwrap_err();
        assert!(err.to_string().contains("exceeds max"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bridge_runtime_abandons_inflight_message_when_upstream_finishes() {
        let reader_state = Arc::new(ReaderState::new());
        let write_started = Arc::new(Notify::new());
        let inflight_set = Arc::new(Notify::new());
        let abandons = Arc::new(AtomicUsize::new(0));
        let inflight_wait = inflight_set.notified();
        let write_wait = write_started.notified();
        let stream = ShutdownTestStream {
            reader_state: Arc::clone(&reader_state),
            write_started: Arc::clone(&write_started),
        };
        let consumer = ShutdownAwareConsumer {
            payload: Some(vec![1u8, 2, 3]),
            inflight: false,
            inflight_set: Arc::clone(&inflight_set),
            abandons: Arc::clone(&abandons),
        };

        let bridge_task = tokio::spawn(async move {
            super::bridge_runtime(stream, FakePublisher::default(), consumer).await
        });

        inflight_wait.await;
        write_wait.await;
        reader_state.finish();

        bridge_task.await.unwrap().unwrap();
        assert_eq!(abandons.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bridge_runtime_abandons_inflight_message_after_downstream_error() {
        let abandon_calls = Arc::new(AtomicUsize::new(0));
        let abandon_inflight_calls = Arc::new(AtomicUsize::new(0));
        let consumer = DownstreamErrorCleanupConsumer {
            payload: Some(vec![1u8, 2, 3]),
            inflight: false,
            first_abandon_fails: true,
            abandon_calls: Arc::clone(&abandon_calls),
            abandon_inflight_calls: Arc::clone(&abandon_inflight_calls),
        };

        let err = super::bridge_runtime(FailingWriteStream, FakePublisher::default(), consumer)
            .await
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("injected downstream write failure"));
        assert_eq!(abandon_inflight_calls.load(Ordering::SeqCst), 1);
        assert_eq!(abandon_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn relative_state_paths_resolve_under_state_directory() {
        let state_dir = Path::new("/tmp/sonde-state");
        assert_eq!(
            resolve_state_relative_path(state_dir, "certs/client.pem").unwrap(),
            state_dir.join("certs/client.pem")
        );
    }

    #[test]
    fn resolve_state_relative_path_rejects_absolute_paths() {
        let state_dir = Path::new("/tmp/sonde-state");
        let absolute = std::env::current_dir().unwrap().join("client.pem");
        let err =
            resolve_state_relative_path(state_dir, &absolute.display().to_string()).unwrap_err();
        assert!(err
            .to_string()
            .contains("must be relative to the state directory"));
    }

    #[test]
    fn resolve_state_relative_path_rejects_parent_directory_traversal() {
        let state_dir = Path::new("/tmp/sonde-state");
        let err = resolve_state_relative_path(state_dir, "../client.pem").unwrap_err();
        assert!(err
            .to_string()
            .contains("must stay within the state directory"));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_ready_rejects_symlink_escape_from_state_directory() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let state_dir = temp.path().join("state");
        let outside_dir = temp.path().join("outside");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();

        let outside_cert = outside_dir.join("client-cert.pem");
        std::fs::write(&outside_cert, "dummy").unwrap();
        symlink(&outside_cert, state_dir.join("client-cert.pem")).unwrap();
        std::fs::write(state_dir.join("client-key.pem"), "dummy").unwrap();

        let state = ServicePrincipalStateFile {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            certificate_path: "client-cert.pem".to_string(),
            private_key_path: "client-key.pem".to_string(),
        };
        std::fs::write(
            state_dir.join("service-principal.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();

        let err = super::load_runtime_credential_state(&state_dir).unwrap_err();
        assert!(err
            .to_string()
            .contains("resolved outside the state directory"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_checked_with_factory_waits_for_broker_before_opening_connector_and_bridges_frames()
    {
        use tokio::net::UnixListener;
        use tokio::time::{timeout, Duration};

        let temp = TempDir::new().unwrap();
        let socket_path = temp.path().join("connector.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let connect_started = Arc::new(Notify::new());
        let release_connect = Arc::new(Notify::new());
        let connect_calls = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(Mutex::new(Vec::new()));
        let connect_started_wait = connect_started.notified();
        let factory = TestBrokerTransportFactory {
            connect_started: Arc::clone(&connect_started),
            release_connect: Arc::clone(&release_connect),
            connect_calls: Arc::clone(&connect_calls),
            published: Arc::clone(&published),
            downstream_payloads: vec![vec![7u8, 8, 9]],
            allow_return: Arc::new(AtomicBool::new(false)),
        };
        let connector_socket = socket_path.to_string_lossy().into_owned();
        let runtime_config = RuntimeConfig {
            namespace: "example.servicebus.windows.net".to_string(),
            upstream_queue: "upstream".to_string(),
            downstream_queue: "downstream".to_string(),
        };
        let runtime_state = RuntimeCredentialState {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            certificate_path: temp.path().join("client-cert.pem"),
            private_key_path: temp.path().join("client-key.pem"),
        };

        let run_task = tokio::spawn(async move {
            super::run_checked_with_factory(
                &connector_socket,
                &runtime_config,
                &runtime_state,
                &factory,
            )
            .await
        });

        connect_started_wait.await;
        assert_eq!(connect_calls.load(Ordering::SeqCst), 1);
        assert!(timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err());

        release_connect.notify_waiters();
        let (mut server, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();

        write_framed(&mut server, b"upstream-test").await.unwrap();
        let downstream = read_framed(&mut server).await.unwrap().unwrap();
        assert_eq!(downstream, vec![7u8, 8, 9]);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if published.lock().await.clone() == vec![b"upstream-test".to_vec()] {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        run_task.abort();
    }
}
