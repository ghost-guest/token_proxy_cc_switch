use axum::http::header::{HeaderName, HeaderValue};
use axum::http::HeaderMap;
const HEADER_OPENAI_BETA: &str = "responses=experimental";
const DEFAULT_USER_AGENT: &str = "codex_cli_rs/0.125.0";

const HEADER_OPENAI_BETA_NAME: HeaderName = HeaderName::from_static("openai-beta");
const HEADER_SESSION_ID_NAME: HeaderName = HeaderName::from_static("session_id");
const HEADER_USER_AGENT_NAME: HeaderName = HeaderName::from_static("user-agent");
const HEADER_ACCEPT_NAME: HeaderName = HeaderName::from_static("accept");
const HEADER_CONNECTION_NAME: HeaderName = HeaderName::from_static("connection");
const HEADER_ORIGINATOR_NAME: HeaderName = HeaderName::from_static("originator");
const HEADER_ORIGINATOR: &str = "codex_cli_rs";

pub(crate) fn apply_codex_headers(headers: &mut HeaderMap, _inbound: &HeaderMap) {
    force_header(headers, &HEADER_OPENAI_BETA_NAME, HEADER_OPENAI_BETA);
    if !headers.contains_key(&HEADER_SESSION_ID_NAME) {
        if let Ok(value) = HeaderValue::from_str(&generate_session_id()) {
            headers.insert(HEADER_SESSION_ID_NAME, value);
        }
    }
    force_header(headers, &HEADER_USER_AGENT_NAME, DEFAULT_USER_AGENT);
    force_header(headers, &HEADER_ORIGINATOR_NAME, HEADER_ORIGINATOR);
    force_header(headers, &HEADER_ACCEPT_NAME, "text/event-stream");
    force_header(headers, &HEADER_CONNECTION_NAME, "Keep-Alive");
}

fn force_header(headers: &mut HeaderMap, name: &HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name.clone(), value);
    }
}

fn generate_session_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
