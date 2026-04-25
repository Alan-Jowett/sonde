// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use oauth_device_flows::provider::GenericProviderConfig;
use oauth_device_flows::{DeviceFlow, DeviceFlowConfig, Provider};
use tonic::transport::{Channel, Endpoint, Uri};
use url::Url;

use sonde_gateway::companion::pb::gateway_companion_client::GatewayCompanionClient;
use sonde_gateway::companion::pb::{
    CompanionListNodesRequest, CompanionShowModemDisplayMessageRequest,
};

#[cfg(unix)]
const DEFAULT_COMPANION_SOCKET: &str = "/var/run/sonde/companion.sock";
#[cfg(windows)]
const DEFAULT_COMPANION_SOCKET: &str = r"\\.\pipe\sonde-companion";

#[cfg(unix)]
const DEFAULT_STATE_DIR: &str = "/var/lib/sonde-azure-companion";
#[cfg(windows)]
const DEFAULT_STATE_DIR: &str = r"C:\ProgramData\sonde-azure-companion";

#[derive(Debug, Parser)]
#[command(name = "sonde-azure-companion")]
struct Cli {
    /// Gateway companion socket path (UDS on Unix, named pipe on Windows).
    #[arg(long, env = "SONDE_GATEWAY_COMPANION_SOCKET", default_value = DEFAULT_COMPANION_SOCKET)]
    companion_socket: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the long-running Azure companion process.
    Run,
    /// Perform Microsoft device auth, display the user code, and discard the token on success.
    BootstrapAuth(BootstrapAuthArgs),
    /// Ask the gateway companion API to render a transient modem display message.
    DisplayMessage {
        /// Between 1 and 4 text lines to render.
        lines: Vec<String>,
    },
}

#[derive(Debug, Args)]
struct BootstrapAuthArgs {
    /// Mounted state directory reserved for later provisioning output.
    #[arg(long, env = "SONDE_AZURE_COMPANION_STATE_DIR", default_value = DEFAULT_STATE_DIR)]
    state_dir: PathBuf,

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

#[cfg(unix)]
async fn connect_companion(
    socket_path: &str,
) -> Result<GatewayCompanionClient<Channel>, Box<dyn Error>> {
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
    Ok(GatewayCompanionClient::new(channel))
}

#[cfg(windows)]
async fn connect_companion(
    pipe_name: &str,
) -> Result<GatewayCompanionClient<Channel>, Box<dyn Error>> {
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
                        Err(e) if e.raw_os_error() == Some(231) => {}
                        Err(e) => return Err(e),
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
    Ok(GatewayCompanionClient::new(channel))
}

#[cfg(not(any(unix, windows)))]
compile_error!(
    "sonde-azure-companion requires Unix (UDS) or Windows (named pipes) — this platform is not supported"
);

fn validate_display_lines(lines: &[String]) -> Result<(), String> {
    if (1..=4).contains(&lines.len()) {
        Ok(())
    } else {
        Err("display-message requires between 1 and 4 lines".to_string())
    }
}

fn validate_device_scopes(scopes: &[String]) -> Result<(), String> {
    if scopes.is_empty() {
        Err("bootstrap-auth requires at least one device scope".to_string())
    } else {
        Ok(())
    }
}

fn bootstrap_provider_and_config(
    args: &BootstrapAuthArgs,
) -> Result<(Provider, DeviceFlowConfig), Box<dyn Error>> {
    validate_device_scopes(&args.device_scopes)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;

    let config = DeviceFlowConfig::new()
        .client_id(args.device_client_id.clone())
        .scopes(args.device_scopes.clone())
        .poll_interval(Duration::from_secs(args.poll_interval_secs))
        .max_attempts(args.max_attempts);

    match (&args.device_auth_url, &args.device_token_url) {
        (Some(device_auth_url), Some(device_token_url)) => {
            let provider = GenericProviderConfig::new(
                Url::parse(device_auth_url)?,
                Url::parse(device_token_url)?,
                "Microsoft test override".to_string(),
            )
            .with_default_scopes(args.device_scopes.clone());
            Ok((Provider::Generic, config.generic_provider(provider)))
        }
        (None, None) => Ok((Provider::Microsoft, config)),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "device auth and token endpoint overrides must be provided together",
        )
        .into()),
    }
}

async fn run(socket_path: &str) -> Result<(), Box<dyn Error>> {
    let mut client = connect_companion(socket_path).await?;
    let response = client
        .list_nodes(CompanionListNodesRequest {})
        .await?
        .into_inner();
    eprintln!(
        "connected to gateway companion API at {socket_path}; {} nodes known",
        response.nodes.len()
    );
    std::future::pending::<()>().await;
    #[allow(unreachable_code)]
    Ok(())
}

async fn display_message(socket_path: &str, lines: Vec<String>) -> Result<(), Box<dyn Error>> {
    validate_display_lines(&lines)
        .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
    let mut client = connect_companion(socket_path).await?;
    client
        .show_modem_display_message(CompanionShowModemDisplayMessageRequest { lines })
        .await?;
    Ok(())
}

async fn bootstrap_auth(socket_path: &str, args: BootstrapAuthArgs) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(&args.state_dir)?;

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
        socket_path,
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
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(&cli.companion_socket).await?,
        Command::BootstrapAuth(args) => bootstrap_auth(&cli.companion_socket, args).await?,
        Command::DisplayMessage { lines } => display_message(&cli.companion_socket, lines).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap_provider_and_config, validate_device_scopes, validate_display_lines,
        BootstrapAuthArgs,
    };
    use oauth_device_flows::Provider;
    use std::path::PathBuf;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    fn bootstrap_args() -> BootstrapAuthArgs {
        BootstrapAuthArgs {
            state_dir: PathBuf::from("state"),
            device_client_id: "client-id".to_string(),
            device_scopes: vec!["scope-a".to_string()],
            poll_interval_secs: 5,
            max_attempts: 60,
            device_auth_url: None,
            device_token_url: None,
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
}
