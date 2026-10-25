//! The provider's JSON envelope, decoded once for every endpoint.
//!
//! Two rules from the documentation drive this module:
//!
//! 1. The HTTP status is authoritative. An authentication failure answers
//!    `{"message": "Unauthorized."}` with **no** `status` field, so a client
//!    that only reads the body cannot tell it apart from a malformed success.
//! 2. Successful answers repeat the code in the body (`"status": 200`). When
//!    both are present they agree; when only the body carries a 4xx/5xx code,
//!    that code is honoured too.
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
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let envelope = letterxpress::parse_envelope("GET /v3/balance", Response::json_response(200, r#"{"status":200,"data":null}"#))?;
    /// assert!(envelope.data().is_none());
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
/// [`Error::Upstream`], [`Error::Json`] and [`Error::UnexpectedShape`].
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
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[allow(clippy::needless_pass_by_value)]
pub fn parse_envelope(endpoint: &'static str, response: Response) -> Result<Envelope> {
    let http_status = response.status();
    let body = parse_body(endpoint, http_status, &response)?;
    let message = body.as_ref().and_then(extract_message);
    let body_status = body
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(status_of);

    // The HTTP status decides first: it is the only signal that is always
    // present (the documented 401 body carries no `status` field).
    if let Some(error) = http_error(endpoint, http_status, message.as_deref(), &response) {
        return Err(error);
    }
    // A body-level 4xx/5xx code is honoured when the HTTP status said 2xx.
    if let Some(error) = body_error(body_status, message.as_deref()) {
        return Err(error);
    }

    let body = body.unwrap_or(Value::Null);
    Ok(Envelope {
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
) -> Option<Error> {
    let fallback = || body_snippet(response);
    let text = message.map_or_else(fallback, str::to_owned);
    match status {
        401 => Some(Error::Unauthorized {
            message: if text.is_empty() {
                "Unauthorized.".to_string()
            } else {
                text
            },
        }),
        403 => Some(Error::Forbidden { message: text }),
        404 => Some(Error::NotFound { message: text }),
        405 => Some(Error::MethodNotAllowed { message: text }),
        429 => Some(Error::RateLimited {
            retry_after: parse_retry_after(response),
        }),
        500..=599 => Some(Error::Upstream {
            status,
            message: text,
        }),
        400..=499 => Some(Error::Rejected {
            status,
            message: text,
        }),
        200..=299 => None,
        // 1xx/3xx: redirects are not followed, because following one would send
        // the credential to whatever host the redirect names.
        _ => Some(Error::UnexpectedShape {
            endpoint,
            message: format!("unexpected HTTP status {status}: {text}"),
        }),
    }
}

/// Maps a body-level status when the HTTP status said 2xx.
fn body_error(status: Option<u16>, message: Option<&str>) -> Option<Error> {
    let status = status?;
    let text = message.unwrap_or_default().to_string();
    match status {
        401 => Some(Error::Unauthorized { message: text }),
        403 => Some(Error::Forbidden { message: text }),
        404 => Some(Error::NotFound { message: text }),
        405 => Some(Error::MethodNotAllowed { message: text }),
        500..=599 => Some(Error::Upstream {
            status,
            message: text,
        }),
        400..=499 => Some(Error::Rejected {
            status,
            message: text,
        }),
        _ => None,
    }
}

/// Parses the body, tolerating an empty body and non-JSON error pages.
fn parse_body(endpoint: &'static str, status: u16, response: &Response) -> Result<Option<Value>> {
    if response.body().iter().all(u8::is_ascii_whitespace) {
        // An empty body is only acceptable when the status already explains it;
        // a 2xx without a body has nothing to decode.
        return if (200..300).contains(&status) {
            Err(Error::MissingField {
                endpoint,
                field: "data",
            })
        } else {
            Ok(None)
        };
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
            Value::String(text) => text.clone(),
            Value::Array(entries) => entries
                .iter()
                .map(render_scalar)
                .collect::<Vec<_>>()
                .join("; "),
            other => render_scalar(other),
        };
        let rendered = rendered.trim().to_string();
        if !rendered.is_empty() {
            return Some(rendered);
        }
    }
    None
}

/// A scalar or object rendered as one line of text.
fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("message")
            .map_or_else(|| value.to_string(), render_scalar),
        other => other.to_string(),
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

/// The first 512 characters of the raw body, for error messages when there is
/// no envelope. Never more: a 50 MB HTML error page must not end up in a log
/// line.
fn body_snippet(response: &Response) -> String {
    let text = String::from_utf8_lossy(response.body());
    let trimmed = text.trim();
    let mut snippet: String = trimmed.chars().take(512).collect();
    if trimmed.chars().count() > 512 {
        snippet.push('…');
    }
    snippet
}

/// `Retry-After` as a duration. HTTP-date values are ignored (the provider
/// documents no format, and a wrong guess is worse than the caller's own
/// backoff).
fn parse_retry_after(response: &Response) -> Option<Duration> {
    response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Decodes a base64 payload from a response, with the documented ceiling
/// checked before allocation.
pub(crate) fn decode_base64(field: &'static str, encoded: &str) -> Result<Vec<u8>> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    let trimmed = encoded.trim();
    let max_encoded = crate::MAX_PDF_BYTES.div_ceil(3) * 4 + 4;
    if trimmed.len() > max_encoded {
        return Err(Error::TooLarge {
            field,
            len: trimmed.len(),
            max: max_encoded,
        });
    }
    let bytes = STANDARD.decode(trimmed).map_err(|error| Error::Base64 {
        field,
        message: error.to_string(),
    })?;
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
    fn a_null_body_is_an_empty_answer() {
        let response = Response::json_response(200, "null");
        // A literal `null` body decodes to an envelope with nothing in it; a
        // caller that needs `data` then gets a typed missing-field error.
        let envelope = parse_envelope("GET /v3/balance", response).expect("envelope");
        assert!(envelope.data().is_none());
        assert!(matches!(
            envelope.require_data("GET /v3/balance"),
            Err(Error::MissingField { field: "data", .. })
        ));
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
    fn a_status_as_a_string_still_counts() {
        let error = envelope(r#"{"status":"500","message":"boom"}"#).expect_err("must fail");
        assert!(matches!(error, Error::Upstream { status: 500, .. }));
    }

    #[test]
    fn oversized_base64_is_refused_before_decoding() {
        let encoded = "A".repeat(crate::MAX_PDF_BYTES.div_ceil(3) * 4 + 8);
        assert!(matches!(
            decode_base64("invoice.base64_data", &encoded),
            Err(Error::TooLarge { .. })
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
}
