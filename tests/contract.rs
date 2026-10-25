//! Contract tests: every fixture is a verbatim example from the official
//! LetterXpress API v3 documentation (see `docs/contracts.md`), and every
//! public model is decoded from it.
//!
//! A failure here means the crate no longer reads what the provider documents.

mod common;

use common::{client_with, fixture, fixture_response};
use letterxpress::transport::Response;
use letterxpress::{
    Balance, Color, EmailJob, EmailJobPage, EmailJobSubmission, Invoice, InvoiceDocument,
    InvoicePage, JobStatus, Journal, Price, PrintJob, PrintJobPage, Registered, Shipping,
    TransactionPage,
};

#[tokio::test]
async fn balance_matches_the_documented_example() {
    let client = client_with(vec![fixture_response("balance.json")]);
    let balance: Balance = client.balance().await.expect("balance");

    assert_eq!(balance.cents(), 5_489);
    assert_eq!(balance.balance.to_string(), "54.89");
    assert_eq!(balance.currency.as_deref(), Some("EUR"));
}

#[tokio::test]
async fn price_echoes_the_priced_specification() {
    let client = client_with(vec![fixture_response("price.json")]);
    let price: Price = client
        .price(&letterxpress::PriceRequest::new(1).unwrap())
        .await
        .expect("price");

    assert_eq!(price.price.cents(), 499);
    let echo = price.echo.expect("echo");
    assert_eq!(echo.pages, 1);
    assert_eq!(echo.color, Some(Color::Color));
    assert_eq!(echo.shipping, Some(Shipping::National));
    assert_eq!(echo.registered, Some(Registered::EinschreibenEinwurf));
    assert!(!echo.c4);
}

#[tokio::test]
async fn print_job_list_matches_the_documented_example() {
    let client = client_with(vec![fixture_response("printjobs_list.json")]);
    let page: PrintJobPage = client
        .print_jobs(&letterxpress::PrintJobQuery::new())
        .await
        .expect("print jobs");

    assert_eq!(page.jobs.len(), 1);
    let job = &page.jobs[0];
    assert_eq!(job.id.as_str(), "6035138");
    assert_eq!(job.status, JobStatus::Done);
    assert_eq!(job.shipping, Some(Shipping::National));
    assert_eq!(job.color, Some(Color::BlackAndWhite));
    assert_eq!(job.group.as_deref(), Some("exampleGroup"));
    assert_eq!(job.created_at.as_deref(), Some("2022-07-26 09:38:00"));

    let item = &job.items[0];
    assert_eq!(
        item.address.as_deref(),
        Some("Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg")
    );
    assert_eq!(item.pages, 1);
    assert_eq!(item.net.cents(), 67);
    assert_eq!(item.vat.cents(), 13);
    assert_eq!(item.total().cents(), 80);
    assert_eq!(item.status, JobStatus::Sent);
    assert_eq!(item.tracking_code.as_deref(), Some("RC123456789DE"));
    assert_eq!(
        item.tracking_status.as_deref(),
        Some("Zugestellt: Die Sendung wurde am 27.07.22 13:32:29 erfolgreich zugestellt.")
    );

    let pagination = page.pagination.expect("pagination");
    assert_eq!(pagination.total, 7);
    assert_eq!(pagination.per_page, 15);
    assert!(!pagination.has_next());
}

#[tokio::test]
async fn created_print_job_reports_queue_state_and_gross_price() {
    let client = client_with(vec![fixture_response("printjob_create.json")]);
    let letter = letterxpress::Letter::new(&common::pdf())
        .expect("letter")
        .with_notice("contract-test")
        .expect("notice");
    let job: PrintJob = client.submit_print_job(&letter).await.expect("submit");

    assert_eq!(job.id.as_str(), "6035143");
    assert_eq!(job.status, JobStatus::Queue);
    assert_eq!(job.total().cents(), 81);
    assert_eq!(job.pages(), 1);
    assert!(!job.is_terminal());
}

#[tokio::test]
async fn registered_print_job_keeps_its_tracking_code() {
    let client = client_with(vec![fixture_response("printjob_get.json")]);
    let job = client
        .print_job(&letterxpress::JobId::new("6035138").unwrap())
        .await
        .expect("job");

    assert_eq!(job.registered, Some(Registered::EinschreibenEinwurf));
    assert_eq!(job.tracking_code(), Some("RC123456789DE"));
    assert!(job.is_terminal());
}

#[tokio::test]
async fn updated_print_job_shows_the_new_options() {
    let client = client_with(vec![fixture_response("printjob_update.json")]);
    let update = letterxpress::PrintJobUpdate::new(
        letterxpress::Specification::new().with_color(Color::Color),
    )
    .with_notice("KdNr. xxxxx")
    .expect("notice");

    let job = client
        .update_print_job(&letterxpress::JobId::new("6035145").unwrap(), &update)
        .await
        .expect("update");

    assert_eq!(job.notice.as_deref(), Some("KdNr. xxxxx"));
    assert_eq!(job.dispatch_date.as_deref(), Some("2022-09-01"));
    assert_eq!(job.color, Some(Color::Color));
    assert_eq!(job.total().cents(), 180);
}

#[tokio::test]
async fn deletion_returns_only_a_message() {
    let client = client_with(vec![fixture_response("printjob_delete.json")]);
    let deletion = client
        .delete_print_job(&letterxpress::JobId::new("6035145").unwrap())
        .await
        .expect("delete");
    assert_eq!(deletion.message, "Print job deleted successfully");
}

#[tokio::test]
async fn single_email_job_carries_its_print_job() {
    let client = client_with(vec![fixture_response("emailjob_create_single.json")]);
    let letter = letterxpress::Letter::new(&common::pdf())
        .expect("letter")
        .with_email(
            letterxpress::EmailDelivery::new(
                "max@letterxpress.email",
                letterxpress::EmailOption::MailPlus,
            )
            .expect("email"),
        )
        .expect("email delivery");

    let submission = client.submit_email_job(&letter).await.expect("submit");
    let EmailJobSubmission::Single(job) = submission else {
        panic!("expected a single job, got {submission:?}");
    };

    assert_eq!(job.id.as_str(), "3096");
    assert_eq!(job.status, JobStatus::Queue);
    assert_eq!(job.total().cents(), 12);
    assert_eq!(job.email_option, Some(letterxpress::EmailOption::MailPlus));
    let printjob = job.printjob.expect("nested print job");
    assert_eq!(printjob.id.as_str(), "138040");
    assert_eq!(printjob.filename_original.as_deref(), Some("anhang.pdf"));
}

#[tokio::test]
async fn serial_email_jobs_decode_as_an_array() {
    let client = client_with(vec![fixture_response("emailjob_create_serial.json")]);
    let letter = letterxpress::Letter::new(&common::pdf())
        .expect("letter")
        .with_email(
            letterxpress::EmailDelivery::new("a@b.de", letterxpress::EmailOption::MailDirect)
                .expect("email"),
        )
        .expect("email delivery");

    let submission = client.submit_email_job(&letter).await.expect("submit");
    assert_eq!(submission.print_jobs().len(), 0);
    let EmailJobSubmission::Serial(jobs) = submission else {
        panic!("expected a serial answer");
    };
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[2].id.as_str(), "3815");
    assert_eq!(jobs[0].subject.as_deref(), Some("Monats Rechnung"));
}

#[tokio::test]
async fn mixed_email_jobs_keep_both_collections() {
    let client = client_with(vec![fixture_response("emailjob_create_mixed.json")]);
    let letter = letterxpress::Letter::new(&common::pdf())
        .expect("letter")
        .with_email(
            letterxpress::EmailDelivery::new("a@b.de", letterxpress::EmailOption::MailPlus)
                .expect("email"),
        )
        .expect("email delivery");

    let submission = client.submit_email_job(&letter).await.expect("submit");
    assert_eq!(submission.print_jobs().len(), 2);
    assert_eq!(submission.email_jobs().len(), 4);

    let EmailJobSubmission::Mixed {
        print_jobs,
        email_jobs,
    } = submission
    else {
        panic!("expected a mixed answer");
    };
    assert_eq!(print_jobs[0].id.as_str(), "138606");
    assert_eq!(
        email_jobs[0]
            .printjob_id
            .as_ref()
            .map(letterxpress::JobId::as_str),
        Some("138606")
    );
    assert_eq!(
        email_jobs[3].email_option,
        Some(letterxpress::EmailOption::MailSecure)
    );
}

#[tokio::test]
async fn email_job_status_uses_the_documented_success_value() {
    let client = client_with(vec![fixture_response("emailjob_get.json")]);
    let job: EmailJob = client
        .email_job(&letterxpress::JobId::new("918").unwrap())
        .await
        .expect("job");

    assert_eq!(job.status, JobStatus::Success);
    assert!(job.status.is_terminal());
    assert_eq!(
        job.email_sender.as_deref(),
        Some("mailjet@letterxpress.email")
    );
    assert_eq!(
        job.content.as_deref(),
        Some("Bitte begleichen Sie die Rechnung Nr. 1234.\r\n\r\n.")
    );
    assert_eq!(
        job.printjob
            .as_ref()
            .and_then(|printjob| printjob.tracking_code()),
        Some("RC123456789DE")
    );
}

#[tokio::test]
async fn updated_email_job_points_at_the_new_recipient() {
    let client = client_with(vec![fixture_response("emailjob_update.json")]);
    let job = client
        .update_email_job(
            &letterxpress::JobId::new("3902").unwrap(),
            &letterxpress::EmailJobUpdate::new("j.knopf@aof.de").unwrap(),
        )
        .await
        .expect("update");

    assert_eq!(job.email_receiver.as_deref(), Some("j.knopf@aof.de"));
    assert_eq!(job.group.as_deref(), Some("exampleGroup"));
}

#[tokio::test]
async fn deleted_email_job_returns_only_a_message() {
    let client = client_with(vec![fixture_response("emailjob_delete.json")]);
    let deletion = client
        .delete_email_job(&letterxpress::JobId::new("3902").unwrap())
        .await
        .expect("delete");
    assert_eq!(deletion.message, "Email job deleted successfully");
}

#[tokio::test]
async fn documented_email_job_list_shape_still_decodes() {
    // The documented example puts the job fields at the top level of `data` and
    // `pagination` beside it, which is malformed JSON for a list. The parser
    // accepts it, because that is what the provider actually sends.
    let client = client_with(vec![fixture_response("emailjobs_list.json")]);
    let page: EmailJobPage = client
        .email_jobs(&letterxpress::EmailJobQuery::new())
        .await
        .expect("email jobs");

    assert_eq!(page.jobs.len(), 1);
    assert_eq!(page.jobs[0].id.as_str(), "3093");
    assert_eq!(page.jobs[0].status, JobStatus::Hold);
    assert_eq!(page.pagination.expect("pagination").total, 2);
    assert!(page.jobs.iter().all(|job| job.status != JobStatus::Done));
}

#[tokio::test]
async fn journal_returns_the_documented_csv() {
    let client = client_with(vec![fixture_response("journal.json")]);
    let journal: Journal = client
        .journal(&letterxpress::JournalFilter::new())
        .await
        .expect("journal");

    let csv = journal.csv().expect("csv");
    assert!(csv.contains("Auftrags-Nr."));
    assert!(csv.contains("Empfänger"));
}

#[tokio::test]
async fn transactions_keep_their_sign_and_pagination() {
    let client = client_with(vec![fixture_response("transactions.json")]);
    let page: TransactionPage = client
        .transactions(&letterxpress::TransactionQuery::new())
        .await
        .expect("transactions");

    assert_eq!(page.entries.len(), 4);
    assert_eq!(page.entries[0].amount.cents(), -104);
    assert_eq!(page.entries[2].amount.cents(), 100_000);
    assert_eq!(
        page.entries[0].description.as_deref(),
        Some("Farbe, Doppelseitig, National, 2 Seiten")
    );
    let pagination = page.pagination.expect("pagination");
    assert_eq!(pagination.total, 62);
    assert_eq!(pagination.last_page, 5);
    assert!(pagination.has_next());
}

#[tokio::test]
async fn invoices_expose_net_tax_and_gross() {
    let client = client_with(vec![fixture_response("invoices.json")]);
    let page: InvoicePage = client.invoices(1).await.expect("invoices");

    assert_eq!(page.invoices.len(), 5);
    let first: &Invoice = &page.invoices[0];
    assert_eq!(first.id.as_str(), "25402");
    assert_eq!(first.amount.cents(), 67);
    assert_eq!(first.vat.cents(), 13);
    assert_eq!(first.total().cents(), 80);
    assert_eq!(first.invoice_date.as_deref(), Some("2020-05-31"));
    assert_eq!(page.pagination.expect("pagination").total, 5);
}

#[tokio::test]
async fn single_invoice_carries_a_pdf() {
    let client = client_with(vec![fixture_response("invoice.json")]);
    let document: InvoiceDocument = client
        .invoice(&letterxpress::InvoiceId::new("25402").unwrap())
        .await
        .expect("invoice");

    assert_eq!(document.invoice.id.as_str(), "25402");
    assert!(document.pdf().expect("pdf").starts_with(b"%PDF-"));
}

#[tokio::test]
async fn documented_auth_failure_is_a_terminal_credential_error() {
    // `{"message": "Unauthorized."}` with HTTP 401 and no `status` field.
    let client = client_with(vec![Response::json_response(
        401,
        fixture("unauthorized.json"),
    )]);
    let error = client.balance().await.expect_err("must fail");

    assert!(matches!(error, letterxpress::Error::Unauthorized { .. }));
    assert_eq!(error.status(), Some(401));
    assert!(!error.is_retryable());
    assert!(!error.is_ambiguous());
    assert_eq!(
        error.to_string(),
        "letterxpress authentication failed: Unauthorized."
    );
}

#[tokio::test]
async fn bad_request_is_reported_as_rejected() {
    let client = client_with(vec![Response::json_response(
        400,
        fixture("bad_request.json"),
    )]);
    let error = client.balance().await.expect_err("must fail");
    assert!(matches!(
        error,
        letterxpress::Error::Rejected { status: 400, .. }
    ));
}

#[test]
fn fixtures_are_traceable_to_the_documentation() {
    let contracts =
        std::fs::read_to_string(format!("{}/docs/contracts.md", env!("CARGO_MANIFEST_DIR")))
            .expect("docs/contracts.md");
    assert!(contracts.contains("dokumentation-letterxpress-api-v3_v0.4.pdf"));
    for name in [
        "balance.json",
        "price.json",
        "printjobs_list.json",
        "printjob_create.json",
        "printjob_get.json",
        "printjob_update.json",
        "printjob_delete.json",
        "emailjob_create_single.json",
        "emailjob_create_serial.json",
        "emailjob_create_mixed.json",
        "emailjob_get.json",
        "emailjob_update.json",
        "emailjob_delete.json",
        "emailjobs_list.json",
        "journal.json",
        "transactions.json",
        "invoices.json",
        "invoice.json",
        "unauthorized.json",
    ] {
        let body = fixture(name);
        assert!(
            body.contains("status") || body.contains("message"),
            "{name}"
        );
    }
}
