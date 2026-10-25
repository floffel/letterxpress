//! Money as the API sends it: decimal euro amounts, never minor units.
//!
//! LetterXpress reports `amount`, `vat`, `price` and `balance` as JSON numbers
//! with two decimals (`0.67`, `4.99`, `1000`). This module keeps that value
//! exactly as received and offers an explicit conversion to cents at the edge,
//! so accounting code never accumulates float error on its own terms.
//!
//! ```
//! use letterxpress::Money;
//!
//! let net = Money::from_euros(0.68);
//! let vat = Money::from_euros(0.13);
//! assert_eq!(net.total(vat).cents(), 81);
//! assert_eq!(net.to_string(), "0.68");
//! ```

use std::fmt;
use std::ops::Add;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A monetary amount in euro, as reported by the provider.
///
/// ```
/// use letterxpress::Money;
///
/// assert_eq!(Money::from_cents(81).euros(), 0.81);
/// assert_eq!(Money::default().cents(), 0);
/// assert_eq!(Money::from_euros(2.5).cents(), 250);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Money {
    euros: f64,
}

impl Money {
    /// Zero euro.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::ZERO.cents(), 0);
    /// ```
    pub const ZERO: Self = Self { euros: 0.0 };

    /// Wraps a euro value.
    ///
    /// Non-finite input is not rejected here (the provider never sends it);
    /// [`Money::cents`] saturates instead of panicking.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// assert_eq!(Money::from_euros(4.99).to_string(), "4.99");
    /// ```
    #[must_use]
    pub const fn from_euros(euros: f64) -> Self {
        Self { euros }
    }

    /// Builds an amount from minor units.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// assert_eq!(Money::from_cents(68).euros(), 0.68);
    /// ```
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn from_cents(cents: i64) -> Self {
        Self {
            euros: cents as f64 / 100.0,
        }
    }

    /// The euro value as received.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_euros(0.67).euros(), 0.67);
    /// ```
    #[must_use]
    pub const fn euros(self) -> f64 {
        self.euros
    }

    /// The value in cents, rounded half away from zero.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// assert_eq!(Money::from_euros(0.675).cents(), 68);
    /// assert_eq!(Money::from_euros(-0.675).cents(), -68);
    /// ```
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn cents(self) -> i64 {
        // `as i64` saturates for NaN/infinity, so this cannot panic even on a
        // hostile response.
        (self.euros * 100.0).round() as i64
    }

    /// Net plus tax.
    ///
    /// ```
    /// use letterxpress::Money;
    ///
    /// let gross = Money::from_euros(0.68).total(Money::from_euros(0.13));
    /// assert_eq!(gross.cents(), 81);
    /// ```
    #[must_use]
    pub fn total(self, vat: Self) -> Self {
        Self {
            euros: self.euros + vat.euros,
        }
    }

    /// Whether the amount is zero.
    ///
    /// ```
    /// assert!(letterxpress::Money::ZERO.is_zero());
    /// ```
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.euros == 0.0
    }
}

impl Add for Money {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            euros: self.euros + rhs.euros,
        }
    }
}

impl fmt::Display for Money {
    /// Always two decimals, regardless of how the provider formatted the
    /// number.
    ///
    /// ```
    /// assert_eq!(letterxpress::Money::from_euros(1000.0).to_string(), "1000.00");
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}", self.euros)
    }
}

impl From<f64> for Money {
    fn from(value: f64) -> Self {
        Self::from_euros(value)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.euros)
    }
}

struct MoneyVisitor;

impl Visitor<'_> for MoneyVisitor {
    type Value = Money;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a euro amount as a number or a decimal string")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(Money::from_euros(value))
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Money::from_euros(value as f64))
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Money::from_euros(value as f64))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        // Invoices and transaction exports are sometimes quoted; a German
        // decimal comma is tolerated because the provider's CSV uses `;` as
        // the separator and `,` as the decimal mark.
        let cleaned = value.trim().replace(',', ".");
        cleaned
            .parse::<f64>()
            .map(Money::from_euros)
            .map_err(|_| E::custom(format!("not a euro amount: {value:?}")))
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
    fn the_money_visitor_accepts_every_documented_scalar() {
        assert_eq!(
            MoneyVisitor
                .visit_f64::<serde_json::Error>(0.5)
                .unwrap()
                .cents(),
            50
        );
        assert_eq!(
            MoneyVisitor
                .visit_i64::<serde_json::Error>(-2)
                .unwrap()
                .cents(),
            -200
        );
        assert_eq!(
            MoneyVisitor
                .visit_u64::<serde_json::Error>(3)
                .unwrap()
                .cents(),
            300
        );
        assert_eq!(
            MoneyVisitor
                .visit_str::<serde_json::Error>(" 4,50 ")
                .unwrap()
                .cents(),
            450
        );
        assert_eq!(
            MoneyVisitor.visit_unit::<serde_json::Error>().unwrap(),
            Money::ZERO
        );
        assert_eq!(
            MoneyVisitor.visit_none::<serde_json::Error>().unwrap(),
            Money::ZERO
        );
    }
}
