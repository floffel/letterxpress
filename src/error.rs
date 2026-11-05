//! Everything this crate can fail with, and what the caller can do about it.
//!
//! One `#[non_exhaustive]` enum with a `source` chain: every variant that wraps
//! a lower-level failure keeps it as `#[source]` — [`Error::Json`] the
//! `serde_json` error, [`Error::Transport`] the transport's own error,
//! [`Error::Base64`] the decoder's, and [`Error::UnexpectedShape`] the
//! `serde_json` error that rejected a decoded payload — so a `source()` walk
//! gets past the variant to the failure that actually explains it. Two
//! questions decide whether a letter is sent twice, and the error alone cannot
//! answer either:
//!
//! * [`Error::is_retryable_for`] — "another attempt is safe **and** likely to
//!   succeed **for this request**". Whether the request is idempotent is part
//!   of the question: a 5xx or a connection that broke mid-flight is a safe
//!   retry for a `GET`, and an unanswerable one for a submit that may already
//!   have produced a billed letter. [`Error::is_retryable`] is the conservative
//!   half — the answer for a request that is *not* idempotent — so a caller
//!   that cannot name the request still cannot duplicate one.
//! * [`Error::is_ambiguous`] — "the provider may already have processed this
//!   request". A retry can produce a second, billed letter. The only safe
//!   recovery is to look the job up (by `notice` or the job list) before
//!   resubmitting.
//!
//! A retry that is allowed is not always a retry that is due: [`Error::retry_after`]
//! carries the provider's own backoff hint when it sent one, and the crate
//! invents no delay of its own.

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
    /// assert!(TransportKind::Timeout.is_retryable_for(true));
    /// assert!(!TransportKind::Timeout.is_retryable());
    /// ```
    Timeout,
    /// The connection broke mid-flight (reset, truncated body, TLS alert).
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(TransportKind::Io.is_retryable_for(true));
    /// assert!(!TransportKind::Io.is_retryable());
    /// ```
    Io,
    /// The transport refused to build the request — a local configuration or
    /// adapter problem, never a server answer.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(!TransportKind::Protocol.is_retryable());
    /// assert!(!TransportKind::Protocol.is_retryable_for(true));
    /// ```
    Protocol,
}

impl TransportKind {
    /// Whether a fresh attempt is safe even for a request that is **not**
    /// idempotent — a submit, where a second attempt can bill twice.
    ///
    /// True only for [`TransportKind::Connect`], where nothing reached the
    /// provider. This is [`TransportKind::is_retryable_for`] with `false`; a
    /// read can also retry a timeout or a mid-flight failure.
    ///
    /// ```
    /// use letterxpress::TransportKind;
    ///
    /// assert!(TransportKind::Connect.is_retryable());
    /// assert!(!TransportKind::Timeout.is_retryable());
    /// assert!(!TransportKind::Protocol.is_retryable());
    /// ```
    #[must_use]
    pub fn is_retryable(self) -> bool {
        self.is_retryable_for(false)
    }

    /// Whether a fresh attempt is safe, given whether the failed request is
    /// idempotent.
    ///
    /// A connection that was never established wrote nothing, so it is safe
    /// whatever the request was. A timeout or a broken connection may have
    /// delivered the request: safe for a read, duplicate-prone for a submit.
    /// An adapter that refused to build the request fails the same way again,
    /// so it is never retryable.
    ///
    /// ```
    /// use letterxpress::{Method, TransportKind};
    ///
    /// // The same failure, decided by the request it belongs to.
    /// assert!(!TransportKind::Io.is_retryable_for(Method::Post.is_idempotent()));
    /// assert!(TransportKind::Io.is_retryable_for(Method::Get.is_idempotent()));
    ///
    /// // Nothing was written, so even a submit is safe to repeat.
    /// assert!(TransportKind::Connect.is_retryable_for(Method::Post.is_idempotent()));
    /// ```
    #[must_use]
    pub fn is_retryable_for(self, idempotent: bool) -> bool {
        match self {
            Self::Connect => true,
            Self::Timeout | Self::Io => idempotent,
            Self::Protocol => false,
        }
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

/// Why a non-idempotent request left an unknown outcome.
///
/// Everything a reconciliation needs is here, so it never has to search the
/// message: whether anything was written at all, and — when the provider did
/// answer — which HTTP status it answered with.
///
/// ```
/// use letterxpress::AmbiguousKind;
///
/// assert_eq!(AmbiguousKind::Timeout.status(), None);
/// assert_eq!(AmbiguousKind::Io.status(), None);
///
/// let upstream = AmbiguousKind::Upstream { status: 503 };
/// assert_eq!(upstream.status(), Some(503));
/// assert_eq!(upstream.to_string(), "upstream 503");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AmbiguousKind {
    /// The request was written, but no answer arrived in time.
    ///
    /// ```
    /// use letterxpress::AmbiguousKind;
    ///
    /// assert_eq!(AmbiguousKind::Timeout.status(), None);
    /// ```
    Timeout,
    /// The connection broke mid-flight (reset, truncated body, TLS alert).
    ///
    /// ```
    /// use letterxpress::AmbiguousKind;
    ///
    /// assert_eq!(AmbiguousKind::Io.status(), None);
    /// ```
    Io,
    /// The provider answered with an HTTP status that does not settle the
    /// outcome: a 5xx (it may have created the job before failing), a 1xx/3xx
    /// that was not followed, or a 2xx whose body did not decode (the job
    /// exists, its tracking data does not).
    ///
    /// ```
    /// use letterxpress::AmbiguousKind;
    ///
    /// assert_eq!(AmbiguousKind::Upstream { status: 500 }.status(), Some(500));
    /// ```
    Upstream {
        /// The HTTP status the provider answered with, or the body-level
        /// status when the HTTP status itself was 2xx.
        status: u16,
    },
}

impl AmbiguousKind {
    /// The HTTP status this kind carries, when the provider answered at all.
    ///
    /// ```
    /// use letterxpress::AmbiguousKind;
    ///
    /// assert_eq!(AmbiguousKind::Timeout.status(), None);
    /// assert_eq!(AmbiguousKind::Io.status(), None);
    /// assert_eq!(AmbiguousKind::Upstream { status: 502 }.status(), Some(502));
    /// ```
    #[must_use]
    pub fn status(self) -> Option<u16> {
        match self {
            Self::Timeout | Self::Io => None,
            Self::Upstream { status } => Some(status),
        }
    }
}

impl std::fmt::Display for AmbiguousKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("timeout"),
            Self::Io => f.write_str("io"),
            Self::Upstream { status } => write!(f, "upstream {status}"),
        }
    }
}

/// The error type returned by every fallible operation in this crate.
///
/// The enum is `#[non_exhaustive]`: new failure modes can appear without a
/// breaking release, so always match with a trailing `_` arm.
///
/// # Retry stance per variant
///
/// One row per variant; [`Error::Transport`] is split by [`TransportKind`] and
/// [`Error::Ambiguous`] by [`AmbiguousKind`], because in both cases the kind is
/// what decides. This table *is* the contract:
/// [`Error::status`], [`Error::is_retryable`], [`Error::is_retryable_for`],
/// [`Error::is_ambiguous`] and [`Error::retry_after`] answer for every variant
/// exactly as listed, and the test module at the end of this file asserts the
/// code and the text of the table against each other, so a variant cannot
/// arrive without a documented stance.
///
/// In the `status()` column, `none` means the error carries no HTTP status and
/// `carried` means the code the response had is reported back. `retry_after()`
/// answers `yes` only for a rate limit that carried the provider's hint.
///
/// `is_retryable()` is the answer for a request that is **not** idempotent — a
/// submit — and equals `is_retryable_for(false)`; the next column is the answer
/// for a read, a `PUT` or a `DELETE`.
///
/// | Variant | `status()` | `is_retryable()` | `is_retryable_for(true)` | `is_ambiguous()` | `retry_after()` |
/// | --- | --- | --- | --- | --- | --- |
/// | [`Error::Validation`] | none | no | no | no | no |
/// | [`Error::Unauthorized`] | 401 | no | no | no | no |
/// | [`Error::Forbidden`] | 403 | no | no | no | no |
/// | [`Error::NotFound`] | 404 | no | no | no | no |
/// | [`Error::MethodNotAllowed`] | 405 | no | no | no | no |
/// | [`Error::RateLimited`] | 429 | yes | yes | no | yes |
/// | [`Error::Rejected`] | carried | no | no | no | no |
/// | [`Error::Upstream`] | carried | no | yes | no | no |
/// | [`Error::UnexpectedStatus`] | carried | no | no | no | no |
/// | [`Error::Ambiguous`] / `Timeout` | none | no | no | yes | no |
/// | [`Error::Ambiguous`] / `Io` | none | no | no | yes | no |
/// | [`Error::Ambiguous`] / `Upstream` | carried | no | no | yes | no |
/// | [`Error::Transport`] / `Connect` | none | yes | yes | no | no |
/// | [`Error::Transport`] / `Timeout` | none | no | yes | no | no |
/// | [`Error::Transport`] / `Io` | none | no | yes | no | no |
/// | [`Error::Transport`] / `Protocol` | none | no | no | no | no |
/// | [`Error::Json`] | none | no | no | no | no |
/// | [`Error::MissingField`] | none | no | no | no | no |
/// | [`Error::UnexpectedShape`] | none | no | no | no | no |
/// | [`Error::TooLarge`] | none | no | no | no | no |
/// | [`Error::Base64`] | none | no | no | no | no |
/// | [`Error::Utf8`] | none | no | no | no | no |
///
/// A `yes` in either retry column is narrower than it reads: `RateLimited` is
/// retryable only once [`Error::retry_after`] has elapsed, and the transport
/// kinds are retryable only when the request may be repeated. Variant by
/// variant:
///
/// * `Validation` — the request was never sent, so the input has to change.
/// * `Unauthorized`, `Forbidden` — the credential has to change.
/// * `NotFound`, `MethodNotAllowed` — the resource or the verb is wrong.
/// * `RateLimited` — the provider dropped the request before processing it, so
///   a fresh attempt is safe *and* likely to succeed, whichever way the
///   request was made. The wait is the provider's to set: when `retry_after()`
///   answers, honour it before the next attempt.
/// * `Rejected` — the provider refused the body; the same body is refused
///   again.
/// * `Upstream` — the provider answered with a 5xx, so it is reachable and the
///   call failed there. For a read it is safe to try again; for a submit the
///   5xx may arrive after the job was created, which is why the client turns a
///   5xx on a submit into [`Error::Ambiguous`] instead of `Upstream`.
///   `is_retryable_for(true)` says the read is safe; nothing is left for the
///   caller to special-case.
/// * `Ambiguous` — the provider may already have acted on the request. A retry
///   can produce a second, billed letter, so reconcile first; this is the only
///   variant for which `is_ambiguous()` is true, and no context makes it
///   retryable. Its [`kind`](Error::Ambiguous::kind) says what was observed:
///   `Timeout` and `Io` mean no usable answer arrived, `Upstream { status }`
///   means the provider answered and that code is what [`Error::status`]
///   returns.
/// * `Transport` — a connection that was never established (`Connect`) cannot
///   have submitted a letter, so it is retryable in both contexts. `Timeout`
///   and `Io` may have delivered the request: retryable for a read only. The
///   client already promotes those two on a submit to [`Error::Ambiguous`]; the
///   predicate agrees with that rewrite instead of contradicting it.
/// * `Json`, `MissingField`, `UnexpectedShape` — the answer was unusable. On a
///   submit the client reports [`Error::Ambiguous`] instead, because a 2xx
///   means the job exists and is billed. `Json` carries no status because it is
///   raised from the body alone, where a `serde_json` error has no slot for
///   one: when the client decodes a response, a non-JSON body is `Json` only
///   for a 2xx, and a non-2xx with an unreadable body is classified by its HTTP
///   status instead, which yields `Rejected` or `Upstream` and does carry the
///   code.
/// * `TooLarge`, `Base64`, `Utf8` — a size or decoding limit hit on a
///   successful read, so the same bytes fail the same way. `Utf8` is the journal
///   CSV arriving as something other than UTF-8, the counterpart of `Base64`.
///
/// No variant is both retryable and ambiguous: a terminal error never hides a
/// possible duplicate.
///
/// An informational (1xx) or redirect (3xx) answer is
/// [`Error::UnexpectedStatus`]. Redirects are never followed, because the
/// `Location` could send the credential to another host. On a submit the
/// request has already been written, so the client promotes that signal to
/// [`Error::Ambiguous`]; on an idempotent call it remains terminal.
///
/// ```
/// use letterxpress::{Error, Method};
///
/// let err = Error::RateLimited { retry_after: Some(std::time::Duration::from_secs(30)) };
/// assert!(err.is_retryable());
/// assert!(err.is_retryable_for(Method::Post.is_idempotent()));
/// assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(30)));
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

    /// The documented limit of 120 requests per minute was hit, either as an
    /// HTTP 429 or as a body-level `status` of 429 when the HTTP status was
    /// 2xx.
    ///
    /// Retrying is safe: the provider rejected the request before processing
    /// it. `retry_after` is the `Retry-After` response header when the provider
    /// sent a parseable one, whichever path carried the 429.
    #[error("letterxpress rate limit reached{}", retry_suffix(.retry_after))]
    RateLimited {
        /// `Retry-After` as a duration, when present and parseable.
        retry_after: Option<Duration>,
    },

    /// The provider refused the request body (HTTP 400, or a body `status` in
    /// the 4xx range other than codes with their own variant, such as 401,
    /// 403, 404, 405 and 429). Nothing was created.
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

    /// The provider answered with an informational (1xx) or redirect (3xx)
    /// status. Redirects are not followed, because doing so could send the
    /// credential to another host.
    ///
    /// On a submit (a `POST`) this is reported as [`Error::Ambiguous`]
    /// instead: the request was already written and the job may exist.
    /// Idempotent calls keep this terminal error.
    ///
    /// ```
    /// use letterxpress::Error;
    ///
    /// let error = Error::UnexpectedStatus {
    ///     status: 302,
    ///     endpoint: "GET /v3/balance",
    ///     message: "Found".into(),
    /// };
    /// assert_eq!(error.status(), Some(302));
    /// assert!(!error.is_ambiguous());
    /// assert!(!error.is_retryable());
    /// ```
    #[error("unexpected HTTP status {status} in response for `{endpoint}`: {message}")]
    UnexpectedStatus {
        /// Informational or redirect status from the HTTP response.
        status: u16,
        /// The endpoint that was called.
        endpoint: &'static str,
        /// Provider message, or a truncated body when there was no envelope.
        message: String,
    },

    /// The request may have been processed, but no usable answer arrived.
    ///
    /// This is only produced for non-idempotent calls (job submission) after a
    /// timeout, a mid-flight connection failure, a 5xx, a 1xx/3xx answer, or a
    /// 2xx whose body could not be decoded — a 200 means the job exists and is
    /// billed even when its tracking data is unreadable. **Do not resubmit
    /// blindly**: the letter may already be in production. Look the job up
    /// first — via [`Client::print_jobs`](crate::Client::print_jobs) with the
    /// `notice` you set on the letter, or via the returned tracking data — and
    /// only submit again if it is absent.
    ///
    /// [`kind`](Error::Ambiguous::kind) says what was observed, so the
    /// reconciliation, the log line and an alerting rule can be driven by data
    /// rather than by the prose in `message`: a timeout means no answer
    /// arrived, `Upstream { status }` means one did and [`Error::status`]
    /// reports that code.
    ///
    /// ```
    /// use letterxpress::{AmbiguousKind, Error};
    ///
    /// let timeout = Error::Ambiguous {
    ///     kind: AmbiguousKind::Timeout,
    ///     message: "timeout after 30s".into(),
    /// };
    /// assert!(timeout.is_ambiguous());
    /// assert!(!timeout.is_retryable());
    /// assert_eq!(timeout.status(), None);
    ///
    /// let upstream = Error::Ambiguous {
    ///     kind: AmbiguousKind::Upstream { status: 503 },
    ///     message: "provider answered http 503 after the submit".into(),
    /// };
    /// assert_eq!(upstream.status(), Some(503));
    /// ```
    #[error("request may have been processed ({kind}): {message}")]
    Ambiguous {
        /// What was observed: the failure class, and the HTTP status when the
        /// provider answered.
        kind: AmbiguousKind,
        /// What was observed, in prose, for logs.
        message: String,
    },

    /// No response was obtained from the provider.
    ///
    /// `source` is the transport's own failure — `reqwest`/`hyper`/`rustls`
    /// with the real stack, or whatever a custom
    /// [`Transport`](crate::transport::Transport) implementation returns — kept
    /// typed instead of flattened into a string, so `source()` and the
    /// `anyhow`/`tracing` walks built on it reach the DNS, TLS or reset detail.
    ///
    /// ```
    /// use letterxpress::{Error, TransportKind};
    ///
    /// let error = Error::Transport {
    ///     kind: TransportKind::Connect,
    ///     source: Box::new(std::io::Error::other("dns lookup failed")),
    /// };
    /// assert!(error.is_retryable(), "nothing was written");
    /// assert!(std::error::Error::source(&error).is_some(), "the cause stays reachable");
    /// ```
    #[error("transport error ({kind}): {source}")]
    Transport {
        /// Failure class; decides whether a retry can duplicate a letter.
        kind: TransportKind,
        /// The transport's own error, with its cause chain intact.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A response body was not valid JSON.
    ///
    /// On a submit (a `POST`) a 2xx with an unreadable body is reported as
    /// [`Error::Ambiguous`] instead, because the job may exist.
    ///
    /// Carries no HTTP status: it is raised from the body alone, and a
    /// `serde_json` error has no slot for one. When the client decodes a
    /// response, a non-JSON body is `Json` only for a 2xx — a non-2xx with an
    /// unreadable body (a proxy's HTML page, say) is classified by its HTTP
    /// status instead and reported as [`Error::Rejected`], [`Error::RateLimited`]
    /// or [`Error::Upstream`], which do carry the code.
    #[error("invalid JSON in response: {source}")]
    Json {
        /// The underlying `serde_json` error, with line and column.
        #[from]
        source: serde_json::Error,
    },

    /// A successful response was missing a field the client needs.
    ///
    /// On a submit (a `POST`) this is reported as [`Error::Ambiguous`] instead.
    #[error("response for `{endpoint}` is missing `{field}`")]
    MissingField {
        /// The endpoint that was called, for example `GET /v3/printjobs/{id}`.
        endpoint: &'static str,
        /// The field that should have been there.
        field: &'static str,
    },

    /// A response parsed as JSON but did not have the documented shape.
    ///
    /// On a submit (a `POST`) this is reported as [`Error::Ambiguous`] instead.
    ///
    /// `source` is the `serde_json` error that rejected the payload when the
    /// envelope was JSON but its `data` could not be decoded into the
    /// documented model. Keeping it typed lets a caller inspect the exact
    /// failure — an unknown field, a wrong type, or a missing field — and
    /// downcast it with [`std::error::Error::source`]. It is `None` when the
    /// shape was rejected without a lower-level error, such as an unknown HTTP
    /// status.
    ///
    /// ```
    /// use letterxpress::Error;
    ///
    /// let source = serde_json::from_str::<u32>("\"not a number\"").unwrap_err();
    /// let error = Error::UnexpectedShape {
    ///     endpoint: "GET /v3/balance",
    ///     message: source.to_string(),
    ///     source: Some(Box::new(source)),
    /// };
    /// let source = std::error::Error::source(&error).expect("typed cause");
    /// assert!(source.downcast_ref::<serde_json::Error>().is_some());
    /// assert!(!error.is_retryable_for(true));
    /// ```
    #[error("response for `{endpoint}` has an unexpected shape: {message}")]
    UnexpectedShape {
        /// The endpoint that was called.
        endpoint: &'static str,
        /// What was expected and what arrived.
        message: String,
        /// The decoding failure that rejected the payload, when there was one.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
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
    ///
    /// `source` is the decoder's own [`base64::DecodeError`] — the invalid byte
    /// and the offset, or the input length — kept typed instead of flattened
    /// into a string.
    ///
    /// ```
    /// use base64::DecodeError;
    /// use letterxpress::Error;
    ///
    /// let error = Error::Base64 {
    ///     field: "invoice.base64_data",
    ///     source: DecodeError::InvalidByte(3, b'!'),
    /// };
    /// assert!(std::error::Error::source(&error).is_some(), "the cause stays reachable");
    /// assert!(error.to_string().contains("offset 3"), "{error}");
    /// ```
    #[error("invalid base64 in `{field}`: {source}")]
    Base64 {
        /// Field that carried the payload.
        field: &'static str,
        /// The decoder's error: which symbol, and where.
        #[source]
        source: base64::DecodeError,
    },

    /// The provider returned a CSV document that is not valid UTF-8.
    #[error("`{field}` is not valid UTF-8")]
    Utf8 {
        /// Field that carried the payload.
        field: &'static str,
    },
}

impl Error {
    /// Whether a fresh attempt is safe even for a request that is **not**
    /// idempotent — a submit, where a second attempt can bill twice.
    ///
    /// This is the conservative half of the decision and exactly
    /// [`Error::is_retryable_for`] with `false`: true for a rate limit and for
    /// a connection that never reached the provider, false for everything
    /// else, including failures that a read could retry. A caller that knows
    /// its request is idempotent should pass that in instead.
    ///
    /// ```
    /// use letterxpress::{AmbiguousKind, Error, TransportKind};
    ///
    /// let ambiguous = Error::Ambiguous {
    ///     kind: AmbiguousKind::Timeout,
    ///     message: "timeout after 30s".into(),
    /// };
    /// assert!(!ambiguous.is_retryable());
    /// assert!(ambiguous.is_ambiguous());
    ///
    /// let refused = Error::Transport {
    ///     kind: TransportKind::Connect,
    ///     source: Box::new(std::io::Error::other("dns")),
    /// };
    /// assert!(refused.is_retryable(), "nothing was written");
    ///
    /// let mid_flight = Error::Transport {
    ///     kind: TransportKind::Io,
    ///     source: Box::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
    /// };
    /// assert!(!mid_flight.is_retryable(), "a submit may already be billed");
    /// ```
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.is_retryable_for(false)
    }

    /// Whether repeating the exact same request is safe and plausibly useful,
    /// given whether that request is idempotent.
    ///
    /// `idempotent` is [`Method::is_idempotent`](crate::Method::is_idempotent)
    /// for the call that failed: `true` for `GET`, `PUT` and `DELETE`, `false`
    /// for a submit. It matters for two failures — a 5xx answer and a
    /// connection that broke mid-flight — because both may arrive after the
    /// provider has acted on the request: a `GET` can repeat them, a submit
    /// cannot without risking a second, billed letter.
    ///
    /// `false` does not always mean "give up" — it means "a retry cannot be
    /// automatic", because the outcome depends on state the client cannot see
    /// (an [`Error::Upstream`] on a submit, or any [`Error::Ambiguous`]). When
    /// the answer is `true` but [`Error::retry_after`] has a value, the
    /// provider asked to be left alone for that long first.
    ///
    /// ```
    /// use letterxpress::{Error, Method, TransportKind};
    ///
    /// let upstream = Error::Upstream { status: 503, message: "unavailable".into() };
    /// assert!(upstream.is_retryable_for(Method::Get.is_idempotent()));
    /// assert!(!upstream.is_retryable_for(Method::Post.is_idempotent()));
    ///
    /// let reset = Error::Transport {
    ///     kind: TransportKind::Io,
    ///     source: Box::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
    /// };
    /// assert!(!reset.is_retryable(), "the context-free answer is the submit one");
    /// assert!(reset.is_retryable_for(true), "a read can be repeated");
    /// ```
    #[must_use]
    pub fn is_retryable_for(&self, idempotent: bool) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Upstream { .. } => idempotent,
            Self::Transport { kind, .. } => kind.is_retryable_for(idempotent),
            _ => false,
        }
    }

    /// The delay the provider asked for before the next attempt, when it asked
    /// for one.
    ///
    /// `Some` only on [`Error::RateLimited`] that carried a parseable
    /// `Retry-After`; `None` on every other error, and on a rate limit without
    /// that header. The crate invents no delay of its own, so `None` is not by
    /// itself a licence to retry at once: decide with
    /// [`Error::is_retryable_for`] first, and pick your own backoff when the
    /// provider sent no hint.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use letterxpress::Error;
    ///
    /// let limited = Error::RateLimited { retry_after: Some(Duration::from_secs(30)) };
    /// assert!(limited.is_retryable_for(true));
    /// assert_eq!(limited.retry_after(), Some(Duration::from_secs(30)));
    ///
    /// let no_hint = Error::RateLimited { retry_after: None };
    /// assert!(no_hint.is_retryable_for(true));
    /// assert_eq!(no_hint.retry_after(), None, "no hint is not no wait");
    ///
    /// let gone = Error::NotFound { message: "gone".into() };
    /// assert_eq!(gone.retry_after(), None);
    /// ```
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// Whether the provider may already have acted on this request.
    ///
    /// ```
    /// use letterxpress::{AmbiguousKind, Error};
    ///
    /// let ambiguous = Error::Ambiguous {
    ///     kind: AmbiguousKind::Io,
    ///     message: "connection reset".into(),
    /// };
    /// assert!(ambiguous.is_ambiguous());
    /// assert!(!Error::NotFound { message: "gone".into() }.is_ambiguous());
    /// ```
    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// The HTTP status this error carries, when there was one.
    ///
    /// An [`Error::Ambiguous`] carries the provider's code when its
    /// [`kind`](Error::Ambiguous::kind) is [`AmbiguousKind::Upstream`], which is
    /// exactly the case a reconciliation has to look up; a timeout or a
    /// mid-flight failure has no status to report.
    ///
    /// ```
    /// use letterxpress::{AmbiguousKind, Error};
    ///
    /// let err = Error::Unauthorized { message: "Unauthorized.".into() };
    /// assert_eq!(err.status(), Some(401));
    ///
    /// let redirect = Error::UnexpectedStatus {
    ///     status: 302,
    ///     endpoint: "GET /v3/balance",
    ///     message: String::new(),
    /// };
    /// assert_eq!(redirect.status(), Some(302));
    ///
    /// let ambiguous = Error::Ambiguous {
    ///     kind: AmbiguousKind::Upstream { status: 500 },
    ///     message: "provider answered http 500 after the submit".into(),
    /// };
    /// assert_eq!(ambiguous.status(), Some(500));
    /// ```
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized { .. } => Some(401),
            Self::Forbidden { .. } => Some(403),
            Self::NotFound { .. } => Some(404),
            Self::MethodNotAllowed { .. } => Some(405),
            Self::RateLimited { .. } => Some(429),
            Self::Rejected { status, .. }
            | Self::Upstream { status, .. }
            | Self::UnexpectedStatus { status, .. } => Some(*status),
            Self::Ambiguous { kind, .. } => kind.status(),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Every variant name the documented table has to cover, sorted. Kept
    /// beside [`name_of`], which cannot compile without naming a new variant.
    const VARIANTS: &[&str] = &[
        "Ambiguous",
        "Base64",
        "Forbidden",
        "Json",
        "MethodNotAllowed",
        "MissingField",
        "NotFound",
        "RateLimited",
        "Rejected",
        "TooLarge",
        "Transport",
        "Unauthorized",
        "UnexpectedShape",
        "UnexpectedStatus",
        "Upstream",
        "Utf8",
        "Validation",
    ];

    /// One row of the documented table: the label the table writes, its answer
    /// cells, and a sample of the variant the row classifies.
    #[allow(clippy::struct_excessive_bools)] // one bool per documentation column
    struct Row {
        /// First column, exactly as the table spells it.
        label: &'static str,
        /// `status()` column: `none`, `carried`, or the fixed code.
        status: &'static str,
        /// `is_retryable()` column: the answer for a submit.
        retryable: bool,
        /// `is_retryable_for(true)` column: the answer for a read.
        retryable_for_read: bool,
        /// `is_ambiguous()` column.
        ambiguous: bool,
        /// `retry_after()` column: whether the provider's hint is carried.
        retry_after: bool,
        /// A constructed value of the variant.
        error: Error,
    }

    impl Row {
        /// The row as it must appear in the documentation, `///` included.
        fn documented(&self) -> String {
            format!(
                "/// | {} | {} | {} | {} | {} | {} |",
                self.label,
                self.status,
                yes_no(self.retryable),
                yes_no(self.retryable_for_read),
                yes_no(self.ambiguous),
                yes_no(self.retry_after)
            )
        }

        /// The variant this row classifies, as [`name_of`] spells it.
        fn variant(&self) -> &'static str {
            name_of(&self.error)
        }
    }

    /// A boolean cell as the table spells it.
    fn yes_no(value: bool) -> &'static str {
        if value {
            "yes"
        } else {
            "no"
        }
    }

    /// The variant name of `error`, matched with **no** wildcard arm.
    ///
    /// `Error` is `#[non_exhaustive]`, which only binds other crates: in here a
    /// new variant stops this module from compiling until it is named, and
    /// [`the_documented_table_covers_every_variant`] then insists on a row.
    fn name_of(error: &Error) -> &'static str {
        match error {
            Error::Validation { .. } => "Validation",
            Error::Unauthorized { .. } => "Unauthorized",
            Error::Forbidden { .. } => "Forbidden",
            Error::NotFound { .. } => "NotFound",
            Error::MethodNotAllowed { .. } => "MethodNotAllowed",
            Error::RateLimited { .. } => "RateLimited",
            Error::Rejected { .. } => "Rejected",
            Error::Upstream { .. } => "Upstream",
            Error::Ambiguous { .. } => "Ambiguous",
            Error::Transport { .. } => "Transport",
            Error::Json { .. } => "Json",
            Error::MissingField { .. } => "MissingField",
            Error::UnexpectedShape { .. } => "UnexpectedShape",
            Error::UnexpectedStatus { .. } => "UnexpectedStatus",
            Error::TooLarge { .. } => "TooLarge",
            Error::Base64 { .. } => "Base64",
            Error::Utf8 { .. } => "Utf8",
        }
    }

    /// The `status()` the row promises, for the row's own sample. The grammar
    /// of the cell is checked by [`the_status_column_uses_the_documented_grammar`].
    fn documented_status(row: &Row) -> Option<u16> {
        match row.status {
            "none" => None,
            "carried" => match &row.error {
                Error::Rejected { status, .. }
                | Error::Upstream { status, .. }
                | Error::UnexpectedStatus { status, .. } => Some(*status),
                Error::Ambiguous { kind, .. } => kind.status(),
                _ => None,
            },
            fixed => fixed.parse::<u16>().ok(),
        }
    }

    /// One documented row: label, the answer cells, and a sample.
    #[allow(clippy::fn_params_excessive_bools)] // one per documented column
    fn row(
        label: &'static str,
        status: &'static str,
        retryable: bool,
        retryable_for_read: bool,
        ambiguous: bool,
        retry_after: bool,
        error: Error,
    ) -> Row {
        Row {
            label,
            status,
            retryable,
            retryable_for_read,
            ambiguous,
            retry_after,
            error,
        }
    }

    /// A stand-in for whatever a real transport returned: a type with a
    /// `Display` and an `Error` impl, so the samples below carry a typed cause
    /// the way `reqwest` and a custom transport do.
    #[derive(Debug)]
    struct Cause(&'static str);

    impl std::fmt::Display for Cause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Cause {}

    /// A `Transport` error whose cause says `text` — the shape the table rows
    /// and the per-kind test build.
    fn transport_error(kind: TransportKind, text: &'static str) -> Error {
        Error::Transport {
            kind,
            source: Box::new(Cause(text)),
        }
    }

    /// The rows of the table in [`Error`]'s documentation, one per line.
    #[allow(clippy::too_many_lines)] // one row per line of the table, on purpose
    fn table() -> Vec<Row> {
        vec![
            row(
                "[`Error::Validation`]",
                "none",
                false,
                false,
                false,
                false,
                Error::invalid("letter.notice", "must not be empty"),
            ),
            row(
                "[`Error::Unauthorized`]",
                "401",
                false,
                false,
                false,
                false,
                Error::Unauthorized {
                    message: "Unauthorized.".into(),
                },
            ),
            row(
                "[`Error::Forbidden`]",
                "403",
                false,
                false,
                false,
                false,
                Error::Forbidden {
                    message: "Forbidden".into(),
                },
            ),
            row(
                "[`Error::NotFound`]",
                "404",
                false,
                false,
                false,
                false,
                Error::NotFound {
                    message: "Not Found".into(),
                },
            ),
            row(
                "[`Error::MethodNotAllowed`]",
                "405",
                false,
                false,
                false,
                false,
                Error::MethodNotAllowed {
                    message: "Method Not Allowed".into(),
                },
            ),
            row(
                "[`Error::RateLimited`]",
                "429",
                true,
                true,
                false,
                true,
                Error::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
            ),
            row(
                "[`Error::Rejected`]",
                "carried",
                false,
                false,
                false,
                false,
                Error::Rejected {
                    status: 400,
                    message: "Bad Request".into(),
                },
            ),
            row(
                "[`Error::Upstream`]",
                "carried",
                false,
                true,
                false,
                false,
                Error::Upstream {
                    status: 502,
                    message: "Bad Gateway".into(),
                },
            ),
            row(
                "[`Error::UnexpectedStatus`]",
                "carried",
                false,
                false,
                false,
                false,
                Error::UnexpectedStatus {
                    status: 302,
                    endpoint: "GET /v3/balance",
                    message: "Found".into(),
                },
            ),
            row(
                "[`Error::Ambiguous`] / `Timeout`",
                "none",
                false,
                false,
                true,
                false,
                Error::Ambiguous {
                    kind: AmbiguousKind::Timeout,
                    message: "timeout after 30s".into(),
                },
            ),
            row(
                "[`Error::Ambiguous`] / `Io`",
                "none",
                false,
                false,
                true,
                false,
                Error::Ambiguous {
                    kind: AmbiguousKind::Io,
                    message: "connection reset".into(),
                },
            ),
            row(
                "[`Error::Ambiguous`] / `Upstream`",
                "carried",
                false,
                false,
                true,
                false,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: 502 },
                    message: "provider answered http 502 after the submit".into(),
                },
            ),
            row(
                "[`Error::Transport`] / `Connect`",
                "none",
                true,
                true,
                false,
                false,
                transport_error(TransportKind::Connect, "dns"),
            ),
            row(
                "[`Error::Transport`] / `Timeout`",
                "none",
                false,
                true,
                false,
                false,
                transport_error(TransportKind::Timeout, "no answer"),
            ),
            row(
                "[`Error::Transport`] / `Io`",
                "none",
                false,
                true,
                false,
                false,
                transport_error(TransportKind::Io, "connection reset"),
            ),
            row(
                "[`Error::Transport`] / `Protocol`",
                "none",
                false,
                false,
                false,
                false,
                transport_error(TransportKind::Protocol, "no scripted response"),
            ),
            row(
                "[`Error::Json`]",
                "none",
                false,
                false,
                false,
                false,
                Error::Json {
                    source: serde_json::Error::io(std::io::Error::from(
                        std::io::ErrorKind::UnexpectedEof,
                    )),
                },
            ),
            row(
                "[`Error::MissingField`]",
                "none",
                false,
                false,
                false,
                false,
                Error::MissingField {
                    endpoint: "GET /v3/balance",
                    field: "data",
                },
            ),
            row(
                "[`Error::UnexpectedShape`]",
                "none",
                false,
                false,
                false,
                false,
                Error::UnexpectedShape {
                    endpoint: "GET /v3/balance",
                    message: "expected an object, got an array".into(),
                    source: None,
                },
            ),
            row(
                "[`Error::TooLarge`]",
                "none",
                false,
                false,
                false,
                false,
                Error::TooLarge {
                    field: "letter.base64_file",
                    len: 11,
                    max: 10,
                },
            ),
            row(
                "[`Error::Base64`]",
                "none",
                false,
                false,
                false,
                false,
                Error::Base64 {
                    field: "invoice.base64_data",
                    source: base64::DecodeError::InvalidByte(0, b'!'),
                },
            ),
            row(
                "[`Error::Utf8`]",
                "none",
                false,
                false,
                false,
                false,
                Error::Utf8 {
                    field: "journal.csv_base64",
                },
            ),
        ]
    }

    /// Each row is the answer the code gives, and the invariants that make the
    /// table worth having hold: nothing is both retryable and ambiguous in
    /// either context, `retry_after()` is only ever set on a rate limit, and
    /// every variant renders a message.
    #[test]
    fn every_row_matches_the_code() {
        for row in table() {
            let name = row.variant();
            assert_eq!(
                row.error.status(),
                documented_status(&row),
                "{name}: `status()` disagrees with the documented table ({})",
                row.status
            );
            assert_eq!(
                row.error.is_retryable(),
                row.retryable,
                "{name}: `is_retryable()` disagrees with the documented table"
            );
            assert_eq!(
                row.error.is_retryable_for(false),
                row.retryable,
                "{name}: `is_retryable()` is the submit answer"
            );
            assert_eq!(
                row.error.is_retryable_for(true),
                row.retryable_for_read,
                "{name}: `is_retryable_for(true)` disagrees with the documented table"
            );
            assert_eq!(
                row.error.is_ambiguous(),
                row.ambiguous,
                "{name}: `is_ambiguous()` disagrees with the documented table"
            );
            assert_eq!(
                row.error.retry_after().is_some(),
                row.retry_after,
                "{name}: `retry_after()` disagrees with the documented table"
            );
            assert!(
                !(row.retryable && row.ambiguous),
                "{name}: a request that may already have been processed is never retryable"
            );
            assert!(
                !(row.retryable_for_read && row.ambiguous),
                "{name}: a request that may already have been processed is never retryable, \
                 not even for a read"
            );
            if row.retryable {
                assert!(
                    row.retryable_for_read,
                    "{name}: an idempotent request is never worse off than a submit"
                );
            }
            assert!(
                !row.retry_after || matches!(&row.error, Error::RateLimited { .. }),
                "{name}: only a rate limit carries the provider's hint"
            );
            assert!(!row.error.to_string().is_empty(), "{name}: empty Display");
        }
    }

    /// The `status()` column has a grammar a reader can rely on, and `carried`
    /// is only used where there is a status field to carry.
    #[test]
    fn the_status_column_uses_the_documented_grammar() {
        for row in table() {
            let cell = row.status;
            assert!(
                cell == "none" || cell == "carried" || cell.parse::<u16>().is_ok(),
                "`{}`: `{cell}` is neither `none`, `carried` nor a status code",
                row.label
            );
            if cell == "carried" {
                assert!(
                    matches!(
                        &row.error,
                        Error::Rejected { .. }
                            | Error::Upstream { .. }
                            | Error::UnexpectedStatus { .. }
                            | Error::Ambiguous {
                                kind: AmbiguousKind::Upstream { .. },
                                ..
                            }
                    ),
                    "`{}`: only `Rejected`, `Upstream`, `UnexpectedStatus` and \
                     `Ambiguous`/`Upstream` carry a status",
                    row.label
                );
            }
        }
    }

    /// No variant falls through to the default arms in `is_retryable`,
    /// `is_ambiguous` or `status`. `name_of` has no wildcard arm, so a new
    /// variant breaks the build; a variant that is named but not tabulated
    /// breaks this test.
    #[test]
    fn the_documented_table_covers_every_variant() {
        let rows = table();
        let covered: BTreeSet<&str> = rows.iter().map(Row::variant).collect();
        let expected: BTreeSet<&str> = VARIANTS.iter().copied().collect();
        assert_eq!(
            covered, expected,
            "a row per variant, and no row without one"
        );

        let labels: BTreeSet<&str> = rows.iter().map(|row| row.label).collect();
        assert_eq!(labels.len(), rows.len(), "two rows share a label");
        assert_eq!(
            rows.len(),
            VARIANTS.len() + 5,
            "the four `Transport` kinds and the three `Ambiguous` kinds are the only repeats"
        );
    }

    /// Every [`TransportKind`], listed so that a new kind cannot stay out of
    /// the documented table: [`kind_name`] matches without a wildcard arm, so
    /// the build breaks first.
    const TRANSPORT_KINDS: &[TransportKind] = &[
        TransportKind::Connect,
        TransportKind::Timeout,
        TransportKind::Io,
        TransportKind::Protocol,
    ];

    /// The `TransportKind` name the table's second column spells, matched with
    /// **no** wildcard arm.
    fn kind_name(kind: TransportKind) -> &'static str {
        match kind {
            TransportKind::Connect => "Connect",
            TransportKind::Timeout => "Timeout",
            TransportKind::Io => "Io",
            TransportKind::Protocol => "Protocol",
        }
    }

    /// The four `Transport` rows are one per kind, labelled exactly as the
    /// table spells them, and each one answers like the kind it classifies.
    /// `TransportKind` is `#[non_exhaustive]` as well, so this is the check
    /// that would catch a kind added without a row.
    #[test]
    fn every_transport_kind_has_exactly_one_documented_row() {
        let rows = table();
        let transport: Vec<&Row> = rows
            .iter()
            .filter(|row| matches!(&row.error, Error::Transport { .. }))
            .collect();
        assert_eq!(
            transport.len(),
            TRANSPORT_KINDS.len(),
            "one `Transport` row per kind, and no kind twice"
        );

        for kind in TRANSPORT_KINDS {
            let label = format!("[`Error::Transport`] / `{}`", kind_name(*kind));
            let matching: Vec<&Row> = transport
                .iter()
                .copied()
                .filter(|row| row.label == label)
                .collect();
            assert_eq!(matching.len(), 1, "no single row spells `{label}`");
            let row = &matching[0];

            let sample = transport_error(*kind, "sample");
            assert_eq!(sample.status(), documented_status(row), "`{label}`: status");
            assert_eq!(row.retryable, sample.is_retryable(), "`{label}`: retry");
            assert_eq!(
                row.retryable_for_read,
                sample.is_retryable_for(true),
                "`{label}`: retry for a read"
            );
            assert_eq!(
                row.retry_after,
                sample.retry_after().is_some(),
                "`{label}`: Retry-After hint"
            );
            assert_eq!(row.ambiguous, sample.is_ambiguous(), "`{label}`: ambiguous");
            assert_eq!(
                sample.to_string(),
                format!("transport error ({kind}): sample"),
                "`{label}`: `Display` names the kind the table names"
            );
            assert_eq!(
                kind.to_string(),
                kind_name(*kind).to_ascii_lowercase(),
                "`{label}`: `Display` spells the kind the way the row does"
            );
        }
    }

    /// Every [`AmbiguousKind`], listed so that a new kind cannot stay out of
    /// the documented table: [`ambiguous_kind_name`] matches without a wildcard
    /// arm, so the build breaks first.
    const AMBIGUOUS_KINDS: &[AmbiguousKind] = &[
        AmbiguousKind::Timeout,
        AmbiguousKind::Io,
        AmbiguousKind::Upstream { status: 502 },
    ];

    /// The `AmbiguousKind` name the table's second column spells, matched with
    /// **no** wildcard arm.
    fn ambiguous_kind_name(kind: AmbiguousKind) -> &'static str {
        match kind {
            AmbiguousKind::Timeout => "Timeout",
            AmbiguousKind::Io => "Io",
            AmbiguousKind::Upstream { .. } => "Upstream",
        }
    }

    /// The three `Ambiguous` rows are one per kind, labelled exactly as the
    /// table spells them, and each one answers with the kind's own status.
    /// `AmbiguousKind` is `#[non_exhaustive]` as well, so this is the check
    /// that would catch a kind added without a row.
    #[test]
    fn every_ambiguous_kind_has_exactly_one_documented_row() {
        let rows = table();
        let ambiguous: Vec<&Row> = rows
            .iter()
            .filter(|row| matches!(&row.error, Error::Ambiguous { .. }))
            .collect();
        assert_eq!(
            ambiguous.len(),
            AMBIGUOUS_KINDS.len(),
            "one `Ambiguous` row per kind, and no kind twice"
        );

        for kind in AMBIGUOUS_KINDS {
            let label = format!("[`Error::Ambiguous`] / `{}`", ambiguous_kind_name(*kind));
            let matching: Vec<&Row> = ambiguous
                .iter()
                .copied()
                .filter(|row| row.label == label)
                .collect();
            assert_eq!(matching.len(), 1, "no single row spells `{label}`");
            let row = &matching[0];

            let sample = Error::Ambiguous {
                kind: *kind,
                message: "sample".into(),
            };
            assert_eq!(
                documented_status(row),
                kind.status(),
                "`{label}`: the row and its kind disagree on the status"
            );
            assert_eq!(sample.status(), kind.status(), "`{label}`: status");
            assert_eq!(row.retryable, sample.is_retryable(), "`{label}`: retry");
            assert_eq!(row.ambiguous, sample.is_ambiguous(), "`{label}`: ambiguous");
            assert!(
                sample.to_string().contains(&kind.to_string()),
                "`{label}`: `Display` names the kind the table names"
            );
        }
    }

    /// Only the failures that may arrive after the provider acted depend on
    /// the request: a 5xx answer, a timeout and a broken connection. Every
    /// other row answers the same for a submit and for a read.
    #[test]
    fn only_a_5xx_and_a_mid_flight_failure_depend_on_the_context() {
        let rows = table();
        let differing: Vec<&Row> = rows
            .iter()
            .filter(|row| row.retryable != row.retryable_for_read)
            .collect();

        assert_eq!(
            differing.len(),
            3,
            "exactly `Upstream` and the two mid-flight kinds differ: {:?}",
            differing.iter().map(|row| row.label).collect::<Vec<_>>()
        );
        for row in &differing {
            assert!(!row.retryable, "{}: a submit is refused", row.label);
            assert!(row.retryable_for_read, "{}: a read is safe", row.label);
            assert!(!row.ambiguous, "{}", row.label);
        }
        assert!(differing
            .iter()
            .any(|row| matches!(&row.error, Error::Upstream { .. })));
        for kind in [TransportKind::Timeout, TransportKind::Io] {
            assert!(differing.iter().any(|row| matches!(
                &row.error,
                Error::Transport { kind: actual, .. } if *actual == kind
            )));
        }
    }

    /// The reported read: a 5xx on a `GET` is retryable without a special
    /// case, and stays terminal for a submit.
    #[test]
    fn a_read_5xx_is_retryable_without_a_special_case() {
        for status in [500, 502, 503] {
            let upstream = Error::Upstream {
                status,
                message: "unavailable".into(),
            };
            assert!(upstream.is_retryable_for(true), "{status}");
            assert!(!upstream.is_retryable_for(false), "{status}");
            assert!(!upstream.is_retryable(), "{status}");
            assert!(!upstream.is_ambiguous(), "{status}");
        }
    }

    /// A submit whose request never left the machine is retryable, and so is a
    /// rate limit: the provider rejected those before acting.
    #[test]
    fn a_failure_that_cannot_have_been_billed_is_retryable_for_a_submit() {
        for error in [
            transport_error(TransportKind::Connect, "dns"),
            Error::RateLimited { retry_after: None },
        ] {
            assert!(error.is_retryable_for(false), "{error:?}");
            assert!(error.is_retryable_for(true), "{error:?}");
            assert!(!error.is_ambiguous(), "{error:?}");
        }
    }

    /// `retry_after()` reports what the provider said, at the boundary of the
    /// hint itself: zero is a hint to retry at once, a missing header is "no
    /// hint" rather than "no wait", and nothing else carries a delay.
    #[test]
    fn retry_after_is_the_providers_hint_at_both_ends() {
        assert_eq!(
            Error::RateLimited {
                retry_after: Some(Duration::ZERO)
            }
            .retry_after(),
            Some(Duration::ZERO),
            "zero seconds is a hint to retry immediately"
        );
        assert_eq!(
            Error::RateLimited {
                retry_after: Some(Duration::from_secs(1))
            }
            .retry_after(),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            Error::RateLimited { retry_after: None }.retry_after(),
            None,
            "no hint is not no wait"
        );
        assert_eq!(
            transport_error(TransportKind::Connect, "dns").retry_after(),
            None
        );
    }

    /// The table is prose, so it too is asserted: every row above appears in
    /// the documentation exactly once, and the documentation has no row the
    /// table does not know. Editing one side alone fails here.
    #[test]
    fn the_documentation_table_lists_every_row() {
        const SOURCE: &str = include_str!("error.rs");
        let rows = table();
        for row in &rows {
            let documented = row.documented();
            assert_eq!(
                SOURCE.matches(documented.as_str()).count(),
                1,
                "the documentation must carry exactly one `{documented}` line"
            );
        }
        let documented_rows = SOURCE
            .lines()
            .filter(|line| line.starts_with("/// | [`Error::"))
            .count();
        assert_eq!(
            documented_rows,
            rows.len(),
            "the documentation and `table()` disagree on the number of rows"
        );
    }

    /// The defect this test file documents against: `Transport` flattened its
    /// cause into a `String`, so `source()` stopped at the variant and a caller
    /// could not tell DNS from TLS from a reset. The chain now runs through the
    /// variant, and it *ends* — the deepest link has no source of its own.
    #[test]
    fn a_transport_error_keeps_the_whole_cause_chain() {
        #[derive(Debug)]
        struct Inner;

        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("unexpected eof while reading the TLS record")
            }
        }

        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);

        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("handshake failed")
            }
        }

        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let error = Error::Transport {
            kind: TransportKind::Io,
            source: Box::new(Outer(Inner)),
        };

        let Some(first) = std::error::Error::source(&error) else {
            panic!("the transport cause must survive the variant");
        };
        assert_eq!(first.to_string(), "handshake failed");
        let Some(second) = first.source() else {
            panic!("the cause of the cause must survive too");
        };
        assert_eq!(
            second.to_string(),
            "unexpected eof while reading the TLS record"
        );
        assert!(
            second.source().is_none(),
            "the chain ends, it does not loop"
        );

        // `Display` names the kind and the cause, exactly once.
        assert_eq!(error.to_string(), "transport error (io): handshake failed");
        // A mid-flight failure is only retryable for a request that is
        // idempotent; the conservative answer stays `false`.
        assert!(!error.is_retryable());
        assert!(error.is_retryable_for(true));
        assert!(!error.is_ambiguous());
        assert_eq!(error.status(), None);
    }

    /// `Base64` keeps the decoder's error, so a caller can tell an invalid
    /// symbol from a truncated input instead of parsing a sentence.
    #[test]
    fn a_base64_error_keeps_the_decoder_error_typed() {
        use base64::DecodeError;

        let truncated = DecodeError::InvalidLastSymbol(7, b'A');
        let error = Error::Base64 {
            field: "journal.csv_base64",
            source: truncated.clone(),
        };

        assert_eq!(
            error.to_string(),
            format!("invalid base64 in `journal.csv_base64`: {truncated}")
        );
        let Some(source) = std::error::Error::source(&error) else {
            panic!("the decoder error must stay reachable");
        };
        match source.downcast_ref::<DecodeError>() {
            Some(DecodeError::InvalidLastSymbol(index, byte)) => {
                assert_eq!((*index, *byte), (7, b'A'));
            }
            other => panic!("the source must be the decoder error, got {other:?}"),
        }
        assert!(!error.is_retryable());
        assert_eq!(error.status(), None);
    }

    /// `retry_suffix` is only ever empty when there is nothing to say, and a
    /// zero-second `Retry-After` is still something to say.
    #[test]
    fn retry_suffix_covers_both_sides_of_the_presence_boundary() {
        assert_eq!(retry_suffix(&None), "");
        assert_eq!(retry_suffix(&Some(Duration::ZERO)), ", retry after 0s");
        assert_eq!(
            retry_suffix(&Some(Duration::from_secs(1))),
            ", retry after 1s"
        );
    }
}
