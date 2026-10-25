//! Shared helpers for the integration tests.
//!
//! Every test drives the public API through a scripted transport: no socket, no
//! network, no credentials.

#![allow(dead_code)]

pub use letterxpress::transport::{MockTransport, RecordingTransport, Response};
use letterxpress::{Client, Credentials, Mode};

/// Loads one fixture that quotes the provider documentation.
pub fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("fixture {path}: {error}"))
}

/// A `200 application/json` response built from a fixture.
pub fn fixture_response(name: &str) -> Response {
    Response::json_response(200, fixture(name))
}

/// A test client with a scripted transport.
pub fn client_with(script: Vec<Response>) -> Client<MockTransport> {
    Client::new(
        MockTransport::new(script.into_iter().map(Ok).collect::<Vec<_>>()),
        Credentials::new("kunde", "test-key", Mode::Test).expect("credentials"),
    )
}

/// A client whose transport records every request it sees.
pub fn recording_client(
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

/// A client that answers everything with the same body.
pub fn always_client(body: &str) -> Client<MockTransport> {
    Client::new(
        MockTransport::always(Response::json_response(200, body.to_string())),
        Credentials::new("kunde", "test-key", Mode::Test).expect("credentials"),
    )
}

/// A minimal PDF-shaped payload, good enough for every local check.
pub fn pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n".to_vec()
}
