//! Fuzz target: [`Money::parse`] and JSON deserialization of [`Money`] must
//! never panic on untrusted provider input. Every accepted amount must preserve
//! its canonical decimal representation.
//!
//! Run with `cargo +nightly fuzz run money_parse`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use letterxpress::Money;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(money) = Money::parse(text) {
        assert_eq!(Money::parse(&money.to_string()), Ok(money));
    }

    if let Ok(money) = serde_json::from_str::<Money>(text) {
        assert_eq!(Money::parse(&money.to_string()), Ok(money));
        let quoted = format!("\"{money}\"");
        assert_eq!(serde_json::from_str::<Money>(&quoted).ok(), Some(money));
    }
});
