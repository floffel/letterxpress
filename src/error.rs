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
        match self {
            Self::Connect | Self::Timeout | Self::Io => true,
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

/// The error type returned by every fallible operation in this crate.
///
/// The enum is `#[non_exhaustive]`: new failure modes can appear without a
/// breaking release, so always match with a trailing `_` arm.
///
/// # Retry stance per variant
///
/// One row per variant; [`Error::Transport`] is split by [`TransportKind`]
/// because the kind is what decides. This table *is* the contract:
/// [`Error::status`], [`Error::is_retryable`] and [`Error::is_ambiguous`] answer
/// for every variant exactly as listed, and the test module at the end of this
/// file asserts the code and the text of the table against each other, so a
/// variant cannot arrive without a documented stance.
///
/// In the `status()` column, `none` means the error carries no HTTP status and
/// `carried` means the code the response had is reported back.
///
/// | Variant | `status()` | `is_retryable()` | `is_ambiguous()` |
/// | --- | --- | --- | --- |
/// | [`Error::Validation`] | none | no | no |
/// | [`Error::Unauthorized`] | 401 | no | no |
/// | [`Error::Forbidden`] | 403 | no | no |
/// | [`Error::NotFound`] | 404 | no | no |
/// | [`Error::MethodNotAllowed`] | 405 | no | no |
/// | [`Error::RateLimited`] | 429 | yes | no |
/// | [`Error::Rejected`] | carried | no | no |
/// | [`Error::Upstream`] | carried | no | no |
/// | [`Error::Ambiguous`] | none | no | yes |
/// | [`Error::Transport`] / `Connect` | none | yes | no |
/// | [`Error::Transport`] / `Timeout` | none | yes | no |
/// | [`Error::Transport`] / `Io` | none | yes | no |
/// | [`Error::Transport`] / `Protocol` | none | no | no |
/// | [`Error::Json`] | none | no | no |
/// | [`Error::MissingField`] | none | no | no |
/// | [`Error::UnexpectedShape`] | none | no | no |
/// | [`Error::TooLarge`] | none | no | no |
/// | [`Error::Base64`] | none | no | no |
/// | [`Error::Utf8`] | none | no | no |
///
/// A `yes` in `is_retryable()` is narrower than it reads: `RateLimited` is
/// retryable only once `Retry-After` has elapsed, and the transport kinds are
/// retryable on an idempotent call. Variant by variant:
///
/// * `Validation` — the request was never sent, so the input has to change.
/// * `Unauthorized`, `Forbidden` — the credential has to change.
/// * `NotFound`, `MethodNotAllowed` — the resource or the verb is wrong.
/// * `RateLimited` — the provider dropped the request before processing it, so
///   a fresh attempt is safe *and* likely to succeed.
/// * `Rejected` — the provider refused the body; the same body is refused
///   again.
/// * `Upstream` — the provider answered with a 5xx, so it is reachable and the
///   call failed there. For a read it is safe to try again; for a submit the
///   5xx may arrive after the job was created, which is why the client turns a
///   5xx on a submit into [`Error::Ambiguous`] instead of `Upstream`. A caller
///   that deliberately retries a failed read gets `Upstream` and decides for
///   itself.
/// * `Ambiguous` — the provider may already have acted on the request. A retry
///   can produce a second, billed letter, so reconcile first; this is the only
///   variant for which `is_ambiguous()` is true.
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
    /// timeout, a mid-flight connection failure, a 5xx, or a 2xx whose body
    /// could not be decoded — a 200 means the job exists and is billed even
    /// when its tracking data is unreadable. **Do not resubmit blindly**: the
    /// letter may already be in production. Look the job up
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
    ///
    /// On a submit (a `POST`) a 2xx with an unreadable body is reported as
    /// [`Error::Ambiguous`] instead, because the job may exist.
    ///
    /// Carries no HTTP status: it is raised from the body alone, and a
    /// `serde_json` error has no slot for one. When the client decodes a
    /// response, a non-JSON body is `Json` only for a 2xx — a non-2xx with an
    /// unreadable body (a proxy's HTML page, say) is classified by its HTTP
    /// status instead and reported as [`Error::Rejected`] or
    /// [`Error::Upstream`], which do carry the code.
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
        "Upstream",
        "Utf8",
        "Validation",
    ];

    /// One row of the documented table: the label the table writes, its three
    /// answer cells, and a sample of the variant the row classifies.
    struct Row {
        /// First column, exactly as the table spells it.
        label: &'static str,
        /// `status()` column: `none`, `carried`, or the fixed code.
        status: &'static str,
        /// `is_retryable()` column.
        retryable: bool,
        /// `is_ambiguous()` column.
        ambiguous: bool,
        /// A constructed value of the variant.
        error: Error,
    }

    impl Row {
        /// The row as it must appear in the documentation, `///` included.
        fn documented(&self) -> String {
            format!(
                "/// | {} | {} | {} | {} |",
                self.label,
                self.status,
                yes_no(self.retryable),
                yes_no(self.ambiguous)
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
                Error::Rejected { status, .. } | Error::Upstream { status, .. } => Some(*status),
                _ => None,
            },
            fixed => fixed.parse::<u16>().ok(),
        }
    }

    /// One documented row: label, the three answer cells, and a sample.
    fn row(
        label: &'static str,
        status: &'static str,
        retryable: bool,
        ambiguous: bool,
        error: Error,
    ) -> Row {
        Row {
            label,
            status,
            retryable,
            ambiguous,
            error,
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
                Error::invalid("letter.notice", "must not be empty"),
            ),
            row(
                "[`Error::Unauthorized`]",
                "401",
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
                Error::Forbidden {
                    message: "Forbidden".into(),
                },
            ),
            row(
                "[`Error::NotFound`]",
                "404",
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
                Error::MethodNotAllowed {
                    message: "Method Not Allowed".into(),
                },
            ),
            row(
                "[`Error::RateLimited`]",
                "429",
                true,
                false,
                Error::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
            ),
            row(
                "[`Error::Rejected`]",
                "carried",
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
                false,
                Error::Upstream {
                    status: 502,
                    message: "Bad Gateway".into(),
                },
            ),
            row(
                "[`Error::Ambiguous`]",
                "none",
                false,
                true,
                Error::Ambiguous {
                    message: "timeout after 30s".into(),
                },
            ),
            row(
                "[`Error::Transport`] / `Connect`",
                "none",
                true,
                false,
                Error::Transport {
                    kind: TransportKind::Connect,
                    message: "dns".into(),
                },
            ),
            row(
                "[`Error::Transport`] / `Timeout`",
                "none",
                true,
                false,
                Error::Transport {
                    kind: TransportKind::Timeout,
                    message: "no answer".into(),
                },
            ),
            row(
                "[`Error::Transport`] / `Io`",
                "none",
                true,
                false,
                Error::Transport {
                    kind: TransportKind::Io,
                    message: "connection reset".into(),
                },
            ),
            row(
                "[`Error::Transport`] / `Protocol`",
                "none",
                false,
                false,
                Error::Transport {
                    kind: TransportKind::Protocol,
                    message: "no scripted response".into(),
                },
            ),
            row(
                "[`Error::Json`]",
                "none",
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
                Error::UnexpectedShape {
                    endpoint: "GET /v3/balance",
                    message: "expected an object, got an array".into(),
                },
            ),
            row(
                "[`Error::TooLarge`]",
                "none",
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
                Error::Base64 {
                    field: "invoice.base64_data",
                    message: "invalid symbol 33".into(),
                },
            ),
            row(
                "[`Error::Utf8`]",
                "none",
                false,
                false,
                Error::Utf8 {
                    field: "journal.csv_base64",
                },
            ),
        ]
    }

    /// Each row is the answer the code gives, and the two invariants that make
    /// the table worth having hold: nothing is both retryable and ambiguous,
    /// and every variant renders a message.
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
                row.error.is_ambiguous(),
                row.ambiguous,
                "{name}: `is_ambiguous()` disagrees with the documented table"
            );
            assert!(
                !(row.retryable && row.ambiguous),
                "{name}: a request that may already have been processed is never retryable"
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
                    matches!(&row.error, Error::Rejected { .. } | Error::Upstream { .. }),
                    "`{}`: only `Rejected` and `Upstream` carry a status",
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
            VARIANTS.len() + 3,
            "the four `Transport` kinds are the only repeated variant"
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

            let sample = Error::Transport {
                kind: *kind,
                message: "sample".into(),
            };
            assert_eq!(sample.status(), documented_status(row), "`{label}`: status");
            assert_eq!(row.retryable, sample.is_retryable(), "`{label}`: retry");
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
