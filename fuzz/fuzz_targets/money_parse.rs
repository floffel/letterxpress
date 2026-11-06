//! Fuzz target: [`Money::parse`] must never panic on arbitrary UTF-8 input, and
//! every accepted amount must round-trip through its canonical decimal form.
//!
//! Run with `cargo +nightly fuzz run money_parse`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use letterxpress::Money;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let Ok(money) = Money::parse(text) else {
        return;
    };

    assert_eq!(Money::parse(&money.to_string()).ok(), Some(money));
});
