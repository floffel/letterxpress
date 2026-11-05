//! Edge matrix: the just-barely-valid value, the invalid value on the other
//! side of the boundary, and every hostile response the provider (or a proxy in
//! front of it) can produce.

mod common;

use common::{client_with, fixture, pdf};
use letterxpress::transport::{Method, MockTransport, Response, Transport};
use letterxpress::{
    Client, Credentials, Date, EmailDelivery, EmailOption, Error, JobId, JobStatus, Letter, Mode,
    Money, MoneyError, PrintJob, Registered, Shipping, TransportKind, MAX_NOTICE_CHARS,
    MAX_PDF_BYTES,
};

fn client_for(status: u16, body: &str) -> Client<MockTransport> {
    client_with(vec![Response::json_response(status, body.to_string())])
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
        assert_eq!(error.status(), None, "{status} {body:?}: {error:?}");
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
        assert!(
            matches!(
                &error,
                Error::Ambiguous { message } if message.contains(&format!("http {status}"))
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
        assert!(
            matches!(
                &error,
                Error::Ambiguous { message } if message.contains(&format!("http {status}"))
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
    assert!(error.is_retryable_for(Method::Post.is_idempotent()));
}

#[tokio::test]
async fn a_read_timeout_is_retryable_for_the_read_but_not_by_default() {
    let client = Client::new(
        MockTransport::new([Err(Error::Transport {
            kind: TransportKind::Timeout,
            message: "timed out".into(),
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
        assert_eq!(error.status(), None, "{body:?} was answered with a 2xx");
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
