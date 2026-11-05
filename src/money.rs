//! Money as the API sends it: decimal euro amounts, stored exactly as cents.
//!
//! LetterXpress reports `amount`, `vat`, `price` and `balance` as JSON numbers
//! with two decimals (`0.67`, `4.99`, `1000`). This module normalises those
//! values to [`i64`] cents once, at the boundary, so accounting code never
//! accumulates binary floating-point error.
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

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A failure to construct or combine a nonnegative [`Money`] amount.
///
/// ```
/// use letterxpress::{Money, MoneyError};
///
/// assert_eq!(Money::from_cents(-1), Err(MoneyError::Negative));
/// assert_eq!(Money::from_euros(f64::NAN), Err(MoneyError::NotFinite));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MoneyError {
    /// The amount is below zero.
    #[error("money amount must not be negative")]
    Negative,
    /// The euro value is NaN or infinite.
    #[error("money amount must be finite")]
    NotFinite,
    /// The amount cannot be represented as a nonnegative [`i64`] cent value.
    #[error("money amount is out of range for i64 cents")]
    OutOfRange,
}

/// A nonnegative monetary amount in euro cents.
///
/// ```
/// use letterxpress::Money;
///
/// assert_eq!(Money::from_cents(81)?.euros(), 0.81);
/// assert_eq!(Money::default().cents(), 0);
/// assert_eq!(Money::from_euros(2.5)?.cents(), 250);
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

    /// Wraps a nonnegative euro value, rounding half-up to cents.
    ///
    /// # Errors
    ///
    /// [`MoneyError::NotFinite`] for NaN or infinity,
    /// [`MoneyError::Negative`] for a negative value, and
    /// [`MoneyError::OutOfRange`] when rounding to [`i64`] cents overflows.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// assert_eq!(Money::from_euros(4.999)?.cents(), 500);
    /// assert_eq!(Money::from_euros(-0.01), Err(MoneyError::Negative));
    /// assert_eq!(Money::from_euros(f64::INFINITY), Err(MoneyError::NotFinite));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn from_euros(euros: f64) -> Result<Self, MoneyError> {
        if !euros.is_finite() {
            return Err(MoneyError::NotFinite);
        }
        if euros < 0.0 {
            return Err(MoneyError::Negative);
        }

        let cents = euros * 100.0;
        if !cents.is_finite() {
            return Err(MoneyError::OutOfRange);
        }

        let rounded = cents.round();
        // `i64::MAX as f64` rounds up to 2^63, so every representable value at
        // or above that point is too large for an i64.
        if rounded >= i64::MAX as f64 {
            return Err(MoneyError::OutOfRange);
        }

        Ok(Self {
            cents: rounded as i64,
        })
    }

    /// Builds an amount from minor units.
    ///
    /// # Errors
    ///
    /// [`MoneyError::Negative`] when `cents` is below zero. Zero and
    /// [`i64::MAX`] are valid.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// assert_eq!(Money::from_cents(68)?.euros(), 0.68);
    /// assert_eq!(Money::from_cents(i64::MAX)?.cents(), i64::MAX);
    /// assert_eq!(Money::from_cents(-1), Err(MoneyError::Negative));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn from_cents(cents: i64) -> Result<Self, MoneyError> {
        if cents < 0 {
            Err(MoneyError::Negative)
        } else {
            Ok(Self { cents })
        }
    }

    /// The euro value, derived from the exact cent amount.
    ///
    /// The result is a floating-point convenience only; cent arithmetic should
    /// use [`Money::cents`].
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_cents(67)?.euros(), 0.67);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn euros(self) -> f64 {
        self.cents as f64 / 100.0
    }

    /// The exact value in cents. It is never negative by invariant.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_cents(68)?.cents(), 68);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
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
    /// let one = Money::from_cents(1)?;
    /// assert_eq!(Money::ZERO.checked_add(one)?.cents(), 1);
    /// assert_eq!(
    ///     Money::from_cents(i64::MAX)?.checked_add(one),
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
    /// [`MoneyError::Negative`] when the result would be below zero.
    ///
    /// ```
    /// use letterxpress::{Money, MoneyError};
    ///
    /// let one = Money::from_cents(1)?;
    /// assert_eq!(one.checked_sub(one)?.cents(), 0);
    /// assert_eq!(Money::ZERO.checked_sub(one), Err(MoneyError::Negative));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn checked_sub(self, rhs: Self) -> Result<Self, MoneyError> {
        match self.cents.checked_sub(rhs.cents) {
            Some(cents) if cents >= 0 => Ok(Self { cents }),
            _ => Err(MoneyError::Negative),
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
    /// let gross = Money::from_cents(68)?.checked_add(Money::from_cents(13)?)?;
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
        write!(f, "{}.{:02}", self.cents / 100, self.cents % 100)
    }
}

impl Serialize for Money {
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
        f.write_str("a nonnegative euro amount as a number or a decimal string")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Self::from_euros(value)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        if value < 0 {
            return Err(E::custom(MoneyError::Negative));
        }
        value
            .checked_mul(100)
            .ok_or_else(|| E::custom(MoneyError::OutOfRange))
            .and_then(|cents| Money::from_cents(cents).map_err(E::custom))
    }

    #[allow(clippy::cast_possible_wrap)]
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        let max_euros = i64::MAX as u64 / 100;
        if value > max_euros {
            return Err(E::custom(MoneyError::OutOfRange));
        }
        Money::from_cents((value * 100) as i64).map_err(E::custom)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        // Invoices and transaction exports are sometimes quoted; a German
        // decimal comma is tolerated because the provider's CSV uses `;` as
        // the separator and `,` as the decimal mark.
        let cleaned = value.trim().replace(',', ".");
        cleaned
            .parse::<f64>()
            .map_err(|_| E::custom(format!("not a euro amount: {value:?}")))
            .and_then(Self::from_euros)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Money::ZERO)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Money::ZERO)
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(MoneyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_boundaries_are_checked() {
        assert_eq!(Money::from_cents(0).map(Money::cents), Ok(0));
        assert_eq!(Money::from_cents(-1), Err(MoneyError::Negative));
        assert_eq!(Money::from_cents(i64::MAX).map(Money::cents), Ok(i64::MAX));
        assert_eq!(Money::from_cents(i64::MIN), Err(MoneyError::Negative));
    }

    #[test]
    fn euro_boundaries_are_checked() -> Result<(), MoneyError> {
        assert_eq!(Money::from_euros(f64::NAN), Err(MoneyError::NotFinite));
        assert_eq!(Money::from_euros(f64::INFINITY), Err(MoneyError::NotFinite));
        assert_eq!(
            Money::from_euros(f64::NEG_INFINITY),
            Err(MoneyError::NotFinite)
        );
        assert_eq!(Money::from_euros(-0.01), Err(MoneyError::Negative));
        assert_eq!(Money::from_euros(f64::MAX), Err(MoneyError::OutOfRange));
        assert_eq!(
            Money::from_euros(9_223_372_036_854_770.0)?.cents(),
            922_337_203_685_476_992
        );
        Ok(())
    }

    #[test]
    fn rounding_is_half_up_for_nonnegative_amounts() {
        assert_eq!(Money::from_euros(4.999).map(Money::cents), Ok(500));
        assert_eq!(Money::from_euros(0.005).map(Money::cents), Ok(1));
        assert_eq!(Money::from_euros(0.0049).map(Money::cents), Ok(0));
    }

    #[test]
    fn arithmetic_is_checked() -> Result<(), MoneyError> {
        let one = Money::from_cents(1)?;
        let max = Money::from_cents(i64::MAX)?;
        assert_eq!(max.checked_add(Money::ZERO), Ok(max));
        assert_eq!(max.checked_add(one), Err(MoneyError::OutOfRange));
        assert_eq!(max.checked_sub(max), Ok(Money::ZERO));
        assert_eq!(Money::ZERO.checked_sub(one), Err(MoneyError::Negative));
        Ok::<(), MoneyError>(())
    }

    #[test]
    fn exactness_comes_from_integer_cents() -> Result<(), MoneyError> {
        let sum = Money::from_euros(0.1)?.checked_add(Money::from_euros(0.2)?)?;
        assert_eq!(sum.cents(), 30);
        Ok(())
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
                .expect("nonnegative")
                .cents(),
            300
        );
        assert_eq!(
            MoneyVisitor
                .visit_u64::<serde_json::Error>(4)
                .expect("nonnegative")
                .cents(),
            400
        );
        assert_eq!(
            MoneyVisitor
                .visit_str::<serde_json::Error>(" 4,50 ")
                .expect("decimal comma")
                .cents(),
            450
        );
        assert_eq!(
            MoneyVisitor
                .visit_unit::<serde_json::Error>()
                .expect("null")
                .cents(),
            0
        );
        assert_eq!(
            MoneyVisitor
                .visit_none::<serde_json::Error>()
                .expect("none")
                .cents(),
            0
        );

        for bad in ["NaN", "inf", "-inf", "1e400", "-1", "9223372036854775808"] {
            assert!(
                MoneyVisitor.visit_str::<serde_json::Error>(bad).is_err(),
                "{bad}"
            );
        }
        assert!(MoneyVisitor
            .visit_f64::<serde_json::Error>(f64::NAN)
            .is_err());
        assert!(MoneyVisitor.visit_i64::<serde_json::Error>(-1).is_err());
        assert!(MoneyVisitor
            .visit_u64::<serde_json::Error>(i64::MAX as u64)
            .is_err());
    }
}
