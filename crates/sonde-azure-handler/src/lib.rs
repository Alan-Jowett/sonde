// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use azservicebus::{
    ServiceBusClient, ServiceBusClientOptions, ServiceBusMessage, ServiceBusSender,
    ServiceBusSenderOptions,
};
use azure_core::credentials::TokenCredential;
use azure_core_legacy::auth::TokenCredential as LegacyTokenCredential;
use azure_core_legacy::error::ErrorKind as LegacyAzureErrorKind;
use azure_core_legacy::StatusCode as LegacyStatusCode;
use azure_data_tables::prelude::{IfMatchCondition, TableServiceClient};
use azure_identity::ManagedIdentityCredential;
use azure_identity_legacy::{AppServiceManagedIdentityCredential, TokenCredentialOptions};
use base64::Engine as _;
use ciborium::Value;
use serde::{Deserialize, Serialize};
use sonde_gateway::connector::{MSG_TYPE_ACTUAL_STATE, MSG_TYPE_APP_DATA, MSG_TYPE_DESIRED_STATE};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Store(String),
    #[error("{0}")]
    Publish(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    AzureServiceBus(#[from] azure_core::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub service_bus_namespace: String,
    pub upstream_queue: String,
    pub downstream_queue: String,
    pub storage_account: String,
    pub node_state_table: String,
    pub program_route_table: String,
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, HandlerError> {
        let service_bus_namespace = required_env("SONDE_AZURE_HANDLER_SERVICE_BUS_NAMESPACE")
            .or_else(|_| required_env("AzureWebJobsServiceBus__fullyQualifiedNamespace"))?;
        Ok(Self {
            service_bus_namespace,
            upstream_queue: required_env("SONDE_AZURE_HANDLER_UPSTREAM_QUEUE")?,
            downstream_queue: required_env("SONDE_AZURE_HANDLER_DOWNSTREAM_QUEUE")?,
            storage_account: required_env("SONDE_AZURE_HANDLER_STORAGE_ACCOUNT")?,
            node_state_table: required_env("SONDE_AZURE_HANDLER_NODE_STATE_TABLE")?,
            program_route_table: required_env("SONDE_AZURE_HANDLER_PROGRAM_ROUTE_TABLE")?,
        })
    }
}

fn required_env(name: &str) -> Result<String, HandlerError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HandlerError::Config(format!("environment variable `{name}` must be set")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStateRow {
    pub node_id: String,
    pub desired_assigned_program_hash: Option<Vec<u8>>,
    pub desired_schedule_interval_s: Option<u32>,
    pub observed_current_program_hash: Option<Vec<u8>>,
    pub observed_assigned_program_hash: Option<Vec<u8>>,
    pub observed_schedule_interval_s: Option<u32>,
    pub battery_mv: Option<u32>,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub last_checkin_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramRouteRow {
    pub program_hash: Vec<u8>,
    pub handler_queue: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedStateUpdate {
    pub node_id: String,
    pub current_program_hash: Option<Vec<u8>>,
    pub assigned_program_hash: Option<Vec<u8>>,
    pub schedule_interval_s: Option<u32>,
    pub battery_mv: Option<u32>,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedStateWriteResult {
    Applied,
    IgnoredStale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualStateMessage {
    pub entity_kind: String,
    pub entity_id: String,
    pub current_program_hash: Option<Vec<u8>>,
    pub assigned_program_hash: Option<Vec<u8>>,
    pub battery_mv: Option<u32>,
    pub firmware_abi_version: Option<u32>,
    pub firmware_version: Option<String>,
    pub timestamp_ms: u64,
    pub schedule_interval_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDataMessage {
    pub node_id: String,
    pub program_hash: Vec<u8>,
    pub payload: Vec<u8>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorMessage {
    ActualState(ActualStateMessage),
    AppData(AppDataMessage),
    Unsupported(u64),
}

#[async_trait]
pub trait HandlerStore: Send + Sync {
    async fn load_node_state(&self, node_id: &str) -> Result<Option<NodeStateRow>, HandlerError>;
    async fn save_node_state(&self, row: &NodeStateRow) -> Result<(), HandlerError>;
    async fn create_node_state_if_absent(&self, row: &NodeStateRow) -> Result<bool, HandlerError>;
    async fn update_observed_state(
        &self,
        update: &ObservedStateUpdate,
    ) -> Result<ObservedStateWriteResult, HandlerError>;
    async fn load_program_route(
        &self,
        program_hash: &[u8],
    ) -> Result<Option<ProgramRouteRow>, HandlerError>;
}

#[async_trait]
pub trait QueuePublisher: Send + Sync {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError>;
}

pub struct AzureHandler<S, P> {
    store: Arc<S>,
    publisher: Arc<P>,
    downstream_queue: String,
}

impl<S, P> AzureHandler<S, P>
where
    S: HandlerStore,
    P: QueuePublisher,
{
    pub fn new(store: Arc<S>, publisher: Arc<P>, downstream_queue: impl Into<String>) -> Self {
        Self {
            store,
            publisher,
            downstream_queue: downstream_queue.into(),
        }
    }

    pub async fn handle_payload(&self, payload: &[u8]) -> Result<(), HandlerError> {
        match decode_connector_message(payload)? {
            ConnectorMessage::ActualState(actual_state) => {
                if actual_state.entity_kind == "node" {
                    self.handle_actual_state(actual_state).await?;
                } else {
                    warn!(
                        entity_kind = %actual_state.entity_kind,
                        entity_id = %actual_state.entity_id,
                        "ignoring non-node ACTUAL_STATE message"
                    );
                }
                Ok(())
            }
            ConnectorMessage::AppData(app_data) => self.handle_app_data(payload, app_data).await,
            ConnectorMessage::Unsupported(msg_type) => {
                warn!(msg_type, "ignoring unsupported connector message");
                Ok(())
            }
        }
    }

    async fn handle_actual_state(
        &self,
        actual_state: ActualStateMessage,
    ) -> Result<(), HandlerError> {
        if actual_state.entity_id.is_empty() {
            return Err(HandlerError::Decode(
                "node-scoped ACTUAL_STATE requires a non-empty entity_id".to_string(),
            ));
        }

        let node_id = actual_state.entity_id.clone();
        let Some(row) = self.store.load_node_state(&node_id).await? else {
            let seeded = NodeStateRow {
                node_id: node_id.clone(),
                desired_assigned_program_hash: actual_state
                    .current_program_hash
                    .clone()
                    .or_else(|| actual_state.assigned_program_hash.clone()),
                desired_schedule_interval_s: actual_state.schedule_interval_s,
                observed_current_program_hash: actual_state.current_program_hash.clone(),
                observed_assigned_program_hash: actual_state.assigned_program_hash.clone(),
                observed_schedule_interval_s: actual_state.schedule_interval_s,
                battery_mv: actual_state.battery_mv,
                firmware_abi_version: actual_state.firmware_abi_version,
                firmware_version: actual_state.firmware_version.clone(),
                last_checkin_ms: actual_state.timestamp_ms,
            };
            if self.store.create_node_state_if_absent(&seeded).await? {
                return Ok(());
            }
            let Some(row) = self.store.load_node_state(&node_id).await? else {
                return Err(HandlerError::Store(format!(
                    "node state row `{node_id}` disappeared during create race"
                )));
            };
            return self
                .reconcile_existing_actual_state(row, actual_state)
                .await;
        };

        self.reconcile_existing_actual_state(row, actual_state)
            .await
    }

    async fn reconcile_existing_actual_state(
        &self,
        row: NodeStateRow,
        actual_state: ActualStateMessage,
    ) -> Result<(), HandlerError> {
        if actual_state.timestamp_ms < row.last_checkin_ms {
            return Ok(());
        }

        let update = ObservedStateUpdate {
            node_id: row.node_id.clone(),
            current_program_hash: actual_state.current_program_hash,
            assigned_program_hash: actual_state.assigned_program_hash,
            schedule_interval_s: actual_state.schedule_interval_s,
            battery_mv: actual_state.battery_mv,
            firmware_abi_version: actual_state.firmware_abi_version,
            firmware_version: actual_state.firmware_version,
            timestamp_ms: actual_state.timestamp_ms,
        };
        let result = self.store.update_observed_state(&update).await?;
        if result == ObservedStateWriteResult::IgnoredStale {
            return Ok(());
        }
        let row = self
            .store
            .load_node_state(&update.node_id)
            .await?
            .ok_or_else(|| {
                HandlerError::Store(format!(
                    "node state row `{}` disappeared after observed-state update",
                    update.node_id
                ))
            })?;

        let program_diverged =
            row.desired_assigned_program_hash != row.observed_current_program_hash;
        let schedule_diverged = row.desired_schedule_interval_s != row.observed_schedule_interval_s;
        if !program_diverged && !schedule_diverged {
            return Ok(());
        }

        let desired = encode_desired_state(&row)?;
        self.publisher
            .publish(&self.downstream_queue, desired)
            .await
    }

    async fn handle_app_data(
        &self,
        raw_payload: &[u8],
        app_data: AppDataMessage,
    ) -> Result<(), HandlerError> {
        let route = self
            .store
            .load_program_route(&app_data.program_hash)
            .await?
            .ok_or_else(|| {
                HandlerError::Publish(format!(
                    "no ProgramRoute row exists for program hash `{}`",
                    hex::encode(&app_data.program_hash)
                ))
            })?;
        self.publisher
            .publish(&route.handler_queue, raw_payload.to_vec())
            .await
    }
}

pub struct AzureTablesStore {
    node_state_table: azure_data_tables::clients::TableClient,
    program_route_table: azure_data_tables::clients::TableClient,
}

impl AzureTablesStore {
    pub fn new(config: &RuntimeConfig) -> Result<Self, HandlerError> {
        let credential: Arc<dyn LegacyTokenCredential> = Arc::new(
            AppServiceManagedIdentityCredential::create(TokenCredentialOptions::default())
                .map_err(|e| {
                    HandlerError::Config(format!("create Azure Table credential failed: {e}"))
                })?,
        );
        let service = TableServiceClient::new(config.storage_account.clone(), credential);
        Ok(Self {
            node_state_table: service.table_client(config.node_state_table.clone()),
            program_route_table: service.table_client(config.program_route_table.clone()),
        })
    }
}

#[async_trait]
impl HandlerStore for AzureTablesStore {
    async fn load_node_state(&self, node_id: &str) -> Result<Option<NodeStateRow>, HandlerError> {
        let entity_client = self
            .node_state_table
            .partition_key_client("node")
            .entity_client(node_id.to_string());
        match entity_client.get::<NodeStateEntity>().await {
            Ok(response) => Ok(Some(NodeStateRow::try_from(response.entity)?)),
            Err(e)
                if matches!(
                    e.kind(),
                    LegacyAzureErrorKind::HttpResponse {
                        status: LegacyStatusCode::NotFound,
                        ..
                    }
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(HandlerError::Store(format!("query node state failed: {e}"))),
        }
    }

    async fn save_node_state(&self, row: &NodeStateRow) -> Result<(), HandlerError> {
        let entity: NodeStateEntity = row.clone().into();
        self.node_state_table
            .partition_key_client(entity.partition_key.clone())
            .entity_client(entity.row_key.clone())
            .insert_or_replace(&entity)
            .map_err(|e| HandlerError::Store(format!("prepare node state upsert failed: {e}")))?
            .await
            .map_err(|e| HandlerError::Store(format!("save node state failed: {e}")))?;
        Ok(())
    }

    async fn create_node_state_if_absent(&self, row: &NodeStateRow) -> Result<bool, HandlerError> {
        let entity: NodeStateEntity = row.clone().into();
        match self
            .node_state_table
            .insert::<_, NodeStateEntity>(&entity)
            .map_err(|e| HandlerError::Store(format!("prepare node state insert failed: {e}")))?
            .await
        {
            Ok(_) => Ok(true),
            Err(e)
                if matches!(
                    e.kind(),
                    LegacyAzureErrorKind::HttpResponse {
                        status: LegacyStatusCode::Conflict,
                        ..
                    }
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(HandlerError::Store(format!(
                "create node state failed: {e}"
            ))),
        }
    }

    async fn update_observed_state(
        &self,
        update: &ObservedStateUpdate,
    ) -> Result<ObservedStateWriteResult, HandlerError> {
        let entity_client = self
            .node_state_table
            .partition_key_client("node")
            .entity_client(update.node_id.clone());
        for _ in 0..4 {
            let response = entity_client.get::<NodeStateEntity>().await.map_err(|e| {
                HandlerError::Store(format!("load node state for merge failed: {e}"))
            })?;
            let mut entity = response.entity;
            if update.timestamp_ms < entity.last_checkin_ms {
                return Ok(ObservedStateWriteResult::IgnoredStale);
            }
            entity.observed_current_program_hash =
                encode_optional_hex(update.current_program_hash.clone());
            entity.observed_assigned_program_hash =
                encode_optional_hex(update.assigned_program_hash.clone());
            entity.observed_schedule_interval_s = update.schedule_interval_s;
            entity.battery_mv = update.battery_mv;
            entity.firmware_abi_version = update.firmware_abi_version;
            entity.firmware_version = update.firmware_version.clone();
            entity.last_checkin_ms = update.timestamp_ms;
            match entity_client
                .update(&entity, IfMatchCondition::from(response.etag))
                .map_err(|e| {
                    HandlerError::Store(format!("prepare observed-state update failed: {e}"))
                })?
                .await
            {
                Ok(_) => return Ok(ObservedStateWriteResult::Applied),
                Err(e)
                    if matches!(
                        e.kind(),
                        LegacyAzureErrorKind::HttpResponse {
                            status: LegacyStatusCode::PreconditionFailed,
                            ..
                        }
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    return Err(HandlerError::Store(format!(
                        "update observed state failed: {e}"
                    )));
                }
            }
        }
        Err(HandlerError::Store(format!(
            "update observed state for node `{}` failed after concurrent retries",
            update.node_id
        )))
    }

    async fn load_program_route(
        &self,
        program_hash: &[u8],
    ) -> Result<Option<ProgramRouteRow>, HandlerError> {
        let row_key = hex::encode(program_hash);
        let entity_client = self
            .program_route_table
            .partition_key_client("program")
            .entity_client(row_key);
        match entity_client.get::<ProgramRouteEntity>().await {
            Ok(response) => Ok(Some(ProgramRouteRow::try_from(response.entity)?)),
            Err(e)
                if matches!(
                    e.kind(),
                    LegacyAzureErrorKind::HttpResponse {
                        status: LegacyStatusCode::NotFound,
                        ..
                    }
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(HandlerError::Store(format!(
                "query program route failed: {e}"
            ))),
        }
    }
}

pub struct ServiceBusQueuePublisher {
    namespace: String,
    credential: Arc<dyn TokenCredential>,
    client: Mutex<Option<ServiceBusClient<azservicebus::core::BasicRetryPolicy>>>,
    senders: Mutex<HashMap<String, Arc<Mutex<ServiceBusSender>>>>,
}

impl ServiceBusQueuePublisher {
    pub fn new(namespace: impl Into<String>) -> Result<Self, HandlerError> {
        let credential: Arc<dyn TokenCredential> =
            ManagedIdentityCredential::new(None).map_err(|e| {
                HandlerError::Config(format!("create Service Bus credential failed: {e}"))
            })?;
        Ok(Self {
            namespace: namespace.into(),
            credential,
            client: Mutex::new(None),
            senders: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl QueuePublisher for ServiceBusQueuePublisher {
    async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError> {
        if let Some(sender) = self.senders.lock().await.get(queue).cloned() {
            return send_with_cached_sender(sender, payload).await;
        }

        let new_sender = {
            let mut client_guard = self.client.lock().await;
            if client_guard.is_none() {
                *client_guard = Some(
                    ServiceBusClient::new_from_token_credential(
                        self.namespace.clone(),
                        Arc::clone(&self.credential),
                        ServiceBusClientOptions::default(),
                    )
                    .await
                    .map_err(|e| {
                        HandlerError::Publish(format!("connect to Service Bus failed: {e}"))
                    })?,
                );
            }
            let client = client_guard.as_mut().ok_or_else(|| {
                HandlerError::Publish("service bus client cache was not initialized".to_string())
            })?;
            Arc::new(Mutex::new(
                client
                    .create_sender(queue.to_string(), ServiceBusSenderOptions::default())
                    .await
                    .map_err(|e| {
                        HandlerError::Publish(format!("create Service Bus sender failed: {e}"))
                    })?,
            ))
        };

        let sender = {
            let mut senders = self.senders.lock().await;
            senders
                .entry(queue.to_string())
                .or_insert_with(|| Arc::clone(&new_sender))
                .clone()
        };

        send_with_cached_sender(sender, payload).await
    }
}

async fn send_with_cached_sender(
    sender: Arc<Mutex<ServiceBusSender>>,
    payload: Vec<u8>,
) -> Result<(), HandlerError> {
    let mut sender = sender.lock().await;
    sender
        .send_message(ServiceBusMessage::new(payload))
        .await
        .map_err(|e| HandlerError::Publish(format!("send Service Bus message failed: {e}")))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct NodeStateEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    desired_assigned_program_hash: Option<String>,
    desired_schedule_interval_s: Option<u32>,
    observed_current_program_hash: Option<String>,
    observed_assigned_program_hash: Option<String>,
    observed_schedule_interval_s: Option<u32>,
    battery_mv: Option<u32>,
    firmware_abi_version: Option<u32>,
    firmware_version: Option<String>,
    last_checkin_ms: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ProgramRouteEntity {
    #[serde(rename = "PartitionKey")]
    partition_key: String,
    #[serde(rename = "RowKey")]
    row_key: String,
    handler_queue: String,
}

impl TryFrom<NodeStateEntity> for NodeStateRow {
    type Error = HandlerError;

    fn try_from(value: NodeStateEntity) -> Result<Self, Self::Error> {
        Ok(Self {
            node_id: value.row_key,
            desired_assigned_program_hash: decode_optional_program_hash(
                value.desired_assigned_program_hash,
                "desired_assigned_program_hash",
            )?,
            desired_schedule_interval_s: value.desired_schedule_interval_s,
            observed_current_program_hash: decode_optional_program_hash(
                value.observed_current_program_hash,
                "observed_current_program_hash",
            )?,
            observed_assigned_program_hash: decode_optional_program_hash(
                value.observed_assigned_program_hash,
                "observed_assigned_program_hash",
            )?,
            observed_schedule_interval_s: value.observed_schedule_interval_s,
            battery_mv: value.battery_mv,
            firmware_abi_version: value.firmware_abi_version,
            firmware_version: value.firmware_version,
            last_checkin_ms: value.last_checkin_ms,
        })
    }
}

impl From<NodeStateRow> for NodeStateEntity {
    fn from(value: NodeStateRow) -> Self {
        Self {
            partition_key: "node".to_string(),
            row_key: value.node_id,
            desired_assigned_program_hash: encode_optional_hex(value.desired_assigned_program_hash),
            desired_schedule_interval_s: value.desired_schedule_interval_s,
            observed_current_program_hash: encode_optional_hex(value.observed_current_program_hash),
            observed_assigned_program_hash: encode_optional_hex(
                value.observed_assigned_program_hash,
            ),
            observed_schedule_interval_s: value.observed_schedule_interval_s,
            battery_mv: value.battery_mv,
            firmware_abi_version: value.firmware_abi_version,
            firmware_version: value.firmware_version,
            last_checkin_ms: value.last_checkin_ms,
        }
    }
}

impl TryFrom<ProgramRouteEntity> for ProgramRouteRow {
    type Error = HandlerError;

    fn try_from(value: ProgramRouteEntity) -> Result<Self, Self::Error> {
        Ok(Self {
            program_hash: decode_hex_program_hash(value.row_key, "ProgramRoute.RowKey")?,
            handler_queue: value.handler_queue,
        })
    }
}

pub fn extract_trigger_payload(request_body: &[u8]) -> Result<Vec<u8>, HandlerError> {
    let envelope: serde_json::Value = serde_json::from_slice(request_body)?;
    if let Some(value) = envelope
        .get("data")
        .and_then(|data| data.get("message"))
        .or_else(|| envelope.get("Data").and_then(|data| data.get("message")))
        .or_else(|| envelope.get("Data"))
        .or_else(|| envelope.get("Body"))
        .or_else(|| envelope.get("body"))
    {
        return extract_json_payload(value);
    }
    if let Some(object) = envelope.get("data").and_then(|value| value.as_object()) {
        if let Some((_, value)) = object.iter().next() {
            return extract_json_payload(value);
        }
    }
    Err(HandlerError::Decode(
        "custom handler request did not contain a Service Bus payload".to_string(),
    ))
}

fn extract_json_payload(value: &serde_json::Value) -> Result<Vec<u8>, HandlerError> {
    match value {
        serde_json::Value::String(text) => {
            match base64::engine::general_purpose::STANDARD.decode(text) {
                Ok(bytes) => Ok(bytes),
                Err(_) => Ok(text.as_bytes().to_vec()),
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| {
                        HandlerError::Decode(
                            "payload byte array must contain uint8 values".to_string(),
                        )
                    })
            })
            .collect(),
        serde_json::Value::Object(_) => serde_json::to_vec(value).map_err(HandlerError::from),
        other => Err(HandlerError::Decode(format!(
            "unsupported trigger payload shape `{}`",
            other
        ))),
    }
}

fn decode_connector_message(bytes: &[u8]) -> Result<ConnectorMessage, HandlerError> {
    let map = decode_map(bytes)?;
    let msg_type = required_u64(&map, 1, "msg_type")?;
    match msg_type {
        MSG_TYPE_ACTUAL_STATE => Ok(ConnectorMessage::ActualState(ActualStateMessage {
            entity_kind: required_text(&map, 2, "entity_kind")?,
            entity_id: required_text(&map, 3, "entity_id")?,
            current_program_hash: optional_program_hash_field(&map, 4, "current_program_hash")?,
            assigned_program_hash: optional_program_hash_field(&map, 5, "assigned_program_hash")?,
            battery_mv: optional_u32_field(&map, 6, "battery_mv")?,
            firmware_abi_version: optional_u32_field(&map, 7, "firmware_abi_version")?,
            firmware_version: optional_text_field(&map, 8, "firmware_version")?,
            timestamp_ms: required_u64(&map, 9, "timestamp_ms")?,
            schedule_interval_s: optional_u32_field(&map, 11, "schedule_interval_s")?,
        })),
        MSG_TYPE_APP_DATA => Ok(ConnectorMessage::AppData(AppDataMessage {
            node_id: required_text(&map, 2, "node_id")?,
            program_hash: required_program_hash(&map, 3, "program_hash")?,
            payload: required_bytes(&map, 4, "payload")?,
            timestamp_ms: required_u64(&map, 5, "timestamp_ms")?,
        })),
        other => Ok(ConnectorMessage::Unsupported(other)),
    }
}

pub fn encode_desired_state(row: &NodeStateRow) -> Result<Vec<u8>, HandlerError> {
    let desired_state = Value::Map(vec![
        map_entry(
            1,
            opt_bytes_value(row.desired_assigned_program_hash.as_deref()),
        ),
        map_entry(2, opt_u32_value(row.desired_schedule_interval_s)),
    ]);
    let value = Value::Map(vec![
        map_entry(1, Value::Integer(MSG_TYPE_DESIRED_STATE.into())),
        map_entry(2, Value::Text("node".to_string())),
        map_entry(3, Value::Text(row.node_id.clone())),
        map_entry(4, desired_state),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes)
        .map_err(|e| HandlerError::Decode(format!("encode desired state failed: {e}")))?;
    Ok(bytes)
}

fn encode_optional_hex(value: Option<Vec<u8>>) -> Option<String> {
    value.map(hex::encode)
}

fn decode_optional_program_hash(
    value: Option<String>,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    value
        .map(|text| decode_hex_program_hash(text, field))
        .transpose()
}

fn decode_map(bytes: &[u8]) -> Result<Vec<(Value, Value)>, HandlerError> {
    let value: Value = ciborium::from_reader(bytes)
        .map_err(|e| HandlerError::Decode(format!("failed to decode connector CBOR: {e}")))?;
    value
        .as_map()
        .cloned()
        .ok_or_else(|| HandlerError::Decode("connector payload must be a CBOR map".to_string()))
}

fn map_entry(key: u64, value: Value) -> (Value, Value) {
    (Value::Integer(key.into()), value)
}

fn map_get(map: &[(Value, Value)], key: u64) -> Option<&Value> {
    map.iter().find_map(
        |(k, v)| match k.as_integer().and_then(|i| u64::try_from(i).ok()) {
            Some(found) if found == key => Some(v),
            _ => None,
        },
    )
}

fn required_u64(map: &[(Value, Value)], key: u64, field: &str) -> Result<u64, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| {
            value
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint")))
        })
}

fn required_text(map: &[(Value, Value)], key: u64, field: &str) -> Result<String, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| {
            value
                .as_text()
                .map(|text| text.to_string())
                .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be text")))
        })
}

fn required_bytes(map: &[(Value, Value)], key: u64, field: &str) -> Result<Vec<u8>, HandlerError> {
    map_get(map, key)
        .ok_or_else(|| HandlerError::Decode(format!("missing `{field}`")))
        .and_then(|value| match value {
            Value::Bytes(bytes) => Ok(bytes.clone()),
            _ => Err(HandlerError::Decode(format!("`{field}` must be bstr"))),
        })
}

fn required_program_hash(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Vec<u8>, HandlerError> {
    let bytes = required_bytes(map, key, field)?;
    validate_program_hash_length(&bytes, field)?;
    Ok(bytes)
}

fn optional_program_hash_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    let value = optional_bytes_field(map, key, field)?;
    if let Some(bytes) = value.as_deref() {
        validate_program_hash_length(bytes, field)?;
    }
    Ok(value)
}

fn optional_bytes_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<Vec<u8>>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Bytes(bytes)) => Ok(Some(bytes.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(HandlerError::Decode(format!(
            "`{field}` must be bstr or null"
        ))),
    }
}

fn optional_u32_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<u32>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .and_then(|v| u32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be uint or null"))),
    }
}

fn optional_text_field(
    map: &[(Value, Value)],
    key: u64,
    field: &str,
) -> Result<Option<String>, HandlerError> {
    match map_get(map, key) {
        Some(Value::Null) | None => Ok(None),
        Some(value) => value
            .as_text()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| HandlerError::Decode(format!("`{field}` must be text or null"))),
    }
}

fn opt_bytes_value(value: Option<&[u8]>) -> Value {
    match value {
        Some(bytes) => Value::Bytes(bytes.to_vec()),
        None => Value::Null,
    }
}

fn opt_u32_value(value: Option<u32>) -> Value {
    match value {
        Some(v) => Value::Integer(u64::from(v).into()),
        None => Value::Null,
    }
}

fn decode_hex_program_hash(text: String, field: &str) -> Result<Vec<u8>, HandlerError> {
    let bytes = hex::decode(&text)
        .map_err(|e| HandlerError::Store(format!("`{field}` must contain valid hex: {e}")))?;
    validate_program_hash_length(&bytes, field).map_err(|e| HandlerError::Store(e.to_string()))?;
    Ok(bytes)
}

fn validate_program_hash_length(bytes: &[u8], field: &str) -> Result<(), HandlerError> {
    if bytes.len() == 32 {
        return Ok(());
    }
    Err(HandlerError::Decode(format!(
        "`{field}` must be exactly 32 bytes"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        nodes: Mutex<HashMap<String, NodeStateRow>>,
        routes: Mutex<HashMap<String, ProgramRouteRow>>,
    }

    #[async_trait]
    impl HandlerStore for MemoryStore {
        async fn load_node_state(
            &self,
            node_id: &str,
        ) -> Result<Option<NodeStateRow>, HandlerError> {
            Ok(self.nodes.lock().await.get(node_id).cloned())
        }

        async fn save_node_state(&self, row: &NodeStateRow) -> Result<(), HandlerError> {
            self.nodes
                .lock()
                .await
                .insert(row.node_id.clone(), row.clone());
            Ok(())
        }

        async fn create_node_state_if_absent(
            &self,
            row: &NodeStateRow,
        ) -> Result<bool, HandlerError> {
            let mut nodes = self.nodes.lock().await;
            if nodes.contains_key(&row.node_id) {
                return Ok(false);
            }
            nodes.insert(row.node_id.clone(), row.clone());
            Ok(true)
        }

        async fn update_observed_state(
            &self,
            update: &ObservedStateUpdate,
        ) -> Result<ObservedStateWriteResult, HandlerError> {
            let mut nodes = self.nodes.lock().await;
            let row = nodes.get_mut(&update.node_id).ok_or_else(|| {
                HandlerError::Store(format!(
                    "missing node `{}` for observed update",
                    update.node_id
                ))
            })?;
            if update.timestamp_ms < row.last_checkin_ms {
                return Ok(ObservedStateWriteResult::IgnoredStale);
            }
            row.observed_current_program_hash = update.current_program_hash.clone();
            row.observed_assigned_program_hash = update.assigned_program_hash.clone();
            row.observed_schedule_interval_s = update.schedule_interval_s;
            row.battery_mv = update.battery_mv;
            row.firmware_abi_version = update.firmware_abi_version;
            row.firmware_version = update.firmware_version.clone();
            row.last_checkin_ms = update.timestamp_ms;
            Ok(ObservedStateWriteResult::Applied)
        }

        async fn load_program_route(
            &self,
            program_hash: &[u8],
        ) -> Result<Option<ProgramRouteRow>, HandlerError> {
            Ok(self
                .routes
                .lock()
                .await
                .get(&hex::encode(program_hash))
                .cloned())
        }
    }

    #[derive(Default)]
    struct RecordingPublisher {
        sends: Mutex<Vec<(String, Vec<u8>)>>,
    }

    #[async_trait]
    impl QueuePublisher for RecordingPublisher {
        async fn publish(&self, queue: &str, payload: Vec<u8>) -> Result<(), HandlerError> {
            self.sends.lock().await.push((queue.to_string(), payload));
            Ok(())
        }
    }

    struct FailingStore {
        load_node_error: Option<String>,
        save_error: Option<String>,
        load_route_error: Option<String>,
    }

    #[async_trait]
    impl HandlerStore for FailingStore {
        async fn load_node_state(
            &self,
            _node_id: &str,
        ) -> Result<Option<NodeStateRow>, HandlerError> {
            match &self.load_node_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(None),
            }
        }

        async fn save_node_state(&self, _row: &NodeStateRow) -> Result<(), HandlerError> {
            match &self.save_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(()),
            }
        }

        async fn create_node_state_if_absent(
            &self,
            _row: &NodeStateRow,
        ) -> Result<bool, HandlerError> {
            match &self.save_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(true),
            }
        }

        async fn update_observed_state(
            &self,
            _update: &ObservedStateUpdate,
        ) -> Result<ObservedStateWriteResult, HandlerError> {
            match &self.save_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(ObservedStateWriteResult::Applied),
            }
        }

        async fn load_program_route(
            &self,
            _program_hash: &[u8],
        ) -> Result<Option<ProgramRouteRow>, HandlerError> {
            match &self.load_route_error {
                Some(error) => Err(HandlerError::Store(error.clone())),
                None => Ok(None),
            }
        }
    }

    struct FailingPublisher {
        error: String,
    }

    #[async_trait]
    impl QueuePublisher for FailingPublisher {
        async fn publish(&self, _queue: &str, _payload: Vec<u8>) -> Result<(), HandlerError> {
            Err(HandlerError::Publish(self.error.clone()))
        }
    }

    fn sample_actual_state(
        node_id: &str,
        current_program_hash: Option<&[u8]>,
        assigned_program_hash: Option<&[u8]>,
        schedule_interval_s: Option<u32>,
    ) -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("node".to_string())),
            map_entry(3, Value::Text(node_id.to_string())),
            map_entry(4, opt_bytes_value(current_program_hash)),
            map_entry(5, opt_bytes_value(assigned_program_hash)),
            map_entry(6, Value::Integer(3300u64.into())),
            map_entry(7, Value::Integer(1u64.into())),
            map_entry(8, Value::Text("1.2.3".to_string())),
            map_entry(9, Value::Integer(1234u64.into())),
            map_entry(10, Value::Map(Vec::new())),
            map_entry(11, opt_u32_value(schedule_interval_s)),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_app_data(program_hash: &[u8], payload: &[u8]) -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_APP_DATA.into())),
            map_entry(2, Value::Text("node-1".to_string())),
            map_entry(3, Value::Bytes(program_hash.to_vec())),
            map_entry(4, Value::Bytes(payload.to_vec())),
            map_entry(5, Value::Integer(321u64.into())),
            map_entry(6, Value::Text("app_data".to_string())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn sample_unsupported() -> Vec<u8> {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(0x99u64.into())),
            map_entry(2, Value::Text("ignored".to_string())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn first_actual_state_seeds_row_without_publish() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                None,
                Some(60),
            ))
            .await
            .unwrap();

        let row = store.load_node_state("node-1").await.unwrap().unwrap();
        assert_eq!(row.desired_assigned_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(row.desired_schedule_interval_s, Some(60));
        assert_eq!(row.observed_current_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(row.observed_assigned_program_hash, None);
        assert_eq!(row.observed_schedule_interval_s, Some(60));
        assert_eq!(row.battery_mv, Some(3300));
        assert_eq!(row.firmware_abi_version, Some(1));
        assert_eq!(row.firmware_version.as_deref(), Some("1.2.3"));
        assert_eq!(row.last_checkin_ms, 1234);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn first_actual_state_seeds_from_assigned_program_when_current_is_missing() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                None,
                Some(&[0xCC; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let row = store.load_node_state("node-1").await.unwrap().unwrap();
        assert_eq!(row.desired_assigned_program_hash, Some(vec![0xCC; 32]));
        assert_eq!(row.observed_current_program_hash, None);
        assert_eq!(row.observed_assigned_program_hash, Some(vec![0xCC; 32]));
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn divergence_publishes_full_desired_state_without_ephemeral() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xBB; 32]),
                desired_schedule_interval_s: Some(120),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(1),
                firmware_version: Some("1.0.0".to_string()),
                last_checkin_ms: 1,
            })
            .await
            .unwrap();

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "desired-state");
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            required_u64(&desired, 1, "msg_type").unwrap(),
            MSG_TYPE_DESIRED_STATE
        );
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xBB; 32])
        );
        assert_eq!(
            optional_u32_field(desired_state, 2, "schedule_interval_s").unwrap(),
            Some(120)
        );
        assert!(map_get(desired_state, 3).is_none());
    }

    #[tokio::test]
    async fn unsupported_messages_do_not_mutate_state() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        handler.handle_payload(&sample_unsupported()).await.unwrap();

        assert!(store.nodes.lock().await.is_empty());
        assert!(store.routes.lock().await.is_empty());
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn aligned_actual_state_refreshes_observed_fields_without_publish() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xAA; 32]),
                desired_schedule_interval_s: Some(60),
                observed_current_program_hash: Some(vec![0x10; 32]),
                observed_assigned_program_hash: Some(vec![0x20; 32]),
                observed_schedule_interval_s: Some(30),
                battery_mv: Some(3100),
                firmware_abi_version: Some(9),
                firmware_version: Some("0.9.0".to_string()),
                last_checkin_ms: 7,
            })
            .await
            .unwrap();

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xCC; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let row = store.load_node_state("node-1").await.unwrap().unwrap();
        assert_eq!(row.desired_assigned_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(row.desired_schedule_interval_s, Some(60));
        assert_eq!(row.observed_current_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(row.observed_assigned_program_hash, Some(vec![0xCC; 32]));
        assert_eq!(row.observed_schedule_interval_s, Some(60));
        assert_eq!(row.battery_mv, Some(3300));
        assert_eq!(row.firmware_abi_version, Some(1));
        assert_eq!(row.firmware_version.as_deref(), Some("1.2.3"));
        assert_eq!(row.last_checkin_ms, 1234);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn stale_actual_state_is_ignored() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xAA; 32]),
                desired_schedule_interval_s: Some(60),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(2),
                firmware_version: Some("2.0.0".to_string()),
                last_checkin_ms: 5000,
            })
            .await
            .unwrap();

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xBB; 32]),
                Some(&[0xBB; 32]),
                Some(120),
            ))
            .await
            .unwrap();

        let row = store.load_node_state("node-1").await.unwrap().unwrap();
        assert_eq!(row.observed_current_program_hash, Some(vec![0xAA; 32]));
        assert_eq!(row.observed_schedule_interval_s, Some(60));
        assert_eq!(row.battery_mv, Some(3200));
        assert_eq!(row.last_checkin_ms, 5000);
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[tokio::test]
    async fn schedule_only_divergence_publishes_one_desired_state() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xAA; 32]),
                desired_schedule_interval_s: Some(120),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(120),
                battery_mv: Some(3200),
                firmware_abi_version: Some(1),
                firmware_version: Some("1.0.0".to_string()),
                last_checkin_ms: 1,
            })
            .await
            .unwrap();

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xAA; 32])
        );
        assert_eq!(
            optional_u32_field(desired_state, 2, "schedule_interval_s").unwrap(),
            Some(120)
        );
    }

    #[tokio::test]
    async fn program_only_divergence_publishes_one_desired_state() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xBB; 32]),
                desired_schedule_interval_s: Some(60),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(1),
                firmware_version: Some("1.0.0".to_string()),
                last_checkin_ms: 1,
            })
            .await
            .unwrap();

        handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        let desired = decode_map(&sends[0].1).unwrap();
        let desired_state = map_get(&desired, 4).unwrap().as_map().unwrap();
        assert_eq!(
            optional_bytes_field(desired_state, 1, "assigned_program_hash").unwrap(),
            Some(vec![0xBB; 32])
        );
        assert_eq!(
            optional_u32_field(desired_state, 2, "schedule_interval_s").unwrap(),
            Some(60)
        );
    }

    #[tokio::test]
    async fn app_data_routes_to_mapped_queue() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");
        store.routes.lock().await.insert(
            hex::encode([0x44; 32]),
            ProgramRouteRow {
                program_hash: vec![0x44; 32],
                handler_queue: "handler-q".to_string(),
            },
        );

        let payload = sample_app_data(&[0x44; 32], &[1, 2, 3]);
        handler.handle_payload(&payload).await.unwrap();

        let sends = publisher.sends.lock().await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "handler-q");
        assert_eq!(sends[0].1, payload);
    }

    #[tokio::test]
    async fn unmapped_app_data_fails_closed() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_app_data(&[0x11; 32], &[9, 8, 7]))
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("no ProgramRoute row exists for program hash"));
        assert!(publisher.sends.lock().await.is_empty());
    }

    #[test]
    fn extract_trigger_payload_decodes_base64_data_field() {
        let raw = vec![1u8, 2, 3, 4];
        let body = serde_json::json!({
            "Data": base64::engine::general_purpose::STANDARD.encode(&raw)
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn extract_trigger_payload_decodes_uppercase_data_message_field() {
        let raw = vec![9u8, 8, 7, 6];
        let body = serde_json::json!({
            "Data": {
                "message": base64::engine::general_purpose::STANDARD.encode(&raw)
            }
        });
        let decoded =
            extract_trigger_payload(serde_json::to_string(&body).unwrap().as_bytes()).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn actual_state_rejects_non_sha256_program_hash() {
        let value = Value::Map(vec![
            map_entry(1, Value::Integer(MSG_TYPE_ACTUAL_STATE.into())),
            map_entry(2, Value::Text("node".to_string())),
            map_entry(3, Value::Text("node-1".to_string())),
            map_entry(4, Value::Bytes(vec![0xAA; 31])),
            map_entry(5, Value::Null),
            map_entry(6, Value::Integer(3300u64.into())),
            map_entry(7, Value::Integer(1u64.into())),
            map_entry(8, Value::Text("1.2.3".to_string())),
            map_entry(9, Value::Integer(1234u64.into())),
            map_entry(10, Value::Map(Vec::new())),
            map_entry(11, Value::Integer(60u64.into())),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&value, &mut bytes).unwrap();

        let err = decode_connector_message(&bytes).unwrap_err();
        assert!(err
            .to_string()
            .contains("`current_program_hash` must be exactly 32 bytes"));
    }

    #[test]
    fn app_data_rejects_non_sha256_program_hash() {
        let payload = sample_app_data(&[0x44; 31], &[1, 2, 3]);
        let err = decode_connector_message(&payload).unwrap_err();
        assert!(err
            .to_string()
            .contains("`program_hash` must be exactly 32 bytes"));
    }

    #[test]
    fn node_state_row_decode_fails_closed_on_invalid_hex() {
        let err = NodeStateRow::try_from(NodeStateEntity {
            partition_key: "node".to_string(),
            row_key: "node-1".to_string(),
            desired_assigned_program_hash: Some("not-hex".to_string()),
            desired_schedule_interval_s: Some(60),
            observed_current_program_hash: None,
            observed_assigned_program_hash: None,
            observed_schedule_interval_s: Some(60),
            battery_mv: Some(3300),
            firmware_abi_version: Some(1),
            firmware_version: Some("1.2.3".to_string()),
            last_checkin_ms: 1234,
        })
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("`desired_assigned_program_hash` must contain valid hex"));
    }

    #[tokio::test]
    async fn table_write_failures_are_surfaced() {
        let store = Arc::new(FailingStore {
            load_node_error: None,
            save_error: Some("save failed".to_string()),
            load_route_error: None,
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                None,
                Some(60),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("save failed"));
    }

    #[tokio::test]
    async fn downstream_publish_failures_are_surfaced() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(FailingPublisher {
            error: "downstream publish failed".to_string(),
        });
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store
            .save_node_state(&NodeStateRow {
                node_id: "node-1".to_string(),
                desired_assigned_program_hash: Some(vec![0xBB; 32]),
                desired_schedule_interval_s: Some(120),
                observed_current_program_hash: Some(vec![0xAA; 32]),
                observed_assigned_program_hash: Some(vec![0xAA; 32]),
                observed_schedule_interval_s: Some(60),
                battery_mv: Some(3200),
                firmware_abi_version: Some(1),
                firmware_version: Some("1.0.0".to_string()),
                last_checkin_ms: 1,
            })
            .await
            .unwrap();

        let err = handler
            .handle_payload(&sample_actual_state(
                "node-1",
                Some(&[0xAA; 32]),
                Some(&[0xAA; 32]),
                Some(60),
            ))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("downstream publish failed"));
    }

    #[tokio::test]
    async fn handler_queue_publish_failures_are_surfaced() {
        let store = Arc::new(MemoryStore::default());
        let publisher = Arc::new(FailingPublisher {
            error: "handler queue publish failed".to_string(),
        });
        let handler =
            AzureHandler::new(Arc::clone(&store), Arc::clone(&publisher), "desired-state");

        store.routes.lock().await.insert(
            hex::encode([0x44; 32]),
            ProgramRouteRow {
                program_hash: vec![0x44; 32],
                handler_queue: "handler-q".to_string(),
            },
        );

        let err = handler
            .handle_payload(&sample_app_data(&[0x44; 32], &[1, 2, 3]))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("handler queue publish failed"));
    }
}
