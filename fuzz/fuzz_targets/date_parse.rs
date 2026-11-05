//! Fuzz target: [`Date::parse`] must never panic on arbitrary UTF-8 input, and
//! every accepted date must preserve its `yyyy-mm-dd` representation.
//!
//! Run with `cargo +nightly fuzz run date_parse`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use letterxpress::Date;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let Ok(date) = Date::parse(text) else {
        return;
    };

    let formatted = date.to_string();
    assert_eq!(formatted.len(), 10);
    assert_eq!(Date::parse(&formatted).ok(), Some(date));
});
