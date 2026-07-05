use axum::{body::Bytes, http::StatusCode};
use serde_json::{Map, Value};

use super::super::{
    config::UpstreamRuntime, http, model, request_body::ReplayableBody, RequestMeta,
};
use super::{request::split_path_query, AttemptOutcome};

const OPENAI_CHAT_PATH: &str = "/v1/chat/completions";
const OPENAI_RESPONSES_PATH: &str = "/v1/responses";
const ANTHROPIC_COUNT_TOKENS_PATH: &str = "/v1/messages/count_tokens";
const REQUEST_MODEL_MAPPING_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_REASONING_LIMIT_BYTES: usize = 100 * 1024 * 1024;
const REQUEST_FILTER_LIMIT_BYTES: usize = 20 * 1024 * 1024;
const CODEX_INSTALLATION_ID_KEY: &str = "x-codex-installation-id";

pub(super) async fn build_upstream_body(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path_with_query: &str,
    body: &ReplayableBody,
    meta: &RequestMeta,
    codex_openai_device_id: Option<&str>,
) -> Result<reqwest::Body, AttemptOutcome> {
    let transformed = build_json_transformed_body(
        provider,
        upstream,
        upstream_path_with_query,
        body,
        meta,
        codex_openai_device_id,
    )
    .await?;
    let final_source = transformed.as_ref().unwrap_or(body);
    final_source.to_reqwest_body().await.map_err(|err| {
        AttemptOutcome::Fatal(http::error_response(
            StatusCode::BAD_GATEWAY,
            format!("Failed to read cached request body: {err}"),
        ))
    })
}

async fn build_json_transformed_body(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path_with_query: &str,
    body: &ReplayableBody,
    meta: &RequestMeta,
    codex_openai_device_id: Option<&str>,
) -> Result<Option<ReplayableBody>, AttemptOutcome> {
    let upstream_path = split_path_query(upstream_path_with_query).0;
    if !needs_json_transform(
        provider,
        upstream,
        upstream_path,
        meta,
        codex_openai_device_id,
    ) {
        return Ok(None);
    }

    let must_strip_sampling =
        should_strip_openai_responses_sampling_params(provider, upstream_path, meta);
    let read_limit = json_transform_read_limit(
        provider,
        upstream,
        upstream_path,
        meta,
        codex_openai_device_id,
    );
    let Some(bytes) = body.read_bytes_if_small(read_limit).await.map_err(|err| {
        AttemptOutcome::Fatal(http::error_response(
            StatusCode::BAD_GATEWAY,
            format!("Failed to read cached request body: {err}"),
        ))
    })?
    else {
        if must_strip_sampling {
            return Err(openai_responses_sampling_params_payload_too_large());
        }
        return Ok(None);
    };

    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };

    let mut changed = false;
    let body_len = bytes.len();
    changed |= rewrite_model_mapping(object, meta, body_len);
    changed |= apply_reasoning_effort(provider, upstream_path, object, meta, body_len);
    changed |= filter_openai_responses_fields(provider, upstream, upstream_path, object, body_len);
    changed |= strip_openai_responses_sampling_params(
        provider,
        upstream_path,
        object,
        meta,
        body_len,
        must_strip_sampling,
    )?;
    changed |= rewrite_developer_roles_if_needed(upstream, upstream_path, object, body_len);
    changed |= filter_anthropic_count_tokens_request(provider, upstream_path, object, body_len);
    changed |= filter_image_content_for_model(object, body_len);
    changed |= inject_codex_installation_id(object, provider, codex_openai_device_id);
    if !changed {
        return Ok(None);
    }

    replayable_from_json(value).map(Some)
}

fn json_transform_read_limit(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path: &str,
    meta: &RequestMeta,
    codex_openai_device_id: Option<&str>,
) -> usize {
    let mut limit = 0usize;
    if meta.model_override().is_some() && meta.mapped_model.is_some() {
        limit = limit.max(REQUEST_MODEL_MAPPING_LIMIT_BYTES);
    }
    if should_apply_reasoning_effort(provider, upstream_path, meta) {
        limit = limit.max(REQUEST_REASONING_LIMIT_BYTES);
    }
    if should_filter_openai_responses_fields(provider, upstream, upstream_path) {
        limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    }
    if should_strip_openai_responses_sampling_params(provider, upstream_path, meta) {
        limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    }
    if should_rewrite_developer_roles(upstream, upstream_path) {
        limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    }
    if should_filter_anthropic_count_tokens_request(provider, upstream_path) {
        limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    }
    if should_inject_codex_installation_id(provider, codex_openai_device_id) {
        limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    }
    // 图片过滤始终需要读取请求体
    limit = limit.max(REQUEST_FILTER_LIMIT_BYTES);
    limit
}

fn needs_json_transform(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path: &str,
    meta: &RequestMeta,
    codex_openai_device_id: Option<&str>,
) -> bool {
    (meta.model_override().is_some() && meta.mapped_model.is_some())
        || should_apply_reasoning_effort(provider, upstream_path, meta)
        || should_filter_openai_responses_fields(provider, upstream, upstream_path)
        || should_strip_openai_responses_sampling_params(provider, upstream_path, meta)
        || should_rewrite_developer_roles(upstream, upstream_path)
        || should_filter_anthropic_count_tokens_request(provider, upstream_path)
        || should_inject_codex_installation_id(provider, codex_openai_device_id)
        // 始终尝试过滤图片内容（按模型能力判断）
        || true
}

fn rewrite_model_mapping(
    object: &mut Map<String, Value>,
    meta: &RequestMeta,
    body_len: usize,
) -> bool {
    if body_len > REQUEST_MODEL_MAPPING_LIMIT_BYTES {
        return false;
    }
    if meta.model_override().is_none() {
        return false;
    }
    let Some(mapped_model) = meta.mapped_model.as_deref() else {
        return false;
    };
    if !object.contains_key("model") {
        return false;
    }
    object.insert("model".to_string(), Value::String(mapped_model.to_string()));
    true
}

fn should_apply_reasoning_effort(provider: &str, upstream_path: &str, meta: &RequestMeta) -> bool {
    meta.reasoning_effort.is_some()
        && ((provider == "openai" && upstream_path == OPENAI_CHAT_PATH)
            || (provider == "openai-response" && upstream_path == OPENAI_RESPONSES_PATH))
}

fn apply_reasoning_effort(
    provider: &str,
    upstream_path: &str,
    object: &mut Map<String, Value>,
    meta: &RequestMeta,
    body_len: usize,
) -> bool {
    if body_len > REQUEST_REASONING_LIMIT_BYTES {
        return false;
    }
    let Some(effort) = meta.reasoning_effort.as_deref() else {
        return false;
    };
    if !should_apply_reasoning_effort(provider, upstream_path, meta) {
        return false;
    }

    let model_for_upstream = meta
        .mapped_model
        .as_deref()
        .or(meta.original_model.as_deref());
    let effort = normalize_glm_reasoning_effort(model_for_upstream, effort).unwrap_or(effort);
    if let Some(model) = model_for_upstream {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }
    if provider == "openai" {
        object.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.to_string()),
        );
        return true;
    }

    let reasoning = ensure_json_object_field(object, "reasoning");
    reasoning.insert("effort".to_string(), Value::String(effort.to_string()));
    true
}

fn normalize_glm_reasoning_effort(model: Option<&str>, effort: &str) -> Option<&'static str> {
    let model = model?.trim().to_ascii_lowercase();
    if !model.starts_with("glm-") {
        return None;
    }
    let normalized = effort
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_', ' '], "");
    match normalized.as_str() {
        "low" | "medium" | "high" => Some("high"),
        "xhigh" | "extrahigh" | "max" | "ultracode" => Some("max"),
        _ => None,
    }
}

fn ensure_json_object_field<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> &'a mut Map<String, Value> {
    if !matches!(object.get(key), Some(Value::Object(_))) {
        object.insert(key.to_string(), Value::Object(Map::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("inserted value must be object")
}

fn should_filter_openai_responses_fields(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path: &str,
) -> bool {
    provider == "openai-response"
        && upstream_path == OPENAI_RESPONSES_PATH
        && (upstream.filter_prompt_cache_retention
            || upstream.filter_safety_identifier
            || upstream.codex_catalog.image_input
            || upstream.codex_catalog.web_search
            || upstream.codex_catalog.parallel_tool_calls
            || upstream.codex_catalog.apply_patch)
}

fn filter_openai_responses_fields(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path: &str,
    object: &mut Map<String, Value>,
    body_len: usize,
) -> bool {
    if body_len > REQUEST_FILTER_LIMIT_BYTES {
        return false;
    }
    if !should_filter_openai_responses_fields(provider, upstream, upstream_path) {
        return false;
    }
    let mut changed = false;
    if upstream.filter_prompt_cache_retention {
        changed |= object.remove("prompt_cache_retention").is_some();
    }
    if upstream.filter_safety_identifier {
        changed |= object.remove("safety_identifier").is_some();
    }
    changed |= filter_codex_capability_fields(upstream, object);
    changed
}

fn filter_codex_capability_fields(upstream: &UpstreamRuntime, object: &mut Map<String, Value>) -> bool {
    let mut changed = false;

    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        let before = tools.len();
        tools.retain(|tool| should_keep_responses_tool(tool));
        changed |= tools.len() != before;
    }

    if let Some(include) = object.get_mut("include").and_then(Value::as_array_mut) {
        let before = include.len();
        include.retain(|item| should_keep_responses_include(item));
        changed |= include.len() != before;
    }

    changed
}

fn should_keep_responses_include(item: &Value) -> bool {
    let Some(value) = item.as_str() else {
        return true;
    };
    let normalized = value.to_ascii_lowercase();
    if normalized.contains("image_generation") || normalized.contains("image_gen") {
        return false;
    }
    true
}

fn should_keep_responses_tool(tool: &Value) -> bool {
    let Some(object) = tool.as_object() else {
        return true;
    };
    let tool_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let tool_name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let marker = format!("{tool_type} {tool_name}");

    // Image input (input_image/image_url in message content) is distinct from image generation tools.
    // Many OpenAI-compatible gateways allow image understanding but reject image generation.
    if marker.contains("image_generation") || marker.contains("image_gen") {
        return false;
    }
    true
}


/// 按模型名称判断是否支持图片输入
/// 与 client_config.rs 中 resolve_model_capabilities 逻辑一致
fn model_supports_image_input(model: &str) -> bool {
    let short_name = model.rsplit('/').next().unwrap_or(model).trim().to_ascii_lowercase();
    
    // GPT系列: 全能力
    if short_name.starts_with("gpt-") || short_name.starts_with("chatgpt-") || short_name == "gpt-4o" || short_name == "gpt-4-turbo" {
        return true;
    }
    // Claude系列: 图片+并行
    if short_name.starts_with("claude-") || short_name.starts_with("claude") {
        return true;
    }
    // DeepSeek系列: 纯文本(除VL视觉版)
    if (short_name.starts_with("deepseek-") || short_name.starts_with("deepseek")) && !short_name.contains("vl") && !short_name.contains("vision") {
        return false;
    }
    // Gemini系列: 图片+搜索+并行
    if short_name.starts_with("gemini-") || short_name.starts_with("gemini") {
        return true;
    }
    // MiniMax系列: 图片+并行
    if short_name.starts_with("minimax-") || short_name.starts_with("minimax") {
        return true;
    }
    // Mimo系列: 图片+并行
    if short_name.starts_with("mimo-") || short_name.starts_with("mimo") {
        return true;
    }
    // LongCat系列: 图片+并行
    if short_name.starts_with("longcat-") || short_name.starts_with("longcat") {
        return true;
    }
    // Qwen系列: 图片+并行
    if short_name.starts_with("qwen-") || short_name.starts_with("qwen") {
        return true;
    }
    // Llama系列: 含vision的图片+并行
    if short_name.starts_with("llama-") || short_name.starts_with("llama") {
        return short_name.contains("vision") || short_name.contains("vl");
    }
    // Mistral/Pixtral系列
    if short_name.starts_with("pixtral-") || short_name.starts_with("pixtral") {
        return true;
    }
    if short_name.starts_with("mistral-") || short_name.starts_with("mistral") {
        return short_name.contains("vision");
    }
    // 默认识别为支持图片（安全保守）
    true
}

/// 从请求体中提取 model 字段
fn extract_model_from_body(object: &Map<String, Value>) -> Option<String> {
    object.get("model").and_then(Value::as_str).map(|s| s.to_string())
}

/// 根据模型能力过滤请求中的图片内容
/// 支持三种格式:
/// - Responses API: input[].type == "input_image"
/// - Chat API: messages[].content[].type == "image_url"  
/// - Messages API (Anthropic): messages[].content[].type == "image"
fn filter_image_content_for_model(object: &mut Map<String, Value>, body_len: usize) -> bool {
    if body_len > REQUEST_FILTER_LIMIT_BYTES {
        return false;
    }
    let Some(model) = extract_model_from_body(object) else {
        return false;
    };
    if model_supports_image_input(&model) {
        return false;
    }
    
    let mut changed = false;
    
    // Responses API: input[] array with input_image items
    if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
        let before = input.len();
        input.retain(|item| {
            if let Some(obj) = item.as_object() {
                let type_val = obj.get("type").and_then(Value::as_str).unwrap_or("");
                if type_val == "input_image" || type_val == "image_url" || type_val.contains("image") {
                    // Only keep if it's likely a text item
                    return false;
                }
            }
            true
        });
        changed |= input.len() != before;
    }
    
    // Chat API / Messages API: messages[].content[] with image items
    if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            let Some(msg_obj) = message.as_object_mut() else { continue };
            let Some(content) = msg_obj.get_mut("content") else { continue };
            
            // Content could be a string (text only) or array (mixed)
            if let Some(content_array) = content.as_array_mut() {
                let before = content_array.len();
                content_array.retain(|part| {
                    if let Some(part_obj) = part.as_object() {
                        let type_val = part_obj.get("type").and_then(Value::as_str).unwrap_or("");
                        // image_url = OpenAI Chat, image = Anthropic Messages, input_image = Responses
                        if type_val == "image_url" || type_val == "image" || type_val == "input_image" {
                            return false;
                        }
                    }
                    true
                });
                changed |= content_array.len() != before;
            }
        }
    }
    
    changed
}

fn should_strip_openai_responses_sampling_params(
    provider: &str,
    upstream_path: &str,
    meta: &RequestMeta,
) -> bool {
    let model = meta
        .mapped_model
        .as_deref()
        .or(meta.original_model.as_deref());
    provider == "openai-response"
        && upstream_path == OPENAI_RESPONSES_PATH
        && model.is_some_and(model::is_openai_responses_reasoning_model)
}

fn strip_openai_responses_sampling_params(
    provider: &str,
    upstream_path: &str,
    object: &mut Map<String, Value>,
    meta: &RequestMeta,
    body_len: usize,
    must_strip_sampling: bool,
) -> Result<bool, AttemptOutcome> {
    if must_strip_sampling && body_len > REQUEST_FILTER_LIMIT_BYTES {
        return Err(openai_responses_sampling_params_payload_too_large());
    }
    if !should_strip_openai_responses_sampling_params(provider, upstream_path, meta) {
        return Ok(false);
    }
    let mut changed = false;
    changed |= object.remove("temperature").is_some();
    changed |= object.remove("top_p").is_some();
    Ok(changed)
}

fn openai_responses_sampling_params_payload_too_large() -> AttemptOutcome {
    AttemptOutcome::Fatal(http::error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        format!(
            "OpenAI Responses reasoning model request is too large to validate sampling parameters; limit is {REQUEST_FILTER_LIMIT_BYTES} bytes."
        ),
    ))
}

fn should_rewrite_developer_roles(upstream: &UpstreamRuntime, upstream_path: &str) -> bool {
    upstream.rewrite_developer_role_to_system
        && (upstream_path == OPENAI_CHAT_PATH || upstream_path == OPENAI_RESPONSES_PATH)
}

fn rewrite_developer_roles_if_needed(
    upstream: &UpstreamRuntime,
    upstream_path: &str,
    object: &mut Map<String, Value>,
    body_len: usize,
) -> bool {
    if body_len > REQUEST_FILTER_LIMIT_BYTES {
        return false;
    }
    if !should_rewrite_developer_roles(upstream, upstream_path) {
        return false;
    }
    if upstream_path == OPENAI_CHAT_PATH {
        return rewrite_chat_developer_roles(object);
    }
    rewrite_responses_developer_roles(object)
}

fn should_filter_anthropic_count_tokens_request(provider: &str, upstream_path: &str) -> bool {
    provider == "anthropic" && upstream_path == ANTHROPIC_COUNT_TOKENS_PATH
}

fn filter_anthropic_count_tokens_request(
    provider: &str,
    upstream_path: &str,
    object: &mut Map<String, Value>,
    body_len: usize,
) -> bool {
    if body_len > REQUEST_FILTER_LIMIT_BYTES {
        return false;
    }
    if !should_filter_anthropic_count_tokens_request(provider, upstream_path) {
        return false;
    }

    // Anthropic count_tokens rejects generation-only fields accepted by messages.
    let mut changed = false;
    for key in [
        "temperature",
        "top_p",
        "top_k",
        "stream",
        "stop_sequences",
        "stop",
        "metadata",
    ] {
        changed |= object.remove(key).is_some();
    }
    if changed {
        tracing::debug!("filtered Anthropic count_tokens generation-only fields");
    }
    changed
}

fn should_inject_codex_installation_id(
    provider: &str,
    codex_openai_device_id: Option<&str>,
) -> bool {
    provider == "codex"
        && codex_openai_device_id
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
}

fn inject_codex_installation_id(
    object: &mut Map<String, Value>,
    provider: &str,
    codex_openai_device_id: Option<&str>,
) -> bool {
    if provider != "codex" {
        return false;
    }
    let Some(device_id) = codex_openai_device_id.map(str::trim) else {
        return false;
    };
    if device_id.is_empty() {
        return false;
    }

    // Codex OAuth requests expect the account installation id inside client metadata.
    let client_metadata = ensure_json_object_field(object, "client_metadata");
    if client_metadata.contains_key(CODEX_INSTALLATION_ID_KEY) {
        return false;
    }
    client_metadata.insert(
        CODEX_INSTALLATION_ID_KEY.to_string(),
        Value::String(device_id.to_string()),
    );
    tracing::debug!("injected Codex installation id into client_metadata");
    true
}

fn replayable_from_json(value: Value) -> Result<ReplayableBody, AttemptOutcome> {
    let outbound_bytes = serde_json::to_vec(&value).map(Bytes::from).map_err(|err| {
        AttemptOutcome::Fatal(http::error_response(
            StatusCode::BAD_GATEWAY,
            format!("Failed to serialize request: {err}"),
        ))
    })?;
    Ok(ReplayableBody::from_bytes(outbound_bytes))
}

#[cfg(test)]
async fn maybe_rewrite_developer_role_to_system(
    upstream: &UpstreamRuntime,
    upstream_path_with_query: &str,
    body: &ReplayableBody,
) -> Result<Option<ReplayableBody>, AttemptOutcome> {
    if !upstream.rewrite_developer_role_to_system {
        return Ok(None);
    }

    let upstream_path = split_path_query(upstream_path_with_query).0;
    if upstream_path != OPENAI_CHAT_PATH && upstream_path != OPENAI_RESPONSES_PATH {
        return Ok(None);
    }

    let Some(bytes) = body
        .read_bytes_if_small(REQUEST_FILTER_LIMIT_BYTES)
        .await
        .map_err(|err| {
            AttemptOutcome::Fatal(http::error_response(
                StatusCode::BAD_GATEWAY,
                format!("Failed to read cached request body: {err}"),
            ))
        })?
    else {
        return Ok(None);
    };

    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };

    let changed = if upstream_path == OPENAI_CHAT_PATH {
        rewrite_chat_developer_roles(object)
    } else {
        rewrite_responses_developer_roles(object)
    };
    if !changed {
        return Ok(None);
    }

    let outbound_bytes = serde_json::to_vec(&value).map(Bytes::from).map_err(|err| {
        AttemptOutcome::Fatal(http::error_response(
            StatusCode::BAD_GATEWAY,
            format!("Failed to serialize request: {err}"),
        ))
    })?;
    Ok(Some(ReplayableBody::from_bytes(outbound_bytes)))
}

fn rewrite_chat_developer_roles(object: &mut serde_json::Map<String, Value>) -> bool {
    let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for message in messages {
        let Some(item) = message.as_object_mut() else {
            continue;
        };
        changed |= rewrite_role_field(item);
    }
    changed
}

fn rewrite_responses_developer_roles(object: &mut serde_json::Map<String, Value>) -> bool {
    let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for item in input {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        changed |= rewrite_role_field(item);
    }
    changed
}

fn rewrite_role_field(object: &mut serde_json::Map<String, Value>) -> bool {
    let Some(role) = object.get_mut("role") else {
        return false;
    };
    if role.as_str() != Some("developer") {
        return false;
    }
    *role = Value::String("system".to_string());
    true
}

#[cfg(test)]
async fn maybe_filter_openai_responses_request_fields(
    provider: &str,
    upstream: &UpstreamRuntime,
    upstream_path_with_query: &str,
    body: &ReplayableBody,
) -> Result<Option<ReplayableBody>, AttemptOutcome> {
    let upstream_path = split_path_query(upstream_path_with_query).0;
    if !should_filter_openai_responses_fields(provider, upstream, upstream_path) {
        return Ok(None);
    }

    let Some(bytes) = body
        .read_bytes_if_small(REQUEST_FILTER_LIMIT_BYTES)
        .await
        .map_err(|err| {
            AttemptOutcome::Fatal(http::error_response(
                StatusCode::BAD_GATEWAY,
                format!("Failed to read cached request body: {err}"),
            ))
        })?
    else {
        // Best-effort: request body too large to rewrite.
        return Ok(None);
    };

    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let changed = filter_openai_responses_fields(
        provider,
        upstream,
        upstream_path,
        object,
        bytes.len(),
    );
    if !changed {
        return Ok(None);
    }

    let outbound_bytes = serde_json::to_vec(&value).map(Bytes::from).map_err(|err| {
        AttemptOutcome::Fatal(http::error_response(
            StatusCode::BAD_GATEWAY,
            format!("Failed to serialize request: {err}"),
        ))
    })?;
    Ok(Some(ReplayableBody::from_bytes(outbound_bytes)))
}

// 单元测试拆到独立文件，使用 `#[path]` 以保持 `.test.rs` 命名约定。
#[cfg(test)]
#[path = "request_body.test.rs"]
mod tests;
