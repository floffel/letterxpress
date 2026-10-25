//! Pluggable HTTP transport, so the client is testable without a socket.
//!
//! The trait is one method wide: [`Transport::execute`] takes a [`Request`] and
//! returns a [`Response`]. Everything above it — URL building, the JSON
//! envelope, error classification, idempotency — lives in [`crate::Client`], so
//! a mock transport exercises every branch offline.
//!
//! Three implementations ship with the crate:
//!
//! * [`ReqwestTransport`] (feature `http`) — the real one.
//! * [`MockTransport`] — scripted responses, deterministic order, no I/O.
//! * [`RecordingTransport`] — wraps any transport and keeps what was sent, so
//!   tests can assert on the exact wire payload.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;

use crate::{Error, Result, TransportKind};

/// HTTP method. LetterXpress uses all four.
///
/// ```
/// use letterxpress::transport::Method;
///
/// assert_eq!(Method::Get.as_str(), "GET");
/// assert_eq!(Method::Delete.as_str(), "DELETE");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// Read.
    Get,
    /// Create (and the two submit endpoints).
    Post,
    /// Update.
    Put,
    /// Delete.
    Delete,
}

impl Method {
    /// The uppercase method name.
    ///
    /// ```
    /// assert_eq!(letterxpress::transport::Method::Put.as_str(), "PUT");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    /// Whether repeating the same request has the same effect as sending it
    /// once.
    ///
    /// `POST` is the only non-idempotent verb here, and it is exactly the one
    /// that creates a billed letter.
    ///
    /// ```
    /// use letterxpress::transport::Method;
    ///
    /// assert!(!Method::Post.is_idempotent());
    /// assert!(Method::Get.is_idempotent());
    /// assert!(Method::Put.is_idempotent());
    /// assert!(Method::Delete.is_idempotent());
    /// ```
    #[must_use]
    pub fn is_idempotent(self) -> bool {
        !matches!(self, Self::Post)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One API call, transport agnostic.
///
/// The body is owned bytes rather than `&[u8]` borrowed from the caller: it is
/// built (and authenticated) inside the client and handed over whole.
///
/// ```
/// use letterxpress::transport::{Method, Request};
///
/// let request = Request::new(Method::Get, "https://api.letterxpress.de/v3/balance", Some(br#"{"auth":{}}"#.to_vec()))
///     .with_header("Content-Type", "application/json");
/// assert_eq!(request.method, Method::Get);
/// assert_eq!(request.header("content-type"), Some("application/json"));
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    /// HTTP verb.
    pub method: Method,
    /// Absolute URL, including any query string.
    pub url: String,
    /// Body bytes; `None` for a bodyless call.
    pub body: Option<Vec<u8>>,
    /// Additional headers as `(name, value)`, in insertion order.
    pub headers: Vec<(String, String)>,
    /// Per-request timeout; `None` leaves the transport default in place.
    pub timeout: Option<Duration>,
}

impl Request {
    /// A request with a body and no extra headers.
    ///
    /// ```
    /// use letterxpress::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Post, "https://example.test/v3/printjobs", Some(b"{}".to_vec()));
    /// assert!(request.body.is_some());
    /// ```
    #[must_use]
    pub fn new(method: Method, url: impl Into<String>, body: Option<Vec<u8>>) -> Self {
        Self {
            method,
            url: url.into(),
            body,
            headers: Vec::new(),
            timeout: None,
        }
    }

    /// Adds one header.
    ///
    /// ```
    /// use letterxpress::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.test/", None)
    ///     .with_header("Content-Type", "application/json");
    /// assert_eq!(request.header("Content-Type"), Some("application/json"));
    /// ```
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Case-insensitive header lookup.
    ///
    /// ```
    /// use letterxpress::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.test/", None)
    ///     .with_header("Content-Type", "application/json");
    /// assert_eq!(request.header("CONTENT-TYPE"), Some("application/json"));
    /// assert_eq!(request.header("accept"), None);
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Attaches a per-request timeout.
    ///
    /// ```
    /// use letterxpress::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Get, "https://example.test/", None)
    ///     .with_timeout(Some(std::time::Duration::from_secs(5)));
    /// assert_eq!(request.timeout, Some(std::time::Duration::from_secs(5)));
    /// ```
    #[must_use]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
}

impl fmt::Debug for Request {
    /// Prints method, URL, headers and body **length** — never the body, which
    /// carries the API key and a base64 letter.
    ///
    /// ```
    /// use letterxpress::transport::{Method, Request};
    ///
    /// let request = Request::new(Method::Post, "https://example.test/", Some(br#"{"auth":{"apikey":"secret"}}"#.to_vec()));
    /// let debug = format!("{request:?}");
    /// assert!(!debug.contains("secret"));
    /// assert!(debug.contains("28 bytes"));
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field(
                "body",
                &self
                    .body
                    .as_ref()
                    .map(|body| format!("{} bytes", body.len())),
            )
            .field("headers", &self.headers)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// A response as the client needs it: status, headers, body.
///
/// Header names are lower-cased on construction and duplicates are preserved.
///
/// ```
/// use letterxpress::transport::Response;
///
/// let response = Response::new(200, [("Retry-After".to_string(), "30".to_string())], br#"{"status":200}"#.to_vec());
/// assert!(response.is_success());
/// assert_eq!(response.header("retry-after"), Some("30"));
/// assert_eq!(response.json().unwrap()["status"], 200);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    /// Builds a response, lower-casing header names.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let response = Response::new(204, std::iter::empty(), Vec::new());
    /// assert_eq!(response.body(), b"");
    /// ```
    pub fn new(
        status: u16,
        headers: impl IntoIterator<Item = (String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            headers: headers
                .into_iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value))
                .collect(),
            body,
        }
    }

    /// A JSON response with `Content-Type: application/json`.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let response = Response::json_response(200, r#"{"status":200,"message":"OK"}"#);
    /// assert_eq!(response.header("content-type"), Some("application/json"));
    /// assert_eq!(response.json().unwrap()["status"], 200);
    /// ```
    #[must_use]
    pub fn json_response(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::new(
            status,
            [("Content-Type".to_string(), "application/json".to_string())],
            body.into(),
        )
    }

    /// HTTP status code.
    ///
    /// ```
    /// assert_eq!(letterxpress::transport::Response::new(429, std::iter::empty(), Vec::new()).status(), 429);
    /// ```
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// First value of a header, case-insensitively.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let response = Response::new(200, [("X-Rate".to_string(), "9".to_string())], Vec::new());
    /// assert_eq!(response.header("x-rate"), Some("9"));
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Raw body bytes.
    ///
    /// ```
    /// assert_eq!(letterxpress::transport::Response::new(200, std::iter::empty(), b"hi".to_vec()).body(), b"hi");
    /// ```
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether the status is 2xx.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// assert!(Response::new(200, std::iter::empty(), Vec::new()).is_success());
    /// assert!(!Response::new(503, std::iter::empty(), Vec::new()).is_success());
    /// ```
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parses the body as JSON; an empty body becomes `null`.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] when the body is not valid JSON.
    ///
    /// ```
    /// use letterxpress::transport::Response;
    ///
    /// let response = Response::json_response(200, r#"{"status":200}"#);
    /// assert_eq!(response.json().unwrap()["status"], 200);
    /// assert!(Response::new(200, std::iter::empty(), Vec::new()).json().unwrap().is_null());
    /// ```
    pub fn json(&self) -> Result<Value> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&self.body).map_err(Error::from)
    }
}

/// Performs one HTTP request. Implement it for your own stack, or use
/// [`ReqwestTransport`].
///
/// Implementations must:
///
/// * send exactly the headers in [`Request::headers`], including
///   `Content-Type: application/json`,
/// * send the body with every verb (the provider authenticates even `GET` and
///   `DELETE` through a JSON body),
/// * return the raw status, headers and body even for non-2xx — the client
///   decides what is retryable,
/// * classify connection failures as [`Error::Transport`] with the right
///   [`TransportKind`], because the ambiguity of a submit depends on it.
///
/// ```
/// use letterxpress::transport::{Request, Response, Transport};
/// use letterxpress::Result;
///
/// struct AlwaysOk;
///
/// impl Transport for AlwaysOk {
///     async fn execute(&self, _request: Request) -> Result<Response> {
///         Ok(Response::json_response(200, r#"{"status":200,"message":"OK"}"#))
///     }
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait Transport {
    /// Executes `request` and returns the response, whatever its status.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] when no response could be obtained.
    fn execute(&self, request: Request) -> impl std::future::Future<Output = Result<Response>>;
}

/// Locks a mutex without panicking on poisoning.
///
/// Every lock in this crate guards a log or a script queue; a panic in another
/// task cannot have left either half-updated, so recovering is correct and
/// keeps the "no panics in library code" rule.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Scripted transport for tests: returns queued responses in order.
///
/// Cheap to clone; clones share the same queue, which is what makes the
/// concurrency test possible.
///
/// ```
/// use letterxpress::transport::{Method, MockTransport, Request, Response, Transport};
///
/// # async fn demo() -> Result<(), letterxpress::Error> {
/// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"balance":1.5,"currency":"EUR"}}"#))]);
/// let response = mock.execute(Request::new(Method::Get, "https://example.test/", None)).await?;
/// assert_eq!(response.status(), 200);
/// assert_eq!(mock.remaining(), 0);
/// // Script exhausted: a protocol error, never a panic.
/// assert!(mock.execute(Request::new(Method::Get, "https://example.test/", None)).await.is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct MockTransport {
    state: Arc<Mutex<MockState>>,
}

/// Queue plus optional catch-all answer.
#[derive(Debug, Default)]
struct MockState {
    script: VecDeque<Result<Response>>,
    fallback: Option<Response>,
}

impl MockTransport {
    /// Builds a mock from scripted results.
    ///
    /// ```
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// let mock = MockTransport::new([Ok(Response::json_response(200, "{}"))]);
    /// assert_eq!(mock.remaining(), 1);
    /// ```
    pub fn new(script: impl IntoIterator<Item = Result<Response>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                script: script.into_iter().collect(),
                fallback: None,
            })),
        }
    }

    /// A mock that answers every call with the same response.
    ///
    /// ```
    /// use letterxpress::transport::{Method, MockTransport, Request, Response, Transport};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::always(Response::json_response(500, "{}"));
    /// for _ in 0..3 {
    ///     let request = Request::new(Method::Get, "https://example.test/", None);
    ///     assert_eq!(mock.execute(request).await?.status(), 500);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn always(response: Response) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState {
                script: VecDeque::new(),
                fallback: Some(response),
            })),
        }
    }

    /// Queues one more scripted result.
    ///
    /// ```
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// let mut mock = MockTransport::default();
    /// mock.push(Ok(Response::json_response(200, "{}")));
    /// assert_eq!(mock.remaining(), 1);
    /// ```
    pub fn push(&mut self, result: Result<Response>) {
        lock(&self.state).script.push_back(result);
    }

    /// How many scripted results are left (a catch-all answer does not count).
    ///
    /// ```
    /// assert_eq!(letterxpress::transport::MockTransport::default().remaining(), 0);
    /// ```
    #[must_use]
    pub fn remaining(&self) -> usize {
        lock(&self.state).script.len()
    }
}

impl Transport for MockTransport {
    async fn execute(&self, request: Request) -> Result<Response> {
        // The script resolves immediately, but the call is still an await point
        // so the mock behaves like the real transports under a scheduler.
        std::future::ready(()).await;
        let next = {
            let mut state = lock(&self.state);
            state
                .script
                .pop_front()
                .or_else(|| state.fallback.clone().map(Ok))
        };
        match next {
            Some(result) => result,
            None => Err(Error::Transport {
                kind: TransportKind::Protocol,
                message: format!(
                    "mock transport has no scripted response left for {} {}",
                    request.method, request.url
                ),
            }),
        }
    }
}

/// What one call put on the wire, as recorded by [`RecordingTransport`].
///
/// ```
/// use letterxpress::transport::Method;
///
/// let recorded = letterxpress::transport::RecordedRequest {
///     method: Method::Post,
///     url: "https://example.test/v3/printjobs".to_string(),
///     body: br#"{"auth":{}}"#.to_vec(),
///     headers: Vec::new(),
/// };
/// assert_eq!(recorded.json()?["auth"], serde_json::json!({}));
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Verb that was used.
    pub method: Method,
    /// Full URL, including query string.
    pub url: String,
    /// Body bytes exactly as handed to the transport.
    pub body: Vec<u8>,
    /// Headers that were set.
    pub headers: Vec<(String, String)>,
}

impl RecordedRequest {
    /// The body parsed as JSON.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] when the body is not valid JSON.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest { method: Method::Get, url: String::new(), body: b"not json".to_vec(), headers: Vec::new() };
    /// assert!(recorded.json().is_err());
    /// ```
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(Error::from)
    }

    /// The path plus query string, for assertions that do not care about the
    /// host.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest { method: Method::Get, url: "https://api.letterxpress.de/v3/printjobs?filter=hold".to_string(), body: Vec::new(), headers: Vec::new() };
    /// assert_eq!(recorded.path_and_query(), "/v3/printjobs?filter=hold");
    /// ```
    #[must_use]
    pub fn path_and_query(&self) -> &str {
        match self.url.find("://") {
            Some(scheme_end) => {
                let after_scheme = &self.url[scheme_end + 3..];
                match after_scheme.find('/') {
                    Some(path_start) => &after_scheme[path_start..],
                    None => "/",
                }
            }
            None => self.url.as_str(),
        }
    }
}

/// Wraps a transport and records every request it sees.
///
/// Clones share the log, so a test can keep a handle while the client owns
/// another.
///
/// ```
/// use letterxpress::transport::{Method, MockTransport, RecordingTransport, Request, Response, Transport};
///
/// # async fn demo() -> Result<(), letterxpress::Error> {
/// let recorder = RecordingTransport::new(MockTransport::new([Ok(Response::json_response(200, "{}"))]));
/// let transport = recorder.clone();
/// transport.execute(Request::new(Method::Delete, "https://example.test/v3/printjobs/1", None)).await?;
/// assert_eq!(recorder.requests()[0].method, Method::Delete);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RecordingTransport<T> {
    inner: T,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl<T> RecordingTransport<T> {
    /// Wraps `inner`.
    ///
    /// ```
    /// use letterxpress::transport::{MockTransport, RecordingTransport};
    ///
    /// let recorder = RecordingTransport::new(MockTransport::default());
    /// assert!(recorder.requests().is_empty());
    /// ```
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A snapshot of everything recorded so far, in order.
    ///
    /// ```
    /// let recorder = letterxpress::transport::RecordingTransport::new(letterxpress::transport::MockTransport::default());
    /// assert_eq!(recorder.requests().len(), 0);
    /// ```
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.log).clone()
    }

    /// Empties the log and returns what was in it.
    ///
    /// ```
    /// let recorder = letterxpress::transport::RecordingTransport::new(letterxpress::transport::MockTransport::default());
    /// assert!(recorder.take_requests().is_empty());
    /// ```
    pub fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(&mut *lock(&self.log))
    }
}

impl<T: Transport> Transport for RecordingTransport<T> {
    async fn execute(&self, request: Request) -> Result<Response> {
        {
            let mut log = lock(&self.log);
            log.push(RecordedRequest {
                method: request.method,
                url: request.url.clone(),
                body: request.body.clone().unwrap_or_default(),
                headers: request.headers.clone(),
            });
        }
        self.inner.execute(request).await
    }
}

/// `reqwest`-based transport, enabled by the default `http` feature.
///
/// The inner client is cheap to clone and safe to share across tasks.
///
/// ```no_run
/// # #[cfg(feature = "http")]
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use letterxpress::{Client, Credentials, Mode, ReqwestTransport};
///
/// let client = Client::new(
///     ReqwestTransport::new(),
///     Credentials::new("kunde", std::env::var("LETTERXPRESS_APIKEY")?, Mode::Test)?,
/// );
/// println!("balance: {} EUR", client.balance().await?.balance);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "http")]
#[derive(Debug, Clone, Default)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(feature = "http")]
impl ReqwestTransport {
    /// Creates a transport with a default client (rustls, no redirects
    /// followed — a redirect would move the credential to another host).
    ///
    /// ```no_run
    /// # #[cfg(feature = "http")]
    /// let transport = letterxpress::ReqwestTransport::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        }
    }

    /// Reuses an existing `reqwest::Client` (for connection pooling with the
    /// rest of the application).
    ///
    /// ```no_run
    /// # #[cfg(feature = "http")]
    /// let transport = letterxpress::ReqwestTransport::with_client(reqwest::Client::new());
    /// ```
    #[must_use]
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[cfg(feature = "http")]
impl Transport for ReqwestTransport {
    async fn execute(&self, request: Request) -> Result<Response> {
        let mut builder = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
            Method::Put => self.client.put(&request.url),
            Method::Delete => self.client.delete(&request.url),
        };
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder
            .send()
            .await
            .map_err(|error| describe_reqwest_error(&error))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>();
        let body = response
            .bytes()
            .await
            .map_err(|error| describe_reqwest_error(&error))?
            .to_vec();

        Ok(Response::new(status, headers, body))
    }
}

/// Maps a `reqwest` failure to a typed transport error without echoing the
/// request body (which carries the API key).
#[cfg(feature = "http")]
fn describe_reqwest_error(error: &reqwest::Error) -> Error {
    let kind = if error.is_timeout() {
        TransportKind::Timeout
    } else if error.is_connect() {
        TransportKind::Connect
    } else if error.is_body() || error.is_decode() || error.is_request() {
        TransportKind::Protocol
    } else {
        TransportKind::Io
    };
    // `reqwest::Error` renders the URL but never the request body or headers.
    Error::Transport {
        kind,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_case_insensitive() {
        let request = Request::new(Method::Get, "https://example.test/", None)
            .with_header("Content-Type", "application/json");
        assert_eq!(request.header("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(request.header("accept"), None);
    }

    #[test]
    fn exhausted_mock_is_a_protocol_error_not_a_panic() {
        let mock = MockTransport::default();
        let error = futures_lite_block(mock);
        assert!(matches!(
            error,
            Error::Transport {
                kind: TransportKind::Protocol,
                ..
            }
        ));
    }

    /// Runs one mock call on a tiny current-thread runtime, so the unit test
    /// does not need the `http` feature.
    fn futures_lite_block(mock: MockTransport) -> Error {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            mock.execute(Request::new(Method::Get, "https://example.test/", None))
                .await
                .unwrap_err()
        })
    }

    #[test]
    fn debug_never_prints_the_body() {
        let request = Request::new(
            Method::Post,
            "https://example.test/",
            Some(br#"{"auth":{"apikey":"secret"}}"#.to_vec()),
        );
        let debug = format!("{request:?}");
        assert!(!debug.contains("secret"));
    }
}
