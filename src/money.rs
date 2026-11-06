//! Money as the API sends it: decimal euro amounts, stored exactly as cents.
//!
//! LetterXpress reports `amount`, `vat`, `price` and `balance` as JSON numbers
//! with two decimals (`0.67`, `4.99`, `1000`). This module normalises those
//! values to [`i64`] cents once, at the boundary, so accounting code never
//! accumulates binary floating-point error.
//!
//! Quoted amounts may use either `.` or `,` as the decimal mark. A separator
//! followed by one or two digits is a decimal mark; a separator followed by
//! exactly three digits is a grouping separator. This makes `4,50` equal to
//! `4.50`, `1,234` equal to `1234`, and `1.234,56` equal to `1234.56`. Both
//! separator styles may appear together only when one is consistently used for
//! groups of three and the other is the decimal mark. Mixed grouping styles,
//! reused separators, trailing or duplicate separators, exponent notation, and
//! a leading `+` are rejected rather than guessed. Negative amounts are valid:
//! transaction-ledger debits use them, and the sign belongs to the saved cents.
//!
//! ```
//! use letterxpress::Money;
//!
//! let net = Money::from_euros(0.68)?;
//! let vat = Money::from_euros(0.13)?;
//! assert_eq!(net.checked_add(vat)?.cents(), 81);
//! assert_eq!(net.to_string(), "0.68");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::fmt;
use std::ops::Add;
use std::str::FromStr;

use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A failure to construct or combine a [`Money`] amount.
///
/// ```
/// use letterxpress::{Money, MoneyError};
///
/// assert_eq!(Money::from_euros(f64::NAN), Err(MoneyError::NotFinite));
/// assert_eq!(
///     "1,234,56".parse::<Money>(),
///     Err(MoneyError::AmbiguousSeparators)
/// );
/// assert_eq!(
///     "12,34,567".parse::<Money>(),
///     Err(MoneyError::MalformedGrouping)
/// );
/// assert_eq!("1,2345".parse::<Money>(), Err(MoneyError::InvalidPrecision));
/// assert_eq!(
///     "4.".parse::<Money>(),
///     Err(MoneyError::InvalidFormat("trailing separator"))
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MoneyError {
    /// The euro value is NaN or infinite.
    #[error("money amount must be finite")]
    NotFinite,
    /// The amount cannot be represented as an [`i64`] cent value.
    #[error("money amount is out of range for i64 cents")]
    OutOfRange,
    /// The amount has a shape that is not a plain decimal.
    ///
    /// The static detail names the rejected shape, such as an exponent,
    /// trailing separator, or duplicate separator.
    #[error("invalid money format: {0}")]
    InvalidFormat(&'static str),
    /// Grouping and decimal-separator roles are mixed or assigned twice.
    #[error("money amount uses ambiguous or mixed separators")]
    AmbiguousSeparators,
    /// The digits between grouping separators are not groups of three.
    #[error("money grouping separators are not consistent groups of three")]
    MalformedGrouping,
    /// The fractional part is longer than two digits.
    #[error("money amount has more than two decimal places")]
    InvalidPrecision,
}

/// A signed monetary amount in euro cents.
///
/// ```
/// use letterxpress::Money;
///
/// assert_eq!(Money::from_cents(81).euros(), 0.81);
/// assert_eq!(Money::default().cents(), 0);
/// assert_eq!(Money::from_euros(2.5)?.cents(), 250);
/// assert_eq!(Money::from_cents(-81).cents(), -81);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Money {
    cents: i64,
}

impl Money {
    /// Zero euro.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::ZERO.cents(), 0);
    /// ```
    pub const ZERO: Self = Self { cents: 0 };

    /// Converts a euro value at the fallible boundary, rounding to cents.
    ///
    /// # Errors
    ///
    /// [`MoneyError::NotFinite`] for NaN or infinity and
    /// [`MoneyError::OutOfRange`] when rounding to [`i64`] cents overflows.
    /// Ties round away from zero; the sign is then stored exactly in cents.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// assert_eq!(Money::from_euros(4.999)?.cents(), 500);
    /// assert_eq!(Money::from_euros(-1.005)?.cents(), -101);
    /// assert_eq!(Money::from_euros(f64::INFINITY), Err(MoneyError::NotFinite));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_euros(euros: f64) -> Result<Self, MoneyError> {
        if !euros.is_finite() {
            return Err(MoneyError::NotFinite);
        }
        let euros = Decimal::from_f64(euros).ok_or(MoneyError::OutOfRange)?;
        let cents = euros.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
            * Decimal::ONE_HUNDRED;
        let cents = i64::try_from(cents).map_err(|_| MoneyError::OutOfRange)?;
        Ok(Self { cents })
    }

    /// Builds an amount from minor units.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// assert_eq!(Money::from_cents(68).euros(), 0.68);
    /// assert_eq!(Money::from_cents(i64::MAX).cents(), i64::MAX);
    /// assert_eq!(Money::from_cents(i64::MIN).cents(), i64::MIN);
    /// assert_eq!(Money::from_cents(-1).cents(), -1);
    /// ```
    #[must_use]
    pub const fn from_cents(cents: i64) -> Self {
        Self { cents }
    }

    /// The euro value, derived from the exact cent amount.
    ///
    /// The result is a floating-point convenience only; cent arithmetic should
    /// use [`Money::cents`].
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_cents(67).euros(), 0.67);
    /// ```
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn euros(self) -> f64 {
        self.cents as f64 / 100.0
    }

    /// The exact signed value in cents.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_cents(68).cents(), 68);
    /// ```
    #[must_use]
    pub const fn cents(self) -> i64 {
        self.cents
    }

    /// Checked addition.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] when the sum exceeds [`i64::MAX`] cents.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// let one = Money::from_cents(1);
    /// assert_eq!(Money::ZERO.checked_add(one)?.cents(), 1);
    /// assert_eq!(
    ///     Money::from_cents(i64::MAX).checked_add(one),
    ///     Err(MoneyError::OutOfRange)
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn checked_add(self, rhs: Self) -> Result<Self, MoneyError> {
        match self.cents.checked_add(rhs.cents) {
            Some(cents) => Ok(Self { cents }),
            None => Err(MoneyError::OutOfRange),
        }
    }

    /// Checked subtraction.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] when the signed difference overflows.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// let one = Money::from_cents(1);
    /// assert_eq!(one.checked_sub(one)?.cents(), 0);
    /// assert_eq!(Money::ZERO.checked_sub(one)?.cents(), -1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn checked_sub(self, rhs: Self) -> Result<Self, MoneyError> {
        match self.cents.checked_sub(rhs.cents) {
            Some(cents) => Ok(Self { cents }),
            None => Err(MoneyError::OutOfRange),
        }
    }

    /// Net plus tax.
    ///
    /// # Errors
    ///
    /// [`MoneyError::OutOfRange`] when the sum exceeds [`i64::MAX`] cents.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// let gross = Money::from_cents(68).checked_add(Money::from_cents(13))?;
    /// assert_eq!(gross.cents(), 81);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn total(self, vat: Self) -> Result<Self, MoneyError> {
        self.checked_add(vat)
    }

    /// Whether the amount is zero.
    ///
    /// ```
    /// assert!(letterxpress::Money::ZERO.is_zero());
    /// ```
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.cents == 0
    }

    /// Parses a signed euro amount using the separator rules documented on
    /// [`Money`]. Leading and trailing whitespace is ignored. A leading `-`
    /// gives a negative amount; a leading `+`, exponent notation, underscores,
    /// and non-ASCII digits are rejected. Every rejection is terminal: the
    /// same text always produces the same error, so retrying it is pointless.
    ///
    /// # Errors
    ///
    /// [`MoneyError::InvalidFormat`] for a non-plain-decimal shape,
    /// [`MoneyError::InvalidPrecision`] for more than two decimal places,
    /// [`MoneyError::AmbiguousSeparators`] when separator roles are mixed,
    /// [`MoneyError::MalformedGrouping`] when groups are not 1-3 digits
    /// followed by groups of exactly three, and [`MoneyError::OutOfRange`] when
    /// the amount does not fit in [`i64`] cents.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// let us: Money = "1,234.56".parse()?;
    /// let german: Money = "1.234,56".parse()?;
    /// assert_eq!(us.cents(), 123_456);
    /// assert_eq!(german.cents(), 123_456);
    /// assert_eq!("4,50".parse::<Money>()?.cents(), 450);
    /// assert_eq!("1,234".parse::<Money>()?.cents(), 123_400);
    /// assert_eq!("-1,234".parse::<Money>()?.cents(), -123_400);
    /// assert_eq!(
    ///     "1,234,56".parse::<Money>(),
    ///     Err(MoneyError::AmbiguousSeparators)
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn parse(text: &str) -> Result<Self, MoneyError> {
        let normalized = normalize_amount(text)?;
        let amount = Decimal::from_str(&normalized).map_err(|_| MoneyError::OutOfRange)?;

        let cents = amount * Decimal::ONE_HUNDRED;
        let cents = cents
            .to_i128()
            .and_then(|cents| i64::try_from(cents).ok())
            .ok_or(MoneyError::OutOfRange)?;
        Ok(Self::from_cents(cents))
    }
}

impl FromStr for Money {
    type Err = MoneyError;

    /// Parses an amount with [`Money::parse`].
    ///
    /// ```
    /// let amount: letterxpress::Money = "1.234,56".parse()?;
    /// assert_eq!(amount.cents(), 123_456);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Add for Money {
    type Output = Result<Self, MoneyError>;

    /// Checked cents addition; the error is [`MoneyError::OutOfRange`].
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// let sum = (Money::from_cents(10) + Money::from_cents(20))?;
    /// assert_eq!(sum.cents(), 30);
    /// # Ok::<(), letterxpress::MoneyError>(())
    /// ```
    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add(rhs)
    }
}

/// The longest text that can still describe an in-range amount after grouping
/// separators and a two-digit fraction. Rejecting longer input before building
/// the normalized string keeps hostile payloads from driving allocations.
const MAX_AMOUNT_TEXT_BYTES: usize = 64;

fn normalize_amount(value: &str) -> Result<String, MoneyError> {
    let text = value.trim();
    if text.is_empty() {
        return Err(MoneyError::InvalidFormat("empty amount"));
    }
    if text.len() > MAX_AMOUNT_TEXT_BYTES {
        return Err(MoneyError::OutOfRange);
    }

    let (negative, unsigned) = if let Some(unsigned) = text.strip_prefix('-') {
        (true, unsigned)
    } else if let Some(_unsigned) = text.strip_prefix('+') {
        return Err(MoneyError::InvalidFormat("leading plus sign"));
    } else {
        (false, text)
    };
    if unsigned.is_empty() {
        return Err(MoneyError::InvalidFormat("missing digits"));
    }

    let bytes = unsigned.as_bytes();
    if bytes
        .iter()
        .any(|byte| !is_digit(*byte) && !is_separator(*byte))
    {
        return Err(MoneyError::InvalidFormat(
            "only digits, dots, and commas are allowed",
        ));
    }

    let Some(last_separator) = bytes.iter().rposition(|byte| is_separator(*byte)) else {
        return finish_normalization(negative, unsigned, None);
    };
    let suffix_len = bytes.len() - last_separator - 1;

    if suffix_len == 0 {
        return Err(MoneyError::InvalidFormat("trailing separator"));
    }
    if bytes
        .windows(2)
        .any(|pair| is_separator(pair[0]) && is_separator(pair[1]))
    {
        return Err(MoneyError::InvalidFormat("duplicate separators"));
    }

    let decimal_separator = match suffix_len {
        1 | 2 => Some(bytes[last_separator]),
        3 => None,
        _ => return Err(MoneyError::InvalidPrecision),
    };

    let integer_end = if decimal_separator.is_some() {
        last_separator
    } else {
        bytes.len()
    };
    let integer = strip_grouping(&unsigned[..integer_end], decimal_separator)?;
    let fraction = decimal_separator.map(|_| &unsigned[last_separator + 1..]);
    finish_normalization(negative, &integer, fraction)
}

fn finish_normalization(
    negative: bool,
    integer: &str,
    fraction: Option<&str>,
) -> Result<String, MoneyError> {
    if integer.is_empty() && fraction.is_none() {
        return Err(MoneyError::InvalidFormat("missing digits"));
    }

    let mut normalized = String::with_capacity(
        usize::from(negative) + integer.len().max(1) + fraction.map_or(0, |part| part.len() + 1),
    );
    if negative {
        normalized.push('-');
    }
    if integer.is_empty() {
        normalized.push('0');
    } else {
        normalized.push_str(integer);
    }
    if let Some(fraction) = fraction {
        normalized.push('.');
        normalized.push_str(fraction);
    }
    Ok(normalized)
}

fn strip_grouping(integer: &str, decimal_separator: Option<u8>) -> Result<String, MoneyError> {
    let bytes = integer.as_bytes();
    let mut grouping_separator = None;
    for &byte in bytes {
        if !is_separator(byte) {
            continue;
        }
        if decimal_separator == Some(byte) || grouping_separator.is_some_and(|seen| seen != byte) {
            return Err(MoneyError::AmbiguousSeparators);
        }
        grouping_separator = Some(byte);
    }

    let Some(separator) = grouping_separator else {
        return Ok(integer.to_owned());
    };

    let mut groups = integer.split(char::from(separator));
    let Some(first) = groups.next() else {
        return Err(MoneyError::MalformedGrouping);
    };
    if !is_group(first, false) {
        return Err(MoneyError::MalformedGrouping);
    }
    if groups.any(|group| !is_group(group, true)) {
        return Err(MoneyError::MalformedGrouping);
    }

    let mut digits = String::with_capacity(integer.len());
    for &byte in bytes {
        if byte != separator {
            digits.push(char::from(byte));
        }
    }
    Ok(digits)
}

fn is_group(group: &str, must_be_three: bool) -> bool {
    let length = group.len();
    let length_ok = if must_be_three {
        length == 3
    } else {
        (1..=3).contains(&length)
    };
    length_ok && group.bytes().all(is_digit)
}

const fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

const fn is_separator(byte: u8) -> bool {
    matches!(byte, b'.' | b',')
}

impl fmt::Display for Money {
    /// Always two decimals, regardless of how the provider formatted the
    /// number.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_euros(1000.0)?.to_string(), "1000.00");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.cents < 0 { "-" } else { "" };
        let magnitude = self.cents.unsigned_abs();
        write!(f, "{sign}{}.{:02}", magnitude / 100, magnitude % 100)
    }
}

impl Serialize for Money {
    /// Serializes through the lossy [`Money::euros`] view as a JSON number so
    /// provider-shaped values keep their established representation. Use
    /// [`Money::cents`] when exactness matters.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.euros())
    }
}

struct MoneyVisitor;

impl MoneyVisitor {
    fn from_euros<E: de::Error>(euros: f64) -> Result<Money, E> {
        Money::from_euros(euros).map_err(E::custom)
    }
}

impl Visitor<'_> for MoneyVisitor {
    type Value = Money;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a signed euro amount as a number or a grouped decimal string")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Self::from_euros(value)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        value
            .checked_mul(100)
            .ok_or_else(|| E::custom(MoneyError::OutOfRange))
            .map(Money::from_cents)
            .map_err(E::custom)
    }

    #[allow(clippy::cast_possible_wrap)]
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        let max_euros = i64::MAX as u64 / 100;
        if value > max_euros {
            return Err(E::custom(MoneyError::OutOfRange));
        }
        Ok(Money::from_cents((value * 100) as i64))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Money::parse(value).map_err(E::custom)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(E::custom(
            "money amount must be a number or a decimal string",
        ))
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(E::custom(
            "money amount must be a number or a decimal string",
        ))
    }
}

impl<'de> Deserialize<'de> for Money {
    /// Numbers become signed cents and strings follow the documented
    /// [`Money::parse`] normalization rules. `null` is not a monetary value;
    /// use `Option<Money>` where the provider may omit it.
    ///
    /// ```
    /// let us: letterxpress::Money = serde_json::from_str(r#""1,234.56""#)?;
    /// let german: letterxpress::Money = serde_json::from_str(r#""1.234,56""#)?;
    /// assert_eq!(us.cents(), 123_456);
    /// assert_eq!(german.cents(), 123_456);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MoneyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_cover_the_full_signed_range() {
        assert_eq!(Money::from_cents(0).cents(), 0);
        assert_eq!(Money::from_cents(-1).cents(), -1);
        assert_eq!(Money::from_cents(i64::MAX).cents(), i64::MAX);
        assert_eq!(Money::from_cents(i64::MIN).cents(), i64::MIN);
    }

    #[test]
    fn euro_boundaries_are_checked() -> Result<(), MoneyError> {
        assert_eq!(Money::from_euros(f64::NAN), Err(MoneyError::NotFinite));
        assert_eq!(Money::from_euros(f64::INFINITY), Err(MoneyError::NotFinite));
        assert_eq!(
            Money::from_euros(f64::NEG_INFINITY),
            Err(MoneyError::NotFinite)
        );
        assert_eq!(Money::from_euros(-0.01)?.cents(), -1);
        assert_eq!(Money::from_euros(f64::MAX), Err(MoneyError::OutOfRange));
        assert_eq!(
            Money::from_euros(9_223_372_036_854_770.0)?.cents(),
            922_337_203_685_477_000
        );
        Ok(())
    }

    #[test]
    fn rounding_is_half_away_from_zero_for_both_signs() {
        assert_eq!(Money::from_euros(4.999).map(Money::cents), Ok(500));
        assert_eq!(Money::from_euros(0.005).map(Money::cents), Ok(1));
        assert_eq!(Money::from_euros(0.0049).map(Money::cents), Ok(0));
        assert_eq!(Money::from_euros(-4.999).map(Money::cents), Ok(-500));
        assert_eq!(Money::from_euros(-0.005).map(Money::cents), Ok(-1));
        assert_eq!(Money::from_euros(-0.0049).map(Money::cents), Ok(0));
    }

    #[test]
    fn arithmetic_is_checked() {
        let one = Money::from_cents(1);
        let max = Money::from_cents(i64::MAX);
        assert_eq!(max.checked_add(Money::ZERO), Ok(max));
        assert_eq!(max.checked_add(one), Err(MoneyError::OutOfRange));
        assert_eq!(max.checked_sub(max), Ok(Money::ZERO));
        assert_eq!(Money::ZERO.checked_sub(one), Ok(Money::from_cents(-1)));
        assert_eq!(
            Money::from_cents(i64::MIN).checked_sub(one),
            Err(MoneyError::OutOfRange)
        );
    }

    #[test]
    fn exactness_comes_from_integer_cents() -> Result<(), MoneyError> {
        let sum = Money::from_euros(0.1)?.checked_add(Money::from_euros(0.2)?)?;
        assert_eq!(sum.cents(), 30);
        Ok(())
    }

    fn cents_from(text: &str) -> Result<i64, MoneyError> {
        Money::parse(text).map(Money::cents)
    }

    #[test]
    fn string_grouping_is_not_mistaken_for_decimal_places() {
        // The old `replace(',', ".")` produced 123 cents for the first value
        // and a parse error for the second.
        assert_eq!(cents_from("1,234"), Ok(123_400));
        assert_eq!(cents_from("1.234,56"), Ok(123_456));
        assert_eq!(cents_from("1,234.56"), Ok(123_456));
        assert_eq!(cents_from("1.234"), Ok(123_400));
    }

    #[test]
    fn documented_decimal_and_grouping_shapes_are_accepted() {
        for (text, cents) in [
            ("0", 0),
            ("0.67", 67),
            ("4,50", 450),
            (" 4,50 ", 450),
            (".5", 50),
            (",25", 25),
            ("1,234,567.89", 123_456_789),
            ("1.234.567,89", 123_456_789),
            ("12,345.67", 1_234_567),
            ("123.456,78", 12_345_678),
            ("-0", 0),
            ("-0.00", 0),
        ] {
            assert_eq!(cents_from(text), Ok(cents), "{text:?}");
        }
    }

    #[test]
    fn separator_boundaries_are_explicit() {
        assert_eq!(cents_from("1.99"), Ok(199));
        assert_eq!(cents_from("1,999"), Ok(199_900));
        assert_eq!(cents_from("1,9999"), Err(MoneyError::InvalidPrecision));
        assert_eq!(
            cents_from("1234,567.89"),
            Err(MoneyError::MalformedGrouping)
        );
        assert_eq!(
            cents_from("12,34,567.89"),
            Err(MoneyError::MalformedGrouping)
        );
    }

    #[test]
    fn mixed_or_reused_separator_roles_are_rejected() {
        for text in ["1,234,56", "1.234.56", "1,234.567", "1.234,567"] {
            assert_eq!(
                cents_from(text),
                Err(MoneyError::AmbiguousSeparators),
                "{text:?}"
            );
        }
    }

    #[test]
    fn malformed_signs_separators_and_exponents_are_rejected() {
        for text in ["", "   ", "+4.50", "--1", "1-", "1e2", "1E2", "1_000"] {
            assert!(
                matches!(cents_from(text), Err(MoneyError::InvalidFormat(_))),
                "{text:?}"
            );
        }
        for text in ["4.", "4,", "1,234."] {
            assert_eq!(
                cents_from(text),
                Err(MoneyError::InvalidFormat("trailing separator")),
                "{text:?}"
            );
        }
        for text in ["1..2", "1,,234", "1.,234"] {
            assert_eq!(
                cents_from(text),
                Err(MoneyError::InvalidFormat("duplicate separators")),
                "{text:?}"
            );
        }
        assert_eq!(cents_from("-1.234,56"), Ok(-123_456));
        assert_eq!(cents_from("-92,233,720,368,547,758.08"), Ok(i64::MIN));
    }

    #[test]
    fn cent_range_boundaries_are_exact() {
        assert_eq!(cents_from("92,233,720,368,547,758.07"), Ok(i64::MAX));
        assert_eq!(cents_from("92233720368547758.07"), Ok(i64::MAX));
        assert_eq!(
            cents_from("92,233,720,368,547,758.08"),
            Err(MoneyError::OutOfRange)
        );
        assert_eq!(
            cents_from("92233720368547758.08"),
            Err(MoneyError::OutOfRange)
        );
        assert_eq!(
            cents_from(&format!("0{}", "0".repeat(MAX_AMOUNT_TEXT_BYTES))),
            Err(MoneyError::OutOfRange)
        );
    }

    #[test]
    fn the_money_visitor_accepts_documented_scalars_and_refuses_bad_ones() {
        assert_eq!(
            MoneyVisitor
                .visit_f64::<serde_json::Error>(0.5)
                .expect("finite")
                .cents(),
            50
        );
        assert_eq!(
            MoneyVisitor
                .visit_i64::<serde_json::Error>(3)
                .expect("signed")
                .cents(),
            300
        );
        assert_eq!(
            MoneyVisitor
                .visit_u64::<serde_json::Error>(4)
                .expect("unsigned")
                .cents(),
            400
        );
        for (text, cents) in [(" 4,50 ", 450), ("1,234", 123_400), ("1.234,56", 123_456)] {
            assert_eq!(
                MoneyVisitor
                    .visit_str::<serde_json::Error>(text)
                    .expect("decimal string")
                    .cents(),
                cents
            );
        }
        assert!(MoneyVisitor.visit_unit::<serde_json::Error>().is_err());
        assert!(MoneyVisitor.visit_none::<serde_json::Error>().is_err());

        for bad in [
            "NaN",
            "inf",
            "-inf",
            "+4.5",
            "4.5e2",
            "4.5.",
            "1,234,56",
            "9223372036854775808",
        ] {
            assert!(
                MoneyVisitor.visit_str::<serde_json::Error>(bad).is_err(),
                "{bad}"
            );
        }
        assert!(MoneyVisitor
            .visit_f64::<serde_json::Error>(f64::NAN)
            .is_err());
        assert_eq!(
            MoneyVisitor
                .visit_i64::<serde_json::Error>(-1)
                .expect("negative")
                .cents(),
            -100
        );
        assert!(MoneyVisitor
            .visit_u64::<serde_json::Error>(i64::MAX as u64)
            .is_err());
    }
}
