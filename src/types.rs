//! Wire vocabularies: the closed value sets the provider documents, plus a
//! carry-all for everything else.
//!
//! Decoding is deliberately permissive: an unknown value is carried in
//! `Other(String)` after trimming and ASCII-lowercasing instead of failing the
//! whole response, so a new provider status cannot take a client offline.
//! Encoding is strict for request-side values. [`Color`], [`PrintMode`],
//! [`Shipping`], [`Registered`] and [`EmailOption`] keep `Other` so responses
//! can be decoded permissively, but serde serialization and the explicit
//! `to_request` conversion reject it. [`JobFilter`], [`TransactionFilter`] and
//! [`JournalType`] have no `Other` at all, because guessing a filter value
//! would silently return the wrong result set.
//!
//! `strum` derives the display and string conversions from the same wire
//! spellings that [`from_wire`](PrintMode::from_wire) matches. Their `FromStr`
//! implementations stay hand-written deliberately: the public contract trims
//! whitespace and returns this crate's typed [`Error`], not `strum::ParseError`.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;

/// Generates a string-valued wire enum with a carry-all variant.
macro_rules! wire_enum {
    (
        request = $request_field:literal,
        strict = $strict:literal,
        $(#[$meta:meta])*
        $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $wire:literal $(| $alias:literal)*
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            Hash,
            strum::AsRefStr,
            strum::Display,
        )]
        #[non_exhaustive]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                #[strum(to_string = $wire, serialize = $wire $(, serialize = $alias)*)]
                $variant,
            )+
            /// A value this crate does not model yet, with surrounding
            /// whitespace trimmed and ASCII letters lowercased.
            ///
            /// Non-ASCII characters are preserved as sent.
            #[strum(transparent)]
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("let value = <", stringify!($name), ">::from_wire(\"  Something-New  \");")]
            /// assert_eq!(value.as_str(), "something-new");
            /// ```
            Other(String),
        }

        impl $name {
            /// The documented wire spelling, or the normalized carried value for
            /// [`Self::Other`].
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert_eq!(<", stringify!($name), ">::from_wire(\"\").as_str(), \"\");")]
            /// ```
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.as_ref()
            }

            /// Parses a wire value, matching case-insensitively after trimming
            /// whitespace. Unknown values become [`Self::Other`] with the
            /// surrounding whitespace removed and ASCII letters lowercased;
            /// non-ASCII characters are preserved.
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("let parsed = <", stringify!($name), ">::from_wire(\"  Unknown-Value  \");")]
            #[doc = concat!("assert_eq!(parsed.as_str(), \"unknown-value\");")]
            #[doc = concat!("let unicode = <", stringify!($name), ">::from_wire(\"  ÄRGER  \");")]
            #[doc = concat!("assert_eq!(unicode.as_str(), \"Ärger\");")]
            /// ```
            #[must_use]
            pub fn from_wire(raw: &str) -> Self {
                let normalized = raw.trim();
                $(
                    $(if normalized.eq_ignore_ascii_case($alias) {
                        return Self::$variant;
                    })*
                    if normalized.eq_ignore_ascii_case($wire) {
                        return Self::$variant;
                    }
                )+
                Self::Other(normalized.to_ascii_lowercase())
            }

            /// Converts this value to the documented request spelling.
            ///
            /// Only modelled variants are valid in an outbound request.
            /// [`Self::Other`] remains available for permissive response
            /// decoding, but is rejected here instead of being copied onto the
            /// wire.
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert!(<", stringify!($name), ">::from_wire(\"unknown\").to_request(\"request\").is_err());")]
            /// ```
            ///
            /// # Errors
            ///
            /// [`Error::Validation`] when this is [`Self::Other`].
            pub fn to_request(&self, field: &'static str) -> Result<&'static str, Error> {
                match self {
                    $(Self::$variant => Ok($wire),)+
                    Self::Other(raw) => Err(Error::invalid(
                        field,
                        format!(
                            concat!("unknown ", stringify!($name), " request value: {:?}"),
                            raw
                        ),
                    )),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let value = if $strict {
                    self.to_request($request_field).map_err(serde::ser::Error::custom)?
                } else {
                    self.as_str()
                };
                serializer.serialize_str(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = deserializer.deserialize_any(WireVisitor)?;
                Ok(Self::from_wire(&raw))
            }
        }
    };
}

/// Visitor shared by every wire enum: accepts strings, numbers, booleans and
/// null without ever failing.
struct WireVisitor;

impl Visitor<'_> for WireVisitor {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string-valued enum member")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(value.to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(value)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(value.to_string())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(String::new())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(String::new())
    }
}

wire_enum! {
    request = "color",
    strict = true,
    /// Print colour, sent as `specification.color`.
    ///
    /// The wire values are the strings `"1"` and `"4"`, not colour names —
    /// that is what the provider documents.
    ///
    /// ```
    /// use letterxpress::Color;
    ///
    /// assert_eq!(Color::BlackAndWhite.as_str(), "1");
    /// assert_eq!(Color::Color.as_str(), "4");
    /// assert_eq!(Color::from_wire("4"), Color::Color);
    /// assert_eq!(Color::from_wire("9"), Color::Other("9".into()));
    /// ```
    Color {
        /// Black and white (`"1"`).
        BlackAndWhite = "1" | "bw" | "black_and_white",
        /// Full colour (`"4"`).
        Color = "4" | "colour" | "full_color",
    }
}

wire_enum! {
    request = "mode",
    strict = true,
    /// Print sides, sent as `specification.mode`.
    ///
    /// ```
    /// use letterxpress::PrintMode;
    ///
    /// assert_eq!(PrintMode::Simplex.as_str(), "simplex");
    /// assert_eq!(PrintMode::from_wire("DUPLEX"), PrintMode::Duplex);
    /// ```
    PrintMode {
        /// One side per sheet.
        Simplex = "simplex",
        /// Both sides per sheet.
        Duplex = "duplex",
    }
}

wire_enum! {
    request = "shipping",
    strict = true,
    /// Shipping class, sent as `specification.shipping`.
    ///
    /// `Auto` leaves the decision to the provider and is the safest default
    /// when the destination is not known to be German.
    ///
    /// ```
    /// use letterxpress::Shipping;
    ///
    /// assert_eq!(Shipping::from_wire("auto"), Shipping::Auto);
    /// assert_eq!(Shipping::International.as_str(), "international");
    /// ```
    Shipping {
        /// Domestic delivery.
        National = "national",
        /// Cross-border delivery.
        International = "international",
        /// Provider decides.
        Auto = "auto",
    }
}

wire_enum! {
    request = "registered",
    strict = true,
    /// Registered-mail option, sent as `registered`.
    ///
    /// Registered mail is **national only**; the builder rejects it for
    /// international shipping instead of silently dropping it.
    ///
    /// ```
    /// use letterxpress::Registered;
    ///
    /// assert_eq!(Registered::EinschreibenEinwurf.as_str(), "r1");
    /// assert_eq!(Registered::Einschreiben.as_str(), "r2");
    /// ```
    Registered {
        /// `r1` — Einschreiben Einwurf (proof of posting into the mailbox).
        EinschreibenEinwurf = "r1",
        /// `r2` — Einschreiben (signed delivery).
        Einschreiben = "r2",
    }
}

wire_enum! {
    request = "email_option",
    strict = true,
    /// SMART@MAIL delivery option, sent as `email_letter.email_option`.
    ///
    /// ```
    /// use letterxpress::EmailOption;
    ///
    /// assert_eq!(EmailOption::MailPlus.as_str(), "mailplus");
    /// assert_eq!(EmailOption::from_wire("MAILDIRECT"), EmailOption::MailDirect);
    /// ```
    EmailOption {
        /// Plain e-mail delivery.
        MailDirect = "maildirect",
        /// E-mail plus a printed copy.
        MailPlus = "mailplus",
        /// Encrypted delivery.
        MailSecure = "mailsecure",
    }
}

wire_enum! {
    request = "status",
    strict = false,
    /// Job state, as reported on print jobs, print items and e-mail jobs.
    ///
    /// The provider documents no state machine, only this vocabulary (from the
    /// `filter` table and the examples). Both spellings of "canceled" are
    /// accepted; anything unknown is carried through after trimming and
    /// ASCII-lowercasing.
    ///
    /// ```
    /// use letterxpress::JobStatus;
    ///
    /// assert_eq!(JobStatus::from_wire("done"), JobStatus::Done);
    /// assert_eq!(JobStatus::from_wire("cancelled"), JobStatus::Canceled);
    /// assert_eq!(JobStatus::from_wire(" Reticulating "), JobStatus::Other("reticulating".into()));
    /// assert!(JobStatus::Canceled.is_terminal());
    /// assert!(!JobStatus::Queue.is_terminal());
    /// ```
    JobStatus {
        /// Waiting in the production queue.
        Queue = "queue",
        /// Paused by the customer or the provider.
        Hold = "hold",
        /// Produced / processed.
        Done = "done",
        /// Canceled.
        Canceled = "canceled" | "cancelled",
        /// Sitting in the Postbox (test mode).
        Draft = "draft",
        /// An item handed to the carrier.
        Sent = "sent",
        /// An e-mail job delivered successfully.
        Success = "success",
    }
}

impl Default for JobStatus {
    /// A status that was never sent. `Other("")` is deliberately not terminal,
    /// so a missing field never looks like a finished job.
    ///
    /// ```
    /// assert_eq!(letterxpress::JobStatus::default(), letterxpress::JobStatus::Other(String::new()));
    /// ```
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl JobStatus {
    /// Whether no further state change is expected.
    ///
    /// `Other` values are **not** assumed terminal.
    ///
    /// ```
    /// use letterxpress::JobStatus;
    ///
    /// assert!(JobStatus::Done.is_terminal());
    /// assert!(JobStatus::Success.is_terminal());
    /// assert!(!JobStatus::Other("unknown".into()).is_terminal());
    /// ```
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Canceled | Self::Success)
    }

    /// Whether the job still sits in the Postbox and will be auto-deleted
    /// after seven days unless it is released.
    ///
    /// ```
    /// use letterxpress::JobStatus;
    ///
    /// assert!(JobStatus::Draft.is_postbox());
    /// assert!(!JobStatus::Queue.is_postbox());
    /// ```
    #[must_use]
    pub fn is_postbox(&self) -> bool {
        matches!(self, Self::Draft)
    }
}

/// The documented `filter` values for `GET /v3/printjobs` and
/// `GET /v3/emailjobs`.
///
/// Closed on purpose: an unknown filter would make the provider return an
/// unfiltered (or empty) page, which is worse than a local error.
///
/// ```
/// use letterxpress::JobFilter;
///
/// assert_eq!(JobFilter::Draft.as_str(), "draft");
/// assert_eq!("hold".parse::<JobFilter>().unwrap(), JobFilter::Hold);
/// assert!("everything".parse::<JobFilter>().is_err());
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumIter, strum::IntoStaticStr,
)]
#[non_exhaustive]
pub enum JobFilter {
    /// Orders in the queue.
    #[strum(serialize = "queue")]
    Queue,
    /// Paused orders.
    #[strum(serialize = "hold")]
    Hold,
    /// Processed orders.
    #[strum(serialize = "done")]
    Done,
    /// Canceled orders.
    #[strum(serialize = "canceled")]
    Canceled,
    /// Orders in the postbox.
    #[strum(serialize = "draft")]
    Draft,
}

impl JobFilter {
    /// The documented wire spelling.
    ///
    /// ```
    /// assert_eq!(letterxpress::JobFilter::Canceled.as_str(), "canceled");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// Whether this filter selects jobs that sit in the Postbox (test mode).
    ///
    /// ```
    /// assert!(letterxpress::JobFilter::Draft.is_postbox());
    /// assert!(!letterxpress::JobFilter::Queue.is_postbox());
    /// ```
    #[must_use]
    pub fn is_postbox(self) -> bool {
        matches!(self, Self::Draft)
    }

    /// Every documented filter value.
    ///
    /// ```
    /// assert_eq!(letterxpress::JobFilter::ALL.len(), 5);
    /// ```
    pub const ALL: [Self; 5] = [
        Self::Queue,
        Self::Hold,
        Self::Done,
        Self::Canceled,
        Self::Draft,
    ];
}

// Kept custom: `strum::EnumString` would change trimming, error type, and the
// public parsing contract.
impl FromStr for JobFilter {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(value.trim()))
            .ok_or_else(|| Error::invalid("filter", format!("unknown job filter: {value:?}")))
    }
}

/// The documented `filter` values for `GET /v3/transactions`.
///
/// ```
/// use letterxpress::TransactionFilter;
///
/// assert_eq!(TransactionFilter::PrintJobs.as_str(), "printjobs");
/// assert!("refunds".parse::<TransactionFilter>().is_err());
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumIter, strum::IntoStaticStr,
)]
#[non_exhaustive]
pub enum TransactionFilter {
    /// Deposits, vouchers, kickbacks.
    #[strum(serialize = "payins")]
    PayIns,
    /// Payouts.
    #[strum(serialize = "payouts")]
    PayOuts,
    /// Job charges.
    #[strum(serialize = "printjobs")]
    PrintJobs,
}

impl TransactionFilter {
    /// The documented wire spelling.
    ///
    /// ```
    /// assert_eq!(letterxpress::TransactionFilter::PayOuts.as_str(), "payouts");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// Every documented filter value.
    ///
    /// ```
    /// assert_eq!(letterxpress::TransactionFilter::ALL.len(), 3);
    /// ```
    pub const ALL: [Self; 3] = [Self::PayIns, Self::PayOuts, Self::PrintJobs];
}

// Kept custom: `strum::EnumString` would change trimming, error type, and the
// public parsing contract.
impl FromStr for TransactionFilter {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(value.trim()))
            .ok_or_else(|| {
                Error::invalid("filter", format!("unknown transaction filter: {value:?}"))
            })
    }
}

/// The documented `filter.type` values for `GET /v3/journal`.
///
/// ```
/// use letterxpress::JournalType;
///
/// assert_eq!(JournalType::Print.as_str(), "print");
/// assert_eq!("mailplus".parse::<JournalType>().unwrap(), JournalType::MailPlus);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumIter, strum::IntoStaticStr,
)]
#[non_exhaustive]
pub enum JournalType {
    /// Printed letters.
    #[strum(serialize = "print")]
    Print,
    /// `maildirect` e-mail jobs.
    #[strum(serialize = "maildirect")]
    MailDirect,
    /// `mailsecure` e-mail jobs.
    #[strum(serialize = "mailsecure")]
    MailSecure,
    /// `mailplus` e-mail jobs.
    #[strum(serialize = "mailplus")]
    MailPlus,
}

impl JournalType {
    /// The documented wire spelling.
    ///
    /// ```
    /// assert_eq!(letterxpress::JournalType::MailSecure.as_str(), "mailsecure");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// Every documented journal type.
    ///
    /// ```
    /// assert_eq!(letterxpress::JournalType::ALL.len(), 4);
    /// ```
    pub const ALL: [Self; 4] = [
        Self::Print,
        Self::MailDirect,
        Self::MailSecure,
        Self::MailPlus,
    ];
}

// Kept custom: `strum::EnumString` would change trimming, error type, and the
// public parsing contract.
impl FromStr for JournalType {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str().eq_ignore_ascii_case(value.trim()))
            .ok_or_else(|| {
                Error::invalid("filter.type", format!("unknown journal type: {value:?}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_visitor_accepts_every_json_scalar() {
        assert_eq!(
            WireVisitor.visit_str::<serde_json::Error>("1").unwrap(),
            "1"
        );
        assert_eq!(
            WireVisitor
                .visit_string::<serde_json::Error>("x".to_string())
                .unwrap(),
            "x"
        );
        assert_eq!(
            WireVisitor.visit_i64::<serde_json::Error>(-1).unwrap(),
            "-1"
        );
        assert_eq!(WireVisitor.visit_u64::<serde_json::Error>(4).unwrap(), "4");
        assert_eq!(
            WireVisitor.visit_f64::<serde_json::Error>(1.5).unwrap(),
            "1.5"
        );
        assert_eq!(
            WireVisitor.visit_bool::<serde_json::Error>(true).unwrap(),
            "true"
        );
        assert_eq!(WireVisitor.visit_unit::<serde_json::Error>().unwrap(), "");
        assert_eq!(WireVisitor.visit_none::<serde_json::Error>().unwrap(), "");
    }

    #[test]
    fn numeric_and_null_values_decode_into_carry_alls() {
        assert_eq!(
            serde_json::from_str::<PrintMode>("-1").unwrap(),
            PrintMode::Other("-1".into())
        );
        assert_eq!(
            serde_json::from_str::<Color>("1.5").unwrap(),
            Color::Other("1.5".into())
        );
    }
}
