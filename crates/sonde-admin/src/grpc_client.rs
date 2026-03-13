// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use tonic::transport::{Channel, Endpoint, Uri};

use crate::pb::gateway_admin_client::GatewayAdminClient;
use crate::pb::*;

/// Thin wrapper around the generated tonic `GatewayAdminClient`.
pub struct AdminClient {
    inner: GatewayAdminClient<Channel>,
}

impl AdminClient {
    /// Connect to the gateway admin API over a Unix domain socket.
    #[cfg(unix)]
    pub async fn connect(socket_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        use hyper_util::rt::TokioIo;

        let socket_path = socket_path.to_owned();
        // URI is ignored for UDS but tonic requires a valid one.
        let channel = Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(tower::service_fn(move |_: Uri| {
                let path = socket_path.clone();
                async move {
                    let stream = tokio::net::UnixStream::connect(path).await?;
                    Ok::<_, std::io::Error>(TokioIo::new(stream))
                }
            }))
            .await?;
        Ok(Self {
            inner: GatewayAdminClient::new(channel),
        })
    }

    /// Connect to the gateway admin API over a Windows named pipe.
    #[cfg(windows)]
    pub async fn connect(pipe_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        use hyper_util::rt::TokioIo;
        use std::time::Duration;
        use tokio::net::windows::named_pipe::ClientOptions;

        let pipe_name = pipe_name.to_owned();
        let channel = Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(tower::service_fn(move |_: Uri| {
                let name = pipe_name.clone();
                async move {
                    // Retry if the pipe is busy (another client is connecting).
                    // Give up after 5 seconds.
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    let client = loop {
                        match ClientOptions::new().open(&name) {
                            Ok(client) => break client,
                            Err(e) if e.raw_os_error() == Some(231) => {} // ERROR_PIPE_BUSY
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
        Ok(Self {
            inner: GatewayAdminClient::new(channel),
        })
    }

    #[cfg(not(any(unix, windows)))]
    compile_error!(
        "sonde-admin requires Unix (UDS) or Windows (named pipes) — this platform is not supported"
    );

    // -- Node management --

    pub async fn list_nodes(&mut self) -> Result<Vec<NodeInfo>, tonic::Status> {
        let resp = self.inner.list_nodes(Empty {}).await?;
        Ok(resp.into_inner().nodes)
    }

    pub async fn get_node(&mut self, node_id: &str) -> Result<NodeInfo, tonic::Status> {
        let resp = self
            .inner
            .get_node(GetNodeRequest {
                node_id: node_id.to_string(),
            })
            .await?;
        Ok(resp.into_inner())
    }

    pub async fn register_node(
        &mut self,
        node_id: &str,
        key_hint: u32,
        psk: Vec<u8>,
    ) -> Result<String, tonic::Status> {
        let resp = self
            .inner
            .register_node(RegisterNodeRequest {
                node_id: node_id.to_string(),
                key_hint,
                psk,
            })
            .await?;
        Ok(resp.into_inner().node_id)
    }

    pub async fn remove_node(&mut self, node_id: &str) -> Result<(), tonic::Status> {
        self.inner
            .remove_node(RemoveNodeRequest {
                node_id: node_id.to_string(),
            })
            .await?;
        Ok(())
    }

    // -- Program management --

    pub async fn ingest_program(
        &mut self,
        image_data: Vec<u8>,
        profile: i32,
    ) -> Result<(Vec<u8>, u32), tonic::Status> {
        let resp = self
            .inner
            .ingest_program(IngestProgramRequest {
                image_data,
                verification_profile: profile,
            })
            .await?;
        let inner = resp.into_inner();
        Ok((inner.program_hash, inner.program_size))
    }

    pub async fn list_programs(&mut self) -> Result<Vec<ProgramInfo>, tonic::Status> {
        let resp = self.inner.list_programs(Empty {}).await?;
        Ok(resp.into_inner().programs)
    }

    pub async fn assign_program(
        &mut self,
        node_id: &str,
        program_hash: Vec<u8>,
    ) -> Result<(), tonic::Status> {
        self.inner
            .assign_program(AssignProgramRequest {
                node_id: node_id.to_string(),
                program_hash,
            })
            .await?;
        Ok(())
    }

    pub async fn remove_program(&mut self, program_hash: Vec<u8>) -> Result<(), tonic::Status> {
        self.inner
            .remove_program(RemoveProgramRequest { program_hash })
            .await?;
        Ok(())
    }

    // -- Command queueing --

    pub async fn set_schedule(
        &mut self,
        node_id: &str,
        interval_s: u32,
    ) -> Result<(), tonic::Status> {
        self.inner
            .set_schedule(SetScheduleRequest {
                node_id: node_id.to_string(),
                interval_s,
            })
            .await?;
        Ok(())
    }

    pub async fn queue_reboot(&mut self, node_id: &str) -> Result<(), tonic::Status> {
        self.inner
            .queue_reboot(QueueRebootRequest {
                node_id: node_id.to_string(),
            })
            .await?;
        Ok(())
    }

    pub async fn queue_ephemeral(
        &mut self,
        node_id: &str,
        program_hash: Vec<u8>,
    ) -> Result<(), tonic::Status> {
        self.inner
            .queue_ephemeral(QueueEphemeralRequest {
                node_id: node_id.to_string(),
                program_hash,
            })
            .await?;
        Ok(())
    }

    // -- Status --

    pub async fn get_node_status(&mut self, node_id: &str) -> Result<NodeStatus, tonic::Status> {
        let resp = self
            .inner
            .get_node_status(GetNodeStatusRequest {
                node_id: node_id.to_string(),
            })
            .await?;
        Ok(resp.into_inner())
    }

    // -- State export/import --

    pub async fn export_state(&mut self) -> Result<Vec<u8>, tonic::Status> {
        let resp = self.inner.export_state(Empty {}).await?;
        Ok(resp.into_inner().data)
    }

    pub async fn import_state(&mut self, data: Vec<u8>) -> Result<(), tonic::Status> {
        self.inner.import_state(ImportStateRequest { data }).await?;
        Ok(())
    }

    // -- Modem management --

    pub async fn get_modem_status(&mut self) -> Result<ModemStatus, tonic::Status> {
        let resp = self.inner.get_modem_status(Empty {}).await?;
        Ok(resp.into_inner())
    }

    pub async fn set_modem_channel(&mut self, channel: u32) -> Result<(), tonic::Status> {
        self.inner
            .set_modem_channel(SetModemChannelRequest { channel })
            .await?;
        Ok(())
    }

    pub async fn scan_modem_channels(&mut self) -> Result<Vec<ChannelSurveyEntry>, tonic::Status> {
        let resp = self.inner.scan_modem_channels(Empty {}).await?;
        Ok(resp.into_inner().entries)
    }
}
