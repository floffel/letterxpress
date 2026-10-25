//! Edge matrix: the just-barely-valid value, the invalid value on the other
//! side of the boundary, and every hostile response the provider (or a proxy in
//! front of it) can produce.

mod common;

use common::{client_with, fixture, pdf};
use letterxpress::transport::{MockTransport, Response, Transport};
use letterxpress::{
    Client, Credentials, Date, EmailDelivery, EmailOption, Error, JobId, JobStatus, Letter, Mode,
    Money, PrintJob, Registered, Shipping, TransportKind, MAX_NOTICE_CHARS, MAX_PDF_BYTES,
};

fn client_for(status: u16, body: &str) -> Client<MockTransport> {
    client_with(vec![Response::json_response(status, body.to_string())])
}

// ---------------------------------------------------------------------------
// Response boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_success_body_is_a_missing_data_error() {
    let client = client_with(vec![Response::json_response(200, "")]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::MissingField { field: "data", .. }));
}

#[tokio::test]
async fn whitespace_only_body_behaves_like_an_empty_one() {
    let client = client_with(vec![Response::json_response(200, "   \n\t ")]);
    assert!(matches!(
        client.balance().await.expect_err("must fail"),
        Error::MissingField { .. }
    ));
}

#[tokio::test]
async fn html_error_page_still_reports_the_http_status() {
    let client = client_with(vec![Response::new(
        503,
        [("Content-Type".to_string(), "text/html".to_string())],
        b"<html><body>Service Unavailable</body></html>".to_vec(),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Upstream { status: 503, .. }));
    assert!(error.to_string().contains("Service Unavailable"));
}

#[tokio::test]
async fn a_5xx_body_is_truncated_before_it_reaches_an_error_message() {
    let huge = "x".repeat(100_000);
    let client = client_with(vec![Response::new(
        500,
        std::iter::empty(),
        huge.into_bytes(),
    )]);
    let error = client.balance().await.expect_err("must fail");
    let rendered = error.to_string();
    assert!(
        rendered.len() < 700,
        "error message must be bounded: {}",
        rendered.len()
    );
    assert!(rendered.ends_with('…'));
}

#[tokio::test]
async fn malformed_json_on_success_is_a_json_error() {
    let client = client_with(vec![Response::json_response(
        200,
        fixture("synthetic_malformed.json"),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Json { .. }));
    // The serde error keeps its position information as the source chain.
    assert!(std::error::Error::source(&error).is_some());
}

#[tokio::test]
async fn duplicate_keys_take_the_last_value() {
    // JSON with repeated keys is invalid-but-common; `serde_json` keeps the
    // last occurrence, which is what every other JSON parser does too.
    let client = client_with(vec![Response::json_response(
        200,
        fixture("synthetic_duplicate_keys.json"),
    )]);
    let balance = client.balance().await.expect("balance");
    assert_eq!(balance.balance.cents(), 250);
    assert_eq!(balance.currency.as_deref(), Some("CHF"));

    // The same rule applies to the envelope's own status: the last one wins, so
    // a duplicated error status is still an error, not a silent success.
    let client = client_for(200, r#"{"status":200,"status":500,"message":"boom"}"#);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Upstream { status: 500, .. }));
}

#[tokio::test]
async fn unauthorized_without_a_status_field_is_still_unauthorized() {
    let client = client_with(vec![Response::json_response(
        401,
        fixture("unauthorized.json"),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Unauthorized { .. }));
}

#[tokio::test]
async fn body_level_status_wins_when_http_says_ok() {
    let client = client_for(200, r#"{"status":400,"message":"Bad Request"}"#);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Rejected { status: 400, .. }));
}

#[tokio::test]
async fn body_level_500_is_an_upstream_error() {
    let client = client_for(200, r#"{"status":500,"message":"Internal Server Error"}"#);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::Upstream { status: 500, .. }));
}

#[tokio::test]
async fn forbidden_not_found_and_method_not_allowed_are_distinct() {
    let cases = [
        (403, "synthetic_forbidden.json"),
        (404, "synthetic_not_found.json"),
    ];
    for (status, name) in cases {
        let client = client_with(vec![Response::json_response(status, fixture(name))]);
        let error = client.balance().await.expect_err("must fail");
        assert_eq!(error.status(), Some(status));
        assert!(!error.is_retryable());
    }

    let client = client_for(405, r#"{"message":"Method Not Allowed"}"#);
    assert!(matches!(
        client.balance().await.expect_err("must fail"),
        Error::MethodNotAllowed { .. }
    ));
}

#[tokio::test]
async fn rate_limit_is_retryable_and_carries_retry_after() {
    let client = client_with(vec![Response::new(
        429,
        [("Retry-After".to_string(), "30".to_string())],
        fixture("synthetic_rate_limited.json").into_bytes(),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(error.is_retryable());
    assert_eq!(error.status(), Some(429));
    match error {
        Error::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_without_a_retry_after_header_still_maps() {
    let client = client_for(429, r#"{"message":"Too Many Requests"}"#);
    match client.balance().await.expect_err("must fail") {
        Error::RateLimited { retry_after } => assert_eq!(retry_after, None),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn an_unfollowed_redirect_is_reported_as_an_unexpected_shape() {
    let client = client_with(vec![Response::new(
        302,
        [(
            "Location".to_string(),
            "https://elsewhere.test/".to_string(),
        )],
        Vec::new(),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(error, Error::UnexpectedShape { .. }));
    assert!(error.to_string().contains("302"));
}

#[tokio::test]
async fn missing_required_field_is_an_unexpected_shape_with_the_endpoint_named() {
    let client = client_for(
        200,
        r#"{"status":200,"message":"OK","data":{"currency":"EUR"}}"#,
    );
    // `Balance` has no required field, so this one succeeds with zero credit.
    let balance = client.balance().await.expect("balance");
    assert_eq!(balance.cents(), 0);

    let client = client_for(
        200,
        r#"{"status":200,"message":"OK","data":{"status":"queue"}}"#,
    );
    let error = client
        .print_job(&JobId::new("1").expect("id"))
        .await
        .expect_err("a job without an id cannot be decoded");
    assert!(matches!(
        error,
        Error::UnexpectedShape {
            endpoint: "GET /v3/printjobs/{id}",
            ..
        }
    ));
}

#[tokio::test]
async fn null_data_is_a_missing_field_not_a_panic() {
    let client = client_for(200, r#"{"status":200,"message":"OK","data":null}"#);
    assert!(matches!(
        client.balance().await.expect_err("must fail"),
        Error::MissingField { .. }
    ));
}

#[tokio::test]
async fn unknown_status_values_are_carried_not_rejected() {
    let client = client_for(
        200,
        r#"{"status":200,"data":{"id":1,"status":"awaiting_carrier_scan","items":[]}}"#,
    );
    let job: PrintJob = client
        .print_job(&JobId::new("1").expect("id"))
        .await
        .expect("job");
    assert_eq!(job.status, JobStatus::Other("awaiting_carrier_scan".into()));
    assert!(
        !job.status.is_terminal(),
        "an unknown state is never assumed final"
    );
}

#[tokio::test]
async fn camel_case_keys_and_string_numbers_decode() {
    let client = client_with(vec![Response::json_response(
        200,
        fixture("synthetic_camel_case.json"),
    )]);
    let job = client
        .print_job(&JobId::new("6035138").expect("id"))
        .await
        .expect("job");

    assert_eq!(job.id.as_str(), "6035138", "an id may arrive as a string");
    assert!(!job.bank_form);
    assert_eq!(
        job.filename_original.as_deref(),
        Some("Rechnung-123456.pdf")
    );
    assert_eq!(
        job.items[0].net.cents(),
        67,
        "an amount may arrive as a string"
    );
    assert_eq!(
        job.items[0].status,
        JobStatus::Sent,
        "enum values are case-insensitive"
    );
    assert_eq!(job.items[0].tracking_code.as_deref(), Some("RC123456789DE"));
}

#[tokio::test]
async fn unicode_addresses_survive_unchanged() {
    let client = client_with(vec![Response::json_response(
        200,
        fixture("synthetic_unicode_address.json"),
    )]);
    let job = client
        .print_job(&JobId::new("7000001").expect("id"))
        .await
        .expect("job");

    assert_eq!(
        job.items[0].address.as_deref(),
        Some("Jürgen Großmann, Müllerstraße 12a, 85049 Ingolstadt, Deutschland 📮")
    );
    assert_eq!(
        job.filename_original.as_deref(),
        Some("Straßenfest 会议.pdf")
    );
    assert_eq!(job.total().cents(), 250);
}

#[tokio::test]
async fn a_job_list_without_pagination_still_decodes() {
    let client = client_for(
        200,
        r#"{"status":200,"data":{"printjobs":[{"id":1,"items":[]}]}}"#,
    );
    let page = client
        .print_jobs(&letterxpress::PrintJobQuery::new())
        .await
        .expect("page");
    assert_eq!(page.jobs.len(), 1);
    assert!(page.pagination.is_none());
}

// ---------------------------------------------------------------------------
// Transport boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_submit_timeout_is_ambiguous_and_sent_once() {
    let recorder = letterxpress::transport::RecordingTransport::new(MockTransport::new([Err(
        Error::Transport {
            kind: TransportKind::Timeout,
            message: "operation timed out".into(),
        },
    )]));
    let client = Client::new(
        recorder.clone(),
        Credentials::new("kunde", "key", Mode::Live).expect("credentials"),
    );

    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(error.is_ambiguous());
    assert!(!error.is_retryable());
    assert_eq!(
        recorder.requests().len(),
        1,
        "the client never retries a submit"
    );
}

#[tokio::test]
async fn a_submit_connection_failure_is_not_ambiguous() {
    let client = Client::new(
        MockTransport::new([Err(Error::Transport {
            kind: TransportKind::Connect,
            message: "dns failure".into(),
        })]),
        Credentials::new("kunde", "key", Mode::Live).expect("credentials"),
    );

    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(!error.is_ambiguous(), "nothing was written");
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_read_timeout_stays_retryable() {
    let client = Client::new(
        MockTransport::new([Err(Error::Transport {
            kind: TransportKind::Timeout,
            message: "timed out".into(),
        })]),
        Credentials::new("kunde", "key", Mode::Test).expect("credentials"),
    );
    let error = client.balance().await.expect_err("must fail");
    assert!(!error.is_ambiguous());
    assert!(error.is_retryable());
}

#[tokio::test]
async fn a_submit_5xx_is_ambiguous_but_a_read_5xx_is_not() {
    let client = client_for(500, r#"{"message":"Internal Server Error"}"#);
    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");
    assert!(error.is_ambiguous());
    assert!(error.to_string().contains("http 500"));

    let client = client_for(500, r#"{"message":"Internal Server Error"}"#);
    let error = client.balance().await.expect_err("must fail");
    assert!(!error.is_ambiguous());
    assert!(matches!(error, Error::Upstream { status: 500, .. }));
}

#[tokio::test]
async fn an_exhausted_mock_is_a_protocol_error_not_a_panic() {
    let client = Client::new(
        MockTransport::default(),
        Credentials::new("kunde", "key", Mode::Test).expect("credentials"),
    );
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(
        error,
        Error::Transport {
            kind: TransportKind::Protocol,
            ..
        }
    ));
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn a_custom_transport_can_answer_without_a_socket() {
    struct Fixed;

    impl Transport for Fixed {
        async fn execute(
            &self,
            _request: letterxpress::transport::Request,
        ) -> letterxpress::Result<Response> {
            std::future::ready(()).await;
            Ok(Response::json_response(
                200,
                r#"{"status":200,"message":"OK","data":{"balance":12.5,"currency":"EUR"}}"#,
            ))
        }
    }

    let client = Client::new(
        Fixed,
        Credentials::new("kunde", "key", Mode::Test).expect("credentials"),
    );
    assert_eq!(client.balance().await.expect("balance").cents(), 1250);
}

// ---------------------------------------------------------------------------
// Request boundaries
// ---------------------------------------------------------------------------

#[test]
fn empty_and_non_pdf_payloads_are_rejected_before_the_wire() {
    assert!(matches!(
        Letter::new(b""),
        Err(Error::Validation {
            field: "letter.base64_file",
            ..
        })
    ));
    assert!(matches!(
        Letter::new(b"not a pdf"),
        Err(Error::Validation { .. })
    ));
    assert!(
        Letter::new(b"%PDF-").is_ok(),
        "the magic is the only shape check"
    );
}

#[test]
fn the_documented_50_mb_ceiling_is_enforced_before_encoding() {
    let oversized = vec![b'x'; MAX_PDF_BYTES + 1];
    let mut pdf_bytes = b"%PDF-1.4\n".to_vec();
    pdf_bytes.extend_from_slice(&oversized);
    let error = Letter::new(&pdf_bytes).expect_err("must fail");
    assert!(matches!(
        error,
        Error::TooLarge {
            field: "letter.base64_file",
            ..
        }
    ));
}

#[test]
fn attachments_share_the_request_budget() {
    // Two attachments that are each fine but together overflow the budget.
    let chunk = vec![b'x'; MAX_PDF_BYTES / 2 + 10];
    let mut attachment = b"%PDF-1.4\n".to_vec();
    attachment.extend_from_slice(&chunk);

    let letter = Letter::new(&pdf()).expect("letter");
    let letter = letter
        .with_attachment(&attachment)
        .expect("first attachment");
    let error = letter
        .with_attachment(&attachment)
        .expect_err("the budget is shared");
    assert!(matches!(error, Error::TooLarge { .. }));
}

#[test]
fn notice_length_is_counted_in_characters_not_bytes() {
    let max = "ä".repeat(MAX_NOTICE_CHARS);
    assert!(Letter::new(&pdf())
        .expect("letter")
        .with_notice(max)
        .is_ok());

    let too_long = "ä".repeat(MAX_NOTICE_CHARS + 1);
    assert!(matches!(
        Letter::new(&pdf()).expect("letter").with_notice(too_long),
        Err(Error::TooLarge {
            max: MAX_NOTICE_CHARS,
            ..
        })
    ));

    assert!(Letter::new(&pdf())
        .expect("letter")
        .with_notice("")
        .is_err());
    assert!(Letter::new(&pdf())
        .expect("letter")
        .with_notice("has\u{0}nul")
        .is_err());
}

#[test]
fn filenames_cannot_smuggle_a_path() {
    let letter = Letter::new(&pdf()).expect("letter");
    for bad in [
        "../etc/passwd",
        "a/b.pdf",
        "a\\b.pdf",
        "line\nbreak.pdf",
        "",
    ] {
        assert!(
            letter.clone().with_filename_original(bad).is_err(),
            "{bad:?}"
        );
    }
    assert!(letter.with_filename_original("Rechnung-123456.pdf").is_ok());
}

#[test]
fn registered_mail_is_rejected_internationally_instead_of_being_dropped() {
    let international = Letter::new(&pdf())
        .expect("letter")
        .with_shipping(Shipping::International);
    let error = international
        .with_registered(Registered::EinschreibenEinwurf)
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::Validation {
            field: "letter.registered",
            ..
        }
    ));

    let international = letterxpress::PriceRequest::new(1)
        .expect("price")
        .with_shipping(Shipping::International);
    assert!(international
        .with_registered(Registered::Einschreiben)
        .is_err());
}

#[test]
fn dispatch_dates_must_be_in_the_future() {
    let today = Date::new(2026, 10, 24).expect("today");
    let letter = Letter::new(&pdf()).expect("letter");

    assert!(letter.clone().with_dispatch_date(today, today).is_err());
    assert!(letter
        .clone()
        .with_dispatch_date(Date::new(2026, 10, 23).expect("past"), today)
        .is_err());
    assert!(letter
        .with_dispatch_date(Date::new(2026, 10, 25).expect("future"), today)
        .is_ok());
}

#[test]
fn date_parsing_rejects_every_malformed_shape() {
    for bad in [
        "",
        "2026-1-1",
        "2026-01-1",
        "26-01-01",
        "2026/01/01",
        "2026-13-01",
        "2026-00-01",
        "2026-01-00",
        "2026-02-30",
        "0000-01-01",
    ] {
        assert!(Date::parse(bad).is_err(), "{bad:?} must be rejected");
    }
    assert_eq!(
        Date::parse("2024-02-29").expect("leap day").to_string(),
        "2024-02-29"
    );
    assert_eq!(
        Date::parse("2000-02-29")
            .expect("century leap day")
            .to_string(),
        "2000-02-29"
    );
    assert!(
        Date::parse("1900-02-29").is_err(),
        "1900 is not a leap year"
    );
}

#[test]
fn email_letter_and_serial_letter_are_mutually_exclusive() {
    let email = EmailDelivery::new("a@b.de", EmailOption::MailDirect).expect("email");
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(email)
        .expect("email delivery");
    assert!(letter
        .with_separator(letterxpress::Separator::after_pages(2).expect("separator"))
        .is_err());

    let serial = Letter::new(&pdf())
        .expect("letter")
        .with_separator(letterxpress::Separator::after_pages(2).expect("separator"))
        .expect("separator");
    assert!(serial
        .with_email(EmailDelivery::new("a@b.de", EmailOption::MailDirect).expect("email"))
        .is_err());
}

#[test]
fn separators_reject_zero_pages_and_empty_keywords() {
    assert!(letterxpress::Separator::after_pages(0).is_err());
    assert!(letterxpress::Separator::after_pages(u32::MAX).is_ok());
    assert!(letterxpress::Separator::at_keyword("").is_err());
    assert!(letterxpress::Separator::at_keyword("   ").is_err());
    assert!(letterxpress::Separator::at_keyword("a\u{0}b").is_err());
}

#[test]
fn bank_form_fields_respect_the_documented_limits() {
    use letterxpress::{BankForm, MAX_IBAN_CHARS, MAX_PAYEE_CHARS};

    assert!(BankForm::generated(
        "A".repeat(MAX_PAYEE_CHARS + 1),
        "DE12500105170648489890",
        "INGDDEFFXXX",
        "81,00",
        "Rechnung"
    )
    .is_err());

    assert!(BankForm::generated(
        "A",
        "D".repeat(MAX_IBAN_CHARS + 1),
        "INGDDEFFXXX",
        "81,00",
        "Rechnung"
    )
    .is_err());

    assert!(
        BankForm::generated(
            "A",
            "DE12500105170648489890",
            "INGDDEFF",
            "81,00",
            "Rechnung"
        )
        .is_ok(),
        "an 8-character BIC is valid"
    );
    assert!(BankForm::generated("A", "DE12500105170648489890", "INGDDEFFXXX", "81", "R").is_ok());
    assert!(
        BankForm::generated("A", "DE12500105170648489890", "INGDDEFFXXX", "81,00", "").is_err(),
        "an empty purpose is rejected"
    );
    assert!(
        BankForm::generated(
            "A",
            "DE12500105170648489890",
            "INGDDEFFXXX",
            "8.1.0",
            "Rechnung"
        )
        .is_err(),
        "only one decimal separator is allowed"
    );
    assert!(BankForm::generated("", "DE12500105170648489890", "INGDDEFFXXX", "1", "R").is_err());
    assert!(BankForm::generated("A", "DE-1250", "INGDDEFFXXX", "1", "R").is_err());
}

#[test]
fn credentials_fail_closed_and_never_leak_in_debug() {
    assert!(Credentials::new("", "key", Mode::Test).is_err());
    assert!(Credentials::new("user", "", Mode::Test).is_err());
    assert!(Credentials::new("   ", "key", Mode::Test).is_err());
    assert!(Credentials::new("user", "\t", Mode::Test).is_err());
    assert!(Credentials::new("us\u{0}er", "key", Mode::Test).is_err());
    assert!(Credentials::new("u".repeat(257), "key", Mode::Test).is_err());
    assert!(Credentials::new("u", "k".repeat(513), Mode::Test).is_err());

    let credentials = Credentials::new(" kunde ", " topsecret ", Mode::Live).expect("credentials");
    assert_eq!(
        credentials.username(),
        "kunde",
        "surrounding space is trimmed"
    );
    let debug = format!("{credentials:?}");
    assert!(!debug.contains("topsecret"));
    assert!(!credentials.auth_json().to_string().contains("<redacted>"));
    assert_eq!(credentials.auth_json()["apikey"], "topsecret");
}

#[test]
fn the_base_url_must_be_https_without_userinfo() {
    let client = Client::new(
        MockTransport::default(),
        Credentials::new("u", "k", Mode::Test).expect("credentials"),
    );

    for bad in [
        "http://api.letterxpress.de/v3",
        "https://user:pass@api.letterxpress.de/v3",
        "https://api.letterxpress.de/v3?x=1",
        "https://api.letterxpress.de/v3#frag",
        "https://",
        "https://api.letterxpress .de/v3",
    ] {
        assert!(client.clone().with_base_url(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        client
            .with_base_url("https://api.letterxpress.de/v3/")
            .expect("valid")
            .base_url(),
        "https://api.letterxpress.de/v3"
    );
}

#[test]
fn timeouts_and_pages_must_be_positive() {
    let client = Client::new(
        MockTransport::default(),
        Credentials::new("u", "k", Mode::Test).expect("credentials"),
    );
    assert!(client
        .clone()
        .with_timeout(std::time::Duration::ZERO)
        .is_err());
    assert!(client
        .with_timeout(std::time::Duration::from_millis(1))
        .is_ok());

    assert!(letterxpress::PrintJobQuery::new().with_page(0).is_err());
    assert!(letterxpress::EmailJobQuery::new().with_page(0).is_err());
    assert!(letterxpress::TransactionQuery::new().with_page(0).is_err());
    assert!(letterxpress::PriceRequest::new(0).is_err());
    assert!(letterxpress::PriceRequest::new(10_001).is_err());
}

#[test]
fn job_ids_reject_path_separators_and_overlong_values() {
    assert!(JobId::new("").is_err());
    assert!(JobId::new("../1").is_err());
    assert!(JobId::new("1/2").is_err());
    assert!(JobId::new("1".repeat(33)).is_err());
    assert_eq!(JobId::new("6035138").expect("id").as_u64(), Some(6_035_138));
    assert_eq!(JobId::new("abc").expect("id").as_u64(), None);
}

#[test]
fn journal_search_and_range_are_validated() {
    assert!(letterxpress::JournalFilter::new().with_search("").is_err());
    assert!(letterxpress::JournalFilter::new()
        .with_search("x".repeat(256))
        .is_err());
    let start = Date::new(2026, 2, 1).expect("start");
    assert!(letterxpress::JournalFilter::new()
        .with_range(start, start)
        .is_ok());
    assert!(letterxpress::JournalFilter::new()
        .with_range(start, Date::new(2026, 1, 31).expect("end"))
        .is_err());
}

#[test]
fn money_helpers_are_stable_at_the_boundaries() {
    assert_eq!(Money::from_cents(0).cents(), 0);
    assert_eq!(Money::from_cents(-1).cents(), -1);
    assert_eq!(Money::from_euros(f64::NAN).cents(), 0, "no panic on NaN");
    assert_eq!(
        Money::from_euros(f64::INFINITY).cents(),
        i64::MAX,
        "saturating"
    );
    assert_eq!(
        Money::from_euros(0.005).cents(),
        1,
        "rounds half away from zero"
    );
    assert_eq!(Money::from_euros(1_000_000.0).to_string(), "1000000.00");
}

#[test]
fn invoice_pdf_and_journal_csv_report_missing_payloads() {
    let document: letterxpress::InvoiceDocument =
        serde_json::from_str(r#"{"id":1,"base64_data":null}"#).expect("invoice");
    assert!(matches!(document.pdf(), Err(Error::MissingField { .. })));

    let document: letterxpress::InvoiceDocument =
        serde_json::from_str(r#"{"id":1,"base64_data":"!!!!"}"#).expect("invoice");
    assert!(matches!(document.pdf(), Err(Error::Base64 { .. })));

    let journal: letterxpress::Journal =
        serde_json::from_str(r#"{"csv_base64":"////"}"#).expect("journal");
    assert!(matches!(journal.csv(), Err(Error::Utf8 { .. })));
}

#[tokio::test]
async fn a_successful_reply_with_an_empty_data_object_is_not_an_error() {
    let client = client_for(200, r#"{"status":200,"message":"OK","data":{}}"#);
    assert_eq!(client.balance().await.expect("balance").cents(), 0);
}

#[tokio::test]
async fn email_job_submission_rejects_an_unknown_shape() {
    let client = client_for(200, r#"{"status":200,"data":{"nothing":"here"}}"#);
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(EmailDelivery::new("a@b.de", EmailOption::MailDirect).expect("email"))
        .expect("email delivery");
    let error = client
        .submit_email_job(&letter)
        .await
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::UnexpectedShape {
            endpoint: "POST /v3/emailjobs",
            ..
        }
    ));
}
