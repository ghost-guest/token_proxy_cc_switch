use axum::body::Bytes;
use futures_util::{stream::try_unfold, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::super::log::{attach_response_body, build_log_entry, LogContext, LogWriter};
use super::super::response::STREAM_DROPPED_ERROR;
use super::super::sse::SseEventParser;
use super::super::token_rate::RequestTokenTracker;
use super::super::usage::SseUsageCollector;
use super::extract_tool_name_map_from_request_body;

const CODEX_STREAM_INVALID_EVENT_LIMIT: usize = 1024;

pub(crate) enum CodexPreludeDecision {
    Pending,
    RetryableError(String),
    ReadyForPassThrough,
}

pub(crate) struct CodexPreludeInspector {
    parser: SseEventParser,
}

impl CodexPreludeInspector {
    pub(crate) fn new() -> Self {
        Self {
            parser: SseEventParser::new(),
        }
    }

    pub(crate) fn inspect_chunk(&mut self, chunk: &[u8]) -> CodexPreludeDecision {
        let mut events = Vec::new();
        self.parser.push_chunk(chunk, |data| events.push(data));
        let mut saw_releasable_preamble = false;
        for data in events {
            let decision = inspect_codex_prelude_event(&data);
            match decision {
                CodexPreludeDecision::Pending => {
                    saw_releasable_preamble = true;
                }
                CodexPreludeDecision::RetryableError(message) => {
                    return CodexPreludeDecision::RetryableError(message);
                }
                CodexPreludeDecision::ReadyForPassThrough => {
                    return CodexPreludeDecision::ReadyForPassThrough;
                }
            }
        }
        if saw_releasable_preamble {
            return CodexPreludeDecision::ReadyForPassThrough;
        }
        CodexPreludeDecision::Pending
    }
}

fn inspect_codex_prelude_event(data: &str) -> CodexPreludeDecision {
    if data == "[DONE]" {
        return CodexPreludeDecision::ReadyForPassThrough;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return CodexPreludeDecision::RetryableError(invalid_event_message(data));
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.created" | "response.in_progress") => CodexPreludeDecision::Pending,
        Some("response.failed" | "error") => {
            CodexPreludeDecision::RetryableError(stream_error_message(&value))
        }
        Some(_) => CodexPreludeDecision::ReadyForPassThrough,
        None => CodexPreludeDecision::RetryableError(malformed_event_message(&value)),
    }
}

pub(crate) fn stream_codex_to_chat<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    token_tracker: RequestTokenTracker,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    let state = CodexToChatState::new(upstream, context, log, token_tracker);
    try_unfold(state, |state| async move { state.step().await })
}

struct CodexToChatState<S> {
    upstream: S,
    parser: SseEventParser,
    collector: SseUsageCollector,
    log: Arc<LogWriter>,
    context: LogContext,
    token_tracker: RequestTokenTracker,
    out: VecDeque<Bytes>,
    response_id: String,
    created: i64,
    model: String,
    function_call_index: i64,
    finish_reason: Option<&'static str>,
    sent_done: bool,
    logged: bool,
    upstream_ended: bool,
    tool_name_map: HashMap<String, String>,
    response_body_buf: String,
}

impl<S> CodexToChatState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        self.logged = true;
        let mut entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        attach_response_body(&mut entry, &self.response_body_buf);
        self.log.clone().write_detached(entry);
    }
}

impl<S> Drop for CodexToChatState<S> {
    fn drop(&mut self) {
        self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
    }
}

impl<S, E> CodexToChatState<S>
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
        let now_ms = now_unix_seconds();
        let response_id = format!("chatcmpl_proxy_{now_ms}");
        let model = context
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let tool_name_map =
            extract_tool_name_map_from_request_body(context.request_body.as_deref());

        Self {
            upstream,
            parser: SseEventParser::new(),
            collector: SseUsageCollector::new(),
            log,
            token_tracker,
            context,
            out: VecDeque::new(),
            response_id,
            created: now_ms,
            model,
            function_call_index: -1,
            finish_reason: None,
            sent_done: false,
            logged: false,
            upstream_ended: false,
            tool_name_map,
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
                return Ok(None);
            }

            match self.upstream.next().await {
                Some(Ok(chunk)) => {
                    self.context.mark_upstream_first_byte();
                    self.collector.push_chunk(&chunk);
                    self.response_body_buf
                        .push_str(&String::from_utf8_lossy(chunk.as_ref()));
                    let mut events = Vec::new();
                    self.parser.push_chunk(&chunk, |data| events.push(data));
                    let mut texts = Vec::new();
                    for data in events {
                        self.handle_event(&data, &mut texts);
                    }
                    for text in texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                }
                Some(Err(err)) => {
                    self.log_usage_once();
                    return Err(std::io::Error::new(std::io::ErrorKind::Other, err));
                }
                None => {
                    self.upstream_ended = true;
                    let mut events = Vec::new();
                    self.parser.finish(|data| events.push(data));
                    let mut texts = Vec::new();
                    for data in events {
                        self.handle_event(&data, &mut texts);
                    }
                    for text in texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                    if !self.sent_done {
                        self.push_done();
                    }
                    self.log_usage_once();
                    if self.out.is_empty() {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, data: &str, token_texts: &mut Vec<String>) {
        if self.sent_done {
            return;
        }
        if data == "[DONE]" {
            self.push_done();
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            self.fail_stream(invalid_event_message(data));
            return;
        };
        let Some(event_type) = value.get("type").and_then(Value::as_str) else {
            self.fail_stream(malformed_event_message(&value));
            return;
        };

        match event_type {
            "response.created" => {
                self.update_from_created(&value);
                self.push_preamble_keepalive();
            }
            "response.in_progress" => {
                self.push_preamble_keepalive();
            }
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    if !delta.is_empty() {
                        self.context.mark_first_output();
                    }
                    token_texts.push(delta.to_string());
                    self.push_chunk(json!({ "role": "assistant", "content": delta }));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    if !delta.is_empty() {
                        self.context.mark_first_output();
                    }
                    token_texts.push(delta.to_string());
                    self.push_chunk(json!({ "role": "assistant", "reasoning_content": delta }));
                }
            }
            "response.reasoning_summary_text.done" => {
                self.push_chunk(json!({ "role": "assistant", "reasoning_content": "\n\n" }));
            }
            "response.output_item.done" => {
                self.handle_output_item_done(&value, token_texts);
            }
            "response.completed" => {
                self.finish_reason = Some(self.resolve_finish_reason());
            }
            "response.failed" | "error" => {
                self.fail_stream(stream_error_message(&value));
            }
            _ => {}
        }
    }

    fn handle_output_item_done(&mut self, value: &Value, token_texts: &mut Vec<String>) {
        self.handle_function_call_item(value);
        self.handle_image_generation_item(value, token_texts);
    }

    fn update_from_created(&mut self, value: &Value) {
        if let Some(response) = value.get("response").and_then(Value::as_object) {
            if let Some(id) = response.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    self.response_id = id.to_string();
                }
            }
            if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
                self.created = created;
            }
            if let Some(model) = response.get("model").and_then(Value::as_str) {
                if !model.is_empty() {
                    self.model = model.to_string();
                }
            }
        }
    }

    fn handle_function_call_item(&mut self, value: &Value) {
        let Some(item) = value.get("item").and_then(Value::as_object) else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return;
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let restored = self
            .tool_name_map
            .get(name)
            .map(String::as_str)
            .unwrap_or(name);
        let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
        let id = item
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .unwrap_or("call_proxy");
        self.function_call_index += 1;
        let tool_call = json!({
            "index": self.function_call_index,
            "id": id,
            "type": "function",
            "function": { "name": restored, "arguments": arguments }
        });
        self.context.mark_first_output();
        self.push_chunk(json!({ "role": "assistant", "tool_calls": [tool_call] }));
    }

    fn handle_image_generation_item(&mut self, value: &Value, token_texts: &mut Vec<String>) {
        let Some(item) = value.get("item").and_then(Value::as_object) else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            return;
        }
        let Some(text) = image_generation_call_text(item) else {
            return;
        };
        self.context.mark_first_output();
        token_texts.push(text.clone());
        self.push_chunk(json!({ "role": "assistant", "content": text }));
    }

    fn push_preamble_keepalive(&mut self) {
        self.out
            .push_back(Bytes::from(": token-proxy-codex-preamble\n\n"));
    }

    fn push_chunk(&mut self, delta: Value) {
        let chunk = chat_chunk_sse(&self.response_id, self.created, &self.model, delta, None);
        self.out.push_back(chunk);
    }

    fn push_done(&mut self) {
        if self.sent_done {
            return;
        }
        let finish = self
            .finish_reason
            .unwrap_or_else(|| self.resolve_finish_reason());
        let done = chat_chunk_sse(
            &self.response_id,
            self.created,
            &self.model,
            json!({}),
            Some(finish),
        );
        self.out.push_back(done);
        self.out.push_back(Bytes::from("data: [DONE]\n\n"));
        self.sent_done = true;
    }

    fn fail_stream(&mut self, message: String) {
        self.context.status = 502;
        self.out.push_back(stream_chat_error_sse(&message));
        self.push_done();
        self.write_log_once(Some(message));
    }

    fn resolve_finish_reason(&self) -> &'static str {
        if self.function_call_index >= 0 {
            "tool_calls"
        } else {
            "stop"
        }
    }

    fn log_usage_once(&mut self) {
        self.write_log_once(None);
    }
}

pub(crate) fn stream_codex_to_responses<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    token_tracker: RequestTokenTracker,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    stream_codex_to_responses_with_semantic_timeout(upstream, context, log, token_tracker, None)
}

pub(crate) fn stream_codex_to_responses_with_semantic_timeout<E>(
    upstream: impl futures_util::stream::Stream<Item = Result<Bytes, E>> + Unpin + Send + 'static,
    context: LogContext,
    log: Arc<LogWriter>,
    token_tracker: RequestTokenTracker,
    semantic_timeout: Option<Duration>,
) -> impl futures_util::stream::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    E: std::error::Error + Send + Sync + 'static,
{
    let state = CodexToResponsesState::new(upstream, context, log, token_tracker, semantic_timeout);
    try_unfold(state, |state| async move { state.step().await })
}

struct CodexToResponsesState<S> {
    upstream: S,
    parser: SseEventParser,
    collector: SseUsageCollector,
    log: Arc<LogWriter>,
    context: LogContext,
    token_tracker: RequestTokenTracker,
    out: VecDeque<Bytes>,
    sent_done: bool,
    logged: bool,
    upstream_ended: bool,
    tool_name_map: HashMap<String, String>,
    response_id: String,
    created: i64,
    model: String,
    saw_terminal_event: bool,
    response_error_override: Option<String>,
    response_body_buf: String,
    semantic_timeout: Option<Duration>,
    last_semantic_event_at: Instant,
}

impl<S> CodexToResponsesState<S> {
    fn write_log_once(&mut self, response_error: Option<String>) {
        if self.logged {
            return;
        }
        self.logged = true;
        let mut entry = build_log_entry(&self.context, self.collector.finish(), response_error);
        attach_response_body(&mut entry, &self.response_body_buf);
        self.log.clone().write_detached(entry);
    }
}

impl<S> Drop for CodexToResponsesState<S> {
    fn drop(&mut self) {
        self.write_log_once(Some(STREAM_DROPPED_ERROR.to_string()));
    }
}

impl<S, E> CodexToResponsesState<S>
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
        let now_seconds = now_unix_seconds();
        let tool_name_map =
            extract_tool_name_map_from_request_body(context.request_body.as_deref());
        let model = context
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        Self {
            upstream,
            parser: SseEventParser::new(),
            collector: SseUsageCollector::new(),
            log,
            token_tracker,
            context,
            out: VecDeque::new(),
            sent_done: false,
            logged: false,
            upstream_ended: false,
            tool_name_map,
            response_id: format!("resp_proxy_{now_seconds}"),
            created: now_seconds,
            model,
            saw_terminal_event: false,
            response_error_override: None,
            response_body_buf: String::new(),
            semantic_timeout,
            last_semantic_event_at: Instant::now(),
        }
    }

    async fn step(mut self) -> Result<Option<(Bytes, Self)>, std::io::Error> {
        loop {
            if let Some(next) = self.out.pop_front() {
                self.context.mark_first_client_flush();
                return Ok(Some((next, self)));
            }
            if self.upstream_ended {
                return Ok(None);
            }
            if self.sent_done {
                self.log_usage_once();
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
                    let had_events = !events.is_empty();
                    let mut texts = Vec::new();
                    for data in events {
                        self.handle_event(&data, &mut texts);
                    }
                    if !had_events {
                        self.push_semantic_timeout_if_due();
                    }
                    for text in texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                }
                Some(Err(err)) => {
                    self.log_usage_once();
                    return Err(std::io::Error::new(std::io::ErrorKind::Other, err));
                }
                None => {
                    self.upstream_ended = true;
                    let mut events = Vec::new();
                    self.parser.finish(|data| events.push(data));
                    let mut texts = Vec::new();
                    for data in events {
                        self.handle_event(&data, &mut texts);
                    }
                    for text in texts {
                        self.token_tracker.add_output_text(&text).await;
                    }
                    if !self.sent_done {
                        self.push_compatible_incomplete_terminal();
                        self.out.push_back(Bytes::from("data: [DONE]\n\n"));
                        self.sent_done = true;
                    }
                    self.log_usage_once();
                    if self.out.is_empty() {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, data: &str, token_texts: &mut Vec<String>) {
        if self.sent_done {
            return;
        }
        if data == "[DONE]" {
            self.push_compatible_incomplete_terminal();
            self.out.push_back(Bytes::from("data: [DONE]\n\n"));
            self.sent_done = true;
            return;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(data) else {
            self.fail_stream(invalid_event_message(data));
            return;
        };
        if matches!(
            value.get("type").and_then(Value::as_str),
            Some("response.failed" | "error")
        ) {
            self.fail_stream(stream_error_message(&value));
            return;
        }
        let Some(event_type) = value.get("type").and_then(Value::as_str) else {
            self.fail_stream(malformed_event_message(&value));
            return;
        };
        self.last_semantic_event_at = Instant::now();
        if event_type == "response.created" {
            self.update_from_created(&value);
        }
        if is_responses_terminal_event(event_type) {
            self.saw_terminal_event = true;
        }
        restore_tool_names_in_event(&mut value, &self.tool_name_map);
        if is_codex_business_output_event(&value) {
            self.context.mark_first_output();
        }
        if let Some(delta) = extract_output_token_delta(&value) {
            token_texts.push(delta.to_string());
        }
        self.out
            .push_back(Bytes::from(format!("data: {}\n\n", value.to_string())));
        if self.saw_terminal_event {
            self.out.push_back(Bytes::from("data: [DONE]\n\n"));
            self.sent_done = true;
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
        self.response_error_override = Some(message.clone());
        stream_responses_error_done_sse(&message)
    }

    fn push_semantic_timeout_if_due(&mut self) {
        if self.sent_done {
            return;
        }
        let Some(timeout) = self.semantic_timeout else {
            return;
        };
        if self.last_semantic_event_at.elapsed() < timeout {
            return;
        }
        let chunk = self.semantic_timeout_chunk(timeout);
        self.sent_done = true;
        self.out.push_back(chunk);
    }

    fn log_usage_once(&mut self) {
        self.write_log_once(self.response_error_override.clone());
    }

    fn update_from_created(&mut self, value: &Value) {
        let Some(response) = value.get("response").and_then(Value::as_object) else {
            return;
        };
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                self.response_id = id.to_string();
            }
        }
        if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
            self.created = created;
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            if !model.is_empty() {
                self.model = model.to_string();
            }
        }
    }

    fn push_compatible_incomplete_terminal(&mut self) {
        if self.saw_terminal_event {
            return;
        }
        self.saw_terminal_event = true;
        self.response_error_override =
            Some("Codex upstream stream disconnected before response.completed".to_string());
        self.out
            .push_back(stream_responses_compatible_incomplete_sse(
                &self.response_id,
                self.created,
                &self.model,
            ));
    }

    fn fail_stream(&mut self, message: String) {
        self.context.status = 502;
        self.out.push_back(stream_responses_error_sse(&message));
        self.out.push_back(Bytes::from("data: [DONE]\n\n"));
        self.sent_done = true;
        self.write_log_once(Some(message));
    }
}

fn invalid_event_message(data: &str) -> String {
    format!(
        "Codex upstream emitted invalid JSON stream event: {}",
        truncate_event_text(data)
    )
}

fn malformed_event_message(value: &Value) -> String {
    format!(
        "Codex upstream emitted malformed stream event: {}",
        truncate_event_text(&value.to_string())
    )
}

fn stream_error_message(value: &Value) -> String {
    if let Some(error) = value.get("error") {
        return format!(
            "Codex upstream stream failed: {}",
            error_value_message(error)
        );
    }
    if let Some(message) = value.get("message") {
        return format!(
            "Codex upstream stream failed: {}",
            error_value_message(message)
        );
    }
    format!(
        "Codex upstream stream failed: {}",
        truncate_event_text(&value.to_string())
    )
}

fn error_value_message(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| value.as_str().map(ToString::to_string))
        .unwrap_or_else(|| value.to_string())
}

fn truncate_event_text(text: &str) -> String {
    if text.len() <= CODEX_STREAM_INVALID_EVENT_LIMIT {
        return text.trim().to_string();
    }
    let end = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= CODEX_STREAM_INVALID_EVENT_LIMIT)
        .last()
        .unwrap_or(CODEX_STREAM_INVALID_EVENT_LIMIT);
    format!("{}... (truncated)", text[..end].trim())
}

pub(crate) fn stream_chat_error_sse(message: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "error": {
                "message": message,
                "type": "proxy_error"
            }
        })
    ))
}

pub(crate) fn stream_responses_error_sse(message: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "type": "error",
            "error": {
                "message": message,
                "type": "proxy_error"
            }
        })
    ))
}

fn stream_responses_error_done_sse(message: &str) -> Bytes {
    let mut bytes = stream_responses_error_sse(message).to_vec();
    bytes.extend_from_slice(b"data: [DONE]\n\n");
    Bytes::from(bytes)
}

fn stream_responses_compatible_incomplete_sse(id: &str, created: i64, model: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "type": "response.completed",
            "response": {
                "id": id,
                "object": "response",
                "created_at": created,
                "model": model,
                "status": "incomplete",
                "incomplete_details": {
                    "reason": "error"
                }
            }
        })
    ))
}

fn restore_tool_names_in_event(value: &mut Value, tool_name_map: &HashMap<String, String>) {
    if tool_name_map.is_empty() {
        return;
    }
    if let Some(item) = value.get_mut("item").and_then(Value::as_object_mut) {
        restore_tool_names_in_item(item, tool_name_map);
    }
    if let Some(response) = value.get_mut("response").and_then(Value::as_object_mut) {
        restore_tool_names_in_response(response, tool_name_map);
    }
    if let Some(response) = value.as_object_mut() {
        restore_tool_names_in_response(response, tool_name_map);
    }
}

fn restore_tool_names_in_response(
    response: &mut Map<String, Value>,
    tool_name_map: &HashMap<String, String>,
) {
    let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    for item in output {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        restore_tool_names_in_item(item, tool_name_map);
    }
}

fn restore_tool_names_in_item(
    item: &mut Map<String, Value>,
    tool_name_map: &HashMap<String, String>,
) {
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let Some(name) = item.get("name").and_then(Value::as_str) else {
        return;
    };
    let Some(restored) = tool_name_map.get(name) else {
        return;
    };
    item.insert("name".to_string(), Value::String(restored.clone()));
}

fn extract_output_token_delta(value: &Value) -> Option<&str> {
    // Realtime token rate only counts incremental Responses events. Final snapshot
    // events may also carry text/arguments/code and would double-count prior deltas.
    let event_type = value.get("type").and_then(Value::as_str)?;
    if !is_realtime_output_delta_event(event_type) {
        return None;
    }
    value.get("delta").and_then(Value::as_str)
}

fn is_realtime_output_delta_event(event_type: &str) -> bool {
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

fn is_codex_business_output_event(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta" | "response.reasoning_summary_text.delta") => value
            .get("delta")
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty()),
        Some("response.output_item.done") => value
            .get("item")
            .and_then(Value::as_object)
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|item_type| {
                matches!(item_type, "function_call" | "image_generation_call")
            }),
        _ => false,
    }
}

fn image_generation_call_text(item: &Map<String, Value>) -> Option<String> {
    if let Some(result) = item.get("result").and_then(Value::as_str) {
        if !result.trim().is_empty() {
            return Some(format!(
                "![generated image](data:image/png;base64,{result})"
            ));
        }
    }
    if let Some(url) = item.get("url").and_then(Value::as_str) {
        if !url.trim().is_empty() {
            return Some(format!("![generated image]({url})"));
        }
    }
    None
}

fn is_responses_terminal_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.completed"
            | "response.incomplete"
            | "response.cancelled"
            | "response.canceled"
            | "response.failed"
    )
}

fn chat_chunk_sse(
    id: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
) -> Bytes {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }
        ]
    });
    Bytes::from(format!("data: {}\n\n", chunk.to_string()))
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
