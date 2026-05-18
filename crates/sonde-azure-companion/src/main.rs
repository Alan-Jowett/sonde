// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use azure_core::date::OffsetDateTime;
use azure_core::error::ErrorKind;
use azure_core::Uuid;
use base64::Engine as _;
use bollard::container::LogOutput;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::pkcs8::DecodePrivateKey as Ed25519DecodePrivateKey;
use futures_util::StreamExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use regex::Regex;
use rsa::pkcs1::DecodeRsaPrivateKey;
use serde::{Deserialize, Deserializer, Serialize};
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

const SERVICE_PRINCIPAL_STATE_FILENAME: &str = "service-principal.json";
const DEFAULT_LOGIN_ENDPOINT: &str = "https://login.microsoftonline.com";
const DEFAULT_BOOTSTRAP_IMAGE_REPOSITORY: &str = "ghcr.io/alan-jowett/sonde-azure-bootstrap";
const CERT_PEM_FILENAME: &str = "cert.pem";
const KEY_PEM_FILENAME: &str = "key.pem";
const STORAGE_QUEUES_CONFIG_FILENAME: &str = "storage-queues.json";
const STAGING_DIR_NAME: &str = ".staging";
const ACTIVE_STATE_FILENAME: &str = ".current-state";
const STATE_GENERATION_PREFIX: &str = ".state-";
const CONNECTOR_MAX_FRAME_LENGTH: usize =
    sonde_gateway::connector::DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE;
const ACCESS_TOKEN_REFRESH_MARGIN_SECS: i64 = 300;
const CLIENT_ASSERTION_LIFETIME_SECS: i64 = 600;
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const TOKEN_HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;
const TOKEN_HTTP_TIMEOUT_SECS: u64 = 30;

#[cfg(windows)]
const SERVICE_NAME: &str = "sonde-azure-companion";
#[cfg(windows)]
const SERVICE_DISPLAY_NAME: &str = "Sonde Azure Companion";
#[cfg(windows)]
const SERVICE_DESCRIPTION: &str = "Bridges the Sonde gateway connector to Azure Storage Queues.";
#[cfg(windows)]
static SERVICE_CLI: OnceLock<Cli> = OnceLock::new();
#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, service_entry);

#[cfg(unix)]
fn default_state_dir() -> PathBuf {
    PathBuf::from(DEFAULT_STATE_DIR)
}

#[cfg(windows)]
fn default_state_dir() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("sonde-azure-companion")
}

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

#[derive(Debug, Clone, Parser)]
#[command(name = "sonde-azure-companion", version)]
struct Cli {
    /// Gateway admin socket path (UDS on Unix, named pipe on Windows).
    #[arg(long, global = true, env = "SONDE_GATEWAY_ADMIN_SOCKET", default_value = DEFAULT_ADMIN_SOCKET)]
    admin_socket: String,

    /// Gateway connector socket path (UDS on Unix, named pipe on Windows).
    #[arg(long, global = true, env = "SONDE_GATEWAY_CONNECTOR_SOCKET", default_value = DEFAULT_CONNECTOR_SOCKET)]
    connector_socket: String,

    /// Persistent state directory reserved for bootstrap output and runtime auth material.
    #[arg(long, global = true, env = "SONDE_AZURE_COMPANION_STATE_DIR", default_value_os_t = default_state_dir())]
    state_dir: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
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
    /// Install the Windows service registration (requires Administrator).
    #[cfg(windows)]
    Install,
    /// Remove the Windows service registration (requires Administrator).
    #[cfg(windows)]
    Uninstall,
    /// Run under the Windows Service Control Manager.
    #[cfg(windows)]
    #[command(hide = true)]
    Service,
}

#[derive(Debug, Clone, Args)]
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

    /// Optional bootstrap image override for development and test workflows.
    #[arg(long, env = "SONDE_AZURE_BOOTSTRAP_IMAGE")]
    bootstrap_image: Option<String>,

    /// Optional custom domain FQDN for the Static Web App (e.g., sondeplatform.com).
    #[arg(long, env = "SONDE_AZURE_CUSTOM_DOMAIN_NAME")]
    custom_domain_name: Option<String>,

    /// Resource group containing the Azure DNS zone for the custom domain.
    #[arg(long, env = "SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP")]
    custom_domain_dns_resource_group: Option<String>,

    /// DNS zone name for the custom domain (defaults to custom_domain_name for apex domains).
    #[arg(long, env = "SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME")]
    custom_domain_dns_zone_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeConfig {
    queue_endpoint: String,
    upstream_queue: String,
    downstream_queue: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct ServicePrincipalStateFile {
    tenant_id: String,
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login_endpoint: Option<String>,
    certificate_path: String,
    private_key_path: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
struct StorageQueuesConfigFile {
    queue_endpoint: String,
    upstream_queue: String,
    downstream_queue: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeCredentialState {
    tenant_id: String,
    client_id: String,
    login_endpoint: String,
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
    #[serde(rename = "tenantId", deserialize_with = "deserialize_bicep_string")]
    tenant_id: String,
    #[serde(rename = "clientId", deserialize_with = "deserialize_bicep_string")]
    client_id: String,
    #[serde(
        rename = "loginEndpoint",
        deserialize_with = "deserialize_bicep_string"
    )]
    login_endpoint: String,
    #[serde(
        rename = "storageQueueEndpoint",
        deserialize_with = "deserialize_bicep_string"
    )]
    storage_queue_endpoint: String,
    #[serde(
        rename = "upstreamQueue",
        deserialize_with = "deserialize_bicep_string"
    )]
    upstream_queue: String,
    #[serde(
        rename = "downstreamQueue",
        deserialize_with = "deserialize_bicep_string"
    )]
    downstream_queue: String,
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BicepStringField {
    Plain(String),
    Wrapped { value: String },
}

fn deserialize_bicep_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let field = BicepStringField::deserialize(deserializer)?;
    Ok(match field {
        BicepStringField::Plain(value) => value,
        BicepStringField::Wrapped { value } => value,
    })
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

struct StorageQueueTransportFactory;

const STORAGE_QUEUE_API_VERSION: &str = "2024-11-04";
const STORAGE_TOKEN_SCOPE: &str = "https://storage.azure.com/.default";
const STORAGE_QUEUE_VISIBILITY_TIMEOUT_SECS: u64 = 30;
const STORAGE_QUEUE_EMPTY_POLL_DELAY: Duration = Duration::from_secs(1);

struct StorageQueuePublisher {
    queue_endpoint: String,
    queue_name: String,
    credential: Arc<dyn TokenCredential>,
    http_client: reqwest::Client,
}

struct StorageQueueConsumer {
    queue_endpoint: String,
    queue_name: String,
    credential: Arc<dyn TokenCredential>,
    http_client: reqwest::Client,
    inflight: Option<StorageQueueMessage>,
}

#[derive(Debug)]
struct StorageQueueMessage {
    message_id: String,
    pop_receipt: String,
    body: Vec<u8>,
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
    let endpoint_env = std::env::var("SONDE_AZURE_STORAGE_QUEUE_ENDPOINT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let upstream_env = std::env::var("SONDE_AZURE_STORAGE_UPSTREAM_QUEUE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let downstream_env = std::env::var("SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let file_config = match load_storage_queues_config_file(state_dir) {
        Ok(config) => Some(config),
        Err(CompanionError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };

    let queue_endpoint = if let Some(value) = endpoint_env {
        require_non_empty(value, "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(
            config.queue_endpoint.clone(),
            "storage-queues.json queue_endpoint",
        )?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT must be set and non-empty (or storage-queues.json must exist in state dir)"
                .into(),
        ));
    };
    let upstream_queue = if let Some(value) = upstream_env {
        require_non_empty(value, "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(
            config.upstream_queue.clone(),
            "storage-queues.json upstream_queue",
        )?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE must be set and non-empty (or storage-queues.json must exist in state dir)"
                .into(),
        ));
    };
    let downstream_queue = if let Some(value) = downstream_env {
        require_non_empty(value, "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE")?
    } else if let Some(config) = file_config.as_ref() {
        require_non_empty(
            config.downstream_queue.clone(),
            "storage-queues.json downstream_queue",
        )?
    } else {
        return Err(CompanionError::Config(
            "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE must be set and non-empty (or storage-queues.json must exist in state dir)"
                .into(),
        ));
    };

    Ok(RuntimeConfig {
        queue_endpoint,
        upstream_queue,
        downstream_queue,
    })
}

fn load_storage_queues_config_file(
    state_dir: &Path,
) -> Result<StorageQueuesConfigFile, CompanionError> {
    let effective_state_dir = resolve_effective_state_dir(state_dir)?;
    let config_path = effective_state_dir.join(STORAGE_QUEUES_CONFIG_FILENAME);
    let bytes = std::fs::read(&config_path)?;
    let config: StorageQueuesConfigFile = serde_json::from_slice(&bytes)?;
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

fn active_state_marker_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ACTIVE_STATE_FILENAME)
}

fn active_state_generation_name(state_dir: &Path) -> Result<Option<String>, CompanionError> {
    let marker_path = active_state_marker_path(state_dir);
    let marker_text = match std::fs::read_to_string(&marker_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let generation_name = marker_text.trim();
    if generation_name.is_empty() {
        return Err(CompanionError::Config(format!(
            "active state marker `{}` must not be empty",
            marker_path.display()
        )));
    }
    let generation_path = resolve_state_relative_path(state_dir, generation_name)?;
    if !generation_path.is_dir() {
        return Err(CompanionError::Config(format!(
            "active state generation directory not found: {}",
            generation_path.display()
        )));
    }
    Ok(Some(generation_name.to_string()))
}

fn resolve_effective_state_dir(state_dir: &Path) -> Result<PathBuf, CompanionError> {
    match active_state_generation_name(state_dir)? {
        Some(generation_name) => resolve_state_relative_path(state_dir, &generation_name),
        None => Ok(state_dir.to_path_buf()),
    }
}

fn commit_staging(staging_dir: &Path, state_dir: &Path) -> Result<(), CompanionError> {
    let previous_generation = active_state_generation_name(state_dir)?;
    let generation_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| CompanionError::Config(format!("system clock before UNIX_EPOCH: {err}")))?
        .as_nanos();
    let generation_name = format!("{STATE_GENERATION_PREFIX}{generation_suffix}");
    let generation_dir = state_dir.join(&generation_name);
    std::fs::rename(staging_dir, &generation_dir)?;

    let marker_path = active_state_marker_path(state_dir);
    let marker_tmp = state_dir.join(format!("{ACTIVE_STATE_FILENAME}.tmp"));
    let marker_update_result: Result<(), CompanionError> = (|| {
        std::fs::write(&marker_tmp, format!("{generation_name}\n"))?;
        std::fs::rename(&marker_tmp, &marker_path)?;
        Ok(())
    })();

    if let Err(err) = marker_update_result {
        let _ = std::fs::remove_file(&marker_tmp);
        let _ = std::fs::remove_dir_all(&generation_dir);
        return Err(err);
    }

    if let Some(previous_generation) = previous_generation {
        if previous_generation.starts_with(STATE_GENERATION_PREFIX)
            && previous_generation != generation_name
        {
            let previous_dir = state_dir.join(previous_generation);
            let _ = std::fs::remove_dir_all(previous_dir);
        }
    }

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

#[cfg(windows)]
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
unsafe fn null_terminated_wide_to_string(value: std::ptr::NonNull<u16>) -> String {
    use std::slice;

    let value = value.as_ptr();
    let mut len = 0usize;
    while *value.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(slice::from_raw_parts(value, len))
}

#[cfg(windows)]
fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> Result<String, CompanionError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut sid_wide = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_wide) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let sid_string = unsafe {
        null_terminated_wide_to_string(std::ptr::NonNull::new(sid_wide).ok_or_else(|| {
            CompanionError::Config("ConvertSidToStringSidW returned a null SID string".to_string())
        })?)
    };
    unsafe {
        let _ = LocalFree(sid_wide.cast());
    }
    Ok(sid_string)
}

#[cfg(windows)]
fn current_user_sid_string() -> Result<String, CompanionError> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let result = {
        let mut required_len = 0;
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required_len)
        };
        if required_len == 0 {
            Err(std::io::Error::last_os_error().into())
        } else {
            let mut buffer = vec![0u8; required_len as usize];
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    required_len,
                    &mut required_len,
                )
            } == 0
            {
                Err(std::io::Error::last_os_error().into())
            } else {
                let token_user =
                    unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
                sid_to_string(token_user.User.Sid)
            }
        }
    };

    unsafe {
        let _ = CloseHandle(token);
    }

    result
}

#[cfg(windows)]
fn lookup_account_sid_string(account_name: &str) -> Result<String, CompanionError> {
    use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE};

    let account_name_wide = wide_null(std::ffi::OsStr::new(account_name));
    let mut sid_len = 0u32;
    let mut domain_len = 0u32;
    let mut sid_use: SID_NAME_USE = 0;
    let _ = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account_name_wide.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut sid_use,
        )
    };
    if sid_len == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut sid_buffer = vec![0u8; sid_len as usize];
    let mut domain_buffer = vec![0u16; domain_len as usize];
    let domain_ptr = if domain_buffer.is_empty() {
        std::ptr::null_mut()
    } else {
        domain_buffer.as_mut_ptr()
    };
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account_name_wide.as_ptr(),
            sid_buffer.as_mut_ptr().cast(),
            &mut sid_len,
            domain_ptr,
            &mut domain_len,
            &mut sid_use,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }

    sid_to_string(sid_buffer.as_mut_ptr().cast())
}

#[cfg(windows)]
fn configured_windows_service_sid_string() -> Result<String, CompanionError> {
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW,
        QUERY_SERVICE_CONFIGW, SC_MANAGER_CONNECT, SERVICE_QUERY_CONFIG,
    };

    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }

    let result = {
        let service_name_wide = wide_null(std::ffi::OsStr::new(SERVICE_NAME));
        let service =
            unsafe { OpenServiceW(manager, service_name_wide.as_ptr(), SERVICE_QUERY_CONFIG) };
        if service.is_null() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
                Ok(String::from("S-1-5-18"))
            } else {
                Err(err.into())
            }
        } else {
            let service_result = {
                let mut bytes_needed = 0u32;
                let _ = unsafe {
                    QueryServiceConfigW(service, std::ptr::null_mut(), 0, &mut bytes_needed)
                };
                if bytes_needed == 0 {
                    Err(std::io::Error::last_os_error().into())
                } else {
                    let mut buffer = vec![0u8; bytes_needed as usize];
                    if unsafe {
                        QueryServiceConfigW(
                            service,
                            buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
                            bytes_needed,
                            &mut bytes_needed,
                        )
                    } == 0
                    {
                        Err(std::io::Error::last_os_error().into())
                    } else {
                        let config = unsafe {
                            std::ptr::read_unaligned(
                                buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
                            )
                        };
                        let account_name = unsafe {
                            null_terminated_wide_to_string(
                                std::ptr::NonNull::new(config.lpServiceStartName).ok_or_else(
                                    || {
                                        CompanionError::Config(
                                            "service configuration returned a null service account"
                                                .to_string(),
                                        )
                                    },
                                )?,
                            )
                        };
                        if account_name.eq_ignore_ascii_case("localsystem")
                            || account_name.eq_ignore_ascii_case(r"NT AUTHORITY\SYSTEM")
                        {
                            Ok(String::from("S-1-5-18"))
                        } else {
                            lookup_account_sid_string(&account_name)
                        }
                    }
                }
            };
            unsafe {
                let _ = CloseServiceHandle(service);
            }
            service_result
        }
    };

    unsafe {
        let _ = CloseServiceHandle(manager);
    }

    result
}

#[cfg(windows)]
fn windows_private_key_sddl() -> Result<String, CompanionError> {
    let current_user_sid = current_user_sid_string()?;
    let service_sid = configured_windows_service_sid_string()?;
    if service_sid == current_user_sid {
        Ok(format!("D:P(A;;FA;;;{current_user_sid})"))
    } else {
        Ok(format!(
            "D:P(A;;FA;;;{current_user_sid})(A;;GR;;;{service_sid})"
        ))
    }
}

#[cfg(unix)]
fn write_private_key_pem(path: &Path, pem: &str) -> Result<(), CompanionError> {
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

#[cfg(windows)]
fn write_private_key_pem(path: &Path, pem: &str) -> Result<(), CompanionError> {
    use std::fs::File;
    use std::io::Write as _;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{LocalFree, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    };

    let private_key_sddl = windows_private_key_sddl()?;
    let private_key_sddl_wide = wide_null(std::ffi::OsStr::new(&private_key_sddl));
    let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            private_key_sddl_wide.as_ptr(),
            1,
            &mut security_descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }

    let path_wide = wide_null(path.as_os_str());
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security_descriptor,
        bInheritHandle: 0,
    };

    let result: Result<(), CompanionError> = {
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                FILE_GENERIC_WRITE,
                0,
                &security_attributes,
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error().into())
        } else {
            let mut file = unsafe { File::from_raw_handle(handle) };
            file.write_all(pem.as_bytes())?;
            file.flush()?;
            Ok(())
        }
    };

    unsafe {
        let _ = LocalFree(security_descriptor.cast());
    }

    if let Err(err) = result {
        let _ = std::fs::remove_file(path);
        return Err(err);
    }

    Ok(())
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

/// Resolves an optional `login_endpoint` from the service-principal state file.
///
/// - `None` (field absent): returns the public-cloud default.
/// - `Some(value)` with non-empty trimmed content: returns the value with any
///   trailing slash stripped.
/// - `Some("")` or whitespace-only: returns a configuration error.
fn resolve_login_endpoint(value: Option<String>) -> Result<String, CompanionError> {
    match value {
        None => Ok(DEFAULT_LOGIN_ENDPOINT.to_string()),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(CompanionError::Config(
                    "service principal login_endpoint must not be empty when present".to_string(),
                ));
            }
            let normalized = trimmed.trim_end_matches('/');
            if normalized.is_empty() {
                return Err(CompanionError::Config(
                    "service principal login_endpoint must not be empty when present".to_string(),
                ));
            }
            Ok(normalized.to_string())
        }
    }
}

fn load_runtime_credential_state(
    state_dir: &Path,
) -> Result<RuntimeCredentialState, CompanionError> {
    let effective_state_dir = resolve_effective_state_dir(state_dir)?;
    let state_path = effective_state_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME);
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
    let login_endpoint = resolve_login_endpoint(state.login_endpoint)?;
    let certificate_path_value =
        require_non_empty(state.certificate_path, "service principal certificate_path")?;
    let certificate_path =
        resolve_state_relative_path(&effective_state_dir, &certificate_path_value)?;
    if !certificate_path.is_file() {
        return Err(CompanionError::Config(format!(
            "service principal certificate file not found: {}",
            certificate_path.display()
        )));
    }
    let certificate_path = canonicalize_state_file_path(
        &effective_state_dir,
        &certificate_path,
        &certificate_path_value,
    )?;
    let private_key_path_value =
        require_non_empty(state.private_key_path, "service principal private_key_path")?;
    let private_key_path =
        resolve_state_relative_path(&effective_state_dir, &private_key_path_value)?;
    if !private_key_path.is_file() {
        return Err(CompanionError::Config(format!(
            "service principal private key file not found: {}",
            private_key_path.display()
        )));
    }
    let private_key_path = canonicalize_state_file_path(
        &effective_state_dir,
        &private_key_path,
        &private_key_path_value,
    )?;
    Ok(RuntimeCredentialState {
        tenant_id,
        client_id,
        login_endpoint,
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
) -> Result<(ServicePrincipalStateFile, StorageQueuesConfigFile), CompanionError> {
    let outputs: BicepOutputs = serde_json::from_str(json).map_err(|e| {
        CompanionError::Config(format!("failed to parse Bicep deployment outputs: {e}"))
    })?;

    let bootstrap_values: BicepBootstrapValues =
        serde_json::from_value(outputs.companion_bootstrap_values.value).map_err(|e| {
            CompanionError::Config(format!("failed to parse companionBootstrapValues: {e}"))
        })?;

    let sp = ServicePrincipalStateFile {
        tenant_id: bootstrap_values.tenant_id,
        client_id: bootstrap_values.client_id,
        login_endpoint: Some(bootstrap_values.login_endpoint),
        certificate_path: CERT_PEM_FILENAME.to_string(),
        private_key_path: KEY_PEM_FILENAME.to_string(),
    };

    let sb = StorageQueuesConfigFile {
        queue_endpoint: bootstrap_values.storage_queue_endpoint,
        upstream_queue: bootstrap_values.upstream_queue,
        downstream_queue: bootstrap_values.downstream_queue,
    };

    Ok((sp, sb))
}

fn default_bootstrap_image() -> String {
    format!(
        "{DEFAULT_BOOTSTRAP_IMAGE_REPOSITORY}:{}",
        env!("CARGO_PKG_VERSION")
    )
}

fn resolve_bootstrap_image(override_image: Option<&str>) -> Result<String, CompanionError> {
    match override_image {
        Some(image) => {
            let trimmed = image.trim();
            if trimmed.is_empty() {
                Err(CompanionError::Config(
                    "bootstrap image override must not be empty".into(),
                ))
            } else {
                Ok(trimmed.to_string())
            }
        }
        None => Ok(default_bootstrap_image()),
    }
}

fn device_code_regex() -> &'static Regex {
    static DEVICE_CODE_RE: OnceLock<Regex> = OnceLock::new();
    DEVICE_CODE_RE.get_or_init(|| {
        Regex::new(r"(?i)enter\s+the\s+code\s+([A-Z0-9-]+)\s+to\s+authenticate")
            .expect("valid device code regex")
    })
}

fn device_code_fallback_regex() -> &'static Regex {
    static DEVICE_CODE_FALLBACK_RE: OnceLock<Regex> = OnceLock::new();
    DEVICE_CODE_FALLBACK_RE.get_or_init(|| {
        Regex::new(r"(?i)microsoft\.com/devicelogin[^\n]*\b([A-Z0-9]{4}(?:-[A-Z0-9]{4})+)\b")
            .expect("valid fallback device code regex")
    })
}

fn extract_device_code(stderr_buffer: &str) -> Option<String> {
    device_code_regex()
        .captures(stderr_buffer)
        .and_then(|captures| captures.get(1))
        .map(|code| code.as_str().to_string())
        .or_else(|| {
            device_code_fallback_regex()
                .captures(stderr_buffer)
                .and_then(|captures| captures.get(1))
                .map(|code| code.as_str().to_string())
        })
}

fn trim_buffer_to_max_len(buffer: &mut String, max_len: usize) {
    if buffer.len() <= max_len {
        return;
    }

    let target_start = buffer.len() - max_len;
    let trim_start = buffer
        .char_indices()
        .find_map(|(idx, _)| (idx >= target_start).then_some(idx))
        .unwrap_or(buffer.len());
    buffer.drain(..trim_start);
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
    if let Some(domain) = &args.custom_domain_name {
        env.push(format!("SONDE_AZURE_CUSTOM_DOMAIN_NAME={domain}"));
    }
    if let Some(rg) = &args.custom_domain_dns_resource_group {
        env.push(format!("SONDE_AZURE_CUSTOM_DOMAIN_DNS_RESOURCE_GROUP={rg}"));
    }
    if let Some(zone) = &args.custom_domain_dns_zone_name {
        env.push(format!("SONDE_AZURE_CUSTOM_DOMAIN_DNS_ZONE_NAME={zone}"));
    }
    env
}

fn downstream_body_to_connector_payload(body: &[u8]) -> Result<Vec<u8>, CompanionError> {
    if body.len() > CONNECTOR_MAX_FRAME_LENGTH {
        return Err(CompanionError::Config(format!(
            "downstream message body length {} exceeds connector max frame length {}",
            body.len(),
            CONNECTOR_MAX_FRAME_LENGTH
        )));
    }
    Ok(body.to_vec())
}

fn build_storage_queue_credential(
    runtime_state: &RuntimeCredentialState,
) -> Result<Arc<dyn TokenCredential>, CompanionError> {
    let certificate_thumbprint = load_certificate_thumbprint(&runtime_state.certificate_path)?;
    let (signing_algorithm, signing_key) = load_signing_key(&runtime_state.private_key_path)?;
    let token_endpoint = format!(
        "{}/{}/oauth2/v2.0/token",
        runtime_state.login_endpoint, runtime_state.tenant_id
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

async fn get_storage_bearer_token(
    credential: &dyn TokenCredential,
) -> Result<String, CompanionError> {
    let token = credential
        .get_token(&[STORAGE_TOKEN_SCOPE], None)
        .await
        .map_err(|e| CompanionError::Config(format!("get Storage Queue token failed: {e}")))?;
    Ok(token.token.secret().to_string())
}

fn storage_queue_date_header() -> String {
    httpdate::fmt_http_date(std::time::SystemTime::now())
}

#[tonic::async_trait]
impl UpstreamPublisher for StorageQueuePublisher {
    async fn publish(&mut self, payload: Vec<u8>) -> Result<(), CompanionError> {
        let token = get_storage_bearer_token(&*self.credential).await?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&payload);
        let body = format!("<QueueMessage><MessageText>{encoded}</MessageText></QueueMessage>");
        let url = format!("{}/{}/messages", self.queue_endpoint, self.queue_name);
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-ms-version", STORAGE_QUEUE_API_VERSION)
            .header("x-ms-date", &storage_queue_date_header())
            .header("Content-Type", "application/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                CompanionError::Config(format!("send Storage Queue message failed: {e}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response>".to_string());
            return Err(CompanionError::Config(format!(
                "Storage Queue POST message returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

fn parse_queue_message_xml(xml: &str) -> Result<Option<StorageQueueMessage>, CompanionError> {
    // Parse the simple XML response from Azure Storage Queue GET messages.
    // Response contains <QueueMessagesList><QueueMessage>...</QueueMessage></QueueMessagesList>
    let message_start = match xml.find("<QueueMessage>") {
        Some(pos) => pos,
        None => return Ok(None),
    };
    let message_xml = &xml[message_start..];

    let message_id = extract_xml_element(message_xml, "MessageId")
        .ok_or_else(|| CompanionError::Config("missing MessageId in queue response".into()))?;
    let pop_receipt = extract_xml_element(message_xml, "PopReceipt")
        .ok_or_else(|| CompanionError::Config("missing PopReceipt in queue response".into()))?;
    let message_text = extract_xml_element(message_xml, "MessageText")
        .ok_or_else(|| CompanionError::Config("missing MessageText in queue response".into()))?;

    let body = base64::engine::general_purpose::STANDARD
        .decode(&message_text)
        .map_err(|e| {
            CompanionError::Config(format!("failed to base64-decode queue message body: {e}"))
        })?;

    Ok(Some(StorageQueueMessage {
        message_id,
        pop_receipt,
        body,
    }))
}

fn extract_xml_element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

#[tonic::async_trait]
impl DownstreamConsumer for StorageQueueConsumer {
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, CompanionError> {
        if self.inflight.is_some() {
            return Err(CompanionError::Config(
                "cannot receive a new downstream message while another message is still inflight"
                    .to_string(),
            ));
        }
        let token = get_storage_bearer_token(&*self.credential).await?;
        let url = format!(
            "{}/{}/messages?numofmessages=1&visibilitytimeout={STORAGE_QUEUE_VISIBILITY_TIMEOUT_SECS}",
            self.queue_endpoint, self.queue_name
        );
        let response = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-ms-version", STORAGE_QUEUE_API_VERSION)
            .header("x-ms-date", &storage_queue_date_header())
            .send()
            .await
            .map_err(|e| {
                CompanionError::Config(format!("receive Storage Queue message failed: {e}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response>".to_string());
            return Err(CompanionError::Config(format!(
                "Storage Queue GET messages returned {status}: {body}"
            )));
        }
        let xml = response.text().await.map_err(|e| {
            CompanionError::Config(format!("read Storage Queue response failed: {e}"))
        })?;

        let message = parse_queue_message_xml(&xml)?;
        if let Some(message) = message {
            let payload = downstream_body_to_connector_payload(&message.body);
            match payload {
                Ok(payload) => {
                    self.inflight = Some(message);
                    Ok(Some(payload))
                }
                Err(err) => {
                    // Abandon the message on decode error by making it visible again
                    let abandon_result = self
                        .abandon_message_direct(&message.message_id, &message.pop_receipt)
                        .await;
                    if let Err(abandon_err) = abandon_result {
                        eprintln!(
                            "failed to abandon downstream Storage Queue message after body decode error: {abandon_err}"
                        );
                    }
                    Err(err)
                }
            }
        } else {
            // Avoid spinning when the queue is empty — the REST API returns
            // immediately unlike Service Bus which blocked for a wait period.
            tokio::time::sleep(STORAGE_QUEUE_EMPTY_POLL_DELAY).await;
            Ok(None)
        }
    }

    async fn complete(&mut self) -> Result<(), CompanionError> {
        let inflight = self.inflight.as_ref().ok_or_else(|| {
            CompanionError::Config("no inflight downstream message to complete".to_string())
        })?;
        let token = get_storage_bearer_token(&*self.credential).await?;
        let pop_receipt = urlencoding_encode(&inflight.pop_receipt);
        let url = format!(
            "{}/{}/messages/{}?popreceipt={pop_receipt}",
            self.queue_endpoint, self.queue_name, inflight.message_id
        );
        let response = self
            .http_client
            .delete(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-ms-version", STORAGE_QUEUE_API_VERSION)
            .header("x-ms-date", &storage_queue_date_header())
            .send()
            .await
            .map_err(|e| {
                CompanionError::Config(format!("delete Storage Queue message failed: {e}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response>".to_string());
            return Err(CompanionError::Config(format!(
                "Storage Queue DELETE message returned {status}: {body}"
            )));
        }
        self.inflight = None;
        Ok(())
    }

    async fn abandon(&mut self) -> Result<(), CompanionError> {
        let inflight = self.inflight.as_ref().ok_or_else(|| {
            CompanionError::Config("no inflight downstream message to abandon".to_string())
        })?;
        let message_id = inflight.message_id.clone();
        let pop_receipt = inflight.pop_receipt.clone();
        self.abandon_message_direct(&message_id, &pop_receipt)
            .await?;
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

impl StorageQueueConsumer {
    async fn abandon_message_direct(
        &self,
        message_id: &str,
        pop_receipt: &str,
    ) -> Result<(), CompanionError> {
        let token = get_storage_bearer_token(&*self.credential).await?;
        let encoded_receipt = urlencoding_encode(pop_receipt);
        let url = format!(
            "{}/{}/messages/{message_id}?popreceipt={encoded_receipt}&visibilitytimeout=0",
            self.queue_endpoint, self.queue_name
        );
        let response = self
            .http_client
            .put(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-ms-version", STORAGE_QUEUE_API_VERSION)
            .header("x-ms-date", &storage_queue_date_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| {
                CompanionError::Config(format!("abandon Storage Queue message failed: {e}"))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read response>".to_string());
            return Err(CompanionError::Config(format!(
                "Storage Queue UPDATE message (abandon) returned {status}: {body}"
            )));
        }
        Ok(())
    }
}

fn urlencoding_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                String::from(b as char)
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tonic::async_trait]
impl BrokerTransportFactory for StorageQueueTransportFactory {
    type Publisher = StorageQueuePublisher;
    type Consumer = StorageQueueConsumer;

    async fn connect(
        &self,
        runtime_config: &RuntimeConfig,
        runtime_state: &RuntimeCredentialState,
    ) -> Result<(Self::Publisher, Self::Consumer), CompanionError> {
        let credential = build_storage_queue_credential(runtime_state)?;
        let http_client = reqwest::Client::new();

        let mut endpoint = runtime_config.queue_endpoint.clone();
        if endpoint.ends_with('/') {
            endpoint.pop();
        }

        let publisher = StorageQueuePublisher {
            queue_endpoint: endpoint.clone(),
            queue_name: runtime_config.upstream_queue.clone(),
            credential: Arc::clone(&credential),
            http_client: http_client.clone(),
        };

        let consumer = StorageQueueConsumer {
            queue_endpoint: endpoint,
            queue_name: runtime_config.downstream_queue.clone(),
            credential,
            http_client,
            inflight: None,
        };

        Ok((publisher, consumer))
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
            eprintln!("failed to abandon downstream queue message after connector write error: {abandon_err}");
        }
        return Err(err);
    }

    if let Err(err) = consumer.complete().await {
        if let Err(abandon_err) = consumer.abandon().await {
            eprintln!(
                "failed to abandon downstream queue message after completion error: {abandon_err}"
            );
        }
        return Err(err);
    }
    Ok(())
}

#[cfg(all(test, unix))]
async fn bridge_runtime<T, P, C>(stream: T, publisher: P, consumer: C) -> Result<(), CompanionError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    P: UpstreamPublisher + 'static,
    C: DownstreamConsumer + 'static,
{
    bridge_runtime_with_shutdown(stream, publisher, consumer, std::future::pending::<()>()).await
}

async fn bridge_runtime_with_shutdown<T, P, C, S>(
    stream: T,
    mut publisher: P,
    mut consumer: C,
    shutdown: S,
) -> Result<(), CompanionError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    P: UpstreamPublisher + 'static,
    C: DownstreamConsumer + 'static,
    S: std::future::Future<Output = ()>,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let upstream = async move {
        while pump_upstream_once(&mut reader, &mut publisher).await? {}
        Ok::<(), CompanionError>(())
    };
    tokio::pin!(upstream);
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            result = &mut upstream => {
                if let Err(abandon_err) = consumer.abandon_inflight().await {
                    eprintln!("failed to abandon downstream queue message during bridge shutdown: {abandon_err}");
                }
                return result;
            }
            result = pump_downstream_once(&mut writer, &mut consumer) => {
                if let Err(err) = result {
                    if let Err(abandon_err) = consumer.abandon_inflight().await {
                        eprintln!("failed to abandon downstream queue message after downstream error: {abandon_err}");
                    }
                    return Err(err);
                }
            }
            _ = &mut shutdown => {
                if let Err(abandon_err) = consumer.abandon_inflight().await {
                    eprintln!("failed to abandon downstream queue message during service shutdown: {abandon_err}");
                }
                return Ok(());
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
    run_checked_with_factory_and_shutdown(
        connector_socket,
        runtime_config,
        runtime_state,
        factory,
        std::future::pending::<()>(),
    )
    .await
}

async fn run_checked_with_factory_and_shutdown<F, S>(
    connector_socket: &str,
    runtime_config: &RuntimeConfig,
    runtime_state: &RuntimeCredentialState,
    factory: &F,
    shutdown: S,
) -> Result<(), CompanionError>
where
    F: BrokerTransportFactory,
    F::Publisher: 'static,
    F::Consumer: 'static,
    S: std::future::Future<Output = ()>,
{
    let (publisher, consumer) = factory.connect(runtime_config, runtime_state).await?;
    let stream = connect_connector(connector_socket).await?;
    eprintln!(
        "connected to gateway connector at {connector_socket} and Azure Storage Queue endpoint {}",
        runtime_config.queue_endpoint
    );
    bridge_runtime_with_shutdown(stream, publisher, consumer, shutdown).await
}

async fn run(connector_socket: &str, state_dir: &Path) -> Result<(), CompanionError> {
    run_with_factory(connector_socket, state_dir, &StorageQueueTransportFactory).await
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
    let mut deployment_displayed = false;

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
                trim_buffer_to_max_len(&mut stderr_buffer, MAX_STDERR_BUFFER_LEN);

                if !deployment_displayed
                    && stderr_buffer.contains("__SONDE_AZURE_DEPLOYMENT_START__")
                {
                    display_progress(admin_socket, "Deploying Azure...").await?;
                    deployment_displayed = true;
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
    cert_base64: &str,
    args: &BootstrapArgs,
) -> Result<(ServicePrincipalStateFile, StorageQueuesConfigFile), CompanionError> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| CompanionError::Config(format!("failed to connect to Docker daemon: {e}")))?;
    run_bootstrap_deployment_with_docker(&docker, admin_socket, cert_base64, args).await
}

async fn run_bootstrap_deployment_with_docker(
    docker: &Docker,
    admin_socket: &str,
    cert_base64: &str,
    args: &BootstrapArgs,
) -> Result<(ServicePrincipalStateFile, StorageQueuesConfigFile), CompanionError> {
    let bootstrap_image = resolve_bootstrap_image(args.bootstrap_image.as_deref())?;
    run_bootstrap_deployment_with_docker_and_image(
        docker,
        admin_socket,
        cert_base64,
        args,
        &bootstrap_image,
    )
    .await
}

async fn run_bootstrap_deployment_with_docker_and_image(
    docker: &Docker,
    admin_socket: &str,
    cert_base64: &str,
    args: &BootstrapArgs,
    bootstrap_image: &str,
) -> Result<(ServicePrincipalStateFile, StorageQueuesConfigFile), CompanionError> {
    eprintln!("Pulling bootstrap image...");
    let pull_opts = CreateImageOptionsBuilder::default()
        .from_image(bootstrap_image)
        .build();
    let mut pull_stream = docker.create_image(Some(pull_opts), None, None);
    while let Some(result) = pull_stream.next().await {
        result.map_err(|e| {
            CompanionError::Config(format!(
                "failed to pull bootstrap image {bootstrap_image}: {e}"
            ))
        })?;
    }

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
                image: Some(bootstrap_image.to_string()),
                env: Some(env_vars),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            CompanionError::Config(format!("failed to create bootstrap container: {e}"))
        })?;
    let container_id = container.id;

    let bootstrap_result = async {
        docker
            .start_container(&container_id, None)
            .await
            .map_err(|e| {
                CompanionError::Config(format!("failed to start bootstrap container: {e}"))
            })?;

        let stdout_output = stream_container_output(docker, &container_id, admin_socket).await?;
        let wait_options = WaitContainerOptionsBuilder::default()
            .condition("not-running")
            .build();
        let mut wait_stream = docker.wait_container(&container_id, Some(wait_options));
        let exit_result = wait_stream.next().await;

        match exit_result {
            Some(Ok(result)) if result.status_code == 0 => {}
            Some(Ok(result)) => {
                return Err(CompanionError::Config(format!(
                    "bootstrap container exited with non-zero status: {}",
                    result.status_code
                )));
            }
            Some(Err(e)) => {
                return Err(CompanionError::Config(format!(
                    "failed to wait for bootstrap container: {e}"
                )));
            }
            None => {
                return Err(CompanionError::Config(
                    "bootstrap container wait stream ended unexpectedly".into(),
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
    if let Err(display_err) = display_progress(admin_socket, "Bootstrap failed").await {
        return Err(CompanionError::Config(format!(
            "bootstrap failed: {err}; additionally failed to display modem error: {display_err}"
        )));
    }
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

    if let Err(err) = display_progress(admin_socket, "Authenticating...").await {
        cleanup_staging(&staging_dir);
        return Err(err);
    }
    eprintln!("Starting sonde-azure-bootstrap for device-code auth and Bicep deployment");
    let (sp_state, sb_config) =
        match run_bootstrap_deployment(admin_socket, &cert_base64, &args).await {
            Ok(result) => result,
            Err(e) => return report_bootstrap_failure(admin_socket, &staging_dir, e).await,
        };

    if let Err(err) = display_progress(admin_socket, "Writing config...").await {
        cleanup_staging(&staging_dir);
        return Err(err);
    }
    eprintln!("Writing runtime artifacts to state volume");

    let sp_path = staging_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME);
    let sp_json = serde_json::to_string_pretty(&sp_state)?;
    std::fs::write(&sp_path, sp_json.as_bytes())?;

    let sb_path = staging_dir.join(STORAGE_QUEUES_CONFIG_FILENAME);
    let sb_json = serde_json::to_string_pretty(&sb_config)?;
    std::fs::write(&sb_path, sb_json.as_bytes())?;

    if let Err(e) = commit_staging(&staging_dir, state_dir) {
        return report_bootstrap_failure(admin_socket, &staging_dir, e).await;
    }

    display_progress(admin_socket, "Bootstrap complete").await?;
    eprintln!("Bootstrap completed successfully");

    Ok(())
}

#[cfg(windows)]
fn build_service_launch_args(cli: &Cli) -> Vec<std::ffi::OsString> {
    vec![
        std::ffi::OsString::from("--admin-socket"),
        std::ffi::OsString::from(&cli.admin_socket),
        std::ffi::OsString::from("--connector-socket"),
        std::ffi::OsString::from(&cli.connector_socket),
        std::ffi::OsString::from("--state-dir"),
        cli.state_dir.as_os_str().to_os_string(),
        std::ffi::OsString::from("service"),
    ]
}

#[cfg(windows)]
fn install_service(cli: &Cli) -> Result<(), CompanionError> {
    use windows_service::service::{
        ServiceAccess, ServiceDependency, ServiceErrorControl, ServiceInfo, ServiceStartType,
        ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SERVICE_EXISTS};

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|err| match err {
        windows_service::Error::Winapi(win_err)
            if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
        {
            CompanionError::Config(
                "Administrator privileges are required to install the service.".to_string(),
            )
        }
        _ => CompanionError::Config(format!("failed to open Service Control Manager: {err}")),
    })?;

    let exe_path = std::env::current_exe()?;
    let service_info = ServiceInfo {
        name: std::ffi::OsString::from(SERVICE_NAME),
        display_name: std::ffi::OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: build_service_launch_args(cli),
        dependencies: vec![ServiceDependency::Service(std::ffi::OsString::from(
            "sonde-gateway",
        ))],
        account_name: None,
        account_password: None,
    };

    match manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG) {
        Ok(service) => {
            service
                .set_description(SERVICE_DESCRIPTION)
                .map_err(|err| {
                    CompanionError::Config(format!("failed to set service description: {err}"))
                })?;
        }
        Err(windows_service::Error::Winapi(err))
            if err.raw_os_error() == Some(ERROR_SERVICE_EXISTS as i32) =>
        {
            let service = manager
                .open_service(SERVICE_NAME, ServiceAccess::CHANGE_CONFIG)
                .map_err(|open_err| match open_err {
                    windows_service::Error::Winapi(win_err)
                        if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                    {
                        CompanionError::Config(
                            "Administrator privileges are required to update the service."
                                .to_string(),
                        )
                    }
                    _ => CompanionError::Config(format!(
                        "failed to open existing {SERVICE_NAME} service: {open_err}"
                    )),
                })?;
            service
                .change_config(&service_info)
                .map_err(|change_err| match change_err {
                    windows_service::Error::Winapi(win_err)
                        if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                    {
                        CompanionError::Config(
                            "Administrator privileges are required to update the service."
                                .to_string(),
                        )
                    }
                    _ => CompanionError::Config(format!(
                        "failed to update existing {SERVICE_NAME} service: {change_err}"
                    )),
                })?;
            service
                .set_description(SERVICE_DESCRIPTION)
                .map_err(|err| {
                    CompanionError::Config(format!("failed to set service description: {err}"))
                })?;
        }
        Err(err) => {
            return Err(match err {
                windows_service::Error::Winapi(win_err)
                    if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                {
                    CompanionError::Config(
                        "Administrator privileges are required to install the service.".to_string(),
                    )
                }
                _ => CompanionError::Config(format!(
                    "failed to create {SERVICE_NAME} service: {err}"
                )),
            });
        }
    }

    println!("{SERVICE_NAME} service installed successfully.");
    Ok(())
}

#[cfg(windows)]
fn uninstall_service() -> Result<(), CompanionError> {
    use std::time::{Duration, Instant};
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SERVICE_DOES_NOT_EXIST};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| match err {
            windows_service::Error::Winapi(win_err)
                if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
            {
                CompanionError::Config(
                    "Administrator privileges are required to uninstall the service.".to_string(),
                )
            }
            _ => CompanionError::Config(format!("failed to open Service Control Manager: {err}")),
        })?;

    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    ) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(err))
            if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
        {
            println!("{SERVICE_NAME} service is not registered.");
            return Ok(());
        }
        Err(err) => {
            return Err(match err {
                windows_service::Error::Winapi(win_err)
                    if win_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                {
                    CompanionError::Config(
                        "Administrator privileges are required to uninstall the service."
                            .to_string(),
                    )
                }
                _ => {
                    CompanionError::Config(format!("failed to open {SERVICE_NAME} service: {err}"))
                }
            });
        }
    };

    let mut status = service.query_status().map_err(|err| {
        CompanionError::Config(format!("failed to query {SERVICE_NAME} status: {err}"))
    })?;
    if status.current_state != ServiceState::Stopped {
        if status.current_state != ServiceState::StopPending {
            service.stop().map_err(|err| {
                CompanionError::Config(format!("failed to stop {SERVICE_NAME} service: {err}"))
            })?;
            println!("Stopping {SERVICE_NAME} service...");
        }

        let stop_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < stop_deadline {
            status = service.query_status().map_err(|err| {
                CompanionError::Config(format!("failed to query {SERVICE_NAME} status: {err}"))
            })?;
            if status.current_state == ServiceState::Stopped {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        if status.current_state != ServiceState::Stopped {
            return Err(CompanionError::Config(format!(
                "timed out waiting for {SERVICE_NAME} service to stop"
            )));
        }
    }

    service.delete().map_err(|err| {
        CompanionError::Config(format!("failed to delete {SERVICE_NAME} service: {err}"))
    })?;

    drop(service);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
            Err(windows_service::Error::Winapi(err))
                if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
            {
                println!("{SERVICE_NAME} service uninstalled successfully.");
                return Ok(());
            }
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }

    println!("{SERVICE_NAME} service is marked for deletion.");
    Ok(())
}

#[cfg(windows)]
fn set_service_status(
    status_handle: &windows_service::service_control_handler::ServiceStatusHandle,
    current_state: windows_service::service::ServiceState,
    controls_accepted: windows_service::service::ServiceControlAccept,
    exit_code: u32,
) {
    set_service_status_with_progress(
        status_handle,
        current_state,
        controls_accepted,
        exit_code,
        0,
        Duration::default(),
    );
}

#[cfg(windows)]
fn set_service_status_with_progress(
    status_handle: &windows_service::service_control_handler::ServiceStatusHandle,
    current_state: windows_service::service::ServiceState,
    controls_accepted: windows_service::service::ServiceControlAccept,
    exit_code: u32,
    checkpoint: u32,
    wait_hint: Duration,
) {
    use windows_service::service::{ServiceExitCode, ServiceStatus, ServiceType};

    let status = ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state,
        controls_accepted,
        exit_code: if exit_code == 0 {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(exit_code)
        },
        checkpoint,
        wait_hint,
        process_id: None,
    };
    let _ = status_handle.set_service_status(status);
}

#[cfg(windows)]
fn log_service_diagnostic(state_dir: &Path, message: &str) {
    let log_path = state_dir.join("service.log");
    if let Err(err) = std::fs::create_dir_all(state_dir) {
        eprintln!(
            "{SERVICE_NAME}: failed to create service diagnostic directory {}: {err}",
            state_dir.display()
        );
        return;
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            let _ = writeln!(file, "{message}");
        }
        Err(err) => {
            eprintln!(
                "{SERVICE_NAME}: failed to write service diagnostic log {}: {err}",
                log_path.display()
            );
        }
    }
}

#[cfg(windows)]
fn service_entry(_arguments: Vec<std::ffi::OsString>) {
    use std::sync::{Arc, Mutex};
    use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceState};
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };

    let cli = match SERVICE_CLI.get() {
        Some(cli) => cli,
        None => return,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_tx = Arc::new(Mutex::new(Some(shutdown_tx)));
    let status_handle_slot = Arc::new(Mutex::new(None::<ServiceStatusHandle>));
    let status_handle_slot_for_events = Arc::clone(&status_handle_slot);

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Ok(guard) = status_handle_slot_for_events.lock() {
                    if let Some(handle) = *guard {
                        set_service_status_with_progress(
                            &handle,
                            ServiceState::StopPending,
                            ServiceControlAccept::empty(),
                            0,
                            1,
                            Duration::from_secs(30),
                        );
                    }
                }
                if let Ok(mut guard) = shutdown_tx.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(());
                    }
                }
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = match service_control_handler::register(SERVICE_NAME, event_handler) {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("{SERVICE_NAME}: failed to register service control handler: {err}");
            log_service_diagnostic(
                &cli.state_dir,
                &format!("{SERVICE_NAME}: failed to register service control handler: {err}"),
            );
            return;
        }
    };
    if let Ok(mut guard) = status_handle_slot.lock() {
        *guard = Some(status_handle);
    }

    set_service_status(
        &status_handle,
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        0,
    );

    let (runtime_config, runtime_state) = match check_runtime_ready(&cli.state_dir) {
        Ok(ready) => ready,
        Err(err) => {
            eprintln!("{err}");
            log_service_diagnostic(&cli.state_dir, &err.to_string());
            set_service_status(
                &status_handle,
                ServiceState::Stopped,
                ServiceControlAccept::empty(),
                1,
            );
            return;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("failed to create tokio runtime for {SERVICE_NAME}: {err}");
            log_service_diagnostic(
                &cli.state_dir,
                &format!("failed to create tokio runtime for {SERVICE_NAME}: {err}"),
            );
            set_service_status(
                &status_handle,
                ServiceState::Stopped,
                ServiceControlAccept::empty(),
                1,
            );
            return;
        }
    };

    set_service_status(
        &status_handle,
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
    );

    let exit_code = match runtime.block_on(run_checked_with_factory_and_shutdown(
        &cli.connector_socket,
        &runtime_config,
        &runtime_state,
        &StorageQueueTransportFactory,
        async move {
            let _ = shutdown_rx.await;
        },
    )) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            log_service_diagnostic(&cli.state_dir, &err.to_string());
            1
        }
    };

    set_service_status(
        &status_handle,
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
    );
}

async fn run_cli() -> Result<(), CompanionError> {
    let cli = Cli::parse();
    match cli.command.clone().unwrap_or(Command::Run) {
        Command::Run => run(&cli.connector_socket, &cli.state_dir).await?,
        Command::Bootstrap(args) => bootstrap(&cli.admin_socket, &cli.state_dir, args).await?,
        Command::DisplayMessage { lines } => display_message(&cli.admin_socket, lines).await?,
        Command::CheckRuntimeReady => {
            check_runtime_ready(&cli.state_dir)?;
        }
        #[cfg(windows)]
        Command::Install => install_service(&cli)?,
        #[cfg(windows)]
        Command::Uninstall => uninstall_service()?,
        #[cfg(windows)]
        Command::Service => {
            SERVICE_CLI.set(cli.clone()).map_err(|_| {
                CompanionError::Config("duplicate Windows service dispatch".to_string())
            })?;
            windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(
                |err| {
                    CompanionError::Config(format!(
                        "failed to start Windows service dispatcher: {err}"
                    ))
                },
            )?;
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
    #[cfg(unix)]
    use super::bridge_runtime_with_shutdown;
    #[cfg(windows)]
    use super::build_service_launch_args;
    #[cfg(windows)]
    use super::default_state_dir;
    #[cfg(unix)]
    use super::validate_certificate_matches_private_key;
    use super::{
        check_runtime_ready, cleanup_staging, commit_staging, default_bootstrap_image,
        downstream_body_to_connector_payload, extract_device_code, extract_xml_element,
        generate_certificate, load_runtime_config, load_runtime_credential_state, load_signing_key,
        parse_bicep_outputs, parse_queue_message_xml, prepare_staging_dir, pump_downstream_once,
        pump_upstream_once, read_framed, resolve_bootstrap_image, resolve_effective_state_dir,
        resolve_login_endpoint, resolve_state_relative_path,
        run_bootstrap_deployment_with_docker_and_image, trim_buffer_to_max_len, urlencoding_encode,
        validate_display_lines, write_framed, ClientAssertionCredential, CompanionError,
        DownstreamConsumer, RuntimeConfig, RuntimeCredentialState, ServicePrincipalStateFile,
        StorageQueuesConfigFile, UpstreamPublisher, ACTIVE_STATE_FILENAME, CERT_PEM_FILENAME,
        CONNECTOR_MAX_FRAME_LENGTH, DEFAULT_LOGIN_ENDPOINT, KEY_PEM_FILENAME,
        SERVICE_PRINCIPAL_STATE_FILENAME, STATE_GENERATION_PREFIX, STORAGE_QUEUES_CONFIG_FILENAME,
    };
    #[cfg(windows)]
    use super::{
        configured_windows_service_sid_string, current_user_sid_string, sid_to_string, wide_null,
        windows_private_key_sddl, Cli, Command,
    };
    use azure_core::credentials::TokenCredential;
    use base64::Engine as _;
    use bollard::{Docker, API_DEFAULT_VERSION};
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
    use wiremock::matchers::{method, path, path_regex};
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
            login_endpoint: None,
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
            login_endpoint: None,
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
                    "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                    Some("https://example.queue.core.windows.net"),
                ),
                ("SONDE_AZURE_STORAGE_UPSTREAM_QUEUE", Some("upstream")),
                ("SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE", Some("downstream")),
            ],
            test,
        );
    }

    fn write_storage_queues_config(
        temp: &TempDir,
        endpoint: &str,
        upstream: &str,
        downstream: &str,
    ) {
        let config = StorageQueuesConfigFile {
            queue_endpoint: endpoint.to_string(),
            upstream_queue: upstream.to_string(),
            downstream_queue: downstream.to_string(),
        };
        std::fs::write(
            temp.path().join(STORAGE_QUEUES_CONFIG_FILENAME),
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

        #[cfg(unix)]
        {
            validate_certificate_matches_private_key(&cert_path, &key_path).unwrap();

            use std::os::unix::fs::PermissionsExt;

            let key_mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(key_mode, 0o600);
        }

        #[cfg(windows)]
        {
            let key_pem = std::fs::read_to_string(&key_path).unwrap();
            assert!(key_pem.contains("BEGIN PRIVATE KEY"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_key_sddl_grants_current_user_and_runtime_service_identity() {
        let current_user_sid = current_user_sid_string().unwrap();
        let runtime_service_sid = configured_windows_service_sid_string().unwrap();
        let sddl = windows_private_key_sddl().unwrap();
        assert!(sddl.starts_with("D:P"));
        assert!(sddl.contains(&format!("(A;;FA;;;{current_user_sid})")));
        if runtime_service_sid != current_user_sid {
            assert!(sddl.contains(&format!("(A;;GR;;;{runtime_service_sid})")));
        }
    }

    #[cfg(windows)]
    fn read_allowed_file_acl_entries(path: &Path) -> Result<Vec<(String, u32)>, CompanionError> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            GetAce, ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        };

        const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

        let path_wide = wide_null(path.as_os_str());
        let mut dacl = std::ptr::null_mut();
        let mut security_descriptor = std::ptr::null_mut();
        let error = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut security_descriptor,
            )
        };
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32).into());
        }

        let result = (|| -> Result<Vec<(String, u32)>, CompanionError> {
            if dacl.is_null() {
                return Err(CompanionError::Config(
                    "private key file did not have a DACL".to_string(),
                ));
            }

            let ace_count = unsafe { (*dacl).AceCount };
            let mut entries = Vec::with_capacity(ace_count as usize);
            for index in 0..ace_count {
                let mut ace_ptr = std::ptr::null_mut();
                if unsafe { GetAce(dacl, index.into(), &mut ace_ptr) } == 0 {
                    return Err(std::io::Error::last_os_error().into());
                }

                let header =
                    unsafe { &*(ace_ptr.cast::<windows_sys::Win32::Security::ACE_HEADER>()) };
                if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                    continue;
                }

                let ace = unsafe { &*(ace_ptr.cast::<ACCESS_ALLOWED_ACE>()) };
                let sid = sid_to_string(
                    std::ptr::addr_of!(ace.SidStart)
                        .cast::<u32>()
                        .cast_mut()
                        .cast(),
                )?;
                entries.push((sid, ace.Mask));
            }

            Ok(entries)
        })();

        unsafe {
            let _ = LocalFree(security_descriptor.cast());
        }

        result
    }

    #[cfg(windows)]
    #[test]
    fn generated_private_key_acl_excludes_broad_principals_and_allows_runtime_identities() {
        use windows_sys::Win32::Foundation::{GENERIC_ALL, GENERIC_READ};
        use windows_sys::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_GENERIC_READ};

        let temp = TempDir::new().unwrap();
        let (_cert_path, key_path, _cert_base64) = generate_certificate(temp.path()).unwrap();
        let acl_entries = read_allowed_file_acl_entries(&key_path).unwrap();
        let current_user_sid = current_user_sid_string().unwrap();
        let runtime_service_sid = configured_windows_service_sid_string().unwrap();

        assert!(!acl_entries.is_empty());
        assert!(!acl_entries
            .iter()
            .any(|(sid, _)| sid == "S-1-1-0" || sid == "S-1-5-32-545"));

        let current_user_mask = acl_entries
            .iter()
            .find(|(sid, _)| sid == &current_user_sid)
            .map(|(_, mask)| *mask)
            .unwrap();
        assert!(
            (current_user_mask & FILE_ALL_ACCESS) == FILE_ALL_ACCESS
                || (current_user_mask & GENERIC_ALL) == GENERIC_ALL
        );

        if runtime_service_sid != current_user_sid {
            let runtime_service_mask = acl_entries
                .iter()
                .find(|(sid, _)| sid == &runtime_service_sid)
                .map(|(_, mask)| *mask)
                .unwrap();
            assert!(
                (runtime_service_mask & FILE_GENERIC_READ) == FILE_GENERIC_READ
                    || (runtime_service_mask & GENERIC_READ) == GENERIC_READ
                    || (runtime_service_mask & FILE_ALL_ACCESS) == FILE_ALL_ACCESS
                    || (runtime_service_mask & GENERIC_ALL) == GENERIC_ALL
            );
        }
    }

    #[test]
    fn test_load_runtime_config_from_file() {
        let temp = TempDir::new().unwrap();
        write_storage_queues_config(
            &temp,
            "https://file.queue.core.windows.net",
            "file-up",
            "file-down",
        );

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE",
                "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE",
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        queue_endpoint: "https://file.queue.core.windows.net".to_string(),
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
        write_storage_queues_config(
            &temp,
            "https://file.queue.core.windows.net",
            "file-up",
            "file-down",
        );

        temp_env::with_vars(
            [
                (
                    "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                    Some("https://env.queue.core.windows.net"),
                ),
                ("SONDE_AZURE_STORAGE_UPSTREAM_QUEUE", Some("env-up")),
                ("SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE", Some("env-down")),
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        queue_endpoint: "https://env.queue.core.windows.net".to_string(),
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
        write_storage_queues_config(
            &temp,
            "  https://file.queue.core.windows.net  ",
            "  file-up  ",
            "  file-down  ",
        );

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE",
                "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE",
            ],
            || {
                let config = load_runtime_config(temp.path()).unwrap();
                assert_eq!(
                    config,
                    RuntimeConfig {
                        queue_endpoint: "https://file.queue.core.windows.net".to_string(),
                        upstream_queue: "file-up".to_string(),
                        downstream_queue: "file-down".to_string(),
                    }
                );
            },
        );
    }

    #[test]
    fn test_load_runtime_config_rejects_blank_storage_queues_file_values() {
        let temp = TempDir::new().unwrap();
        write_storage_queues_config(
            &temp,
            "https://example.queue.core.windows.net",
            "  ",
            "downstream",
        );

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE",
                "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE",
            ],
            || {
                let err = load_runtime_config(temp.path()).unwrap_err();
                assert!(err
                    .to_string()
                    .contains("storage-queues.json upstream_queue must be set and non-empty"));
            },
        );
    }

    #[test]
    fn test_load_runtime_config_surfaces_invalid_storage_queues_json() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join(STORAGE_QUEUES_CONFIG_FILENAME),
            b"{not valid json",
        )
        .unwrap();

        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE",
                "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE",
            ],
            || {
                let err = load_runtime_config(temp.path()).unwrap_err();
                assert!(matches!(err, CompanionError::Json(_)));
            },
        );
    }

    #[test]
    fn default_bootstrap_image_uses_package_version_tag() {
        assert_eq!(
            default_bootstrap_image(),
            format!(
                "ghcr.io/alan-jowett/sonde-azure-bootstrap:{}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn resolve_bootstrap_image_uses_override_when_provided() {
        assert_eq!(
            resolve_bootstrap_image(Some("sonde-azure-bootstrap:test-override")).unwrap(),
            "sonde-azure-bootstrap:test-override"
        );
    }

    #[test]
    fn resolve_bootstrap_image_trims_override() {
        assert_eq!(
            resolve_bootstrap_image(Some("  sonde-azure-bootstrap:test-override  ")).unwrap(),
            "sonde-azure-bootstrap:test-override"
        );
    }

    #[test]
    fn resolve_bootstrap_image_rejects_empty_override() {
        let err = resolve_bootstrap_image(Some("   ")).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
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
    fn extract_device_code_is_case_insensitive() {
        let buffer = "ENTER THE CODE WXYZ-1234 TO AUTHENTICATE";
        assert_eq!(extract_device_code(buffer).as_deref(), Some("WXYZ-1234"));
    }

    #[test]
    fn extract_device_code_supports_devicelogin_fallback_pattern() {
        let buffer = "Open https://microsoft.com/devicelogin and use code QRST-UVWX when prompted.";
        assert_eq!(extract_device_code(buffer).as_deref(), Some("QRST-UVWX"));
    }

    #[test]
    fn trim_buffer_to_max_len_preserves_utf8_boundaries() {
        let mut buffer = "a🙂".repeat(2_000);
        trim_buffer_to_max_len(&mut buffer, 4_096);
        assert!(buffer.len() <= 4_096);
        assert!(std::str::from_utf8(buffer.as_bytes()).is_ok());
        assert!(buffer.ends_with("a🙂"));
    }

    #[test]
    fn test_parse_bicep_outputs() {
        let json = r#"{
            "companionBootstrapValues": {
                "value": {
                    "tenantId": "11111111-1111-1111-1111-111111111111",
                    "clientId": "22222222-2222-2222-2222-222222222222",
                    "loginEndpoint": "https://login.microsoftonline.com/",
                    "storageQueueEndpoint": "https://example.queue.core.windows.net",
                    "upstreamQueue": "upstream",
                    "downstreamQueue": "downstream"
                }
            }
        }"#;

        let (sp, sb) = parse_bicep_outputs(json).unwrap();
        assert_eq!(
            sp,
            ServicePrincipalStateFile {
                tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                login_endpoint: Some("https://login.microsoftonline.com/".to_string()),
                certificate_path: CERT_PEM_FILENAME.to_string(),
                private_key_path: KEY_PEM_FILENAME.to_string(),
            }
        );
        assert_eq!(
            sb,
            StorageQueuesConfigFile {
                queue_endpoint: "https://example.queue.core.windows.net".to_string(),
                upstream_queue: "upstream".to_string(),
                downstream_queue: "downstream".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_bicep_outputs_accepts_wrapped_nested_values() {
        let json = r#"{
            "companionBootstrapValues": {
                "value": {
                    "tenantId": { "value": "11111111-1111-1111-1111-111111111111" },
                    "clientId": { "value": "22222222-2222-2222-2222-222222222222" },
                    "loginEndpoint": { "value": "https://login.microsoftonline.com/" },
                    "storageQueueEndpoint": { "value": "https://example.queue.core.windows.net" },
                    "upstreamQueue": { "value": "upstream" },
                    "downstreamQueue": { "value": "downstream" }
                }
            }
        }"#;

        let (sp, sb) = parse_bicep_outputs(json).unwrap();
        assert_eq!(sp.tenant_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(sp.client_id, "22222222-2222-2222-2222-222222222222");
        assert_eq!(
            sp.login_endpoint.as_deref(),
            Some("https://login.microsoftonline.com/")
        );
        assert_eq!(sb.queue_endpoint, "https://example.queue.core.windows.net");
        assert_eq!(sb.upstream_queue, "upstream");
        assert_eq!(sb.downstream_queue, "downstream");
    }

    #[test]
    fn test_parse_bicep_outputs_rejects_missing_login_endpoint() {
        let json = r#"{
            "companionBootstrapValues": {
                "value": {
                    "tenantId": "11111111-1111-1111-1111-111111111111",
                    "clientId": "22222222-2222-2222-2222-222222222222",
                    "storageQueueEndpoint": "https://example.queue.core.windows.net",
                    "upstreamQueue": "upstream",
                    "downstreamQueue": "downstream"
                }
            }
        }"#;

        let err = parse_bicep_outputs(json).unwrap_err();
        assert!(err
            .to_string()
            .contains("failed to parse companionBootstrapValues"));
    }

    #[test]
    fn test_staged_commit() {
        let temp = TempDir::new().unwrap();
        let staging_dir = prepare_staging_dir(temp.path()).unwrap();
        std::fs::write(staging_dir.join(CERT_PEM_FILENAME), b"new-cert").unwrap();
        std::fs::write(staging_dir.join(KEY_PEM_FILENAME), b"new-key").unwrap();
        std::fs::write(
            staging_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME),
            serde_json::to_vec(&ServicePrincipalStateFile {
                tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                login_endpoint: None,
                certificate_path: CERT_PEM_FILENAME.to_string(),
                private_key_path: KEY_PEM_FILENAME.to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            staging_dir.join(STORAGE_QUEUES_CONFIG_FILENAME),
            serde_json::to_vec(&StorageQueuesConfigFile {
                queue_endpoint: "https://example.queue.core.windows.net".to_string(),
                upstream_queue: "upstream".to_string(),
                downstream_queue: "downstream".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(temp.path().join(CERT_PEM_FILENAME), b"old-cert").unwrap();

        commit_staging(&staging_dir, temp.path()).unwrap();

        let active_marker = temp.path().join(ACTIVE_STATE_FILENAME);
        assert!(active_marker.exists());
        let effective_state_dir = resolve_effective_state_dir(temp.path()).unwrap();
        assert_ne!(effective_state_dir, temp.path());
        assert_eq!(
            std::fs::read(effective_state_dir.join(CERT_PEM_FILENAME)).unwrap(),
            b"new-cert"
        );
        assert_eq!(
            std::fs::read(effective_state_dir.join(KEY_PEM_FILENAME)).unwrap(),
            b"new-key"
        );
        let runtime_state = load_runtime_credential_state(temp.path()).unwrap();
        assert_eq!(
            runtime_state.certificate_path,
            effective_state_dir
                .join(CERT_PEM_FILENAME)
                .canonicalize()
                .unwrap()
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
    fn test_staged_commit_rolls_back_generation_on_marker_update_failure() {
        let temp = TempDir::new().unwrap();
        let staging_dir = prepare_staging_dir(temp.path()).unwrap();
        std::fs::write(staging_dir.join(CERT_PEM_FILENAME), b"new-cert").unwrap();
        std::fs::write(staging_dir.join(KEY_PEM_FILENAME), b"new-key").unwrap();
        std::fs::write(
            staging_dir.join(SERVICE_PRINCIPAL_STATE_FILENAME),
            serde_json::to_vec(&ServicePrincipalStateFile {
                tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                login_endpoint: None,
                certificate_path: CERT_PEM_FILENAME.to_string(),
                private_key_path: KEY_PEM_FILENAME.to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            staging_dir.join(STORAGE_QUEUES_CONFIG_FILENAME),
            serde_json::to_vec(&StorageQueuesConfigFile {
                queue_endpoint: "https://example.queue.core.windows.net".to_string(),
                upstream_queue: "upstream".to_string(),
                downstream_queue: "downstream".to_string(),
            })
            .unwrap(),
        )
        .unwrap();

        std::fs::create_dir(temp.path().join(format!("{ACTIVE_STATE_FILENAME}.tmp"))).unwrap();

        let err = commit_staging(&staging_dir, temp.path()).unwrap_err();
        assert!(matches!(err, CompanionError::Io(_)));
        assert!(!staging_dir.exists());
        assert!(!temp.path().join(ACTIVE_STATE_FILENAME).exists());

        let state_generations = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let file_name = entry.file_name();
                let file_name = file_name.to_str()?;
                if file_name.starts_with(STATE_GENERATION_PREFIX) {
                    Some(file_name.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert!(
            state_generations.is_empty(),
            "unexpected generations: {state_generations:?}"
        );
    }

    #[tokio::test]
    async fn bootstrap_deployment_removes_container_after_start_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/images/create"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/containers/create"))
            .respond_with(
                ResponseTemplate::new(201)
                    .append_header("content-type", "application/json")
                    .set_body_string(r#"{"Id":"test-container","Warnings":null}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/containers/test-container/archive"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/containers/test-container/start"))
            .respond_with(ResponseTemplate::new(500).set_body_string("start failed"))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path_regex(r"^/containers/test-container$"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let docker = Docker::connect_with_http(&server.uri(), 120, API_DEFAULT_VERSION).unwrap();
        let args = super::BootstrapArgs {
            azure_location: "westus2".to_string(),
            azure_project_name: "sonde".to_string(),
            azure_subscription_id: None,
            bootstrap_image: Some("sonde-azure-bootstrap:test-override".to_string()),
            custom_domain_name: None,
            custom_domain_dns_resource_group: None,
            custom_domain_dns_zone_name: None,
        };

        let err = run_bootstrap_deployment_with_docker_and_image(
            &docker,
            "/tmp/unused-admin.sock",
            "dummy-cert-base64",
            &args,
            "sonde-azure-bootstrap:test-override",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CompanionError::Config(_)));

        let requests = server.received_requests().await.unwrap();
        let methods_and_paths = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.url.path()))
            .collect::<Vec<_>>();
        assert_eq!(
            methods_and_paths,
            vec![
                "POST /images/create".to_string(),
                "POST /containers/create".to_string(),
                "POST /containers/test-container/start".to_string(),
                "DELETE /containers/test-container".to_string(),
            ]
        );
        assert!(
            requests[0].url.query().is_some_and(
                |query| query.contains("fromImage=sonde-azure-bootstrap%3Atest-override")
            )
        );
        // No platform override — Docker pulls the native architecture.
        let create_body = String::from_utf8(requests[1].body.clone()).unwrap();
        assert!(create_body.contains("COMPANION_CERT_BASE64=dummy-cert-base64"));
    }

    #[test]
    fn runtime_ready_requires_namespace_and_queue_config() {
        temp_env::with_vars_unset(
            [
                "SONDE_AZURE_STORAGE_QUEUE_ENDPOINT",
                "SONDE_AZURE_STORAGE_UPSTREAM_QUEUE",
                "SONDE_AZURE_STORAGE_DOWNSTREAM_QUEUE",
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
                    queue_endpoint: "https://example.queue.core.windows.net".to_string(),
                    upstream_queue: "upstream".to_string(),
                    downstream_queue: "downstream".to_string(),
                }
            );
            assert_eq!(
                state,
                RuntimeCredentialState {
                    tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
                    client_id: "22222222-2222-2222-2222-222222222222".to_string(),
                    login_endpoint: DEFAULT_LOGIN_ENDPOINT.to_string(),
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
            login_endpoint: None,
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
    fn resolve_login_endpoint_defaults_when_absent() {
        let result = resolve_login_endpoint(None).unwrap();
        assert_eq!(result, DEFAULT_LOGIN_ENDPOINT);
    }

    #[test]
    fn resolve_login_endpoint_strips_trailing_slash() {
        let result =
            resolve_login_endpoint(Some("https://login.microsoftonline.com/".to_string())).unwrap();
        assert_eq!(result, "https://login.microsoftonline.com");
    }

    #[test]
    fn resolve_login_endpoint_preserves_no_trailing_slash() {
        let result =
            resolve_login_endpoint(Some("https://login.microsoftonline.com".to_string())).unwrap();
        assert_eq!(result, "https://login.microsoftonline.com");
    }

    #[test]
    fn resolve_login_endpoint_rejects_empty() {
        let err = resolve_login_endpoint(Some("".to_string())).unwrap_err();
        assert!(err
            .to_string()
            .contains("login_endpoint must not be empty when present"));
    }

    #[test]
    fn resolve_login_endpoint_rejects_whitespace_only() {
        let err = resolve_login_endpoint(Some("   ".to_string())).unwrap_err();
        assert!(err
            .to_string()
            .contains("login_endpoint must not be empty when present"));
    }

    #[test]
    fn resolve_login_endpoint_sovereign_cloud() {
        let result =
            resolve_login_endpoint(Some("https://login.microsoftonline.us/".to_string())).unwrap();
        assert_eq!(result, "https://login.microsoftonline.us");
    }

    #[test]
    fn resolve_login_endpoint_rejects_slash_only() {
        let err = resolve_login_endpoint(Some("/".to_string())).unwrap_err();
        assert!(err
            .to_string()
            .contains("login_endpoint must not be empty when present"));
    }

    #[test]
    fn runtime_ready_uses_default_login_endpoint_when_absent() {
        let temp = TempDir::new().unwrap();
        write_service_principal_state(&temp);
        with_runtime_env(|| {
            let state = load_runtime_credential_state(temp.path()).unwrap();
            assert_eq!(state.login_endpoint, DEFAULT_LOGIN_ENDPOINT);
        });
    }

    #[test]
    fn runtime_ready_rejects_empty_login_endpoint() {
        let temp = TempDir::new().unwrap();
        write_service_principal_state(&temp);
        let state_path = temp.path().join("service-principal.json");
        std::fs::write(
            &state_path,
            br#"{"tenant_id":"11111111-1111-1111-1111-111111111111","client_id":"22222222-2222-2222-2222-222222222222","login_endpoint":"","certificate_path":"client-cert.pem","private_key_path":"client-key.pem"}"#,
        )
        .unwrap();
        with_runtime_env(|| {
            let err = load_runtime_credential_state(temp.path()).unwrap_err();
            assert!(err
                .to_string()
                .contains("login_endpoint must not be empty when present"));
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

    #[test]
    fn parse_queue_message_xml_extracts_message() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<QueueMessagesList>
  <QueueMessage>
    <MessageId>msg-123</MessageId>
    <PopReceipt>pop-abc</PopReceipt>
    <MessageText>aGVsbG8=</MessageText>
  </QueueMessage>
</QueueMessagesList>"#;
        let msg = parse_queue_message_xml(xml).unwrap().unwrap();
        assert_eq!(msg.message_id, "msg-123");
        assert_eq!(msg.pop_receipt, "pop-abc");
        assert_eq!(msg.body, b"hello");
    }

    #[test]
    fn parse_queue_message_xml_returns_none_for_empty_list() {
        let xml = r#"<?xml version="1.0"?><QueueMessagesList></QueueMessagesList>"#;
        assert!(parse_queue_message_xml(xml).unwrap().is_none());
    }

    #[test]
    fn parse_queue_message_xml_rejects_invalid_base64() {
        let xml = r#"<QueueMessagesList><QueueMessage>
<MessageId>id</MessageId><PopReceipt>pr</PopReceipt>
<MessageText>!!!not-base64!!!</MessageText>
</QueueMessage></QueueMessagesList>"#;
        let err = parse_queue_message_xml(xml).unwrap_err();
        assert!(err.to_string().contains("base64"));
    }

    #[test]
    fn extract_xml_element_returns_none_for_missing_tag() {
        assert!(extract_xml_element("<Root></Root>", "Missing").is_none());
    }

    #[test]
    fn urlencoding_encode_encodes_special_chars() {
        assert_eq!(urlencoding_encode("a b+c"), "a%20b%2Bc");
        assert_eq!(urlencoding_encode("simple"), "simple");
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
            bootstrap_image: None,
            custom_domain_name: None,
            custom_domain_dns_resource_group: None,
            custom_domain_dns_zone_name: None,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn bridge_runtime_returns_ok_on_shutdown_signal() {
        let reader_state = Arc::new(ReaderState::new());
        let write_started = Arc::new(Notify::new());
        let inflight_set = Arc::new(Notify::new());
        let abandons = Arc::new(AtomicUsize::new(0));
        let stream = ShutdownTestStream {
            reader_state,
            write_started,
        };
        let consumer = ShutdownAwareConsumer {
            payload: Some(vec![1u8, 2, 3]),
            inflight: false,
            inflight_set: Arc::clone(&inflight_set),
            abandons: Arc::clone(&abandons),
        };
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let bridge_task = tokio::spawn(async move {
            bridge_runtime_with_shutdown(stream, FakePublisher::default(), consumer, async move {
                let _ = shutdown_rx.await;
            })
            .await
        });

        inflight_set.notified().await;
        shutdown_tx.send(()).unwrap();

        bridge_task.await.unwrap().unwrap();
        assert_eq!(abandons.load(Ordering::SeqCst), 1);
    }

    #[cfg(windows)]
    #[test]
    fn service_launch_args_use_canonical_global_option_order() {
        use std::ffi::OsString;

        let args = build_service_launch_args(&Cli {
            admin_socket: r"\\.\pipe\custom-admin".to_string(),
            connector_socket: r"\\.\pipe\custom-connector".to_string(),
            state_dir: PathBuf::from(r"C:\ProgramData\sonde-azure-companion"),
            command: Some(Command::Install),
        });

        assert_eq!(
            args,
            vec![
                OsString::from("--admin-socket"),
                OsString::from(r"\\.\pipe\custom-admin"),
                OsString::from("--connector-socket"),
                OsString::from(r"\\.\pipe\custom-connector"),
                OsString::from("--state-dir"),
                OsString::from(r"C:\ProgramData\sonde-azure-companion"),
                OsString::from("service"),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn default_state_dir_uses_programdata_environment() {
        temp_env::with_var("PROGRAMDATA", Some(r"D:\RelocatedProgramData"), || {
            assert_eq!(
                default_state_dir(),
                PathBuf::from(r"D:\RelocatedProgramData\sonde-azure-companion")
            );
        });
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
            login_endpoint: None,
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
            queue_endpoint: "https://example.queue.core.windows.net".to_string(),
            upstream_queue: "upstream".to_string(),
            downstream_queue: "downstream".to_string(),
        };
        let runtime_state = RuntimeCredentialState {
            tenant_id: "11111111-1111-1111-1111-111111111111".to_string(),
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            login_endpoint: DEFAULT_LOGIN_ENDPOINT.to_string(),
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
