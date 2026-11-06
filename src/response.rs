//! The provider's JSON envelope, decoded once for every endpoint.
//!
//! Two rules from the documentation drive this module:
//!
//! 1. The HTTP status is authoritative. An authentication failure answers
//!    `{"message": "Unauthorized."}` with **no** `status` field, so a client
//!    that only reads the body cannot tell it apart from a malformed success.
//! 2. Successful answers repeat the code in the body (`"status": 200`). When
//!    both are present they agree; when only the body carries a 4xx/5xx code,
//!    that code is honoured too. A body-level 429 is the same rate-limit
//!    signal as an HTTP 429, including the `Retry-After` hint. Both sources
//!    use one classifier, so a body-level 1xx/3xx or out-of-range code cannot
//!    silently become a successful envelope.
//!
//! ```
//! use letterxpress::transport::Response;
//!
//! // The documented auth-failure shape, with no `status` field at all.
//! let response = Response::json_response(401, r#"{"message": "Unauthorized."}"#);
//! let error = letterxpress::parse_envelope("GET /v3/balance", response).unwrap_err();
//! assert!(matches!(error, letterxpress::Error::Unauthorized { .. }));
//! ```

use std::time::Duration;

use serde_json::Value;

use crate::transport::Response;
use crate::{Error, Result};

/// A decoded provider envelope: `{status, message, data}`.
///
/// Returned by [`parse_envelope`] so callers (and the fuzz target) can inspect
/// the documented framing without going through a [`Client`](crate::Client).
///
/// ```
/// use letterxpress::transport::Response;
///
/// let envelope = letterxpress::parse_envelope(
///     "GET /v3/balance",
///     Response::json_response(200, r#"{"status":200,"message":"OK","data":{"balance":1.0}}"#),
/// )?;
/// assert_eq!(envelope.message(), Some("OK"));
/// assert_eq!(envelope.status(), Some(200));
/// assert_eq!(envelope.data().unwrap()["balance"], 1.0);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct Envelope {
    /// The HTTP status line the answer arrived with.
    pub(crate) http_status: u16,
    /// `status` from the body, when present.
    pub(crate) status: Option<u16>,
    /// `message` rendered as text, when present.
    pub(crate) message: Option<String>,
    /// `data`, when present and not null.
    pub(crate) data: Option<Value>,
    /// A `pagination` block that sits beside `data` instead of inside it —
    /// the documented e-mail-job list does exactly that.
    pub(crate) pagination: Option<Value>,
}

impl Envelope {
    /// The `data` payload, when the provider sent one.
    ///
    /// A bodiless success (`204 No Content`, or `Content-Length: 0`) and a
    /// literal `"data": null` have the same representation: no payload.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let envelope = letterxpress::parse_envelope("GET /v3/balance", Response::json_response(200, r#"{"status":200,"data":null}"#))?;
    /// assert!(envelope.data().is_none());
    ///
    /// let empty = letterxpress::parse_envelope("DELETE /v3/printjobs/{id}", Response::new(204, std::iter::empty(), Vec::new()))?;
    /// assert!(empty.data().is_none());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn data(&self) -> Option<&Value> {
        self.data.as_ref()
    }

    /// The `message` rendered as text, when present.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let envelope = letterxpress::parse_envelope("GET /v3/balance", Response::json_response(200, r#"{"status":200,"message":"OK"}"#))?;
    /// assert_eq!(envelope.message(), Some("OK"));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The body-level `status`, when present.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let envelope = letterxpress::parse_envelope("GET /v3/balance", Response::json_response(200, r#"{"status":200,"data":{}}"#))?;
    /// assert_eq!(envelope.status(), Some(200));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// The `pagination` block, when the provider put it beside `data`.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let envelope = letterxpress::parse_envelope("GET /v3/emailjobs", Response::json_response(200, r#"{"status":200,"data":{"id":1},"pagination":{"total":1}}"#))?;
    /// assert_eq!(envelope.pagination().unwrap()["total"], 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn pagination(&self) -> Option<&Value> {
        self.pagination.as_ref()
    }

    /// The `data` payload, or a typed missing-field error.
    pub(crate) fn require_data(self, endpoint: &'static str) -> Result<Value> {
        self.data.ok_or(Error::MissingField {
            endpoint,
            field: "data",
        })
    }

    /// A `DELETE` answer: no data, only a message.
    pub(crate) fn deletion(self) -> crate::Deletion {
        crate::Deletion {
            message: self.message.unwrap_or_default(),
        }
    }
}

/// Decodes one response into an [`Envelope`], mapping every documented failure
/// to a typed error first.
///
/// Public so the fuzz target can hammer it directly; not part of the stable,
/// documented surface (the [`Client`](crate::Client) is).
///
/// # Errors
///
/// [`Error::Unauthorized`], [`Error::Forbidden`], [`Error::NotFound`],
/// [`Error::MethodNotAllowed`], [`Error::RateLimited`], [`Error::Rejected`],
/// [`Error::Upstream`], [`Error::Json`], [`Error::UnexpectedStatus`] and
/// [`Error::UnexpectedShape`].
///
/// An empty or whitespace-only body is an envelope with no `data`, exactly like
/// a literal `null` body. Endpoints that require a payload still report
/// [`Error::MissingField`] from their own decoding step.
///
/// A 2xx body whose top level is an array, string, number or boolean is not an
/// envelope and reports [`Error::UnexpectedShape`]. Status classification runs
/// first, so an error response still reports its HTTP or body-level status.
///
/// ```
/// use letterxpress::transport::Response;
///
/// let ok = Response::json_response(200, r#"{"status":200,"message":"OK","data":{"balance":1.0}}"#);
/// let envelope = letterxpress::parse_envelope("GET /v3/balance", ok)?;
/// assert_eq!(envelope.data().unwrap()["balance"], 1.0);
///
/// let limited = Response::new(429, [("Retry-After".to_string(), "30".to_string())], Vec::new());
/// let error = letterxpress::parse_envelope("POST /v3/printjobs", limited).unwrap_err();
/// assert_eq!(error.status(), Some(429));
///
/// let body_limited = Response::new(200, [("Retry-After".to_string(), "30".to_string())], br#"{"status":429,"message":"Too Many Requests"}"#.to_vec());
/// let error = letterxpress::parse_envelope("POST /v3/printjobs", body_limited).unwrap_err();
/// assert!(matches!(error, letterxpress::Error::RateLimited { retry_after: Some(delay) } if delay.as_secs() == 30));
///
/// let redirect = Response::new(302, std::iter::empty(), Vec::new());
/// let error = letterxpress::parse_envelope("GET /v3/balance", redirect).unwrap_err();
/// assert!(matches!(error, letterxpress::Error::UnexpectedStatus { status: 302, .. }));
///
/// let empty = letterxpress::parse_envelope("DELETE /v3/printjobs/{id}", Response::new(204, std::iter::empty(), Vec::new()))?;
/// assert!(empty.data().is_none());
///
/// let list = Response::json_response(200, r#"[{"id":1},{"id":2}]"#);
/// let error = letterxpress::parse_envelope("GET /v3/emailjobs", list).unwrap_err();
/// assert!(matches!(error, letterxpress::Error::UnexpectedShape { .. }));
///
/// let null = letterxpress::parse_envelope("GET /v3/balance", Response::json_response(200, "null"))?;
/// assert!(null.data().is_none());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[allow(clippy::needless_pass_by_value)]
pub fn parse_envelope(endpoint: &'static str, response: Response) -> Result<Envelope> {
    let http_status = response.status();
    let body = parse_body(http_status, &response)?;
    let message = body.as_ref().and_then(extract_message);
    let body_status = body
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(status_of);

    // The HTTP status decides first: it is the only signal that is always
    // present (the documented 401 body carries no `status` field).
    let body_fallback = || body_snippet(&response);
    if let Some(error) = http_error(
        endpoint,
        http_status,
        message.as_deref(),
        &response,
        body_fallback,
    ) {
        return Err(error);
    }
    // A body-level 4xx/5xx code is honoured when the HTTP status said 2xx.
    if let Some(error) = body_error(endpoint, body_status, message.as_deref(), &response) {
        return Err(error);
    }

    // Status classification above is authoritative; only now is the successful
    // body's shape checked. An object is the documented envelope, and null is
    // the same no-payload answer as an empty body.
    let body = match body {
        Some(Value::Object(map)) => Value::Object(map),
        Some(Value::Null) | None => Value::Null,
        Some(other) => {
            return Err(Error::UnexpectedShape {
                endpoint,
                message: format!(
                    "expected an object or null, got {}",
                    json_shape_name(&other)
                ),
                source: None,
            });
        }
    };
    Ok(Envelope {
        http_status,
        status: body_status,
        message,
        data: body.get("data").filter(|value| !value.is_null()).cloned(),
        pagination: body
            .get("pagination")
            .filter(|value| !value.is_null())
            .cloned(),
    })
}

/// Maps a non-2xx HTTP status to the documented error, using the response
/// headers for `Retry-After` and the body text when there is no envelope.
fn http_error(
    endpoint: &'static str,
    status: u16,
    message: Option<&str>,
    response: &Response,
    body_fallback: impl FnOnce() -> String,
) -> Option<Error> {
    if status_class(status) == StatusClass::Success {
        return None;
    }
    let text = message.map_or_else(body_fallback, str::to_owned);
    Some(status_error(endpoint, status, &text, response))
}

/// Maps a body-level status when the HTTP status said 2xx, using the same
/// classifier and `Retry-After` handling as the HTTP-status path.
fn body_error(
    endpoint: &'static str,
    status: Option<u16>,
    message: Option<&str>,
    response: &Response,
) -> Option<Error> {
    let status = status?;
    if status_class(status) == StatusClass::Success {
        return None;
    }
    Some(status_error(
        endpoint,
        status,
        message.unwrap_or_default(),
        response,
    ))
}

/// The only classification shared by HTTP and body-level status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusClass {
    Success,
    Unauthorized,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    RateLimited,
    Upstream,
    Rejected,
    UnexpectedStatus,
    UnexpectedShape,
}

/// Classifies one status value without regard to where it came from.
fn status_class(status: u16) -> StatusClass {
    match status {
        200..=299 => StatusClass::Success,
        401 => StatusClass::Unauthorized,
        403 => StatusClass::Forbidden,
        404 => StatusClass::NotFound,
        405 => StatusClass::MethodNotAllowed,
        429 => StatusClass::RateLimited,
        500..=599 => StatusClass::Upstream,
        400..=499 => StatusClass::Rejected,
        // Redirects are not followed, because following one would send the
        // credential to whatever host the redirect names.
        100..=199 | 300..=399 => StatusClass::UnexpectedStatus,
        _ => StatusClass::UnexpectedShape,
    }
}

/// Builds the typed error for a non-success class. The text is bounded here as
/// a final guard, independent of whether it came from the envelope or a raw
/// body snippet.
fn status_error(endpoint: &'static str, status: u16, text: &str, response: &Response) -> Error {
    let text = cap_error_text(text);
    match status_class(status) {
        StatusClass::Success => {
            // Both callers filter success first. Treat a future caller that
            // forgets as a terminal classification rather than panicking.
            Error::UnexpectedShape {
                endpoint,
                message: format!("unexpected response status {status}: {text}"),
                source: None,
            }
        }
        StatusClass::Unauthorized => Error::Unauthorized {
            message: if text.is_empty() {
                "Unauthorized.".to_string()
            } else {
                text
            },
        },
        StatusClass::Forbidden => Error::Forbidden { message: text },
        StatusClass::NotFound => Error::NotFound { message: text },
        StatusClass::MethodNotAllowed => Error::MethodNotAllowed { message: text },
        StatusClass::RateLimited => Error::RateLimited {
            retry_after: parse_retry_after(response),
        },
        StatusClass::Upstream => Error::Upstream {
            status,
            message: text,
        },
        StatusClass::Rejected => Error::Rejected {
            status,
            message: text,
        },
        StatusClass::UnexpectedStatus => Error::UnexpectedStatus {
            status,
            endpoint,
            message: text,
        },
        StatusClass::UnexpectedShape => Error::UnexpectedShape {
            endpoint,
            message: format!("unexpected response status {status}: {text}"),
            source: None,
        },
    }
}

/// Names a JSON shape for a diagnostic message.
fn json_shape_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Parses the body, representing an empty body as no payload and tolerating
/// non-JSON error pages.
fn parse_body(status: u16, response: &Response) -> Result<Option<Value>> {
    if response.body().iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(response.body()) {
        Ok(Value::Null) => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(source) => {
            if (200..300).contains(&status) {
                Err(Error::Json { source })
            } else {
                // A proxy or WAF answered with HTML; the status is still the
                // useful information.
                Ok(None)
            }
        }
    }
}

/// Renders the envelope's message field as text, whatever shape it has.
fn extract_message(body: &Value) -> Option<String> {
    for key in ["message", "error", "errors"] {
        let Some(value) = body.get(key) else {
            continue;
        };
        let rendered = match value {
            Value::Null => continue,
            Value::String(text) => cap_error_text(text),
            Value::Array(entries) => render_error_entries(entries),
            other => render_scalar(other),
        };
        if !rendered.is_empty() {
            return Some(rendered);
        }
    }
    None
}

const MAX_ERROR_ENTRIES: usize = 16;

/// Joins at most [`MAX_ERROR_ENTRIES`] entries and never builds more than the
/// shared error-text budget. A provider can send arbitrarily many validation
/// entries; only the useful prefix belongs in an error or a log line.
fn render_error_entries(entries: &[Value]) -> String {
    let mut rendered = String::new();
    let mut truncated = entries.len() > MAX_ERROR_ENTRIES;
    for entry in entries.iter().take(MAX_ERROR_ENTRIES) {
        let entry = render_scalar(entry);
        if entry.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            if rendered.chars().count() >= ERROR_TEXT_CHARS {
                truncated = true;
                break;
            }
            rendered.push_str("; ");
        }
        let remaining = ERROR_TEXT_CHARS.saturating_sub(rendered.chars().count());
        let mut chars = entry.chars();
        rendered.extend(chars.by_ref().take(remaining));
        if chars.next().is_some() {
            truncated = true;
            break;
        }
    }
    if truncated {
        rendered.push('…');
    }
    rendered
}

/// A scalar or object rendered as one line of text.
fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => cap_error_text(text),
        Value::Object(map) => map
            .get("message")
            .map_or_else(|| "[object]".to_string(), render_scalar),
        // Error entries are diagnostics. Avoid serialising nested arrays or
        // objects only to discard nearly all of the resulting text.
        Value::Array(_) => "[array]".to_string(),
        other => cap_error_text(&other.to_string()),
    }
}

/// Reads a status code from a JSON number or a numeric string.
fn status_of(value: &Value) -> Option<u16> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|code| u16::try_from(code).ok()),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

const ERROR_TEXT_CHARS: usize = 512;
const ERROR_TEXT_PREFIX_BYTES: usize = ERROR_TEXT_CHARS * 4;

/// The first 512 characters of the raw body, for error messages when there is
/// no envelope. Never more: a large HTML error page must not end up in a log
/// line. Only a prefix large enough to hold 512 four-byte UTF-8 characters is
/// copied before the cap is applied. Leading whitespace is skipped within that
/// prefix so a padded error page still contributes useful text.
fn body_snippet(response: &Response) -> String {
    let body = response.body();
    let prefix = &body[..body.len().min(ERROR_TEXT_PREFIX_BYTES)];
    let text = String::from_utf8_lossy(prefix);
    let mut snippet = cap_error_text(text.trim_start());
    if body.len() > prefix.len() && !snippet.ends_with('…') {
        snippet.push('…');
    }
    snippet
}

/// Trims provider-controlled text and keeps at most 512 characters, adding an
/// ellipsis when anything was discarded. This is the one budget used for raw
/// body snippets and envelope messages alike.
fn cap_error_text(text: &str) -> String {
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let mut capped: String = chars.by_ref().take(ERROR_TEXT_CHARS).collect();
    if chars.next().is_some() {
        capped.push('…');
    }
    capped
}

/// `Retry-After` in whole seconds, clamped to
/// [`MAX_RETRY_AFTER`](crate::MAX_RETRY_AFTER).
///
/// Only the delta-seconds form is honoured. HTTP-date values are ignored
/// (the provider documents no format, and a wrong guess is worse than the
/// caller's own backoff), and so is anything that is not a plain `u64`: a
/// negative number, a fraction, a comma list. Negative values are refused
/// rather than clamped to zero, because `Retry-After: -1` is not a hint to
/// retry immediately — it is a header nobody meant to send.
///
/// The ceiling is applied here, where provider-controlled data enters the
/// crate, so no decoded error carries a wait longer than five minutes. See
/// [`MAX_RETRY_AFTER`](crate::MAX_RETRY_AFTER) for why that is the right bound
/// for a 120-per-minute contract.
fn parse_retry_after(response: &Response) -> Option<Duration> {
    let seconds = response.header("retry-after")?.trim().parse::<u64>().ok()?;
    Some(crate::error::bounded_retry_after(Duration::from_secs(
        seconds,
    )))
}

/// Decodes a base64 payload from a response, with the documented ceiling
/// checked before allocation.
pub(crate) fn decode_base64(field: &'static str, encoded: &str) -> Result<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    let trimmed = encoded.trim();
    let Some(max_encoded) = base64::encoded_len(crate::MAX_PDF_BYTES, true) else {
        // The documented ceiling is far inside `usize`; fail closed if that
        // ever stops being representable rather than dropping the guard.
        return Err(Error::TooLarge {
            field,
            len: trimmed.len(),
            max: 0,
        });
    };
    if trimmed.len() > max_encoded {
        return Err(Error::TooLarge {
            field,
            len: trimmed.len(),
            max: max_encoded,
        });
    }
    let bytes = STANDARD
        .decode(trimmed)
        .map_err(|source| Error::Base64 { field, source })?;
    if bytes.len() > crate::MAX_PDF_BYTES {
        return Err(Error::TooLarge {
            field,
            len: bytes.len(),
            max: crate::MAX_PDF_BYTES,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(body: &str) -> Result<Envelope> {
        parse_envelope("GET /v3/balance", Response::json_response(200, body))
    }

    fn snippet_for(body: impl Into<Vec<u8>>) -> String {
        body_snippet(&Response::new(500, std::iter::empty(), body.into()))
    }

    #[test]
    fn envelope_accessors_expose_the_documented_parts() {
        let envelope =
            envelope(r#"{"status":200,"message":"OK","data":{"a":1},"pagination":{"total":2}}"#)
                .expect("envelope");
        assert_eq!(envelope.status(), Some(200));
        assert_eq!(envelope.message(), Some("OK"));
        assert_eq!(envelope.data().expect("data")["a"], 1);
        assert_eq!(envelope.pagination().expect("pagination")["total"], 2);
    }

    #[test]
    fn null_and_bodiless_successes_have_the_same_empty_answer() {
        for body in ["null", "", "   \n\t "] {
            for status in [200, 204, 205] {
                let response = Response::new(
                    status,
                    [("Content-Length".to_string(), body.len().to_string())],
                    body.as_bytes().to_vec(),
                );
                // A literal `null` body and a bodiless success decode to an
                // envelope with nothing in it; a caller that needs `data` then
                // gets a typed missing-field error.
                let envelope = parse_envelope("GET /v3/balance", response)
                    .unwrap_or_else(|error| panic!("{status} {body:?}: {error}"));
                assert!(envelope.data().is_none(), "{status} {body:?}");
                assert!(
                    matches!(
                        envelope.require_data("GET /v3/balance"),
                        Err(Error::MissingField { field: "data", .. })
                    ),
                    "{status} {body:?}"
                );
            }
        }
    }

    #[test]
    fn unauthorized_without_a_message_gets_the_documented_default() {
        let response = Response::new(401, std::iter::empty(), Vec::new());
        match parse_envelope("GET /v3/balance", response) {
            Err(Error::Unauthorized { message }) => assert_eq!(message, "Unauthorized."),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn successful_statuses_do_not_evaluate_the_body_fallback() {
        for status in [200, 299] {
            let calls = std::cell::Cell::new(0);
            let response = Response::new(status, std::iter::empty(), b"{}".to_vec());
            let error = http_error("GET /v3/balance", status, None, &response, || {
                calls.set(calls.get() + 1);
                "unused".to_string()
            });
            assert!(error.is_none(), "{status}");
            assert_eq!(calls.get(), 0, "{status} must not build a snippet");
        }

        for status in [199, 300] {
            let calls = std::cell::Cell::new(0);
            let response = Response::new(status, std::iter::empty(), b"ignored".to_vec());
            let error = http_error("GET /v3/balance", status, None, &response, || {
                calls.set(calls.get() + 1);
                "fallback".to_string()
            })
            .expect("a non-success status is an error");
            assert_eq!(calls.get(), 1, "{status} must use the fallback");
            assert!(error.to_string().contains("fallback"), "{status}: {error}");
        }
    }

    #[test]
    fn body_snippet_stops_at_the_character_boundary() {
        let just_inside = "a".repeat(ERROR_TEXT_CHARS - 1);
        assert_eq!(snippet_for(just_inside.clone()), just_inside);

        let at_limit = "a".repeat(ERROR_TEXT_CHARS);
        assert_eq!(snippet_for(at_limit.clone()), at_limit);

        let over_limit = "a".repeat(ERROR_TEXT_CHARS + 1);
        assert_eq!(
            snippet_for(over_limit),
            format!("{}…", "a".repeat(ERROR_TEXT_CHARS))
        );
    }

    #[test]
    fn body_snippet_holds_512_four_byte_characters() {
        let at_limit = "😀".repeat(ERROR_TEXT_CHARS);
        assert_eq!(at_limit.len(), ERROR_TEXT_PREFIX_BYTES);
        assert_eq!(snippet_for(at_limit.clone()), at_limit);

        let over_limit = format!("{at_limit}😀");
        assert_eq!(snippet_for(over_limit), format!("{at_limit}…"));
    }

    #[test]
    fn body_snippet_does_not_read_past_the_prefix_boundary() {
        let mut just_inside = vec![b' '; ERROR_TEXT_PREFIX_BYTES - 1];
        just_inside.push(b'x');
        assert_eq!(snippet_for(just_inside), "x");

        let mut just_outside = vec![b' '; ERROR_TEXT_PREFIX_BYTES];
        just_outside.push(b'x');
        assert_eq!(snippet_for(just_outside), "…");
    }

    #[test]
    fn body_snippet_skips_leading_whitespace_within_the_prefix() {
        let mut padded = vec![b' '; ERROR_TEXT_PREFIX_BYTES];
        padded.extend_from_slice(b"useful error");
        assert_eq!(snippet_for(padded), "…");

        let mut just_visible = vec![b' '; ERROR_TEXT_PREFIX_BYTES - 1];
        just_visible.extend_from_slice(b"x");
        assert_eq!(snippet_for(just_visible), "x");
    }

    #[test]
    fn informational_and_redirect_statuses_are_reported_without_method_context() {
        for status in [100, 199, 300, 399] {
            let response = Response::json_response(status, r#"{"message":"notice"}"#);
            let error = parse_envelope("GET /v3/balance", response).expect_err("must fail");
            assert!(
                matches!(&error, Error::UnexpectedStatus { status: actual, .. } if *actual == status),
                "{status}: {error:?}"
            );
            assert_eq!(error.status(), Some(status));
            assert!(!error.is_ambiguous());
            assert!(!error.is_retryable());
            assert!(
                !error.is_retryable_for(true),
                "{status}: a redirect is terminal even for a read"
            );
            assert!(error.to_string().contains(&status.to_string()));
        }

        for status in [200, 299] {
            let response = Response::json_response(status, r#"{"status":200,"data":{}}"#);
            assert!(
                parse_envelope("GET /v3/balance", response).is_ok(),
                "{status}"
            );
        }

        let error = parse_envelope(
            "POST /v3/printjobs",
            Response::new(302, std::iter::empty(), Vec::new()),
        )
        .expect_err("the parser has no method context");
        assert!(matches!(error, Error::UnexpectedStatus { status: 302, .. }));

        let response = Response::json_response(400, r#"{"message":"bad"}"#);
        assert!(matches!(
            parse_envelope("GET /v3/balance", response),
            Err(Error::Rejected { status: 400, .. })
        ));
    }

    #[test]
    fn body_level_authorisation_codes_are_honoured() {
        for (status, expected) in [
            (401, "unauthorized"),
            (403, "forbidden"),
            (404, "not_found"),
            (405, "method_not_allowed"),
        ] {
            let body = format!(r#"{{"status":{status},"message":"x"}}"#);
            let error = envelope(&body).expect_err("must fail");
            let matched = match (status, &error) {
                (401, Error::Unauthorized { .. }) => "unauthorized",
                (403, Error::Forbidden { .. }) => "forbidden",
                (404, Error::NotFound { .. }) => "not_found",
                (405, Error::MethodNotAllowed { .. }) => "method_not_allowed",
                _ => "other",
            };
            assert_eq!(matched, expected, "status {status}: {error:?}");
        }
    }

    #[test]
    fn body_level_non_success_codes_use_the_http_classifier() {
        for status in [100, 199, 300, 399] {
            let body = format!(r#"{{"status":{status},"message":"not successful"}}"#);
            let error = envelope(&body).expect_err("must fail");
            assert!(
                matches!(
                    &error,
                    Error::UnexpectedStatus {
                        status: actual,
                        endpoint: "GET /v3/balance",
                        message,
                    } if *actual == status && message == "not successful"
                ),
                "status {status}: {error:?}"
            );
        }

        for status in [0, 600, u16::MAX] {
            let body = format!(r#"{{"status":{status},"message":"out of range"}}"#);
            let error = envelope(&body).expect_err("must fail");
            assert!(
                matches!(
                    &error,
                    Error::UnexpectedShape {
                        endpoint: "GET /v3/balance",
                        message,
                        source: None,
                    } if message.contains(&status.to_string()) && message.contains("out of range")
                ),
                "status {status}: {error:?}"
            );
        }

        for status in [200, 204, 299] {
            let body = format!(r#"{{"status":{status},"data":{{}}}}"#);
            assert!(envelope(&body).is_ok(), "status {status}");
        }
    }

    #[test]
    fn messages_are_rendered_from_every_shape_the_api_uses() {
        let array =
            envelope(r#"{"status":200,"errors":["first","second"],"data":{}}"#).expect("ok");
        assert_eq!(array.message(), Some("first; second"));

        let object =
            envelope(r#"{"status":200,"error":{"message":"nested"},"data":{}}"#).expect("ok");
        assert_eq!(object.message(), Some("nested"));

        let number = envelope(r#"{"status":200,"message":42,"data":{}}"#).expect("ok");
        assert_eq!(number.message(), Some("42"));

        let null_message =
            envelope(r#"{"status":200,"message":null,"errors":["from errors"],"data":{}}"#)
                .expect("ok");
        assert_eq!(null_message.message(), Some("from errors"));

        let none = envelope(r#"{"status":200,"data":{}}"#).expect("ok");
        assert_eq!(none.message(), None);
    }

    #[test]
    fn provider_message_text_is_bounded_to_the_body_snippet_budget() {
        let huge = "x".repeat(100_000);
        let body = serde_json::json!({"status": 500, "message": huge});
        let error = envelope(&body.to_string()).expect_err("must fail");
        let Error::Upstream { message, .. } = error else {
            panic!("unexpected error");
        };
        assert_eq!(message.chars().count(), ERROR_TEXT_CHARS + 1);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn provider_error_arrays_cap_entries_and_joined_text() {
        let entries: Vec<Value> = (0..10_000)
            .map(|index| Value::String(format!("validation error {index}")))
            .collect();
        let body = serde_json::json!({"status": 400, "errors": entries});
        let error = envelope(&body.to_string()).expect_err("must fail");
        let Error::Rejected { message, .. } = error else {
            panic!("unexpected error");
        };
        assert!(message.chars().count() <= ERROR_TEXT_CHARS + 1);
        assert!(message.ends_with('…'));

        let exactly_capped: Vec<Value> = (0..MAX_ERROR_ENTRIES)
            .map(|_| Value::String("x".repeat(100)))
            .collect();
        let body = serde_json::json!({"status": 400, "errors": exactly_capped});
        let error = envelope(&body.to_string()).expect_err("must fail");
        let Error::Rejected { message, .. } = error else {
            panic!("unexpected error");
        };
        assert!(message.chars().count() <= ERROR_TEXT_CHARS + 1);
        assert!(message.ends_with('…'));
        assert!(!message.contains("validation error 16"));
    }

    #[test]
    fn a_status_as_a_string_still_counts() {
        let error = envelope(r#"{"status":"500","message":"boom"}"#).expect_err("must fail");
        assert!(matches!(error, Error::Upstream { status: 500, .. }));
    }

    #[test]
    fn the_base64_ceiling_uses_the_maintained_encoded_length() {
        let max_encoded = base64::encoded_len(crate::MAX_PDF_BYTES, true)
            .expect("the documented ceiling fits in usize");

        {
            // Exactly at the encoded ceiling: a valid padded payload.
            let mut at_ceiling = "A".repeat(max_encoded - 1);
            at_ceiling.push('=');
            let decoded = decode_base64("invoice.base64_data", &at_ceiling)
                .expect("the encoded ceiling is accepted");
            assert_eq!(decoded.len(), crate::MAX_PDF_BYTES);
        }

        // One padded quartet past it. This is valid base64 that the hand-rolled
        // `+ 4` accepted before decoding; the maintained helper rejects it
        // before allocating, and reports the encoded boundary.
        let encoded = "A".repeat(max_encoded + 4);
        assert!(matches!(
            decode_base64("invoice.base64_data", &encoded),
            Err(Error::TooLarge { len, max, .. })
                if len == max_encoded + 4 && max == max_encoded
        ));
    }

    #[test]
    fn valid_base64_decodes_and_invalid_base64_is_a_typed_error() {
        assert_eq!(decode_base64("x", "JVBERi0=").expect("pdf"), b"%PDF-");
        assert!(matches!(
            decode_base64("x", "!!!!"),
            Err(Error::Base64 { .. })
        ));
    }

    /// `Retry-After` is read in whole seconds and clamped at the ceiling; any
    /// value that is not a plain count of seconds yields no hint at all.
    #[test]
    fn retry_after_is_bounded_at_the_ceiling_and_unreadable_values_are_dropped() {
        fn hint(value: Option<&str>) -> Option<Duration> {
            let headers: Vec<(String, String)> = value
                .map(|value| vec![("Retry-After".to_string(), value.to_string())])
                .unwrap_or_default();
            parse_envelope("GET /v3/balance", Response::new(429, headers, Vec::new()))
                .expect_err("429 is an error")
                .retry_after()
        }

        // No header at all: no hint, which means back off, not retry now.
        assert_eq!(hint(None), None);

        // Zero is a legal delta-seconds value and is reported as written.
        assert_eq!(hint(Some("0")), Some(Duration::ZERO));

        // Just inside the ceiling, exactly at it, and one second past it.
        assert_eq!(hint(Some("299")), Some(Duration::from_secs(299)));
        assert_eq!(hint(Some("300")), Some(crate::MAX_RETRY_AFTER));
        assert_eq!(hint(Some("301")), Some(crate::MAX_RETRY_AFTER));

        // A day, and a value one above `u64::MAX` (which cannot be parsed).
        assert_eq!(hint(Some("86400")), Some(crate::MAX_RETRY_AFTER));
        assert_eq!(hint(Some("18446744073709551616")), None);

        // Surrounding whitespace is not a reason to drop a readable header.
        assert_eq!(hint(Some(" 45 ")), Some(Duration::from_secs(45)));

        // Everything else is unreadable: an HTTP-date, a fraction, a negative
        // number, a comma list, a unit suffix, scientific notation, an empty
        // value.
        for unreadable in [
            "",
            "   ",
            "-1",
            "30.5",
            "30s",
            "30, 60",
            "1e3",
            "Wed, 21 Oct 2015 07:28:00 GMT",
        ] {
            assert_eq!(hint(Some(unreadable)), None, "{unreadable:?}");
        }
    }
}
