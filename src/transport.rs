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
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;
use std::time::Duration;

use serde_json::Value;
use url::Url;

use crate::{Error, Result, TransportKind};

/// Response bodies are buffered only up to this many bytes by the bundled HTTP
/// transport. A larger declared or streamed body is a terminal protocol error.
///
/// The list and mutation endpoints return compact JSON envelopes. The invoice
/// document endpoint is the sole exception because it embeds a PDF as base64;
/// see [`MAX_INVOICE_RESPONSE_BODY_BYTES`].
pub const MAX_RESPONSE_BODY_BYTES: u64 = 1_048_576;

/// Response-body ceiling for `GET /v3/invoices/{id}`.
///
/// The documented input PDF ceiling is 50 MB. Base64 expands that to roughly
/// 67 MB, so this leaves room for the JSON envelope while still bounding the
/// only response that can legitimately be large.
pub const MAX_INVOICE_RESPONSE_BODY_BYTES: u64 = 70 * 1_048_576;

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
///   [`TransportKind`], because the ambiguity of a submit depends on it. In
///   particular, a failure after the provider answered belongs in
///   [`TransportKind::ResponseBody`], not [`TransportKind::Protocol`].
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
    /// [`Error::Transport`] when the request could not be built or no complete
    /// response could be obtained.
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
    pub fn push(&self, result: Result<Response>) {
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
        // Suspend exactly once before consuming the script. This gives a
        // scheduler a real chance to run another task without requiring Tokio
        // in builds where the mock is the only transport.
        let mut suspended = false;
        poll_fn(|context| {
            if suspended {
                Poll::Ready(())
            } else {
                suspended = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
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
                // `io::Error::other` is the standard way to carry a one-off
                // message with an `Error` impl; the mock has no deeper cause.
                source: Box::new(std::io::Error::other(format!(
                    "mock transport has no scripted response left for {} {}",
                    request.method, request.url
                ))),
            }),
        }
    }
}

/// What one call put on the wire, as recorded by [`RecordingTransport`].
///
/// ```
/// use letterxpress::transport::Method;
///
/// let recorded = letterxpress::transport::RecordedRequest::new(
///     Method::Post,
///     "https://example.test/v3/printjobs",
///     br#"{"auth":{}}"#.to_vec(),
///     Vec::new(),
/// );
/// assert_eq!(recorded.json()?["auth"], serde_json::json!({}));
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Verb that was used.
    pub method: Method,
    /// Full URL, including query string.
    pub url: String,
    /// Body bytes exactly as handed to the transport.
    ///
    /// Private on purpose: the body carries the API key and a base64 letter, so
    /// the redacting [`Debug`] impl must not be bypassable through a struct
    /// literal. Use [`RecordedRequest::body`] to read it.
    body: Vec<u8>,
    /// Headers that were set.
    pub headers: Vec<(String, String)>,
}

impl RecordedRequest {
    /// Creates a recorded request value.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest::new(Method::Get, "https://example.test/x", b"{}".to_vec(), Vec::new());
    /// assert_eq!(recorded.body(), b"{}");
    /// ```
    #[must_use]
    pub fn new(
        method: Method,
        url: impl Into<String>,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            method,
            url: url.into(),
            body,
            headers,
        }
    }

    /// Body bytes exactly as handed to the transport.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest::new(Method::Get, "https://example.test/x", b"{}".to_vec(), Vec::new());
    /// assert_eq!(recorded.body(), b"{}");
    /// ```
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body parsed as JSON.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] when the body is not valid JSON.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest::new(Method::Get, String::new(), b"not json".to_vec(), Vec::new());
    /// assert!(recorded.json().is_err());
    /// ```
    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(Error::from)
    }

    /// The path plus query string, for assertions that do not care about the
    /// host.
    ///
    /// The URL is parsed rather than split by hand, so userinfo, a port and a
    /// query on a pathless URL all land on the right side. The fragment, if
    /// any, is not part of the result, and the path is normalised the way
    /// [`url`](https://docs.rs/url) normalises it (non-ASCII bytes are
    /// percent-encoded).
    ///
    /// Returns `None` when [`url`](Self::url) is not an absolute URL.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest::new(
    ///     Method::Get,
    ///     "https://api.letterxpress.de/v3/printjobs?filter=hold".to_string(),
    ///     Vec::new(),
    ///     Vec::new(),
    /// );
    /// assert_eq!(recorded.path_and_query().as_deref(), Some("/v3/printjobs?filter=hold"));
    ///
    /// // A query on a pathless URL is kept, and a relative URL is rejected.
    /// let pathless = RecordedRequest::new(Method::Get, "https://api.letterxpress.de?filter=hold", Vec::new(), Vec::new());
    /// assert_eq!(pathless.path_and_query().as_deref(), Some("/?filter=hold"));
    ///
    /// let relative = RecordedRequest::new(Method::Get, "/v3/balance", Vec::new(), Vec::new());
    /// assert_eq!(relative.path_and_query(), None);
    /// ```
    #[must_use]
    pub fn path_and_query(&self) -> Option<String> {
        let parsed = Url::parse(&self.url).ok()?;
        let mut target = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            target.push('?');
            target.push_str(query);
        }
        Some(target)
    }
}

impl fmt::Debug for RecordedRequest {
    /// Prints method, URL, headers and body **length** — never the body, which
    /// carries the API key and a base64 letter.
    ///
    /// ```
    /// use letterxpress::transport::{Method, RecordedRequest};
    ///
    /// let recorded = RecordedRequest::new(
    ///     Method::Post,
    ///     "https://example.test/",
    ///     br#"{"auth":{"apikey":"secret"}}"#.to_vec(),
    ///     Vec::new(),
    /// );
    /// let debug = format!("{recorded:?}");
    /// assert!(!debug.contains("secret"));
    /// assert!(debug.contains("28 bytes"));
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordedRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("body", &format!("{} bytes", self.body.len()))
            .field("headers", &self.headers)
            .finish()
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
            log.push(RecordedRequest::new(
                request.method,
                request.url.clone(),
                request.body.clone().unwrap_or_default(),
                request.headers.clone(),
            ));
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
///     ReqwestTransport::new()?,
///     Credentials::new("kunde", std::env::var("LETTERXPRESS_APIKEY")?, Mode::Test)?,
/// );
/// println!("balance: {} EUR", client.balance().await?.balance);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "http")]
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

#[cfg(feature = "http")]
impl ReqwestTransport {
    /// Creates a transport with a default client (rustls, no redirects
    /// followed — a redirect would move the credential to another host).
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] with [`TransportKind::Protocol`] when the hardened
    /// client cannot be built. It never falls back to a default client, which
    /// would follow redirects and carry the credential-bearing body to another
    /// host.
    ///
    /// ```no_run
    /// # #[cfg(feature = "http")]
    /// # fn demo() -> letterxpress::Result<()> {
    /// let transport = letterxpress::ReqwestTransport::new()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Result<Self> {
        let client =
            build_reqwest_client(reqwest::Client::builder()).map_err(describe_reqwest_error)?;
        Ok(Self { client })
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

        let response = builder.send().await.map_err(describe_reqwest_error)?;
        let status = response.status().as_u16();
        let headers = collect_response_headers(response.headers());
        let limit = response_body_limit(&request.url);
        let body = read_response_body(response, limit).await?;

        Ok(Response::new(status, headers, body))
    }
}

/// The invoice-document response is the only endpoint allowed to carry a PDF
/// large enough to exceed the ordinary envelope budget.
#[cfg(feature = "http")]
fn response_body_limit(url: &str) -> u64 {
    let Ok(parsed) = Url::parse(url) else {
        return MAX_RESPONSE_BODY_BYTES;
    };
    let segments: Vec<_> = parsed
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() >= 2 && segments[segments.len() - 2] == "invoices" {
        MAX_INVOICE_RESPONSE_BODY_BYTES
    } else {
        MAX_RESPONSE_BODY_BYTES
    }
}

/// Reads a response body in chunks, rejecting the body before reading it when
/// the declared length is too large and while streaming when the length is not
/// known or is wrong. The bytes are copied once into the final `Vec`; unlike
/// `Response::bytes().to_vec()`, there is no complete `Bytes` body held beside
/// a second allocation.
#[cfg(feature = "http")]
async fn read_response_body(mut response: reqwest::Response, limit: u64) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(response_body_too_large(limit));
    }

    let capacity = usize::try_from(response.content_length().unwrap_or_default().min(limit))
        .unwrap_or_default();
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(describe_reqwest_error)? {
        let new_len = body
            .len()
            .checked_add(chunk.len())
            .filter(|length| u64::try_from(*length).is_ok_and(|length| length <= limit));
        let Some(new_len) = new_len else {
            return Err(response_body_too_large(limit));
        };
        body.reserve_exact(new_len - body.len());
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(feature = "http")]
fn response_body_too_large(limit: u64) -> Error {
    Error::Transport {
        kind: TransportKind::ResponseBody,
        source: Box::new(std::io::Error::other(format!(
            "response body exceeds the {limit}-byte limit"
        ))),
    }
}

/// Builds the hardened client the transport uses: the redirect policy is part
/// of the builder, so the only successful construction path has it in place.
#[cfg(feature = "http")]
fn build_reqwest_client(
    builder: reqwest::ClientBuilder,
) -> std::result::Result<reqwest::Client, reqwest::Error> {
    builder.redirect(reqwest::redirect::Policy::none()).build()
}

#[cfg(feature = "http")]
fn collect_response_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|text| (name.as_str().to_string(), text.to_string()))
        })
        .collect()
}

/// Maps a `reqwest` failure to a typed transport error without echoing the
/// request body (which carries the API key).
///
/// The error is moved into the variant rather than formatted: `reqwest::Error`
/// keeps the hyper/rustls cause, and that chain is what tells DNS from TLS from
/// a reset. Its `Display` renders the URL but never the body or the headers.
#[cfg(feature = "http")]
fn describe_reqwest_error(error: reqwest::Error) -> Error {
    let kind = if error.is_timeout() {
        TransportKind::Timeout
    } else if error.is_connect() {
        TransportKind::Connect
    } else if error.is_builder() {
        TransportKind::Protocol
    } else if error.is_body() || error.is_decode() {
        TransportKind::ResponseBody
    } else {
        TransportKind::Io
    };
    Error::Transport {
        kind,
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::task::{Context, Waker};

    #[test]
    fn mock_execute_is_pending_before_it_consumes_the_script() {
        let mock = MockTransport::new([Ok(Response::json_response(200, "{}"))]);
        let mut call =
            Box::pin(mock.execute(Request::new(Method::Get, "https://example.test/", None)));
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(call.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(mock.remaining(), 1, "a suspended call must not consume");

        assert!(matches!(
            call.as_mut().poll(&mut context),
            Poll::Ready(Ok(_))
        ));
        assert_eq!(mock.remaining(), 0);
    }

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

    #[test]
    fn recorded_request_debug_never_prints_the_body() {
        let recorded = RecordedRequest::new(
            Method::Post,
            "https://example.test/",
            br#"{"auth":{"apikey":"secret"}}"#.to_vec(),
            Vec::new(),
        );
        let debug = format!("{recorded:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("28 bytes"));
    }

    #[test]
    fn recorded_request_debug_covers_the_empty_and_one_byte_body_boundary() {
        let empty =
            RecordedRequest::new(Method::Get, "https://example.test/", Vec::new(), Vec::new());
        let debug = format!("{empty:?}");
        assert!(debug.contains("0 bytes"));

        let one_byte = RecordedRequest::new(
            Method::Get,
            "https://example.test/",
            b"Z".to_vec(),
            Vec::new(),
        );
        let debug = format!("{one_byte:?}");
        assert!(debug.contains("1 bytes"));
        assert!(!debug.contains('Z'));
        assert_eq!(one_byte.body(), b"Z");
    }

    /// The reported leak path: `format!("{recorder:?}")` and a failed
    /// `assert_eq!` on `requests()` both render the log through this wrapper.
    #[test]
    fn recorder_debug_never_prints_the_body() {
        let recorder =
            RecordingTransport::new(MockTransport::new([Ok(Response::json_response(200, "{}"))]));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            recorder
                .execute(Request::new(
                    Method::Post,
                    "https://example.test/v3/printjobs",
                    Some(br#"{"auth":{"apikey":"secret"}}"#.to_vec()),
                ))
                .await
                .expect("scripted response");
        });

        let debug = format!("{recorder:?}");
        assert!(!debug.contains("secret"));
        assert!(debug.contains("28 bytes"));

        let rendered_log = format!("{:?}", recorder.requests());
        assert!(!rendered_log.contains("secret"));
        assert_eq!(
            recorder.requests()[0].body(),
            br#"{"auth":{"apikey":"secret"}}"#
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn reqwest_transport_new_builds_the_hardened_client() {
        assert!(ReqwestTransport::new().is_ok());
    }

    #[cfg(feature = "http")]
    #[test]
    fn response_body_limits_distinguish_invoice_documents() {
        assert_eq!(
            response_body_limit("https://api.letterxpress.de/v3/invoices/25402"),
            MAX_INVOICE_RESPONSE_BODY_BYTES
        );
        for ordinary in [
            "https://api.letterxpress.de/v3/invoices",
            "https://api.letterxpress.de/v3/invoices/25402/items",
            "https://api.letterxpress.de/v3/printjobs/1",
            "https://api.letterxpress.de/v3/invoices/",
        ] {
            assert_eq!(
                response_body_limit(ordinary),
                MAX_RESPONSE_BODY_BYTES,
                "{ordinary}"
            );
        }
        assert_eq!(
            response_body_limit("not an absolute URL"),
            MAX_RESPONSE_BODY_BYTES
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn declared_response_bodies_over_the_limit_fail_before_reading() {
        let polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let response = http_response_with_declared_length(
            MAX_RESPONSE_BODY_BYTES + 1,
            std::sync::Arc::clone(&polled),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(read_response_body(response, MAX_RESPONSE_BODY_BYTES))
            .expect_err("the declared length is over the cap");
        assert_response_body_error(&error);
        assert!(error
            .to_string()
            .contains(&MAX_RESPONSE_BODY_BYTES.to_string()));
        assert!(
            !polled.load(std::sync::atomic::Ordering::SeqCst),
            "the body must be rejected before the first poll"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn response_body_reader_accepts_the_exact_limit_and_rejects_one_more_byte() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");

        let at_limit = vec![b'x'; 8];
        let response = http_response_with_length(8, at_limit.clone());
        let body = runtime
            .block_on(read_response_body(response, 8))
            .expect("the exact limit is accepted");
        assert_eq!(body, at_limit);

        // Unknown length exercises the streaming path, and the chunk boundary
        // is intentionally inside the first and second chunks on either side.
        let response = http_response_with_chunks(vec![vec![b'x'; 7], vec![b'y'; 2]]);
        let error = runtime
            .block_on(read_response_body(response, 8))
            .expect_err("one byte past the streamed limit must fail");
        assert_response_body_error(&error);
    }

    #[cfg(feature = "http")]
    #[test]
    fn a_response_body_that_errors_is_a_post_write_kind() {
        let response = http_response_with_failing_body(8);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let error = runtime
            .block_on(read_response_body(response, 16))
            .expect_err("the response body is truncated");

        assert_response_body_error(&error);
        assert!(
            error.to_string().contains("response body"),
            "the body failure stays in the source chain: {error}"
        );
    }

    #[cfg(feature = "http")]
    fn http_response_with_length(length: u64, body: Vec<u8>) -> reqwest::Response {
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .header(http::header::CONTENT_LENGTH, length)
            .body(reqwest::Body::from(body))
            .expect("http response");
        reqwest::Response::from(response)
    }

    #[cfg(feature = "http")]
    fn http_response_with_declared_length(
        length: u64,
        polled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> reqwest::Response {
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .body(reqwest::Body::wrap(DeclaredBody { length, polled }))
            .expect("http response");
        reqwest::Response::from(response)
    }

    #[cfg(feature = "http")]
    struct DeclaredBody {
        length: u64,
        polled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[cfg(feature = "http")]
    impl http_body::Body for DeclaredBody {
        type Data = bytes::Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
        {
            self.polled.store(true, std::sync::atomic::Ordering::SeqCst);
            std::task::Poll::Ready(None)
        }

        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::with_exact(self.length)
        }
    }

    #[cfg(feature = "http")]
    fn http_response_with_chunks(chunks: Vec<Vec<u8>>) -> reqwest::Response {
        let body = reqwest::Body::wrap(ChunkedBody {
            chunks: chunks.into_iter(),
        });
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .body(body)
            .expect("http response");
        reqwest::Response::from(response)
    }

    #[cfg(feature = "http")]
    fn http_response_with_failing_body(declared: u64) -> reqwest::Response {
        let response = http::Response::builder()
            .status(http::StatusCode::OK)
            .body(reqwest::Body::wrap(FailingBody { declared }))
            .expect("http response");
        reqwest::Response::from(response)
    }

    #[cfg(feature = "http")]
    struct FailingBody {
        declared: u64,
    }

    #[cfg(feature = "http")]
    impl http_body::Body for FailingBody {
        type Data = bytes::Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
        {
            std::task::Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "response body ended before its declared length",
            ))))
        }

        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::with_exact(self.declared)
        }
    }

    #[cfg(feature = "http")]
    struct ChunkedBody {
        chunks: std::vec::IntoIter<Vec<u8>>,
    }

    #[cfg(feature = "http")]
    impl http_body::Body for ChunkedBody {
        type Data = bytes::Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>>
        {
            std::task::Poll::Ready(
                self.chunks
                    .next()
                    .map(|chunk| Ok(http_body::Frame::data(bytes::Bytes::from(chunk)))),
            )
        }

        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::default()
        }
    }

    #[cfg(feature = "http")]
    fn assert_response_body_error(error: &Error) {
        assert!(
            matches!(
                error,
                Error::Transport {
                    kind: TransportKind::ResponseBody,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!error.is_retryable());
        assert!(error.is_retryable_for(true));
    }

    /// Runs the same steps as [`ReqwestTransport::new`] with a caller-chosen
    /// builder, so a failing builder is reachable from a test.
    #[cfg(feature = "http")]
    fn built_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
        build_reqwest_client(builder).map_err(describe_reqwest_error)
    }

    #[cfg(feature = "http")]
    #[test]
    fn non_utf8_response_headers_are_dropped_not_emptied() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_bytes(b"\xFF").expect("header value"),
        );
        headers.insert("x-rate", reqwest::header::HeaderValue::from_static("9"));

        let response = Response::new(429, collect_response_headers(&headers), Vec::new());
        assert_eq!(response.header("retry-after"), None);
        assert_eq!(response.header("x-rate"), Some("9"));
    }

    #[cfg(feature = "http")]
    #[test]
    fn reqwest_transport_new_fails_closed_on_a_broken_builder() {
        // No fallback client: the failure is reported, and it is terminal.
        let outcome = built_client(reqwest::Client::builder().user_agent("letterxpress\u{7f}"));
        assert!(
            matches!(
                &outcome,
                Err(Error::Transport {
                    kind: TransportKind::Protocol,
                    ..
                })
            ),
            "a builder failure must be a terminal protocol error, got {outcome:?}"
        );
        if let Err(error) = &outcome {
            assert!(!error.is_retryable());
            assert!(
                !error.is_retryable_for(true),
                "a local adapter failure is terminal even for a read"
            );
        }
    }

    #[cfg(feature = "http")]
    #[test]
    fn reqwest_transport_builder_accepts_a_tab_and_rejects_a_line_feed() {
        // 0x09 (tab) is the last byte `http` accepts in a header value, 0x0A
        // (LF) the next code point and the first one it rejects.
        assert!(
            built_client(reqwest::Client::builder().user_agent("letterxpress/0.1\u{9}")).is_ok()
        );
        let outside = built_client(reqwest::Client::builder().user_agent("letterxpress/0.1\n"));
        assert!(
            matches!(
                &outside,
                Err(Error::Transport {
                    kind: TransportKind::Protocol,
                    ..
                })
            ),
            "the first illegal header byte must fail closed, got {outside:?}"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn a_reqwest_failure_stays_in_the_source_chain() {
        // The reported defect on the real transport: the `reqwest` failure was
        // flattened into a string at the boundary, so nothing above could tell
        // DNS from TLS from a builder fault by type. It is now the variant's
        // `source`.
        let Err(error) = built_client(reqwest::Client::builder().user_agent("letterxpress\u{7f}"))
        else {
            panic!("a control character in a header value must fail");
        };
        assert!(matches!(
            &error,
            Error::Transport {
                kind: TransportKind::Protocol,
                ..
            }
        ));
        let Some(source) = std::error::Error::source(&error) else {
            panic!("the reqwest error must stay reachable");
        };
        assert!(
            source.downcast_ref::<reqwest::Error>().is_some(),
            "the chain carries `reqwest::Error` itself, not its rendering"
        );
        assert_eq!(
            error.to_string(),
            format!("transport error (protocol): {source}"),
            "`Display` is rendered from the source"
        );
    }

    #[cfg(feature = "http")]
    #[test]
    fn utf8_empty_response_header_stays_present_and_empty() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", reqwest::header::HeaderValue::from_static(""));

        let response = Response::new(429, collect_response_headers(&headers), Vec::new());
        assert_eq!(response.header("retry-after"), Some(""));
    }
}
