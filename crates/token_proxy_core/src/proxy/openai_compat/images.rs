use axum::{body::Bytes, http::StatusCode};
use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

const CODEX_IMAGE_RESPONSES_MODEL: &str = "gpt-5.4-mini";
const DEFAULT_IMAGE_TOOL_MODEL: &str = "gpt-image-2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexImagesGenerationError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

pub(crate) fn images_generation_request_to_responses(body: &Bytes) -> Result<Bytes, String> {
    let object = parse_object(body)?;
    let prompt = required_string(&object, "prompt", "Images request must include prompt.")?;
    let tool_model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_IMAGE_TOOL_MODEL);
    let requested_count = image_count(&object)?;
    if requested_count > 1 {
        return Err("Codex image bridge currently supports n=1 only.".to_string());
    }

    // Codex accepts image generation through the Responses API, not the Images endpoint.
    // Keep the text model at the top level and put the image model inside the tool.
    let mut tool = Map::new();
    tool.insert("type".to_string(), json!("image_generation"));
    tool.insert("action".to_string(), json!("generate"));
    tool.insert("model".to_string(), json!(tool_model));
    copy_string_fields(
        &object,
        &mut tool,
        &[
            "size",
            "quality",
            "background",
            "output_format",
            "moderation",
            "style",
        ],
    );
    copy_value_fields(
        &object,
        &mut tool,
        &["output_compression", "partial_images"],
    );

    let output = json!({
        "instructions": "",
        "stream": true,
        "reasoning": { "effort": "medium", "summary": "auto" },
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "model": CODEX_IMAGE_RESPONSES_MODEL,
        "store": false,
        "tool_choice": { "type": "image_generation" },
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": prompt }
                ]
            }
        ],
        "tools": [Value::Object(tool)]
    });

    serde_json::to_vec(&output)
        .map(Bytes::from)
        .map_err(|err| format!("Failed to serialize images bridge request: {err}"))
}

pub(crate) fn codex_response_to_images_generation(
    bytes: &Bytes,
    response_format: Option<&str>,
) -> Result<Bytes, CodexImagesGenerationError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| codex_images_error("Codex image bridge response must be JSON."))?;
    if let Some(error) = codex_images_upstream_error(&value) {
        return Err(error);
    }
    let response = extract_response_object(&value).ok_or_else(|| {
        codex_images_error("Codex image bridge response missing response object.")
    })?;
    let created = response
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(now_unix_seconds);
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| codex_images_error("Codex image bridge response missing output array."))?;

    let response_meta = first_image_tool_meta(response);
    let mut data = Vec::new();
    let mut first_meta = Map::new();
    for item in output {
        let Some(object) = item.as_object() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            continue;
        }
        let Some(result) = object
            .get("result")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let mut image = Map::new();
        image.insert("b64_json".to_string(), Value::String(result.to_string()));
        if response_format_is_url(response_format) {
            let mime_type =
                image_output_mime_type(object.get("output_format").and_then(Value::as_str));
            image.insert(
                "url".to_string(),
                Value::String(format!("data:{mime_type};base64,{result}")),
            );
        }
        copy_string_fields(object, &mut image, &["revised_prompt"]);
        if data.is_empty() {
            first_meta = response_meta.clone();
            copy_string_fields(
                object,
                &mut first_meta,
                &["background", "output_format", "quality", "size", "model"],
            );
        }
        data.push(Value::Object(image));
    }

    if data.is_empty() {
        return Err(codex_images_retryable_error(format!(
            "Codex image bridge response contained no generated images. {}",
            summarize_no_image_output(&value)
        )));
    }

    let mut output = Map::new();
    output.insert("created".to_string(), json!(created));
    output.insert("data".to_string(), Value::Array(data));
    for key in ["background", "output_format", "quality", "size", "model"] {
        if let Some(value) = first_meta.get(key).cloned() {
            output.insert(key.to_string(), value);
        }
    }
    if let Some(usage) = image_generation_usage(response) {
        output.insert("usage".to_string(), usage);
    }

    serde_json::to_vec(&Value::Object(output))
        .map(Bytes::from)
        .map_err(|err| codex_images_error(format!("Failed to serialize images response: {err}")))
}

fn parse_object(body: &Bytes) -> Result<Map<String, Value>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "Images request must be JSON.".to_string())?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "Images request must be a JSON object.".to_string())
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    message: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| message.to_string())
}

fn image_count(object: &Map<String, Value>) -> Result<u64, String> {
    let Some(value) = object.get("n") else {
        return Ok(1);
    };
    value
        .as_u64()
        .filter(|count| *count > 0)
        .ok_or_else(|| "Images request n must be a positive integer.".to_string())
}

fn copy_string_fields(source: &Map<String, Value>, target: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = source
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            target.insert((*key).to_string(), Value::String(value.to_string()));
        }
    }
}

fn copy_value_fields(source: &Map<String, Value>, target: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = source.get(*key).filter(|value| !value.is_null()) {
            target.insert((*key).to_string(), value.clone());
        }
    }
}

fn first_image_tool_meta(response: &Map<String, Value>) -> Map<String, Value> {
    let Some(tool) = response
        .get("tools")
        .and_then(Value::as_array)
        .and_then(|tools| tools.first())
        .and_then(Value::as_object)
        .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation"))
    else {
        return Map::new();
    };
    let mut meta = Map::new();
    copy_string_fields(
        tool,
        &mut meta,
        &["background", "output_format", "quality", "size", "model"],
    );
    meta
}

fn extract_response_object(value: &Value) -> Option<&Map<String, Value>> {
    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
        return value.get("response").and_then(Value::as_object);
    }
    if let Some(response) = value.get("response").and_then(Value::as_object) {
        return Some(response);
    }
    value.as_object()
}

fn codex_images_upstream_error(value: &Value) -> Option<CodexImagesGenerationError> {
    match value.get("type").and_then(Value::as_str) {
        Some("error") => Some(codex_images_error_from_object(
            value.get("error"),
            StatusCode::BAD_GATEWAY,
            true,
        )),
        Some("response.failed") => Some(codex_images_error_from_object(
            value
                .get("response")
                .and_then(|response| response.get("error")),
            StatusCode::BAD_GATEWAY,
            true,
        )),
        Some("response.incomplete") => value
            .get("response")
            .map(codex_images_incomplete_error)
            .or_else(|| {
                Some(codex_images_retryable_error(
                    "Upstream did not complete image generation",
                ))
            }),
        _ => {
            match value.get("status").and_then(Value::as_str) {
                Some("failed") => {
                    return Some(codex_images_error_from_object(
                        value.get("error"),
                        StatusCode::BAD_GATEWAY,
                        true,
                    ));
                }
                Some("incomplete") => {
                    return Some(codex_images_incomplete_error(value));
                }
                _ => {}
            }
            let response = value.get("response")?;
            match response.get("status").and_then(Value::as_str) {
                Some("failed") => Some(codex_images_error_from_object(
                    response.get("error"),
                    StatusCode::BAD_GATEWAY,
                    true,
                )),
                Some("incomplete") => Some(codex_images_incomplete_error(response)),
                _ => None,
            }
        }
    }
}

fn codex_images_error_from_object(
    error: Option<&Value>,
    fallback_status: StatusCode,
    retryable: bool,
) -> CodexImagesGenerationError {
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("Upstream image generation failed");
    CodexImagesGenerationError {
        status: fallback_status,
        message: message.to_string(),
        retryable,
    }
}

fn codex_images_incomplete_error(response: &Value) -> CodexImagesGenerationError {
    let reason = response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty());
    let is_content_filter = reason.is_some_and(|reason| {
        let reason = reason.to_ascii_lowercase();
        reason.contains("content_filter") || reason.contains("moderation")
    });
    let message = reason
        .map(|reason| format!("Upstream image generation incomplete: {reason}"))
        .unwrap_or_else(|| "Upstream did not complete image generation".to_string());
    CodexImagesGenerationError {
        status: if is_content_filter {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        },
        message,
        retryable: !is_content_filter,
    }
}

fn codex_images_error(message: impl Into<String>) -> CodexImagesGenerationError {
    CodexImagesGenerationError {
        status: StatusCode::BAD_GATEWAY,
        message: message.into(),
        retryable: false,
    }
}

fn codex_images_retryable_error(message: impl Into<String>) -> CodexImagesGenerationError {
    CodexImagesGenerationError {
        status: StatusCode::BAD_GATEWAY,
        message: message.into(),
        retryable: true,
    }
}

fn summarize_no_image_output(value: &Value) -> String {
    let status = value
        .get("status")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("status"))
        })
        .and_then(Value::as_str);
    let incomplete_reason = value
        .get("incomplete_details")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("incomplete_details"))
        })
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    let mut parts = vec!["no_image_output".to_string()];
    if let Some(event_type) = value.get("type").and_then(Value::as_str) {
        parts.push(format!("last_event={event_type}"));
    }
    if let Some(status) = status {
        parts.push(format!("status={status}"));
    }
    if let Some(incomplete_reason) = incomplete_reason {
        parts.push(format!("incomplete_reason={incomplete_reason}"));
    }
    parts.join(" ")
}

pub(crate) fn image_generation_usage(response: &Map<String, Value>) -> Option<Value> {
    // Responses token usage is the client-visible accounting source; tool_usage
    // is a Codex image fallback for older or partial response envelopes.
    response
        .get("usage")
        .filter(|usage| usage.is_object())
        .cloned()
        .or_else(|| {
            response
                .get("tool_usage")
                .and_then(|tool_usage| tool_usage.get("image_gen"))
                .filter(|usage| usage.is_object())
                .cloned()
        })
}

fn response_format_is_url(response_format: Option<&str>) -> bool {
    response_format
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
        == Some("url")
}

pub(crate) fn image_output_mime_type(output_format: Option<&str>) -> &'static str {
    match output_format
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("png") | None | Some("") => "image/png",
        Some(_) => "image/png",
    }
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
