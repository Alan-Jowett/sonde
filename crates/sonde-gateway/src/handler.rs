// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ciborium::Value;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const MAX_MESSAGE_SIZE: u32 = 1_048_576;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(30);
enum ReadOutcome {
    Reply(HandlerMessage),
    Eof,
    ReadError(String),
}

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
        delivery: u8,
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
            HandlerMessage::DataReply {
                request_id,
                data,
                delivery,
            } => {
                let mut pairs = vec![
                    (
                        Value::Integer(1.into()),
                        Value::Integer(MSG_TYPE_DATA_REPLY.into()),
                    ),
                    (
                        Value::Integer(2.into()),
                        Value::Integer((*request_id).into()),
                    ),
                    (Value::Integer(3.into()), Value::Bytes(data.clone())),
                ];
                if *delivery != 0 {
                    pairs.push((Value::Integer(4.into()), Value::Integer((*delivery).into())));
                }
                Value::Map(pairs)
            }
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
                let delivery = get_uint(map, 4).unwrap_or(0) as u8;
                Ok(HandlerMessage::DataReply {
                    request_id,
                    data,
                    delivery,
                })
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
    if payload.len() > MAX_MESSAGE_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let len = payload.len() as u32;
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

#[derive(Debug, Clone, PartialEq)]
pub enum ProgramMatcher {
    Any,
    Hash(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HandlerConfig {
    pub matchers: Vec<ProgramMatcher>,
    pub command: String,
    pub args: Vec<String>,
    /// Per-handler I/O timeout override for communication with the handler
    /// process (e.g. reading DATA replies and writing EVENT messages).
    /// Falls back to the default 30 s when `None`. Useful for tests that
    /// need a shorter timeout.
    pub reply_timeout: Option<Duration>,
    /// Optional working directory for the handler process.
    pub working_dir: Option<String>,
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
        if let Some(child) = &mut self.child {
            if let Some(status) = child.try_wait()? {
                // GW-1308 AC5: handler exited with code.
                let code = status.code().unwrap_or(-1);
                if status.success() {
                    info!(
                        command = %self.config.command,
                        code = code,
                        "handler exited"
                    );
                } else {
                    error!(
                        command = %self.config.command,
                        code = code,
                        "handler exited"
                    );
                }
                self.stdin = None;
                self.stdout_reader = None;
                self.child = None;
            }
        }

        if self.child.is_none() {
            let mut cmd = Command::new(&self.config.command);
            cmd.args(&self.config.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(ref dir) = self.config.working_dir {
                cmd.current_dir(dir);
            }
            let mut child = cmd.spawn()?;

            self.stdin = child.stdin.take();
            self.stdout_reader = child.stdout.take().map(BufReader::new);

            // Drain stderr in a background task so handler diagnostics
            // (e.g. Python tracebacks) appear in the gateway log instead
            // of being silently discarded.  Lines are capped at 4 KB to
            // prevent a misbehaving handler from causing unbounded memory
            // growth.
            if let Some(stderr) = child.stderr.take() {
                let cmd_label = self.config.command.clone();
                const MAX_STDERR_LINE: usize = 4096;
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stderr);
                    let mut buf = String::with_capacity(256);
                    loop {
                        buf.clear();
                        match reader.read_line(&mut buf).await {
                            Ok(0) => break, // EOF
                            Ok(n) if n > MAX_STDERR_LINE => {
                                buf.truncate(MAX_STDERR_LINE);
                                warn!(
                                    handler = %cmd_label,
                                    "{}… (truncated, {} bytes total)",
                                    buf.trim_end(),
                                    n,
                                );
                            }
                            Ok(_) => {
                                let line = buf.trim_end();
                                if !line.is_empty() {
                                    warn!(handler = %cmd_label, "{line}");
                                }
                            }
                            Err(e) => {
                                warn!(
                                    handler = %cmd_label,
                                    error = %e,
                                    "stderr read error",
                                );
                                break;
                            }
                        }
                    }
                });
            }

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
            error!(error = %e, command = %self.config.command, "handler process unavailable");
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
        let timeout = self.config.reply_timeout.unwrap_or(HANDLER_TIMEOUT);
        let result = tokio::time::timeout(timeout, async {
            loop {
                match read_message(reader).await {
                    Ok(HandlerMessage::Log { level, message }) => match level.as_str() {
                        "error" => error!(handler = %self.config.command, "{message}"),
                        "warn" => warn!(handler = %self.config.command, "{message}"),
                        "info" => info!(handler = %self.config.command, "{message}"),
                        "debug" => debug!(handler = %self.config.command, "{message}"),
                        _ => debug!(handler = %self.config.command, level = %level, "{message}"),
                    },
                    Ok(reply @ HandlerMessage::DataReply { .. }) => {
                        if let HandlerMessage::DataReply { request_id, .. } = &reply {
                            if *request_id != expected_request_id {
                                warn!(
                                    expected = expected_request_id,
                                    got = *request_id,
                                    "handler reply request_id mismatch — discarding, continuing"
                                );
                                continue;
                            }
                        }
                        return ReadOutcome::Reply(reply);
                    }
                    Ok(_) => {
                        warn!("unexpected message type from handler — ignoring");
                    }
                    Err(e) => {
                        return if e.kind() == io::ErrorKind::UnexpectedEof {
                            ReadOutcome::Eof
                        } else {
                            ReadOutcome::ReadError(e.to_string())
                        };
                    }
                }
            }
        })
        .await;

        match result {
            Ok(ReadOutcome::ReadError(msg)) => {
                error!(error = %msg, "error reading from handler stdout — killing child");
                self.kill_child().await;
                None
            }
            Ok(ReadOutcome::Reply(reply)) => Some(reply),
            Ok(ReadOutcome::Eof) => {
                self.check_exit_status().await;
                None
            }
            Err(_) => {
                error!(
                    command = %self.config.command,
                    timeout_secs = timeout.as_secs(),
                    "handler timed out — killing child"
                );
                self.kill_child().await;
                None
            }
        }
    }

    /// Send an EVENT message (fire-and-forget, no response expected).
    /// Uses a timeout to prevent handler I/O from blocking the caller.
    /// After writing, drains any pending stdout messages (LOG) to prevent
    /// the handler's stdout pipe from filling up.
    pub async fn send_event(&mut self, msg: &HandlerMessage) {
        if let Err(e) = self.ensure_running() {
            error!(error = %e, "failed to spawn handler for event");
            return;
        }

        let stdin = match self.stdin.as_mut() {
            Some(s) => s,
            None => return,
        };

        let timeout = self.config.reply_timeout.unwrap_or(HANDLER_TIMEOUT);
        match tokio::time::timeout(timeout, write_message(stdin, msg)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!(error = %e, "failed to write event to handler stdin");
                self.kill_child().await;
                return;
            }
            Err(_) => {
                error!(
                    command = %self.config.command,
                    "handler event write timed out — killing child"
                );
                self.kill_child().await;
                return;
            }
        }

        // Drain any pending stdout messages (e.g., LOG responses to the event)
        // with a short timeout to prevent blocking.
        self.drain_stdout().await;
    }

    /// Drain pending stdout messages (LOG) without blocking. Uses `fill_buf()`
    /// to peek for available data before committing to a full frame read,
    /// avoiding the risk of cancelling `read_message` mid-frame and
    /// desynchronizing the stream. Caps at 16 messages.
    async fn drain_stdout(&mut self) {
        let reader = match self.stdout_reader.as_mut() {
            Some(r) => r,
            None => return,
        };

        const MAX_DRAIN_MESSAGES: usize = 16;
        let peek_timeout = Duration::from_millis(50);

        for _ in 0..MAX_DRAIN_MESSAGES {
            // Peek: check if any bytes are buffered or available without
            // consuming them. If fill_buf times out, nothing is pending.
            let has_data = match tokio::time::timeout(
                peek_timeout,
                tokio::io::AsyncBufReadExt::fill_buf(reader),
            )
            .await
            {
                Ok(Ok(buf)) => !buf.is_empty(),
                _ => false,
            };

            if !has_data {
                break;
            }

            // Data is available — read a full frame. Wrap in a short timeout
            // in case the handler wrote a partial frame and stalled.
            match tokio::time::timeout(Duration::from_secs(2), read_message(reader)).await {
                Ok(Ok(HandlerMessage::Log { level, message })) => match level.as_str() {
                    "error" => error!(handler = %self.config.command, "{message}"),
                    "warn" => warn!(handler = %self.config.command, "{message}"),
                    "info" => info!(handler = %self.config.command, "{message}"),
                    "debug" => debug!(handler = %self.config.command, "{message}"),
                    _ => debug!(handler = %self.config.command, level = %level, "{message}"),
                },
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => {
                    // Read error or timeout mid-frame — stream is corrupt, kill child
                    self.kill_child().await;
                    break;
                }
            }
        }
    }

    async fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.stdin = None;
        self.stdout_reader = None;
    }

    /// Attempt a graceful shutdown: close stdin (signals EOF to the handler),
    /// wait up to `timeout` for the process to exit, then forcibly kill it.
    async fn graceful_shutdown(&mut self, timeout: Duration) {
        // Close stdin to signal the handler that no more input is coming.
        self.stdin = None;

        if let Some(ref mut child) = self.child {
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => {
                    let code = status.code().unwrap_or(-1);
                    if status.success() {
                        info!(
                            command = %self.config.command,
                            code = code,
                            "handler exited gracefully"
                        );
                    } else {
                        warn!(
                            command = %self.config.command,
                            code = code,
                            "handler exited with error during graceful shutdown"
                        );
                    }
                }
                Ok(Err(e)) => {
                    error!(
                        command = %self.config.command,
                        error = %e,
                        "failed to wait for handler during graceful shutdown"
                    );
                }
                Err(_) => {
                    warn!(
                        command = %self.config.command,
                        timeout_secs = timeout.as_secs(),
                        "handler did not exit within timeout — forcibly killing"
                    );
                    if let Some(mut child) = self.child.take() {
                        if let Err(e) = child.kill().await {
                            warn!(
                                command = %self.config.command,
                                error = %e,
                                "failed to forcibly kill handler"
                            );
                        }
                        // Bound the post-kill wait to 2 s to guarantee shutdown completes.
                        match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                error!(
                                    command = %self.config.command,
                                    error = %e,
                                    "failed to wait for handler after forced kill"
                                );
                            }
                            Err(_) => {
                                error!(
                                    command = %self.config.command,
                                    "handler did not exit after forced kill within 2 s"
                                );
                            }
                        }
                    }
                }
            }
        }
        self.child = None;
        self.stdout_reader = None;
    }

    async fn check_exit_status(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.wait().await {
                Ok(status) if status.success() => {
                    info!(
                        command = %self.config.command,
                        code = status.code().unwrap_or(0),
                        "handler exited"
                    );
                }
                Ok(status) => {
                    error!(
                        command = %self.config.command,
                        code = status.code().unwrap_or(-1),
                        "handler exited"
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

// --- Handler configuration file format ---

/// Raw YAML entry: `program_hash` may be a single string, a list of strings,
/// or the wildcard `"*"`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawProgramHash {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct RawHandlerEntry {
    program_hash: RawProgramHash,
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawHandlerConfigFile {
    handlers: Vec<RawHandlerEntry>,
}

/// Error returned when the handler config file cannot be loaded or parsed.
#[derive(Debug)]
pub struct HandlerConfigError(pub String);

impl std::fmt::Display for HandlerConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "handler config error: {}", self.0)
    }
}

impl std::error::Error for HandlerConfigError {}

/// Parse a hex string into bytes, returning an error on invalid input.
/// Returns an error if the string contains non-ASCII characters.
fn parse_hex(s: &str) -> Result<Vec<u8>, HandlerConfigError> {
    if !s.is_ascii() {
        return Err(HandlerConfigError(format!(
            "hex string contains non-ASCII characters: {s}"
        )));
    }
    if !s.len().is_multiple_of(2) {
        return Err(HandlerConfigError(format!(
            "hex string has odd length: {s}"
        )));
    }
    // s is ASCII, so every character is exactly 1 byte; byte-offset slicing is safe.
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| HandlerConfigError(format!("invalid hex character in: {s}")))
        })
        .collect()
}

/// Parse a single program_hash string into a `ProgramMatcher`.
/// `"*"` becomes `ProgramMatcher::Any`; anything else must be a 64-character
/// hex string encoding a 32-byte SHA-256 digest.
fn parse_program_matcher(s: &str) -> Result<ProgramMatcher, HandlerConfigError> {
    if s == "*" {
        Ok(ProgramMatcher::Any)
    } else {
        let bytes = parse_hex(s)?;
        if bytes.len() != 32 {
            return Err(HandlerConfigError(format!(
                "program_hash must be 64 hex characters (32 bytes), got {} bytes from: {s}",
                bytes.len()
            )));
        }
        Ok(ProgramMatcher::Hash(bytes))
    }
}

/// Load handler configurations from a YAML file.
///
/// File-level I/O errors and YAML parse errors are fatal (returned as
/// `Err`). Individual handler entries whose `program_hash` values fail
/// validation (e.g. non-hex characters, wrong length) are **skipped**
/// with a warning and do not abort the load. The returned `Vec` contains
/// only the successfully parsed entries.
///
/// The expected format is:
///
/// ```yaml
/// handlers:
///   - program_hash: "a1b2c3..."
///     command: "/usr/local/bin/my-app"
///   - program_hash: ["7a8b9c...", "0d1e2f..."]
///     command: "/usr/local/bin/multi-sensor-app"
///   - program_hash: "*"
///     command: "/usr/local/bin/default-handler"
/// ```
pub fn load_handler_configs(path: &Path) -> Result<Vec<HandlerConfig>, HandlerConfigError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| HandlerConfigError(format!("failed to read {}: {e}", path.display())))?;
    let raw: RawHandlerConfigFile = serde_yaml_ng::from_str(&content)
        .map_err(|e| HandlerConfigError(format!("failed to parse {}: {e}", path.display())))?;

    let mut configs = Vec::new();
    for entry in raw.handlers {
        let matchers_result = match entry.program_hash {
            RawProgramHash::Single(s) => parse_program_matcher(&s).map(|m| vec![m]),
            RawProgramHash::Multiple(hashes) => hashes
                .into_iter()
                .map(|h| parse_program_matcher(&h))
                .collect::<Result<Vec<_>, _>>(),
        };
        match matchers_result {
            Ok(matchers) => {
                configs.push(HandlerConfig {
                    matchers,
                    command: entry.command,
                    args: entry.args,
                    reply_timeout: None,
                    working_dir: None,
                });
            }
            Err(e) => {
                warn!(path = %path.display(), command = %entry.command, "skipping invalid handler entry: {e}");
            }
        }
    }
    Ok(configs)
}

// --- HandlerRouter ---

pub struct HandlerRouter {
    handlers: Vec<(HandlerConfig, Arc<Mutex<HandlerProcess>>)>,
}

impl HandlerRouter {
    pub fn new(configs: Vec<HandlerConfig>) -> Self {
        let handlers = configs
            .into_iter()
            .map(|c| {
                let process = HandlerProcess::new(c.clone());
                (c, Arc::new(Mutex::new(process)))
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

    /// Find the handler matching `program_hash` and return cloned references.
    ///
    /// The returned `Arc<Mutex<HandlerProcess>>` can be used after releasing
    /// the `RwLock` read guard, avoiding lock contention during handler I/O.
    pub fn find_handler_cloned(
        &self,
        program_hash: &[u8],
    ) -> Option<(HandlerConfig, Arc<Mutex<HandlerProcess>>)> {
        let idx = self.find_handler(program_hash)?;
        let (config, process) = &self.handlers[idx];
        Some((config.clone(), Arc::clone(process)))
    }

    /// Return the number of configured handlers (for diagnostics).
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Clone all handler process references for event broadcasting.
    ///
    /// The returned `Arc`s can be used after releasing the `RwLock` read
    /// guard, avoiding lock contention during handler I/O.
    pub fn clone_all_process_refs(&self) -> Vec<Arc<Mutex<HandlerProcess>>> {
        self.handlers.iter().map(|(_, p)| Arc::clone(p)).collect()
    }

    /// Replace the handler set with a new configuration (GW-1404).
    ///
    /// Diffs the old and new config sets:
    /// - **Added** handlers are inserted (process spawned lazily on first message).
    /// - **Removed** handlers are removed from routing when `self.handlers` is
    ///   replaced, then returned for the caller to shut down *after* releasing
    ///   the write lock to avoid prolonged lock contention.
    /// - **Unchanged** handlers (same config) retain their existing `HandlerProcess`.
    ///
    /// This method updates routing immediately; removed handlers stop
    /// receiving new requests as soon as `reload` returns, and their
    /// processes are terminated afterwards by the caller via
    /// [`shutdown_removed_handlers`].
    pub fn reload(
        &mut self,
        new_configs: Vec<HandlerConfig>,
    ) -> Vec<(HandlerConfig, Arc<Mutex<HandlerProcess>>)> {
        // Build the new handler list, reusing existing processes where configs match.
        let mut old_handlers: Vec<Option<(HandlerConfig, Arc<Mutex<HandlerProcess>>)>> =
            self.handlers.drain(..).map(Some).collect();
        let mut new_handlers = Vec::with_capacity(new_configs.len());

        for new_cfg in new_configs {
            // Look for a matching existing handler to reuse.
            let reused = old_handlers.iter_mut().position(|slot| {
                if let Some((old_cfg, _)) = slot.as_ref() {
                    old_cfg == &new_cfg
                } else {
                    false
                }
            });

            if let Some(idx) = reused {
                // Unchanged handler — retain existing process.
                new_handlers.push(old_handlers[idx].take().unwrap());
            } else {
                // Added handler — create new process (spawned lazily).
                let process = HandlerProcess::new(new_cfg.clone());
                new_handlers.push((new_cfg, Arc::new(Mutex::new(process))));
            }
        }

        self.handlers = new_handlers;

        // Return removed handlers for the caller to shut down outside the lock.
        old_handlers.into_iter().flatten().collect()
    }
}

/// Gracefully shut down a set of removed handler processes (GW-1404 AC3).
///
/// Call this *after* releasing the `HandlerRouter` write lock so that the
/// shutdown timeout (5 s per handler) does not block APP_DATA routing.
pub async fn shutdown_removed_handlers(removed: Vec<(HandlerConfig, Arc<Mutex<HandlerProcess>>)>) {
    const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(5);
    for (cfg, process_arc) in removed {
        info!(command = %cfg.command, "removing handler — initiating graceful shutdown");
        let mut process = process_arc.lock().await;
        process.graceful_shutdown(GRACEFUL_TIMEOUT).await;
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
            delivery: 0,
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
            delivery: 0,
        };

        let (mut writer, mut reader) = tokio::io::duplex(4096);
        write_message(&mut writer, &msg).await.unwrap();
        drop(writer);

        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_handler_router_find_exact_match() {
        let router = HandlerRouter::new(vec![
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xAA])],
                command: "handler_a".to_string(),
                args: vec![],
                reply_timeout: None,
                working_dir: None,
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xBB])],
                command: "handler_b".to_string(),
                args: vec![],
                reply_timeout: None,
                working_dir: None,
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
                reply_timeout: None,
                working_dir: None,
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Any],
                command: "catch_all".to_string(),
                args: vec![],
                reply_timeout: None,
                working_dir: None,
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
                reply_timeout: None,
                working_dir: None,
            },
            HandlerConfig {
                matchers: vec![ProgramMatcher::Hash(vec![0xAA])],
                command: "exact".to_string(),
                args: vec![],
                reply_timeout: None,
                working_dir: None,
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
            reply_timeout: None,
            working_dir: None,
        }]);

        assert_eq!(router.find_handler(&[0xAA]), Some(0));
        assert_eq!(router.find_handler(&[0xBB]), Some(0));
        assert_eq!(router.find_handler(&[0xCC]), None);
    }

    // --- load_handler_configs tests ---

    // 64-character hex strings representing valid 32-byte SHA-256 hashes.
    const HASH_A: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
    const HASH_B: &str = "ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122ccdd1122";
    const HASH_C: &str = "7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b";
    const HASH_D: &str = "0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e2f7a8b9c0d1e";

    #[test]
    fn test_load_handler_configs_single_hash() {
        let yaml = format!(
            r#"
handlers:
  - program_hash: "{HASH_A}"
    command: "/usr/bin/handler"
"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        let configs = load_handler_configs(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].command, "/usr/bin/handler");
        assert!(configs[0].args.is_empty());
        assert_eq!(configs[0].matchers.len(), 1);
        match &configs[0].matchers[0] {
            ProgramMatcher::Hash(h) => {
                assert_eq!(h.len(), 32);
                assert_eq!(h[0], 0xa1);
                assert_eq!(h[1], 0xb2);
            }
            _ => panic!("expected Hash matcher"),
        }
    }

    #[test]
    fn test_load_handler_configs_catch_all() {
        let yaml = r#"
handlers:
  - program_hash: "*"
    command: "/usr/bin/default"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        let configs = load_handler_configs(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(matches!(configs[0].matchers[0], ProgramMatcher::Any));
    }

    #[test]
    fn test_load_handler_configs_multiple_hashes() {
        let yaml = format!(
            r#"
handlers:
  - program_hash: ["{HASH_A}", "{HASH_B}"]
    command: "/usr/bin/multi"
"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        let configs = load_handler_configs(&path).unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].matchers.len(), 2);
        match &configs[0].matchers[0] {
            ProgramMatcher::Hash(h) => {
                assert_eq!(h.len(), 32);
                assert_eq!(h[0], 0xa1);
            }
            _ => panic!("expected Hash matcher"),
        }
        match &configs[0].matchers[1] {
            ProgramMatcher::Hash(h) => {
                assert_eq!(h.len(), 32);
                assert_eq!(h[0], 0xcc);
            }
            _ => panic!("expected Hash matcher"),
        }
    }

    #[test]
    fn test_load_handler_configs_with_args() {
        let yaml = format!(
            r#"
handlers:
  - program_hash: "{HASH_A}"
    command: "/usr/bin/handler"
    args: ["--verbose", "--output=/tmp/out"]
"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        let configs = load_handler_configs(&path).unwrap();
        assert_eq!(configs[0].args, vec!["--verbose", "--output=/tmp/out"]);
    }

    #[test]
    fn test_load_handler_configs_multiple_handlers() {
        let yaml = format!(
            r#"
handlers:
  - program_hash: "{HASH_A}"
    command: "/usr/local/bin/soil-moisture-app"
  - program_hash: "{HASH_B}"
    command: "/usr/local/bin/temperature-alert-app"
  - program_hash: ["{HASH_C}", "{HASH_D}"]
    command: "/usr/local/bin/multi-sensor-app"
  - program_hash: "*"
    command: "/usr/local/bin/default-handler"
"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        let configs = load_handler_configs(&path).unwrap();
        assert_eq!(configs.len(), 4);
        assert_eq!(configs[0].command, "/usr/local/bin/soil-moisture-app");
        assert_eq!(configs[1].command, "/usr/local/bin/temperature-alert-app");
        assert_eq!(configs[2].command, "/usr/local/bin/multi-sensor-app");
        assert_eq!(configs[2].matchers.len(), 2);
        assert_eq!(configs[3].command, "/usr/local/bin/default-handler");
        assert!(matches!(configs[3].matchers[0], ProgramMatcher::Any));
    }

    #[test]
    fn test_load_handler_configs_invalid_hex() {
        let yaml = r#"
handlers:
  - program_hash: "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
    command: "/usr/bin/handler"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        // Lenient loader skips invalid entries (GW-1405)
        let result = load_handler_configs(&path);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_empty(),
            "invalid entry should be skipped"
        );
    }

    #[test]
    fn test_load_handler_configs_wrong_length_hash() {
        let yaml = r#"
handlers:
  - program_hash: "aabb"
    command: "/usr/bin/handler"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        // Lenient loader skips invalid entries (GW-1405)
        let result = load_handler_configs(&path);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_empty(),
            "wrong-length entry should be skipped"
        );
    }

    #[test]
    fn test_load_handler_configs_non_ascii_hash() {
        let yaml = "handlers:\n  - program_hash: \"é1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\"\n    command: \"/usr/bin/handler\"\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handlers.yaml");
        std::fs::write(&path, yaml).unwrap();

        // Lenient loader skips invalid entries (GW-1405)
        let result = load_handler_configs(&path);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_empty(),
            "non-ASCII entry should be skipped"
        );
    }

    #[test]
    fn test_load_handler_configs_file_not_found() {
        let result = load_handler_configs(std::path::Path::new("/nonexistent/path.yaml"));
        assert!(result.is_err());
    }
}
