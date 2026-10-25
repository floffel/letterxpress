//! Concurrency: one client, many tasks, no torn state and no duplicated send.
//!
//! The transport is the only shared thing — the client itself keeps no mutable
//! state — so a clone is another handle on the same provider account, and these
//! tests drive exactly that: 32 tasks submitting at once, plus 32 tasks reading.

mod common;

use std::sync::Arc;

use common::{pdf, MockTransport, RecordingTransport, Response};
use letterxpress::{Client, Credentials, Letter, Mode};

fn submit_response() -> Response {
    Response::json_response(
        200,
        r#"{"status":200,"message":"OK","data":{"id":1,"status":"queue","items":[]}}"#,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_concurrent_submits_each_send_exactly_one_request() {
    let recorder = RecordingTransport::new(MockTransport::always(submit_response()));
    let client = Arc::new(Client::new(
        recorder.clone(),
        Credentials::new("kunde", "key", Mode::Live).expect("credentials"),
    ));

    let mut handles = Vec::new();
    for index in 0..32 {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            let letter = Letter::new(&pdf())
                .expect("letter")
                .with_notice(format!("concurrent-{index}"))
                .expect("notice");
            client.submit_print_job(&letter).await.map(|job| job.id)
        }));
    }

    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.await.expect("task").expect("submit"));
    }

    assert_eq!(ids.len(), 32);
    assert_eq!(
        recorder.requests().len(),
        32,
        "one request per submit, no duplicates and no drops"
    );

    let notices: std::collections::HashSet<String> = recorder
        .requests()
        .iter()
        .map(|request| {
            request.json().expect("json")["letter"]["notice"]
                .as_str()
                .expect("notice")
                .to_string()
        })
        .collect();
    assert_eq!(notices.len(), 32, "each task sent its own letter");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_concurrent_reads_do_not_interleave_their_answers() {
    let recorder = RecordingTransport::new(MockTransport::always(Response::json_response(
        200,
        r#"{"status":200,"message":"OK","data":{"balance":54.89,"currency":"EUR"}}"#,
    )));
    let client = Arc::new(Client::new(
        recorder.clone(),
        Credentials::new("kunde", "key", Mode::Test).expect("credentials"),
    ));

    let mut handles = Vec::new();
    for _ in 0..32 {
        let client = Arc::clone(&client);
        handles.push(tokio::spawn(async move { client.balance().await }));
    }

    for handle in handles {
        let balance = handle.await.expect("task").expect("balance");
        assert_eq!(balance.cents(), 5_489);
        assert_eq!(balance.currency.as_deref(), Some("EUR"));
    }
    assert_eq!(recorder.requests().len(), 32);
}

#[test]
fn the_shared_types_are_send_sync_and_static() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}

    assert_send_sync::<Client<MockTransport>>();
    assert_send_sync::<Client<RecordingTransport<MockTransport>>>();
    assert_send_sync::<Letter>();
    assert_send_sync::<Credentials>();
    assert_send_sync::<letterxpress::Error>();
    assert_send_sync::<serde_json::Value>();
}
