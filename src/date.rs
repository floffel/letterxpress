//! A calendar date in the provider's `yyyy-mm-dd` format.
//!
//! [`Date`] is a thin wrapper around [`jiff::civil::Date`], restricted to the
//! provider's `1970..=9999` range. The maintained library owns the proleptic
//! Gregorian calendar, leap years, month lengths and ISO rendering; this
//! wrapper keeps the strict `yyyy-mm-dd` wire shape, maps failures to
//! [`Error`], and orders dates by their civil fields.
//!
//! ```
//! use letterxpress::Date;
//!
//! let date = Date::parse("2027-02-28")?;
//! assert_eq!(date.to_string(), "2027-02-28");
//! assert!(Date::parse("2027-02-29").is_err()); // not a leap year
//! assert!(date.is_after(&Date::parse("2027-02-27")?));
//! assert!(Date::parse("2023-12-31")? < Date::parse("2024-01-01")?);
//! assert!(Date::parse("2024-02-28")? < Date::parse("2024-02-29")?);
//! assert!(Date::parse("2024-02-29")? < Date::parse("2024-03-01")?);
//! # Ok::<(), letterxpress::Error>(())
//! ```

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use jiff::civil::Date as CivilDate;

use crate::Error;

const UNIX_EPOCH: CivilDate = CivilDate::constant(1970, 1, 1);
const SECONDS_PER_DAY: i64 = 86_400;

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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Date(CivilDate);

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
        let Ok(year) = i16::try_from(year) else {
            return Err(year_error());
        };
        if !(1970..=9999).contains(&year) {
            return Err(year_error());
        }
        let Ok(month) = i8::try_from(month) else {
            return Err(month_error());
        };
        if !(1..=12).contains(&month) {
            return Err(month_error());
        }
        let first_of_month = CivilDate::new(year, month, 1)
            .map_err(|error| Error::invalid("date", error.to_string()))?;
        let max_day = first_of_month.days_in_month();
        let Ok(day) = i8::try_from(day) else {
            return Err(day_error(year, month, max_day));
        };
        if !(1..=max_day).contains(&day) {
            return Err(day_error(year, month, max_day));
        }
        CivilDate::new(year, month, day)
            .map(Self)
            .map_err(|error| Error::invalid("date", error.to_string()))
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
        // jiff also accepts a date embedded in an ISO date-time. The provider
        // does not, so enforce its fixed-width wire shape before validating the
        // calendar through `Date::new`.
        let bytes = text.as_bytes();
        let shaped = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
        if !shaped {
            return Err(date_shape_error(text));
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
        self.0.year().unsigned_abs()
    }

    /// The month, `1`-based.
    ///
    /// ```
    /// assert_eq!(letterxpress::Date::new(2026, 10, 24).unwrap().month(), 10);
    /// ```
    #[must_use]
    pub fn month(self) -> u8 {
        self.0.month().unsigned_abs()
    }

    /// The day of the month.
    ///
    /// ```
    /// assert_eq!(letterxpress::Date::new(2026, 10, 24).unwrap().day(), 24);
    /// ```
    #[must_use]
    pub fn day(self) -> u8 {
        self.0.day().unsigned_abs()
    }

    /// Whether this date falls strictly after `other`.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// let today = Date::new(2026, 10, 24)?;
    /// assert!(Date::new(2026, 10, 25)?.is_after(&today));
    /// assert!(!today.is_after(&today));
    /// assert!(Date::new(2024, 1, 1)?.is_after(&Date::new(2023, 12, 31)?));
    /// assert!(Date::new(2024, 3, 1)?.is_after(&Date::new(2024, 2, 29)?));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn is_after(self, other: &Self) -> bool {
        self > *other
    }

    /// Days since 1970-01-01, valid for `1970-01-01..=9999-12-31`, the range
    /// [`Date::new`] accepts. Used for cheap range checks; ordering compares the
    /// civil fields directly.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// assert_eq!(Date::new(1970, 1, 1).unwrap().days_since_epoch(), 0);
    /// assert_eq!(Date::new(1970, 1, 2).unwrap().days_since_epoch(), 1);
    /// assert_eq!(Date::new(9999, 12, 31).unwrap().days_since_epoch(), 2_932_896);
    /// ```
    #[must_use]
    pub fn days_since_epoch(self) -> i64 {
        self.0.duration_since(UNIX_EPOCH).as_secs() / SECONDS_PER_DAY
    }
}

fn year_error() -> Error {
    Error::invalid("date", "year must be between 1970 and 9999")
}

fn month_error() -> Error {
    Error::invalid("date", "month must be between 1 and 12")
}

fn day_error(year: i16, month: i8, max_day: i8) -> Error {
    Error::invalid(
        "date",
        format!("day must be between 1 and {max_day} for {year:04}-{month:02}"),
    )
}

const DATE_ERROR_PREVIEW_CHARS: usize = 32;

fn date_shape_error(text: &str) -> Error {
    let mut chars = text.chars();
    let mut preview: String = chars.by_ref().take(DATE_ERROR_PREVIEW_CHARS).collect();
    if chars.next().is_none() {
        return Error::invalid("date", format!("expected yyyy-mm-dd, got {text:?}"));
    }

    preview.push_str("...");
    Error::invalid(
        "date",
        format!(
            "expected yyyy-mm-dd, got {} bytes starting {preview:?}",
            text.len()
        ),
    )
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Date")
            .field("year", &self.year())
            .field("month", &self.month())
            .field("day", &self.day())
            .finish()
    }
}

impl FromStr for Date {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Ord for Date {
    /// Orders dates by their civil `(year, month, day)` fields.
    ///
    /// ```
    /// use letterxpress::Date;
    ///
    /// let end_of_year = Date::new(2023, 12, 31)?;
    /// let new_year = Date::new(2024, 1, 1)?;
    /// let leap_day = Date::new(2024, 2, 29)?;
    ///
    /// assert!(end_of_year < new_year);
    /// assert!(Date::new(2024, 2, 28)? < leap_day);
    /// assert!(leap_day < Date::new(2024, 3, 1)?);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    fn cmp(&self, other: &Self) -> Ordering {
        (self.year(), self.month(), self.day()).cmp(&(other.year(), other.month(), other.day()))
    }
}

impl PartialOrd for Date {
    /// Returns the civil-field order defined by [`Ord::cmp`].
    ///
    /// ```
    /// use std::cmp::Ordering;
    ///
    /// use letterxpress::Date;
    ///
    /// let earlier = Date::new(2023, 12, 31)?;
    /// let later = Date::new(2024, 1, 1)?;
    /// assert_eq!(earlier.partial_cmp(&later), Some(Ordering::Less));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    fn assert_validation_error(text: &str) {
        assert!(
            matches!(
                Date::parse(text),
                Err(Error::Validation { field: "date", .. })
            ),
            "expected a date validation error for {text:?}"
        );
    }

    fn assert_new_validation_error(year: u16, month: u8, day: u8) {
        assert!(
            matches!(
                Date::new(year, month, day),
                Err(Error::Validation { field: "date", .. })
            ),
            "expected a date validation error for {year:04}-{month:02}-{day:02}"
        );
    }

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date::new(year, month, day).expect("valid test date")
    }

    #[test]
    fn date_operations_are_backed_by_jiff_civil_date() {
        let date = Date::new(2024, 2, 29).expect("leap day");
        let civil = CivilDate::new(2024, 2, 29).expect("jiff leap day");
        let later = Date::new(2024, 3, 1).expect("next day");

        assert_eq!(date.0, civil);
        assert_eq!(date.to_string(), civil.to_string());
        assert_eq!(
            date.days_since_epoch(),
            civil.duration_since(UNIX_EPOCH).as_secs() / SECONDS_PER_DAY
        );
        assert_eq!(date.cmp(&later), Ordering::Less);
    }

    #[test]
    fn year_boundaries_are_inclusive() {
        assert!(Date::new(1970, 1, 1).is_ok());
        assert_new_validation_error(1969, 12, 31);

        assert!(Date::new(9999, 12, 31).is_ok());
        assert_new_validation_error(10_000, 1, 1);
        assert_new_validation_error(u16::MAX, 1, 1);
    }

    #[test]
    fn month_boundaries_are_inclusive() {
        assert!(Date::new(2026, 1, 1).is_ok());
        assert_new_validation_error(2026, 0, 1);

        assert!(Date::new(2026, 12, 31).is_ok());
        assert_new_validation_error(2026, 13, 1);
        assert_new_validation_error(2026, u8::MAX, 1);
    }

    #[test]
    fn day_boundaries_include_leap_years_and_month_lengths() {
        for (year, month, last_day) in [
            (2024, 2, 29),
            (2023, 2, 28),
            (2026, 4, 30),
            (2026, 1, 31),
            (2000, 2, 29),
            (2100, 2, 28),
        ] {
            assert!(Date::new(year, month, last_day).is_ok());
            assert_new_validation_error(year, month, last_day + 1);
            assert_new_validation_error(year, month, 0);
        }
        assert_new_validation_error(2026, 1, u8::MAX);
    }

    #[test]
    fn ordering_matches_the_civil_tuple_at_calendar_boundaries() {
        let dates = [
            date(1970, 1, 1),
            date(1970, 1, 31),
            date(1970, 2, 1),
            date(1970, 2, 28),
            date(1970, 3, 1),
            date(1970, 3, 31),
            date(1970, 4, 1),
            date(1970, 4, 30),
            date(1970, 5, 1),
            date(1970, 5, 31),
            date(1970, 6, 1),
            date(1970, 6, 30),
            date(1970, 7, 1),
            date(1970, 7, 31),
            date(1970, 8, 1),
            date(1970, 8, 31),
            date(1970, 9, 1),
            date(1970, 9, 30),
            date(1970, 10, 1),
            date(1970, 10, 31),
            date(1970, 11, 1),
            date(1970, 11, 30),
            date(1970, 12, 1),
            date(1970, 12, 31),
            date(1971, 1, 1),
            date(1999, 12, 31),
            date(2000, 1, 1),
            date(2000, 2, 28),
            date(2000, 2, 29),
            date(2000, 3, 1),
            date(2000, 12, 31),
            date(2001, 1, 1),
            date(2099, 12, 31),
            date(2100, 1, 1),
            date(2100, 2, 28),
            date(2100, 3, 1),
            date(2100, 12, 31),
            date(2101, 1, 1),
            date(2399, 12, 31),
            date(2400, 1, 1),
            date(2400, 2, 28),
            date(2400, 2, 29),
            date(2400, 3, 1),
            date(2400, 12, 31),
            date(2401, 1, 1),
            date(9998, 12, 31),
            date(9999, 1, 1),
            date(9999, 12, 30),
            date(9999, 12, 31),
        ];

        for pair in dates.windows(2) {
            let earlier = pair[0];
            let later = pair[1];
            let expected = (earlier.year(), earlier.month(), earlier.day()).cmp(&(
                later.year(),
                later.month(),
                later.day(),
            ));

            assert_eq!(expected, Ordering::Less, "{earlier} must precede {later}");
            assert_eq!(earlier.cmp(&later), expected);
            assert_eq!(later.cmp(&earlier), Ordering::Greater);
            assert_eq!(earlier.cmp(&earlier), Ordering::Equal);
            assert!(later.is_after(&earlier));
            assert!(!earlier.is_after(&later));
            assert!(!earlier.is_after(&earlier));
        }
    }

    #[test]
    fn century_leap_rules_hold_across_the_supported_range() {
        for year in (2000u16..=9900).step_by(100) {
            assert!(Date::new(year, 2, 28).is_ok(), "{year}-02-28 must be valid");
            if year.is_multiple_of(400) {
                assert!(Date::new(year, 2, 29).is_ok(), "{year} is a leap year");
                assert_new_validation_error(year, 2, 30);
            } else {
                assert_new_validation_error(year, 2, 29);
            }
        }

        assert!(Date::new(9999, 2, 28).is_ok());
        assert_new_validation_error(9999, 2, 29);
    }

    #[test]
    fn new_keeps_the_existing_validation_messages() {
        assert_eq!(
            Date::new(1969, 1, 1).expect_err("year").to_string(),
            "invalid date: year must be between 1970 and 9999"
        );
        assert_eq!(
            Date::new(2026, 13, 1).expect_err("month").to_string(),
            "invalid date: month must be between 1 and 12"
        );
        assert_eq!(
            Date::new(2023, 2, 29).expect_err("day").to_string(),
            "invalid date: day must be between 1 and 28 for 2023-02"
        );
    }

    #[test]
    fn parse_accepts_the_calendar_boundaries() {
        for text in [
            "1970-01-01",
            "2024-02-29",
            "2026-01-31",
            "2026-12-31",
            "9999-12-31",
        ] {
            assert!(Date::parse(text).is_ok(), "expected {text:?} to parse");
        }
    }

    #[test]
    fn days_since_epoch_and_ordering_are_stable_at_supported_bounds() {
        let min = Date::new(1970, 1, 1).expect("minimum supported date");
        let max = Date::new(9999, 12, 31).expect("maximum supported date");

        assert_eq!(min.days_since_epoch(), 0);
        assert_eq!(max.days_since_epoch(), 2_932_896);
        assert_eq!(min.cmp(&max), Ordering::Less);
        assert!(max.is_after(&min));
        assert!(!min.is_after(&max));
    }

    #[test]
    fn parse_rejects_lengths_on_either_side_of_ten() {
        for text in ["2026-10-2", "2026-10-240"] {
            assert_validation_error(text);
        }
    }

    #[test]
    fn parse_rejects_years_outside_the_supported_range() {
        for text in ["0000-01-01", "1969-12-31", "10000-01-01"] {
            assert_validation_error(text);
        }
    }

    #[test]
    fn parse_rejects_impossible_calendar_fields() {
        for text in [
            "2023-02-29",
            "2026-02-30",
            "2026-00-10",
            "2026-13-01",
            "2026-04-31",
            "2026-10-00",
        ] {
            assert_validation_error(text);
        }
    }

    #[test]
    fn parse_rejects_non_ascii_digits_before_slicing() {
        for text in ["2026-10-é", "2026-10-2４"] {
            assert_validation_error(text);
        }
    }

    #[test]
    fn parse_keeps_the_existing_message_for_ordinary_invalid_dates() {
        let error = Date::parse("2026-10-2").expect_err("invalid date");
        assert_eq!(
            error.to_string(),
            "invalid date: expected yyyy-mm-dd, got \"2026-10-2\""
        );
    }

    #[test]
    fn parse_truncates_the_error_preview_after_32_characters() {
        let at_limit = "x".repeat(32);
        let error = Date::parse(&at_limit).expect_err("invalid date");
        assert_eq!(
            error.to_string(),
            format!("invalid date: expected yyyy-mm-dd, got {at_limit:?}")
        );

        let over_limit = format!("{at_limit}y");
        let error = Date::parse(&over_limit).expect_err("invalid date");
        assert!(matches!(error, Error::Validation { field: "date", .. }));
        assert_eq!(
            error.to_string(),
            format!("invalid date: expected yyyy-mm-dd, got 33 bytes starting \"{at_limit}...\"")
        );
    }

    #[test]
    fn parse_bounds_the_error_preview_for_large_inputs() {
        let tail = "UNBOUNDED-TAIL-MARKER";
        let text = format!("{}{tail}", "x".repeat(4096));
        let error = Date::parse(&text).expect_err("invalid date");

        assert!(matches!(error, Error::Validation { field: "date", .. }));
        let rendered = error.to_string();
        assert!(rendered.contains(&format!("{} bytes", text.len())));
        assert!(rendered.contains(&format!("starting \"{}...\"", "x".repeat(32))));
        assert!(!rendered.contains(&"x".repeat(33)));
        assert!(!rendered.contains(tail));
    }

    #[test]
    fn parse_preview_uses_characters_but_reports_bytes() {
        let text = "é".repeat(33);
        let error = Date::parse(&text).expect_err("invalid date");

        assert!(matches!(error, Error::Validation { field: "date", .. }));
        let rendered = error.to_string();
        assert!(rendered.contains(&format!("{} bytes", text.len())));
        assert!(rendered.contains(&format!("starting \"{}...\"", "é".repeat(32))));
        assert!(!rendered.contains(&"é".repeat(33)));
    }
}
