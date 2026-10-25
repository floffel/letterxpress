//! Wire vocabularies: the closed value sets the provider documents, plus a
//! carry-all for everything else.
//!
//! Decoding is deliberately permissive: an unknown value is kept verbatim in
//! `Other(String)` instead of failing the whole response, so a new provider
//! status cannot take a client offline. Encoding is strict for request-side
//! enums ([`JobFilter`], [`TransactionFilter`], [`JournalType`]) — those have
//! no `Other`, because guessing a filter value would silently return the wrong
//! result set.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;

/// Generates a string-valued wire enum with a carry-all variant.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $wire:literal $(| $alias:literal)*
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                $variant,
            )+
            /// A value this crate does not model yet, kept verbatim.
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("let value = <", stringify!($name), ">::from_wire(\"something-new\");")]
            /// assert_eq!(value.as_str(), "something-new");
            /// ```
            Other(String),
        }

        impl $name {
            /// The documented wire spelling, or the raw value for
            /// [`Self::Other`].
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("assert_eq!(<", stringify!($name), ">::from_wire(\"\").as_str(), \"\");")]
            /// ```
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Other(raw) => raw,
                }
            }

            /// Parses a wire value, case-insensitively and trimming
            /// whitespace. Unknown values become [`Self::Other`].
            ///
            /// ```
            #[doc = concat!("# use letterxpress::", stringify!($name), ";")]
            #[doc = concat!("let parsed = <", stringify!($name), ">::from_wire(\"  unknown-value  \");")]
            #[doc = concat!("assert_eq!(parsed.as_str(), \"unknown-value\");")]
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
                Self::Other(normalized.to_string())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
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
    /// Job state, as reported on print jobs, print items and e-mail jobs.
    ///
    /// The provider documents no state machine, only this vocabulary (from the
    /// `filter` table and the examples). Both spellings of "canceled" are
    /// accepted; anything unknown is carried through.
    ///
    /// ```
    /// use letterxpress::JobStatus;
    ///
    /// assert_eq!(JobStatus::from_wire("done"), JobStatus::Done);
    /// assert_eq!(JobStatus::from_wire("cancelled"), JobStatus::Canceled);
    /// assert_eq!(JobStatus::from_wire("reticulating"), JobStatus::Other("reticulating".into()));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JobFilter {
    /// Orders in the queue.
    Queue,
    /// Paused orders.
    Hold,
    /// Processed orders.
    Done,
    /// Canceled orders.
    Canceled,
    /// Orders in the postbox.
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
        match self {
            Self::Queue => "queue",
            Self::Hold => "hold",
            Self::Done => "done",
            Self::Canceled => "canceled",
            Self::Draft => "draft",
        }
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

impl fmt::Display for JobFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TransactionFilter {
    /// Deposits, vouchers, kickbacks.
    PayIns,
    /// Payouts.
    PayOuts,
    /// Job charges.
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
        match self {
            Self::PayIns => "payins",
            Self::PayOuts => "payouts",
            Self::PrintJobs => "printjobs",
        }
    }

    /// Every documented filter value.
    ///
    /// ```
    /// assert_eq!(letterxpress::TransactionFilter::ALL.len(), 3);
    /// ```
    pub const ALL: [Self; 3] = [Self::PayIns, Self::PayOuts, Self::PrintJobs];
}

impl fmt::Display for TransactionFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JournalType {
    /// Printed letters.
    Print,
    /// `maildirect` e-mail jobs.
    MailDirect,
    /// `mailsecure` e-mail jobs.
    MailSecure,
    /// `mailplus` e-mail jobs.
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
        match self {
            Self::Print => "print",
            Self::MailDirect => "maildirect",
            Self::MailSecure => "mailsecure",
            Self::MailPlus => "mailplus",
        }
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

impl fmt::Display for JournalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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
