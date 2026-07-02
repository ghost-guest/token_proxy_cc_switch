use std::sync::Arc;

use tauri::Manager;

use crate::agent_node_service::{
    read_agent_node_config, AgentNodeServiceHandle, AgentNodeServiceStatus, AgentNodeStoredConfig,
};

#[tauri::command]
pub async fn agent_node_read_config(
    app: tauri::AppHandle,
) -> Result<AgentNodeStoredConfig, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    read_agent_node_config(paths.inner().as_ref()).await
}

#[tauri::command]
pub async fn agent_node_status(
    app: tauri::AppHandle,
    service: tauri::State<'_, AgentNodeServiceHandle>,
) -> Result<AgentNodeServiceStatus, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    Ok(service.status(paths.inner().as_ref()).await)
}

#[tauri::command]
pub async fn agent_node_save_config(
    app: tauri::AppHandle,
    service: tauri::State<'_, AgentNodeServiceHandle>,
    config: AgentNodeStoredConfig,
) -> Result<AgentNodeServiceStatus, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    service.save_config(paths.inner().as_ref(), config).await
}

#[tauri::command]
pub async fn agent_node_start(
    app: tauri::AppHandle,
    service: tauri::State<'_, AgentNodeServiceHandle>,
) -> Result<AgentNodeServiceStatus, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    service.start(paths.inner().as_ref()).await
}

#[tauri::command]
pub async fn agent_node_stop(
    app: tauri::AppHandle,
    service: tauri::State<'_, AgentNodeServiceHandle>,
) -> Result<AgentNodeServiceStatus, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    service.stop(paths.inner().as_ref()).await
}

#[tauri::command]
pub async fn agent_node_restart(
    app: tauri::AppHandle,
    service: tauri::State<'_, AgentNodeServiceHandle>,
) -> Result<AgentNodeServiceStatus, String> {
    let paths = app.state::<Arc<token_proxy_core::paths::TokenProxyPaths>>();
    service.restart(paths.inner().as_ref()).await
}
