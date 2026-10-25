//! Wire-surface tests: what each method actually puts on the socket.
//!
//! The provider authenticates through a JSON body on **every** verb — including
//! `GET` and `DELETE` — so these tests pin the verb, the path, and the body
//! shape for all 16 documented endpoints.

mod common;

use common::pdf;
use letterxpress::transport::{Method, Response};
use letterxpress::{
    Client, Credentials, Date, EmailDelivery, EmailJobQuery, EmailJobUpdate, EmailOption,
    JobFilter, JobId, JournalFilter, JournalType, Letter, Mode, PriceRequest, PrintJobQuery,
    PrintJobUpdate, Registered, Shipping, Specification, TransactionFilter, TransactionQuery,
};

/// A client that records requests and answers with one fixed envelope.
fn recording(
    body: &str,
) -> (
    Client<letterxpress::transport::RecordingTransport<letterxpress::transport::MockTransport>>,
    letterxpress::transport::RecordingTransport<letterxpress::transport::MockTransport>,
) {
    let recorder = letterxpress::transport::RecordingTransport::new(
        letterxpress::transport::MockTransport::always(Response::json_response(200, body)),
    );
    let client = Client::new(
        recorder.clone(),
        Credentials::new("kunde", "test-key", Mode::Test).expect("credentials"),
    );
    (client, recorder)
}

fn letter() -> Letter {
    Letter::new(&pdf())
        .expect("letter")
        .with_notice("surface-test")
        .expect("notice")
}

#[tokio::test]
async fn get_balance_sends_auth_in_a_get_body() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"balance":1.0,"currency":"EUR"}}"#);
    client.balance().await.expect("balance");

    let requests = recorder.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(requests[0].url, "https://api.letterxpress.de/v3/balance");
    assert_eq!(
        requests[0].headers[0],
        ("Content-Type".to_string(), "application/json".to_string())
    );
    let body = requests[0].json().expect("json");
    assert_eq!(body["auth"]["username"], "kunde");
    assert_eq!(body["auth"]["apikey"], "test-key");
    assert_eq!(body["auth"]["mode"], "test");
    assert!(body.get("letter").is_none());
}

#[tokio::test]
async fn get_price_sends_the_letter_object_with_pages() {
    let (client, recorder) = recording(r#"{"status":200,"message":"OK","data":{"price":1.0}}"#);
    let request = PriceRequest::new(3)
        .expect("price request")
        .with_registered(Registered::Einschreiben)
        .expect("registered");
    client.price(&request).await.expect("price");

    let body = recorder.requests()[0].json().expect("json");
    assert_eq!(body["letter"]["specification"]["pages"], 3);
    assert_eq!(body["letter"]["registered"], "r2");
    assert_eq!(recorder.requests()[0].method, Method::Get);
    assert_eq!(recorder.requests()[0].path_and_query(), "/v3/price");
}

#[tokio::test]
async fn list_print_jobs_sends_the_documented_filter_and_page() {
    let (client, recorder) = recording(r#"{"status":200,"message":"OK","data":{"printjobs":[]}}"#);
    let query = PrintJobQuery::new()
        .with_filter(JobFilter::Hold)
        .with_page(2)
        .expect("page");
    client.print_jobs(&query).await.expect("list");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path_and_query(), "/v3/printjobs?filter=hold&page=2");
    assert!(request.json().expect("json").get("auth").is_some());
}

#[tokio::test]
async fn get_print_job_uses_the_documented_path() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":42,"status":"queue","items":[]}}"#);
    client
        .print_job(&JobId::new("42").expect("id"))
        .await
        .expect("get");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path_and_query(), "/v3/printjobs/42");
}

#[tokio::test]
async fn submit_print_job_posts_the_full_letter_object() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":7,"status":"queue","items":[]}}"#);
    let letter = letter()
        .with_color(letterxpress::Color::Color)
        .with_print_mode(letterxpress::PrintMode::Duplex)
        .with_shipping(Shipping::National)
        .with_c4_envelope(true)
        .with_filename_original("Rechnung-1.pdf")
        .expect("filename");
    client.submit_print_job(&letter).await.expect("submit");

    let requests = recorder.requests();
    assert_eq!(requests.len(), 1, "a submit is sent exactly once");
    let request = &requests[0];
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path_and_query(), "/v3/printjobs");

    let body = request.json().expect("json");
    assert!(body["letter"]["base64_file"]
        .as_str()
        .unwrap()
        .starts_with("JVBERi0"));
    assert_eq!(
        body["letter"]["base64_file_checksum"]
            .as_str()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(body["letter"]["specification"]["color"], "4");
    assert_eq!(body["letter"]["specification"]["mode"], "duplex");
    assert_eq!(body["letter"]["specification"]["c4"], 1);
    assert_eq!(body["letter"]["filename_original"], "Rechnung-1.pdf");
    assert_eq!(body["letter"]["notice"], "surface-test");
    assert_eq!(body["auth"]["mode"], "test");
}

#[tokio::test]
async fn update_print_job_puts_only_the_mutable_fields() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":5,"status":"queue","items":[]}}"#);
    let update = PrintJobUpdate::new(Specification::new().with_shipping(Shipping::National))
        .with_dispatch_date(
            Date::new(2026, 11, 2).expect("date"),
            Date::new(2026, 10, 24).expect("today"),
        )
        .expect("dispatch date")
        .with_registered(Registered::EinschreibenEinwurf)
        .expect("registered");
    client
        .update_print_job(&JobId::new("5").expect("id"), &update)
        .await
        .expect("update");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Put);
    assert_eq!(request.path_and_query(), "/v3/printjobs/5");
    let body = request.json().expect("json");
    assert_eq!(body["letter"]["dispatch_date"], "2026-11-02");
    assert_eq!(body["letter"]["registered"], "r1");
    assert!(
        body["letter"].get("base64_file").is_none(),
        "the PDF cannot be changed"
    );
}

#[tokio::test]
async fn delete_print_job_sends_a_body_with_auth() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"Print job deleted successfully"}"#);
    client
        .delete_print_job(&JobId::new("5").expect("id"))
        .await
        .expect("delete");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Delete);
    assert_eq!(request.path_and_query(), "/v3/printjobs/5");
    assert_eq!(request.json().expect("json")["auth"]["mode"], "test");
}

#[tokio::test]
async fn submit_email_job_posts_the_email_letter_object() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":9,"status":"queue"}}"#);
    let letter = Letter::new(&pdf())
        .expect("letter")
        .with_email(EmailDelivery::new("kunde@example.de", EmailOption::MailSecure).expect("email"))
        .expect("email delivery");
    client.submit_email_job(&letter).await.expect("submit");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.path_and_query(), "/v3/emailjobs");
    let body = request.json().expect("json");
    assert_eq!(body["letter"]["email_letter"]["email_option"], "mailsecure");
    assert_eq!(
        body["letter"]["email_letter"]["email_receiver"],
        "kunde@example.de"
    );
}

#[tokio::test]
async fn list_email_jobs_uses_the_documented_query() {
    let (client, recorder) = recording(r#"{"status":200,"message":"OK","data":{"emailjobs":[]}}"#);
    let query = EmailJobQuery::new().with_filter(JobFilter::Queue);
    client.email_jobs(&query).await.expect("list");
    assert_eq!(
        recorder.requests()[0].path_and_query(),
        "/v3/emailjobs?filter=queue"
    );
}

#[tokio::test]
async fn get_email_job_uses_the_documented_path() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":9,"status":"queue"}}"#);
    client
        .email_job(&JobId::new("9").expect("id"))
        .await
        .expect("get");
    assert_eq!(recorder.requests()[0].path_and_query(), "/v3/emailjobs/9");
    assert_eq!(recorder.requests()[0].method, Method::Get);
}

#[tokio::test]
async fn update_email_job_sends_the_email_object() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"id":9,"status":"queue"}}"#);
    client
        .update_email_job(
            &JobId::new("9").expect("id"),
            &EmailJobUpdate::new("neu@example.de").expect("update"),
        )
        .await
        .expect("update");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Put);
    assert_eq!(request.path_and_query(), "/v3/emailjobs/9");
    assert_eq!(
        request.json().expect("json")["email"]["email_receiver"],
        "neu@example.de"
    );
}

#[tokio::test]
async fn delete_email_job_uses_delete_with_auth() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"Email job deleted successfully"}"#);
    client
        .delete_email_job(&JobId::new("9").expect("id"))
        .await
        .expect("delete");
    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Delete);
    assert_eq!(request.path_and_query(), "/v3/emailjobs/9");
}

#[tokio::test]
async fn journal_sends_the_filter_object() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"csv_base64":"SWQ="}}"#);
    let filter = JournalFilter::new()
        .with_type(JournalType::Print)
        .with_search("4711")
        .expect("search")
        .with_range(
            Date::new(2026, 1, 1).expect("start"),
            Date::new(2026, 1, 31).expect("end"),
        )
        .expect("range");
    client.journal(&filter).await.expect("journal");

    let request = &recorder.requests()[0];
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path_and_query(), "/v3/journal");
    let body = request.json().expect("json");
    assert_eq!(body["filter"]["type"], "print");
    assert_eq!(body["filter"]["searchstr"], "4711");
    assert_eq!(body["filter"]["start_date"], "2026-01-01");
    assert_eq!(body["filter"]["end_date"], "2026-01-31");
}

#[tokio::test]
async fn transactions_send_the_documented_filter() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"transactions":[]}}"#);
    let query = TransactionQuery::new().with_filter(TransactionFilter::PrintJobs);
    client.transactions(&query).await.expect("transactions");
    assert_eq!(
        recorder.requests()[0].path_and_query(),
        "/v3/transactions?filter=printjobs"
    );
}

#[tokio::test]
async fn invoices_and_invoice_use_the_documented_paths() {
    let (client, recorder) = recording(r#"{"status":200,"message":"OK","data":{"invoices":[]}}"#);
    client.invoices(2).await.expect("invoices");
    assert_eq!(
        recorder.requests()[0].path_and_query(),
        "/v3/invoices?page=2"
    );

    let (client, recorder) = recording(
        r#"{"status":200,"message":"OK","data":{"id":1,"amount":0,"vat":0,"base64_data":"JVBERi0="}}"#,
    );
    client
        .invoice(&letterxpress::InvoiceId::new("1").expect("id"))
        .await
        .expect("invoice");
    assert_eq!(recorder.requests()[0].path_and_query(), "/v3/invoices/1");
    assert_eq!(recorder.requests()[0].method, Method::Get);
}

#[tokio::test]
async fn every_request_carries_the_user_agent_and_json_content_type() {
    let (client, recorder) =
        recording(r#"{"status":200,"message":"OK","data":{"balance":0,"currency":"EUR"}}"#);
    client.balance().await.expect("balance");
    let request = &recorder.requests()[0];
    assert_eq!(request.headers[0].0, "Content-Type");
    assert!(request
        .headers
        .iter()
        .any(|(name, value)| name == "User-Agent" && value.starts_with("letterxpress/")));
    assert!(request
        .headers
        .iter()
        .any(|(name, value)| name == "Accept" && value == "application/json"));
}

#[tokio::test]
async fn test_and_live_clients_only_differ_in_the_auth_mode() {
    for (mode, expected) in [(Mode::Test, "test"), (Mode::Live, "live")] {
        let recorder = letterxpress::transport::RecordingTransport::new(
            letterxpress::transport::MockTransport::always(Response::json_response(
                200,
                r#"{"status":200,"message":"OK","data":{"balance":0}}"#,
            )),
        );
        let client = Client::new(
            recorder.clone(),
            Credentials::new("kunde", "test-key", mode).expect("credentials"),
        );
        client.balance().await.expect("balance");
        assert_eq!(client.mode(), mode);
        assert_eq!(
            recorder.requests()[0].json().expect("json")["auth"]["mode"],
            expected
        );
    }
}
