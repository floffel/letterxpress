//! Property tests: the invariants that must hold for *every* input, not just
//! the fixtures.
//!
//! Four groups: payload builders (a value that passes validation always
//! produces the documented wire shape), parsers (no input panics, and decoding
//! is permissive), the money/date arithmetic, and the retry decision.

mod common;

use std::time::Duration;

use common::{client_with, pdf};
use letterxpress::transport::{Method, RecordedRequest, Response};
use letterxpress::{
    AmbiguousKind, Color, Credentials, Date, EmailOption, Error, JobStatus, Letter, Mode, Money,
    PrintMode, Registered, Shipping, TransportKind, MAX_NOTICE_CHARS, MAX_RETRY_AFTER,
};
use proptest::prelude::*;

fn valid_date() -> impl Strategy<Value = Date> {
    (1970u16..=9999, 1u8..=12, 1u8..=31)
        .prop_filter_map("a valid calendar date", |(year, month, day)| {
            Date::new(year, month, day).ok()
        })
}

/// One `Error` per public variant, with `text` in every prose slot and
/// `status`/`kind` in the slots that decide the stance. Kept exhaustive by
/// hand: a variant added to the enum has to be added here too, which is what
/// makes [`stance`] a statement about the whole enum.
fn every_variant(text: &str, status: u16, kind: TransportKind) -> Vec<Error> {
    use std::time::Duration;

    vec![
        Error::Validation {
            field: "letter.notice",
            message: text.into(),
        },
        Error::Unauthorized {
            message: text.into(),
        },
        Error::Forbidden {
            message: text.into(),
        },
        Error::NotFound {
            message: text.into(),
        },
        Error::MethodNotAllowed {
            message: text.into(),
        },
        Error::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        Error::Rejected {
            status,
            message: text.into(),
        },
        Error::Upstream {
            status,
            message: text.into(),
        },
        Error::UnexpectedStatus {
            status,
            endpoint: "GET /v3/balance",
            message: text.into(),
        },
        Error::Ambiguous {
            kind: AmbiguousKind::Upstream { status },
            message: text.into(),
        },
        Error::Transport {
            kind,
            source: Box::new(std::io::Error::other(text)),
        },
        Error::Json {
            source: serde_json::Error::io(std::io::Error::from(std::io::ErrorKind::InvalidData)),
        },
        Error::MissingField {
            endpoint: "GET /v3/balance",
            field: "data",
        },
        Error::UnexpectedShape {
            endpoint: "GET /v3/balance",
            message: text.into(),
            source: None,
        },
        Error::TooLarge {
            field: "letter.base64_file",
            len: 11,
            max: 10,
        },
        Error::Base64 {
            field: "invoice.base64_data",
            source: base64::DecodeError::InvalidByte(0, b'!'),
        },
        Error::Utf8 {
            field: "journal.csv_base64",
        },
    ]
}

/// The answers the documented table gives per variant: retry for a submit,
/// retry for a read, ambiguity, whether a `Retry-After` hint is carried, and
/// the HTTP status.
fn stance(error: &Error) -> (bool, bool, bool, bool, Option<u16>) {
    (
        error.is_retryable(),
        error.is_retryable_for(true),
        error.is_ambiguous(),
        error.retry_after().is_some(),
        error.status(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Credentials accept exactly non-empty, bounded, log-safe ASCII: graphic
    /// characters and internal spaces, never bidi controls, separators, or
    /// zero-width characters.
    #[test]
    fn credential_validation_accepts_only_ascii_graphic_and_space(
        username in proptest::collection::vec(any::<char>(), 0..300),
        apikey in proptest::collection::vec(any::<char>(), 0..550),
    ) {
        let username: String = username.into_iter().collect();
        let apikey: String = apikey.into_iter().collect();
        let expected_username = is_valid_credential_token(&username, 256);
        let expected_apikey = is_valid_credential_token(&apikey, 512);

        prop_assert_eq!(
            Credentials::new(&username, "key", Mode::Test).is_ok(),
            expected_username
        );
        prop_assert_eq!(
            Credentials::new("user", &apikey, Mode::Test).is_ok(),
            expected_apikey
        );
    }

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

    /// Money survives a trip through cents and back, for every valid value.
    #[test]
    fn cents_round_trip(cents in 0i64..100_000_000) {
        prop_assert_eq!(Money::from_cents(cents).map(Money::cents), Ok(cents));
    }

    /// Net plus tax is the checked sum, in cents as in euro.
    #[test]
    fn totals_add_up(a in 0i64..100_000, b in 0i64..100_000) {
        let net = Money::from_cents(a).expect("nonnegative");
        let vat = Money::from_cents(b).expect("nonnegative");
        prop_assert_eq!(net.total(vat).map(Money::cents), Ok(a + b));
        prop_assert_eq!(net.checked_add(vat), net.total(vat));
    }

    /// Arbitrary text reaching the money parser is rejected or accepted, never
    /// a panic, and a successfully parsed amount round-trips through its
    /// canonical decimal form.
    #[test]
    fn money_text_parsing_never_panics(raw in "\\PC{0,100}") {
        if let Ok(money) = Money::parse(&raw) {
            prop_assert_eq!(Money::parse(&money.to_string()), Ok(money));
        }
    }

    /// US and German grouping are presentation only: for every amount and cent
    /// value around the i64 boundary, both grouped forms resolve exactly like
    /// the ungrouped form.
    #[test]
    fn grouped_money_agrees_with_the_ungrouped_value(
        euros in 0u64..=92_233_720_368_547_758,
        cents in 0u8..100,
    ) {
        let whole = euros.to_string();
        let ungrouped = format!("{whole}.{cents:02}");
        let us = format!("{}.{cents:02}", group_digits(&whole, ','));
        let german = format!("{},{cents:02}", group_digits(&whole, '.'));

        let expected = Money::parse(&ungrouped);
        prop_assert_eq!(Money::parse(&us), expected, "US: {}", us);
        prop_assert_eq!(Money::parse(&german), expected, "German: {}", german);
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

    /// Every validated date formats as `yyyy-mm-dd` and round-trips through the
    /// parser, including the 29th to 31st and the final supported year.
    #[test]
    fn date_round_trip(date in valid_date()) {
        let text = date.to_string();
        prop_assert_eq!(text.len(), 10);
        let parsed = Date::parse(&text);
        prop_assert!(parsed.is_ok(), "re-parsing {text:?} failed: {parsed:?}");
        prop_assert_eq!(parsed.ok(), Some(date));
        prop_assert_eq!(
            Date::parse(&text).ok().map(Date::days_since_epoch),
            Some(date.days_since_epoch())
        );
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
            if let (Ok(total), Ok(net), Some(vat)) = (
                job.total(),
                job.net(),
                job.items
                    .iter()
                    .try_fold(0i64, |sum, item| sum.checked_add(item.vat.cents())),
            ) {
                prop_assert_eq!(total.cents(), net.cents() + vat);
            }
        }
        if let Ok(page) = serde_json::from_value::<letterxpress::PrintJobPage>(value.clone()) {
            prop_assert!(page.jobs.len() <= 1024);
        }
        let _ = letterxpress::EmailJobPage::parse(value.clone(), None);
        let _ = letterxpress::EmailJobSubmission::parse(value);
    }

    /// `PrintJob::pages` is a saturating sum: no list of provider counts wraps
    /// to a smaller number, and no value panics — the failure the audit found
    /// was a debug-build panic and a release-build wrap to a tiny total.
    #[test]
    fn the_page_total_is_a_saturating_sum(
        pages in proptest::collection::vec(any::<u32>(), 0..8)
    ) {
        let job = letterxpress::PrintJob {
            items: pages
                .iter()
                .map(|pages| letterxpress::PrintItem {
                    pages: *pages,
                    ..letterxpress::PrintItem::default()
                })
                .collect(),
            ..letterxpress::PrintJob::default()
        };
        let expected = pages
            .iter()
            .map(|pages| u64::from(*pages))
            .sum::<u64>()
            .min(u64::from(u32::MAX));
        prop_assert_eq!(u64::from(job.pages()), expected, "{:?}", pages);
    }

    /// The bug this guards: whatever the letter-level `registered` key holds, a
    /// letter the specification priced as registered is never echoed as
    /// unregistered. A token that is no code at all is refused; everything else
    /// leaves the priced `r2` in place.
    #[test]
    fn a_priced_registered_letter_is_never_echoed_unregistered(
        extra in proptest::collection::vec(any::<u8>(), 0..1024)
    ) {
        let Ok(extra) = serde_json::from_slice::<serde_json::Value>(&extra) else {
            return Ok(());
        };
        let letter = serde_json::json!({
            "specification": {"pages": 1, "registered": "r2"},
            "registered": extra,
        });
        match letterxpress::PriceEcho::from_envelope(&letter) {
            Err(_) => {}
            Ok(echo) => {
                prop_assert!(echo.registered.is_some(), "the priced r2 was dropped");
                prop_assert_eq!(echo.pages, 1);
            }
        }
    }

    /// A price echo never panics on hostile JSON, and never invents a page
    /// count: when it decodes, `pages` is the number the specification carried.
    #[test]
    fn price_echo_decoding_never_panics_and_never_invents_pages(
        bytes in proptest::collection::vec(any::<u8>(), 0..4096)
    ) {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(());
        };
        if let Ok(echo) = letterxpress::PriceEcho::from_envelope(&value) {
            let spec = value.get("specification").expect("an echo has a specification");
            prop_assert!(spec.is_object(), "an echo needs a specification object");
            prop_assert_eq!(
                u64::from(echo.pages),
                spec.get("pages")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            );
        }
        let _ = serde_json::from_value::<letterxpress::Price>(value);
    }

    /// A flag means the same thing as a number and as its decimal string, and
    /// zero is the only `false` — the rule that used to hold for numbers only.
    #[test]
    fn a_flag_is_zero_or_not_for_numbers_and_strings(value in any::<i64>()) {
        let as_number: letterxpress::PrintJob =
            serde_json::from_str(&format!(r#"{{"id":1,"c4":{value}}}"#))
                .expect("a number flag always decodes");
        let as_string: letterxpress::PrintJob =
            serde_json::from_str(&serde_json::json!({"id": 1, "c4": value.to_string()}).to_string())
                .expect("its string form decodes the same way");

        prop_assert_eq!(as_number.c4, value != 0);
        prop_assert_eq!(as_string.c4, as_number.c4);
    }

    /// A token outside the flag rule is refused, never quietly read as `false`.
    #[test]
    fn a_token_outside_the_flag_rule_is_refused(token in "[^0-9A-Za-z+-]{1,6}") {
        if token.chars().all(char::is_whitespace) {
            // Whitespace alone is "not supplied", like null.
            return Ok(());
        }
        let body = serde_json::json!({"id": 1, "c4": token}).to_string();
        prop_assert!(
            serde_json::from_str::<letterxpress::PrintJob>(&body).is_err(),
            "{token:?} must not decode as a flag"
        );
    }

    /// A text field keeps every scalar spelling the provider could use: numbers
    /// become their `Display` form, strings survive byte for byte, and an id
    /// rendered as an integer or a float decodes to the same digits.
    #[test]
    fn text_fields_keep_the_providers_spelling(
        number in any::<u64>(),
        fraction in any::<f64>(),
        text in ".*",
    ) {
        let job: letterxpress::PrintJob =
            serde_json::from_value(serde_json::json!({"id": number, "notice": number}))
                .expect("a number is a valid id and a valid notice");
        let expected = number.to_string();
        prop_assert_eq!(job.id.as_str(), expected.as_str());
        prop_assert_eq!(job.notice.as_deref(), Some(expected.as_str()));

        let job: letterxpress::PrintJob =
            serde_json::from_value(serde_json::json!({"id": 1, "notice": text}))
                .expect("any string is a notice");
        prop_assert_eq!(job.notice.as_deref(), Some(text.as_str()));

        if fraction.is_finite() {
            let job: letterxpress::PrintJob =
                serde_json::from_value(serde_json::json!({"id": 1, "notice": fraction}))
                    .expect("a finite float is a notice");
            let expected = fraction.to_string();
            prop_assert_eq!(job.notice.as_deref(), Some(expected.as_str()));
        }
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

    /// The registered/shipping rule is enforced from both sides: for every
    /// class and both call orders, the pair is accepted exactly when the class
    /// is not international, and an accepted letter never emits the pair the
    /// provider documents as impossible.
    #[test]
    fn registered_mail_never_meets_international_shipping(
        registered_index in 0usize..2,
        shipping_index in 0usize..4,
        registered_first in any::<bool>(),
    ) {
        let registered = if registered_index == 0 {
            Registered::EinschreibenEinwurf
        } else {
            Registered::Einschreiben
        };
        let shipping = match shipping_index {
            0 => Shipping::National,
            1 => Shipping::International,
            2 => Shipping::Auto,
            // The carry-all spells the same wire value and must be caught too.
            _ => Shipping::Other("International".into()),
        };
        let impossible = matches!(shipping_index, 1 | 3);

        let letter = Letter::new(b"%PDF-1.4\n").expect("valid pdf");
        let built = if registered_first {
            letter
                .with_registered(registered.clone())
                .and_then(|letter| letter.with_shipping(shipping.clone()))
        } else {
            letter
                .with_shipping(shipping.clone())
                .and_then(|letter| letter.with_registered(registered))
        };
        prop_assert_eq!(built.is_ok(), !impossible);
        if let Ok(letter) = built {
            let json = letter.to_json();
            prop_assert!(json.get("registered").is_some());
            prop_assert!(!json["specification"]["shipping"]
                .as_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case("international"));
        }

        // A price quote carries the same pair, so the same rule applies.
        let quote = letterxpress::PriceRequest::new(1)
            .expect("price")
            .with_registered(Registered::Einschreiben)
            .and_then(|request| request.with_shipping(shipping));
        prop_assert_eq!(quote.is_ok(), !impossible);
        if let Ok(quote) = quote {
            prop_assert!(quote.to_json().get("registered").is_some());
        }
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

    /// The recorded helper agrees with `url` as the oracle for every absolute
    /// URL: path plus query, fragment dropped.
    #[test]
    fn path_and_query_matches_the_url_oracle(
        host in "[a-z]{1,20}\\.test",
        port in proptest::option::of(1u16..65535),
        path in "/[a-z0-9/]{0,40}",
        query in proptest::option::of("[a-z0-9=&]{1,30}"),
    ) {
        let mut text = format!("https://{host}");
        if let Some(port) = port {
            text.push(':');
            text.push_str(&port.to_string());
        }
        text.push_str(&path);
        if let Some(query) = &query {
            text.push('?');
            text.push_str(query);
        }
        text.push_str("#fragment");

        let parsed = url::Url::parse(&text).expect("the generated URL is absolute");
        let mut expected = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            expected.push('?');
            expected.push_str(query);
        }

        let recorded = RecordedRequest::new(Method::Get, text, Vec::new(), Vec::new());
        prop_assert_eq!(recorded.path_and_query(), Some(expected));
    }

    /// No input panics, and the result is present exactly when `url` accepts
    /// the text as an absolute URL.
    #[test]
    fn path_and_query_never_panics(text in ".*") {
        let recorded = RecordedRequest::new(Method::Get, text.clone(), Vec::new(), Vec::new());
        prop_assert_eq!(
            recorded.path_and_query().is_some(),
            url::Url::parse(&text).is_ok(),
            "{:?}",
            text
        );
    }

    /// Whatever a `Retry-After` header contains, a `429` never reports more
    /// than the ceiling, and it reports a hint exactly when the header is a
    /// plain count of seconds.
    #[test]
    fn retry_after_headers_are_bounded_and_never_panic(header in any::<String>()) {
        let response = Response::new(
            429,
            [("Retry-After".to_string(), header.clone())],
            Vec::new(),
        );
        let error = letterxpress::parse_envelope("GET /v3/balance", response).expect_err("429");

        let expected = header
            .trim()
            .parse::<u64>()
            .ok()
            .map(|seconds| std::time::Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
        prop_assert_eq!(error.retry_after(), expected, "{:?}", header);
        if let Some(wait) = error.retry_after() {
            prop_assert!(wait <= MAX_RETRY_AFTER, "{:?}", header);
        }
        // The wording carries no number above the ceiling either.
        prop_assert!(
            !error
                .to_string()
                .contains(&format!("{}s", MAX_RETRY_AFTER.as_secs() + 1)),
            "{:?}: {}",
            header,
            error
        );
    }

    /// A rate limit built by hand is bounded as well, and the printed wait is
    /// the one the accessor reports — a caller that logs the error and a caller
    /// that reads `retry_after()` cannot disagree.
    #[test]
    fn a_hand_built_rate_limit_advertises_the_bounded_wait(
        seconds in any::<u64>(),
        nanos in 0u32..1_000_000_000,
    ) {
        let raw = std::time::Duration::new(seconds, nanos);
        let error = letterxpress::Error::RateLimited {
            retry_after: Some(raw),
        };
        let bounded = raw.min(MAX_RETRY_AFTER);

        prop_assert_eq!(error.retry_after(), Some(bounded));
        prop_assert!(
            error
                .to_string()
                .contains(&format!("retry after {}s", bounded.as_secs())),
            "{}",
            error
        );
    }

    /// `Retry-After` round-trips through `Display`: what the wording shows is
    /// what the caller can read back, and the seconds are `Duration::as_secs` —
    /// a sub-second remainder is truncated, never rounded up into a retry that
    /// would hit the limit again. A value above the ceiling is shown as the
    /// wait a caller may act on, naming what the provider claimed.
    #[test]
    fn the_retry_after_wording_round_trips(
        secs in any::<u64>(),
        nanos in 0u32..1_000_000_000,
    ) {
        let raw = std::time::Duration::new(secs, nanos);
        let bounded = raw.min(MAX_RETRY_AFTER);
        let rendered = letterxpress::Error::RateLimited {
            retry_after: Some(raw),
        }
        .to_string();
        let expected = if bounded == raw {
            format!(
                "letterxpress rate limit reached, retry after {}s",
                bounded.as_secs()
            )
        } else {
            format!(
                "letterxpress rate limit reached, retry after {}s (capped from {}s)",
                bounded.as_secs(),
                raw.as_secs()
            )
        };
        prop_assert_eq!(&rendered, &expected);

        // The number the wording leads with is the wait `retry_after()` reports.
        let parsed = rendered
            .strip_prefix("letterxpress rate limit reached, retry after ")
            .and_then(|tail| tail.split('s').next())
            .and_then(|digits| digits.parse::<u64>().ok());
        prop_assert_eq!(parsed, Some(bounded.as_secs()));
    }

    /// `Error::status()` reports exactly what the structured kind carries, for
    /// every possible code: reconciliation never has to parse the message to
    /// learn the status, and the two transport-shaped kinds never invent one.
    #[test]
    fn an_ambiguous_status_is_the_kind_and_nothing_else(status in any::<u16>()) {
        let upstream = Error::Ambiguous {
            kind: AmbiguousKind::Upstream { status },
            message: String::new(),
        };
        prop_assert_eq!(upstream.status(), Some(status));
        prop_assert_eq!(
            upstream.status(),
            AmbiguousKind::Upstream { status }.status()
        );

        for kind in [AmbiguousKind::Timeout, AmbiguousKind::Io] {
            let error = Error::Ambiguous {
                kind,
                message: String::new(),
            };
            prop_assert_eq!(error.status(), None);
            prop_assert_eq!(error.status(), kind.status());
        }
    }

    /// The stance belongs to the variant — plus the status or transport kind it
    /// carries — never to the prose. Two errors of the same variant, built from
    /// arbitrary and different text, answer identically; rendering either never
    /// panics; and no variant is ever both retryable and ambiguous, because a
    /// retry of a possibly-processed submit would bill a second letter.
    #[test]
    fn the_stance_belongs_to_the_variant_not_the_payload(
        first in ".*",
        second in ".*",
        status in any::<u16>(),
        kind in prop_oneof![
            Just(TransportKind::Connect),
            Just(TransportKind::Timeout),
            Just(TransportKind::Io),
            Just(TransportKind::Protocol),
        ],
    ) {
        let left = every_variant(&first, status, kind);
        let right = every_variant(&second, status, kind);
        prop_assert_eq!(left.len(), right.len());

        for (left, right) in left.iter().zip(right.iter()) {
            prop_assert_eq!(stance(left), stance(right), "{} / {}", left, right);
            prop_assert!(!left.to_string().is_empty());
            prop_assert!(!right.to_string().is_empty());
            prop_assert!(
                !(left.is_retryable() && left.is_ambiguous()),
                "{}: a possible duplicate is never retryable",
                left
            );
            prop_assert!(
                !(left.is_retryable_for(true) && left.is_ambiguous()),
                "{}: a possible duplicate is never retryable, not even for a read",
                left
            );
            if left.is_retryable() {
                prop_assert!(
                    left.is_retryable_for(true),
                    "{}: an idempotent request is never worse off than a submit",
                    left
                );
            }
        }
    }
}

fn group_digits(digits: &str, separator: char) -> String {
    let first_group = digits.len() % 3;
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && index % 3 == first_group {
            grouped.push(separator);
        }
        grouped.push(digit);
    }
    grouped
}

fn is_valid_credential_token(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|character| character.is_ascii_graphic() || character == ' ')
}

#[test]
fn the_retry_after_wording_truncates_at_the_second_boundary() {
    use std::time::Duration;

    let render = |retry_after: Option<Duration>| {
        letterxpress::Error::RateLimited { retry_after }.to_string()
    };

    // Just inside the limit: 999 999 999 ns still reports whole seconds, so it
    // is "0s" and not "1s".
    assert_eq!(
        render(Some(Duration::new(0, 999_999_999))),
        "letterxpress rate limit reached, retry after 0s"
    );
    // Exactly at it.
    assert_eq!(
        render(Some(Duration::new(1, 0))),
        "letterxpress rate limit reached, retry after 1s"
    );
    // And just past it: the remainder is truncated, not rounded.
    assert_eq!(
        render(Some(Duration::new(1, 1))),
        "letterxpress rate limit reached, retry after 1s"
    );
    // The largest representable duration renders without wrapping — at the
    // ceiling, naming the wait the provider claimed.
    assert_eq!(
        render(Some(Duration::new(u64::MAX, 999_999_999))),
        format!(
            "letterxpress rate limit reached, retry after {}s (capped from {}s)",
            MAX_RETRY_AFTER.as_secs(),
            u64::MAX
        )
    );
    // No `Retry-After` at all: no suffix, and in particular not "0s".
    assert_eq!(render(None), "letterxpress rate limit reached");
}

#[test]
fn a_carried_status_is_reported_verbatim_at_both_ends_of_the_range() {
    // The boundary pair for the `status` column: the smallest and largest code
    // a `u16` can hold, plus the documented 4xx/5xx edges in between, come back
    // unchanged rather than being clamped or mapped.
    for status in [0, 1, 399, 400, 499, 500, 599, 600, u16::MAX] {
        for error in [
            Error::Rejected {
                status,
                message: "rejected".into(),
            },
            Error::Upstream {
                status,
                message: "upstream".into(),
            },
            Error::UnexpectedStatus {
                status,
                endpoint: "GET /v3/balance",
                message: "redirect".into(),
            },
        ] {
            assert_eq!(error.status(), Some(status), "{status}: {error}");
            assert!(!error.is_retryable(), "{status}: {error}");
            assert!(!error.is_ambiguous(), "{status}: {error}");
        }
    }
}

#[test]
fn unreadable_json_and_the_wrong_shape_share_one_stance() {
    // `Error::Json` is reached through `#[from] serde_json::Error` as well as by
    // construction; both entries answer like the table's `Json` row, which is
    // the same stance as `UnexpectedShape`: the same bytes fail the same way,
    // so neither invites a retry.
    let constructed = Error::Json {
        source: serde_json::Error::io(std::io::Error::from(std::io::ErrorKind::InvalidData)),
    };
    let converted = Error::from(serde_json::Error::io(std::io::Error::from(
        std::io::ErrorKind::InvalidData,
    )));
    let shape = Error::UnexpectedShape {
        endpoint: "GET /v3/balance",
        message: "expected an object, got an array".into(),
        source: None,
    };

    assert_eq!(stance(&constructed), stance(&converted));
    assert_eq!(stance(&converted), stance(&shape));
    assert_eq!(stance(&converted), (false, false, false, false, None));
    assert!(std::error::Error::source(&converted).is_some());
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

/// Any failure whose payload the client can vary: the status it carries, the
/// transport kind that produced it, and the `Retry-After` a rate limit saw.
fn arbitrary_errors(kind: TransportKind, status: u16, retry_after: Option<Duration>) -> Vec<Error> {
    vec![
        Error::Upstream {
            status,
            message: String::new(),
        },
        Error::Rejected {
            status,
            message: String::new(),
        },
        Error::UnexpectedStatus {
            status,
            endpoint: "GET /v3/balance",
            message: String::new(),
        },
        Error::RateLimited { retry_after },
        Error::Transport {
            kind,
            source: Box::new(std::io::Error::other("")),
        },
        Error::NotFound {
            message: String::new(),
        },
        Error::Ambiguous {
            kind: AmbiguousKind::Upstream { status },
            message: String::new(),
        },
    ]
}

/// Every [`TransportKind`], listed rather than sampled from a `Debug` string.
fn transport_kind_strategy() -> impl Strategy<Value = TransportKind> {
    prop_oneof![
        Just(TransportKind::Connect),
        Just(TransportKind::Timeout),
        Just(TransportKind::Io),
        Just(TransportKind::Protocol),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The retry decision is a function of the error *and* the request, and the
    /// two are consistent for every status, kind and `Retry-After` the provider
    /// can produce:
    ///
    /// * `is_retryable()` is the submit answer, never the optimistic one;
    /// * an idempotent request is never worse off than a submit;
    /// * nothing retryable is ever ambiguous;
    /// * `retry_after()` is exactly the hint the rate limit carried, and nothing
    ///   else carries one.
    #[test]
    fn the_retry_decision_never_outruns_the_request_it_belongs_to(
        kind in transport_kind_strategy(),
        status in 100u16..=599,
        retry_after in proptest::option::of(0u64..=86_400u64),
    ) {
        let retry_after = retry_after.map(Duration::from_secs);
        for error in arbitrary_errors(kind, status, retry_after) {
            let submit = error.is_retryable_for(false);
            let read = error.is_retryable_for(true);

            prop_assert_eq!(error.is_retryable(), submit, "{:?}", error);
            if submit {
                prop_assert!(read, "{:?}: a read is never worse off than a submit", error);
            }
            prop_assert!(
                !(submit && error.is_ambiguous()),
                "{:?}: a possible duplicate is never a retry",
                error
            );
            prop_assert!(
                !(read && error.is_ambiguous()),
                "{:?}: a possible duplicate is never a retry, not even for a read",
                error
            );

            if let Error::RateLimited { .. } = &error {
                prop_assert_eq!(
                    error.retry_after(),
                    retry_after.map(|wait| wait.min(MAX_RETRY_AFTER))
                );
            } else {
                prop_assert_eq!(error.retry_after(), None, "{:?}", error);
            }
        }
    }
}
