use std::{collections::HashMap, sync::Mutex, time::Duration};

use reqwest::{Client, ClientBuilder, Proxy};

pub(crate) struct ProxyHttpClients {
    direct: Client,
    by_proxy_url: Mutex<HashMap<String, Client>>,
    codex_by_proxy_key: Mutex<HashMap<CodexClientKey, Client>>,
}

impl ProxyHttpClients {
    pub(crate) fn new() -> Result<Self, String> {
        let direct = tuned_client_builder()
            // 默认不走系统代理；仅当用户显式配置 proxy_url 时才走代理。
            .no_proxy()
            .build()
            .map_err(|err| format!("Failed to build direct HTTP client: {err}"))?;
        Ok(Self {
            direct,
            by_proxy_url: Mutex::new(HashMap::new()),
            codex_by_proxy_key: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn client_for_proxy_url(&self, proxy_url: Option<&str>) -> Result<Client, String> {
        let Some(proxy_url) = proxy_url
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        else {
            return Ok(self.direct.clone());
        };

        let mut guard = self
            .by_proxy_url
            .lock()
            .map_err(|_| "HTTP client pool is poisoned.".to_string())?;
        if let Some(existing) = guard.get(proxy_url) {
            return Ok(existing.clone());
        }

        let proxy = Proxy::all(proxy_url)
            .map_err(|_| "proxy_url is invalid or not supported.".to_string())?;
        let client = tuned_client_builder()
            .proxy(proxy)
            .build()
            .map_err(|err| format!("Failed to build proxied HTTP client: {err}"))?;
        guard.insert(proxy_url.to_string(), client.clone());
        Ok(client)
    }

    pub(crate) fn codex_client_for_proxy_url(
        &self,
        proxy_url: Option<&str>,
        http1_only: bool,
    ) -> Result<Client, String> {
        let key = CodexClientKey::new(proxy_url, http1_only);
        let mut guard = self
            .codex_by_proxy_key
            .lock()
            .map_err(|_| "Codex HTTP client pool is poisoned.".to_string())?;
        if let Some(existing) = guard.get(&key) {
            return Ok(existing.clone());
        }
        let client = build_codex_client(key.proxy_url.as_deref(), key.http1_only)?;
        guard.insert(key, client.clone());
        Ok(client)
    }

    #[cfg(test)]
    pub(crate) fn codex_client_count(&self) -> usize {
        self.codex_by_proxy_key
            .lock()
            .map(|guard| guard.len())
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HttpClientTuning {
    connect_timeout: Duration,
    pool_idle_timeout: Duration,
    pool_max_idle_per_host: usize,
    tcp_nodelay: bool,
    http2_adaptive_window: bool,
}

impl Default for HttpClientTuning {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            pool_idle_timeout: Duration::from_secs(180),
            pool_max_idle_per_host: 32,
            tcp_nodelay: true,
            http2_adaptive_window: true,
        }
    }
}

fn tuned_client_builder() -> ClientBuilder {
    let tuning = HttpClientTuning::default();
    ClientBuilder::new()
        .connect_timeout(tuning.connect_timeout)
        .pool_idle_timeout(tuning.pool_idle_timeout)
        .pool_max_idle_per_host(tuning.pool_max_idle_per_host)
        .tcp_nodelay(tuning.tcp_nodelay)
        .http2_adaptive_window(tuning.http2_adaptive_window)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodexClientKey {
    proxy_url: Option<String>,
    http1_only: bool,
}

impl CodexClientKey {
    fn new(proxy_url: Option<&str>, http1_only: bool) -> Self {
        Self {
            proxy_url: proxy_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            http1_only,
        }
    }
}

fn build_codex_client(proxy_url: Option<&str>, http1_only: bool) -> Result<Client, String> {
    let mut builder = tuned_client_builder();
    if let Some(proxy_url) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) {
        let proxy = Proxy::all(proxy_url)
            .map_err(|_| "proxy_url is invalid or not supported.".to_string())?;
        builder = builder.proxy(proxy);
    } else {
        builder = builder.no_proxy();
    }
    if http1_only {
        builder = builder.http1_only();
    }
    builder
        .build()
        .map_err(|err| format!("Failed to build Codex upstream client: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_clients_are_cached_by_proxy_and_http1_mode() {
        let clients = ProxyHttpClients::new().expect("clients");

        let _ = clients
            .codex_client_for_proxy_url(Some("http://127.0.0.1:7890"), false)
            .expect("proxied codex client");
        let _ = clients
            .codex_client_for_proxy_url(Some("http://127.0.0.1:7890"), false)
            .expect("same proxied codex client");
        let _ = clients
            .codex_client_for_proxy_url(Some("http://127.0.0.1:7890"), true)
            .expect("http1 codex client");

        assert_eq!(clients.codex_client_count(), 2);
    }

    #[test]
    fn default_tuning_keeps_idle_connections_ready_for_burst_traffic() {
        let tuning = HttpClientTuning::default();

        assert!(tuning.tcp_nodelay);
        assert!(tuning.http2_adaptive_window);
        assert_eq!(tuning.connect_timeout, std::time::Duration::from_secs(10));
        assert_eq!(
            tuning.pool_idle_timeout,
            std::time::Duration::from_secs(180)
        );
        assert_eq!(tuning.pool_max_idle_per_host, 32);
    }
}
