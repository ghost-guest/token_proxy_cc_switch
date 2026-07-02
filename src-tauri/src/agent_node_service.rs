use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use token_proxy_core::agent_node::{AgentNodeClient, AgentNodeConfig};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const AGENT_NODE_CONFIG_FILE_NAME: &str = "agent-node.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentNodeStoredConfig {
    pub enabled: bool,
    pub server_url: String,
    pub api_key: String,
    pub hostname: Option<String>,
}

impl Default for AgentNodeStoredConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_url: String::new(),
            api_key: String::new(),
            hostname: default_hostname(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentNodeServiceState {
    Running,
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentNodeServiceStatus {
    pub state: AgentNodeServiceState,
    pub enabled: bool,
    pub server_url: Option<String>,
    pub hostname: Option<String>,
    pub last_error: Option<String>,
    pub started_at_ms: Option<u128>,
}

struct RunningAgentNode {
    task: JoinHandle<Result<(), String>>,
    started_at: SystemTime,
    config: AgentNodeStoredConfig,
}

struct AgentNodeServiceInner {
    running: Option<RunningAgentNode>,
    last_error: Option<String>,
}

#[derive(Clone)]
pub struct AgentNodeServiceHandle {
    inner: Arc<Mutex<AgentNodeServiceInner>>,
}

impl AgentNodeServiceHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AgentNodeServiceInner {
                running: None,
                last_error: None,
            })),
        }
    }

    pub async fn status(
        &self,
        paths: &token_proxy_core::paths::TokenProxyPaths,
    ) -> AgentNodeServiceStatus {
        let config = read_agent_node_config(paths).await.unwrap_or_default();
        self.status_for_config(config).await
    }

    pub async fn start(
        &self,
        paths: &token_proxy_core::paths::TokenProxyPaths,
    ) -> Result<AgentNodeServiceStatus, String> {
        let config = read_agent_node_config(paths).await?;
        self.start_with_config(config).await
    }

    pub async fn start_with_config(
        &self,
        config: AgentNodeStoredConfig,
    ) -> Result<AgentNodeServiceStatus, String> {
        validate_runnable_config(&config)?;

        let mut inner = self.inner.lock().await;
        if inner.running.is_some() {
            return Ok(status_from_inner(&inner, &config));
        }

        let runtime_config = to_runtime_config(&config);
        let task = tokio::spawn(async move {
            let mut client = AgentNodeClient::new(runtime_config);
            client.run_with_reconnect().await
        });
        inner.last_error = None;
        inner.running = Some(RunningAgentNode {
            task,
            started_at: SystemTime::now(),
            config: config.clone(),
        });
        tracing::info!(server_url = %config.server_url, "agent node service started");
        Ok(status_from_inner(&inner, &config))
    }

    pub async fn stop(
        &self,
        paths: &token_proxy_core::paths::TokenProxyPaths,
    ) -> Result<AgentNodeServiceStatus, String> {
        let config = read_agent_node_config(paths).await.unwrap_or_default();
        let mut inner = self.inner.lock().await;
        if let Some(running) = inner.running.take() {
            running.task.abort();
            tracing::info!("agent node service stopped");
        }
        Ok(status_from_inner(&inner, &config))
    }

    pub async fn save_config(
        &self,
        paths: &token_proxy_core::paths::TokenProxyPaths,
        config: AgentNodeStoredConfig,
    ) -> Result<AgentNodeServiceStatus, String> {
        write_agent_node_config(paths, &config).await?;

        if !config.enabled {
            return self.stop(paths).await;
        }

        if config.server_url.trim().is_empty() || config.api_key.trim().is_empty() {
            let mut inner = self.inner.lock().await;
            if let Some(running) = inner.running.take() {
                running.task.abort();
            }
            inner.last_error = Some("Agent node server URL and API key are required".to_string());
            return Ok(status_from_inner(&inner, &config));
        }

        self.restart_with_config(config).await
    }

    pub async fn restart(
        &self,
        paths: &token_proxy_core::paths::TokenProxyPaths,
    ) -> Result<AgentNodeServiceStatus, String> {
        let config = read_agent_node_config(paths).await?;
        self.restart_with_config(config).await
    }

    async fn restart_with_config(
        &self,
        config: AgentNodeStoredConfig,
    ) -> Result<AgentNodeServiceStatus, String> {
        {
            let mut inner = self.inner.lock().await;
            if let Some(running) = inner.running.take() {
                running.task.abort();
            }
        }
        self.start_with_config(config).await
    }

    async fn status_for_config(&self, config: AgentNodeStoredConfig) -> AgentNodeServiceStatus {
        let mut inner = self.inner.lock().await;
        if let Some(running) = inner.running.as_ref() {
            if running.task.is_finished() {
                let running = inner.running.take().expect("running task existed");
                match running.task.await {
                    Ok(Ok(())) => inner.last_error = None,
                    Ok(Err(err)) => inner.last_error = Some(err),
                    Err(err) if err.is_cancelled() => inner.last_error = None,
                    Err(err) => inner.last_error = Some(format!("Agent node task failed: {err}")),
                }
            }
        }
        status_from_inner(&inner, &config)
    }
}

pub async fn read_agent_node_config(
    paths: &token_proxy_core::paths::TokenProxyPaths,
) -> Result<AgentNodeStoredConfig, String> {
    let path = paths.data_dir().join(AGENT_NODE_CONFIG_FILE_NAME);
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|err| format!("Failed to parse agent node config: {err}")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(AgentNodeStoredConfig::default())
        }
        Err(err) => Err(format!("Failed to read agent node config: {err}")),
    }
}

pub async fn write_agent_node_config(
    paths: &token_proxy_core::paths::TokenProxyPaths,
    config: &AgentNodeStoredConfig,
) -> Result<(), String> {
    let path = paths.data_dir().join(AGENT_NODE_CONFIG_FILE_NAME);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| format!("Failed to create agent node config directory: {err}"))?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|err| format!("Failed to serialize agent node config: {err}"))?;
    tokio::fs::write(path, format!("{text}\n"))
        .await
        .map_err(|err| format!("Failed to write agent node config: {err}"))
}

fn validate_runnable_config(config: &AgentNodeStoredConfig) -> Result<(), String> {
    if config.server_url.trim().is_empty() {
        return Err("Agent node server URL is required".to_string());
    }
    if config.api_key.trim().is_empty() {
        return Err("Agent node API key is required".to_string());
    }
    Ok(())
}

fn to_runtime_config(config: &AgentNodeStoredConfig) -> AgentNodeConfig {
    AgentNodeConfig {
        server_url: config.server_url.trim().to_string(),
        api_key: config.api_key.trim().to_string(),
        hostname: config
            .hostname
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(default_hostname),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn status_from_inner(
    inner: &AgentNodeServiceInner,
    config: &AgentNodeStoredConfig,
) -> AgentNodeServiceStatus {
    let running = inner.running.as_ref();
    let active_config = running.map(|value| &value.config).unwrap_or(config);
    AgentNodeServiceStatus {
        state: if running.is_some() {
            AgentNodeServiceState::Running
        } else {
            AgentNodeServiceState::Stopped
        },
        enabled: config.enabled,
        server_url: non_empty_string(active_config.server_url.clone()),
        hostname: active_config.hostname.clone().and_then(non_empty_string),
        last_error: inner.last_error.clone(),
        started_at_ms: running.and_then(|value| {
            value
                .started_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_millis())
        }),
    }
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let config = AgentNodeStoredConfig::default();

        assert!(!config.enabled);
        assert_eq!(config.server_url, "");
        assert_eq!(config.api_key, "");
    }

    #[test]
    fn validates_runnable_config() {
        let mut config = AgentNodeStoredConfig::default();
        assert!(validate_runnable_config(&config).is_err());

        config.server_url = "https://agent.example.com".to_string();
        config.api_key = "acn_secret".to_string();
        assert_eq!(validate_runnable_config(&config), Ok(()));
    }
}
