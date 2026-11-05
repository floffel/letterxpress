//! Credentials and the operating mode, with redaction and fail-closed checks.
//!
//! The API key is the account's payment credential: whoever holds it can spend
//! money. It therefore never appears in `Debug`, never appears in an error
//! message and is only reachable through a crate-private accessor that the
//! request builder uses.
//!
//! ```
//! use letterxpress::{Credentials, Mode};
//!
//! let credentials = Credentials::new("kunde@example.de", "s3cret", Mode::Test)?;
//! assert_eq!(credentials.mode(), Mode::Test);
//! // Neither the key nor a fragment of it appears in the debug output.
//! let debug = format!("{credentials:?}");
//! assert!(!debug.contains("s3cret"));
//! assert!(debug.contains("<redacted>"));
//! # Ok::<(), letterxpress::Error>(())
//! ```

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::Error;

/// Longest accepted username; the provider documents no limit, this is a
/// sanity bound against pathological input.
const MAX_USERNAME_LEN: usize = 256;
/// Longest accepted API key; keys are short opaque tokens.
const MAX_APIKEY_LEN: usize = 512;

/// Test or live mode, sent as `auth.mode`.
///
/// There is no default: a client that silently picks one is a client that can
/// bill a customer from a test run. Construct it explicitly, usually from
/// configuration you have already validated.
///
/// ```
/// use letterxpress::Mode;
///
/// assert_eq!(Mode::Test.as_str(), "test");
/// assert_eq!("LIVE".parse::<Mode>().unwrap(), Mode::Live);
/// assert!("prod".parse::<Mode>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Jobs land in the Postbox, are never produced, are never billed and are
    /// deleted after seven days.
    ///
    /// ```
    /// assert_eq!(letterxpress::Mode::Test.as_str(), "test");
    /// ```
    Test,
    /// Jobs are produced and billed immediately.
    ///
    /// ```
    /// assert_eq!(letterxpress::Mode::Live.as_str(), "live");
    /// ```
    Live,
}

impl Mode {
    /// The wire spelling.
    ///
    /// ```
    /// assert_eq!(letterxpress::Mode::Live.as_str(), "live");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Live => "live",
        }
    }

    /// Whether this mode spends money.
    ///
    /// ```
    /// assert!(letterxpress::Mode::Live.is_billable());
    /// assert!(!letterxpress::Mode::Test.is_billable());
    /// ```
    #[must_use]
    pub fn is_billable(self) -> bool {
        matches!(self, Self::Live)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().eq_ignore_ascii_case("test") {
            Ok(Self::Test)
        } else if value.trim().eq_ignore_ascii_case("live") {
            Ok(Self::Live)
        } else {
            Err(Error::invalid(
                "mode",
                format!("expected \"test\" or \"live\", got {value:?}"),
            ))
        }
    }
}

/// Username plus API key plus mode, ready to be put in every request body.
///
/// Construction fails closed: an empty (or whitespace-only) username or key is
/// rejected instead of being sent, because the provider would answer with an
/// indistinguishable `{"message": "Unauthorized."}` and the caller would keep
/// retrying a configuration mistake.
///
/// ```
/// use letterxpress::{Credentials, Mode};
///
/// assert!(Credentials::new("", "key", Mode::Test).is_err());
/// assert!(Credentials::new("user", "   ", Mode::Test).is_err());
/// let ok = Credentials::new(" user ", "key", Mode::Test); // surrounding space is trimmed
/// assert!(ok.is_ok());
/// ```
///
/// The key is not exposed by a public accessor:
///
/// ```compile_fail
/// use letterxpress::{Credentials, Mode};
///
/// let credentials = Credentials::new("kunde", "abc", Mode::Test).unwrap();
/// let _ = credentials.auth_json();
/// ```
#[derive(Clone)]
pub struct Credentials {
    username: String,
    apikey: String,
    mode: Mode,
}

/// The borrowed `auth` object serialized into every request body.
///
/// This stays crate-private and deliberately implements only [`Serialize`], so
/// callers cannot format or reserialize the API key.
#[derive(Serialize)]
pub(crate) struct AuthPayload<'a> {
    username: &'a str,
    apikey: &'a str,
    mode: &'static str,
}

impl Credentials {
    /// Builds credentials, rejecting empty or control-character values.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `username` or `apikey` is empty after
    /// trimming, contains a control character, or exceeds the sanity bound.
    ///
    /// ```
    /// use letterxpress::{Credentials, Mode};
    ///
    /// let credentials = Credentials::new("kunde", "abc123", Mode::Live)?;
    /// assert_eq!(credentials.username(), "kunde");
    /// assert!(credentials.mode().is_billable());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn new(
        username: impl Into<String>,
        apikey: impl Into<String>,
        mode: Mode,
    ) -> Result<Self, Error> {
        let username = username.into();
        let apikey = apikey.into();

        let username = username.trim();
        let apikey = apikey.trim();

        validate_token("credentials.username", username, MAX_USERNAME_LEN)?;
        validate_token("credentials.apikey", apikey, MAX_APIKEY_LEN)?;

        Ok(Self {
            username: username.to_owned(),
            apikey: apikey.to_owned(),
            mode,
        })
    }

    /// The account name. Safe to log: it identifies the account, not the key.
    ///
    /// ```
    /// use letterxpress::{Credentials, Mode};
    ///
    /// let credentials = Credentials::new("kunde", "abc", Mode::Test)?;
    /// assert_eq!(credentials.username(), "kunde");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The configured mode.
    ///
    /// ```
    /// use letterxpress::{Credentials, Mode};
    ///
    /// assert_eq!(Credentials::new("u", "k", Mode::Test)?.mode(), Mode::Test);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Borrows the credentials as the `auth` object sent on the wire.
    ///
    /// This is crate-private: the API key must only be serialized into a
    /// request, never handed to callers as a `Debug` or `Serialize` value.
    #[must_use]
    pub(crate) fn auth_payload(&self) -> AuthPayload<'_> {
        AuthPayload {
            username: &self.username,
            apikey: &self.apikey,
            mode: self.mode.as_str(),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("apikey", &"<redacted>")
            .field("mode", &self.mode)
            .finish()
    }
}

/// Rejects empty, oversized, and control-character-bearing values.
fn validate_token(field: &'static str, value: &str, max: usize) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::invalid(field, "must not be empty"));
    }
    if value.chars().count() > max {
        return Err(Error::invalid(
            field,
            format!("must be at most {max} characters"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::invalid(field, "must not contain control characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_reports_whether_it_spends_money() {
        assert!(!Mode::Test.is_billable());
        assert!(Mode::Live.is_billable());
        assert_eq!(Mode::Test.to_string(), "test");
        assert_eq!(Mode::Live.to_string(), "live");
        assert_eq!(" Live ".parse::<Mode>().expect("live"), Mode::Live);
        assert!("prod".parse::<Mode>().is_err());
    }

    #[test]
    fn token_validation_rejects_every_bad_shape() {
        assert!(validate_token("field", "", 10).is_err());
        assert!(validate_token("field", "abcdef", 3).is_err());
        assert!(validate_token("field", "a\u{1}b", 10).is_err());
        assert!(validate_token("field", "abc", 3).is_ok());
        assert!(
            validate_token("field", "äöü", 3).is_ok(),
            "three characters, six bytes"
        );
    }

    #[test]
    fn debug_redacts_the_key_but_keeps_the_account() {
        let credentials = Credentials::new("kunde", "secret-key", Mode::Test).expect("credentials");
        let debug = format!("{credentials:?}");
        assert!(debug.contains("kunde"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret-key"));
    }

    #[test]
    fn auth_payload_serializes_the_documented_shape() {
        let credentials = Credentials::new("kunde", "secret-key", Mode::Live).expect("credentials");
        let payload = serde_json::to_value(credentials.auth_payload()).expect("auth payload");
        assert_eq!(payload["username"], "kunde");
        assert_eq!(payload["apikey"], "secret-key");
        assert_eq!(payload["mode"], "live");
    }

    #[test]
    fn credential_length_limits_accept_exact_and_reject_one_past() {
        assert!(Credentials::new("u".repeat(MAX_USERNAME_LEN), "k", Mode::Test).is_ok());
        assert!(matches!(
            Credentials::new("u".repeat(MAX_USERNAME_LEN + 1), "k", Mode::Test),
            Err(Error::Validation {
                field: "credentials.username",
                ..
            })
        ));

        assert!(Credentials::new("u", "k".repeat(MAX_APIKEY_LEN), Mode::Test).is_ok());
        assert!(matches!(
            Credentials::new("u", "k".repeat(MAX_APIKEY_LEN + 1), Mode::Test),
            Err(Error::Validation {
                field: "credentials.apikey",
                ..
            })
        ));
    }
}
