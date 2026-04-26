use axum::{
    body::{Body, Bytes},
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::Response,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

use super::super::super::{
    codex_compat, http,
    log::{build_log_entry, LogContext, LogWriter, UsageSnapshot},
    model,
    openai_compat::{transform_response_body, FormatTransform},
    redact::redact_query_param_value,
    request_body::ReplayableBody,
    server_helpers::log_debug_headers_body,
    sse::SseEventParser,
    token_rate::RequestTokenTracker,
    usage::extract_usage_from_response,
};
use super::super::{
    kiro_to_anthropic, kiro_to_responses, token_count, upstream_read, upstream_stream,
    RetryableStreamResponse, PROVIDER_GEMINI, RESPONSE_ERROR_LIMIT_BYTES,
};

const DEBUG_BODY_LOG_LIMIT_BYTES: usize = usize::MAX;

pub(super) async fn build_buffered_response(
    status: StatusCode,
    upstream_res: reqwest::Response,
    mut headers: HeaderMap,
    context: LogContext,
    log: Arc<LogWriter>,
    request_tracker: RequestTokenTracker,
    response_transform: FormatTransform,
    model_override: Option<&str>,
    estimated_input_tokens: Option<u64>,
    upstream_no_data_timeout: Duration,
) -> Response {
    let mut context = context;
    let response_headers = upstream_res.headers().clone();
    let bytes =
        match read_upstream_bytes(upstream_res, &mut context, &log, upstream_no_data_timeout).await
        {
            Ok(bytes) => bytes,
            Err(response) => return response,
        };
    log_debug_headers_body(
        "upstream.response.raw",
        Some(&response_headers),
        Some(&ReplayableBody::from_bytes(bytes.clone())),
        DEBUG_BODY_LOG_LIMIT_BYTES,
    )
    .await;
    let bytes = if status.is_success() && is_event_stream_response(&response_headers) {
        match buffer_event_stream_response(&bytes) {
            Ok(buffered) => {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.remove(CONTENT_LENGTH);
                buffered
            }
            Err(message) => {
                let usage = UsageSnapshot {
                    usage: None,
                    cached_tokens: None,
                    usage_json: None,
                };
                return respond_transform_error(&mut context, usage, log, message);
            }
        }
    } else {
        bytes
    };
    let mut usage = extract_usage_from_response(&bytes);
    let response_error = response_error_for_status(status, &bytes);
    let request_body = context.request_body.clone();
    let output = if status.is_success() {
        match convert_success_body(
            response_transform,
            &bytes,
            &mut context,
            usage,
            log.clone(),
            estimated_input_tokens,
            request_body.as_deref(),
        ) {
            Ok(converted) => {
                usage = converted.usage;
                converted.output
            }
            Err(response) => return response,
        }
    } else {
        bytes
    };
    if let Some(message) =
        empty_chat_completion_retry_message(&output, &context, response_transform)
    {
        context.status = StatusCode::BAD_GATEWAY.as_u16();
        let entry = build_log_entry(&context, usage, Some(message.clone()));
        log.clone().write_detached(entry);
        let mut response = http::error_response(StatusCode::BAD_GATEWAY, &message);
        response.extensions_mut().insert(RetryableStreamResponse {
            message,
            should_cooldown: false,
        });
        return response;
    }

    let entry = build_log_entry(&context, usage, response_error);
    log.clone().write_detached(entry);

    let output = maybe_override_response_model(output, model_override);
    log_debug_headers_body(
        "outbound.response",
        Some(&headers),
        Some(&ReplayableBody::from_bytes(output.clone())),
        DEBUG_BODY_LOG_LIMIT_BYTES,
    )
    .await;
    let provider_for_tokens = provider_for_tokens(response_transform, context.provider.as_str());
    token_count::apply_output_tokens_from_response(&request_tracker, provider_for_tokens, &output)
        .await;

    http::build_response(status, headers, Body::from(output))
}

pub(super) fn buffer_event_stream_response(bytes: &Bytes) -> Result<Bytes, String> {
    let mut parser = SseEventParser::new();
    let mut events = Vec::new();
    parser.push_chunk(bytes.as_ref(), |event| events.push(event));
    parser.finish(|event| events.push(event));

    let mut chat_buffer = ChatCompletionBuffer::default();
    for event in events {
        if event == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(&event)
            .map_err(|err| format!("Invalid event-stream JSON payload: {err}"))?;
        if let Some(response) = completed_response_from_event(&value) {
            return serialize_buffered_event(response);
        }
        chat_buffer.push_event(&value);
    }

    if let Some(value) = chat_buffer.into_value() {
        return serialize_buffered_event(value);
    }

    Err("No supported event-stream payload found".to_string())
}

#[derive(Default)]
struct ChatCompletionBuffer {
    id: Option<String>,
    created: Option<Value>,
    model: Option<String>,
    role: Option<String>,
    content: String,
    finish_reason: Option<Value>,
    usage: Option<Value>,
    saw_chunk: bool,
}

impl ChatCompletionBuffer {
    fn push_event(&mut self, value: &Value) {
        let object = value.get("object").and_then(Value::as_str);
        let choices = value.get("choices").and_then(Value::as_array);
        if object != Some("chat.completion.chunk") && choices.is_none() {
            return;
        }

        let Some(choice) = choices.and_then(|items| items.first()) else {
            return;
        };
        self.saw_chunk = true;
        self.id = self
            .id
            .take()
            .or_else(|| value.get("id").and_then(Value::as_str).map(str::to_string));
        self.created = self
            .created
            .take()
            .or_else(|| value.get("created").cloned());
        self.model = self.model.take().or_else(|| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        self.usage = value.get("usage").filter(|usage| !usage.is_null()).cloned();
        if let Some(reason) = choice
            .get("finish_reason")
            .filter(|reason| !reason.is_null())
        {
            self.finish_reason = Some(reason.clone());
        }

        let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
            return;
        };
        if let Some(role) = delta.get("role").and_then(Value::as_str) {
            self.role = Some(role.to_string());
        }
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            self.content.push_str(content);
        }
    }

    fn into_value(self) -> Option<Value> {
        if !self.saw_chunk {
            return None;
        }

        let mut message = Map::new();
        message.insert(
            "role".to_string(),
            Value::String(self.role.unwrap_or_else(|| "assistant".to_string())),
        );
        message.insert("content".to_string(), Value::String(self.content));

        let mut choice = Map::new();
        choice.insert("index".to_string(), json!(0));
        choice.insert("message".to_string(), Value::Object(message));
        choice.insert(
            "finish_reason".to_string(),
            self.finish_reason.unwrap_or(Value::Null),
        );

        let mut output = Map::new();
        output.insert(
            "id".to_string(),
            Value::String(self.id.unwrap_or_else(|| "chatcmpl_buffered".to_string())),
        );
        output.insert(
            "object".to_string(),
            Value::String("chat.completion".to_string()),
        );
        output.insert(
            "created".to_string(),
            self.created.unwrap_or_else(|| json!(0)),
        );
        output.insert(
            "model".to_string(),
            Value::String(self.model.unwrap_or_else(|| "unknown".to_string())),
        );
        output.insert(
            "choices".to_string(),
            Value::Array(vec![Value::Object(choice)]),
        );
        if let Some(usage) = self.usage {
            output.insert("usage".to_string(), usage);
        }
        Some(Value::Object(output))
    }
}

fn completed_response_from_event(value: &Value) -> Option<Value> {
    let event_type = value.get("type").and_then(Value::as_str)?;
    let is_terminal_response = matches!(
        event_type,
        "response.completed"
            | "response.incomplete"
            | "response.cancelled"
            | "response.canceled"
            | "response.failed"
    );
    if !is_terminal_response {
        return None;
    }
    value
        .get("response")
        .filter(|response| response.is_object())
        .cloned()
}

fn serialize_buffered_event(value: Value) -> Result<Bytes, String> {
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|err| format!("Failed to serialize buffered event-stream payload: {err}"))
}

fn is_event_stream_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value.split(';').next().is_some_and(|content_type| {
                content_type
                    .trim()
                    .eq_ignore_ascii_case("text/event-stream")
            })
        })
        .unwrap_or(false)
}

struct ConvertedBody {
    output: Bytes,
    usage: UsageSnapshot,
}

fn convert_success_body(
    transform: FormatTransform,
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    estimated_input_tokens: Option<u64>,
    request_body: Option<&str>,
) -> Result<ConvertedBody, Response> {
    match transform {
        FormatTransform::KiroToAnthropic => {
            convert_kiro_to_anthropic_body(bytes, context, usage, log, estimated_input_tokens)
        }
        FormatTransform::CodexToChat => {
            convert_codex_to_chat_body(bytes, context, usage, log, request_body)
        }
        FormatTransform::CodexToResponses => {
            convert_codex_to_responses_body(bytes, context, usage, log, request_body)
        }
        FormatTransform::CodexToAnthropic => {
            convert_codex_to_anthropic_body(bytes, context, usage, log, request_body)
        }
        _ if transform != FormatTransform::None => {
            convert_generic_body(transform, bytes, context, usage, log)
        }
        _ => Ok(ConvertedBody {
            output: bytes.clone(),
            usage,
        }),
    }
}

fn convert_kiro_to_anthropic_body(
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    estimated_input_tokens: Option<u64>,
) -> Result<ConvertedBody, Response> {
    let converted = match kiro_to_anthropic::convert_kiro_response(
        bytes,
        context.model.as_deref(),
        estimated_input_tokens,
    ) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    let usage = resolve_kiro_usage(
        bytes,
        &converted,
        context.model.as_deref(),
        estimated_input_tokens,
    );
    Ok(ConvertedBody {
        output: converted,
        usage,
    })
}

fn convert_codex_to_chat_body(
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    request_body: Option<&str>,
) -> Result<ConvertedBody, Response> {
    let converted = match codex_compat::codex_response_to_chat(bytes, request_body) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    Ok(ConvertedBody {
        output: converted,
        usage,
    })
}

fn convert_codex_to_responses_body(
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    request_body: Option<&str>,
) -> Result<ConvertedBody, Response> {
    let converted = match codex_compat::codex_response_to_responses(bytes, request_body) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    Ok(ConvertedBody {
        output: converted,
        usage,
    })
}

fn convert_codex_to_anthropic_body(
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    request_body: Option<&str>,
) -> Result<ConvertedBody, Response> {
    let responses = match codex_compat::codex_response_to_responses(bytes, request_body) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    let anthropic = match transform_response_body(
        FormatTransform::ResponsesToAnthropic,
        &responses,
        context.model.as_deref(),
    ) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    Ok(ConvertedBody {
        output: anthropic,
        usage,
    })
}

fn convert_generic_body(
    transform: FormatTransform,
    bytes: &Bytes,
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
) -> Result<ConvertedBody, Response> {
    let converted = match transform_response_body(transform, bytes, context.model.as_deref()) {
        Ok(converted) => converted,
        Err(message) => {
            return Err(respond_transform_error(context, usage, log, message));
        }
    };
    Ok(ConvertedBody {
        output: converted,
        usage,
    })
}

pub(super) fn empty_chat_completion_retry_message(
    bytes: &Bytes,
    context: &LogContext,
    transform: FormatTransform,
) -> Option<String> {
    if context.path != "/v1/chat/completions" || !produces_chat_completion(transform) {
        return None;
    }

    let value: Value = serde_json::from_slice(bytes).ok()?;
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)?;
    if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
        return None;
    }

    let message = choice.get("message").and_then(Value::as_object)?;
    if !value_is_absent(message.get("content"))
        || !value_is_absent(message.get("reasoning_content"))
        || !value_is_absent(message.get("tool_calls"))
        || !value_is_absent(message.get("refusal"))
        || !value_is_absent(message.get("audio"))
    {
        return None;
    }

    if message
        .get("annotations")
        .is_some_and(|value| value.as_array().is_none_or(|items| !items.is_empty()))
    {
        return None;
    }

    Some("Upstream returned empty chat completion content for stop response.".to_string())
}

fn produces_chat_completion(transform: FormatTransform) -> bool {
    matches!(
        transform,
        FormatTransform::None
            | FormatTransform::ResponsesToChat
            | FormatTransform::AnthropicToChat
            | FormatTransform::GeminiToChat
            | FormatTransform::CodexToChat
    )
}

pub(super) fn value_is_absent(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(items)) => items.is_empty(),
        _ => false,
    }
}

async fn read_upstream_bytes(
    upstream_res: reqwest::Response,
    context: &mut LogContext,
    log: &Arc<LogWriter>,
    upstream_no_data_timeout: Duration,
) -> Result<Bytes, Response> {
    let bytes = match upstream_read::read_upstream_bytes_with_ttfb(
        upstream_res,
        context,
        upstream_no_data_timeout,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(err) => {
            let (status, message) = match err {
                upstream_stream::UpstreamStreamError::IdleTimeout(_) => (
                    StatusCode::GATEWAY_TIMEOUT,
                    format!(
                        "Upstream response timed out after {}s.",
                        upstream_no_data_timeout.as_secs()
                    ),
                ),
                upstream_stream::UpstreamStreamError::Upstream(err) => {
                    let raw = err.to_string();
                    let message = if context.provider == PROVIDER_GEMINI {
                        redact_query_param_value(&raw, "key")
                    } else {
                        raw
                    };
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("Failed to read upstream response: {message}"),
                    )
                }
            };
            context.status = status.as_u16();
            let empty_usage = UsageSnapshot {
                usage: None,
                cached_tokens: None,
                usage_json: None,
            };
            let entry = build_log_entry(context, empty_usage, Some(message.clone()));
            log.clone().write_detached(entry);
            return Err(http::error_response(status, message));
        }
    };
    Ok(bytes)
}

fn respond_transform_error(
    context: &mut LogContext,
    usage: UsageSnapshot,
    log: Arc<LogWriter>,
    message: String,
) -> Response {
    let error_message = format!("Failed to transform upstream response: {message}");
    context.status = StatusCode::BAD_GATEWAY.as_u16();
    let entry = build_log_entry(context, usage, Some(error_message.clone()));
    log.clone().write_detached(entry);
    http::error_response(StatusCode::BAD_GATEWAY, error_message)
}

fn resolve_kiro_usage(
    raw_bytes: &Bytes,
    responses_bytes: &Bytes,
    model: Option<&str>,
    estimated_input_tokens: Option<u64>,
) -> UsageSnapshot {
    let usage = extract_usage_from_response(responses_bytes);
    if usage.usage.is_none() && usage.cached_tokens.is_none() && usage.usage_json.is_none() {
        if let Some(fallback) =
            kiro_to_responses::extract_kiro_usage_snapshot(raw_bytes, model, estimated_input_tokens)
        {
            return fallback;
        }
    }
    usage
}

fn maybe_override_response_model(bytes: Bytes, model_override: Option<&str>) -> Bytes {
    let Some(model_override) = model_override else {
        return bytes;
    };
    model::rewrite_response_model(&bytes, model_override).unwrap_or(bytes)
}

fn response_error_text(bytes: &Bytes) -> String {
    let slice = bytes.as_ref();
    if slice.len() <= RESPONSE_ERROR_LIMIT_BYTES {
        return String::from_utf8_lossy(slice).to_string();
    }
    let truncated = &slice[..RESPONSE_ERROR_LIMIT_BYTES];
    format!("{}... (truncated)", String::from_utf8_lossy(truncated))
}

fn response_error_for_status(status: StatusCode, bytes: &Bytes) -> Option<String> {
    if status.is_client_error() || status.is_server_error() {
        Some(response_error_text(bytes))
    } else {
        None
    }
}

fn provider_for_tokens(transform: FormatTransform, provider: &str) -> &str {
    match transform {
        FormatTransform::KiroToAnthropic => "anthropic",
        FormatTransform::CodexToChat => "openai",
        FormatTransform::CodexToResponses => "openai-response",
        FormatTransform::CodexToAnthropic => "anthropic",
        _ => provider,
    }
}
