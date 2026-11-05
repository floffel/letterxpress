//! Edge matrix: the just-barely-valid value, the invalid value on the other
//! side of the boundary, and every hostile response the provider (or a proxy in
//! front of it) can produce.

mod common;

use common::{client_with, fixture, pdf};
use letterxpress::transport::{Method, MockTransport, Response, Transport};
use letterxpress::{
    AmbiguousKind, Client, Credentials, Date, EmailDelivery, EmailOption, Error, JobId, JobStatus,
    Letter, Mode, Money, MoneyError, PrintJob, Registered, Shipping, TransportKind,
    MAX_NOTICE_CHARS, MAX_PDF_BYTES, MAX_RETRY_AFTER,
};

fn client_for(status: u16, body: &str) -> Client<MockTransport> {
    client_with(vec![Response::json_response(status, body.to_string())])
}

fn client_for_with_retry_after(status: u16, body: String) -> Client<MockTransport> {
    client_with(vec![Response::new(
        status,
        [("Retry-After".to_string(), "30".to_string())],
        body.into_bytes(),
    )])
}

fn expected_status_error(status: u16) -> Error {
    match status {
        401 => Error::Unauthorized {
            message: String::new(),
        },
        403 => Error::Forbidden {
            message: String::new(),
        },
        404 => Error::NotFound {
            message: String::new(),
        },
        405 => Error::MethodNotAllowed {
            message: String::new(),
        },
        429 => Error::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(30)),
        },
        500..=599 => Error::Upstream {
            status,
            message: String::new(),
        },
        400..=499 => Error::Rejected {
            status,
            message: String::new(),
        },
        _ => Error::UnexpectedShape {
            endpoint: "GET /v3/balance",
            message: String::new(),
            source: None,
        },
    }
}

fn assert_same_classification(actual: &Error, expected: &Error, context: &str) {
    assert_eq!(
        std::mem::discriminant(actual),
        std::mem::discriminant(expected),
        "{context}: {actual:?} != {expected:?}"
    );
    assert_eq!(actual.status(), expected.status(), "{context}: status");
    assert_eq!(
        actual.is_retryable(),
        expected.is_retryable(),
        "{context}: retryable"
    );
    assert_eq!(
        actual.is_retryable_for(true),
        expected.is_retryable_for(true),
        "{context}: retryable for an idempotent request"
    );
    assert_eq!(
        actual.is_ambiguous(),
        expected.is_ambiguous(),
        "{context}: ambiguous"
    );
    assert_eq!(
        actual.retry_after(),
        expected.retry_after(),
        "{context}: retry-after"
    );
}

// ---------------------------------------------------------------------------
// Response boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_success_body_has_no_data_but_a_required_payload_still_errors() {
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
async fn bodiless_deletes_succeed_for_empty_and_whitespace_bodies() {
    for (status, body) in [(204, ""), (200, ""), (200, " \n\t")] {
        let client = client_with(vec![Response::new(
            status,
            [("Content-Length".to_string(), body.len().to_string())],
            body.as_bytes().to_vec(),
        )]);
        let deletion = client
            .delete_print_job(&JobId::new("6035145").expect("id"))
            .await
            .expect("a bodiless 2xx is a successful deletion");
        assert_eq!(deletion.message, "", "{status} {body:?}");
    }
}

#[tokio::test]
async fn bodiless_submits_are_ambiguous_not_terminal() {
    for (status, body) in [(200, ""), (204, ""), (200, " \n\t")] {
        let client = client_with(vec![Response::new(
            status,
            [("Content-Length".to_string(), body.len().to_string())],
            body.as_bytes().to_vec(),
        )]);
        let error = client
            .submit_print_job(&Letter::new(&pdf()).expect("letter"))
            .await
            .expect_err("a submit without a decoded job is ambiguous");
        assert!(error.is_ambiguous(), "{status} {body:?}: {error:?}");
        assert!(!error.is_retryable(), "{status} {body:?}: {error:?}");
        assert_eq!(
            error.status(),
            Some(status),
            "{status} {body:?}: the status the provider answered with is kept as data"
        );
        assert!(
            matches!(
                error,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: actual },
                    ..
                } if actual == status
            ),
            "{status} {body:?}: structured kind"
        );
    }
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
async fn decoded_shape_failures_keep_the_serde_error_reachable() {
    // Just inside: a provider can add fields without breaking a read.
    let client = client_for(
        200,
        r#"{"status":200,"data":{"id":6035143,"items":[],"provider_added":"new"}}"#,
    );
    let job = client
        .print_job(&JobId::new("6035143").expect("id"))
        .await
        .expect("unknown fields stay ignored");
    assert_eq!(job.id.as_str(), "6035143");

    // Just outside: the same endpoint rejects a wrong type. Before the fix the
    // serde error was rendered into `message` and `source()` returned `None`.
    let client = client_for(
        200,
        r#"{"status":200,"data":{"id":6035143,"items":"not a list"}}"#,
    );
    let error = client
        .print_job(&JobId::new("6035143").expect("id"))
        .await
        .expect_err("a wrong item shape cannot be decoded");

    let source = std::error::Error::source(&error).expect("the serde cause stays reachable");
    let source = source
        .downcast_ref::<serde_json::Error>()
        .expect("the source is the original serde_json error");
    assert_eq!(source.classify(), serde_json::error::Category::Data);
    assert!(error.to_string().contains("items"), "{error}");
    assert!(!error.is_retryable_for(true), "the same bytes fail again");
}

#[tokio::test]
async fn email_page_shape_failures_keep_the_serde_error_reachable() {
    // Just inside: unknown job fields do not prevent the page from decoding.
    let client = client_for(
        200,
        r#"{"status":200,"data":{"emailjobs":[{"id":1,"provider_added":true}]}}"#,
    );
    let page = client
        .email_jobs(&letterxpress::EmailJobQuery::new())
        .await
        .expect("unknown fields stay ignored");
    assert_eq!(page.jobs.len(), 1);

    // Just outside: `emailjobs` must be an array.
    let client = client_for(200, r#"{"status":200,"data":{"emailjobs":{}}}"#);
    let error = client
        .email_jobs(&letterxpress::EmailJobQuery::new())
        .await
        .expect_err("an object is not a job list");

    let source = std::error::Error::source(&error).expect("the serde cause stays reachable");
    let source = source
        .downcast_ref::<serde_json::Error>()
        .expect("the source is the original serde_json error");
    assert_eq!(source.classify(), serde_json::error::Category::Data);
    assert!(matches!(
        error,
        Error::UnexpectedShape {
            endpoint: "GET /v3/emailjobs",
            ..
        }
    ));
}

#[tokio::test]
async fn email_submit_shape_failures_remain_ambiguous() {
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(
            EmailDelivery::new("kunde@example.de", EmailOption::MailSecure).expect("delivery"),
        )
        .expect("email");
    let client = client_for(200, r#"{"status":200,"data":{"nothing":true}}"#);
    let error = client
        .submit_email_job(&letter)
        .await
        .expect_err("the job may already exist");

    assert!(error.is_ambiguous());
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("undecodable body"), "{error}");
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

/// The provider's `Retry-After` is honoured as written up to the ceiling and
/// clamped at it — a header cannot park a caller for hours.
#[tokio::test]
async fn retry_after_is_honoured_up_to_the_ceiling_and_clamped_past_it() {
    use std::time::Duration;

    for (header, expected) in [
        ("299", Some(Duration::from_secs(299))), // just inside the ceiling
        ("300", Some(MAX_RETRY_AFTER)),          // exactly at it
        ("301", Some(MAX_RETRY_AFTER)),          // just outside it
        ("86400", Some(MAX_RETRY_AFTER)),        // a day: clamped, not obeyed
        ("Wed, 21 Oct 2015 07:28:00 GMT", None), // HTTP-date: no usable hint
        ("garbage", None),
    ] {
        let client = client_with(vec![Response::new(
            429,
            [("Retry-After".to_string(), header.to_string())],
            fixture("synthetic_rate_limited.json").into_bytes(),
        )]);
        let error = client.balance().await.expect_err("must fail");
        assert!(error.is_retryable(), "{header}");
        assert_eq!(error.retry_after(), expected, "{header}");

        // Decoding already bounds the value, so nothing needs the "capped"
        // note and a log line never shows a wait longer than the ceiling.
        let wording = error.to_string();
        assert!(!wording.contains("capped from"), "{wording}");
    }
}

/// A rate-limit error built by hand is bounded by the accessor and named as
/// capped in its wording, so neither path hands out an unbounded wait.
#[test]
fn a_hand_built_rate_limit_is_capped_by_the_accessor() {
    use std::time::Duration;

    let absurd = Error::RateLimited {
        retry_after: Some(Duration::from_secs(86_400)),
    };
    assert_eq!(absurd.retry_after(), Some(MAX_RETRY_AFTER));
    assert!(absurd.to_string().contains("retry after 300s"));
    assert!(absurd.to_string().contains("capped from 86400s"));

    // At or below the ceiling nothing is rewritten.
    let inside = Error::RateLimited {
        retry_after: Some(MAX_RETRY_AFTER),
    };
    assert_eq!(inside.retry_after(), Some(MAX_RETRY_AFTER));
    assert_eq!(
        inside.to_string(),
        "letterxpress rate limit reached, retry after 300s"
    );

    // No hint: no suffix, and the accessor has nothing to offer.
    let bare = Error::RateLimited { retry_after: None };
    assert_eq!(bare.retry_after(), None);
    assert_eq!(bare.to_string(), "letterxpress rate limit reached");

    // Only a rate limit carries a wait.
    for terminal in [
        Error::NotFound {
            message: "gone".into(),
        },
        Error::Ambiguous {
            kind: AmbiguousKind::Timeout,
            message: "timeout".into(),
        },
    ] {
        assert_eq!(terminal.retry_after(), None, "{terminal:?}");
    }
}

#[tokio::test]
async fn a_body_level_rate_limit_on_submit_stays_retryable() {
    let client = client_for_with_retry_after(
        200,
        r#"{"status":429,"message":"Too Many Requests"}"#.to_string(),
    );
    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(error.is_retryable());
    assert!(!error.is_ambiguous());
    assert_eq!(error.status(), Some(429));
    match error {
        Error::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn a_body_level_rate_limit_without_a_retry_after_header_still_maps() {
    let client = client_for(200, r#"{"status":429,"message":"Too Many Requests"}"#);
    match client.balance().await.expect_err("must fail") {
        Error::RateLimited { retry_after } => assert_eq!(retry_after, None),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn every_4xx_and_5xx_has_the_same_body_and_http_classification() {
    for status in 400..=599 {
        let http =
            client_for_with_retry_after(status, format!(r#"{{"message":"status {status}"}}"#));
        let body = client_for_with_retry_after(
            200,
            format!(r#"{{"status":{status},"message":"status {status}"}}"#),
        );
        let http_error = http.balance().await.expect_err("HTTP status must fail");
        let body_error = body.balance().await.expect_err("body status must fail");
        let expected = expected_status_error(status);
        let context = format!("status {status}");

        assert_same_classification(&http_error, &expected, &format!("{context} over HTTP"));
        assert_same_classification(&body_error, &expected, &format!("{context} in the body"));
        assert_same_classification(&body_error, &http_error, &format!("{context} parity"));
    }
}

#[tokio::test]
async fn an_unfollowed_informational_or_redirect_on_a_read_stays_terminal() {
    for status in [100, 199, 300, 399] {
        let client = client_with(vec![Response::new(
            status,
            [(
                "Location".to_string(),
                "https://elsewhere.test/".to_string(),
            )],
            Vec::new(),
        )]);
        let error = client.balance().await.expect_err("must fail");
        assert!(
            matches!(&error, Error::UnexpectedStatus { status: actual, .. } if *actual == status),
            "{status} produced {error:?}"
        );
        assert_eq!(error.status(), Some(status));
        assert!(!error.is_ambiguous(), "{status} produced {error:?}");
        assert!(!error.is_retryable(), "{status} produced {error:?}");
        assert!(error.to_string().contains(&status.to_string()));
    }
}

#[tokio::test]
async fn an_unfollowed_informational_or_redirect_answer_to_a_submit_is_ambiguous() {
    for status in [100, 199, 300, 399] {
        let client = client_with(vec![Response::new(
            status,
            [(
                "Location".to_string(),
                "https://elsewhere.test/".to_string(),
            )],
            Vec::new(),
        )]);
        let error = client
            .submit_print_job(&Letter::new(&pdf()).expect("letter"))
            .await
            .expect_err("must fail");
        assert!(error.is_ambiguous(), "{status} produced {error:?}");
        assert!(!error.is_retryable(), "{status} produced {error:?}");
        assert_eq!(error.status(), Some(status), "{status} produced {error:?}");
        assert!(
            matches!(
                &error,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: actual },
                    ..
                } if *actual == status
            ),
            "{status} produced {error:?}"
        );
    }
}

#[tokio::test]
async fn an_unfollowed_informational_or_redirect_answer_to_an_email_submit_is_ambiguous() {
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(
            EmailDelivery::new("kunde@example.de", EmailOption::MailSecure).expect("delivery"),
        )
        .expect("email");
    for status in [100, 199, 300, 399] {
        let client = client_with(vec![Response::new(
            status,
            [(
                "Location".to_string(),
                "https://elsewhere.test/".to_string(),
            )],
            Vec::new(),
        )]);
        let error = client
            .submit_email_job(&letter)
            .await
            .expect_err("must fail");
        assert!(error.is_ambiguous(), "{status} produced {error:?}");
        assert!(!error.is_retryable(), "{status} produced {error:?}");
        assert_eq!(error.status(), Some(status), "{status} produced {error:?}");
        assert!(
            matches!(
                &error,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: actual },
                    ..
                } if *actual == status
            ),
            "{status} produced {error:?}"
        );
    }
}

#[tokio::test]
async fn a_submit_2xx_status_boundary_still_decodes() {
    for status in [200, 299] {
        let client = client_with(vec![Response::json_response(
            status,
            fixture("printjob_create.json"),
        )]);
        let job = client
            .submit_print_job(&Letter::new(&pdf()).expect("letter"))
            .await
            .unwrap_or_else(|error| panic!("{status}: {error}"));
        assert_eq!(job.id.as_str(), "6035143", "{status}");
    }
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
    assert_eq!(job.total().expect("total").cents(), 250);
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
// Flag boundary: numbers and strings share one rule
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_numeric_flag_and_its_string_form_mean_the_same_thing() {
    // The boundary of the numeric rule, as a number and as a string: zero is
    // the only "not requested"; one, two, `01` and `+1` are all requested.
    let cases = [
        ("0", false),
        ("1", true),
        ("2", true),
        ("-1", true),
        ("\"0\"", false),
        ("\"00\"", false),
        ("\"0.0\"", false),
        ("\"1\"", true),
        ("\"01\"", true),
        ("\"+1\"", true),
        ("\"2\"", true),
        ("true", true),
        ("false", false),
        ("\"yes\"", true),
        ("\"NO\"", false),
        ("null", false),
        ("\"\"", false),
    ];

    for (raw, expected) in cases {
        let client = client_for(
            200,
            &format!(
                r#"{{"status":200,"data":{{"id":1,"c4":{raw},"bank_form":{raw},"items":[]}}}}"#
            ),
        );
        let job = client
            .print_job(&JobId::new("1").expect("id"))
            .await
            .expect("job");
        assert_eq!(job.c4, expected, "c4 = {raw}");
        assert_eq!(job.bank_form, expected, "bank_form = {raw}");
    }
}

#[tokio::test]
async fn an_unreadable_flag_fails_closed_on_a_read() {
    let client = client_for(200, r#"{"status":200,"data":{"id":1,"c4":"y","items":[]}}"#);
    let error = client
        .print_job(&JobId::new("1").expect("id"))
        .await
        .expect_err("an unreadable flag must not decode as \"not requested\"");

    assert!(matches!(
        error,
        Error::UnexpectedShape {
            endpoint: "GET /v3/printjobs/{id}",
            ..
        }
    ));
    assert!(error.to_string().contains("flag"), "{error}");
    assert!(!error.is_retryable(), "the same bytes decode the same way");
    assert!(!error.is_ambiguous());
}

#[tokio::test]
async fn an_unreadable_flag_on_a_billed_submit_is_ambiguous() {
    let client = client_for(200, r#"{"status":200,"data":{"id":7,"c4":"y","items":[]}}"#);
    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(error.is_ambiguous(), "the job may exist and be billed");
    assert!(!error.is_retryable(), "never resubmit what may be billed");
    assert_eq!(error.status(), Some(200), "the provider answered a 2xx");
    assert!(
        matches!(
            &error,
            Error::Ambiguous {
                kind: AmbiguousKind::Upstream { status: 200 },
                ..
            }
        ),
        "{error:?}"
    );
}

// ---------------------------------------------------------------------------
// Transport boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_submit_timeout_is_ambiguous_and_sent_once() {
    let recorder = letterxpress::transport::RecordingTransport::new(MockTransport::new([Err(
        Error::Transport {
            kind: TransportKind::Timeout,
            source: Box::new(std::io::Error::from(std::io::ErrorKind::TimedOut)),
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
    assert!(
        matches!(
            error,
            Error::Ambiguous {
                kind: AmbiguousKind::Timeout,
                ..
            }
        ),
        "a timeout is its own kind: {error:?}"
    );
    assert_eq!(error.status(), None, "no answer carried a status");
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
            source: Box::new(std::io::Error::other("dns failure")),
        })]),
        Credentials::new("kunde", "key", Mode::Live).expect("credentials"),
    );

    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(!error.is_ambiguous(), "nothing was written");
    assert!(error.is_retryable());
    assert!(
        matches!(
            error,
            Error::Transport {
                kind: TransportKind::Connect,
                ..
            }
        ),
        "a connect-phase failure stays a plain transport error: {error:?}"
    );
    assert!(error.is_retryable_for(Method::Post.is_idempotent()));
}

#[tokio::test]
async fn a_read_timeout_is_retryable_for_the_read_but_not_by_default() {
    let client = Client::new(
        MockTransport::new([Err(Error::Transport {
            kind: TransportKind::Timeout,
            source: Box::new(std::io::Error::from(std::io::ErrorKind::TimedOut)),
        })]),
        Credentials::new("kunde", "key", Mode::Test).expect("credentials"),
    );
    let error = client.balance().await.expect_err("must fail");
    assert!(!error.is_ambiguous());
    assert!(
        error.is_retryable_for(Method::Get.is_idempotent()),
        "the same failure on a submit would be ambiguous"
    );
    assert!(
        !error.is_retryable(),
        "the context-free answer stays the submit one"
    );
}

#[tokio::test]
async fn a_submit_5xx_is_ambiguous_but_a_read_5xx_is_not() {
    let client = client_for(500, r#"{"message":"Internal Server Error"}"#);
    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");
    assert!(error.is_ambiguous());
    assert_eq!(error.status(), Some(500));
    assert!(
        matches!(
            &error,
            Error::Ambiguous {
                kind: AmbiguousKind::Upstream { status: 500 },
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.is_retryable_for(Method::Post.is_idempotent()));
    assert!(error.to_string().contains("http 500"));

    let client = client_for(500, r#"{"message":"Internal Server Error"}"#);
    let error = client.balance().await.expect_err("must fail");
    assert!(!error.is_ambiguous());
    assert!(
        error.is_retryable_for(Method::Get.is_idempotent()),
        "a 5xx answer on a read is safe to retry"
    );
    assert!(
        !error.is_retryable(),
        "but a submit could already be billed"
    );
    assert!(matches!(error, Error::Upstream { status: 500, .. }));
}

#[tokio::test]
async fn the_submit_status_boundary_decides_between_rejected_and_reconciliation() {
    // Just below the 5xx class: a definitive refusal, nothing was created.
    let client = client_for(499, r#"{"message":"Client Closed Request"}"#);
    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");
    assert!(!error.is_ambiguous(), "499: {error:?}");
    assert_eq!(error.status(), Some(499));

    // The 5xx class, first and last code, plus one outside the documented
    // classes: each answer came back from the provider, so the submit may
    // already have been processed and the code is kept as data.
    for status in [500, 599, 600] {
        let client = client_for(status, r#"{"message":"upstream"}"#);
        let error = client
            .submit_print_job(&Letter::new(&pdf()).expect("letter"))
            .await
            .expect_err("must fail");
        assert!(error.is_ambiguous(), "{status}: {error:?}");
        assert_eq!(error.status(), Some(status), "{status}: {error:?}");
        assert!(
            matches!(
                &error,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: actual },
                    ..
                } if *actual == status
            ),
            "{status}: {error:?}"
        );
    }
}

#[tokio::test]
async fn a_submit_io_failure_is_its_own_kind() {
    let client = Client::new(
        MockTransport::new([Err(Error::Transport {
            kind: TransportKind::Io,
            source: Box::new(std::io::Error::other("connection reset by peer")),
        })]),
        Credentials::new("kunde", "key", Mode::Live).expect("credentials"),
    );

    let error = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect_err("must fail");

    assert!(error.is_ambiguous());
    assert_eq!(error.status(), None, "no answer carried a status");
    assert!(
        matches!(
            error,
            Error::Ambiguous {
                kind: AmbiguousKind::Io,
                ..
            }
        ),
        "a mid-flight failure is distinguished from a timeout: {error:?}"
    );
}

#[tokio::test]
async fn a_submit_200_with_an_undecodable_body_is_ambiguous() {
    // Just inside: the documented success body decodes.
    let client = client_for(
        200,
        r#"{"status":200,"data":{"id":6035143,"status":"queue","items":[]}}"#,
    );
    let job = client
        .submit_print_job(&Letter::new(&pdf()).expect("letter"))
        .await
        .expect("documented answer");
    assert_eq!(job.id.as_str(), "6035143");

    // Just outside: the same 2xx, but nothing the client can read. The provider
    // accepted (and billed) the letter in every one of these cases, so a
    // terminal error would invite the caller to send it a second time.
    for body in [
        r#"{"status":200,"data":{"id":"6035143","status":"queue","items":"not a list"}}"#,
        r#"{"status":200,"data":"not a job"}"#,
        r#"{"status":200,"message":"OK"}"#,
        "",
        r#"{"status":200,"data":{"id":6035143"#,
    ] {
        let client = client_for(200, body);
        let error = client
            .submit_print_job(&Letter::new(&pdf()).expect("letter"))
            .await
            .expect_err("must fail");
        assert!(error.is_ambiguous(), "{body:?} produced {error:?}");
        assert!(!error.is_retryable(), "{body:?} produced {error:?}");
        assert_eq!(
            error.status(),
            Some(200),
            "{body:?}: the 2xx that created the job is kept as data"
        );
        assert!(
            matches!(
                &error,
                Error::Ambiguous {
                    kind: AmbiguousKind::Upstream { status: 200 },
                    ..
                }
            ),
            "{body:?} produced {error:?}"
        );
    }
}

#[tokio::test]
async fn a_read_200_with_an_undecodable_body_stays_terminal() {
    // The same bodies on an idempotent endpoint: nothing was created, so the
    // error stays a shape error and a retry is the caller's decision.
    for body in [
        r#"{"status":200,"data":{"id":"6035143","items":"not a list"}}"#,
        r#"{"status":200,"message":"OK"}"#,
        "",
    ] {
        let client = client_for(200, body);
        let error = client
            .print_job(&JobId::new("6035143").expect("id"))
            .await
            .expect_err("must fail");
        assert!(!error.is_ambiguous(), "{body:?} produced {error:?}");
        assert!(
            matches!(
                error,
                Error::UnexpectedShape { .. } | Error::MissingField { .. }
            ),
            "{body:?} produced {error:?}"
        );
    }
}

#[tokio::test]
async fn an_email_submit_200_with_an_undecodable_body_is_ambiguous() {
    let letter = || {
        Letter::new(&pdf())
            .expect("letter")
            .with_email(
                EmailDelivery::new("kunde@example.de", EmailOption::MailSecure).expect("delivery"),
            )
            .expect("email")
    };

    // Just inside: the documented single-job body.
    let client = client_for(200, r#"{"status":200,"data":{"id":3096,"status":"queue"}}"#);
    assert!(matches!(
        client.submit_email_job(&letter()).await,
        Ok(letterxpress::EmailJobSubmission::Single(_))
    ));

    // Just outside: bodies that parse as JSON but are neither of the three
    // documented shapes, and one with no `data` at all. The job exists.
    for body in [
        r#"{"status":200,"data":{"nothing":true}}"#,
        r#"{"status":200,"message":"OK"}"#,
        r#"{"status":200,"data":[{"id":"not a number"}]}"#,
        r#"{"status":200,"data":[{"id":1}"#,
    ] {
        let client = client_for(200, body);
        let error = client
            .submit_email_job(&letter())
            .await
            .expect_err("must fail");
        assert!(error.is_ambiguous(), "{body:?} produced {error:?}");
        assert!(!error.is_retryable(), "{body:?} produced {error:?}");
        assert_eq!(error.status(), Some(200), "{body:?} produced {error:?}");
    }
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
    // Valid: registered mail on the class it requires, in either order.
    let national = Letter::new(&pdf())
        .expect("letter")
        .with_shipping(Shipping::National)
        .expect("national")
        .with_registered(Registered::EinschreibenEinwurf)
        .expect("registered");
    assert_eq!(national.to_json()["registered"], "r1");
    assert_eq!(national.to_json()["specification"]["shipping"], "national");

    let registered_first = Letter::new(&pdf())
        .expect("letter")
        .with_registered(Registered::Einschreiben)
        .expect("registered")
        .with_shipping(Shipping::National)
        .expect("national");
    assert_eq!(
        registered_first.to_json()["specification"]["shipping"],
        "national"
    );

    // Just inside the limit: `Auto` is not international, so the pair stays legal.
    assert!(Letter::new(&pdf())
        .expect("letter")
        .with_shipping(Shipping::Auto)
        .expect("auto")
        .with_registered(Registered::Einschreiben)
        .is_ok());

    // Just outside: international, whichever setter runs first.
    let error = Letter::new(&pdf())
        .expect("letter")
        .with_shipping(Shipping::International)
        .expect("international")
        .with_registered(Registered::EinschreibenEinwurf)
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::Validation {
            field: "letter.registered",
            ..
        }
    ));

    let registered = Letter::new(&pdf())
        .expect("letter")
        .with_registered(Registered::Einschreiben)
        .expect("registered");
    let error = registered
        .clone()
        .with_shipping(Shipping::International)
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::Validation {
            field: "letter.specification.shipping",
            ..
        }
    ));

    // Replacing the whole specification is the third way into the same pair.
    let international = letterxpress::Specification::new().with_shipping(Shipping::International);
    assert!(registered
        .clone()
        .with_specification(international)
        .is_err());

    // A carry-all spelling of the same wire value cannot slip past the rule,
    // padded or not.
    let smuggled = Shipping::from_wire("International");
    assert!(matches!(smuggled, Shipping::International));
    assert!(registered
        .clone()
        .with_shipping(Shipping::Other("international".into()))
        .is_err());
    assert!(registered
        .clone()
        .with_shipping(Shipping::Other("  international  ".into()))
        .is_err());
}

#[test]
fn a_price_request_carries_the_same_pair_and_the_same_rule() {
    let international = letterxpress::PriceRequest::new(1)
        .expect("price")
        .with_shipping(Shipping::International)
        .expect("international");
    let error = international
        .with_registered(Registered::Einschreiben)
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::Validation {
            field: "price.registered",
            ..
        }
    ));

    let registered = letterxpress::PriceRequest::new(1)
        .expect("price")
        .with_registered(Registered::Einschreiben)
        .expect("registered");
    let error = registered
        .clone()
        .with_shipping(Shipping::International)
        .expect_err("must fail");
    assert!(matches!(
        error,
        Error::Validation {
            field: "price.specification.shipping",
            ..
        }
    ));
    assert!(registered.clone().with_shipping(Shipping::National).is_ok());
    assert!(registered
        .with_shipping(Shipping::Other("international".into()))
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
    assert!(matches!(
        letterxpress::Separator::after_pages(0),
        Err(Error::Validation {
            field: "letter.serial_letter.pages_separator_value",
            ..
        })
    ));
    assert!(letterxpress::Separator::after_pages(u32::MAX).is_ok());
    for invalid in ["", "   ", "a\u{0}b"] {
        assert!(matches!(
            letterxpress::Separator::at_keyword(invalid),
            Err(Error::Validation {
                field: "letter.serial_letter.pages_separator_value",
                ..
            })
        ));
    }
    assert!(letterxpress::Separator::at_keyword("x".repeat(255)).is_ok());
    assert!(matches!(
        letterxpress::Separator::at_keyword("x".repeat(256)),
        Err(Error::Validation {
            field: "letter.serial_letter.pages_separator_value",
            ..
        })
    ));
}

#[test]
fn forged_separator_variants_are_revalidated_by_letter() {
    let invalid = [
        letterxpress::Separator::Pages(0),
        letterxpress::Separator::Keyword(String::new()),
        letterxpress::Separator::Keyword("   ".to_string()),
        letterxpress::Separator::Keyword("a\u{0}b".to_string()),
        letterxpress::Separator::Keyword("a\nb".to_string()),
        letterxpress::Separator::Keyword("x".repeat(256)),
    ];

    for separator in invalid {
        let label = format!("{separator:?}");
        let error = Letter::new(&pdf())
            .expect("letter")
            .with_separator(separator)
            .expect_err("invalid public variant must be rejected");
        assert!(
            matches!(
                error,
                Error::Validation {
                    field: "letter.serial_letter.pages_separator_value",
                    ..
                }
            ),
            "{label:?} produced the wrong error"
        );
    }

    for separator in [
        letterxpress::Separator::Pages(1),
        letterxpress::Separator::Keyword("x".repeat(255)),
        letterxpress::Separator::Keyword(":: NEXT ::".to_string()),
    ] {
        let expected = separator.to_json();
        let letter = Letter::new(&pdf())
            .expect("letter")
            .with_separator(separator)
            .expect("valid public variant must be accepted");
        assert_eq!(letter.to_json()["serial_letter"], expected);
    }
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
    assert_eq!(credentials.username(), "kunde");
    assert_eq!(credentials.mode(), Mode::Live);
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
        "https:///v3",
        "https://:8443",
        "https:////",
        "https:////v3",
        "https://./v3",
        "https://api.letterxpress.de:65536/v3",
        "https://api.letterxpress.de:not-a-port/v3",
        "https://api.letterxpress .de/v3",
    ] {
        let error = client
            .clone()
            .with_base_url(bad)
            .expect_err("an invalid base URL must be refused");
        assert!(
            matches!(
                error,
                Error::Validation {
                    field: "base_url",
                    ..
                }
            ),
            "{bad:?}: {error:?}"
        );
    }
    for good in [
        "https://a",
        "https://api.letterxpress.de:65535/v3",
        "https://[::1]:65535/v3",
        "HTTPS://api.letterxpress.de/v3",
    ] {
        assert!(client.clone().with_base_url(good).is_ok(), "{good:?}");
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
fn a_page_total_is_bounded_at_the_decode_boundary() {
    // Inside: the documented ceiling decodes, along with its string and float
    // spellings, `null` and an absent field.
    let at_limit: PrintJob = serde_json::from_str(
        r#"{"id":1,"items":[{"pages":"10000"},{"pages":2.0},{"pages":null},{}]}"#,
    )
    .expect("documented page counts decode");
    assert_eq!(at_limit.pages(), 10_002);

    // One page above the ceiling, a whole u32, a negative count or a token that
    // is not a count at all: refused, never truncated into a smaller total.
    for body in [
        r#"{"id":1,"items":[{"pages":10001}]}"#,
        r#"{"id":1,"items":[{"pages":4294967295}]}"#,
        r#"{"id":1,"items":[{"pages":-1}]}"#,
        r#"{"id":1,"items":[{"pages":"x"}]}"#,
    ] {
        assert!(serde_json::from_str::<PrintJob>(body).is_err(), "{body}");
    }
}

#[test]
fn a_hand_built_page_total_saturates_instead_of_wrapping() {
    let job = PrintJob {
        items: vec![
            letterxpress::PrintItem {
                pages: u32::MAX - 1,
                ..letterxpress::PrintItem::default()
            },
            letterxpress::PrintItem {
                pages: 10,
                ..letterxpress::PrintItem::default()
            },
        ],
        ..PrintJob::default()
    };
    assert_eq!(
        job.pages(),
        u32::MAX,
        "saturated: no wrap to 8 and no panic in a debug build"
    );
}

#[test]
fn money_helpers_are_stable_at_the_boundaries() {
    assert_eq!(Money::from_cents(0).map(Money::cents), Ok(0));
    assert_eq!(Money::from_cents(-1), Err(MoneyError::Negative));
    assert_eq!(Money::from_cents(i64::MAX).map(Money::cents), Ok(i64::MAX));
    assert_eq!(
        Money::from_cents(i64::MAX)
            .expect("max")
            .checked_add(Money::from_cents(1).expect("one")),
        Err(MoneyError::OutOfRange)
    );

    assert_eq!(Money::from_euros(f64::NAN), Err(MoneyError::NotFinite));
    assert_eq!(Money::from_euros(f64::INFINITY), Err(MoneyError::NotFinite));
    assert_eq!(Money::from_euros(-0.01), Err(MoneyError::Negative));
    assert_eq!(Money::from_euros(f64::MAX), Err(MoneyError::OutOfRange));
    assert_eq!(
        Money::from_euros(4.999).map(Money::cents),
        Ok(500),
        "rounds half-up"
    );
    assert_eq!(
        Money::from_euros(1_000_000.0).expect("valid").to_string(),
        "1000000.00"
    );

    let sum = Money::from_euros(0.1)
        .expect("0.1")
        .checked_add(Money::from_euros(0.2).expect("0.2"))
        .expect("sum");
    assert_eq!(sum.cents(), 30, "0.1 + 0.2 is exact in cents");
}

#[test]
fn money_deserialisation_rejects_non_finite_negative_and_overflowing_values() {
    for bad in [
        "NaN",
        "inf",
        "-inf",
        "1e400",
        "-0.01",
        "9223372036854775808",
    ] {
        assert!(
            serde_json::from_str::<Money>(&format!("\"{bad}\"")).is_err(),
            "{bad}"
        );
    }
    assert!(serde_json::from_str::<Money>("-1").is_err());
    assert!(serde_json::from_str::<Money>("1e400").is_err());
    assert!(serde_json::from_str::<Money>(&i64::MAX.to_string()).is_err());
    assert_eq!(serde_json::from_str::<Money>("0").expect("zero").cents(), 0);
}

#[test]
fn money_deserialisation_keeps_unknown_enclosing_fields_permissive() {
    let balance: letterxpress::Balance = serde_json::from_str(
        r#"{"balance":"54.89","currency":"EUR","future_field":{"nested":true}}"#,
    )
    .expect("unknown fields stay ignored");
    assert_eq!(balance.cents(), 5_489);
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

#[test]
fn a_decodable_payload_keeps_the_decoder_error_reachable() {
    use base64::Engine as _;

    // The valid value.
    let document: letterxpress::InvoiceDocument =
        serde_json::from_str(r#"{"id":1,"base64_data":"JVBERi0xLjQ="}"#).expect("invoice");
    assert_eq!(
        document.pdf().expect("a valid payload decodes"),
        b"%PDF-1.4"
    );

    // Just outside it: one illegal symbol. The decoder's own error survives as
    // the `source`, so a caller can branch on the failure instead of parsing
    // the rendered sentence.
    let encoded = "JVBERi0xLjQ!";
    let expected = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect_err("the fixture must be invalid base64");
    let document: letterxpress::InvoiceDocument =
        serde_json::from_str(&format!(r#"{{"id":1,"base64_data":"{encoded}"}}"#)).expect("invoice");
    let error = document.pdf().expect_err("an illegal symbol must fail");

    let Error::Base64 { field, source } = &error else {
        panic!("expected Error::Base64, got {error:?}");
    };
    assert_eq!(*field, "invoice.base64_data");
    assert_eq!(*source, expected, "the decoder's error, not its text");
    assert_eq!(
        error.to_string(),
        format!("invalid base64 in `invoice.base64_data`: {source}")
    );
    assert!(
        std::error::Error::source(&error).is_some(),
        "the cause is reachable, not flattened"
    );
}

#[tokio::test]
async fn a_successful_reply_with_an_empty_data_object_is_not_an_error() {
    let client = client_for(200, r#"{"status":200,"message":"OK","data":{}}"#);
    assert_eq!(client.balance().await.expect("balance").cents(), 0);
}

#[tokio::test]
async fn email_job_submission_with_an_unknown_shape_is_ambiguous() {
    // The provider answered 2xx, so the job exists; a shape error here would be
    // terminal and invite a second, billed submission.
    let client = client_for(200, r#"{"status":200,"data":{"nothing":"here"}}"#);
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(EmailDelivery::new("a@b.de", EmailOption::MailDirect).expect("email"))
        .expect("email delivery");
    let error = client
        .submit_email_job(&letter)
        .await
        .expect_err("must fail");
    assert!(error.is_ambiguous(), "{error:?}");
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("POST /v3/emailjobs"), "{error}");
}
