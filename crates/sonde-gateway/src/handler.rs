// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::BTreeMap;
use std::io;
use std::process::Stdio;

use ciborium::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

const MAX_MESSAGE_SIZE: u32 = 1_048_576;

const MSG_TYPE_DATA: u64 = 0x01;
const MSG_TYPE_EVENT: u64 = 0x02;
const MSG_TYPE_DATA_REPLY: u64 = 0x81;
const MSG_TYPE_LOG: u64 = 0x82;

// --- Message types ---

#[derive(Debug, Clone, PartialEq)]
pub enum HandlerMessage {
    Data {
        request_id: u64,
        node_id: String,
        program_hash: Vec<u8>,
        data: Vec<u8>,
        timestamp: u64,
    },
    Event {
        node_id: String,
        event_type: String,
        details: BTreeMap<String, Value>,
        timestamp: u64,
    },
    DataReply {
        request_id: u64,
        data: Vec<u8>,
    },
    Log {
        level: String,
        message: String,
    },
}

impl HandlerMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let map = match self {
            HandlerMessage::Data {
                request_id,
                node_id,
                program_hash,
                data,
                timestamp,
            } => Value::Map(vec![
                (
                    Value::Integer(1.into()),
                    Value::Integer(MSG_TYPE_DATA.into()),
                ),
                (
                    Value::Integer(2.into()),
                    Value::Integer((*request_id).into()),
                ),
                (Value::Integer(3.into()), Value::Text(node_id.clone())),
                (Value::Integer(4.into()), Value::Bytes(program_hash.clone())),
                (Value::Integer(5.into()), Value::Bytes(data.clone())),
                (
                    Value::Integer(6.into()),
                    Value::Integer((*timestamp).into()),
                ),
            ]),
            HandlerMessage::Event {
                node_id,
                event_type,
                details,
                timestamp,
            } => {
                let details_cbor = Value::Map(
                    details
                        .iter()
                        .map(|(k, v)| (Value::Text(k.clone()), v.clone()))
                        .collect(),
                );
                Value::Map(vec![
                    (
                        Value::Integer(1.into()),
                        Value::Integer(MSG_TYPE_EVENT.into()),
                    ),
                    (Value::Integer(3.into()), Value::Text(node_id.clone())),
                    (Value::Integer(4.into()), Value::Text(event_type.clone())),
                    (Value::Integer(5.into()), details_cbor),
                    (
                        Value::Integer(6.into()),
                        Value::Integer((*timestamp).into()),
                    ),
                ])
            }
            HandlerMessage::DataReply { request_id, data } => Value::Map(vec![
                (
                    Value::Integer(1.into()),
                    Value::Integer(MSG_TYPE_DATA_REPLY.into()),
                ),
                (
                    Value::Integer(2.into()),
                    Value::Integer((*request_id).into()),
                ),
                (Value::Integer(3.into()), Value::Bytes(data.clone())),
            ]),
            HandlerMessage::Log { level, message } => Value::Map(vec![
                (
                    Value::Integer(1.into()),
                    Value::Integer(MSG_TYPE_LOG.into()),
                ),
                (Value::Integer(2.into()), Value::Text(level.clone())),
                (Value::Integer(3.into()), Value::Text(message.clone())),
            ]),
        };

        let mut buf = Vec::new();
        ciborium::into_writer(&map, &mut buf).map_err(|_| EncodeError)?;
        Ok(buf)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let value: Value =
            ciborium::from_reader(bytes).map_err(|_| DecodeError("invalid CBOR".into()))?;

        let map = match &value {
            Value::Map(m) => m,
            _ => return Err(DecodeError("expected CBOR map".into())),
        };

        let msg_type = get_uint(map, 1).ok_or_else(|| DecodeError("missing msg_type".into()))?;

        match msg_type {
            MSG_TYPE_DATA => {
                let request_id =
                    get_uint(map, 2).ok_or_else(|| DecodeError("missing request_id".into()))?;
                let node_id =
                    get_text(map, 3).ok_or_else(|| DecodeError("missing node_id".into()))?;
                let program_hash =
                    get_bytes(map, 4).ok_or_else(|| DecodeError("missing program_hash".into()))?;
                let data = get_bytes(map, 5).ok_or_else(|| DecodeError("missing data".into()))?;
                let timestamp =
                    get_uint(map, 6).ok_or_else(|| DecodeError("missing timestamp".into()))?;
                Ok(HandlerMessage::Data {
                    request_id,
                    node_id,
                    program_hash,
                    data,
                    timestamp,
                })
            }
            MSG_TYPE_EVENT => {
                let node_id =
                    get_text(map, 3).ok_or_else(|| DecodeError("missing node_id".into()))?;
                let event_type =
                    get_text(map, 4).ok_or_else(|| DecodeError("missing event_type".into()))?;
                let details_val =
                    get_value(map, 5).ok_or_else(|| DecodeError("missing details".into()))?;
                let details = decode_details(details_val)?;
                let timestamp =
                    get_uint(map, 6).ok_or_else(|| DecodeError("missing timestamp".into()))?;
                Ok(HandlerMessage::Event {
                    node_id,
                    event_type,
                    details,
                    timestamp,
                })
            }
            MSG_TYPE_DATA_REPLY => {
                let request_id =
                    get_uint(map, 2).ok_or_else(|| DecodeError("missing request_id".into()))?;
                let data = get_bytes(map, 3).ok_or_else(|| DecodeError("missing data".into()))?;
                Ok(HandlerMessage::DataReply { request_id, data })
            }
            MSG_TYPE_LOG => {
                let level = get_text(map, 2).ok_or_else(|| DecodeError("missing level".into()))?;
                let message =
                    get_text(map, 3).ok_or_else(|| DecodeError("missing message".into()))?;
                Ok(HandlerMessage::Log { level, message })
            }
            _ => Err(DecodeError(format!("unknown msg_type: {msg_type:#x}"))),
        }
    }
}

fn get_value(map: &[(Value, Value)], key: i128) -> Option<&Value> {
    map.iter().find_map(|(k, v)| match k {
        Value::Integer(i) if i128::from(*i) == key => Some(v),
        _ => None,
    })
}

fn get_uint(map: &[(Value, Value)], key: i128) -> Option<u64> {
    get_value(map, key).and_then(|v| match v {
        Value::Integer(i) => {
            let val = i128::from(*i);
            u64::try_from(val).ok()
        }
        _ => None,
    })
}

fn get_text(map: &[(Value, Value)], key: i128) -> Option<String> {
    get_value(map, key).and_then(|v| match v {
        Value::Text(s) => Some(s.clone()),
        _ => None,
    })
}

fn get_bytes(map: &[(Value, Value)], key: i128) -> Option<Vec<u8>> {
    get_value(map, key).and_then(|v| match v {
        Value::Bytes(b) => Some(b.clone()),
        _ => None,
    })
}

fn decode_details(val: &Value) -> Result<BTreeMap<String, Value>, DecodeError> {
    match val {
        Value::Map(entries) => {
            let mut out = BTreeMap::new();
            for (k, v) in entries {
                let key = match k {
                    Value::Text(s) => s.clone(),
                    _ => return Err(DecodeError("details key must be text".into())),
                };
                out.insert(key, v.clone());
            }
            Ok(out)
        }
        _ => Err(DecodeError("details must be a map".into())),
    }
}

#[derive(Debug, Clone)]
pub struct EncodeError;

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CBOR encode error")
    }
}

impl std::error::Error for EncodeError {}

#[derive(Debug, Clone)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CBOR decode error: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

// --- Framing: 4-byte BE length prefix ---

pub async fn write_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &HandlerMessage,
) -> io::Result<()> {
    let payload = msg
        .encode()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message exceeds 1 MB limit",
        ));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<HandlerMessage> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message size {len} exceeds 1 MB limit"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    HandlerMessage::decode(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// --- Configuration ---

#[derive(Debug, Clone)]
pub enum ProgramMatcher {
    Any,
    Hash(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct HandlerConfig {
    pub matchers: Vec<ProgramMatcher>,
    pub command: String,
    pub args: Vec<String>,
}

// --- HandlerProcess ---

pub struct HandlerProcess {
    config: HandlerConfig,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<BufReader<ChildStdout>>,
}

impl HandlerProcess {
    pub fn new(config: HandlerConfig) -> Self {
        Self {
            config,
            child: None,
            stdin: None,
            stdout_reader: None,
        }
    }

    fn ensure_running(&mut self) -> io::Result<()> {
        let needs_spawn = match &mut self.child {
            Some(child) => match child.try_wait()? {
                Some(_status) => {
                    self.stdin = None;
                    self.stdout_reader = None;
                    self.child = None;
                    true
                }
                None => false,
            },
            None => true,
        };

        if needs_spawn {
            let mut child = Command::new(&self.config.command)
                .args(&self.config.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;

            self.stdin = child.stdin.take();
            self.stdout_reader = child.stdout.take().map(BufReader::new);
            self.child = Some(child);
            debug!(command = %self.config.command, "spawned handler process");
        }

        Ok(())
    }

    /// Send a DATA message and read the response. LOG messages interleaved
    /// on stdout are consumed and logged. Returns the DATA_REPLY if one arrives
    /// with a matching `request_id`, or `None` on handler crash / mismatch.
    pub async fn send_data(&mut self, msg: &HandlerMessage) -> Option<HandlerMessage> {
        if let Err(e) = self.ensure_running() {
            error!(error = %e, "failed to spawn handler");
            return None;
        }

        let expected_request_id = match msg {
            HandlerMessage::Data { request_id, .. } => *request_id,
            _ => return None,
        };

        let stdin = self.stdin.as_mut()?;
        if let Err(e) = write_message(stdin, msg).await {
            error!(error = %e, "failed to write to handler stdin");
            self.kill_child().await;
            return None;
        }

        let reader = self.stdout_reader.as_mut()?;
        loop {
            match read_message(reader).await {
                Ok(HandlerMessage::Log { level, message }) => match level.as_str() {
                    "error" => error!(handler = %self.config.command, "{message}"),
                    "warn" => warn!(handler = %self.config.command, "{message}"),
                    "debug" => debug!(handler = %self.config.command, "{message}"),
                    _ => debug!(handler = %self.config.command, level = %level, "{message}"),
                },
                Ok(reply @ HandlerMessage::DataReply { .. }) => {
                    if let HandlerMessage::DataReply { request_id, .. } = &reply {
                        if *request_id != expected_request_id {
                            warn!(
                                expected = expected_request_id,
                                got = *request_id,
                                "handler reply request_id mismatch — discarding"
                            );
                            return None;
                        }
                    }
                    return Some(reply);
                }
                Ok(other) => {
                    warn!(msg_type = ?other, "unexpected message type from handler — ignoring");
                }
                Err(e) => {
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        debug!("handler process closed stdout");
                    } else {
                        error!(error = %e, "error reading from handler stdout");
                    }
                    self.check_exit_status().await;
                    return None;
                }
            }
        }
    }

    /// Send an EVENT message (fire-and-forget, no response expected).
    pub async fn send_event(&mut self, msg: &HandlerMessage) {
        if let Err(e) = self.ensure_running() {
            error!(error = %e, "failed to spawn handler for event");
            return;
        }

        let stdin = match self.stdin.as_mut() {
            Some(s) => s,
            None => return,
        };

        if let Err(e) = write_message(stdin, msg).await {
            error!(error = %e, "failed to write event to handler stdin");
            self.kill_child().await;
        }
    }

    async fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
        }
        self.stdin = None;
        self.stdout_reader = None;
    }

    async fn check_exit_status(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.wait().await {
                Ok(status) if status.success() => {
                    debug!(command = %self.config.command, "handler exited cleanly");
                }
                Ok(status) => {
                    error!(
                        command = %self.config.command,
                        status = %status,
                        "handler exited with error"
                    );
                }
                Err(e) => {
                    error!(
                        command = %self.config.command,
                        error = %e,
                        "failed to get handler exit status"
                    );
                }
            }
        }
        self.stdin = None;
        self.stdout_reader = None;
    }
}

// --- HandlerRouter ---

pub struct HandlerRouter {
    handlers: Vec<(HandlerConfig, Mutex<HandlerProcess>)>,
}

impl HandlerRouter {
    pub fn new(configs: Vec<HandlerConfig>) -> Self {
        let handlers = configs
            .into_iter()
            .map(|c| {
                let process = HandlerProcess::new(c.clone());
                (c, Mutex::new(process))
            })
            .collect();
        Self { handlers }
    }

    fn find_handler(&self, program_hash: &[u8]) -> Option<usize> {
        // First pass: look for exact hash match
        for (i, (config, _)) in self.handlers.iter().enumerate() {
            for matcher in &config.matchers {
                if let ProgramMatcher::Hash(h) = matcher {
                    if h == program_hash {
                        return Some(i);
                    }
                }
            }
        }
        // Second pass: look for catch-all
        for (i, (config, _)) in self.handlers.iter().enumerate() {
            for matcher in &config.matchers {
                if matches!(matcher, ProgramMatcher::Any) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Route APP_DATA to the matching handler. Returns the reply data blob,
    /// or `None` if no handler matched, the handler crashed, or the reply
    /// was empty.
    pub async fn route_app_data(
        &self,
        node_id: &str,
        program_hash: &[u8],
        data: &[u8],
        timestamp: u64,
        request_id: u64,
    ) -> Option<Vec<u8>> {
        let idx = self.find_handler(program_hash)?;
        let (_, process_mutex) = &self.handlers[idx];
        let mut process = process_mutex.lock().await;

        let msg = HandlerMessage::Data {
            request_id,
            node_id: node_id.to_string(),
            program_hash: program_hash.to_vec(),
            data: data.to_vec(),
            timestamp,
        };

        let reply = process.send_data(&msg).await?;
        match reply {
            HandlerMessage::DataReply { data, .. } => {
                if data.is_empty() {
                    None
                } else {
                    Some(data)
                }
            }
            _ => None,
        }
    }

    /// Route an EVENT to all handlers matching the given program hash.
    pub async fn route_event(
        &self,
        node_id: &str,
        event_type: &str,
        details: BTreeMap<String, Value>,
        timestamp: u64,
    ) {
        let msg = HandlerMessage::Event {
            node_id: node_id.to_string(),
            event_type: event_type.to_string(),
            details,
            timestamp,
        };

        for (_, process_mutex) in &self.handlers {
            let mut process = process_mutex.lock().await;
            process.send_event(&msg).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_message_roundtrip() {
        let msg = HandlerMessage::Data {
            request_id: 42,
            node_id: "node-01".to_string(),
            program_hash: vec![0xAA, 0xBB, 0xCC],
            data: vec![0x01, 0x02, 0x03],
            timestamp: 1700000000,
        };
        let encoded = msg.encode().unwrap();
        let decoded = HandlerMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_data_reply_message_roundtrip() {
        let msg = HandlerMessage::DataReply {
            request_id: 42,
            data: vec![0xDE, 0xAD],
        };
        let encoded = msg.encode().unwrap();
        let decoded = HandlerMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_event_message_roundtrip() {
        let mut details = BTreeMap::new();
        details.insert("battery_mv".to_string(), Value::Integer(3300.into()));
        let msg = HandlerMessage::Event {
            node_id: "node-01".to_string(),
            event_type: "node_online".to_string(),
            details,
            timestamp: 1700000000,
        };
        let encoded = msg.encode().unwrap();
        let decoded = HandlerMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_log_message_roundtrip() {
        let msg = HandlerMessage::Log {
            level: "info".to_string(),
            message: "test log message".to_string(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = HandlerMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_data_message_cbor_keys() {
        let msg = HandlerMessage::Data {
            request_id: 1,
            node_id: "n".to_string(),
            program_hash: vec![0xFF],
            data: vec![0x00],
            timestamp: 100,
        };
        let encoded = msg.encode().unwrap();
        let val: Value = ciborium::from_reader(&encoded[..]).unwrap();
        let map = match val {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        // Verify integer keys 1..6 are present
        let keys: Vec<i128> = map
            .iter()
            .map(|(k, _)| match k {
                Value::Integer(i) => i128::from(*i),
                _ => panic!("expected integer key"),
            })
            .collect();
        assert_eq!(keys, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_decode_unknown_msg_type() {
        let val = Value::Map(vec![(
            Value::Integer(1.into()),
            Value::Integer(0xFF.into()),
        )]);
        let mut buf = Vec::new();
        ciborium::into_writer(&val, &mut buf).unwrap();
        let err = HandlerMessage::decode(&buf).unwrap_err();
        assert!(err.0.contains("unknown msg_type"));
    }

    #[tokio::test]
    async fn test_framing_roundtrip() {
        let msg = HandlerMessage::DataReply {
            request_id: 99,
            data: vec![0x01, 0x02],
        };

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        // Verify 4-byte BE length prefix
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len as usize, buf.len() - 4);

        let mut cursor = &buf[..];
        let decoded = read_message(&mut cursor).await.unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_handler_router_find_exact_match() {
        let router = HandlerRouter::new(vec![
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xAA])],
                command: "handler_a".to_string(),
                args: vec![],
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xBB])],
                command: "handler_b".to_string(),
                args: vec![],
            },
        ]);

        assert_eq!(router.find_handler(&[0xAA]), Some(0));
        assert_eq!(router.find_handler(&[0xBB]), Some(1));
        assert_eq!(router.find_handler(&[0xCC]), None);
    }

    #[test]
    fn test_handler_router_catch_all() {
        let router = HandlerRouter::new(vec![
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xAA])],
                command: "handler_a".to_string(),
                args: vec![],
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Any],
                command: "catch_all".to_string(),
                args: vec![],
            },
        ]);

        assert_eq!(router.find_handler(&[0xAA]), Some(0));
        assert_eq!(router.find_handler(&[0xCC]), Some(1));
    }

    #[test]
    fn test_handler_router_exact_match_takes_priority() {
        let router = HandlerRouter::new(vec![
            HandlerConfig {
                matchers: vec![ProgramMatcher::Any],
                command: "catch_all".to_string(),
                args: vec![],
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xAA])],
                command: "exact".to_string(),
                args: vec![],
            },
        ]);

        // Exact match should win even though catch-all is listed first
        assert_eq!(router.find_handler(&[0xAA]), Some(1));
        assert_eq!(router.find_handler(&[0xBB]), Some(0));
    }

    #[test]
    fn test_handler_config_multiple_matchers() {
        let router = HandlerRouter::new(vec![HandlerConfig {
            matchers: vec![
                ProgramMatcher::Hash(vec![0xAA]),
                ProgramMatcher::Hash(vec![0xBB]),
            ],
            command: "multi".to_string(),
            args: vec![],
        }]);

        assert_eq!(router.find_handler(&[0xAA]), Some(0));
        assert_eq!(router.find_handler(&[0xBB]), Some(0));
        assert_eq!(router.find_handler(&[0xCC]), None);
    }
}
