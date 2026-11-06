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
//! assert!(debug.contains("***"));
//! # Ok::<(), letterxpress::Error>(())
//! ```

use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use vitaminc_protected::{Controlled, Protected};

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
///
/// let error = "live kunde@x.de s3cret".parse::<Mode>().unwrap_err();
/// assert_eq!(
///     error.to_string(),
///     "invalid mode: unrecognised mode, 22 characters"
/// );
/// assert!(!error.to_string().contains("s3cret"));
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
                format!("unrecognised mode, {} characters", value.chars().count()),
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
/// let _ = credentials.expose_api_key();
/// ```
///
/// Cloning makes an independent key copy in a zeroizing wrapper; neither copy
/// is retained after drop.
///
/// ```
/// use letterxpress::{Credentials, Mode};
///
/// let credentials = Credentials::new("kunde", "abc", Mode::Test)?;
/// let clone = credentials.clone();
/// assert_eq!(clone.username(), credentials.username());
/// assert!(!format!("{clone:?}").contains("abc"));
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Credentials {
    username: String,
    apikey: Protected<String>,
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
    /// Builds credentials, rejecting empty or non-ASCII-printable values.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `username` or `apikey` is empty after
    /// trimming, contains anything other than ASCII graphic characters or
    /// spaces, or exceeds the sanity bound.
    ///
    /// ```
    /// use letterxpress::{Credentials, Mode};
    ///
    /// let credentials = Credentials::new("kunde", "abc123", Mode::Live)?;
    /// assert_eq!(credentials.username(), "kunde");
    /// assert!(credentials.mode().is_billable());
    /// assert!(Credentials::new("kunde\u{2028}name", "abc123", Mode::Live).is_err());
    /// assert!(Credentials::new("kunde", "abc\u{202E}123", Mode::Live).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn new(
        username: impl Into<String>,
        apikey: impl Into<String>,
        mode: Mode,
    ) -> Result<Self, Error> {
        let username = username.into();
        let mut apikey = Protected::new(apikey.into());

        let username = username.trim();
        trim_in_place(apikey.inner_mut());

        validate_token("credentials.username", username, MAX_USERNAME_LEN)?;
        validate_token("credentials.apikey", apikey.risky_ref(), MAX_APIKEY_LEN)?;

        Ok(Self {
            username: username.to_owned(),
            apikey,
            mode,
        })
    }

    /// The account name, restricted to ASCII graphic characters and spaces so
    /// it is safe to log: it identifies the account, not the key, and cannot
    /// inject line breaks, bidi controls, or zero-width characters.
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
            apikey: self.expose_api_key(),
            mode: self.mode.as_str(),
        }
    }

    /// Explicit crate-private access to the secret.
    ///
    /// The wrapper deliberately has no public accessor, `Deref`, `Display`, or
    /// secret-bearing `Debug` implementation.
    pub(crate) fn expose_api_key(&self) -> &str {
        self.apikey.risky_ref()
    }
}

/// Trims in place so the caller-provided allocation becomes the protected one.
fn trim_in_place(value: &mut String) {
    let trimmed_len = value.trim().len();
    let start = value.len() - value.trim_start().len();
    value.drain(..start);
    value.truncate(trimmed_len);
}

/// Rejects empty, oversized, and non-ASCII-printable values.
fn validate_token(field: &'static str, value: &str, max: usize) -> Result<(), Error> {
    if value.is_empty() {
        return Err(Error::invalid(field, "must not be empty"));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_graphic() || character == ' ')
    {
        return Err(Error::invalid(
            field,
            "must contain only ASCII graphic characters or spaces",
        ));
    }
    if value.len() > max {
        return Err(Error::invalid(
            field,
            format!("must be at most {max} characters"),
        ));
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
        assert_eq!("test".parse::<Mode>().expect("test"), Mode::Test);
        assert_eq!("live".parse::<Mode>().expect("live"), Mode::Live);
        assert_eq!(" Live ".parse::<Mode>().expect("live"), Mode::Live);
        assert!("prod".parse::<Mode>().is_err());
    }

    #[test]
    fn mode_parse_error_redacts_the_untrusted_input_but_reports_its_length() {
        let invalid = "live kunde@x.de s3cret";
        let error = invalid.parse::<Mode>().expect_err("invalid mode");

        assert!(matches!(error, Error::Validation { field: "mode", .. }));
        let rendered = error.to_string();
        assert_eq!(rendered, "invalid mode: unrecognised mode, 22 characters");
        assert!(!rendered.contains("kunde@x.de"));
        assert!(!rendered.contains("s3cret"));
    }

    #[test]
    fn token_validation_rejects_every_bad_shape() {
        assert!(matches!(
            validate_token("field", "", 10),
            Err(Error::Validation { field: "field", .. })
        ));
        assert!(matches!(
            validate_token("field", "abcdef", 3),
            Err(Error::Validation { field: "field", .. })
        ));
        assert!(validate_token("field", "abc", 3).is_ok());
        assert!(validate_token("field", "abc def", 7).is_ok());
        assert!(validate_token("field", "!\"#$%&'()*+,-./", 16).is_ok());
    }

    #[test]
    fn token_validation_rejects_invisible_and_directional_characters() {
        for value in [
            "a\u{1}b",    // Cc: START OF HEADING
            "a\tb",       // Cc: TAB
            "a\u{200B}b", // Cf: ZERO WIDTH SPACE
            "a\u{2028}b", // Zl: LINE SEPARATOR
            "a\u{202E}b", // Cf: RIGHT-TO-LEFT OVERRIDE
            "äöü",        // printable non-ASCII
            "a\u{7F}b",   // Cc: DELETE
        ] {
            assert!(matches!(
                validate_token("field", value, 10),
                Err(Error::Validation { field: "field", .. })
            ));
        }
    }

    #[test]
    fn credentials_reject_log_unsafe_username_and_api_key_characters() {
        for value in ["us\u{2028}er", "us\u{202E}er", "us\u{200B}er"] {
            assert!(matches!(
                Credentials::new(value, "key", Mode::Test),
                Err(Error::Validation {
                    field: "credentials.username",
                    ..
                })
            ));
            assert!(matches!(
                Credentials::new("user", value, Mode::Test),
                Err(Error::Validation {
                    field: "credentials.apikey",
                    ..
                })
            ));
        }
    }

    #[test]
    fn debug_redacts_the_key_but_keeps_the_account() {
        let credentials = Credentials::new("kunde", "secret-key", Mode::Test).expect("credentials");
        let debug = format!("{credentials:?}");
        assert!(debug.contains("kunde"));
        assert!(debug.contains("***"));
        assert!(!debug.contains("secret-key"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn clone_is_redacted_and_keeps_the_key_usable() {
        let credentials = Credentials::new("kunde", "secret-key", Mode::Test).expect("credentials");
        let clone = credentials.clone();
        let debug = format!("{clone:?}");
        assert!(!debug.contains("secret-key"));
        assert!(matches!(
            credentials.auth_payload(),
            AuthPayload {
                apikey: "secret-key",
                ..
            }
        ));
        assert!(matches!(
            clone.auth_payload(),
            AuthPayload {
                apikey: "secret-key",
                ..
            }
        ));
    }

    #[test]
    fn api_key_is_stored_in_a_zeroizing_wrapper() {
        let credentials = Credentials::new("kunde", "secret-key", Mode::Test).expect("credentials");
        let protected: &Protected<String> = &credentials.apikey;
        assert!(std::mem::needs_drop::<Protected<String>>());
        assert_eq!(protected.risky_ref(), "secret-key");
    }

    #[test]
    fn empty_and_whitespace_only_secret_are_rejected_with_typed_errors() {
        for secret in ["", " ", "\t", "\n"] {
            assert!(matches!(
                Credentials::new("kunde", secret, Mode::Test),
                Err(Error::Validation {
                    field: "credentials.apikey",
                    ..
                })
            ));
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_from_the_protected_key() {
        let credentials =
            Credentials::new(" kunde ", "  secret-key  ", Mode::Test).expect("credentials");
        assert_eq!(credentials.username(), "kunde");
        assert_eq!(credentials.expose_api_key(), "secret-key");
        let payload = serde_json::to_value(credentials.auth_payload()).expect("auth payload");
        assert_eq!(payload["apikey"], "secret-key");
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
