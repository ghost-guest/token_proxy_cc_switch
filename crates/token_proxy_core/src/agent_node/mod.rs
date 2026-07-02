use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use portable_pty::{
    native_pty_system, Child as PtyChild, CommandBuilder as PtyCommandBuilder, MasterPty, PtySize,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const NODE_CONNECT_PATH: &str = "/api/node/connect";
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_STABLE_RESET_AFTER: Duration = Duration::from_secs(60);
const TERMINAL_SCROLLBACK_LIMIT_BYTES: usize = 1024 * 1024;
const FILE_CONTENT_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct AgentNodeConfig {
    pub server_url: String,
    pub api_key: String,
    pub hostname: Option<String>,
    pub os: String,
    pub arch: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeCapabilities {
    pub runtime: bool,
    pub fs: bool,
    pub terminal: bool,
}

impl Default for NodeCapabilities {
    fn default() -> Self {
        Self {
            runtime: true,
            fs: true,
            terminal: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum AgentNodeOutgoing {
    #[serde(rename = "node.hello")]
    Hello {
        #[serde(skip_serializing_if = "Option::is_none")]
        hostname: Option<String>,
        os: String,
        arch: String,
        version: String,
        capabilities: NodeCapabilities,
    },
    #[serde(rename = "node.heartbeat")]
    Heartbeat {
        status: NodeStatus,
        capabilities: NodeCapabilities,
    },
    #[serde(rename = "response")]
    Response {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "requestId")]
        request_id: String,
        code: String,
        message: String,
    },
    #[serde(rename = "runtime.message")]
    RuntimeMessage {
        #[serde(rename = "channelId")]
        channel_id: String,
        payload: Value,
    },
    #[serde(rename = "terminal.output")]
    TerminalOutput {
        #[serde(rename = "channelId")]
        channel_id: String,
        payload: Value,
    },
    #[serde(rename = "terminal.exit")]
    TerminalExit {
        #[serde(rename = "channelId")]
        channel_id: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Online,
    Degraded,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum AgentNodeIncoming {
    #[serde(rename = "node.connected")]
    NodeConnected {
        #[serde(rename = "nodeId")]
        node_id: String,
    },
    #[serde(rename = "node.heartbeat.ack")]
    HeartbeatAck { at: String },
    #[serde(rename = "runtime.open")]
    RuntimeOpen {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        #[serde(default)]
        payload: Option<Value>,
    },
    #[serde(rename = "runtime.send")]
    RuntimeSend {
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        payload: Value,
    },
    #[serde(rename = "runtime.close")]
    RuntimeClose {
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
    },
    #[serde(rename = "fs.list")]
    FsList {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        payload: Value,
    },
    #[serde(rename = "fs.read")]
    FsRead {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        payload: Value,
    },
    #[serde(rename = "fs.write")]
    FsWrite {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        payload: Value,
    },
    #[serde(rename = "terminal.open")]
    TerminalOpen {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        #[serde(rename = "projectId")]
        project_id: Option<String>,
        #[serde(default)]
        payload: Option<Value>,
    },
    #[serde(rename = "terminal.input")]
    TerminalInput {
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        payload: Option<Value>,
    },
    #[serde(rename = "terminal.resize")]
    TerminalResize {
        #[serde(rename = "channelId")]
        channel_id: String,
        #[serde(default)]
        payload: Option<Value>,
    },
    #[serde(rename = "terminal.snapshot")]
    TerminalSnapshot {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "channelId")]
        channel_id: String,
    },
    #[serde(rename = "terminal.close")]
    TerminalClose {
        #[serde(rename = "channelId")]
        channel_id: String,
    },
    #[serde(other)]
    Unsupported,
}

pub fn node_connect_url(server_url: &str) -> Result<String, String> {
    let mut url =
        Url::parse(server_url).map_err(|err| format!("Invalid Agent Console URL: {err}"))?;
    match url.scheme() {
        "http" => {
            url.set_scheme("ws")
                .map_err(|_| "Failed to convert http URL to ws URL".to_string())?;
            url.set_path(NODE_CONNECT_PATH);
        }
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| "Failed to convert https URL to wss URL".to_string())?;
            url.set_path(NODE_CONNECT_PATH);
        }
        "ws" | "wss" => {
            if url.path() == "/" || url.path().is_empty() {
                url.set_path(NODE_CONNECT_PATH);
            }
        }
        other => return Err(format!("Unsupported Agent Console URL scheme: {other}")),
    }
    url.set_query(None);
    Ok(url.to_string())
}

pub struct AgentNodeClient {
    config: AgentNodeConfig,
    runtime: RuntimeChannelManager,
    terminals: TerminalSessionManager,
    heartbeat_interval: Duration,
    reconnect_policy: AgentNodeReconnectPolicy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentNodeReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub stable_reset_after: Duration,
}

impl Default for AgentNodeReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: DEFAULT_RECONNECT_INITIAL_DELAY,
            max_delay: DEFAULT_RECONNECT_MAX_DELAY,
            stable_reset_after: DEFAULT_RECONNECT_STABLE_RESET_AFTER,
        }
    }
}

impl AgentNodeClient {
    pub fn new(config: AgentNodeConfig) -> Self {
        Self {
            config,
            runtime: RuntimeChannelManager::new(CodexRuntimeLauncher::default()),
            terminals: TerminalSessionManager::new(),
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            reconnect_policy: AgentNodeReconnectPolicy::default(),
        }
    }

    pub fn with_reconnect_policy(mut self, policy: AgentNodeReconnectPolicy) -> Self {
        self.reconnect_policy = policy;
        self
    }

    pub async fn run_with_reconnect(&mut self) -> Result<(), String> {
        node_connect_url(&self.config.server_url)?;
        let mut delay = self.reconnect_policy.initial_delay;
        loop {
            let started_at = Instant::now();
            match self.run().await {
                Ok(()) => {
                    tracing::info!(
                        server_url = %self.config.server_url,
                        "agent node websocket disconnected; reconnecting"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        server_url = %self.config.server_url,
                        error = %err,
                        "agent node websocket failed; reconnecting"
                    );
                }
            }
            if started_at.elapsed() >= self.reconnect_policy.stable_reset_after {
                delay = self.reconnect_policy.initial_delay;
            }
            let sleep_for = delay;
            delay = next_reconnect_delay(delay, &self.reconnect_policy);
            tokio::time::sleep(sleep_for).await;
        }
    }

    pub async fn run(&mut self) -> Result<(), String> {
        let connect_url = node_connect_url(&self.config.server_url)?;
        let mut request = connect_url
            .into_client_request()
            .map_err(|err| format!("Invalid node websocket request: {err}"))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.config.api_key)
                .parse()
                .map_err(|err| format!("Invalid node API key header: {err}"))?,
        );

        let (socket, _) = connect_async(request)
            .await
            .map_err(|err| format!("Failed to connect Agent Console node websocket: {err}"))?;
        let (mut sink, mut stream) = socket.split();
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<AgentNodeOutgoing>(128);

        let hello = AgentNodeOutgoing::Hello {
            hostname: self.config.hostname.clone(),
            os: self.config.os.clone(),
            arch: self.config.arch.clone(),
            version: self.config.version.clone(),
            capabilities: NodeCapabilities::default(),
        };
        send_ws_json(&mut sink, &hello).await?;

        let mut heartbeat = tokio::time::interval(self.heartbeat_interval);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    send_ws_json(&mut sink, &AgentNodeOutgoing::Heartbeat {
                        status: NodeStatus::Online,
                        capabilities: NodeCapabilities::default(),
                    }).await?;
                }
                Some(message) = outbound_rx.recv() => {
                    send_ws_json(&mut sink, &message).await?;
                }
                Some(incoming) = stream.next() => {
                    let incoming = incoming.map_err(|err| format!("Node websocket error: {err}"))?;
                    if incoming.is_close() {
                        self.runtime.close_all().await;
                        self.terminals.close_all();
                        return Ok(());
                    }
                    if !incoming.is_text() {
                        continue;
                    }
                    let text = incoming.into_text().map_err(|err| format!("Invalid node websocket text: {err}"))?;
                    let parsed = serde_json::from_str::<AgentNodeIncoming>(&text)
                        .map_err(|err| format!("Invalid Agent Console node message: {err}"))?;
                    self.handle_message(parsed, outbound_tx.clone()).await?;
                }
                else => {
                    self.runtime.close_all().await;
                    self.terminals.close_all();
                    return Ok(());
                }
            }
        }
    }

    async fn handle_message(
        &mut self,
        message: AgentNodeIncoming,
        outbound_tx: mpsc::Sender<AgentNodeOutgoing>,
    ) -> Result<(), String> {
        match message {
            AgentNodeIncoming::RuntimeOpen {
                request_id,
                channel_id,
                payload,
                ..
            } => {
                let cwd = runtime_open_cwd(payload.as_ref())?;
                match self
                    .runtime
                    .open(channel_id.clone(), cwd, outbound_tx.clone())
                    .await
                {
                    Ok(()) => {
                        send_outbound(
                            &outbound_tx,
                            AgentNodeOutgoing::Response {
                                request_id,
                                payload: Some(serde_json::json!({ "ready": true })),
                            },
                        )
                        .await?;
                    }
                    Err(err) => {
                        send_outbound(
                            &outbound_tx,
                            AgentNodeOutgoing::Error {
                                request_id,
                                code: "NODE_RUNTIME_UNAVAILABLE".to_string(),
                                message: err,
                            },
                        )
                        .await?;
                    }
                }
            }
            AgentNodeIncoming::RuntimeSend {
                channel_id,
                payload,
                ..
            } => {
                self.runtime.send(&channel_id, payload).await?;
            }
            AgentNodeIncoming::RuntimeClose { channel_id, .. } => {
                self.runtime.close(&channel_id).await;
            }
            AgentNodeIncoming::FsList {
                request_id,
                payload,
                ..
            } => match list_directory(payload).await {
                Ok(payload) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Response {
                            request_id,
                            payload: Some(payload),
                        },
                    )
                    .await?;
                }
                Err(err) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Error {
                            request_id,
                            code: "NODE_FS_UNAVAILABLE".to_string(),
                            message: err,
                        },
                    )
                    .await?;
                }
            },
            AgentNodeIncoming::FsRead {
                request_id,
                payload,
                ..
            } => match read_file(payload).await {
                Ok(payload) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Response {
                            request_id,
                            payload: Some(payload),
                        },
                    )
                    .await?;
                }
                Err(err) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Error {
                            request_id,
                            code: "NODE_FS_UNAVAILABLE".to_string(),
                            message: err,
                        },
                    )
                    .await?;
                }
            },
            AgentNodeIncoming::FsWrite {
                request_id,
                payload,
                ..
            } => match write_file(payload).await {
                Ok(payload) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Response {
                            request_id,
                            payload: Some(payload),
                        },
                    )
                    .await?;
                }
                Err(err) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Error {
                            request_id,
                            code: "NODE_FS_UNAVAILABLE".to_string(),
                            message: err,
                        },
                    )
                    .await?;
                }
            },
            AgentNodeIncoming::TerminalOpen {
                request_id,
                channel_id,
                payload,
                ..
            } => match self
                .terminals
                .open(channel_id.clone(), payload, outbound_tx.clone())
                .await
            {
                Ok(snapshot) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Response {
                            request_id,
                            payload: Some(serde_json::json!({
                                "ready": true,
                                "snapshot": snapshot,
                            })),
                        },
                    )
                    .await?;
                }
                Err(err) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Error {
                            request_id,
                            code: "NODE_TERMINAL_UNAVAILABLE".to_string(),
                            message: err,
                        },
                    )
                    .await?;
                }
            },
            AgentNodeIncoming::TerminalInput {
                channel_id,
                payload,
            } => {
                self.terminals.input(&channel_id, payload)?;
            }
            AgentNodeIncoming::TerminalResize {
                channel_id,
                payload,
            } => {
                self.terminals.resize(&channel_id, payload)?;
            }
            AgentNodeIncoming::TerminalSnapshot {
                request_id,
                channel_id,
            } => match self.terminals.snapshot(&channel_id) {
                Ok(snapshot) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Response {
                            request_id,
                            payload: Some(snapshot),
                        },
                    )
                    .await?;
                }
                Err(err) => {
                    send_outbound(
                        &outbound_tx,
                        AgentNodeOutgoing::Error {
                            request_id,
                            code: "NODE_TERMINAL_UNAVAILABLE".to_string(),
                            message: err,
                        },
                    )
                    .await?;
                }
            },
            AgentNodeIncoming::TerminalClose { channel_id } => {
                self.terminals.close(&channel_id);
            }
            AgentNodeIncoming::NodeConnected { .. }
            | AgentNodeIncoming::HeartbeatAck { .. }
            | AgentNodeIncoming::Unsupported => {}
        }
        Ok(())
    }
}

fn next_reconnect_delay(current: Duration, policy: &AgentNodeReconnectPolicy) -> Duration {
    let doubled = current.saturating_mul(2);
    doubled.min(policy.max_delay).max(policy.initial_delay)
}

async fn send_outbound(
    outbound_tx: &mpsc::Sender<AgentNodeOutgoing>,
    message: AgentNodeOutgoing,
) -> Result<(), String> {
    outbound_tx
        .send(message)
        .await
        .map_err(|_| "Node websocket outbound queue closed".to_string())
}

async fn send_ws_json<S>(sink: &mut S, payload: &AgentNodeOutgoing) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let text = serde_json::to_string(payload)
        .map_err(|err| format!("Failed to serialize node message: {err}"))?;
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|err| format!("Failed to send node websocket message: {err}"))
}

fn runtime_open_cwd(payload: Option<&Value>) -> Result<PathBuf, String> {
    let cwd = payload
        .and_then(|value| value.get("cwd"))
        .and_then(Value::as_str)
        .ok_or_else(|| "runtime.open payload.cwd is required".to_string())?;
    if cwd.trim().is_empty() {
        return Err("runtime.open payload.cwd cannot be empty".to_string());
    }
    Ok(PathBuf::from(cwd))
}

async fn list_directory(payload: Value) -> Result<Value, String> {
    let requested_path = payload
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("/");
    let root_path = payload.get("rootPath").and_then(Value::as_str);

    let (root, target, virtual_path) = if let Some(root_path) = root_path {
        let root = canonicalize_existing_dir(Path::new(root_path)).await?;
        let relative = requested_path.trim_start_matches('/');
        let target = if requested_path == "/" || relative.is_empty() {
            root.clone()
        } else {
            canonicalize_existing_dir(&root.join(relative)).await?
        };
        if !target.starts_with(&root) {
            return Err("Requested path escapes project root".to_string());
        }
        let virtual_path = if target == root {
            "/".to_string()
        } else {
            let relative = target
                .strip_prefix(&root)
                .map_err(|err| format!("Failed to strip project root: {err}"))?;
            format!("/{}", path_to_slash_string(relative))
        };
        (Some(root), target, virtual_path)
    } else {
        let target = canonicalize_existing_dir(Path::new(requested_path)).await?;
        let path = path_to_slash_string(&target);
        (None, target, path)
    };

    let mut children = Vec::new();
    let mut entries = tokio::fs::read_dir(&target)
        .await
        .map_err(|err| format!("Failed to read directory: {err}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| format!("Failed to read directory entry: {err}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|err| format!("Failed to read directory entry type: {err}"))?;
        let kind = if file_type.is_dir() {
            "directory"
        } else if file_type.is_file() {
            "file"
        } else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let child_path = if let Some(root) = &root {
            let child = entry.path();
            let child = child
                .strip_prefix(root)
                .map_err(|err| format!("Failed to strip child path root: {err}"))?;
            format!("/{}", path_to_slash_string(child))
        } else {
            path_to_slash_string(&entry.path())
        };
        children.push(serde_json::json!({
            "kind": kind,
            "name": name,
            "path": child_path,
        }));
    }
    children.sort_by(|left, right| {
        let left_kind = left.get("kind").and_then(Value::as_str).unwrap_or_default();
        let right_kind = right
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if left_kind != right_kind {
            return if left_kind == "directory" {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let left_name = left.get("name").and_then(Value::as_str).unwrap_or_default();
        let right_name = right
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_name.cmp(right_name)
    });

    let name = if virtual_path == "/" {
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("/")
            .to_string()
    } else {
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(&virtual_path)
            .to_string()
    };
    let parent = parent_path(&virtual_path);
    Ok(serde_json::json!({
        "name": name,
        "path": virtual_path,
        "parent": parent,
        "children": children,
    }))
}

async fn read_file(payload: Value) -> Result<Value, String> {
    let root_path = payload
        .get("rootPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "fs.read payload.rootPath is required".to_string())?;
    let requested_path = payload
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "fs.read payload.path is required".to_string())?;
    let root = canonicalize_existing_dir(Path::new(root_path)).await?;
    let target =
        canonicalize_existing_file(&root.join(requested_path.trim_start_matches('/'))).await?;
    if !target.starts_with(&root) {
        return Err("Requested file escapes project root".to_string());
    }
    let metadata = tokio::fs::metadata(&target)
        .await
        .map_err(|err| format!("Failed to read file metadata: {err}"))?;
    if metadata.len() > FILE_CONTENT_LIMIT_BYTES as u64 {
        return Err("File is too large to read".to_string());
    }
    let bytes = tokio::fs::read(&target)
        .await
        .map_err(|err| format!("Failed to read file: {err}"))?;
    let content = String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8".to_string())?;
    Ok(serde_json::json!({
        "path": project_virtual_path(&root, &target)?,
        "content": content,
        "encoding": "utf8",
    }))
}

async fn write_file(payload: Value) -> Result<Value, String> {
    let root_path = payload
        .get("rootPath")
        .and_then(Value::as_str)
        .ok_or_else(|| "fs.write payload.rootPath is required".to_string())?;
    let requested_path = payload
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "fs.write payload.path is required".to_string())?;
    let encoding = payload
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("utf8");
    if encoding != "utf8" {
        return Err("fs.write only supports utf8 encoding".to_string());
    }
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "fs.write payload.content is required".to_string())?;
    if content.len() > FILE_CONTENT_LIMIT_BYTES {
        return Err("File content is too large to write".to_string());
    }

    let root = canonicalize_existing_dir(Path::new(root_path)).await?;
    let target = resolve_writable_project_file(&root, requested_path).await?;
    tokio::fs::write(&target, content.as_bytes())
        .await
        .map_err(|err| format!("Failed to write file: {err}"))?;
    Ok(serde_json::json!({
        "path": project_virtual_path(&root, &target)?,
        "bytesWritten": content.len(),
    }))
}

async fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf, String> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|err| format!("Directory does not exist: {err}"))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|err| format!("Failed to read directory metadata: {err}"))?;
    if !metadata.is_dir() {
        return Err("Path is not a directory".to_string());
    }
    Ok(canonical)
}

async fn canonicalize_existing_file(path: &Path) -> Result<PathBuf, String> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|err| format!("File does not exist: {err}"))?;
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|err| format!("Failed to read file metadata: {err}"))?;
    if !metadata.is_file() {
        return Err("Path is not a file".to_string());
    }
    Ok(canonical)
}

async fn resolve_writable_project_file(
    root: &Path,
    requested_path: &str,
) -> Result<PathBuf, String> {
    let relative = requested_path.trim_start_matches('/');
    if relative.is_empty() {
        return Err("fs.write path cannot point to project root".to_string());
    }
    let target = root.join(relative);
    match tokio::fs::canonicalize(&target).await {
        Ok(canonical) => {
            let metadata = tokio::fs::metadata(&canonical)
                .await
                .map_err(|err| format!("Failed to read file metadata: {err}"))?;
            if !metadata.is_file() {
                return Err("Path is not a file".to_string());
            }
            if !canonical.starts_with(root) {
                return Err("Requested file escapes project root".to_string());
            }
            Ok(canonical)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent = target
                .parent()
                .ok_or_else(|| "fs.write path parent is missing".to_string())?;
            let parent = canonicalize_existing_dir(parent).await?;
            if !parent.starts_with(root) {
                return Err("Requested file escapes project root".to_string());
            }
            let file_name = target
                .file_name()
                .ok_or_else(|| "fs.write path file name is missing".to_string())?;
            Ok(parent.join(file_name))
        }
        Err(err) => Err(format!("Failed to resolve writable file path: {err}")),
    }
}

fn project_virtual_path(root: &Path, target: &Path) -> Result<String, String> {
    let relative = target
        .strip_prefix(root)
        .map_err(|err| format!("Failed to strip project root: {err}"))?;
    Ok(format!("/{}", path_to_slash_string(relative)))
}

fn path_to_slash_string(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        ".".to_string()
    } else {
        text
    }
}

fn parent_path(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    let Some(index) = trimmed.rfind('/') else {
        return Some("/".to_string());
    };
    if index == 0 {
        Some("/".to_string())
    } else {
        Some(trimmed[..index].to_string())
    }
}

#[derive(Default)]
pub struct CodexRuntimeLauncher {
    codex_bin: String,
}

impl CodexRuntimeLauncher {
    fn command(&self) -> Command {
        let bin = if self.codex_bin.trim().is_empty() {
            "codex"
        } else {
            self.codex_bin.as_str()
        };
        let mut command = Command::new(bin);
        command.args(["app-server", "--listen", "stdio://"]);
        command
    }
}

pub struct RuntimeChannelManager {
    launcher: CodexRuntimeLauncher,
    channels: HashMap<String, RuntimeChannel>,
}

impl RuntimeChannelManager {
    pub fn new(launcher: CodexRuntimeLauncher) -> Self {
        Self {
            launcher,
            channels: HashMap::new(),
        }
    }

    pub async fn open(
        &mut self,
        channel_id: String,
        cwd: PathBuf,
        outbound_tx: mpsc::Sender<AgentNodeOutgoing>,
    ) -> Result<(), String> {
        if self.channels.contains_key(&channel_id) {
            return Err(format!("Runtime channel already exists: {channel_id}"));
        }
        let channel = RuntimeChannel::spawn(&self.launcher, &channel_id, &cwd, outbound_tx).await?;
        self.channels.insert(channel_id, channel);
        Ok(())
    }

    pub async fn send(&mut self, channel_id: &str, payload: Value) -> Result<(), String> {
        let channel = self
            .channels
            .get(channel_id)
            .ok_or_else(|| format!("Runtime channel not found: {channel_id}"))?;
        channel.send(payload).await
    }

    pub async fn close(&mut self, channel_id: &str) {
        if let Some(channel) = self.channels.remove(channel_id) {
            channel.close().await;
        }
    }

    pub async fn close_all(&mut self) {
        let channels = std::mem::take(&mut self.channels);
        for (_, channel) in channels {
            channel.close().await;
        }
    }
}

struct RuntimeChannel {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
}

impl RuntimeChannel {
    async fn spawn(
        launcher: &CodexRuntimeLauncher,
        channel_id: &str,
        cwd: &Path,
        outbound_tx: mpsc::Sender<AgentNodeOutgoing>,
    ) -> Result<Self, String> {
        let mut command = launcher.command();
        command.current_dir(cwd);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|err| format!("Failed to spawn Codex app-server stdio: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Codex app-server stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Codex app-server stdout unavailable".to_string())?;
        let child = Arc::new(Mutex::new(child));
        let channel_id_for_task = channel_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(payload) => {
                                let _ = outbound_tx
                                    .send(AgentNodeOutgoing::RuntimeMessage {
                                        channel_id: channel_id_for_task.clone(),
                                        payload,
                                    })
                                    .await;
                            }
                            Err(err) => {
                                let _ = outbound_tx
                                .send(AgentNodeOutgoing::RuntimeMessage {
                                    channel_id: channel_id_for_task.clone(),
                                    payload: serde_json::json!({
                                            "method": "error",
                                            "params": {
                                                "message": format!("Invalid Codex app-server JSONL: {err}")
                                            }
                                        }),
                                    })
                                    .await;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            child,
        })
    }

    async fn send(&self, payload: Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_vec(&payload)
            .map_err(|err| format!("Failed to serialize Codex JSON-RPC frame: {err}"))?;
        line.push(b'\n');
        stdin
            .write_all(&line)
            .await
            .map_err(|err| format!("Failed to write Codex app-server stdin: {err}"))?;
        stdin
            .flush()
            .await
            .map_err(|err| format!("Failed to flush Codex app-server stdin: {err}"))
    }

    async fn close(self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

pub struct TerminalSessionManager {
    sessions: HashMap<String, TerminalSession>,
}

impl TerminalSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    pub async fn open(
        &mut self,
        channel_id: String,
        payload: Option<Value>,
        outbound_tx: mpsc::Sender<AgentNodeOutgoing>,
    ) -> Result<Value, String> {
        if let Some(session) = self.sessions.get(&channel_id) {
            if let Some(payload) = payload.as_ref() {
                session.resize(terminal_size_from_payload(Some(payload))?)?;
            }
            return session.snapshot();
        }
        let payload = payload.unwrap_or(Value::Null);
        let cwd = terminal_open_cwd(&payload).await?;
        let size = terminal_size_from_payload(Some(&payload))?;
        let session = TerminalSession::spawn(&channel_id, &cwd, size, outbound_tx)?;
        let snapshot = session.snapshot()?;
        self.sessions.insert(channel_id, session);
        Ok(snapshot)
    }

    pub fn input(&self, channel_id: &str, payload: Option<Value>) -> Result<(), String> {
        let session = self
            .sessions
            .get(channel_id)
            .ok_or_else(|| format!("Terminal channel not found: {channel_id}"))?;
        let data = payload
            .as_ref()
            .and_then(|value| value.get("data"))
            .and_then(Value::as_str)
            .ok_or_else(|| "terminal.input payload.data is required".to_string())?;
        session.input(data.as_bytes())
    }

    pub fn resize(&self, channel_id: &str, payload: Option<Value>) -> Result<(), String> {
        let session = self
            .sessions
            .get(channel_id)
            .ok_or_else(|| format!("Terminal channel not found: {channel_id}"))?;
        let size = terminal_size_from_payload(payload.as_ref())?;
        session.resize(size)
    }

    pub fn snapshot(&self, channel_id: &str) -> Result<Value, String> {
        let session = self
            .sessions
            .get(channel_id)
            .ok_or_else(|| format!("Terminal channel not found: {channel_id}"))?;
        session.snapshot()
    }

    pub fn close(&mut self, channel_id: &str) {
        if let Some(session) = self.sessions.remove(channel_id) {
            session.close();
        }
    }

    pub fn close_all(&mut self) {
        let sessions = std::mem::take(&mut self.sessions);
        for (_, session) in sessions {
            session.close();
        }
    }
}

struct TerminalSession {
    master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    child: Arc<StdMutex<Box<dyn PtyChild + Send + Sync>>>,
    scrollback: Arc<StdMutex<VecDeque<u8>>>,
}

impl TerminalSession {
    fn spawn(
        channel_id: &str,
        cwd: &Path,
        size: PtySize,
        outbound_tx: mpsc::Sender<AgentNodeOutgoing>,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size)
            .map_err(|err| format!("Failed to open PTY: {err}"))?;
        let mut command = PtyCommandBuilder::new(default_shell_program());
        command.cwd(cwd.as_os_str());
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| format!("Failed to spawn shell: {err}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| format!("Failed to clone PTY reader: {err}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| format!("Failed to take PTY writer: {err}"))?;
        let scrollback = Arc::new(StdMutex::new(VecDeque::new()));
        let reader_scrollback = scrollback.clone();
        let reader_channel_id = channel_id.to_string();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = outbound_tx.blocking_send(AgentNodeOutgoing::TerminalExit {
                            channel_id: reader_channel_id.clone(),
                            payload: serde_json::json!({ "reason": "eof" }),
                        });
                        break;
                    }
                    Ok(byte_count) => {
                        append_scrollback(&reader_scrollback, &buffer[..byte_count]);
                        let text = String::from_utf8_lossy(&buffer[..byte_count]).to_string();
                        let _ = outbound_tx.blocking_send(AgentNodeOutgoing::TerminalOutput {
                            channel_id: reader_channel_id.clone(),
                            payload: serde_json::json!({ "data": text }),
                        });
                    }
                    Err(err) => {
                        let _ = outbound_tx.blocking_send(AgentNodeOutgoing::TerminalExit {
                            channel_id: reader_channel_id.clone(),
                            payload: serde_json::json!({
                                "reason": "read_error",
                                "message": err.to_string(),
                            }),
                        });
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master: Arc::new(StdMutex::new(pair.master)),
            writer: Arc::new(StdMutex::new(writer)),
            child: Arc::new(StdMutex::new(child)),
            scrollback,
        })
    }

    fn input(&self, bytes: &[u8]) -> Result<(), String> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "Terminal writer lock is poisoned".to_string())?;
        writer
            .write_all(bytes)
            .map_err(|err| format!("Failed to write terminal input: {err}"))?;
        writer
            .flush()
            .map_err(|err| format!("Failed to flush terminal input: {err}"))
    }

    fn resize(&self, size: PtySize) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|_| "Terminal master lock is poisoned".to_string())?;
        master
            .resize(size)
            .map_err(|err| format!("Failed to resize terminal: {err}"))
    }

    fn snapshot(&self) -> Result<Value, String> {
        let scrollback = self
            .scrollback
            .lock()
            .map_err(|_| "Terminal scrollback lock is poisoned".to_string())?;
        let bytes = scrollback.iter().copied().collect::<Vec<u8>>();
        Ok(serde_json::json!({
            "data": String::from_utf8_lossy(&bytes).to_string(),
        }))
    }

    fn close(self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

async fn terminal_open_cwd(payload: &Value) -> Result<PathBuf, String> {
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| "terminal.open payload.cwd is required".to_string())?;
    if cwd.trim().is_empty() {
        return Err("terminal.open payload.cwd cannot be empty".to_string());
    }
    canonicalize_existing_dir(Path::new(cwd)).await
}

fn terminal_size_from_payload(payload: Option<&Value>) -> Result<PtySize, String> {
    let default = PtySize::default();
    let Some(payload) = payload else {
        return Ok(default);
    };
    let cols = optional_u16(payload, "cols")?.unwrap_or(default.cols);
    let rows = optional_u16(payload, "rows")?.unwrap_or(default.rows);
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn optional_u16(payload: &Value, field: &str) -> Result<Option<u16>, String> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    let Some(number) = value.as_u64() else {
        return Err(format!("{field} must be a positive integer"));
    };
    if number == 0 || number > u16::MAX as u64 {
        return Err(format!("{field} is out of range"));
    }
    Ok(Some(number as u16))
}

fn append_scrollback(scrollback: &StdMutex<VecDeque<u8>>, bytes: &[u8]) {
    let Ok(mut scrollback) = scrollback.lock() else {
        return;
    };
    scrollback.extend(bytes.iter().copied());
    while scrollback.len() > TERMINAL_SCROLLBACK_LIMIT_BYTES {
        scrollback.pop_front();
    }
}

fn default_shell_program() -> String {
    if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "sh".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_connect_url_normalizes_http_and_ws_inputs() {
        assert_eq!(
            node_connect_url("https://agent.example.com").unwrap(),
            "wss://agent.example.com/api/node/connect"
        );
        assert_eq!(
            node_connect_url("http://127.0.0.1:3000/base?x=1").unwrap(),
            "ws://127.0.0.1:3000/api/node/connect"
        );
        assert_eq!(
            node_connect_url("ws://127.0.0.1:3000").unwrap(),
            "ws://127.0.0.1:3000/api/node/connect"
        );
        assert_eq!(
            node_connect_url("wss://agent.example.com/api/node/connect?old=1").unwrap(),
            "wss://agent.example.com/api/node/connect"
        );
    }

    #[test]
    fn protocol_serializes_runtime_message_shape_used_by_agent_console() {
        let frame = AgentNodeOutgoing::RuntimeMessage {
            channel_id: "runtime-1".to_string(),
            payload: serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {} }),
        };
        let value = serde_json::to_value(frame).unwrap();
        assert_eq!(value["type"], "runtime.message");
        assert_eq!(value["channelId"], "runtime-1");
        assert_eq!(value["payload"]["id"], 1);
    }

    #[test]
    fn protocol_serializes_terminal_output_shape_used_by_agent_console() {
        let frame = AgentNodeOutgoing::TerminalOutput {
            channel_id: "term-1".to_string(),
            payload: serde_json::json!({ "data": "hello" }),
        };
        let value = serde_json::to_value(frame).unwrap();
        assert_eq!(value["type"], "terminal.output");
        assert_eq!(value["channelId"], "term-1");
        assert_eq!(value["payload"]["data"], "hello");
    }

    #[test]
    fn runtime_open_requires_non_empty_cwd() {
        assert_eq!(
            runtime_open_cwd(Some(&serde_json::json!({ "cwd": "/repo" }))).unwrap(),
            PathBuf::from("/repo")
        );
        assert!(runtime_open_cwd(Some(&serde_json::json!({ "cwd": "" }))).is_err());
        assert!(runtime_open_cwd(Some(&serde_json::json!({}))).is_err());
    }

    #[tokio::test]
    async fn runtime_manager_rejects_unknown_channel_send() {
        let mut manager = RuntimeChannelManager::new(CodexRuntimeLauncher::default());
        let err = manager
            .send("missing", serde_json::json!({ "jsonrpc": "2.0" }))
            .await
            .unwrap_err();
        assert!(err.contains("Runtime channel not found"));
    }

    #[test]
    fn terminal_size_rejects_invalid_dimensions() {
        let size = terminal_size_from_payload(Some(&serde_json::json!({
            "cols": 120,
            "rows": 40
        })))
        .unwrap();
        assert_eq!(size.cols, 120);
        assert_eq!(size.rows, 40);
        assert!(terminal_size_from_payload(Some(&serde_json::json!({
            "cols": 0
        })))
        .is_err());
    }

    #[test]
    fn reconnect_delay_doubles_until_cap() {
        let policy = AgentNodeReconnectPolicy {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(5),
            stable_reset_after: Duration::from_secs(60),
        };

        let first = next_reconnect_delay(Duration::from_secs(1), &policy);
        let second = next_reconnect_delay(first, &policy);
        let capped = next_reconnect_delay(second, &policy);

        assert_eq!(first, Duration::from_secs(2));
        assert_eq!(second, Duration::from_secs(4));
        assert_eq!(capped, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn fs_list_returns_project_relative_directory_shape() {
        let root = std::env::temp_dir().join(format!("agent-node-fs-{}", std::process::id()));
        let nested = root.join("nested");
        let file = root.join("file.txt");
        let root_for_payload = root.to_string_lossy().to_string();
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(&file, "ignored").await.unwrap();

        let payload = list_directory(serde_json::json!({
            "rootPath": root_for_payload,
            "path": "/"
        }))
        .await
        .unwrap();

        assert_eq!(payload["path"], "/");
        assert_eq!(payload["parent"], Value::Null);
        assert_eq!(
            payload["children"],
            serde_json::json!([
                { "kind": "directory", "name": "nested", "path": "/nested" },
                { "kind": "file", "name": "file.txt", "path": "/file.txt" }
            ])
        );
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn fs_read_returns_utf8_project_file() {
        let root = std::env::temp_dir().join(format!("agent-node-read-{}", std::process::id()));
        let nested = root.join("src");
        let file = nested.join("main.ts");
        let root_for_payload = root.to_string_lossy().to_string();
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(&file, "export const ok = true;\n")
            .await
            .unwrap();

        let payload = read_file(serde_json::json!({
            "rootPath": root_for_payload,
            "path": "/src/main.ts"
        }))
        .await
        .unwrap();

        assert_eq!(payload["path"], "/src/main.ts");
        assert_eq!(payload["content"], "export const ok = true;\n");
        assert_eq!(payload["encoding"], "utf8");
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn fs_write_creates_project_file() {
        let root = std::env::temp_dir().join(format!("agent-node-write-{}", std::process::id()));
        let nested = root.join("src");
        let root_for_payload = root.to_string_lossy().to_string();
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&nested).await.unwrap();

        let payload = write_file(serde_json::json!({
            "rootPath": root_for_payload,
            "path": "/src/main.ts",
            "content": "export const ok = true;\n",
            "encoding": "utf8"
        }))
        .await
        .unwrap();

        assert_eq!(payload["path"], "/src/main.ts");
        assert_eq!(payload["bytesWritten"], 24);
        let content = tokio::fs::read_to_string(nested.join("main.ts"))
            .await
            .unwrap();
        assert_eq!(content, "export const ok = true;\n");
        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
