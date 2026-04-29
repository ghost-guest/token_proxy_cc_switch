use axum::body::Bytes;
use futures_util::{stream::try_unfold, StreamExt};
use serde_json::Value;
use std::{collections::VecDeque, sync::Arc};

use super::super::log::{build_log_entry, LogContext, LogWriter};
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
    let state = LoggingStreamState::new(upstream, context, log, token_tracker);
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
}

#[derive(Default)]
struct StreamObservation {
    starts_client_output: bool,
    texts: Vec<String>,
}

impl<S> LoggingStreamState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        let entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        self.log.clone().write_detached(entry);
        self.logged = true;
    }
}

impl<S> Drop for LoggingStreamState<S> {
    fn drop(&mut self) {
        // 流被客户端提前取消时不会再进入 `None/Err` 分支，这里兜底保证日志至少落一行。
        self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
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
    ) -> Self {
        Self {
            upstream,
            collector: SseUsageCollector::new(),
            parser: SseEventParser::new(),
            log,
            context,
            token_tracker,
            logged: false,
        }
    }

    async fn step(mut self) -> Result<Option<(Bytes, Self)>, std::io::Error> {
        match self.upstream.next().await {
            Some(Ok(chunk)) => {
                self.context.mark_upstream_first_byte();
                self.collector.push_chunk(&chunk);
                let provider = self.context.provider.as_str();
                let mut observation = StreamObservation::default();
                self.parser.push_chunk(&chunk, |data| {
                    observe_stream_data(provider, &data, &mut observation);
                });
                if observation.starts_client_output {
                    self.context.mark_first_output();
                }
                for text in observation.texts {
                    self.token_tracker.add_output_text(&text).await;
                }
                self.context.mark_first_client_flush();
                Ok(Some((chunk, self)))
            }
            Some(Err(err)) => {
                let provider = self.context.provider.as_str();
                let mut observation = StreamObservation::default();
                self.parser.finish(|data| {
                    observe_stream_data(provider, &data, &mut observation);
                });
                if observation.starts_client_output {
                    self.context.mark_first_output();
                }
                for text in observation.texts {
                    self.token_tracker.add_output_text(&text).await;
                }
                self.write_log_once(None);
                Err(std::io::Error::new(std::io::ErrorKind::Other, err))
            }
            None => {
                let provider = self.context.provider.as_str();
                let mut observation = StreamObservation::default();
                self.parser.finish(|data| {
                    observe_stream_data(provider, &data, &mut observation);
                });
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
}

pub(super) fn stream_with_logging_and_model_override<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    model_override: String,
    token_tracker: RequestTokenTracker,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    let state =
        ModelOverrideStreamState::new(upstream, context, log, model_override, token_tracker);
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
}

impl<S> ModelOverrideStreamState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        let entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        self.log.clone().write_detached(entry);
        self.logged = true;
    }
}

impl<S> Drop for ModelOverrideStreamState<S> {
    fn drop(&mut self) {
        // 和基础流一致：提前 drop 也必须落日志，避免“请求发生但无日志行”。
        self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
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

            match self.upstream.next().await {
                Some(Ok(chunk)) => {
                    self.context.mark_upstream_first_byte();
                    self.collector.push_chunk(&chunk);
                    let mut events = Vec::new();
                    self.parser.push_chunk(&chunk, |data| events.push(data));
                    let mut observation = StreamObservation::default();
                    for data in events {
                        observe_stream_data(&self.context.provider, &data, &mut observation);
                        self.push_event_output(&data);
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
                    for data in events {
                        observe_stream_data(&self.context.provider, &data, &mut observation);
                        self.push_event_output(&data);
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

    fn push_event_output(&mut self, data: &str) {
        let output = rewrite_sse_data(data, &self.model_override);
        self.out
            .push_back(Bytes::from(format!("data: {output}\n\n")));
    }
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

fn observe_stream_data(provider: &str, data: &str, observation: &mut StreamObservation) {
    if data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
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

fn extract_stream_text_from_value(provider: &str, value: &Value) -> Option<String> {
    match provider {
        PROVIDER_OPENAI | PROVIDER_OPENAI_RESPONSES | PROVIDER_CODEX => {
            extract_openai_stream_text(value)
        }
        PROVIDER_ANTHROPIC => extract_anthropic_stream_text(value),
        PROVIDER_GEMINI => extract_gemini_stream_text(value),
        _ => None,
    }
    .or_else(|| extract_fallback_stream_text(value))
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
