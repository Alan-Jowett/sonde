// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::error::Error;

use clap::{Parser, Subcommand};
use tonic::transport::{Channel, Endpoint, Uri};

use sonde_gateway::companion::pb::gateway_companion_client::GatewayCompanionClient;
use sonde_gateway::companion::pb::{
    CompanionListNodesRequest, CompanionShowModemDisplayMessageRequest,
};

#[cfg(unix)]
const DEFAULT_COMPANION_SOCKET: &str = "/var/run/sonde/companion.sock";
#[cfg(windows)]
const DEFAULT_COMPANION_SOCKET: &str = r"\\.\pipe\sonde-companion";

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
    /// Ask the gateway companion API to render a transient modem display message.
    DisplayMessage {
        /// Between 1 and 4 text lines to render.
        lines: Vec<String>,
    },
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
    use std::time::Duration;
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(&cli.companion_socket).await?,
        Command::DisplayMessage { lines } => display_message(&cli.companion_socket, lines).await?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_display_lines;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
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
}
