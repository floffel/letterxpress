//! Response models: jobs, e-mail jobs, pages, balances, invoices, journals.
//!
//! Every struct decodes permissively: unknown keys are ignored, every documented
//! scalar is accepted as a number *or* a string, both `snake_case` and
//! `camelCase` key spellings are accepted, missing optional fields default, and
//! unknown enum values survive as `Other(String)`. Nothing in here can fail on
//! a provider that adds a field.
//!
//! ```
//! use letterxpress::PrintJob;
//!
//! let job: PrintJob = serde_json::from_str(r#"{
//!     "id": 6035138,
//!     "status": "done",
//!     "items": [{"address": "Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg", "pages": 1, "amount": 0.67, "vat": 0.13, "status": "sent"}],
//!     "some_future_field": {"nested": true}
//! }"#)?;
//! assert_eq!(job.id.as_str(), "6035138");
//! assert_eq!(job.items[0].total().cents(), 80);
//! # Ok::<(), serde_json::Error>(())
//! ```

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{Color, EmailOption, Error, JobStatus, Money, PrintMode, Registered, Shipping};

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
                let raw = deserializer.deserialize_any(IdVisitor)?;
                Self::new(raw).map_err(|error| de::Error::custom(error.to_string()))
            }
        }
    };
}

/// Accepts an id as a string or an integer.
struct IdVisitor;

impl Visitor<'_> for IdVisitor {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an id as a number or a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(value.trim().to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(value.trim().to_owned())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(value.to_string())
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

/// `0`/`1`, `true`/`false`, `"0"`/`"1"` and null all decode.
fn de_flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    struct FlagVisitor;

    impl Visitor<'_> for FlagVisitor {
        type Value = bool;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("0/1, true/false or null")
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value != 0)
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let value = value.trim();
            Ok(matches!(value, "1" | "true" | "TRUE" | "True" | "yes"))
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(false)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(false)
        }
    }

    deserializer.deserialize_any(FlagVisitor)
}

/// A scalar that may arrive as a string, a number or null.
fn de_opt_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    struct TextVisitor;

    impl Visitor<'_> for TextVisitor {
        type Value = Option<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string, a number or null")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(Some(value.to_owned()))
        }

        fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
            Ok(Some(value))
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
            Ok(Some(value.to_string()))
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(TextVisitor)
}

/// Status used when a response omits the field entirely.
fn unspecified_status() -> JobStatus {
    JobStatus::Other(String::new())
}

/// One address block inside a print job (`items[]`).
///
/// `amount` is **net**, `vat` the tax share; the amount actually charged is
/// [`PrintItem::total`] — the documentation's own ledger confirms it.
///
/// ```
/// use letterxpress::PrintItem;
///
/// let item: PrintItem = serde_json::from_str(
///     r#"{"address":"Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg","pages":1,"amount":0.68,"vat":0.13,"status":"sent"}"#,
/// )?;
/// assert_eq!(item.net.cents(), 68);
/// assert_eq!(item.vat.cents(), 13);
/// assert_eq!(item.total().cents(), 81);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PrintItem {
    /// The recipient address as printed in the PDF, one line.
    #[serde(default, deserialize_with = "de_opt_string", alias = "Address")]
    pub address: Option<String>,
    /// Pages in this item.
    #[serde(default)]
    pub pages: u32,
    /// Net amount (wire key `amount`).
    #[serde(default, rename = "amount")]
    pub net: Money,
    /// Tax share.
    #[serde(default)]
    pub vat: Money,
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
    /// Net amount.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintItem::default().net.cents(), 0);
    /// ```
    #[must_use]
    pub fn net(&self) -> Money {
        self.net
    }

    /// Tax share.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintItem::default().vat.cents(), 0);
    /// ```
    #[must_use]
    pub fn vat(&self) -> Money {
        self.vat
    }

    /// Net plus tax — what the provider actually charges.
    ///
    /// ```
    /// use letterxpress::{Money, PrintItem};
    ///
    /// let item = PrintItem { net: Money::from_euros(0.67), vat: Money::from_euros(0.13), ..PrintItem::default() };
    /// assert_eq!(item.total().cents(), 80);
    /// ```
    #[must_use]
    pub fn total(&self) -> Money {
        self.net.total(self.vat)
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
    #[serde(default, deserialize_with = "de_flag")]
    pub c4: bool,
    /// Registered-mail option.
    #[serde(default)]
    pub registered: Option<Registered>,
    /// Whether a bank form was requested.
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
    /// One entry per letter in the job.
    #[serde(default)]
    pub items: Vec<PrintItem>,
}

impl PrintJob {
    /// Total pages across all items.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintJob::default().pages(), 0);
    /// ```
    #[must_use]
    pub fn pages(&self) -> u32 {
        self.items.iter().map(|item| item.pages).sum()
    }

    /// Net sum across items.
    ///
    /// ```
    /// assert!(letterxpress::PrintJob::default().net().is_zero());
    /// ```
    #[must_use]
    pub fn net(&self) -> Money {
        self.items
            .iter()
            .fold(Money::ZERO, |sum, item| sum + item.net)
    }

    /// Gross sum across items — the amount billed for this job.
    ///
    /// ```
    /// assert!(letterxpress::PrintJob::default().total().is_zero());
    /// ```
    #[must_use]
    pub fn total(&self) -> Money {
        self.items
            .iter()
            .fold(Money::ZERO, |sum, item| sum + item.total())
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
/// use letterxpress::EmailJob;
///
/// let job: EmailJob = serde_json::from_str(
///     r#"{"id":3813,"email_receiver":"userone@letterxpress.de","email_option":"maildirect",
///         "amount":0.05,"vat":0.01,"status":"queue","subject":"Monats Rechnung"}"#,
/// )?;
/// assert_eq!(job.status, letterxpress::JobStatus::Queue);
/// assert_eq!(job.total().cents(), 6);
/// # Ok::<(), serde_json::Error>(())
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
    /// Net amount (wire key `amount`).
    #[serde(default, rename = "amount")]
    pub net: Money,
    /// Tax share.
    #[serde(default)]
    pub vat: Money,
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
    /// Net amount.
    ///
    /// ```
    /// assert!(letterxpress::EmailJob::default().net().is_zero());
    /// ```
    #[must_use]
    pub fn net(&self) -> Money {
        self.net
    }

    /// Tax share.
    ///
    /// ```
    /// assert!(letterxpress::EmailJob::default().vat().is_zero());
    /// ```
    #[must_use]
    pub fn vat(&self) -> Money {
        self.vat
    }

    /// Net plus tax.
    ///
    /// ```
    /// assert!(letterxpress::EmailJob::default().total().is_zero());
    /// ```
    #[must_use]
    pub fn total(&self) -> Money {
        self.net.total(self.vat)
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
            net: Money::ZERO,
            vat: Money::ZERO,
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

impl EmailJobSubmission {
    /// Parses any of the three documented shapes.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the payload is neither shape.
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
        match value {
            Value::Array(entries) => {
                let jobs = entries
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<Vec<EmailJob>, _>>()?;
                Ok(Self::Serial(jobs))
            }
            Value::Object(map) => {
                if map.contains_key("emailjobs") || map.contains_key("printjobs") {
                    let print_jobs = map
                        .get("printjobs")
                        .cloned()
                        .map(serde_json::from_value::<Vec<PrintJob>>)
                        .transpose()?
                        .unwrap_or_default();
                    let email_jobs = map
                        .get("emailjobs")
                        .cloned()
                        .map(serde_json::from_value::<Vec<EmailJob>>)
                        .transpose()?
                        .unwrap_or_default();
                    if print_jobs.is_empty() && email_jobs.is_empty() {
                        return Err(de::Error::custom(
                            "email job response has neither printjobs nor emailjobs",
                        ));
                    }
                    Ok(Self::Mixed {
                        print_jobs,
                        email_jobs,
                    })
                } else if map.contains_key("id") {
                    Ok(Self::Single(serde_json::from_value(Value::Object(map))?))
                } else {
                    Err(de::Error::custom(
                        "email job response is neither a job, an array, nor a mixed envelope",
                    ))
                }
            }
            _ => Err(de::Error::custom(
                "email job response must be an object or an array",
            )),
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Pagination {
    /// Total entries across all pages.
    #[serde(default)]
    pub total: u32,
    /// Entries on this page.
    #[serde(default)]
    pub count: u32,
    /// Current page, 1-based.
    #[serde(default)]
    pub current_page: u32,
    /// Last page.
    #[serde(default)]
    pub last_page: u32,
    /// Page size (15 in every documented example).
    #[serde(default)]
    pub per_page: u32,
    /// First-page URL.
    #[serde(default, deserialize_with = "de_opt_string", alias = "firstPageUrl")]
    pub first_page_url: Option<String>,
    /// Last-page URL.
    #[serde(default, deserialize_with = "de_opt_string", alias = "lastPageUrl")]
    pub last_page_url: Option<String>,
    /// Previous-page URL.
    #[serde(default, deserialize_with = "de_opt_string", alias = "prevPageUrl")]
    pub prev_page_url: Option<String>,
    /// Next-page URL.
    #[serde(default, deserialize_with = "de_opt_string", alias = "nextPageUrl")]
    pub next_page_url: Option<String>,
}

impl Pagination {
    /// Whether another page exists.
    ///
    /// ```
    /// assert!(!letterxpress::Pagination::default().has_next());
    /// ```
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.current_page < self.last_page
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
/// Negative amounts are charges, positive amounts are top-ups.
///
/// ```
/// use letterxpress::Transaction;
///
/// let entry: Transaction = serde_json::from_str(
///     r#"{"amount":-0.81,"currency":"EUR","description":"Schwarzweiß, Einseitig, National, 1 Seite","created_at":"2022-09-21 11:45:28"}"#,
/// )?;
/// assert_eq!(entry.amount.cents(), -81);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Transaction {
    /// Signed amount: negative is a charge.
    #[serde(default)]
    pub amount: Money,
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
/// use letterxpress::Invoice;
///
/// let invoice: Invoice = serde_json::from_str(r#"{"id":25402,"amount":0.67,"vat":0.13,"invoice_date":"2020-05-31"}"#)?;
/// assert_eq!(invoice.total().cents(), 80);
/// assert_eq!(invoice.amount.cents(), 67);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Invoice {
    /// Invoice id.
    pub id: InvoiceId,
    /// Net amount.
    #[serde(default)]
    pub amount: Money,
    /// Tax share.
    #[serde(default)]
    pub vat: Money,
    /// Invoice date, `yyyy-mm-dd`.
    #[serde(default, deserialize_with = "de_opt_string", alias = "invoiceDate")]
    pub invoice_date: Option<String>,
}

impl Invoice {
    /// Net plus tax — the invoiced amount.
    ///
    /// ```
    /// use letterxpress::{Invoice, Money};
    ///
    /// let invoice = Invoice { amount: Money::from_euros(0.67), vat: Money::from_euros(0.13), ..Invoice::default() };
    /// assert_eq!(invoice.total().cents(), 80);
    /// ```
    #[must_use]
    pub fn total(&self) -> Money {
        self.amount.total(self.vat)
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
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct PriceEcho {
    /// Pages that were priced.
    #[serde(default)]
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
    /// # Errors
    ///
    /// [`Error`] when the payload cannot be decoded or the provider rejects the
    /// call.
    pub fn from_envelope(letter: &Value) -> Result<Self, serde_json::Error> {
        let specification = letter.get("specification").cloned().unwrap_or(Value::Null);
        let mut echo = if specification.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(specification)?
        };
        echo.registered = letter
            .get("registered")
            .and_then(Value::as_str)
            .map(Registered::from_wire);
        Ok(echo)
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
        let echo = match raw.letter {
            Some(value) if !value.is_null() => {
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

    #[test]
    fn flags_accept_every_documented_spelling() {
        assert!(de_flag(serde_json::json!(true)).unwrap());
        assert!(de_flag(serde_json::json!(1)).unwrap());
        assert!(de_flag(serde_json::json!(1u64)).unwrap());
        assert!(de_flag(serde_json::json!(2i64)).unwrap());
        assert!(!de_flag(serde_json::json!(0)).unwrap());
        assert!(de_flag(serde_json::json!("1")).unwrap());
        assert!(de_flag(serde_json::json!("true")).unwrap());
        assert!(!de_flag(serde_json::json!("0")).unwrap());
        assert!(!de_flag(serde_json::json!(null)).unwrap());
        assert!(!de_flag(serde_json::json!(false)).unwrap());
    }

    #[test]
    fn optional_text_accepts_every_scalar() {
        assert_eq!(
            de_opt_string(serde_json::json!("x")).unwrap(),
            Some("x".into())
        );
        assert_eq!(
            de_opt_string(serde_json::json!("x".to_string())).unwrap(),
            Some("x".into())
        );
        assert_eq!(
            de_opt_string(serde_json::json!(-5)).unwrap(),
            Some("-5".into())
        );
        assert_eq!(
            de_opt_string(serde_json::json!(5u64)).unwrap(),
            Some("5".into())
        );
        assert_eq!(
            de_opt_string(serde_json::json!(1.5)).unwrap(),
            Some("1.5".into())
        );
        assert_eq!(
            de_opt_string(serde_json::json!(true)).unwrap(),
            Some("true".into())
        );
        assert_eq!(de_opt_string(serde_json::json!(null)).unwrap(), None);
        assert_eq!(
            IdVisitor.visit_str::<serde_json::Error>(" 42 ").unwrap(),
            "42"
        );
        assert_eq!(
            IdVisitor
                .visit_string::<serde_json::Error>("7".to_string())
                .unwrap(),
            "7"
        );
        assert_eq!(IdVisitor.visit_i64::<serde_json::Error>(-1).unwrap(), "-1");
        assert_eq!(IdVisitor.visit_u64::<serde_json::Error>(8).unwrap(), "8");
    }
}
