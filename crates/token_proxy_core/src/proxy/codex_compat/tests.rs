use axum::body::Bytes;
use futures_util::{future, StreamExt};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::super::{
    log::{LogContext, LogWriter},
    token_rate::TokenRateTracker,
};
use super::tool_names::shorten_name_if_needed;
use super::{
    chat_request_to_codex, codex_response_to_chat, codex_response_to_responses,
    responses_compact_request_to_codex, responses_request_to_codex, stream_codex_to_chat,
    stream_codex_to_responses, stream_codex_to_responses_with_semantic_timeout,
};

#[test]
fn chat_request_to_codex_sets_model_and_stream() {
    let input = json!({
        "model": "gpt-5",
        "stream": true,
        "messages": [
            { "role": "user", "content": "hi" }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let output = chat_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(value["model"], "gpt-5-codex");
    assert_eq!(value["stream"], true);
    assert_eq!(value["input"][0]["type"], "message");
}

#[test]
fn chat_request_to_codex_forces_upstream_streaming() {
    let input = json!({
        "model": "gpt-5",
        "stream": false,
        "messages": [
            { "role": "user", "content": "hi" }
        ]
    });

    let output = chat_request_to_codex(&Bytes::from(input.to_string()), Some("gpt-5-codex"))
        .expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["stream"], true);
}

#[test]
fn chat_request_to_codex_accepts_responses_shaped_body() {
    let input = json!({
        "model": "openai/gpt 5.5",
        "stream": false,
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hi" }]
            }
        ],
        "metadata": { "client": "cursor" },
        "stream_options": { "include_usage": true },
        "prompt_cache_retention": "24h",
        "safety_identifier": "sid_1"
    });

    let output = chat_request_to_codex(&Bytes::from(input.to_string()), None)
        .expect("convert responses-shaped chat request");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["model"], "gpt-5.5");
    assert_eq!(value["stream"], true);
    assert_eq!(value["store"], false);
    assert_eq!(value["instructions"], "You are a helpful coding assistant.");
    assert_eq!(value["input"][0]["role"], "user");
    assert!(value.get("messages").is_none());
    assert!(value.get("metadata").is_none());
    assert!(value.get("stream_options").is_none());
    assert!(value.get("prompt_cache_retention").is_none());
    assert!(value.get("safety_identifier").is_none());
}

#[test]
fn chat_request_to_codex_rejects_missing_messages_without_input() {
    let input = json!({ "model": "gpt-5" });
    let error = chat_request_to_codex(&Bytes::from(input.to_string()), None)
        .expect_err("should reject malformed chat request");

    assert!(error.contains("messages"), "error: {error}");
}

#[test]
fn responses_request_to_codex_normalizes_gpt_5_5_and_sanitizes_oauth_payload() {
    let input = json!({
        "model": "openai/gpt 5.5",
        "stream": false,
        "store": true,
        "frequency_penalty": 0.2,
        "presence_penalty": 0.3,
        "prompt_cache_retention": "24h",
        "input": [
            {
                "type": "message",
                "role": "system",
                "content": [
                    { "type": "input_text", "text": "system rules" }
                ]
            },
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "hi" }
                ]
            }
        ]
    });

    let output = responses_request_to_codex(&Bytes::from(input.to_string()), None)
        .expect("convert responses request");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["model"], "gpt-5.5");
    assert_eq!(value["stream"], true);
    assert_eq!(value["store"], false);
    assert_eq!(value["instructions"], "system rules");
    assert_eq!(value["input"].as_array().expect("input").len(), 1);
    assert_eq!(value["input"][0]["role"], "user");
    assert!(value.get("frequency_penalty").is_none());
    assert!(value.get("presence_penalty").is_none());
    assert!(value.get("prompt_cache_retention").is_none());
}

#[test]
fn responses_compact_request_to_codex_normalizes_gpt_5_5_and_removes_stream_store() {
    let input = json!({
        "model": "gpt-5.5-medium",
        "stream": true,
        "store": true,
        "input": "hi"
    });

    let output = responses_compact_request_to_codex(&Bytes::from(input.to_string()), None)
        .expect("convert compact responses request");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["model"], "gpt-5.5");
    assert_eq!(value["instructions"], "You are a helpful coding assistant.");
    assert_eq!(value["stream"], true);
    assert_eq!(value["store"], false);
    assert!(value.get("include").is_none());
}

#[test]
fn responses_compact_request_to_codex_normalizes_openai_message_input() {
    let input = json!({
        "model": "gpt-5.5",
        "input": [
            { "role": "user", "content": "hi" }
        ]
    });

    let output = responses_compact_request_to_codex(&Bytes::from(input.to_string()), None)
        .expect("convert compact responses request");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["input"][0]["type"], "message");
    assert_eq!(value["input"][0]["role"], "user");
    assert_eq!(value["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(value["input"][0]["content"][0]["text"], "hi");
}

#[test]
fn codex_response_to_chat_restores_tool_name() {
    let original = "mcp__very_long_tool_name_for_codex_restoration_check_v1_tool_extra_long_suffix";
    let short = shorten_name_if_needed(original);
    assert!(short.len() <= 64);
    assert_ne!(short, original);

    let request_body = json!({
        "tools": [
            { "type": "function", "function": { "name": original } }
        ]
    })
    .to_string();

    let response = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_1",
            "created_at": 123,
            "model": "gpt-5",
            "status": "completed",
            "output": [
                { "type": "function_call", "call_id": "call_1", "name": short, "arguments": "{}" }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 2, "total_tokens": 3 }
        }
    });
    let bytes = Bytes::from(response.to_string());
    let output = codex_response_to_chat(&bytes, Some(&request_body)).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let name = value["choices"][0]["message"]["tool_calls"][0]["function"]["name"]
        .as_str()
        .expect("tool name");
    assert_eq!(name, original);
}

#[test]
fn codex_response_to_chat_preserves_image_generation_call_result() {
    let response = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_img",
            "created_at": 123,
            "model": "gpt-5.5",
            "status": "completed",
            "output": [
                {
                    "type": "image_generation_call",
                    "id": "img_1",
                    "status": "completed",
                    "result": "BASE64PNG"
                }
            ]
        }
    });
    let bytes = Bytes::from(response.to_string());
    let output = codex_response_to_chat(&bytes, None).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .expect("content");

    assert!(content.contains("data:image/png;base64,BASE64PNG"));
}

#[test]
fn codex_response_to_responses_rejects_json_error_payload() {
    let bytes = Bytes::from(
        json!({
            "error": {
                "message": "rate limit exceeded",
                "type": "rate_limit_error"
            }
        })
        .to_string(),
    );

    let error = codex_response_to_responses(&bytes, None).expect_err("should fail");
    assert!(
        error.contains("rate limit exceeded"),
        "unexpected error: {error}"
    );
    assert!(error.contains("error payload"), "unexpected error: {error}");
}

#[test]
fn codex_response_to_responses_rejects_non_json_success_payload_with_raw_text() {
    let bytes = Bytes::from("upstream gateway said nope");

    let error = codex_response_to_responses(&bytes, None).expect_err("should fail");
    assert!(
        error.contains("non-JSON success payload"),
        "unexpected error: {error}"
    );
    assert!(
        error.contains("upstream gateway said nope"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn stream_codex_to_responses_emits_error_event_for_invalid_json_event() {
    let upstream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        "data: not-json\n\n",
    ))]);
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = stream_codex_to_responses(upstream, context, log, tracker)
        .collect::<Vec<_>>()
        .await;
    let text = join_stream_chunks(&chunks);

    assert!(text.contains("\"type\":\"error\""), "chunks: {text}");
    assert!(text.contains("invalid JSON stream event"), "chunks: {text}");
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_responses_emits_compatible_terminal_event_when_upstream_ends_early() {
    let upstream = futures_util::stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_early\",\"model\":\"gpt-5.4\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial output\"}\n\n",
        )),
    ]);
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = stream_codex_to_responses(upstream, context, log, tracker)
        .collect::<Vec<_>>()
        .await;
    let text = join_stream_chunks(&chunks);

    assert!(
        text.contains("\"type\":\"response.completed\""),
        "chunks: {text}"
    );
    assert!(text.contains("\"status\":\"incomplete\""), "chunks: {text}");
    assert!(
        text.contains("\"incomplete_details\":{\"reason\":\"error\"}"),
        "chunks: {text}"
    );
    assert!(text.contains("partial output"), "chunks: {text}");
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_responses_counts_reasoning_summary_delta_for_token_rate() {
    let (first_chunk, output_tokens) = first_codex_responses_output_tokens(
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking out loud\"}\n\n",
    )
    .await;

    assert!(
        String::from_utf8_lossy(&first_chunk).contains("response.reasoning_summary_text.delta"),
        "chunk: {:?}",
        first_chunk
    );
    assert!(output_tokens > 0, "output_tokens: {output_tokens}");
}

#[tokio::test]
async fn stream_codex_to_responses_counts_official_output_delta_events_for_token_rate() {
    for event in [
        "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"private reasoning\"}\n\n",
        "data: {\"type\":\"response.refusal.delta\",\"delta\":\"cannot comply\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"output_index\":0,\"delta\":\"{\\\"city\\\":\"}\n\n",
        "data: {\"type\":\"response.mcp_call_arguments.delta\",\"item_id\":\"mcp_1\",\"output_index\":0,\"delta\":\"{\\\"query\\\":\"}\n\n",
        "data: {\"type\":\"response.custom_tool_call_input.delta\",\"item_id\":\"ctc_1\",\"output_index\":0,\"delta\":\"partial input\"}\n\n",
        "data: {\"type\":\"response.code_interpreter_call_code.delta\",\"item_id\":\"ci_1\",\"output_index\":0,\"delta\":\"print(1)\"}\n\n",
    ] {
        let (_, output_tokens) = first_codex_responses_output_tokens(event).await;

        assert!(output_tokens > 0, "event should count output tokens: {event}");
    }
}

#[tokio::test]
async fn stream_codex_to_responses_does_not_count_final_snapshots_for_token_rate() {
    for event in [
        "data: {\"type\":\"response.output_text.done\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"text\":\"final text\"}\n\n",
        "data: {\"type\":\"response.reasoning_text.done\",\"item_id\":\"rs_1\",\"output_index\":0,\"content_index\":0,\"text\":\"final reasoning\"}\n\n",
        "data: {\"type\":\"response.custom_tool_call_input.done\",\"item_id\":\"ctc_1\",\"output_index\":0,\"input\":\"final input\"}\n\n",
        "data: {\"type\":\"response.code_interpreter_call_code.done\",\"item_id\":\"ci_1\",\"output_index\":0,\"code\":\"print(1)\"}\n\n",
        "data: {\"type\":\"response.mcp_call_arguments.done\",\"item_id\":\"mcp_1\",\"output_index\":0,\"arguments\":\"{\\\"query\\\":\\\"repo\\\"}\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_done\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"final text\"}]}]}}\n\n",
    ] {
        let (_, output_tokens) = first_codex_responses_output_tokens(event).await;

        assert_eq!(
            output_tokens, 0,
            "final snapshot should not count realtime output tokens: {event}"
        );
    }
}

#[tokio::test]
async fn stream_codex_to_responses_ignores_empty_delta_for_token_rate() {
    let (_, output_tokens) = first_codex_responses_output_tokens(
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"\"}\n\n",
    )
    .await;

    assert_eq!(output_tokens, 0);
}

#[tokio::test]
async fn stream_codex_to_responses_closes_after_terminal_without_upstream_close() {
    let upstream = futures_util::stream::unfold(0usize, |index| async move {
        if index == 0 {
            return Some((
                Ok::<Bytes, std::io::Error>(Bytes::from(
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_done\",\"model\":\"gpt-5.4\",\"status\":\"completed\"}}\n\n",
                )),
                1,
            ));
        }
        future::pending::<Option<(Result<Bytes, std::io::Error>, usize)>>().await
    })
    .boxed();
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = tokio::time::timeout(
        Duration::from_millis(200),
        stream_codex_to_responses(upstream, context, log, tracker).collect::<Vec<_>>(),
    )
    .await
    .expect("stream should end after terminal Responses event");
    let text = join_stream_chunks(&chunks);

    assert!(
        text.contains("\"type\":\"response.completed\""),
        "chunks: {text}"
    );
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_responses_semantic_timeout_ignores_heartbeat_comments() {
    let upstream = futures_util::stream::unfold(0usize, |index| async move {
        if index == 0 {
            return Some((
                Ok::<Bytes, std::io::Error>(Bytes::from(
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
                )),
                1,
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        Some((Ok::<Bytes, std::io::Error>(Bytes::from(":\n\n")), index + 1))
    })
    .boxed();
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = tokio::time::timeout(
        Duration::from_millis(500),
        stream_codex_to_responses_with_semantic_timeout(
            upstream,
            context,
            log,
            tracker,
            Some(Duration::from_millis(40)),
        )
        .collect::<Vec<_>>(),
    )
    .await
    .expect("heartbeat-only Codex stream should not hang");
    let text = join_stream_chunks(&chunks);

    assert!(text.contains("\"type\":\"error\""), "chunks: {text}");
    assert!(text.contains("\"type\":\"proxy_error\""), "chunks: {text}");
    assert!(text.contains("semantic timeout"), "chunks: {text}");
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_responses_accepts_canceled_terminal_event() {
    let upstream = futures_util::stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cancel\",\"model\":\"gpt-5.5\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.canceled\",\"response\":{\"id\":\"resp_cancel\",\"status\":\"cancelled\",\"model\":\"gpt-5.5\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n")),
    ]);
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = stream_codex_to_responses(upstream, context, log, tracker)
        .collect::<Vec<_>>()
        .await;
    let text = join_stream_chunks(&chunks);

    assert!(
        text.contains("\"type\":\"response.canceled\""),
        "chunks: {text}"
    );
    assert!(text.contains("\"status\":\"cancelled\""), "chunks: {text}");
    assert!(
        !text.contains("Codex upstream stream disconnected before response.completed"),
        "chunks: {text}"
    );
    assert!(
        !text.contains("\"incomplete_details\":{\"reason\":\"error\"}"),
        "chunks: {text}"
    );
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_chat_emits_error_event_for_invalid_json_event() {
    let upstream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        "data: not-json\n\n",
    ))]);
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = stream_codex_to_chat(upstream, context, log, tracker)
        .collect::<Vec<_>>()
        .await;
    let text = join_stream_chunks(&chunks);

    assert!(text.contains("\"error\":{"), "chunks: {text}");
    assert!(text.contains("invalid JSON stream event"), "chunks: {text}");
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

#[tokio::test]
async fn stream_codex_to_chat_emits_image_generation_call_result() {
    let upstream = futures_util::stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_img\",\"model\":\"gpt-5.5\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"image_generation_call\",\"id\":\"img_1\",\"status\":\"completed\",\"result\":\"BASE64PNG\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_img\",\"status\":\"completed\"}}\n\n",
        )),
        Ok::<Bytes, std::io::Error>(Bytes::from("data: [DONE]\n\n")),
    ]);
    let tracker = TokenRateTracker::new().register(None, None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));

    let chunks = stream_codex_to_chat(upstream, context, log, tracker)
        .collect::<Vec<_>>()
        .await;
    let text = join_stream_chunks(&chunks);

    assert!(
        text.contains("data:image/png;base64,BASE64PNG"),
        "chunks: {text}"
    );
    assert!(text.contains("data: [DONE]"), "chunks: {text}");
}

fn test_log_context() -> LogContext {
    LogContext {
        path: "/v1/responses".to_string(),
        provider: "codex".to_string(),
        upstream_id: "test".to_string(),
        account_id: None,
        model: Some("gpt-5-codex".to_string()),
        mapped_model: None,
        stream: true,
        status: 200,
        upstream_request_id: None,
        request_headers: None,
        request_body: None,
        ttfb_ms: None,
        timings: Default::default(),
        start: Instant::now(),
    }
}

fn join_stream_chunks(chunks: &[Result<Bytes, std::io::Error>]) -> String {
    chunks
        .iter()
        .map(|chunk| chunk.as_ref().expect("stream chunk"))
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect::<Vec<_>>()
        .join("")
}

async fn first_codex_responses_output_tokens(event: &str) -> (Bytes, u64) {
    let upstream = futures_util::stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
        event.to_string(),
    ))]);
    let token_rate = TokenRateTracker::new();
    let tracker = token_rate.register(Some("gpt-5.5".to_string()), None).await;
    let context = test_log_context();
    let log = Arc::new(LogWriter::new(None));
    let stream = stream_codex_to_responses(upstream, context, log, tracker);
    futures_util::pin_mut!(stream);
    let first_chunk = stream.next().await.expect("stream item").expect("chunk");
    let snapshot = token_rate.snapshot().await;

    (first_chunk, snapshot.output)
}

#[test]
fn chat_request_to_codex_skips_missing_tool_names() {
    let input = json!({
        "model": "gpt-5",
        "messages": [
            { "role": "user", "content": "hi" }
        ],
        "tools": [
            { "type": "function", "function": { "description": "noop", "parameters": {} } }
        ],
        "tool_choice": { "type": "function", "function": {} }
    });
    let bytes = Bytes::from(input.to_string());
    let output = chat_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let tools = value["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert!(tools[0].get("name").is_none());
    let tool_choice = value["tool_choice"].as_object().expect("tool_choice");
    assert_eq!(
        tool_choice.get("type").and_then(serde_json::Value::as_str),
        Some("function")
    );
    assert!(tool_choice.get("name").is_none());
}

#[test]
fn chat_request_to_codex_rejects_spark_image_url() {
    let input = json!({
        "model": "gpt-5.3-codex-spark",
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
                ]
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let error = chat_request_to_codex(&bytes, None).expect_err("should reject image input");

    assert!(error.contains("gpt-5.3-codex-spark"), "error: {error}");
    assert!(error.contains("text-only"), "error: {error}");
}

#[test]
fn responses_request_to_codex_uses_top_level_tool_name() {
    let input = json!({
        "model": "gpt-5",
        "input": "hi",
        "tools": [
            { "type": "function", "name": "demo_tool", "description": "noop", "parameters": {} }
        ],
        "tool_choice": { "type": "function", "name": "demo_tool" }
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let tools = value["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "demo_tool");
    assert_eq!(tools[0]["description"], "noop");
    assert!(tools[0]["parameters"].is_object());
    assert_eq!(
        value["tool_choice"]
            .get("name")
            .and_then(serde_json::Value::as_str),
        Some("demo_tool")
    );
}

#[test]
fn responses_request_to_codex_strips_prompt_cache_retention() {
    let input = json!({
        "model": "gpt-5",
        "input": "hi",
        "prompt_cache_retention": "24h",
        "previous_response_id": "resp_123",
        "safety_identifier": "sid_1"
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    assert!(value.get("prompt_cache_retention").is_none());
    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("safety_identifier").is_none());
}

#[test]
fn responses_request_to_codex_preserves_parallel_tool_calls_false() {
    let input = json!({
        "model": "gpt-5",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hi" }]
            }
        ],
        "parallel_tool_calls": false
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(value["parallel_tool_calls"], json!(false));
}

#[test]
fn responses_request_to_codex_strips_output_parts_from_function_call_output() {
    let input = json!({
        "model": "gpt-5",
        "input": [
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "ok",
                "output_parts": [
                    { "type": "text", "text": "ok" }
                ]
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let input_items = value["input"].as_array().expect("input array");
    assert_eq!(input_items.len(), 1);
    assert!(input_items[0].get("output_parts").is_none());
}

#[test]
fn responses_request_to_codex_rejects_spark_input_image() {
    let input = json!({
        "model": "gpt-5.3-codex-spark",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "describe" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
                ]
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let error = responses_request_to_codex(&bytes, None).expect_err("should reject image input");

    assert!(error.contains("gpt-5.3-codex-spark"), "error: {error}");
    assert!(error.contains("text-only"), "error: {error}");
}

#[test]
fn responses_request_to_codex_rejects_spark_image_generation_tool() {
    let input = json!({
        "model": "gpt-5.3-codex-spark",
        "input": "draw icon",
        "tools": [
            { "type": "image_generation" }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let error =
        responses_request_to_codex(&bytes, None).expect_err("should reject image generation");

    assert!(error.contains("gpt-5.3-codex-spark"), "error: {error}");
    assert!(error.contains("text-only"), "error: {error}");
}

#[test]
fn responses_request_to_codex_converts_tool_role_message_to_function_call_output() {
    let input = json!({
        "model": "gpt-5",
        "input": [
            {
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "ok"
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");
    let item = &value["input"][0];

    assert_eq!(item["type"], "function_call_output");
    assert_eq!(item["call_id"], "call_1");
    assert_eq!(item["output"], "ok");
    assert!(item.get("role").is_none());
}

#[test]
fn responses_request_to_codex_stringifies_non_string_message_content_text() {
    let input = json!({
        "model": "gpt-5",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": ["a", "b"] }
                ]
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["input"][0]["content"][0]["text"], "[\"a\",\"b\"]");
}

#[test]
fn responses_request_to_codex_downgrades_unknown_tool_choice_to_auto() {
    let input = json!({
        "model": "gpt-5",
        "input": "hi",
        "tools": [
            { "type": "function", "name": "shell" }
        ],
        "tool_choice": { "type": "custom" }
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["tool_choice"], "auto");
}

#[test]
fn responses_request_to_codex_adds_fallback_name_for_tool_call_input() {
    let input = json!({
        "model": "gpt-5",
        "input": [
            {
                "type": "function_call",
                "call_id": "call_1",
                "arguments": "{}"
            }
        ]
    });
    let bytes = Bytes::from(input.to_string());
    let output = responses_request_to_codex(&bytes, Some("gpt-5-codex")).expect("convert");
    let value: serde_json::Value = serde_json::from_slice(&output).expect("json");

    assert_eq!(value["input"][0]["type"], "function_call");
    assert_eq!(value["input"][0]["call_id"], "call_1");
    assert_eq!(value["input"][0]["name"], "tool");
}
