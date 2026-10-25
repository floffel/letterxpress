//! Everything this crate can fail with, and what the caller can do about it.
//!
//! One `#[non_exhaustive]` enum with a `source` chain. Two questions have a
//! dedicated answer, because they decide whether a letter is sent twice:
//!
//! * [`Error::is_retryable`] — "another attempt is safe **and** likely to
//!   succeed". Only connection failures and rate limiting qualify.
//! * [`Error::is_ambiguous`] — "the provider may already have processed this
//!   request". A retry can produce a second, billed letter. The only safe
//!   recovery is to look the job up (by `notice` or the job list) before
//!   resubmitting.

use std::time::Duration;

use thiserror::Error;

/// Why a transport call produced no response.
///
/// The distinction matters: a connection that was never established cannot have
/// submitted a letter, a request that timed out after being written may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransportKind {
    /// DNS, TCP or TLS handshake failed — nothing was written to the socket.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert_eq!(TransportKind::Connect.to_string(), "connect");
    /// assert!(TransportKind::Connect.is_retryable());
    /// ```
    Connect,
    /// The request was written but no response arrived in time.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(TransportKind::Timeout.is_retryable());
    /// ```
    Timeout,
    /// The connection broke mid-flight (reset, truncated body, TLS alert).
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(TransportKind::Io.is_retryable());
    /// ```
    Io,
    /// The transport refused to build the request — a local configuration or
    /// adapter problem, never a server answer.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(!TransportKind::Protocol.is_retryable());
    /// ```
    Protocol,
}

impl TransportKind {
    /// Whether a *fresh* attempt is safe for an idempotent request.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(TransportKind::Connect.is_retryable());
    /// assert!(!TransportKind::Protocol.is_retryable());
    /// ```
    #[must_use]
    pub fn is_retryable(self) -> bool {
        !matches!(self, Self::Protocol)
    }
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Protocol => "protocol",
        };
        f.write_str(text)
    }
}

/// The error type returned by every fallible operation in this crate.
///
/// The enum is `#[non_exhaustive]`: new failure modes can appear without a
/// breaking release, so always match with a trailing `_` arm.
///
/// # Retry stance per variant
///
/// | Variant | `is_retryable` | `is_ambiguous` |
/// | --- | --- | --- |
/// | [`Error::Validation`] | no — fix the input | no |
/// | [`Error::Unauthorized`] / [`Error::Forbidden`] | no — fix the credentials | no |
/// | [`Error::NotFound`] / [`Error::MethodNotAllowed`] | no | no |
/// | [`Error::RateLimited`] | yes, after `Retry-After` | no |
/// | [`Error::Rejected`] | no — the provider refused the body | no |
/// | [`Error::Upstream`] | no (see below) | no |
/// | [`Error::Ambiguous`] | **no** — reconcile first | yes |
/// | [`Error::Transport`] | per [`TransportKind`] | no |
/// | [`Error::Json`] / [`Error::MissingField`] / [`Error::UnexpectedShape`] | no | no |
/// | [`Error::TooLarge`] / [`Error::Base64`] | no | no |
///
/// `Upstream` reports a 5xx *answer*, so the server is talking and the request
/// failed there. For a read it is safe to try again; for a submit the 5xx may
/// arrive after the job was created, which is why the client turns a 5xx on a
/// submit into [`Error::Ambiguous`] instead of `Upstream`. A caller that
/// deliberately retries a failed read gets `Upstream` and decides for itself.
///
/// ```
/// use letterxpress::Error;
///
/// let err = Error::RateLimited { retry_after: Some(std::time::Duration::from_secs(30)) };
/// assert!(err.is_retryable());
/// assert!(!err.is_ambiguous());
/// assert_eq!(err.status(), Some(429));
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// An argument would be rejected by the provider, or is unsafe to put on
    /// the wire. The request is never sent.
    #[error("invalid {field}: {message}")]
    Validation {
        /// Dotted path of the offending field, for example `letter.notice`.
        field: &'static str,
        /// What is wrong and what the accepted shape is.
        message: String,
    },

    /// The provider rejected the API key, the username or the account (HTTP
    /// 401), or answered with the documented body-only `{"message":
    /// "Unauthorized."}` envelope.
    #[error("letterxpress authentication failed: {message}")]
    Unauthorized {
        /// Message from the provider, or a description of what was missing.
        message: String,
    },

    /// The key is valid but not allowed to do this (HTTP 403).
    #[error("letterxpress access denied: {message}")]
    Forbidden {
        /// Message from the provider.
        message: String,
    },

    /// The job or invoice does not exist — or is no longer deletable (HTTP
    /// 404).
    #[error("letterxpress resource not found: {message}")]
    NotFound {
        /// Message from the provider.
        message: String,
    },

    /// The endpoint does not accept this verb (HTTP 405). In practice this
    /// means the request body was dropped, which some proxies do for `GET`.
    #[error("letterxpress method not allowed: {message}")]
    MethodNotAllowed {
        /// Message from the provider.
        message: String,
    },

    /// The documented limit of 120 requests per minute was hit (HTTP 429).
    ///
    /// Retrying is safe: the provider rejected the request before processing
    /// it. `retry_after` is the `Retry-After` header when the provider sent a
    /// parseable one.
    #[error("letterxpress rate limit reached{}", retry_suffix(.retry_after))]
    RateLimited {
        /// `Retry-After` as a duration, when present and parseable.
        retry_after: Option<Duration>,
    },

    /// The provider refused the request body (HTTP 400, or a body `status`
    /// in the 4xx range). Nothing was created.
    #[error("letterxpress rejected the request (http {status}): {message}")]
    Rejected {
        /// HTTP status, or the body `status` when the HTTP status was 2xx.
        status: u16,
        /// Provider message.
        message: String,
    },

    /// The provider answered with a 5xx. It is reachable, but the call failed
    /// there.
    #[error("letterxpress upstream error (http {status}): {message}")]
    Upstream {
        /// HTTP status.
        status: u16,
        /// Provider message, or a truncated body when there was no envelope.
        message: String,
    },

    /// The request may have been processed, but no usable answer arrived.
    ///
    /// This is only produced for non-idempotent calls (job submission) after a
    /// timeout, a mid-flight connection failure or a 5xx. **Do not resubmit
    /// blindly**: the letter may already be in production. Look the job up
    /// first — via [`Client::print_jobs`](crate::Client::print_jobs) with the
    /// `notice` you set on the letter, or via the returned tracking data — and
    /// only submit again if it is absent.
    #[error("request may have been processed: {message}")]
    Ambiguous {
        /// What was observed, for logs and for the reconciliation decision.
        message: String,
    },

    /// No response was obtained from the provider.
    #[error("transport error ({kind}): {message}")]
    Transport {
        /// Failure class; decides whether a retry can duplicate a letter.
        kind: TransportKind,
        /// Description from the transport.
        message: String,
    },

    /// A response body was not valid JSON.
    #[error("invalid JSON in response: {source}")]
    Json {
        /// The underlying `serde_json` error, with line and column.
        #[from]
        source: serde_json::Error,
    },

    /// A successful response was missing a field the client needs.
    #[error("response for `{endpoint}` is missing `{field}`")]
    MissingField {
        /// The endpoint that was called, for example `GET /v3/printjobs/{id}`.
        endpoint: &'static str,
        /// The field that should have been there.
        field: &'static str,
    },

    /// A response parsed as JSON but did not have the documented shape.
    #[error("response for `{endpoint}` has an unexpected shape: {message}")]
    UnexpectedShape {
        /// The endpoint that was called.
        endpoint: &'static str,
        /// What was expected and what arrived.
        message: String,
    },

    /// A PDF or text field exceeds a documented size ceiling. Checked before
    /// encoding or allocating.
    #[error("{field} is {len} bytes, the documented maximum is {max}")]
    TooLarge {
        /// Dotted path of the offending field.
        field: &'static str,
        /// Observed length in bytes (or characters, for text fields).
        len: usize,
        /// Documented maximum.
        max: usize,
    },

    /// A base64 payload in a response could not be decoded.
    #[error("invalid base64 in `{field}`: {message}")]
    Base64 {
        /// Field that carried the payload.
        field: &'static str,
        /// Decoder message.
        message: String,
    },

    /// The provider returned a CSV document that is not valid UTF-8.
    #[error("`{field}` is not valid UTF-8")]
    Utf8 {
        /// Field that carried the payload.
        field: &'static str,
    },
}

impl Error {
    /// Whether repeating the exact same request is safe and plausibly useful.
    ///
    /// `false` does not always mean "give up" — it means "a retry cannot be
    /// automatic", because the outcome depends on state the client cannot see
    /// (an [`Error::Upstream`] on a read, or an [`Error::Ambiguous`] submit).
    ///
    /// ```
    /// use letterxpress::{Error, TransportKind};
    ///
    /// let ambiguous = Error::Ambiguous { message: "timeout after 30s".into() };
    /// assert!(!ambiguous.is_retryable());
    /// assert!(ambiguous.is_ambiguous());
    ///
    /// let reset = Error::Transport { kind: TransportKind::Io, message: "reset".into() };
    /// assert!(reset.is_retryable());
    /// ```
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Transport { kind, .. } => kind.is_retryable(),
            _ => false,
        }
    }

    /// Whether the provider may already have acted on this request.
    ///
    /// ```
    /// use letterxpress::Error;
    ///
    /// assert!(Error::Ambiguous { message: "timeout".into() }.is_ambiguous());
    /// assert!(!Error::NotFound { message: "gone".into() }.is_ambiguous());
    /// ```
    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// The HTTP status this error carries, when there was one.
    ///
    /// ```
    /// use letterxpress::Error;
    ///
    /// let err = Error::Unauthorized { message: "Unauthorized.".into() };
    /// assert_eq!(err.status(), Some(401));
    /// ```
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized { .. } => Some(401),
            Self::Forbidden { .. } => Some(403),
            Self::NotFound { .. } => Some(404),
            Self::MethodNotAllowed { .. } => Some(405),
            Self::RateLimited { .. } => Some(429),
            Self::Rejected { status, .. } | Self::Upstream { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Convenience constructor for validation failures.
    ///
    /// ```
    /// use letterxpress::Error;
    ///
    /// let err = Error::invalid("letter.notice", "must not be empty");
    /// assert_eq!(err.status(), None);
    /// ```
    #[must_use]
    pub fn invalid(field: &'static str, message: impl Into<String>) -> Self {
        Self::Validation {
            field,
            message: message.into(),
        }
    }
}

/// `Retry-After` rendered for `Display`, or nothing when it is unknown.
#[allow(clippy::ref_option)]
fn retry_suffix(retry_after: &Option<Duration>) -> String {
    retry_after.map_or_else(String::new, |duration| {
        format!(", retry after {}s", duration.as_secs())
    })
}
