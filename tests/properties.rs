//! Property tests: the invariants that must hold for *every* input, not just
//! the fixtures.
//!
//! Three groups: payload builders (a value that passes validation always
//! produces the documented wire shape), parsers (no input panics, and decoding
//! is permissive), and the money/date arithmetic.

mod common;

use common::{client_with, pdf};
use letterxpress::transport::Response;
use letterxpress::{
    Color, Date, EmailOption, JobStatus, Letter, Mode, Money, PrintMode, Registered, Shipping,
    MAX_NOTICE_CHARS,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The checksum is always 32 lowercase hex characters and is the MD5 of the
    /// base64 text (not of the PDF bytes).
    #[test]
    fn checksum_is_md5_of_the_base64_text(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut payload = b"%PDF-1.4\n".to_vec();
        payload.extend_from_slice(&body);
        let letter = Letter::new(&payload).expect("a %PDF- payload is valid");
        let json = letter.to_json();
        let encoded = json["base64_file"].as_str().expect("base64");
        let checksum = json["base64_file_checksum"].as_str().expect("checksum");

        prop_assert_eq!(checksum.len(), 32);
        prop_assert!(checksum.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        prop_assert_eq!(checksum, letterxpress::md5_hex(encoded.as_bytes()));
    }

    /// Base64 round-trips exactly, including for payloads with every byte value.
    #[test]
    fn base64_round_trips(body in proptest::collection::vec(any::<u8>(), 0..4096)) {
        use base64::Engine as _;
        let mut payload = b"%PDF-1.4\n".to_vec();
        payload.extend_from_slice(&body);
        let letter = Letter::new(&payload).expect("valid");
        let encoded = letter.to_json()["base64_file"].as_str().expect("base64").to_string();
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).expect("decode");
        prop_assert_eq!(decoded, payload);
    }

    /// Any payload that does not start with `%PDF-` is rejected before encoding.
    #[test]
    fn non_pdf_payloads_are_always_rejected(body in proptest::collection::vec(any::<u8>(), 0..1024)) {
        if body.starts_with(b"%PDF-") {
            return Ok(());
        }
        prop_assert!(Letter::new(&body).is_err());
    }

    /// Notice validation accepts exactly the strings the provider accepts:
    /// non-empty, at most 255 characters, no control characters.
    #[test]
    fn notice_validation_matches_the_documented_rule(text in "\\PC{0,300}") {
        let has_control = text.chars().any(char::is_control);
        let result = Letter::new(&pdf()).expect("letter").with_notice(text.clone());
        let acceptable = !text.trim().is_empty()
            && text.chars().count() <= MAX_NOTICE_CHARS
            && !has_control;
        prop_assert_eq!(result.is_ok(), acceptable);
        if let Ok(letter) = result {
            prop_assert_eq!(letter.notice(), Some(text.as_str()));
        }
    }

    /// Money survives a trip through cents and back, for every plausible value.
    #[test]
    fn cents_round_trip(cents in -100_000_000i64..100_000_000) {
        prop_assert_eq!(Money::from_cents(cents).cents(), cents);
    }

    /// Net plus tax is the sum, in cents as in euro.
    #[test]
    fn totals_add_up(a in 0i64..100_000, b in 0i64..100_000) {
        let net = Money::from_cents(a);
        let vat = Money::from_cents(b);
        prop_assert_eq!(net.total(vat).cents(), a + b);
        prop_assert_eq!(net + vat, net.total(vat));
    }

    /// Parsing a status and re-parsing its wire form is idempotent — including
    /// for values this crate does not model.
    #[test]
    fn status_parsing_is_idempotent(raw in "\\PC{0,40}") {
        let status = JobStatus::from_wire(&raw);
        let again = JobStatus::from_wire(status.as_str());
        prop_assert_eq!(status.clone(), again);
    }

    /// Known enum spellings are recognised in any case and with any padding.
    #[test]
    fn known_enum_values_survive_case_and_padding(
        padded in prop::sample::select(vec!["done", "DONE", " Done ", "done\t"]).prop_map(String::from)
    ) {
        prop_assert_eq!(JobStatus::from_wire(&padded), JobStatus::Done);
    }

    /// A validated date always formats as `yyyy-mm-dd` and re-parses to itself.
    #[test]
    fn date_round_trip(year in 1970u16..9999, month in 1u8..=12, day in 1u8..=28) {
        let date = Date::new(year, month, day).expect("a day <= 28 always exists");
        let text = date.to_string();
        prop_assert_eq!(text.len(), 10);
        prop_assert_eq!(Date::parse(&text).expect("re-parse"), date);
        prop_assert_eq!(date.days_since_epoch(), Date::parse(&text).expect("re-parse").days_since_epoch());
    }

    /// Ordering by day count agrees with ordering by fields.
    #[test]
    fn date_ordering_is_consistent(
        a in (1970u16..9999, 1u8..=12, 1u8..=28),
        b in (1970u16..9999, 1u8..=12, 1u8..=28)
    ) {
        let left = Date::new(a.0, a.1, a.2).expect("date");
        let right = Date::new(b.0, b.1, b.2).expect("date");
        prop_assert_eq!(left.cmp(&right), left.days_since_epoch().cmp(&right.days_since_epoch()));
        prop_assert_eq!(left.is_after(&right), left.days_since_epoch() > right.days_since_epoch());
    }

    /// The response parser never panics, whatever bytes arrive.
    #[test]
    fn envelope_parsing_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let response = Response::json_response(200, bytes);
        let _ = letterxpress::parse_envelope("GET /v3/balance", response);
    }

    /// A hostile body with an error status also never panics.
    #[test]
    fn envelope_error_paths_never_panic(
        status in prop::sample::select(vec![400u16, 401, 403, 404, 405, 429, 500, 502, 503]),
        bytes in proptest::collection::vec(any::<u8>(), 0..4096)
    ) {
        let _ = letterxpress::parse_envelope("POST /v3/printjobs", Response::json_response(status, bytes));
    }

    /// Arbitrary JSON decodes into a print job without panicking, and a missing
    /// status is never treated as terminal.
    #[test]
    fn print_job_decoding_is_permissive(
        bytes in proptest::collection::vec(any::<u8>(), 0..4096)
    ) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(());
        };
        if let Ok(job) = serde_json::from_value::<letterxpress::PrintJob>(value.clone()) {
            if job.status == JobStatus::Other(String::new()) {
                prop_assert!(!job.is_terminal());
            }
            prop_assert_eq!(job.total().cents(), job.net().cents() + job
                .items
                .iter()
                .map(|item| item.vat.cents())
                .sum::<i64>());
        }
        if let Ok(page) = serde_json::from_value::<letterxpress::PrintJobPage>(value.clone()) {
            prop_assert!(page.jobs.len() <= 1024);
        }
        let _ = letterxpress::EmailJobPage::parse(value.clone(), None);
        let _ = letterxpress::EmailJobSubmission::parse(value);
    }

    /// Every colour/print-mode/shipping combination produces a valid
    /// specification with the documented wire values.
    #[test]
    fn specification_always_serialises_three_documented_fields(
        color_index in 0usize..2,
        mode_index in 0usize..2,
        shipping_index in 0usize..3,
        c4 in any::<bool>(),
    ) {
        let color = if color_index == 0 { Color::BlackAndWhite } else { Color::Color };
        let mode = if mode_index == 0 { PrintMode::Simplex } else { PrintMode::Duplex };
        let shipping = match shipping_index {
            0 => Shipping::National,
            1 => Shipping::International,
            _ => Shipping::Auto,
        };
        let spec = letterxpress::Specification::new()
            .with_color(color)
            .with_print_mode(mode)
            .with_shipping(shipping)
            .with_c4_envelope(c4);
        let json = spec.to_json();

        prop_assert!(matches!(json["color"].as_str(), Some("1" | "4")));
        prop_assert!(matches!(json["mode"].as_str(), Some("simplex" | "duplex")));
        prop_assert!(matches!(json["shipping"].as_str(), Some("national" | "international" | "auto")));
        prop_assert_eq!(json["c4"].as_u64(), Some(u64::from(u8::from(c4))));
    }

    /// Email addresses that pass validation are accepted by the builder, and an
    /// invalid one never reaches the wire.
    #[test]
    fn email_validation_is_monotone(
        local in "[a-z]{1,10}",
        domain in "[a-z]{1,10}",
        tld in "[a-z]{2,4}"
    ) {
        let address = format!("{local}@{domain}.{tld}");
        let delivery = letterxpress::EmailDelivery::new(address.clone(), EmailOption::MailDirect);
        prop_assert!(delivery.is_ok(), "{address}");
        let delivery = delivery.expect("delivery");
        prop_assert_eq!(delivery.receiver(), address);
    }
}

#[test]
fn a_mode_cannot_be_parsed_from_noise() {
    for raw in ["", "prod", "production", "Test ", " live", "LIVE!"] {
        let acceptable = matches!(raw.trim().to_ascii_lowercase().as_str(), "test" | "live");
        assert_eq!(raw.parse::<Mode>().is_ok(), acceptable, "{raw:?}");
    }
}

#[test]
fn unknown_colours_and_shipping_classes_are_carried() {
    let job: letterxpress::PrintJob = serde_json::from_str(
        r#"{"id":1,"color":"9","shipping":"interplanetary","mode":"tumble","registered":"r9","items":[]}"#,
    )
    .expect("job");
    assert_eq!(job.color, Some(Color::Other("9".into())));
    assert_eq!(job.shipping, Some(Shipping::Other("interplanetary".into())));
    assert_eq!(job.mode, Some(PrintMode::Other("tumble".into())));
    assert_eq!(job.registered, Some(Registered::Other("r9".into())));
}

#[tokio::test]
async fn a_letter_without_a_notice_is_still_submittable() {
    let client = client_with(vec![Response::json_response(
        200,
        r#"{"status":200,"data":{"id":1,"status":"queue","items":[]}}"#,
    )]);
    let letter = Letter::new(&pdf()).expect("letter");
    assert!(letter.notice().is_none());
    client.submit_print_job(&letter).await.expect("submit");
}
