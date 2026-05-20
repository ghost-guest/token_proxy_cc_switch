use axum::body::Bytes;
use futures_util::{stream::try_unfold, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use super::super::log::{attach_response_body, build_log_entry, LogContext, LogWriter};
use super::super::model;
use super::super::sse::SseEventParser;
use super::super::token_rate::RequestTokenTracker;
use super::super::usage::SseUsageCollector;
use super::{
    PROVIDER_ANTHROPIC, PROVIDER_CODEX, PROVIDER_GEMINI, PROVIDER_OPENAI, PROVIDER_OPENAI_RESPONSES,
};

pub(crate) const STREAM_DROPPED_ERROR: &str = "stream dropped before completion";

pub(super) fn stream_with_logging<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    token_tracker: RequestTokenTracker,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    stream_with_logging_and_semantic_timeout(upstream, context, log, token_tracker, None)
}

pub(super) fn stream_with_logging_and_semantic_timeout<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    token_tracker: RequestTokenTracker,
    semantic_timeout: Option<Duration>,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    let state = LoggingStreamState::new(upstream, context, log, token_tracker, semantic_timeout);
    try_unfold(state, |state| async move { state.step().await })
}

struct LoggingStreamState<S> {
    upstream: S,
    collector: SseUsageCollector,
    parser: SseEventParser,
    log: Arc<LogWriter>,
    context: LogContext,
    token_tracker: RequestTokenTracker,
    logged: bool,
    terminal_seen: bool,
    terminal_error: Option<String>,
    semantic_timeout: Option<Duration>,
    last_semantic_event_at: Instant,
    response_body_buf: String,
}

#[derive(Default)]
struct StreamObservation {
    starts_client_output: bool,
    semantic_event: bool,
    terminal: bool,
    terminal_json: bool,
    saw_done: bool,
    texts: Vec<String>,
}

impl<S> LoggingStreamState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        let mut entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        attach_response_body(&mut entry, &self.response_body_buf);
        self.log.clone().write_detached(entry);
        self.logged = true;
    }

    fn write_terminal_log_once(&mut self) {
        let response_error = self.terminal_error.take();
        self.write_log_once(response_error);
    }
}

impl<S> Drop for LoggingStreamState<S> {
    fn drop(&mut self) {
        // 流被客户端提前取消时不会再进入 `None/Err` 分支，这里兜底保证日志至少落一行。
        if self.terminal_seen {
            self.write_terminal_log_once();
        } else {
            self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
        }
    }
}

impl<S, E> LoggingStreamState<S>
where
    S: futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    fn new(
        upstream: S,
        context: LogContext,
        log: Arc<LogWriter>,
        token_tracker: RequestTokenTracker,
        semantic_timeout: Option<Duration>,
    ) -> Self {
        Self {
            upstream,
            collector: SseUsageCollector::new(),
            parser: SseEventParser::new(),
            log,
            context,
            token_tracker,
            logged: false,
            terminal_seen: false,
            terminal_error: None,
            semantic_timeout,
            last_semantic_event_at: Instant::now(),
            response_body_buf: String::new(),
        }
    }

    async fn step(mut self) -> Result<Option<(Bytes, Self)>, std::io::Error> {
        if self.terminal_seen {
            self.write_terminal_log_once();
            return Ok(None);
        }

        match self.next_upstream_item().await? {
            Some(Ok(chunk)) => {
                self.context.mark_upstream_first_byte();
                let mut out_chunk = chunk;
                let semantics = self.openai_stream_semantics();
                let mut observation = StreamObservation::default();
                self.parser.push_chunk(&out_chunk, |data| {
                    observe_stream_data(semantics, &self.context.provider, &data, &mut observation);
                });
                if observation.semantic_event {
                    self.last_semantic_event_at = Instant::now();
                }
                if observation.terminal {
                    self.terminal_seen = true;
                }
                if observation.should_synthesize_done() {
                    out_chunk = append_openai_done(out_chunk);
                }
                self.collector.push_chunk(&out_chunk);
                self.response_body_buf
                    .push_str(&String::from_utf8_lossy(out_chunk.as_ref()));
                if observation.starts_client_output {
                    self.context.mark_first_output();
                }
                for text in observation.texts {
                    self.token_tracker.add_output_text(&text).await;
                }
                self.context.mark_first_client_flush();
                Ok(Some((out_chunk, self)))
            }
            Some(Err(err)) => {
                let semantics = self.openai_stream_semantics();
                let mut observation = StreamObservation::default();
                self.parser.finish(|data| {
                    observe_stream_data(semantics, &self.context.provider, &data, &mut observation);
                });
                if observation.semantic_event {
                    self.last_semantic_event_at = Instant::now();
                }
                if observation.starts_client_output {
                    self.context.mark_first_output();
                }
                for text in observation.texts {
                    self.token_tracker.add_output_text(&text).await;
                }
                if observation.terminal {
                    self.terminal_seen = true;
                    self.write_terminal_log_once();
                    return Ok(None);
                }
                self.write_log_once(None);
                Err(std::io::Error::new(std::io::ErrorKind::Other, err))
            }
            None => {
                let semantics = self.openai_stream_semantics();
                let mut observation = StreamObservation::default();
                self.parser.finish(|data| {
                    observe_stream_data(semantics, &self.context.provider, &data, &mut observation);
                });
                if observation.semantic_event {
                    self.last_semantic_event_at = Instant::now();
                }
                if observation.terminal {
                    self.terminal_seen = true;
                }
                if observation.starts_client_output {
                    self.context.mark_first_output();
                }
                for text in observation.texts {
                    self.token_tracker.add_output_text(&text).await;
                }
                self.write_log_once(None);
                Ok(None)
            }
        }
    }

    async fn next_upstream_item(&mut self) -> Result<Option<Result<Bytes, E>>, std::io::Error> {
        let Some(timeout) = self.semantic_timeout else {
            return Ok(self.upstream.next().await);
        };
        let elapsed = self.last_semantic_event_at.elapsed();
        if elapsed >= timeout {
            return Ok(Some(Ok(self.semantic_timeout_chunk(timeout))));
        }
        let remaining = timeout.saturating_sub(elapsed);
        match tokio::time::timeout(remaining, self.upstream.next()).await {
            Ok(item) => Ok(item),
            Err(_) => Ok(Some(Ok(self.semantic_timeout_chunk(timeout)))),
        }
    }

    fn semantic_timeout_chunk(&mut self, timeout: Duration) -> Bytes {
        let message = format!(
            "OpenAI Responses stream semantic timeout after {}s.",
            timeout.as_secs_f64()
        );
        tracing::warn!(
            path = %self.context.path,
            provider = %self.context.provider,
            upstream_id = %self.context.upstream_id,
            timeout_secs = timeout.as_secs_f64(),
            "OpenAI Responses stream semantic timeout"
        );
        self.terminal_seen = true;
        self.terminal_error = Some(message.clone());
        openai_error_done_chunk(&message)
    }

    fn openai_stream_semantics(&self) -> OpenAiStreamSemantics {
        openai_stream_semantics(&self.context.provider, &self.context.path)
    }
}

pub(super) fn stream_with_logging_and_model_override_semantic_timeout<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    model_override: String,
    token_tracker: RequestTokenTracker,
    semantic_timeout: Option<Duration>,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    let state = ModelOverrideStreamState::new(
        upstream,
        context,
        log,
        model_override,
        token_tracker,
        semantic_timeout,
    );
    try_unfold(state, |state| async move { state.step().await })
}

struct ModelOverrideStreamState<S> {
    upstream: S,
    parser: SseEventParser,
    collector: SseUsageCollector,
    log: Arc<LogWriter>,
    context: LogContext,
    token_tracker: RequestTokenTracker,
    out: VecDeque<Bytes>,
    model_override: String,
    upstream_ended: bool,
    logged: bool,
    terminal_seen: bool,
    terminal_error: Option<String>,
    semantic_timeout: Option<Duration>,
    last_semantic_event_at: Instant,
    response_body_buf: String,
}

impl<S> ModelOverrideStreamState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        let mut entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        attach_response_body(&mut entry, &self.response_body_buf);
        self.log.clone().write_detached(entry);
        self.logged = true;
    }

    fn write_terminal_log_once(&mut self) {
        let response_error = self.terminal_error.take();
        self.write_log_once(response_error);
    }
}

impl<S> Drop for ModelOverrideStreamState<S> {
    fn drop(&mut self) {
        // 和基础流一致：提前 drop 也必须落日志，避免“请求发生但无日志行”。
        if self.terminal_seen {
            self.write_terminal_log_once();
        } else {
            self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
        }
    }
}

impl<S, E> ModelOverrideStreamState<S>
where
    S: futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    fn new(
        upstream: S,
        context: LogContext,
        log: Arc<LogWriter>,
        model_override: String,
        token_tracker: RequestTokenTracker,
        semantic_timeout: Option<Duration>,
    ) -> Self {
        Self {
            upstream,
            parser: SseEventParser::new(),
            collector: SseUsageCollector::new(),
            log,
            context,
            token_tracker,
            out: VecDeque::new(),
            model_override,
            upstream_ended: false,
            logged: false,
            terminal_seen: false,
            terminal_error: None,
            semantic_timeout,
            last_semantic_event_at: Instant::now(),
            response_body_buf: String::new(),
        }
    }

    async fn step(mut self) -> Result<Option<(Bytes, Self)>, std::io::Error> {
        loop {
            if let Some(next) = self.out.pop_front() {
                self.context.mark_first_client_flush();
                return Ok(Some((next, self)));
            }
            if self.upstream_ended {
                self.write_log_once(None);
                return Ok(None);
            }
            if self.terminal_seen {
                self.write_terminal_log_once();
                return Ok(None);
            }

            match self.next_upstream_item().await? {
                Some(Ok(chunk)) => {
                    self.context.mark_upstream_first_byte();
                    self.collector.push_chunk(&chunk);
                    self.response_body_buf
                        .push_str(&String::from_utf8_lossy(chunk.as_ref()));
                    let mut events = Vec::new();
                    self.parser.push_chunk(&chunk, |data| events.push(data));
                    let mut observation = StreamObservation::default();
                    let saw_done = events.iter().any(|data| data.trim() == "[DONE]");
                    let semantics = self.openai_stream_semantics();
                    for data in events {
                        let mut event_observation = StreamObservation::default();
                        observe_stream_data(
                            semantics,
                            &self.context.provider,
                            &data,
                            &mut event_observation,
                        );
                        let should_synthesize_done = event_observation.terminal_json && !saw_done;
                        observation.merge(event_observation);
                        self.push_event_output(&data);
                        if should_synthesize_done {
                            self.out.push_back(Bytes::from("data: [DONE]\n\n"));
                        }
                    }
                    if observation.semantic_event {
                        self.last_semantic_event_at = Instant::now();
                    }
                    if observation.terminal {
                        self.terminal_seen = true;
                    }
                    if observation.starts_client_output {
                        self.context.mark_first_output();
                    }
                    for text in observation.texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                }
                Some(Err(err)) => {
                    self.write_log_once(None);
                    return Err(std::io::Error::new(std::io::ErrorKind::Other, err));
                }
                None => {
                    self.upstream_ended = true;
                    let mut events = Vec::new();
                    self.parser.finish(|data| events.push(data));
                    let mut observation = StreamObservation::default();
                    let saw_done = events.iter().any(|data| data.trim() == "[DONE]");
                    let semantics = self.openai_stream_semantics();
                    for data in events {
                        let mut event_observation = StreamObservation::default();
                        observe_stream_data(
                            semantics,
                            &self.context.provider,
                            &data,
                            &mut event_observation,
                        );
                        let should_synthesize_done = event_observation.terminal_json && !saw_done;
                        observation.merge(event_observation);
                        self.push_event_output(&data);
                        if should_synthesize_done {
                            self.out.push_back(Bytes::from("data: [DONE]\n\n"));
                        }
                    }
                    if observation.semantic_event {
                        self.last_semantic_event_at = Instant::now();
                    }
                    if observation.terminal {
                        self.terminal_seen = true;
                    }
                    if observation.starts_client_output {
                        self.context.mark_first_output();
                    }
                    for text in observation.texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                }
            }
        }
    }

    async fn next_upstream_item(&mut self) -> Result<Option<Result<Bytes, E>>, std::io::Error> {
        let Some(timeout) = self.semantic_timeout else {
            return Ok(self.upstream.next().await);
        };
        let elapsed = self.last_semantic_event_at.elapsed();
        if elapsed >= timeout {
            return Ok(Some(Ok(self.semantic_timeout_chunk(timeout))));
        }
        let remaining = timeout.saturating_sub(elapsed);
        match tokio::time::timeout(remaining, self.upstream.next()).await {
            Ok(item) => Ok(item),
            Err(_) => Ok(Some(Ok(self.semantic_timeout_chunk(timeout)))),
        }
    }

    fn semantic_timeout_chunk(&mut self, timeout: Duration) -> Bytes {
        let message = format!(
            "OpenAI Responses stream semantic timeout after {}s.",
            timeout.as_secs_f64()
        );
        tracing::warn!(
            path = %self.context.path,
            provider = %self.context.provider,
            upstream_id = %self.context.upstream_id,
            timeout_secs = timeout.as_secs_f64(),
            "OpenAI Responses stream semantic timeout"
        );
        self.terminal_seen = true;
        self.terminal_error = Some(message.clone());
        openai_error_done_chunk(&message)
    }

    fn openai_stream_semantics(&self) -> OpenAiStreamSemantics {
        openai_stream_semantics(&self.context.provider, &self.context.path)
    }

    fn push_event_output(&mut self, data: &str) {
        let output = rewrite_sse_data(data, &self.model_override);
        self.out
            .push_back(Bytes::from(format!("data: {output}\n\n")));
    }
}

#[derive(Clone, Copy, Default)]
struct OpenAiStreamSemantics {
    done_sentinel: bool,
    responses_events: bool,
}

fn rewrite_sse_data(data: &str, model_override: &str) -> String {
    if data == "[DONE]" {
        return data.to_string();
    }
    let bytes = Bytes::copy_from_slice(data.as_bytes());
    model::rewrite_response_model(&bytes, model_override)
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
        .unwrap_or_else(|| data.to_string())
}

fn observe_stream_data(
    semantics: OpenAiStreamSemantics,
    provider: &str,
    data: &str,
    observation: &mut StreamObservation,
) {
    if data.trim() == "[DONE]" {
        if semantics.done_sentinel {
            observation.semantic_event = true;
            observation.terminal = true;
            observation.saw_done = true;
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if semantics.responses_events {
        observation.semantic_event = true;
        if openai_stream_value_is_terminal(&value) {
            observation.terminal = true;
            observation.terminal_json = true;
        }
    }
    if openai_responses_data_starts_client_output(provider, &value) {
        observation.starts_client_output = true;
    }
    if let Some(text) = extract_stream_text_from_value(provider, &value) {
        if !text.is_empty() {
            observation.starts_client_output = true;
        }
        observation.texts.push(text);
    }
}

impl StreamObservation {
    fn merge(&mut self, next: StreamObservation) {
        self.starts_client_output |= next.starts_client_output;
        self.semantic_event |= next.semantic_event;
        self.terminal |= next.terminal;
        self.terminal_json |= next.terminal_json;
        self.saw_done |= next.saw_done;
        self.texts.extend(next.texts);
    }

    fn should_synthesize_done(&self) -> bool {
        self.terminal_json && !self.saw_done
    }
}

fn append_openai_done(chunk: Bytes) -> Bytes {
    let mut bytes = Vec::with_capacity(chunk.len() + b"data: [DONE]\n\n".len());
    bytes.extend_from_slice(chunk.as_ref());
    bytes.extend_from_slice(b"data: [DONE]\n\n");
    Bytes::from(bytes)
}

fn openai_error_done_chunk(message: &str) -> Bytes {
    let payload = json!({
        "type": "error",
        "error": {
            "type": "proxy_error",
            "message": message,
        }
    });
    Bytes::from(format!("data: {payload}\n\ndata: [DONE]\n\n"))
}

fn openai_stream_semantics(provider: &str, path: &str) -> OpenAiStreamSemantics {
    let done_sentinel = matches!(
        provider,
        PROVIDER_OPENAI | PROVIDER_OPENAI_RESPONSES | PROVIDER_CODEX
    );
    OpenAiStreamSemantics {
        done_sentinel,
        responses_events: done_sentinel && is_openai_responses_stream_path(path),
    }
}

fn is_openai_responses_stream_path(path: &str) -> bool {
    let path = path.split_once('?').map(|(path, _)| path).unwrap_or(path);
    path == "/v1/responses" || path == "/v1/responses/compact" || path.starts_with("/v1/responses/")
}

fn openai_stream_value_is_terminal(value: &Value) -> bool {
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return false;
    };
    matches!(
        event_type.trim(),
        "response.completed"
            | "response.done"
            | "response.failed"
            | "response.incomplete"
            | "response.cancelled"
            | "response.canceled"
            | "error"
    )
}

fn extract_stream_text_from_value(provider: &str, value: &Value) -> Option<String> {
    match provider {
        PROVIDER_OPENAI | PROVIDER_OPENAI_RESPONSES | PROVIDER_CODEX => {
            extract_openai_stream_text(value)
                .or_else(|| extract_openai_responses_delta_text(value))
                .or_else(|| {
                    if value.get("type").is_none() {
                        extract_fallback_stream_text(value)
                    } else {
                        None
                    }
                })
        }
        PROVIDER_ANTHROPIC => extract_anthropic_stream_text(value),
        PROVIDER_GEMINI => extract_gemini_stream_text(value),
        _ => extract_fallback_stream_text(value),
    }
}

fn openai_responses_data_starts_client_output(provider: &str, value: &Value) -> bool {
    if !matches!(provider, PROVIDER_OPENAI_RESPONSES | PROVIDER_CODEX) {
        return false;
    }
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return false;
    };
    !matches!(
        event_type.trim(),
        "" | "response.created" | "response.in_progress" | "response.failed" | "error"
    )
}

fn extract_openai_stream_text(value: &Value) -> Option<String> {
    let delta = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(Value::as_str);
    if let Some(delta) = delta {
        return Some(delta.to_string());
    }
    let event_type = value.get("type").and_then(Value::as_str)?;
    if event_type.ends_with("output_text.delta") {
        return value
            .get("delta")
            .and_then(Value::as_str)
            .map(|text| text.to_string());
    }
    None
}

fn extract_openai_responses_delta_text(value: &Value) -> Option<String> {
    // Responses final snapshot events can carry full text/arguments/code. Realtime
    // rate only counts incremental deltas to avoid double-counting completed frames.
    let event_type = value.get("type").and_then(Value::as_str)?;
    if !is_openai_responses_realtime_output_delta(event_type) {
        return None;
    }
    value
        .get("delta")
        .and_then(Value::as_str)
        .map(|text| text.to_string())
}

fn is_openai_responses_realtime_output_delta(event_type: &str) -> bool {
    event_type.ends_with(".delta")
        && matches!(
            event_type
                .strip_prefix("response.")
                .unwrap_or(event_type)
                .strip_suffix(".delta")
                .unwrap_or(event_type),
            "output_text"
                | "reasoning_text"
                | "reasoning_summary_text"
                | "refusal"
                | "function_call_arguments"
                | "mcp_call_arguments"
                | "custom_tool_call_input"
                | "code_interpreter_call_code"
        )
}

fn extract_anthropic_stream_text(value: &Value) -> Option<String> {
    if let Some(delta) = value.get("delta") {
        if let Some(text) = delta.get("text").and_then(Value::as_str) {
            return Some(text.to_string());
        }
        if let Some(text) = delta.as_str() {
            return Some(text.to_string());
        }
    }
    value
        .get("content_block")
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str)
        .map(|text| text.to_string())
}

fn extract_gemini_stream_text(value: &Value) -> Option<String> {
    let candidates = value.get("candidates").and_then(Value::as_array)?;
    for candidate in candidates {
        if let Some(content) = candidate.get("content") {
            if let Some(parts) = content.get("parts").and_then(Value::as_array) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        return Some(text.to_string());
                    }
                }
            }
        }
    }
    None
}

fn extract_fallback_stream_text(value: &Value) -> Option<String> {
    value
        .get("delta")
        .and_then(Value::as_str)
        .or_else(|| value.get("text").and_then(Value::as_str))
        .map(|text| text.to_string())
}
