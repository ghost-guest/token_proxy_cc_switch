use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(debug_assertions)]
macro_rules! debug_log_error {
    ($($arg:tt)*) => {
        eprintln!($($arg)*);
    };
}

#[cfg(not(debug_assertions))]
macro_rules! debug_log_error {
    ($($arg:tt)*) => {};
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TokenUsage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct UsageSnapshot {
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) cached_tokens: Option<u64>,
    pub(crate) usage_json: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct LogEntry {
    pub(crate) ts_ms: u128,
    pub(crate) path: String,
    pub(crate) provider: String,
    pub(crate) upstream_id: String,
    pub(crate) account_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mapped_model: Option<String>,
    pub(crate) stream: bool,
    pub(crate) status: u16,
    pub(crate) usage: Option<TokenUsage>,
    pub(crate) cached_tokens: Option<u64>,
    pub(crate) usage_json: Option<Value>,
    pub(crate) upstream_request_id: Option<String>,
    pub(crate) request_headers: Option<String>,
    pub(crate) request_body: Option<String>,
    pub(crate) response_error: Option<String>,
    pub(crate) latency_ms: u128,
    pub(crate) upstream_first_byte_ms: Option<u128>,
    pub(crate) upstream_response_headers_ms: Option<u128>,
    pub(crate) upstream_first_body_chunk_ms: Option<u128>,
    pub(crate) first_client_flush_ms: Option<u128>,
    pub(crate) first_output_ms: Option<u128>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct RequestTimingSnapshot {
    pub(crate) upstream_first_byte_ms: Option<u128>,
    pub(crate) upstream_response_headers_ms: Option<u128>,
    pub(crate) upstream_first_body_chunk_ms: Option<u128>,
    pub(crate) first_client_flush_ms: Option<u128>,
    pub(crate) first_output_ms: Option<u128>,
}

#[derive(Clone, Default)]
pub(crate) struct RequestTimings {
    inner: Arc<Mutex<RequestTimingSnapshot>>,
}

impl RequestTimings {
    pub(crate) fn mark_upstream_response_headers(&self, value: u128) {
        self.mark_once(|snapshot| &mut snapshot.upstream_response_headers_ms, value);
    }

    pub(crate) fn mark_upstream_first_body_chunk(&self, value: u128) {
        self.mark_once(|snapshot| &mut snapshot.upstream_first_body_chunk_ms, value);
        self.mark_once(|snapshot| &mut snapshot.upstream_first_byte_ms, value);
    }

    fn mark_upstream_first_byte(&self, value: u128) {
        self.mark_upstream_first_body_chunk(value);
    }

    fn mark_first_client_flush(&self, value: u128) {
        self.mark_once(|snapshot| &mut snapshot.first_client_flush_ms, value);
    }

    fn mark_first_output(&self, value: u128) {
        self.mark_once(|snapshot| &mut snapshot.first_output_ms, value);
    }

    fn snapshot(&self) -> RequestTimingSnapshot {
        self.inner.lock().map(|guard| *guard).unwrap_or_default()
    }

    fn mark_once(
        &self,
        select: impl FnOnce(&mut RequestTimingSnapshot) -> &mut Option<u128>,
        value: u128,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let slot = select(&mut guard);
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

#[derive(Clone)]
pub(crate) struct LogContext {
    pub(crate) path: String,
    pub(crate) provider: String,
    pub(crate) upstream_id: String,
    pub(crate) account_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mapped_model: Option<String>,
    pub(crate) stream: bool,
    pub(crate) status: u16,
    pub(crate) upstream_request_id: Option<String>,
    pub(crate) request_headers: Option<String>,
    pub(crate) request_body: Option<String>,
    // Legacy field name: this records first upstream body chunk, not response headers.
    pub(crate) ttfb_ms: Option<u128>,
    pub(crate) timings: RequestTimings,
    pub(crate) start: Instant,
}

impl LogContext {
    pub(crate) fn mark_upstream_first_byte(&mut self) {
        let value = self.start.elapsed().as_millis();
        if self.ttfb_ms.is_none() {
            self.ttfb_ms = Some(value);
        }
        self.timings.mark_upstream_first_byte(value);
    }

    pub(crate) fn mark_first_client_flush(&mut self) {
        self.timings
            .mark_first_client_flush(self.start.elapsed().as_millis());
    }

    pub(crate) fn mark_first_output(&mut self) {
        self.timings
            .mark_first_output(self.start.elapsed().as_millis());
    }

    pub(crate) fn timing_snapshot(&self) -> RequestTimingSnapshot {
        self.timings.snapshot()
    }
}

pub(crate) struct LogWriter {
    sqlite: Option<SqlitePool>,
}

impl LogWriter {
    pub(crate) fn new(sqlite: Option<SqlitePool>) -> Self {
        Self { sqlite }
    }

    // Fire-and-forget logging to avoid blocking the request path.
    pub(crate) fn write_detached(self: Arc<Self>, entry: LogEntry) {
        tokio::spawn(async move {
            self.write(&entry).await;
        });
    }

    pub(crate) async fn write(&self, entry: &LogEntry) {
        let Some(pool) = self.sqlite.as_ref() else {
            return;
        };
        if let Err(_err) = insert_log_entry(pool, entry).await {
            debug_log_error!("proxy sqlite write failed: {_err}");
        }
    }
}

pub(crate) fn build_log_entry(
    context: &LogContext,
    usage: UsageSnapshot,
    response_error: Option<String>,
) -> LogEntry {
    let timing = context.timing_snapshot();
    let upstream_first_body_chunk_ms = timing
        .upstream_first_body_chunk_ms
        .or(timing.upstream_first_byte_ms)
        .or(context.ttfb_ms);
    let upstream_first_byte_ms = timing
        .upstream_first_byte_ms
        .or(upstream_first_body_chunk_ms);
    let latency_ms = timing
        .first_output_ms
        .or(timing.first_client_flush_ms)
        .or(upstream_first_body_chunk_ms)
        .or(timing.upstream_response_headers_ms)
        .unwrap_or_else(|| context.start.elapsed().as_millis());
    LogEntry {
        ts_ms: now_ms(),
        path: context.path.clone(),
        provider: context.provider.clone(),
        upstream_id: context.upstream_id.clone(),
        account_id: context.account_id.clone(),
        model: context.model.clone(),
        mapped_model: context.mapped_model.clone(),
        stream: context.stream,
        status: context.status,
        usage: usage.usage,
        cached_tokens: usage.cached_tokens,
        usage_json: usage.usage_json,
        upstream_request_id: context.upstream_request_id.clone(),
        request_headers: context.request_headers.clone(),
        request_body: context.request_body.clone(),
        response_error,
        latency_ms,
        upstream_first_byte_ms,
        upstream_response_headers_ms: timing.upstream_response_headers_ms,
        upstream_first_body_chunk_ms,
        first_client_flush_ms: timing.first_client_flush_ms,
        first_output_ms: timing.first_output_ms,
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn insert_log_entry(pool: &SqlitePool, entry: &LogEntry) -> Result<(), sqlx::Error> {
    let usage = entry.usage.as_ref();
    let input_tokens = usage.and_then(|usage| usage.input_tokens).map(to_i64_u64);
    let output_tokens = usage.and_then(|usage| usage.output_tokens).map(to_i64_u64);
    let total_tokens = usage.and_then(|usage| usage.total_tokens).map(to_i64_u64);
    let cached_tokens = entry.cached_tokens.map(to_i64_u64);
    let usage_json = entry.usage_json.as_ref().map(Value::to_string);

    sqlx::query(
        r#"
INSERT INTO request_logs (
  ts_ms,
  path,
  provider,
  upstream_id,
  account_id,
  model,
  mapped_model,
  stream,
  status,
  input_tokens,
  output_tokens,
  total_tokens,
  cached_tokens,
  usage_json,
  upstream_request_id,
  request_headers,
  request_body,
  response_error,
  latency_ms,
  upstream_first_byte_ms,
  upstream_response_headers_ms,
  upstream_first_body_chunk_ms,
  first_client_flush_ms,
  first_output_ms
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
"#,
    )
    .bind(to_i64_u128(entry.ts_ms))
    .bind(entry.path.as_str())
    .bind(entry.provider.as_str())
    .bind(entry.upstream_id.as_str())
    .bind(entry.account_id.as_deref())
    .bind(entry.model.as_deref())
    .bind(entry.mapped_model.as_deref())
    .bind(entry.stream)
    .bind(i64::from(entry.status))
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(total_tokens)
    .bind(cached_tokens)
    .bind(usage_json.as_deref())
    .bind(entry.upstream_request_id.as_deref())
    .bind(entry.request_headers.as_deref())
    .bind(entry.request_body.as_deref())
    .bind(entry.response_error.as_deref())
    .bind(to_i64_u128(entry.latency_ms))
    .bind(entry.upstream_first_byte_ms.map(to_i64_u128))
    .bind(entry.upstream_response_headers_ms.map(to_i64_u128))
    .bind(entry.upstream_first_body_chunk_ms.map(to_i64_u128))
    .bind(entry.first_client_flush_ms.map(to_i64_u128))
    .bind(entry.first_output_ms.map(to_i64_u128))
    .execute(pool)
    .await?;

    Ok(())
}

fn to_i64_u128(value: u128) -> i64 {
    value.min(i64::MAX as u128) as i64
}

fn to_i64_u64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn build_log_entry_keeps_response_headers_and_body_chunk_timings_separate() {
        let timings = RequestTimings::default();
        timings.mark_upstream_response_headers(25);
        timings.mark_upstream_first_body_chunk(120);
        timings.mark_upstream_first_byte(120);
        timings.mark_first_output(220);

        let context = LogContext {
            path: "/v1/responses".to_string(),
            provider: "openai-response".to_string(),
            upstream_id: "airouter".to_string(),
            account_id: None,
            model: Some("gpt-5.5".to_string()),
            mapped_model: None,
            stream: true,
            status: 200,
            upstream_request_id: None,
            request_headers: None,
            request_body: None,
            ttfb_ms: None,
            timings,
            start: Instant::now() - Duration::from_millis(300),
        };

        let entry = build_log_entry(&context, UsageSnapshot::default(), None);

        assert_eq!(entry.upstream_response_headers_ms, Some(25));
        assert_eq!(entry.upstream_first_body_chunk_ms, Some(120));
        assert_eq!(entry.upstream_first_byte_ms, Some(120));
        assert_eq!(entry.first_output_ms, Some(220));
        assert_eq!(entry.latency_ms, 220);
    }
}
