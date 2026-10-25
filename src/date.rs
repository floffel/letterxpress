//! A calendar date in the provider's `yyyy-mm-dd` format.
//!
//! The crate does not pull in a date library for two fields. [`Date`] validates
//! the calendar (including leap years), orders chronologically and formats
//! exactly as the provider documents.
//!
//! ```
//! use letterxpress::Date;
//!
//! let date = Date::parse("2027-02-28")?;
//! assert_eq!(date.to_string(), "2027-02-28");
//! assert!(Date::parse("2027-02-29").is_err()); // not a leap year
//! assert!(date.is_after(&Date::parse("2027-02-27")?));
//! # Ok::<(), letterxpress::Error>(())
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::Error;

/// A validated calendar date, used for `dispatch_date` and journal ranges.
///
/// ```
/// use letterxpress::Date;
///
/// assert_eq!(Date::new(2026, 10, 24)?.to_string(), "2026-10-24");
/// assert!(Date::new(2026, 13, 1).is_err());
/// assert!(Date::new(2026, 2, 29).is_err());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// Builds a date, validating the calendar.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for years outside `1970..=9999`, months outside
    /// `1..=12` and days outside the month's length.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// assert!(Date::new(2024, 2, 29).is_ok()); // leap year
    /// assert!(Date::new(2023, 2, 29).is_err());
    /// assert!(Date::new(1969, 1, 1).is_err());
    /// ```
    pub fn new(year: u16, month: u8, day: u8) -> Result<Self, Error> {
        if !(1970..=9999).contains(&year) {
            return Err(Error::invalid("date", "year must be between 1970 and 9999"));
        }
        if !(1..=12).contains(&month) {
            return Err(Error::invalid("date", "month must be between 1 and 12"));
        }
        let max_day = days_in_month(year, month);
        if day == 0 || day > max_day {
            return Err(Error::invalid(
                "date",
                format!("day must be between 1 and {max_day} for {year:04}-{month:02}"),
            ));
        }
        Ok(Self { year, month, day })
    }

    /// Parses `yyyy-mm-dd`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the shape or the calendar is wrong.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// assert_eq!(Date::parse("2026-10-24")?.day(), 24);
    /// assert!(Date::parse("24.10.2026").is_err());
    /// assert!(Date::parse("2026-1-4").is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bytes = text.as_bytes();
        let shaped = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
        if !shaped {
            return Err(Error::invalid(
                "date",
                format!("expected yyyy-mm-dd, got {text:?}"),
            ));
        }
        let year = text[0..4]
            .parse::<u16>()
            .map_err(|_| Error::invalid("date", "year is not a number"))?;
        let month = text[5..7]
            .parse::<u8>()
            .map_err(|_| Error::invalid("date", "month is not a number"))?;
        let day = text[8..10]
            .parse::<u8>()
            .map_err(|_| Error::invalid("date", "day is not a number"))?;
        Self::new(year, month, day)
    }

    /// The year.
    ///
    /// ```
    /// assert_eq!(letterxpress::Date::new(2026, 10, 24).unwrap().year(), 2026);
    /// ```
    #[must_use]
    pub fn year(self) -> u16 {
        self.year
    }

    /// The month, `1`-based.
    ///
    /// ```
    /// assert_eq!(letterxpress::Date::new(2026, 10, 24).unwrap().month(), 10);
    /// ```
    #[must_use]
    pub fn month(self) -> u8 {
        self.month
    }

    /// The day of the month.
    ///
    /// ```
    /// assert_eq!(letterxpress::Date::new(2026, 10, 24).unwrap().day(), 24);
    /// ```
    #[must_use]
    pub fn day(self) -> u8 {
        self.day
    }

    /// Whether this date falls strictly after `other`.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// let today = Date::new(2026, 10, 24).unwrap();
    /// assert!(Date::new(2026, 10, 25).unwrap().is_after(&today));
    /// assert!(!today.is_after(&today));
    /// ```
    #[must_use]
    pub fn is_after(self, other: &Self) -> bool {
        self > *other
    }

    /// Days since 1970-01-01, proleptic Gregorian. Used for ordering tests and
    /// for cheap range checks.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// assert_eq!(Date::new(1970, 1, 1).unwrap().days_since_epoch(), 0);
    /// assert_eq!(Date::new(1970, 1, 2).unwrap().days_since_epoch(), 1);
    /// ```
    #[must_use]
    pub fn days_since_epoch(self) -> i64 {
        // Days from civil (Howard Hinnant's algorithm), valid for any
        // proleptic Gregorian date.
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let month = i64::from(self.month);
        let day = i64::from(self.day);
        let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for Date {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Ord for Date {
    fn cmp(&self, other: &Self) -> Ordering {
        self.days_since_epoch().cmp(&other.days_since_epoch())
    }
}

impl PartialOrd for Date {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Gregorian month length.
fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_months_have_no_length() {
        // `Date::new` rejects these before the table is consulted; the arm keeps
        // the function total.
        assert_eq!(days_in_month(2026, 0), 0);
        assert_eq!(days_in_month(2026, 13), 0);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }
}
