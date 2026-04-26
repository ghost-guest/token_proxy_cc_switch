use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamModelProbeStatus {
    Pending,
    Ok,
    Failed,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamModelProbe {
    pub upstream_id: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub status: UpstreamModelProbeStatus,
    pub checked_at_ts_ms: Option<u64>,
    pub error: Option<String>,
    pub models: Vec<String>,
}

impl UpstreamModelProbe {
    pub(crate) fn pending(upstream_id: &str, provider: &str, account_id: Option<String>) -> Self {
        Self {
            upstream_id: upstream_id.to_string(),
            provider: provider.to_string(),
            account_id,
            status: UpstreamModelProbeStatus::Pending,
            checked_at_ts_ms: None,
            error: None,
            models: Vec::new(),
        }
    }

    pub(crate) fn completed(
        upstream_id: &str,
        provider: &str,
        account_id: Option<String>,
        status: UpstreamModelProbeStatus,
        error: Option<String>,
        models: Vec<String>,
    ) -> Self {
        Self {
            upstream_id: upstream_id.to_string(),
            provider: provider.to_string(),
            account_id,
            status,
            checked_at_ts_ms: Some(now_ts_ms()),
            error,
            models,
        }
    }
}

#[derive(Default)]
pub(crate) struct UpstreamModelDiscoveryCache {
    probes: RwLock<Vec<UpstreamModelProbe>>,
}

impl UpstreamModelDiscoveryCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn snapshot(&self) -> Vec<UpstreamModelProbe> {
        self.probes.read().await.clone()
    }

    pub(crate) async fn replace_all(&self, probes: Vec<UpstreamModelProbe>) {
        *self.probes.write().await = probes;
    }

    pub(crate) async fn replace_at(&self, index: usize, probe: UpstreamModelProbe) {
        let mut probes = self.probes.write().await;
        if let Some(slot) = probes.get_mut(index) {
            *slot = probe;
            return;
        }
        probes.push(probe);
    }
}

fn now_ts_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
