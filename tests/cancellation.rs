//! Bulk-cancellation behavior over the documented single-job DELETE endpoints.
//!
//! The provider has no batch endpoint. Every test below uses the recording mock
//! transport, so no socket is involved and the exact request sequence is
//! visible.

mod common;

use std::time::{Duration, Instant};

use common::{MockTransport, RecordingTransport, Response};
use letterxpress::{
    CancellationBatch, CancellationJob, CancellationOutcome, Client, Credentials, Error, JobId,
    Mode, TransportKind, CANCELLATION_WINDOW, MAX_CANCELLATION_BATCH,
};

fn id(value: &str) -> JobId {
    JobId::new(value).expect("valid id")
}

fn print_deleted() -> Response {
    Response::json_response(
        200,
        r#"{"status":200,"message":"Print job deleted successfully"}"#,
    )
}

fn email_deleted() -> Response {
    Response::json_response(
        200,
        r#"{"status":200,"message":"Email job deleted successfully"}"#,
    )
}

fn recording_client(
    script: Vec<Response>,
) -> (
    Client<RecordingTransport<MockTransport>>,
    RecordingTransport<MockTransport>,
) {
    let recorder = RecordingTransport::new(MockTransport::new(
        script.into_iter().map(Ok).collect::<Vec<_>>(),
    ));
    let client = Client::new(
        recorder.clone(),
        Credentials::new("kunde", "test-key", Mode::Test).expect("credentials"),
    );
    (client, recorder)
}

#[tokio::test]
async fn group_a_partial_failure_reports_each_job_and_continues_in_order() {
    let (client, recorder) = recording_client(vec![
        print_deleted(),
        Response::json_response(404, r#"{"status":404,"message":"Not found"}"#),
        email_deleted(),
    ]);
    let transmitted_at = Instant::now();
    let mut batch = CancellationBatch::new();
    for (job, expected_kind) in [
        (
            CancellationJob::print(id("10"), transmitted_at),
            "printjobs",
        ),
        (
            CancellationJob::print(id("20"), transmitted_at),
            "printjobs",
        ),
        (
            CancellationJob::email(id("30"), transmitted_at),
            "emailjobs",
        ),
    ] {
        let job = job.expect("cancellation job");
        assert_eq!(job.kind().as_str(), expected_kind);
        batch.push(job).expect("unique job");
    }

    let report = client.cancel_batch(&batch).await;

    assert_eq!(report.len(), 3);
    assert_eq!(report.deleted(), 2);
    assert_eq!(report.failed(), 1);
    assert_eq!(report.window_closed(), 0);
    assert_eq!(report.results()[0].id().as_str(), "10");
    assert_eq!(
        report.results()[0].outcome().message(),
        Some("Print job deleted successfully")
    );
    assert!(matches!(
        report.results()[1].outcome(),
        CancellationOutcome::Failed(Error::NotFound { .. })
    ));
    assert_eq!(report.results()[2].id().as_str(), "30");
    assert!(report.results()[2].outcome().is_deleted());

    let requests = recorder.requests();
    assert_eq!(requests.len(), 3, "one DELETE per job, no retry");
    assert_eq!(
        requests[0].path_and_query().as_deref(),
        Some("/v3/printjobs/10")
    );
    assert_eq!(
        requests[1].path_and_query().as_deref(),
        Some("/v3/printjobs/20")
    );
    assert_eq!(
        requests[2].path_and_query().as_deref(),
        Some("/v3/emailjobs/30")
    );
    assert!(requests
        .iter()
        .all(|request| request.method == letterxpress::Method::Delete));
    assert!(requests
        .iter()
        .all(|request| request.json().expect("json")["auth"]["mode"] == "test"));
}

#[tokio::test]
async fn group_b_entirely_failed_batch_returns_typed_failures_and_keeps_trying() {
    let (client, recorder) = recording_client(vec![
        Response::json_response(429, r#"{"status":429,"message":"Too Many Requests"}"#),
        Response::json_response(500, r#"{"status":500,"message":"Internal Server Error"}"#),
        Response::json_response(404, r#"{"status":404,"message":"Not found"}"#),
    ]);
    let transmitted_at = Instant::now();
    let mut batch = CancellationBatch::new();
    batch
        .push(CancellationJob::print(id("1"), transmitted_at).expect("job"))
        .expect("first");
    batch
        .push(CancellationJob::email(id("2"), transmitted_at).expect("job"))
        .expect("second");
    batch
        .push(CancellationJob::print(id("3"), transmitted_at).expect("job"))
        .expect("third");

    let report = client.cancel_batch(&batch).await;

    assert_eq!(report.len(), 3);
    assert_eq!(report.deleted(), 0);
    assert_eq!(report.failed(), 3);
    assert_eq!(report.window_closed(), 0);
    assert!(matches!(
        report.results()[0].outcome(),
        CancellationOutcome::Failed(Error::RateLimited { .. })
    ));
    assert_eq!(
        report.results()[1]
            .outcome()
            .error()
            .and_then(Error::status),
        Some(500)
    );
    assert_eq!(
        report.results()[2]
            .outcome()
            .error()
            .and_then(Error::status),
        Some(404)
    );
    assert_eq!(
        recorder.requests().len(),
        3,
        "a failure does not stop the batch"
    );
}

#[tokio::test]
async fn empty_batch_is_valid_and_sends_nothing() {
    let (client, recorder) = recording_client(Vec::new());

    let report = client.cancel_batch(&CancellationBatch::new()).await;

    assert!(report.is_empty());
    assert_eq!(report.len(), 0);
    assert_eq!(report.deleted(), 0);
    assert_eq!(report.failed(), 0);
    assert_eq!(report.window_closed(), 0);
    assert!(report.results().is_empty());
    assert_eq!(recorder.requests().len(), 0);
}

#[tokio::test]
async fn just_inside_the_window_uses_the_wire_and_just_outside_is_skipped() {
    // The helper samples `Instant::now()` internally. Starting 899 seconds ago
    // is just inside; 901 seconds ago leaves a full second of margin beyond the
    // exclusive 900-second boundary, so the test cannot flake while still
    // proving both sides of the limit.
    let (client, recorder) = recording_client(vec![print_deleted()]);
    let mut batch = CancellationBatch::new();
    batch
        .push(
            CancellationJob::print(
                id("inside"),
                Instant::now()
                    .checked_sub(Duration::from_secs(899))
                    .expect("an instant before now"),
            )
            .expect("job"),
        )
        .expect("inside");
    batch
        .push(
            CancellationJob::print(
                id("outside"),
                Instant::now()
                    .checked_sub(Duration::from_secs(901))
                    .expect("an instant before now"),
            )
            .expect("job"),
        )
        .expect("outside");

    let report = client.cancel_batch(&batch).await;

    assert_eq!(report.len(), 2);
    assert_eq!(report.deleted(), 1);
    assert_eq!(report.failed(), 0);
    assert_eq!(report.window_closed(), 1);
    assert!(report.results()[0].outcome().is_deleted());
    assert!(matches!(
        report.results()[1].outcome(),
        CancellationOutcome::WindowClosed {
            transmitted_at,
            deadline,
        } if deadline.duration_since(*transmitted_at) == CANCELLATION_WINDOW
    ));
    let requests = recorder.requests();
    assert_eq!(requests.len(), 1, "the expired job never reaches the wire");
    assert_eq!(
        requests[0].path_and_query().as_deref(),
        Some("/v3/printjobs/inside")
    );
}

#[test]
fn exact_window_boundaries_are_inclusive_at_start_and_exclusive_at_end() {
    let transmitted_at = Instant::now();
    let job = CancellationJob::print(id("1"), transmitted_at).expect("job");

    assert!(job.is_in_window_at(transmitted_at));
    assert!(job.is_in_window_at(transmitted_at + Duration::from_secs(899)));
    assert!(!job.is_in_window_at(transmitted_at + CANCELLATION_WINDOW));
    assert_eq!(job.remaining_at(transmitted_at), CANCELLATION_WINDOW);
    assert_eq!(job.remaining_at(job.deadline()), Duration::ZERO);
    assert_eq!(job.deadline(), transmitted_at + CANCELLATION_WINDOW);
    assert_eq!(job.transmitted_at(), transmitted_at);
}

#[test]
fn batch_capacity_accepts_the_ceiling_and_rejects_one_above() {
    assert!(CancellationBatch::try_with_capacity(MAX_CANCELLATION_BATCH).is_ok());
    assert!(CancellationBatch::try_with_capacity(MAX_CANCELLATION_BATCH + 1).is_err());
}

#[tokio::test]
async fn full_batch_is_bounded_at_one_minute_of_the_rate_limit() {
    let script = (0..MAX_CANCELLATION_BATCH)
        .map(|_| print_deleted())
        .collect();
    let (client, recorder) = recording_client(script);
    let transmitted_at = Instant::now();
    let mut batch =
        CancellationBatch::try_with_capacity(MAX_CANCELLATION_BATCH).expect("batch capacity");
    for index in 0..MAX_CANCELLATION_BATCH {
        batch
            .push(CancellationJob::print(id(&format!("{index}")), transmitted_at).expect("job"))
            .expect("within cap");
    }
    assert!(batch
        .push(CancellationJob::print(id("121"), transmitted_at).expect("job"))
        .is_err());

    let report = client.cancel_batch(&batch).await;

    assert_eq!(report.len(), MAX_CANCELLATION_BATCH);
    assert_eq!(report.deleted(), MAX_CANCELLATION_BATCH);
    assert_eq!(recorder.requests().len(), MAX_CANCELLATION_BATCH);
}

#[tokio::test]
async fn transport_failure_is_one_per_job_result_and_no_retry_happens() {
    let (client, recorder) = recording_client(vec![
        Response::json_response(200, r#"{"status":500,"message":"Unavailable"}"#),
        print_deleted(),
    ]);
    let transmitted_at = Instant::now();
    let mut batch = CancellationBatch::new();
    batch
        .push(CancellationJob::print(id("1"), transmitted_at).expect("job"))
        .expect("first");
    batch
        .push(CancellationJob::email(id("2"), transmitted_at).expect("job"))
        .expect("second");

    let report = client.cancel_batch(&batch).await;

    assert!(matches!(
        report.results()[0].outcome(),
        CancellationOutcome::Failed(Error::Upstream { status: 500, .. })
    ));
    assert_eq!(
        report.results()[0]
            .outcome()
            .error()
            .map(|error| error.is_retryable_for(true)),
        Some(true),
        "DELETE is idempotent, so the error exposes safe retry guidance"
    );
    assert!(report.results()[1].outcome().is_deleted());
    assert_eq!(recorder.requests().len(), 2);
}

#[test]
fn transport_kind_stays_typed_in_a_failed_outcome() {
    let outcome = CancellationOutcome::Failed(Error::Transport {
        kind: TransportKind::Connect,
        source: Box::new(std::io::Error::other("offline")),
    });
    assert!(outcome.error().is_some());
    assert_eq!(outcome.message(), None);
    assert!(!outcome.is_deleted());
}
