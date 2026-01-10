use super::*;

#[test]
fn retryable_status_matches_new_api_policy() {
    assert!(is_retryable_status(StatusCode::FORBIDDEN));
    assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
    assert!(is_retryable_status(StatusCode::TEMPORARY_REDIRECT));
    assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));

    // new-api excludes 504/524 timeouts from retries.
    assert!(!is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
    assert!(!is_retryable_status(StatusCode::from_u16(524).expect("524")));

    assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
}

#[test]
fn extract_query_param_reads_key_value() {
    let value = extract_query_param("/v1beta/models/x:generateContent?key=abc&foo=bar", "key");
    assert_eq!(value.as_deref(), Some("abc"));
}

#[test]
fn ensure_query_param_overrides_existing_value() {
    let url = "https://example.com/v1beta/models/x:generateContent?foo=bar&key=old";
    let updated = ensure_query_param(url, "key", "new").expect("updated url");
    assert!(updated.contains("foo=bar"));
    assert!(updated.contains("key=new"));
    assert!(!updated.contains("key=old"));
}

#[test]
fn redact_query_param_value_hides_secret() {
    let message = "error sending request for url (https://example.com/path?key=SECRET&foo=bar)";
    let redacted = redact_query_param_value(message, "key");
    assert!(redacted.contains("key=***"));
    assert!(!redacted.contains("SECRET"));
    assert!(redacted.contains("foo=bar"));
}
