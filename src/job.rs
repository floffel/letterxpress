//! Response models: jobs, e-mail jobs, pages, balances, invoices, journals.
//!
//! Every struct decodes permissively: unknown keys are ignored, every documented
//! scalar is accepted as a number *or* a string, both `snake_case` and
//! `camelCase` key spellings are accepted, missing optional fields default, and
//! unknown enum values survive as `Other(String)`. Nothing in here can fail on
//! a provider that adds a field.
//!
//! Three things refuse instead of defaulting, because defaulting them would
//! misreport what the provider billed:
//!
//! * A flag (`c4`, `bank_form`): a value nobody can read — `"y"`, `"on"`, a
//!   nested object — is a decode error, not a silent `false`. Numbers and their
//!   string forms follow the same rule, so `1`, `"1"`, `"2"` and `"01"` are all
//!   `true`.
//! * A count inside a bound: `items[].pages` above [`MAX_ITEM_PAGES`], and a
//!   job with more than [`MAX_JOB_ITEMS`] items, are refused at the boundary.
//!   Every decoded job therefore sums to a page total that fits a `u32`, which
//!   is what keeps [`PrintJob::pages`] arithmetic sound.
//! * A price echo without a `specification`: there is no honest
//!   [`PriceEcho`] to build, and "zero pages, nothing registered" is a
//!   statement about the price that nobody made. See
//!   [`PriceEcho::from_envelope`].
//!
//! The permissiveness is declared, not visited: the scalar shapes (`"7"`, `7`,
//! `6035138.0`, `true`, null) are one `#[serde(untagged)]` enum that the id,
//! text and flag decoders share, and the three `POST /v3/emailjobs` answer
//! shapes are one `#[serde(untagged)]` enum, so `serde` reads the shape from the
//! payload instead of this module sniffing keys.
//!
//! ```
//! use letterxpress::{Money, PrintJob};
//!
//! let job: PrintJob = serde_json::from_str(r#"{
//!     "id": 6035138,
//!     "status": "done",
//!     "items": [{"address": "Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg", "pages": 1, "amount": 0.67, "vat": 0.13, "status": "sent"}],
//!     "some_future_field": {"nested": true}
//! }"#)?;
//! assert_eq!(job.id.as_str(), "6035138");
//! assert_eq!(job.items[0].total()?.map(Money::cents), Some(80));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{
    Color, EmailOption, Error, JobStatus, Money, MoneyError, PrintMode, Registered, Shipping,
};

/// Generates a string-backed identifier that accepts numbers and strings.
macro_rules! record_id {
    ($(#[$meta:meta])* $name:ident, $label:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Builds an id from a string.
            ///
            /// # Errors
            ///
            /// [`Error::Validation`] when the value is empty, longer than 32
            /// characters, or contains anything but ASCII letters and digits.
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert!(", stringify!($name), "::new(\"6035138\").is_ok());")]
            #[doc = concat!("assert!(", stringify!($name), "::new(\"\").is_err());")]
            #[doc = concat!("assert!(", stringify!($name), "::new(\"../1\").is_err());")]
            /// ```
            pub fn new(value: impl Into<String>) -> Result<Self, Error> {
                let value = value.into();
                if value.is_empty() {
                    return Err(Error::invalid($label, "must not be empty"));
                }
                if value.len() > 32 {
                    return Err(Error::invalid($label, "must be at most 32 characters"));
                }
                if !value.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err(Error::invalid(
                        $label,
                        "must contain ASCII letters and digits only",
                    ));
                }
                Ok(Self(value))
            }

            /// The id as sent by the provider.
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert_eq!(", stringify!($name), "::new(\"6035138\").unwrap().as_str(), \"6035138\");")]
            /// ```
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The id as a number, when it is one (it always is in practice).
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert_eq!(", stringify!($name), "::new(\"42\").unwrap().as_u64(), Some(42));")]
            /// ```
            #[must_use]
            pub fn as_u64(&self) -> Option<u64> {
                self.0.parse().ok()
            }
        }

        impl Default for $name {
            /// An empty id, for `Default` on the containing model. It is never
            /// built from a response (the field is required there).
            fn default() -> Self {
                Self(String::new())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                // Ids arrive as numbers, as strings and, in the provider's
                // exports, as floats (`6035138.0`). The scalar does the shape
                // work; only the id rules below are crate-specific.
                let raw = Scalar::deserialize(deserializer)?
                    .into_id()
                    .map_err(de::Error::custom)?;
                Self::new(raw).map_err(|error| de::Error::custom(error.to_string()))
            }
        }
    };
}

/// Any JSON scalar, wherever the provider documented text or a flag.
///
/// One declaration replaces a hand-written visitor per field: `serde` walks the
/// scalar types itself, so a value arrives whether the provider rendered it as
/// a string, an integer or — in its CSV-derived exports — as a float such as
/// `6035138.0`. `null` is not a variant here: every such field is decoded as an
/// `Option`, which carries the null case on its own.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scalar {
    /// Text, as documented.
    Text(String),
    /// A flag rendered as a JSON boolean.
    Bool(bool),
    /// An integer.
    Integer(i64),
    /// An integer above [`i64::MAX`].
    Unsigned(u64),
    /// A number with a fraction.
    Float(f64),
}

impl Scalar {
    /// The scalar as text, in the provider's own spelling: an integral float
    /// loses its `.0`, everything else is its `Display` form.
    fn into_text(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Bool(flag) => flag.to_string(),
            Self::Integer(number) => number.to_string(),
            Self::Unsigned(number) => number.to_string(),
            Self::Float(number) => number.to_string(),
        }
    }

    /// The scalar as an identifier: trimmed text of a non-boolean scalar.
    ///
    /// A boolean is refused rather than spelled `"true"`: that string would
    /// pass the alphanumeric id rule and turn a malformed response into a
    /// plausible-looking id.
    fn into_id(self) -> Result<String, String> {
        match self {
            Self::Bool(flag) => Err(format!("an id cannot be the boolean {flag}")),
            other => Ok(other.into_text().trim().to_owned()),
        }
    }

    /// The scalar as a flag: a boolean is itself, a number is `false` only when
    /// it is zero, and a string follows the flag rule — the words
    /// `true`/`yes`/`false`/`no`, a decimal number, or "not supplied" for the
    /// empty string. Anything else is refused, so a C4 envelope the provider
    /// spelled oddly cannot decode as "not requested".
    fn into_flag(self) -> Result<bool, String> {
        match self {
            Self::Text(text) => flag_from_str(&text).ok_or_else(|| {
                format!("flag value {text:?} is not a number, true/false, yes/no or null")
            }),
            Self::Bool(flag) => Ok(flag),
            Self::Integer(number) => Ok(number != 0),
            Self::Unsigned(number) => Ok(number != 0),
            Self::Float(number) => Ok(number != 0.0),
        }
    }
}

record_id! {
    /// A print-job or e-mail-job identifier.
    ///
    /// ```
    /// use letterxpress::JobId;
    ///
    /// let id: JobId = "6035138".parse()?;
    /// assert_eq!(id.as_u64(), Some(6_035_138));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    JobId, "job id"
}

record_id! {
    /// An invoice identifier.
    ///
    /// ```
    /// use letterxpress::InvoiceId;
    ///
    /// assert_eq!(InvoiceId::new("25402")?.to_string(), "25402");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    InvoiceId, "invoice id"
}

/// The flag words, in any ASCII case: `true`/`yes` are set, `false`/`no` clear.
fn flag_word(value: &str) -> Option<bool> {
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

/// One flag token as the provider spells it, or `None` when this crate cannot
/// interpret it.
///
/// The rule is the same for every JSON type: any non-zero number is `true`, `0`
/// is `false`; the strings add the words `true`/`yes` and `false`/`no` in any
/// ASCII case; `null` and the empty string mean "not supplied" and are `false`.
/// Everything else is `None`, so the caller can refuse it instead of guessing.
fn flag_from_str(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.is_empty() {
        return Some(false);
    }
    if let Some(flag) = flag_word(value) {
        return Some(flag);
    }
    if let Ok(number) = value.parse::<i64>() {
        return Some(number != 0);
    }
    match value.parse::<f64>() {
        Ok(number) if number.is_finite() => Some(number != 0.0),
        _ => None,
    }
}

/// Decodes a documented `0`/`1` flag with the same rule for numbers and strings.
///
/// `0`/`false`/`no`, the empty string and `null` are `false`; `1`/`true`/`yes`
/// and every other non-zero number — as a number or as a string — are `true`.
/// A token outside that set is a [`de::Error`], never a silent `false`: a
/// requested and billed C4 envelope must not decode as "not requested" just
/// because the provider spelled the flag with another string.
fn de_flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    // `Option` carries the null case, [`Scalar`] every non-null spelling.
    match Option::<Scalar>::deserialize(deserializer)? {
        Some(scalar) => scalar.into_flag().map_err(de::Error::custom),
        None => Ok(false),
    }
}

/// A scalar that may arrive as a string, a number, a boolean or null.
fn de_opt_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<Scalar>::deserialize(deserializer)?.map(Scalar::into_text))
}

/// Status used when a response omits the field entirely.
fn unspecified_status() -> JobStatus {
    JobStatus::Other(String::new())
}

/// Adds a reported value to an aggregate without turning unknown into zero.
fn add_reported(sum: Option<Money>, amount: Option<Money>) -> Result<Option<Money>, MoneyError> {
    match (sum, amount) {
        (Some(sum), Some(amount)) => sum.checked_add(amount).map(Some),
        _ => Ok(None),
    }
}

/// The documented ceiling for one letter's page count, and therefore for one
/// `items[].pages` value in a job response.
///
/// The price specification accepts exactly the same range (`1` to `10 000`), so
/// a job item claiming more pages than a letter can have is malformed rather
/// than merely large. Decoding refuses it instead of letting one absurd number
/// reach a page total, a cost estimate or an allocation.
///
/// A job of at most [`MAX_JOB_ITEMS`] items with at most this many pages each
/// sums to at most 500 000 000 pages, which fits a [`u32`] with room to spare —
/// that bound is what makes [`PrintJob::pages`] arithmetic overflow-free for
/// every value that came off the wire.
///
/// ```
/// use letterxpress::{PrintJob, MAX_ITEM_PAGES};
///
/// // The ceiling itself decodes ...
/// let at_limit = serde_json::json!({"id": 1, "items": [{"pages": MAX_ITEM_PAGES}]});
/// let job: PrintJob = serde_json::from_value(at_limit)?;
/// assert_eq!(job.pages(), MAX_ITEM_PAGES);
///
/// // ... one page above it does not.
/// let above = serde_json::json!({"id": 1, "items": [{"pages": MAX_ITEM_PAGES + 1}]});
/// assert!(serde_json::from_value::<PrintJob>(above).is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
pub const MAX_ITEM_PAGES: u32 = 10_000;

/// The most `items[]` entries a decoded job may carry.
///
/// One entry is one letter, and a job is built from a single upload that is
/// capped at [`crate::MAX_PDF_BYTES`] (50 MB): one addressed page per letter
/// makes 50 000 items far more than any upload can produce, while still
/// bounding what a hostile response can allocate. Decoding refuses more.
///
/// ```
/// use letterxpress::{PrintJob, MAX_JOB_ITEMS};
///
/// // The limit itself decodes ...
/// let at_limit = serde_json::json!({
///     "id": 1,
///     "items": vec![serde_json::json!({"pages": 1}); MAX_JOB_ITEMS],
/// });
/// let job: PrintJob = serde_json::from_value(at_limit)?;
/// assert_eq!(job.items.len(), MAX_JOB_ITEMS);
///
/// // ... one item above it does not.
/// let above = serde_json::json!({
///     "id": 1,
///     "items": vec![serde_json::json!({"pages": 1}); MAX_JOB_ITEMS + 1],
/// });
/// assert!(serde_json::from_value::<PrintJob>(above).is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
pub const MAX_JOB_ITEMS: usize = 50_000;

/// A page count as an unsigned number, or the reason it is not one.
fn pages_from_u64(value: u64) -> Result<u32, String> {
    u32::try_from(value)
        .ok()
        .filter(|pages| *pages <= MAX_ITEM_PAGES)
        .ok_or_else(|| {
            format!("a page count of {value} is above the documented maximum of {MAX_ITEM_PAGES}")
        })
}

/// Decodes `items[].pages` — a number, its integral string form or `null` — and
/// refuses anything outside `0..=MAX_ITEM_PAGES`.
///
/// The ceiling is a validation, not a guess: a job that reports more pages than
/// a letter can have is malformed, and silently truncating it would understate
/// what the caller is billed for.
fn de_pages<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    struct PagesVisitor;

    impl Visitor<'_> for PagesVisitor {
        type Value = u32;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "a page count from 0 to {MAX_ITEM_PAGES}, as a number, as its string form or null"
            )
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            pages_from_u64(value).map_err(E::custom)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            match u64::try_from(value) {
                Ok(value) => pages_from_u64(value).map_err(E::custom),
                Err(_) => Err(E::custom(format_args!(
                    "a page count must not be negative: {value}"
                ))),
            }
        }

        // `12.0` is twelve pages. A JSON writer that only emits floats is
        // spelling the same number, so it gets the same answer.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
            let in_range = (0.0..=f64::from(MAX_ITEM_PAGES)).contains(&value);
            if value.is_finite() && value.fract() == 0.0 && in_range {
                Ok(value as u32)
            } else {
                Err(E::custom(format_args!(
                    "a page count must be a whole number from 0 to {MAX_ITEM_PAGES}: {value}"
                )))
            }
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let trimmed = value.trim();
            if let Ok(number) = trimmed.parse::<u64>() {
                return pages_from_u64(number).map_err(E::custom);
            }
            match trimmed.parse::<f64>() {
                Ok(number) => self.visit_f64(number),
                Err(_) => Err(E::custom(format_args!(
                    "a page count must be a whole number from 0 to {MAX_ITEM_PAGES}: {value:?}"
                ))),
            }
        }

        // `null` is "not supplied" and means zero pages, like an absent field.
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(0)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(0)
        }
    }

    deserializer.deserialize_any(PagesVisitor)
}

/// Decodes `items[]` with [`MAX_JOB_ITEMS`] as a hard ceiling.
///
/// An unbounded array lets a response decide how much memory the caller
/// allocates; the limit is checked while the sequence is read, before the
/// allocation grows, and it is generous enough that no real job can reach it.
fn de_items<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<PrintItem>, D::Error> {
    struct ItemsVisitor;

    impl<'de> Visitor<'de> for ItemsVisitor {
        type Value = Vec<PrintItem>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "an array of at most {MAX_JOB_ITEMS} print items")
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_JOB_ITEMS));
            while let Some(item) = seq.next_element::<PrintItem>()? {
                if items.len() == MAX_JOB_ITEMS {
                    return Err(de::Error::custom(format_args!(
                        "a job carries at most {MAX_JOB_ITEMS} items"
                    )));
                }
                items.push(item);
            }
            Ok(items)
        }
    }

    deserializer.deserialize_seq(ItemsVisitor)
}

/// One address block inside a print job (`items[]`).
///
/// `amount` is **net**, `vat` the tax share; the amount actually charged is
/// [`PrintItem::total`] — the documentation's own ledger confirms it.
///
/// ```
/// use letterxpress::{Money, PrintItem};
///
/// let item: PrintItem = serde_json::from_str(
///     r#"{"address":"Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg","pages":1,"amount":0.68,"vat":0.13,"status":"sent"}"#,
/// )?;
/// assert_eq!(item.net.map(Money::cents), Some(68));
/// assert_eq!(item.vat.map(Money::cents), Some(13));
/// assert_eq!(item.total()?.map(Money::cents), Some(81));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PrintItem {
    /// The recipient address as printed in the PDF, one line.
    #[serde(default, deserialize_with = "de_opt_string", alias = "Address")]
    pub address: Option<String>,
    /// Pages in this item, `0` to [`MAX_ITEM_PAGES`].
    ///
    /// Decoded from a number, its integral string form (`"2"`) or `null`
    /// (nothing supplied → `0`). A negative count or one above the documented
    /// ceiling fails the decode: an absurd page count must not reach a page
    /// total or a cost estimate.
    #[serde(default, deserialize_with = "de_pages")]
    pub pages: u32,
    /// Net amount (wire key `amount`), or `None` when not reported.
    #[serde(default, rename = "amount")]
    pub net: Option<Money>,
    /// Tax share, or `None` when not reported.
    #[serde(default)]
    pub vat: Option<Money>,
    /// Item state (`queue`, `sent`, …).
    #[serde(default = "unspecified_status")]
    pub status: JobStatus,
    /// Registered-mail tracking number, when the provider has one.
    #[serde(default, deserialize_with = "de_opt_string", alias = "trackingCode")]
    pub tracking_code: Option<String>,
    /// Carrier status text (German, free-form).
    #[serde(default, deserialize_with = "de_opt_string", alias = "trackingStatus")]
    pub tracking_status: Option<String>,
}

impl PrintItem {
    /// Net amount, when reported.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintItem::default().net, None);
    /// ```
    #[must_use]
    pub fn net(&self) -> Option<Money> {
        self.net
    }

    /// Tax share, when reported.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintItem::default().vat, None);
    /// ```
    #[must_use]
    pub fn vat(&self) -> Option<Money> {
        self.vat
    }

    /// Net plus tax — what the provider actually charges.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] if the cent sum overflows. The outer `None`
    /// means the provider did not report both components; it is never treated
    /// as zero.
    ///
    /// ```
    /// use letterxpress::{Money, PrintItem};
    ///
    /// let item = PrintItem {
    ///     net: Some(Money::from_euros(0.67)?),
    ///     vat: Some(Money::from_euros(0.13)?),
    ///     ..PrintItem::default()
    /// };
    /// assert_eq!(item.total()?.map(Money::cents), Some(80));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn total(&self) -> Result<Option<Money>, MoneyError> {
        match (self.net, self.vat) {
            (Some(net), Some(vat)) => net.total(vat).map(Some),
            _ => Ok(None),
        }
    }
}

/// A print job as returned by every `printjobs` endpoint.
///
/// ```
/// use letterxpress::PrintJob;
///
/// let job: PrintJob = serde_json::from_str(
///     r#"{"id":6035143,"shipping":"national","mode":"simplex","color":"1","c4":0,"registered":null,
///         "bank_form":0,"notice":null,"status":"queue","group":null,"dispatch_date":null,
///         "filename_original":null,"created_at":"2022-08-01 13:38:07","updated_at":"2022-08-01 13:38:07",
///         "items":[]}"#,
/// )?;
/// assert_eq!(job.status, letterxpress::JobStatus::Queue);
/// assert_eq!(job.color, Some(letterxpress::Color::BlackAndWhite));
/// assert!(job.items.is_empty());
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// `c4` and `bank_form` follow the crate's flag rule, so a number and its
/// string form mean the same thing — and a token nobody can read fails the
/// decode instead of silently claiming the envelope was not requested:
///
/// ```
/// use letterxpress::PrintJob;
///
/// let job: PrintJob = serde_json::from_str(r#"{"id":1,"c4":"2","bank_form":"no"}"#)?;
/// assert!(job.c4, "any non-zero number is true, also as a string");
/// assert!(!job.bank_form);
///
/// let unreadable = serde_json::from_str::<PrintJob>(r#"{"id":1,"c4":"y"}"#);
/// assert!(unreadable.is_err(), "\"y\" is not a spelling this crate guesses at");
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// Page counts are bounded at the decode boundary, because [`PrintJob::pages`]
/// sums them: an item may claim up to [`MAX_ITEM_PAGES`] pages, one above is
/// refused, and so is an `items[]` array longer than [`MAX_JOB_ITEMS`].
///
/// ```
/// use letterxpress::PrintJob;
///
/// let at_limit: PrintJob = serde_json::from_str(r#"{"id":1,"items":[{"pages":10000}]}"#)?;
/// assert_eq!(at_limit.pages(), 10_000);
///
/// let above = serde_json::from_str::<PrintJob>(r#"{"id":1,"items":[{"pages":10001}]}"#);
/// assert!(above.is_err(), "more pages than a letter can have");
///
/// let negative = serde_json::from_str::<PrintJob>(r#"{"id":1,"items":[{"pages":-1}]}"#);
/// assert!(negative.is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PrintJob {
    /// Provider job id.
    pub id: JobId,
    /// Shipping class.
    #[serde(default)]
    pub shipping: Option<Shipping>,
    /// Print mode.
    #[serde(default)]
    pub mode: Option<PrintMode>,
    /// Colour.
    #[serde(default)]
    pub color: Option<Color>,
    /// Whether a C4 envelope was requested.
    ///
    /// Decoded by the flag rule: `0`, `false`, `no`, `""` and `null` are
    /// `false`; `1`, `true`, `yes` and every other non-zero number — as a
    /// number or as a string — are `true`.
    #[serde(default, deserialize_with = "de_flag")]
    pub c4: bool,
    /// Registered-mail option.
    #[serde(default)]
    pub registered: Option<Registered>,
    /// Whether a bank form was requested.
    ///
    /// Same flag rule as [`PrintJob::c4`]; `bankForm` is accepted too.
    #[serde(default, deserialize_with = "de_flag", alias = "bankForm")]
    pub bank_form: bool,
    /// Free-text notice.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub notice: Option<String>,
    /// Job state.
    #[serde(default = "unspecified_status")]
    pub status: JobStatus,
    /// Group, when the job was submitted by a serial letter.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub group: Option<String>,
    /// Planned dispatch date, `yyyy-mm-dd`.
    #[serde(default, deserialize_with = "de_opt_string", alias = "dispatchDate")]
    pub dispatch_date: Option<String>,
    /// File name the provider stored.
    #[serde(
        default,
        deserialize_with = "de_opt_string",
        alias = "filenameOriginal"
    )]
    pub filename_original: Option<String>,
    /// Creation timestamp, `"YYYY-MM-DD HH:MM:SS"` in an undocumented zone.
    #[serde(default, deserialize_with = "de_opt_string", alias = "createdAt")]
    pub created_at: Option<String>,
    /// Last-update timestamp, same format.
    #[serde(default, deserialize_with = "de_opt_string", alias = "updatedAt")]
    pub updated_at: Option<String>,
    /// One entry per letter in the job, at most [`MAX_JOB_ITEMS`].
    ///
    /// A longer array fails the decode instead of allocating whatever a hostile
    /// response asks for.
    #[serde(default, deserialize_with = "de_items")]
    pub items: Vec<PrintItem>,
}

impl PrintJob {
    /// Total pages across all items, saturated at [`u32::MAX`].
    ///
    /// The sum is accumulated with `checked_add`-style saturation, so no
    /// provider value can make it wrap to a small number or panic a debug
    /// build. A decoded job cannot reach the ceiling anyway — decoding bounds
    /// each item to [`MAX_ITEM_PAGES`] and the list to [`MAX_JOB_ITEMS`], so at
    /// most 500 000 000 pages are possible; only a hand-built job can saturate.
    ///
    /// ```
    /// use letterxpress::{PrintItem, PrintJob};
    ///
    /// let job = PrintJob {
    ///     items: vec![
    ///         PrintItem { pages: 2, ..PrintItem::default() },
    ///         PrintItem { pages: 3, ..PrintItem::default() },
    ///     ],
    ///     ..PrintJob::default()
    /// };
    /// assert_eq!(job.pages(), 5);
    ///
    /// // Hand-built and impossible: it saturates instead of wrapping to 0.
    /// let hostile = PrintJob {
    ///     items: vec![PrintItem { pages: u32::MAX, ..PrintItem::default() }; 2],
    ///     ..PrintJob::default()
    /// };
    /// assert_eq!(hostile.pages(), u32::MAX);
    /// ```
    #[must_use]
    pub fn pages(&self) -> u32 {
        self.items
            .iter()
            .fold(0u32, |sum, item| sum.saturating_add(item.pages))
    }

    /// Net sum across items.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] if a cent sum overflows. `None` means at
    /// least one item omitted a net amount, so the aggregate is unknown.
    ///
    /// ```
    /// assert!(letterxpress::PrintJob::default().net()?.unwrap().is_zero());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn net(&self) -> Result<Option<Money>, MoneyError> {
        self.items
            .iter()
            .try_fold(Some(Money::ZERO), |sum, item| add_reported(sum, item.net))
    }

    /// Gross sum across items — the amount billed for this job.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] if a cent sum overflows. `None` means any
    /// item has an unreported component; an empty job is `Some(ZERO)` because
    /// no amount was omitted.
    ///
    /// ```
    /// assert!(letterxpress::PrintJob::default().total()?.unwrap().is_zero());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn total(&self) -> Result<Option<Money>, MoneyError> {
        self.items.iter().try_fold(Some(Money::ZERO), |sum, item| {
            add_reported(sum, item.total()?)
        })
    }

    /// The first tracking code, when the job is registered mail.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintJob::default().tracking_code(), None);
    /// ```
    #[must_use]
    pub fn tracking_code(&self) -> Option<&str> {
        self.items
            .iter()
            .find_map(|item| item.tracking_code.as_deref())
    }

    /// Whether the job has reached a state the provider will not leave.
    ///
    /// ```
    /// assert!(!letterxpress::PrintJob::default().is_terminal());
    /// ```
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

/// A SMART@MAIL job.
///
/// ```
/// use letterxpress::{EmailJob, Money};
///
/// let job: EmailJob = serde_json::from_str(
///     r#"{"id":3813,"email_receiver":"userone@letterxpress.de","email_option":"maildirect",
///         "amount":0.05,"vat":0.01,"status":"queue","subject":"Monats Rechnung"}"#,
/// )?;
/// assert_eq!(job.status, letterxpress::JobStatus::Queue);
/// assert_eq!(job.total()?.map(Money::cents), Some(6));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EmailJob {
    /// Provider job id.
    pub id: JobId,
    /// Sender address chosen by the provider.
    #[serde(default, deserialize_with = "de_opt_string", alias = "emailSender")]
    pub email_sender: Option<String>,
    /// Recipient address.
    #[serde(default, deserialize_with = "de_opt_string", alias = "emailReceiver")]
    pub email_receiver: Option<String>,
    /// Delivery option.
    #[serde(default, alias = "emailOption")]
    pub email_option: Option<EmailOption>,
    /// Delivery timestamp.
    #[serde(default, deserialize_with = "de_opt_string", alias = "sentAt")]
    pub sent_at: Option<String>,
    /// Net amount (wire key `amount`), or `None` when not reported.
    #[serde(default, rename = "amount")]
    pub net: Option<Money>,
    /// Tax share, or `None` when not reported.
    #[serde(default)]
    pub vat: Option<Money>,
    /// Job state.
    #[serde(default = "unspecified_status")]
    pub status: JobStatus,
    /// Subject as extracted from the white-code.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub subject: Option<String>,
    /// Body as extracted from the white-code.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub content: Option<String>,
    /// Footer as extracted from the white-code.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub footer: Option<String>,
    /// Creation timestamp.
    #[serde(default, deserialize_with = "de_opt_string", alias = "createdAt")]
    pub created_at: Option<String>,
    /// Group, for serial letters.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub group: Option<String>,
    /// Print job this e-mail belongs to (MailPlus).
    #[serde(default, alias = "printjobId")]
    pub printjob_id: Option<JobId>,
    /// Embedded print job (MailPlus).
    #[serde(default)]
    pub printjob: Option<PrintJob>,
}

impl EmailJob {
    /// Net amount, when reported.
    ///
    /// ```
    /// assert_eq!(letterxpress::EmailJob::default().net(), None);
    /// ```
    #[must_use]
    pub fn net(&self) -> Option<Money> {
        self.net
    }

    /// Tax share, when reported.
    ///
    /// ```
    /// assert_eq!(letterxpress::EmailJob::default().vat(), None);
    /// ```
    #[must_use]
    pub fn vat(&self) -> Option<Money> {
        self.vat
    }

    /// Net plus tax, when both components were reported.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] if the cent sum overflows. `None` means the
    /// provider omitted at least one component.
    ///
    /// ```
    /// assert_eq!(letterxpress::EmailJob::default().total()?, None);
    /// # Ok::<(), letterxpress::MoneyError>(())
    /// ```
    pub fn total(&self) -> Result<Option<Money>, MoneyError> {
        match (self.net, self.vat) {
            (Some(net), Some(vat)) => net.total(vat).map(Some),
            _ => Ok(None),
        }
    }
}

impl Default for EmailJob {
    fn default() -> Self {
        Self {
            id: JobId(String::new()),
            email_sender: None,
            email_receiver: None,
            email_option: None,
            sent_at: None,
            net: None,
            vat: None,
            status: unspecified_status(),
            subject: None,
            content: None,
            footer: None,
            created_at: None,
            group: None,
            printjob_id: None,
            printjob: None,
        }
    }
}

impl Default for PrintJob {
    fn default() -> Self {
        Self {
            id: JobId(String::new()),
            shipping: None,
            mode: None,
            color: None,
            c4: false,
            registered: None,
            bank_form: false,
            notice: None,
            status: unspecified_status(),
            group: None,
            dispatch_date: None,
            filename_original: None,
            created_at: None,
            updated_at: None,
            items: Vec::new(),
        }
    }
}

/// What `POST /v3/emailjobs` returned.
///
/// The provider answers with three different shapes depending on how the PDF
/// was built (single job, serial letter, or a mix of print and e-mail jobs).
/// The variant is derived from the payload, never guessed.
///
/// ```
/// use letterxpress::EmailJobSubmission;
///
/// let single = EmailJobSubmission::parse(serde_json::json!({"id": 3096}))?;
/// assert!(matches!(single, EmailJobSubmission::Single(_)));
///
/// let serial = EmailJobSubmission::parse(serde_json::json!([{"id": 1}, {"id": 2}]))?;
/// assert!(matches!(serial, EmailJobSubmission::Serial(jobs) if jobs.len() == 2));
/// # Ok::<(), serde_json::Error>(())
/// ```
// The `Single` variant carries a whole job (624 bytes). The enum is returned by
// value and never stored, so boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EmailJobSubmission {
    /// One e-mail job.
    Single(EmailJob),
    /// One job per white-code in a serial letter.
    Serial(Vec<EmailJob>),
    /// A serial letter that produced print jobs and e-mail jobs.
    Mixed {
        /// Print jobs created.
        print_jobs: Vec<PrintJob>,
        /// E-mail jobs created.
        email_jobs: Vec<EmailJob>,
    },
}

/// The three answer shapes `POST /v3/emailjobs` documents, selected by `serde`
/// from the payload itself instead of by sniffing keys.
///
/// The order is the precedence the provider uses: an array is a serial answer,
/// an object carrying `printjobs` or `emailjobs` is a mixed answer, and only an
/// object without either — one that carries an `id` — is a single job.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Answer {
    /// One job per white-code in a serial letter.
    Serial(Vec<EmailJob>),
    /// A serial letter that produced print jobs and e-mail jobs.
    Mixed(MixedAnswer),
    /// One e-mail job, boxed so a private enum stays small next to the
    /// collection variants.
    Single(Box<EmailJob>),
}

/// The `printjobs`/`emailjobs` block of a mixed answer, in the two spellings the
/// provider uses: both collections, or only one of them.
///
/// Each variant demands its key, so an object with an `id` and neither
/// collection cannot be mistaken for a mixed answer.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MixedAnswer {
    /// Both collections, as documented.
    Both {
        /// Print jobs created.
        printjobs: Vec<PrintJob>,
        /// E-mail jobs created.
        emailjobs: Vec<EmailJob>,
    },
    /// A submission that produced print jobs only.
    PrintOnly {
        /// Print jobs created.
        printjobs: Vec<PrintJob>,
    },
    /// A submission that produced e-mail jobs only.
    EmailOnly {
        /// E-mail jobs created.
        emailjobs: Vec<EmailJob>,
    },
}

impl MixedAnswer {
    /// The two collections; a spelling that lacks one leaves it empty.
    fn into_collections(self) -> (Vec<PrintJob>, Vec<EmailJob>) {
        match self {
            Self::Both {
                printjobs,
                emailjobs,
            } => (printjobs, emailjobs),
            Self::PrintOnly { printjobs } => (printjobs, Vec::new()),
            Self::EmailOnly { emailjobs } => (Vec::new(), emailjobs),
        }
    }
}

impl EmailJobSubmission {
    /// Parses any of the three documented shapes.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the payload is neither shape, and when a
    /// mixed envelope carries both collections empty.
    ///
    /// ```
    /// use letterxpress::EmailJobSubmission;
    ///
    /// let mixed = EmailJobSubmission::parse(serde_json::json!({
    ///     "printjobs": [{"id": 1}],
    ///     "emailjobs": [{"id": 2}],
    /// }))?;
    /// assert!(matches!(mixed, EmailJobSubmission::Mixed { print_jobs, email_jobs } if print_jobs.len() == 1 && email_jobs.len() == 1));
    ///
    /// assert!(EmailJobSubmission::parse(serde_json::json!({"nothing": true})).is_err());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    pub fn parse(value: Value) -> Result<Self, serde_json::Error> {
        match serde_json::from_value::<Answer>(value)? {
            Answer::Serial(jobs) => Ok(Self::Serial(jobs)),
            Answer::Single(job) => Ok(Self::Single(*job)),
            Answer::Mixed(mixed) => {
                let (print_jobs, email_jobs) = mixed.into_collections();
                if print_jobs.is_empty() && email_jobs.is_empty() {
                    return Err(de::Error::custom(
                        "email job response has neither printjobs nor emailjobs",
                    ));
                }
                Ok(Self::Mixed {
                    print_jobs,
                    email_jobs,
                })
            }
        }
    }

    /// All e-mail jobs in the response.
    ///
    /// ```
    /// use letterxpress::EmailJobSubmission;
    ///
    /// let serial = EmailJobSubmission::parse(serde_json::json!([{"id": 1}, {"id": 2}]))?;
    /// assert_eq!(serial.email_jobs().len(), 2);
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    #[must_use]
    pub fn email_jobs(&self) -> Vec<&EmailJob> {
        match self {
            Self::Single(job) => vec![job],
            Self::Serial(jobs) => jobs.iter().collect(),
            Self::Mixed { email_jobs, .. } => email_jobs.iter().collect(),
        }
    }

    /// All print jobs in the response.
    ///
    /// ```
    /// use letterxpress::EmailJobSubmission;
    ///
    /// let single = EmailJobSubmission::parse(serde_json::json!({"id": 1}))?;
    /// assert!(single.print_jobs().is_empty());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    #[must_use]
    pub fn print_jobs(&self) -> Vec<&PrintJob> {
        match self {
            Self::Mixed { print_jobs, .. } => print_jobs.iter().collect(),
            _ => Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for EmailJobSubmission {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// Laravel-style pagination block, present on every list endpoint.
///
/// ```
/// use letterxpress::Pagination;
///
/// let page: Pagination = serde_json::from_str(
///     r#"{"total":62,"count":15,"current_page":1,"last_page":5,"per_page":15,"next_page_url":"https://api.letterxpress.de/v3/transactions?page=2"}"#,
/// )?;
/// assert!(page.has_next());
/// assert_eq!(page.per_page, 15);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pagination {
    /// Total entries across all pages.
    pub total: u32,
    /// Entries on this page.
    pub count: u32,
    /// Current page, 1-based.
    pub current_page: u32,
    /// Last page.
    pub last_page: u32,
    /// Page size (15 in every documented example).
    pub per_page: u32,
    /// First-page URL.
    pub first_page_url: Option<String>,
    /// Last-page URL.
    pub last_page_url: Option<String>,
    /// Previous-page URL.
    pub prev_page_url: Option<String>,
    /// Next-page URL.
    pub next_page_url: Option<String>,
}

impl Pagination {
    /// Whether another page exists.
    ///
    /// Continuation follows the provider's explicit next-page link as well as
    /// the page numbers. A response that omits or understates `last_page` must
    /// not make a non-final page look complete.
    ///
    /// ```
    /// use letterxpress::Pagination;
    ///
    /// assert!(!Pagination::default().has_next());
    ///
    /// let linked: Pagination = serde_json::from_str(
    ///     r#"{"current_page":1,"next_page_url":"https://api.letterxpress.de/v3/invoices?page=2"}"#,
    /// )?;
    /// assert!(linked.has_next());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.next_page_url.is_some() || self.current_page < self.last_page
    }
}

/// Wire representation used to distinguish an absent count from a zero.
#[derive(Deserialize)]
struct PaginationWire {
    #[serde(default)]
    total: u32,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    current_page: Option<u32>,
    #[serde(default)]
    last_page: Option<u32>,
    #[serde(default)]
    per_page: Option<u32>,
    #[serde(default, deserialize_with = "de_opt_string", alias = "firstPageUrl")]
    first_page_url: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string", alias = "lastPageUrl")]
    last_page_url: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string", alias = "prevPageUrl")]
    prev_page_url: Option<String>,
    #[serde(default, deserialize_with = "de_opt_string", alias = "nextPageUrl")]
    next_page_url: Option<String>,
}

impl TryFrom<PaginationWire> for Pagination {
    type Error = &'static str;

    fn try_from(wire: PaginationWire) -> Result<Self, Self::Error> {
        if wire.per_page == Some(0) {
            return Err("pagination.per_page must be at least 1 when supplied");
        }
        if let (Some(current_page), Some(last_page)) = (wire.current_page, wire.last_page) {
            if last_page < current_page {
                return Err("pagination.last_page must not precede current_page");
            }
        }
        Ok(Self {
            total: wire.total,
            count: wire.count,
            current_page: wire.current_page.unwrap_or_default(),
            last_page: wire.last_page.unwrap_or_default(),
            per_page: wire.per_page.unwrap_or_default(),
            first_page_url: wire.first_page_url,
            last_page_url: wire.last_page_url,
            prev_page_url: wire.prev_page_url,
            next_page_url: wire.next_page_url,
        })
    }
}

impl<'de> Deserialize<'de> for Pagination {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(PaginationWire::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// One page of print jobs.
///
/// ```
/// use letterxpress::PrintJobPage;
///
/// let page: PrintJobPage = serde_json::from_str(r#"{"printjobs":[],"pagination":{"total":0}}"#)?;
/// assert!(page.jobs.is_empty());
/// assert!(page.pagination.is_some());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PrintJobPage {
    /// Jobs on this page.
    #[serde(default, rename = "printjobs")]
    pub jobs: Vec<PrintJob>,
    /// Pagination block, when the provider sent one.
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

/// One page of e-mail jobs.
///
/// The documented example puts the job fields at the top level of `data` and
/// `pagination` beside it, which is malformed. The parser accepts that, a
/// proper `{"emailjobs": [...]}` envelope, and a bare array.
///
/// ```
/// use letterxpress::EmailJobPage;
///
/// let page = EmailJobPage::parse(serde_json::json!({"id": 3093}), None)?;
/// assert_eq!(page.jobs.len(), 1);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmailJobPage {
    /// Jobs on this page.
    pub jobs: Vec<EmailJob>,
    /// Pagination block, when the provider sent one.
    pub pagination: Option<Pagination>,
}

impl EmailJobPage {
    /// Parses the documented shapes, with an optional pagination block taken
    /// from the envelope.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the payload is not a job, an array of jobs or
    /// an `{"emailjobs": [...]}` envelope.
    ///
    /// ```
    /// use letterxpress::EmailJobPage;
    ///
    /// let page = EmailJobPage::parse(serde_json::json!({"emailjobs": [{"id": 1}, {"id": 2}]}), None)?;
    /// assert_eq!(page.jobs.len(), 2);
    ///
    /// let empty = EmailJobPage::parse(serde_json::json!({"emailjobs": []}), None)?;
    /// assert!(empty.jobs.is_empty());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    #[allow(clippy::needless_pass_by_value)]
    pub fn parse(data: Value, pagination: Option<Value>) -> Result<Self, serde_json::Error> {
        let mut parsed_pagination: Option<Pagination> = match &data {
            Value::Object(map) => map
                .get("pagination")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?,
            _ => None,
        };
        if let (Some(pagination), None) = (pagination, &parsed_pagination) {
            if !pagination.is_null() {
                parsed_pagination = Some(serde_json::from_value(pagination)?);
            }
        }

        let jobs = match data {
            Value::Array(_) => serde_json::from_value::<Vec<EmailJob>>(data)?,
            Value::Object(ref map) => {
                if let Some(list) = map.get("emailjobs") {
                    serde_json::from_value::<Vec<EmailJob>>(list.clone())?
                } else if map.contains_key("id") {
                    vec![serde_json::from_value::<EmailJob>(data.clone())?]
                } else {
                    return Err(de::Error::custom(
                        "email job list is neither an array nor an object with `id`",
                    ));
                }
            }
            _ => {
                return Err(de::Error::custom(
                    "email job list must be an object or an array",
                ))
            }
        };
        Ok(Self {
            jobs,
            pagination: parsed_pagination,
        })
    }
}

/// Credit balance (`GET /v3/balance`).
///
/// ```
/// use letterxpress::Balance;
///
/// let balance: Balance = serde_json::from_str(r#"{"balance":54.89,"currency":"EUR"}"#)?;
/// assert_eq!(balance.balance.cents(), 5489);
/// assert_eq!(balance.currency.as_deref(), Some("EUR"));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Balance {
    /// Available credit.
    #[serde(default, alias = "Balance")]
    pub balance: Money,
    /// Currency, `EUR` in every documented example.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub currency: Option<String>,
}

impl Balance {
    /// The credit as euro cents.
    ///
    /// ```
    /// assert_eq!(letterxpress::Balance::default().cents(), 0);
    /// ```
    #[must_use]
    pub fn cents(&self) -> i64 {
        self.balance.cents()
    }
}

/// One ledger entry (`GET /v3/transactions`).
///
/// Ledger debits are negative and credits positive. Both are exact signed
/// [`Money`] values.
///
/// ```
/// use letterxpress::{Money, Transaction};
///
/// let entry: Transaction = serde_json::from_str(
///     r#"{"amount":0.81,"currency":"EUR","description":"Schwarzweiß, Einseitig, National, 1 Seite","created_at":"2022-09-21 11:45:28"}"#,
/// )?;
/// assert_eq!(entry.amount.map(Money::cents), Some(81));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Transaction {
    /// Signed amount, or `None` when not reported.
    #[serde(default)]
    pub amount: Option<Money>,
    /// Currency.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub currency: Option<String>,
    /// Provider description of what was charged.
    #[serde(default, deserialize_with = "de_opt_string")]
    pub description: Option<String>,
    /// Creation timestamp.
    #[serde(default, deserialize_with = "de_opt_string", alias = "createdAt")]
    pub created_at: Option<String>,
}

/// One page of ledger entries.
///
/// ```
/// use letterxpress::TransactionPage;
///
/// let page: TransactionPage = serde_json::from_str(r#"{"transactions":[],"pagination":{"total":0}}"#)?;
/// assert!(page.entries.is_empty());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct TransactionPage {
    /// Entries on this page.
    #[serde(default, rename = "transactions")]
    pub entries: Vec<Transaction>,
    /// Pagination block.
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

/// An invoice summary (`GET /v3/invoices`).
///
/// ```
/// use letterxpress::{Invoice, Money};
///
/// let invoice: Invoice = serde_json::from_str(r#"{"id":25402,"amount":0.67,"vat":0.13,"invoice_date":"2020-05-31"}"#)?;
/// assert_eq!(invoice.total()?.map(Money::cents), Some(80));
/// assert_eq!(invoice.amount.map(Money::cents), Some(67));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Invoice {
    /// Invoice id.
    pub id: InvoiceId,
    /// Net amount, or `None` when not reported.
    #[serde(default)]
    pub amount: Option<Money>,
    /// Tax share, or `None` when not reported.
    #[serde(default)]
    pub vat: Option<Money>,
    /// Invoice date, `yyyy-mm-dd`.
    #[serde(default, deserialize_with = "de_opt_string", alias = "invoiceDate")]
    pub invoice_date: Option<String>,
}

impl Invoice {
    /// Net plus tax — the invoiced amount.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] if the cent sum overflows. `None` means the
    /// provider omitted either component, so the invoiced amount is unknown.
    ///
    /// ```
    /// use letterxpress::{Invoice, Money};
    ///
    /// let invoice = Invoice {
    ///     amount: Some(Money::from_euros(0.67)?),
    ///     vat: Some(Money::from_euros(0.13)?),
    ///     ..Invoice::default()
    /// };
    /// assert_eq!(invoice.total()?.map(Money::cents), Some(80));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn total(&self) -> Result<Option<Money>, MoneyError> {
        match (self.amount, self.vat) {
            (Some(amount), Some(vat)) => amount.total(vat).map(Some),
            _ => Ok(None),
        }
    }
}

/// One page of invoices.
///
/// ```
/// use letterxpress::InvoicePage;
///
/// let page: InvoicePage = serde_json::from_str(r#"{"invoices":[],"pagination":{"total":0}}"#)?;
/// assert!(page.invoices.is_empty());
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct InvoicePage {
    /// Invoices on this page.
    #[serde(default, rename = "invoices")]
    pub invoices: Vec<Invoice>,
    /// Pagination block.
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

/// A single invoice including its PDF (`GET /v3/invoices/{id}`).
///
/// ```
/// use letterxpress::InvoiceDocument;
///
/// let document: InvoiceDocument = serde_json::from_str(
///     r#"{"id":25402,"amount":0.67,"vat":0.13,"invoice_date":"2020-05-31","base64_data":"JVBERi0xLjQK"}"#,
/// )?;
/// assert_eq!(document.invoice.id.as_str(), "25402");
/// assert!(document.pdf()?.starts_with(b"%PDF-"));
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InvoiceDocument {
    /// The invoice summary.
    #[serde(flatten)]
    pub invoice: Invoice,
    /// Base64 PDF.
    #[serde(default, deserialize_with = "de_opt_string", alias = "base64Data")]
    pub base64_data: Option<String>,
}

impl InvoiceDocument {
    /// Decodes the invoice PDF.
    ///
    /// # Errors
    ///
    /// [`Error::MissingField`] when the provider sent no PDF,
    /// [`Error::Base64`] when it is not valid base64,
    /// [`Error::TooLarge`] when the decoded payload exceeds the documented
    /// 50 MB ceiling.
    ///
    /// ```
    /// use letterxpress::InvoiceDocument;
    ///
    /// let document: InvoiceDocument = serde_json::from_str(r#"{"id":1,"base64_data":null}"#)?;
    /// assert!(document.pdf().is_err());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    pub fn pdf(&self) -> Result<Vec<u8>, Error> {
        let encoded = self.base64_data.as_deref().ok_or(Error::MissingField {
            endpoint: "GET /v3/invoices/{id}",
            field: "base64_data",
        })?;
        crate::decode_base64("invoice.base64_data", encoded)
    }
}

/// The journal export (`GET /v3/journal`): a base64 CSV.
///
/// ```
/// use letterxpress::Journal;
///
/// let journal: Journal = serde_json::from_str(r#"{"csv_base64":"SWQ7TmFtZQ=="}"#)?;
/// assert_eq!(journal.csv()?, "Id;Name");
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Journal {
    /// Base64-encoded CSV.
    #[serde(default, deserialize_with = "de_opt_string", alias = "csvBase64")]
    pub csv_base64: Option<String>,
}

impl Journal {
    /// Decodes the CSV as UTF-8 text.
    ///
    /// # Errors
    ///
    /// [`Error::MissingField`] when the provider sent no document,
    /// [`Error::Base64`], [`Error::Utf8`], [`Error::TooLarge`].
    ///
    /// ```
    /// use letterxpress::Journal;
    ///
    /// let journal: Journal = serde_json::from_str(r#"{"csv_base64":null}"#)?;
    /// assert!(journal.csv().is_err());
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    pub fn csv(&self) -> Result<String, Error> {
        let encoded = self.csv_base64.as_deref().ok_or(Error::MissingField {
            endpoint: "GET /v3/journal",
            field: "csv_base64",
        })?;
        let bytes = crate::decode_base64("journal.csv_base64", encoded)?;
        String::from_utf8(bytes).map_err(|_| Error::Utf8 {
            field: "journal.csv_base64",
        })
    }
}

/// The echo of the priced letter in `GET /v3/price`.
///
/// ```
/// use letterxpress::PriceEcho;
///
/// let echo = PriceEcho::from_envelope(&serde_json::json!({
///     "specification": {"pages": 1, "color": "4", "mode": "simplex", "shipping": "national", "email_option": "mailsecure", "c4": 0},
///     "registered": "r1"
/// }))?;
/// assert_eq!(echo.pages, 1);
/// assert_eq!(echo.color, Some(letterxpress::Color::Color));
/// assert_eq!(echo.registered, Some(letterxpress::Registered::EinschreibenEinwurf));
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// `c4` is read with the same flag rule as [`PrintJob::c4`]: `"0"` and `0` are
/// the same thing, `"2"` is `true`, and an unreadable spelling is refused
/// instead of being reported as "no envelope":
///
/// ```
/// use letterxpress::PriceEcho;
///
/// let echo = PriceEcho::from_envelope(&serde_json::json!({"specification": {"c4": "2"}}))?;
/// assert!(echo.c4);
///
/// let refused = PriceEcho::from_envelope(&serde_json::json!({"specification": {"c4": "y"}}));
/// assert!(refused.is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// `registered` is the one field the provider echoes in either of two places:
/// inside `specification` and next to it. The letter-level value wins when it
/// carries a code, and the priced value survives when it does not — an absent
/// key, `null` or an empty string is "not supplied", never "not registered":
///
/// ```
/// use letterxpress::{PriceEcho, Registered};
///
/// // Echoed only inside the specification: that is the value that was priced.
/// let inside = PriceEcho::from_envelope(&serde_json::json!({
///     "specification": {"pages": 1, "registered": "r2"},
/// }))?;
/// assert_eq!(inside.registered, Some(Registered::Einschreiben));
///
/// // Both places say something: the letter-level code is the priced one.
/// let both = PriceEcho::from_envelope(&serde_json::json!({
///     "specification": {"pages": 1, "registered": "r1"},
///     "registered": "r2",
/// }))?;
/// assert_eq!(both.registered, Some(Registered::Einschreiben));
///
/// // The letter level is silent, so it must not erase the priced code.
/// for silent in [serde_json::Value::Null, serde_json::json!("")] {
///     let kept = PriceEcho::from_envelope(&serde_json::json!({
///         "specification": {"pages": 1, "registered": "r2"},
///         "registered": silent,
///     }))?;
///     assert_eq!(kept.registered, Some(Registered::Einschreiben));
/// }
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// A code that arrives as a number is not dropped either: it goes through the
/// same [`Registered::from_wire`] path as a string, so it is normalized like
/// any other code this crate does not model: surrounding whitespace is trimmed
/// and ASCII letters are lowercased.
///
/// ```
/// use letterxpress::{PriceEcho, Registered};
///
/// let echo = PriceEcho::from_envelope(&serde_json::json!({
///     "specification": {"pages": 1},
///     "registered": 2,
/// }))?;
/// assert_eq!(echo.registered, Some(Registered::Other("2".into())));
///
/// // A value that cannot be a code at all is refused, not guessed at.
/// let nested = PriceEcho::from_envelope(&serde_json::json!({
///     "specification": {"pages": 1},
///     "registered": {"code": "r2"},
/// }));
/// assert!(nested.is_err());
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// Without a `specification` there is no echo at all: the read is refused
/// rather than answered with "zero pages, nothing registered", which would
/// misreport a letter the provider did price.
///
/// ```
/// use letterxpress::PriceEcho;
///
/// assert!(PriceEcho::from_envelope(&serde_json::json!({"registered": "r1"})).is_err());
/// assert!(PriceEcho::from_envelope(&serde_json::json!({"specification": null})).is_err());
///
/// // The emptiest honest echo: a specification that says nothing.
/// let empty = PriceEcho::from_envelope(&serde_json::json!({"specification": {}}))?;
/// assert_eq!(empty.pages, 0);
/// assert_eq!(empty.registered, None);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PriceEcho {
    /// Pages that were priced, `0` to [`MAX_ITEM_PAGES`].
    ///
    /// Decoded with the same bounded rule as [`PrintItem::pages`]: `2`, `"2"`
    /// and `2.0` are the same count, `null` is zero, and anything above the
    /// documented ceiling is refused rather than echoed into a cost estimate.
    #[serde(default, deserialize_with = "de_pages")]
    pub pages: u32,
    /// Colour that was priced.
    #[serde(default)]
    pub color: Option<Color>,
    /// Print mode that was priced.
    #[serde(default)]
    pub mode: Option<PrintMode>,
    /// Shipping class that was priced.
    #[serde(default)]
    pub shipping: Option<Shipping>,
    /// SMART@MAIL option that was priced.
    #[serde(default, alias = "emailOption")]
    pub email_option: Option<EmailOption>,
    /// Whether a C4 envelope was priced.
    ///
    /// Same flag rule as [`PrintJob::c4`].
    #[serde(default, deserialize_with = "de_flag")]
    pub c4: bool,
    /// Registered-mail option that was priced.
    #[serde(default)]
    pub registered: Option<Registered>,
}

impl PriceEcho {
    /// Builds the echo from the nested `letter.specification` shape the
    /// provider actually returns.
    ///
    /// ```
    /// use letterxpress::PriceEcho;
    ///
    /// let echo = PriceEcho::from_envelope(&serde_json::json!({
    ///     "specification": {"pages": 2, "color": "1", "mode": "duplex", "shipping": "national"},
    ///     "registered": "r2"
    /// }))?;
    /// assert_eq!(echo.pages, 2);
    /// # Ok::<(), serde_json::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when `letter` has no `specification` (or carries
    /// `null` there) — an echo nobody can read is refused instead of invented —
    /// when `specification` is not an object, when a flag inside it is
    /// unreadable, or when the letter-level `registered` is a token that cannot
    /// be a code (an object, an array).
    pub fn from_envelope(letter: &Value) -> Result<Self, serde_json::Error> {
        let specification = letter
            .get("specification")
            .filter(|value| !value.is_null())
            .ok_or_else(|| de::Error::missing_field("specification"))?;
        let mut echo = <Self as Deserialize>::deserialize(specification)?;
        // The provider echoes the flag next to `specification` as well. Only a
        // value that carries a code may replace the priced one.
        if let Some(registered) = letter_registered(letter.get("registered"))? {
            echo.registered = Some(registered);
        }
        Ok(echo)
    }
}

/// The letter-level `registered` echo, when the provider sent one.
///
/// The flag lives either inside `specification` or next to it, so this is the
/// half that must stay silent about the half it cannot see: an absent key,
/// `null`, an empty string and a string of blanks all mean "not supplied" and
/// leave whatever the specification priced untouched. Everything else goes
/// through [`Registered`]'s own decoding rule, which reads a number as its
/// string form and carries an unknown code after trimming and ASCII-lowercasing;
/// a token that is not a code at all fails the deserialiser instead of being
/// reported as "not registered".
fn letter_registered(value: Option<&Value>) -> Result<Option<Registered>, serde_json::Error> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    match <Registered as Deserialize>::deserialize(value)? {
        // `""` is the spelling `Registered` uses for a value that is there but
        // names nothing; it is "not supplied", not a code.
        Registered::Other(raw) if raw.is_empty() => Ok(None),
        registered => Ok(Some(registered)),
    }
}

/// The answer to `GET /v3/price`.
///
/// ```
/// use letterxpress::Price;
///
/// let price: Price = serde_json::from_str(
///     r#"{"price":4.99,"letter":{"specification":{"pages":1,"color":"4"},"registered":"r1"}}"#,
/// )?;
/// assert_eq!(price.price.cents(), 499);
/// assert_eq!(price.echo.as_ref().unwrap().pages, 1);
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// An echo is built only from a `letter` **object**; `null`, a string or an
/// array carries nothing to echo and is ignored, so a body this crate cannot
/// place never costs the caller the price. A `letter` object whose
/// `specification` is missing is different — that is a price with a hole where
/// the letter should be, and it fails the decode instead of being reported as
/// "zero pages, nothing registered" (see [`PriceEcho::from_envelope`]):
///
/// ```
/// use letterxpress::Price;
///
/// let bare: Price = serde_json::from_str(r#"{"price":1.0}"#)?;
/// assert!(bare.echo.is_none());
///
/// let ignored: Price = serde_json::from_str(r#"{"price":1.0,"letter":"nope"}"#)?;
/// assert!(ignored.echo.is_none());
///
/// let hole = serde_json::from_str::<Price>(r#"{"price":1.0,"letter":{"registered":"r1"}}"#);
/// assert!(hole.is_err(), "no specification is not an echo");
///
/// let also_hole = serde_json::from_str::<Price>(r#"{"price":1.0,"letter":{}}"#);
/// assert!(also_hole.is_err(), "no specification is not an echo");
///
/// // One empty object is enough to say "a letter was priced, nothing set".
/// let empty: Price = serde_json::from_str(r#"{"price":1.0,"letter":{"specification":{}}}"#)?;
/// assert_eq!(empty.echo.as_ref().map(|echo| echo.pages), Some(0));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Price {
    /// Quoted price in euro.
    pub price: Money,
    /// Echo of the priced specification.
    pub echo: Option<PriceEcho>,
}

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            price: Money,
            #[serde(default)]
            letter: Option<Value>,
        }

        let raw = Raw::deserialize(deserializer)?;
        // Only an object can hold a specification. Anything else is not an
        // echo, and refusing the whole price over it would cost the caller the
        // number that is actually useful.
        let echo = match raw.letter {
            Some(value) if value.is_object() => {
                Some(PriceEcho::from_envelope(&value).map_err(de::Error::custom)?)
            }
            _ => None,
        };
        Ok(Self {
            price: raw.price,
            echo,
        })
    }
}

/// The result of a `DELETE`: the provider's message, no data.
///
/// ```
/// use letterxpress::Deletion;
///
/// let deletion = Deletion { message: "Print job deleted successfully".to_string() };
/// assert!(deletion.message.contains("deleted"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Deletion {
    /// Message from the provider, e.g. "Print job deleted successfully".
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(value: serde_json::Value) -> Option<String> {
        de_opt_string(value).expect("a scalar decodes")
    }

    #[test]
    fn a_number_and_its_string_form_mean_the_same_in_both_directions() {
        // The boundary of the numeric rule: zero is false, the smallest
        // positive and the smallest negative value are already true — as a
        // number and as a string.
        for value in [json!(0), json!(0i64), json!(0u64), json!(0.0)] {
            assert!(!de_flag(&value).unwrap(), "{value} is zero");
        }
        for value in [
            json!(1),
            json!(-1),
            json!(1u64),
            json!(1.0),
            json!(0.5),
            json!(-0.5),
            json!(2),
            json!(i64::MAX),
            json!(u64::MAX),
        ] {
            assert!(de_flag(&value).unwrap(), "{value} is not zero");
        }

        // String forms: identical answers to the numbers above.
        assert!(!de_flag(json!("0")).unwrap());
        assert!(!de_flag(json!("00")).unwrap());
        assert!(!de_flag(json!("0.0")).unwrap());
        assert!(!de_flag(json!(" -0 ")).unwrap());
        assert!(de_flag(json!("1")).unwrap());
        assert!(de_flag(json!("01")).unwrap());
        assert!(de_flag(json!("+1")).unwrap());
        assert!(de_flag(json!("-1")).unwrap());
        assert!(de_flag(json!("2")).unwrap());
        assert!(de_flag(json!("0.5")).unwrap());
        assert!(de_flag(json!("1e3")).unwrap());
        assert!(de_flag(json!(" 1 ")).unwrap());
        assert!(de_flag(json!("18446744073709551615")).unwrap());
    }

    #[test]
    fn every_flag_word_is_case_insensitive() {
        for value in ["true", "TRUE", "True", "TrUe", "yes", "YES", "Yes", " yes "] {
            assert!(de_flag(json!(value)).unwrap(), "{value} is set");
        }
        for value in ["false", "FALSE", "False", "no", "NO", "No", " no "] {
            assert!(!de_flag(json!(value)).unwrap(), "{value} is clear");
        }
    }

    #[test]
    fn an_absent_flag_is_false_but_an_unreadable_one_is_an_error() {
        assert!(!de_flag(json!(null)).unwrap());
        assert!(!de_flag(json!("")).unwrap());
        assert!(!de_flag(json!("   ")).unwrap());

        // Just outside the rule: these used to decode as `false`, which is how
        // a billed C4 envelope disappeared.
        for value in ["y", "Y", "n", "on", "off", "2 fasle", "1.0.0", "NaN", "inf"] {
            assert!(
                de_flag(json!(value)).is_err(),
                "{value:?} must be refused, not guessed"
            );
        }

        // A token that cannot be a flag at all is refused too, never `false`.
        assert!(de_flag(json!({"c4": 1})).is_err());
        assert!(de_flag(json!([1])).is_err());
    }

    #[test]
    fn the_flag_error_names_the_value_and_the_expected_spellings() {
        let error = de_flag::<serde_json::Value>(json!("y")).expect_err("refused");
        let text = error.to_string();
        assert!(text.contains('y'), "{text}");
        assert!(text.contains("true/false"), "{text}");
    }

    #[test]
    fn text_fields_accept_every_scalar_and_null() {
        assert_eq!(text(json!("x")), Some("x".to_owned()));
        assert_eq!(text(json!(42)), Some("42".to_owned()));
        assert_eq!(text(json!(-5)), Some("-5".to_owned()));
        assert_eq!(text(json!(1.5)), Some("1.5".to_owned()));
        assert_eq!(text(json!(true)), Some("true".to_owned()));
        assert_eq!(text(json!(null)), None);
        assert_eq!(
            text(json!(u64::MAX)),
            Some(u64::MAX.to_string()),
            "an integer above i64::MAX keeps every digit"
        );
        assert_eq!(
            text(json!(6_035_138.0)),
            Some("6035138".to_owned()),
            "an integral float is text without the fraction"
        );
        // A structure is not a scalar: the field is documented as text.
        assert!(de_opt_string(json!({"a": 1})).is_err());
        assert!(de_opt_string(json!([1])).is_err());
    }

    #[test]
    fn ids_accept_numbers_strings_and_float_spellings() {
        let from_float: JobId =
            serde_json::from_value(json!(6_035_138.0)).expect("an id rendered as a float");
        assert_eq!(from_float.as_str(), "6035138");

        let from_string: JobId = serde_json::from_value(json!(" 42 ")).expect("a padded id");
        assert_eq!(from_string.as_str(), "42", "surrounding space is trimmed");

        let above_i64: JobId = serde_json::from_value(json!(u64::MAX)).expect("an unsigned id");
        assert_eq!(above_i64.as_str(), u64::MAX.to_string());

        let longest: JobId =
            serde_json::from_value(json!("x".repeat(32))).expect("the documented maximum");
        assert_eq!(longest.as_str().len(), 32);
        assert!(
            serde_json::from_value::<JobId>(json!("x".repeat(33))).is_err(),
            "33 characters is one over the documented maximum"
        );

        assert!(
            serde_json::from_value::<JobId>(json!(1.5)).is_err(),
            "a fraction is not an id"
        );
        assert!(
            serde_json::from_value::<JobId>(json!(true)).is_err(),
            "a boolean is not an id, even though `true` is alphanumeric"
        );
        assert!(
            serde_json::from_value::<JobId>(json!({"id": 1})).is_err(),
            "a structure is not an id"
        );
    }

    #[test]
    fn every_answer_shape_is_selected_from_the_payload() {
        let both = EmailJobSubmission::parse(json!({
            "printjobs": [{"id": 1}],
            "emailjobs": [{"id": 2}, {"id": 3}],
        }))
        .expect("a mixed answer with both collections");
        assert_eq!(both.print_jobs().len(), 1);
        assert_eq!(both.email_jobs().len(), 2);

        let print_only = EmailJobSubmission::parse(json!({"printjobs": [{"id": 1}]}))
            .expect("a mixed answer with print jobs only");
        assert_eq!(
            (print_only.print_jobs().len(), print_only.email_jobs().len()),
            (1, 0),
            "a print-only envelope carries no e-mail job"
        );

        let email_only = EmailJobSubmission::parse(json!({"emailjobs": [{"id": 2}]}))
            .expect("a mixed answer with e-mail jobs only");
        assert_eq!(
            (email_only.print_jobs().len(), email_only.email_jobs().len()),
            (0, 1),
            "an e-mail-only envelope carries no print job"
        );

        let single = EmailJobSubmission::parse(json!({"id": 7}))
            .expect("a single job carries an id and no collection");
        assert_eq!(single.email_jobs().len(), 1);

        // An envelope that names a collection but carries no entry is a broken
        // answer, not an empty one.
        assert!(EmailJobSubmission::parse(json!({"printjobs": [], "emailjobs": []})).is_err());
        assert!(EmailJobSubmission::parse(json!({"emailjobs": []})).is_err());
        // An empty serial answer is legitimate: the letter had no white-codes.
        let empty = EmailJobSubmission::parse(json!([])).expect("an empty array");
        assert_eq!(empty.email_jobs().len(), 0);

        assert!(EmailJobSubmission::parse(json!({})).is_err());
        assert!(EmailJobSubmission::parse(json!(42)).is_err());
        assert!(EmailJobSubmission::parse(json!("nope")).is_err());
        assert!(EmailJobSubmission::parse(json!([{"nope": true}])).is_err());
    }

    #[test]
    fn a_page_count_inside_the_documented_range_decodes_in_every_spelling() {
        // Inside the range: zero, one, and the documented ceiling itself.
        for (value, expected) in [
            (json!(0), 0),
            (json!(1), 1),
            (json!(10_000), 10_000),
            (json!(10_000u64), 10_000),
            (json!(10_000.0), 10_000),
            (json!("1"), 1),
            (json!(" 10000 "), 10_000),
            (json!("10000.0"), 10_000),
            (json!(null), 0),
        ] {
            assert_eq!(de_pages(value.clone()).unwrap(), expected, "{value}");
        }

        // … and an absent field is zero pages.
        let item: PrintItem = serde_json::from_str("{}").unwrap();
        assert_eq!(item.pages, 0);
    }

    #[test]
    fn a_page_count_outside_the_documented_range_is_refused() {
        // Just outside the ceiling, and every value that is not a page count at
        // all: none of them may be truncated into a smaller total.
        for value in [
            json!(10_001),
            json!(10_001u64),
            json!(10_001.0),
            json!(u32::MAX),
            json!(u64::MAX),
            json!(i64::MAX),
            json!(-1),
            json!(-1.0),
            json!("10001"),
            json!("18446744073709551615"),
            json!("-1"),
            json!("1.5"),
            json!(1.5),
            json!("x"),
            json!(""),
            json!(true),
            json!([]),
            json!({}),
        ] {
            assert!(
                de_pages(value.clone()).is_err(),
                "{value} is not a page count"
            );
        }
    }

    #[test]
    fn the_page_error_names_the_documented_maximum() {
        let error = de_pages::<serde_json::Value>(json!(10_001)).expect_err("refused");
        let text = error.to_string();
        assert!(text.contains("10001"), "{text}");
        assert!(text.contains("10000"), "{text}");

        let negative = de_pages::<serde_json::Value>(json!(-2)).expect_err("refused");
        assert!(negative.to_string().contains("negative"), "{negative}");
    }

    #[test]
    fn an_item_array_at_the_limit_decodes_and_one_more_is_refused() {
        let at_limit = json!({
            "id": 1,
            "items": vec![json!({"pages": 1}); MAX_JOB_ITEMS],
        });
        let job: PrintJob = serde_json::from_value(at_limit).unwrap();
        assert_eq!(job.items.len(), MAX_JOB_ITEMS);
        assert_eq!(job.pages(), u32::try_from(MAX_JOB_ITEMS).unwrap());

        let above = json!({
            "id": 1,
            "items": vec![json!({"pages": 1}); MAX_JOB_ITEMS + 1],
        });
        let error = serde_json::from_value::<PrintJob>(above).expect_err("refused");
        assert!(error.to_string().contains("at most 50000"), "{error}");

        // A shape that is not an array at all is refused by the same rule.
        let not_an_array = serde_json::from_value::<PrintJob>(json!({"id": 1, "items": 5}));
        assert!(not_an_array.is_err());
    }

    #[test]
    fn pages_never_wraps_for_a_hand_built_job() {
        let job = PrintJob {
            items: vec![
                PrintItem {
                    pages: u32::MAX,
                    ..PrintItem::default()
                },
                PrintItem {
                    pages: u32::MAX,
                    ..PrintItem::default()
                },
            ],
            ..PrintJob::default()
        };
        assert_eq!(job.pages(), u32::MAX, "saturated, not wrapped to 1");
    }

    // -----------------------------------------------------------------------
    // The price echo: `registered` from either place, and only from a
    // specification that exists.
    // -----------------------------------------------------------------------

    #[test]
    fn the_letter_level_registered_code_is_the_one_that_wins() {
        let both = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 1, "registered": "r1"},
            "registered": "r2",
        }))
        .unwrap();
        assert_eq!(both.registered, Some(Registered::Einschreiben));

        // The same in the other order, so the answer is the letter level and
        // not whichever branch happened to run last.
        let both = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 1, "registered": "r2"},
            "registered": "r1",
        }))
        .unwrap();
        assert_eq!(both.registered, Some(Registered::EinschreibenEinwurf));
    }

    #[test]
    fn a_silent_letter_level_registered_keeps_the_priced_one() {
        // The bug: every one of these used to overwrite the specification's
        // `r2` with `None`, so a registered letter was echoed as unregistered.
        for silent in [json!(null), json!(""), json!("   "), json!("\t\n")] {
            let echo = PriceEcho::from_envelope(&json!({
                "specification": {"pages": 2, "registered": "r2"},
                "registered": silent.clone(),
            }))
            .unwrap();
            assert_eq!(
                echo.registered,
                Some(Registered::Einschreiben),
                "{silent} is not a code"
            );
        }

        // An absent key is the same state as a null one.
        let echo = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 2, "registered": "r2"},
        }))
        .unwrap();
        assert_eq!(echo.registered, Some(Registered::Einschreiben));

        // … and a letter level that is silent about a *silent* specification
        // stays `None` instead of inventing a code.
        let echo = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 2},
            "registered": null,
        }))
        .unwrap();
        assert_eq!(echo.registered, None);
    }

    #[test]
    fn a_non_string_registered_echo_is_carried_instead_of_dropped() {
        // Only the string form used to be read; a number was thrown away.
        let echo = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 1},
            "registered": 2,
        }))
        .unwrap();
        assert_eq!(echo.registered, Some(Registered::Other("2".into())));

        let echo = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 1},
            "registered": "R2",
        }))
        .unwrap();
        assert_eq!(echo.registered, Some(Registered::Einschreiben));

        // An unknown string is normalized like any other wire code.
        let echo = PriceEcho::from_envelope(&json!({
            "specification": {"pages": 1},
            "registered": " R7 ",
        }))
        .unwrap();
        assert_eq!(echo.registered, Some(Registered::Other("r7".into())));
    }

    #[test]
    fn a_registered_token_that_is_no_code_is_refused() {
        // Just outside the rule: a nested object or an array is not a code, and
        // must not be read as "not registered".
        for broken in [json!({"code": "r2"}), json!(["r2"]), json!([1, 2])] {
            let result = PriceEcho::from_envelope(&json!({
                "specification": {"pages": 1, "registered": "r2"},
                "registered": broken.clone(),
            }));
            assert!(result.is_err(), "{broken} is not a code");
        }
    }

    #[test]
    fn a_price_echo_needs_a_specification() {
        // Missing, null and a non-object specification have nothing to echo.
        for letter in [
            json!({}),
            json!({"registered": "r1"}),
            json!({"specification": null}),
            json!({"specification": 5}),
            json!({"specification": "nope"}),
        ] {
            let result = PriceEcho::from_envelope(&letter);
            assert!(result.is_err(), "{letter} has no specification");
        }

        // The other side of the boundary: an empty object *is* a specification,
        // and every field of the echo is then honestly "nothing".
        let empty = PriceEcho::from_envelope(&json!({"specification": {}})).unwrap();
        assert_eq!(empty, PriceEcho::default());

        // A non-object `letter` can never carry one either.
        assert!(PriceEcho::from_envelope(&json!("specification")).is_err());
        assert!(PriceEcho::from_envelope(&json!([{"specification": {}}])).is_err());
    }

    #[test]
    fn the_rest_of_the_echo_is_read_from_the_specification() {
        let echo = PriceEcho::from_envelope(&json!({
            "specification": {
                "pages": 3,
                "color": "1",
                "mode": "duplex",
                "shipping": "international",
                "email_option": "maildirect",
                "c4": true,
            },
            "registered": "r1",
        }))
        .unwrap();
        assert_eq!(echo.pages, 3);
        assert_eq!(echo.color, Some(Color::BlackAndWhite));
        assert_eq!(echo.mode, Some(PrintMode::Duplex));
        assert_eq!(echo.shipping, Some(Shipping::International));
        assert_eq!(echo.email_option, Some(EmailOption::MailDirect));
        assert!(echo.c4);
        assert_eq!(echo.registered, Some(Registered::EinschreibenEinwurf));
    }

    #[test]
    fn a_price_without_a_letter_has_no_echo_and_a_price_with_a_hole_is_refused() {
        let bare: Price = serde_json::from_str(r#"{"price": 1.25}"#).unwrap();
        assert_eq!(bare.price.cents(), 125);
        assert_eq!(bare.echo, None);

        for ignored in [json!(null), json!("letter"), json!([1])] {
            let price: Price = serde_json::from_value(json!({"price": 1.0, "letter": ignored}))
                .expect("a `letter` that is not an object is not an echo");
            assert_eq!(price.echo, None);
        }

        assert!(
            serde_json::from_value::<Price>(json!({"price": 1.0, "letter": {}})).is_err(),
            "a letter object without a specification is not an echo"
        );

        let echoed: Price =
            serde_json::from_value(json!({"price": 1.0, "letter": {"specification": {}}})).unwrap();
        assert_eq!(echoed.echo, Some(PriceEcho::default()));
    }
}
