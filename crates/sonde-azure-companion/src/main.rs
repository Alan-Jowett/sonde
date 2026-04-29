// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
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
use clap::{Args, Parser, Subcommand};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use oauth_device_flows::provider::GenericProviderConfig;
use oauth_device_flows::{DeviceFlow, DeviceFlowConfig, Provider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::Duration as TimeDuration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint, Uri};
use url::Url;

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
const DEFAULT_DOWNSTREAM_WAIT_SECS: u64 = 1;
const CONNECTOR_MAX_FRAME_LENGTH: usize =
    sonde_gateway::connector::DEFAULT_CONNECTOR_MAX_MESSAGE_SIZE;
const ACCESS_TOKEN_REFRESH_MARGIN_SECS: i64 = 300;
const CLIENT_ASSERTION_LIFETIME_SECS: i64 = 600;
const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

#[derive(Debug, Error)]
enum CompanionError {
    #[error("{0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    TonicTransport(#[from] tonic::transport::Error),
    #[error(transparent)]
    TonicStatus(#[from] tonic::Status),
    #[error(transparent)]
    AzureCore(#[from] azure_core::Error),
    #[error(transparent)]
    OAuth(#[from] oauth_device_flows::DeviceFlowError),
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
    /// Perform Microsoft device auth and display the user code on the modem.
    BootstrapAuth(BootstrapAuthArgs),
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
struct BootstrapAuthArgs {
    /// Microsoft Entra application client ID used for device auth.
    #[arg(long, env = "SONDE_AZURE_DEVICE_CLIENT_ID")]
    device_client_id: String,

    /// Comma-delimited OAuth scopes to request during device auth.
    #[arg(long, env = "SONDE_AZURE_DEVICE_SCOPES", value_delimiter = ',', num_args = 1..)]
    device_scopes: Vec<String>,

    /// Poll interval in seconds for the device auth token endpoint.
    #[arg(
        long,
        env = "SONDE_AZURE_DEVICE_POLL_INTERVAL_SECS",
        default_value_t = 5
    )]
    poll_interval_secs: u64,

    /// Maximum number of token polling attempts before bootstrap fails.
    #[arg(long, env = "SONDE_AZURE_DEVICE_MAX_ATTEMPTS", default_value_t = 60)]
    max_attempts: u32,

    /// Optional override for the device authorization endpoint, primarily for tests.
    #[arg(long, env = "SONDE_AZURE_DEVICE_AUTH_URL")]
    device_auth_url: Option<String>,

    /// Optional override for the token endpoint, primarily for tests.
    #[arg(long, env = "SONDE_AZURE_DEVICE_TOKEN_URL")]
    device_token_url: Option<String>,
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

fn validate_device_scopes(scopes: &[String]) -> Result<(), CompanionError> {
    if scopes.is_empty() {
        Err(CompanionError::Config(
            "bootstrap-auth requires at least one device scope".to_string(),
        ))
    } else if scopes.iter().any(|scope| scope.trim().is_empty()) {
        Err(CompanionError::Config(
            "bootstrap-auth device scopes must not be empty".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn validate_device_client_id(client_id: &str) -> Result<(), CompanionError> {
    if client_id.trim().is_empty() {
        Err(CompanionError::Config(
            "bootstrap-auth requires a non-empty device client ID".to_string(),
        ))
    } else {
        Ok(())
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

fn load_runtime_config() -> Result<RuntimeConfig, CompanionError> {
    Ok(RuntimeConfig {
        namespace: require_non_empty(
            std::env::var("SONDE_AZURE_SERVICEBUS_NAMESPACE").unwrap_or_default(),
            "SONDE_AZURE_SERVICEBUS_NAMESPACE",
        )?,
        upstream_queue: require_non_empty(
            std::env::var("SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE").unwrap_or_default(),
            "SONDE_AZURE_SERVICEBUS_UPSTREAM_QUEUE",
        )?,
        downstream_queue: require_non_empty(
            std::env::var("SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE").unwrap_or_default(),
            "SONDE_AZURE_SERVICEBUS_DOWNSTREAM_QUEUE",
        )?,
    })
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
    let state: ServicePrincipalStateFile = serde_json::from_slice(&std::fs::read(&state_path)?)?;
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
    let runtime_config = load_runtime_config()?;
    let runtime_state = load_runtime_credential_state(state_dir)?;
    let _ = load_certificate_thumbprint(&runtime_state.certificate_path)?;
    let _ = load_signing_key(&runtime_state.private_key_path)?;
    Ok((runtime_config, runtime_state))
}

fn bootstrap_provider_and_config(
    args: &BootstrapAuthArgs,
) -> Result<(Provider, DeviceFlowConfig), CompanionError> {
    validate_device_client_id(&args.device_client_id)?;
    validate_device_scopes(&args.device_scopes)?;

    let device_client_id = args.device_client_id.trim().to_string();
    let device_scopes: Vec<String> = args
        .device_scopes
        .iter()
        .map(|scope| scope.trim().to_string())
        .collect();

    let config = DeviceFlowConfig::new()
        .client_id(device_client_id)
        .scopes(device_scopes.clone())
        .poll_interval(Duration::from_secs(args.poll_interval_secs))
        .max_attempts(args.max_attempts);

    match (&args.device_auth_url, &args.device_token_url) {
        (Some(device_auth_url), Some(device_token_url)) => {
            let provider = GenericProviderConfig::new(
                Url::parse(device_auth_url)?,
                Url::parse(device_token_url)?,
                "Microsoft test override".to_string(),
            )
            .with_default_scopes(device_scopes);
            Ok((Provider::Generic, config.generic_provider(provider)))
        }
        (None, None) => Ok((Provider::Microsoft, config)),
        _ => Err(CompanionError::Config(
            "device auth and token endpoint overrides must be provided together".to_string(),
        )),
    }
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

fn load_signing_key(private_key_path: &Path) -> Result<(Algorithm, EncodingKey), CompanionError> {
    let private_key_pem = std::fs::read(private_key_path)?;

    if let Ok(key) = EncodingKey::from_rsa_pem(&private_key_pem) {
        return Ok((Algorithm::RS256, key));
    }
    if let Ok(key) = EncodingKey::from_ec_pem(&private_key_pem) {
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

fn build_service_bus_credential(
    runtime_state: &RuntimeCredentialState,
) -> Result<Arc<dyn TokenCredential>, CompanionError> {
    let certificate_thumbprint = load_certificate_thumbprint(&runtime_state.certificate_path)?;
    let (signing_algorithm, signing_key) = load_signing_key(&runtime_state.private_key_path)?;
    let token_endpoint = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        runtime_state.tenant_id
    );
    let http_client = reqwest::Client::builder().build()?;
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
            .map_err(|err| azure_core::Error::new(ErrorKind::Credential, err))?
            .error_for_status()
            .map_err(|err| azure_core::Error::new(ErrorKind::Credential, err))?;
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
        let message = self
            .receiver
            .receive_message_with_max_wait_time(Some(Duration::from_secs(
                DEFAULT_DOWNSTREAM_WAIT_SECS,
            )))
            .await?;
        if let Some(message) = message {
            let payload = message
                .body()
                .map_err(|err| {
                    CompanionError::Config(format!(
                        "downstream Service Bus message body was not raw binary data: {err}"
                    ))
                })?
                .to_vec();
            self.inflight = Some(message);
            Ok(Some(payload))
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
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
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

    consumer.complete().await?;
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
                result?;
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

async fn display_message(admin_socket: &str, lines: Vec<String>) -> Result<(), CompanionError> {
    validate_display_lines(&lines)?;
    let mut client = connect_admin(admin_socket).await?;
    client
        .show_modem_display_message(ShowModemDisplayMessageRequest { lines })
        .await?;
    Ok(())
}

async fn bootstrap_auth(
    admin_socket: &str,
    state_dir: &Path,
    args: BootstrapAuthArgs,
) -> Result<(), CompanionError> {
    std::fs::create_dir_all(state_dir)?;

    let (provider, config) = bootstrap_provider_and_config(&args)?;
    let mut flow = DeviceFlow::new(provider, config)?;
    let auth = flow.initialize().await?;

    eprintln!(
        "Azure device auth required. Open {} and enter code {}",
        auth.verification_uri(),
        auth.user_code()
    );
    if let Some(verification_uri_complete) = auth.verification_uri_complete() {
        eprintln!("Complete verification URL: {verification_uri_complete}");
    }

    display_message(
        admin_socket,
        vec!["Azure login".to_string(), auth.user_code().to_string()],
    )
    .await?;

    let token = flow.poll_for_token().await?;
    if let Some(expires_in) = token.expires_in {
        eprintln!(
            "Azure device auth succeeded; received temporary {} token valid for {} seconds",
            token.token_type, expires_in
        );
    } else {
        eprintln!(
            "Azure device auth succeeded; received temporary {} token",
            token.token_type
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), CompanionError> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(&cli.connector_socket, &cli.state_dir).await?,
        Command::BootstrapAuth(args) => {
            bootstrap_auth(&cli.admin_socket, &cli.state_dir, args).await?
        }
        Command::DisplayMessage { lines } => display_message(&cli.admin_socket, lines).await?,
        Command::CheckRuntimeReady => {
            check_runtime_ready(&cli.state_dir)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap_provider_and_config, check_runtime_ready, load_runtime_config,
        pump_downstream_once, pump_upstream_once, read_framed, resolve_state_relative_path,
        validate_device_client_id, validate_device_scopes, validate_display_lines, write_framed,
        BootstrapAuthArgs, ClientAssertionCredential, CompanionError, DownstreamConsumer,
        RuntimeConfig, RuntimeCredentialState, ServicePrincipalStateFile, UpstreamPublisher,
        CONNECTOR_MAX_FRAME_LENGTH,
    };
    use azure_core::credentials::TokenCredential;
    use jsonwebtoken::{Algorithm, EncodingKey};
    use oauth_device_flows::Provider;
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

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn bootstrap_args() -> BootstrapAuthArgs {
        BootstrapAuthArgs {
            device_client_id: "client-id".to_string(),
            device_scopes: vec!["scope-a".to_string()],
            poll_interval_secs: 5,
            max_attempts: 60,
            device_auth_url: None,
            device_token_url: None,
        }
    }

    fn write_service_principal_state(temp: &TempDir) -> PathBuf {
        let cert_path = temp.path().join("client-cert.pem");
        let key_path = temp.path().join("client-key.pem");
        std::fs::write(
            &cert_path,
            concat!(
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
            ),
        )
        .unwrap();
        std::fs::write(
            &key_path,
            concat!(
                "-----BEGIN PRIVATE KEY-----\n",
                "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg2X8i4lE4hM2t0b5Y\n",
                "fI7xW0ZzM3ZrY4L3s67qG8R0uYWhRANCAAStNVLyCQaqRPW+p7wtNOVgX5c18vv6\n",
                "4n71/Bsfc/1KImuM3gnXDOo/xw/qU/TC0P67YThvhbRBb4TdqNB7ytb7\n",
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
    }

    impl FakeConsumer {
        fn new(payloads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                queued: payloads.into_iter().collect(),
                inflight: None,
                completes: 0,
                abandons: 0,
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
    fn bootstrap_auth_requires_at_least_one_scope() {
        assert!(validate_device_scopes(&[]).is_err());
        assert!(validate_device_scopes(&["scope".to_string()]).is_ok());
    }

    #[test]
    fn bootstrap_auth_rejects_blank_scope_entries() {
        assert!(validate_device_scopes(&[" ".to_string()]).is_err());
        assert!(validate_device_scopes(&["scope".to_string(), "".to_string()]).is_err());
    }

    #[test]
    fn bootstrap_auth_requires_non_empty_client_id() {
        assert!(validate_device_client_id("").is_err());
        assert!(validate_device_client_id("   ").is_err());
        assert!(validate_device_client_id("client-id").is_ok());
    }

    #[test]
    fn bootstrap_auth_uses_microsoft_provider_by_default() {
        let (provider, _config) = bootstrap_provider_and_config(&bootstrap_args()).unwrap();
        assert_eq!(provider, Provider::Microsoft);
    }

    #[test]
    fn bootstrap_auth_accepts_explicit_endpoint_overrides() {
        let mut args = bootstrap_args();
        args.device_auth_url = Some("http://127.0.0.1/device".to_string());
        args.device_token_url = Some("http://127.0.0.1/token".to_string());

        let (provider, _config) = bootstrap_provider_and_config(&args).unwrap();
        assert_eq!(provider, Provider::Generic);
    }

    #[test]
    fn bootstrap_auth_rejects_partial_endpoint_override() {
        let mut args = bootstrap_args();
        args.device_auth_url = Some("http://127.0.0.1/device".to_string());

        assert!(bootstrap_provider_and_config(&args).is_err());
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
                let err = load_runtime_config().unwrap_err();
                assert!(matches!(err, CompanionError::Config(_)));
            },
        );
    }

    #[test]
    fn runtime_ready_uses_service_principal_state_file() {
        let temp = TempDir::new().unwrap();
        write_service_principal_state(&temp);
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
            || {
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
                        certificate_path: temp
                            .path()
                            .join("client-cert.pem")
                            .canonicalize()
                            .unwrap(),
                        private_key_path: temp
                            .path()
                            .join("client-key.pem")
                            .canonicalize()
                            .unwrap(),
                    }
                );
            },
        );
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
            || {
                let err = check_runtime_ready(temp.path()).unwrap_err();
                assert!(err
                    .to_string()
                    .contains("service principal certificate_path must be set and non-empty"));
            },
        );
    }

    #[test]
    fn runtime_ready_rejects_unparseable_pem_material() {
        let temp = TempDir::new().unwrap();
        write_invalid_service_principal_state(&temp);
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
            || {
                assert!(check_runtime_ready(temp.path()).is_err());
            },
        );
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
