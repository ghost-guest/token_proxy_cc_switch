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
        && (upstream.filter_prompt_cache_retention || upstream.filter_safety_identifier)
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
    let should_filter_prompt_cache_retention = upstream.filter_prompt_cache_retention;
    let should_filter_safety_identifier = upstream.filter_safety_identifier;
    if provider != "openai-response"
        || (!should_filter_prompt_cache_retention && !should_filter_safety_identifier)
    {
        return Ok(None);
    }
    let upstream_path = split_path_query(upstream_path_with_query).0;
    if upstream_path != OPENAI_RESPONSES_PATH {
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
    let mut changed = false;
    if should_filter_prompt_cache_retention {
        changed = changed || object.remove("prompt_cache_retention").is_some();
    }
    if should_filter_safety_identifier {
        changed = changed || object.remove("safety_identifier").is_some();
    }
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
