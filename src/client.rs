//! The client: one method per documented endpoint, all of them over one
//! [`Transport`].
//!
//! Every method builds the documented body (including the `auth` object, which
//! even `GET` and `DELETE` carry), sends it once, and decodes the envelope. No
//! method retries: a submit that may have been processed returns
//! [`Error::Ambiguous`](crate::Error::Ambiguous) instead — after a timeout, a
//! 5xx, a 1xx/3xx answer, or a 2xx whose body does not decode — so a caller
//! cannot double-send a letter by accident.
//!
//! ```no_run
//! # #[cfg(feature = "http")]
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use letterxpress::{Client, Credentials, Mode, PriceRequest, ReqwestTransport};
//!
//! let client = Client::new(
//!     ReqwestTransport::new()?,
//!     Credentials::new("kunde", std::env::var("LETTERXPRESS_APIKEY")?, Mode::Test)?,
//! );
//!
//! let quote = client.price(&PriceRequest::new(2)?).await?;
//! println!("{} for 2 pages", quote.price);
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use url::Url;

use crate::letter::validate_email;
use crate::response::{parse_envelope, Envelope};
use crate::transport::{Method, Request, Transport};
use crate::{
    AmbiguousKind, Balance, Credentials, Date, Deletion, EmailJob, EmailJobPage,
    EmailJobSubmission, InvoiceDocument, InvoiceId, InvoicePage, JobFilter, JobId, Journal,
    JournalType, Letter, Mode, Price, PriceRequest, PrintJob, PrintJobPage, Registered,
    Specification, TransactionFilter, TransactionPage, TransportKind,
};
use crate::{Error, Result};

/// The documented base URL (p. 6 of the v3 documentation).
pub const DEFAULT_BASE_URL: &str = "https://api.letterxpress.de/v3";

/// The authority component of an `https://` URL, before any path, query or
/// fragment. The scheme comparison is ASCII case-insensitive.
fn base_url_authority(url: &str) -> Option<&str> {
    const SCHEME: &str = "https://";

    let scheme = url.get(..SCHEME.len())?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let rest = url.get(SCHEME.len()..)?;
    let end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Documented rate limit: 120 requests per minute (p. 3).
///
/// The client does not throttle — a library that sleeps is a library that
/// surprises its caller. Budget for this in the caller, or react to
/// [`Error::RateLimited`] and wait out [`Error::retry_after`].
pub const RATE_LIMIT_PER_MINUTE: u32 = 120;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A client for the LetterXpress API v3.
///
/// Generic over the transport, so the same code path runs against
/// [`ReqwestTransport`](crate::ReqwestTransport) in production and
/// [`MockTransport`](crate::transport::MockTransport) in tests.
///
/// Prices are quoted in euro; `notice` is the only correlation handle the API
/// offers, so give each letter one.
///
/// ```
/// use letterxpress::{Client, Credentials, Mode};
/// use letterxpress::transport::MockTransport;
///
/// let client = Client::new(
///     MockTransport::default(),
///     Credentials::new("kunde", "key", Mode::Test)?,
/// );
/// assert_eq!(client.base_url(), letterxpress::DEFAULT_BASE_URL);
/// assert_eq!(client.mode(), Mode::Test);
/// # Ok::<(), letterxpress::Error>(())
/// ```
pub struct Client<T> {
    transport: T,
    base_url: String,
    credentials: Credentials,
    timeout: Duration,
}

impl<T> Client<T> {
    /// Builds a client against the documented endpoint.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Live)?);
    /// assert_eq!(client.mode(), Mode::Live);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn new(transport: T, credentials: Credentials) -> Self {
        Self {
            transport,
            base_url: DEFAULT_BASE_URL.to_string(),
            credentials,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Replaces the base URL.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] unless the URL is an absolute `https://` URL with
    /// a non-empty host and a valid port, no userinfo (`user@host`), no query
    /// string and no fragment. The scheme is matched case-insensitively, as URL
    /// schemes are. Credentials are sent to this host on every call, so a
    /// plaintext or ambiguous URL is refused rather than sanitised.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert!(client.clone().with_base_url("http://api.letterxpress.de/v3").is_err());
    /// assert!(client.clone().with_base_url("https://user@api.letterxpress.de/v3").is_err());
    /// assert!(client.clone().with_base_url("https:///v3").is_err());
    /// assert!(client.clone().with_base_url("https://api.letterxpress.de:65536/v3").is_err());
    /// assert!(client.clone().with_base_url("HTTPS://api.letterxpress.de/v3").is_ok());
    /// assert!(client.clone().with_base_url("https://api.letterxpress.de:65535/v3").is_ok());
    /// assert!(client.with_base_url("https://api.letterxpress.de/v3/").is_ok());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        let trimmed = base_url.trim().trim_end_matches('/');
        if trimmed.chars().any(char::is_whitespace) {
            return Err(Error::invalid("base_url", "must not contain whitespace"));
        }
        let parsed = Url::parse(trimmed).map_err(|error| {
            Error::invalid(
                "base_url",
                format!("must be a valid https URL with a host and valid port: {error}"),
            )
        })?;
        if parsed.scheme() != "https" {
            return Err(Error::invalid(
                "base_url",
                "must start with https:// (credentials travel in every body)",
            ));
        }
        if parsed
            .host_str()
            .is_none_or(|host| host.trim_matches('.').is_empty())
        {
            return Err(Error::invalid("base_url", "must name a host"));
        }
        let authority = base_url_authority(trimmed)
            .ok_or_else(|| Error::invalid("base_url", "must name a host"))?;
        if authority.is_empty() {
            return Err(Error::invalid("base_url", "must name a host"));
        }
        if authority.contains('@') {
            return Err(Error::invalid(
                "base_url",
                "must not contain userinfo (credentials belong in Credentials)",
            ));
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err(Error::invalid(
                "base_url",
                "must not contain a query string or fragment",
            ));
        }
        self.base_url = trimmed.to_string();
        Ok(self)
    }

    /// Sets the per-request timeout.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a zero timeout.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode};
    /// use letterxpress::transport::MockTransport;
    ///
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert!(client.clone().with_timeout(std::time::Duration::from_secs(0)).is_err());
    /// assert_eq!(client.with_timeout(std::time::Duration::from_secs(5))?.timeout(), std::time::Duration::from_secs(5));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::invalid("timeout", "must be greater than zero"));
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// The base URL in use.
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.base_url(), "https://api.letterxpress.de/v3");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured mode.
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.mode(), Mode::Test);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.credentials.mode()
    }

    /// The configured timeout.
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.timeout(), std::time::Duration::from_secs(30));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Borrows the transport (to inspect a recorder, for example).
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.transport().remaining(), 0);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Clone> Clone for Client<T> {
    /// Copies the client. The transport clone must share its state (all three
    /// bundled transports do), so a clone is another handle on the same client,
    /// not a second one.
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.clone().mode(), client.mode());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            base_url: self.base_url.clone(),
            credentials: self.credentials.clone(),
            timeout: self.timeout,
        }
    }
}

impl<T> fmt::Debug for Client<T> {
    /// Prints the endpoint, the mode and the timeout — never the key.
    ///
    /// ```
    /// # use letterxpress::{Client, Credentials, Mode};
    /// # use letterxpress::transport::MockTransport;
    /// let client = Client::new(MockTransport::default(), Credentials::new("u", "topsecret", Mode::Live)?);
    /// let debug = format!("{client:?}");
    /// assert!(!debug.contains("topsecret"));
    /// assert!(debug.contains("Live"));
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("mode", &self.credentials.mode())
            .field("timeout", &self.timeout)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Query objects
// ---------------------------------------------------------------------------

/// Validates a 1-based page number shared by every paginated query.
fn validate_page(page: u32) -> Result<()> {
    if page == 0 {
        return Err(Error::invalid("page", "must be at least 1"));
    }
    Ok(())
}

/// Builds a query string from the shared `filter` and `page` parameters.
fn query_string(filter: Option<&str>, page: Option<u32>) -> Option<String> {
    let mut query = String::new();
    if let Some(filter) = filter {
        query.push_str("filter=");
        query.push_str(filter);
    }
    if let Some(page) = page {
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str("page=");
        query.push_str(&page.to_string());
    }
    (!query.is_empty()).then_some(query)
}

/// `GET /v3/printjobs` query: an optional documented filter plus a page.
///
/// ```
/// use letterxpress::{JobFilter, PrintJobQuery};
///
/// let query = PrintJobQuery::new().with_filter(JobFilter::Draft).with_page(2)?;
/// assert_eq!(query.to_query_string(), Some("filter=draft&page=2".to_string()));
/// assert_eq!(PrintJobQuery::new().to_query_string(), None);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrintJobQuery {
    /// `queue`, `hold`, `done`, `canceled` or `draft`.
    pub filter: Option<JobFilter>,
    /// 1-based page number.
    pub page: Option<u32>,
}

impl PrintJobQuery {
    /// No filter, first page.
    ///
    /// ```
    /// assert_eq!(letterxpress::PrintJobQuery::new().page, None);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts to one documented filter value.
    ///
    /// ```
    /// # use letterxpress::{JobFilter, PrintJobQuery};
    /// assert_eq!(PrintJobQuery::new().with_filter(JobFilter::Hold).filter, Some(JobFilter::Hold));
    /// ```
    #[must_use]
    pub fn with_filter(mut self, filter: JobFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Selects a page.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for page 0.
    ///
    /// ```
    /// # use letterxpress::PrintJobQuery;
    /// assert!(PrintJobQuery::new().with_page(0).is_err());
    /// assert!(PrintJobQuery::new().with_page(1).is_ok());
    /// ```
    pub fn with_page(mut self, page: u32) -> Result<Self> {
        validate_page(page)?;
        self.page = Some(page);
        Ok(self)
    }

    /// The query string, without `?`, when anything is set.
    ///
    /// ```
    /// # use letterxpress::{JobFilter, PrintJobQuery};
    /// assert_eq!(PrintJobQuery::new().with_filter(JobFilter::Queue).to_query_string().unwrap(), "filter=queue");
    /// ```
    #[must_use]
    pub fn to_query_string(&self) -> Option<String> {
        query_string(self.filter.map(JobFilter::as_str), self.page)
    }
}

/// `GET /v3/emailjobs` query.
///
/// ```
/// use letterxpress::{EmailJobQuery, JobFilter};
///
/// assert_eq!(EmailJobQuery::new().with_filter(JobFilter::Done).to_query_string().unwrap(), "filter=done");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmailJobQuery {
    /// `queue`, `hold`, `done`, `canceled` or `draft`.
    pub filter: Option<JobFilter>,
    /// 1-based page number.
    pub page: Option<u32>,
}

impl EmailJobQuery {
    /// No filter, first page.
    ///
    /// ```
    /// assert_eq!(letterxpress::EmailJobQuery::new().filter, None);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts to one documented filter value.
    ///
    /// ```
    /// # use letterxpress::{EmailJobQuery, JobFilter};
    /// assert_eq!(EmailJobQuery::new().with_filter(JobFilter::Hold).filter, Some(JobFilter::Hold));
    /// ```
    #[must_use]
    pub fn with_filter(mut self, filter: JobFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Selects a page.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for page 0.
    ///
    /// ```
    /// # use letterxpress::EmailJobQuery;
    /// assert!(EmailJobQuery::new().with_page(0).is_err());
    /// ```
    pub fn with_page(mut self, page: u32) -> Result<Self> {
        validate_page(page)?;
        self.page = Some(page);
        Ok(self)
    }

    /// The query string, without `?`.
    ///
    /// ```
    /// # use letterxpress::EmailJobQuery;
    /// assert_eq!(EmailJobQuery::new().with_page(2)?.to_query_string().unwrap(), "page=2");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_query_string(&self) -> Option<String> {
        query_string(self.filter.map(JobFilter::as_str), self.page)
    }
}

/// `GET /v3/transactions` query.
///
/// ```
/// use letterxpress::{TransactionFilter, TransactionQuery};
///
/// let query = TransactionQuery::new().with_filter(TransactionFilter::PrintJobs);
/// assert_eq!(query.to_query_string().unwrap(), "filter=printjobs");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransactionQuery {
    /// `payins`, `payouts` or `printjobs`.
    pub filter: Option<TransactionFilter>,
    /// 1-based page number.
    pub page: Option<u32>,
}

impl TransactionQuery {
    /// No filter, first page.
    ///
    /// ```
    /// assert!(letterxpress::TransactionQuery::new().filter.is_none());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts to one documented filter value.
    ///
    /// ```
    /// # use letterxpress::{TransactionFilter, TransactionQuery};
    /// assert_eq!(TransactionQuery::new().with_filter(TransactionFilter::PayIns).filter, Some(TransactionFilter::PayIns));
    /// ```
    #[must_use]
    pub fn with_filter(mut self, filter: TransactionFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Selects a page.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for page 0.
    ///
    /// ```
    /// # use letterxpress::TransactionQuery;
    /// assert!(TransactionQuery::new().with_page(0).is_err());
    /// ```
    pub fn with_page(mut self, page: u32) -> Result<Self> {
        validate_page(page)?;
        self.page = Some(page);
        Ok(self)
    }

    /// The query string, without `?`.
    ///
    /// ```
    /// # use letterxpress::TransactionQuery;
    /// assert_eq!(TransactionQuery::new().with_page(3)?.to_query_string().unwrap(), "page=3");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_query_string(&self) -> Option<String> {
        query_string(self.filter.map(TransactionFilter::as_str), self.page)
    }
}

/// `GET /v3/invoices` query.
///
/// ```
/// use letterxpress::InvoiceQuery;
///
/// let query = InvoiceQuery::new().with_page(2)?;
/// assert_eq!(query.to_query_string().unwrap(), "page=2");
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvoiceQuery {
    /// 1-based page number.
    pub page: Option<u32>,
}

impl InvoiceQuery {
    /// First page.
    ///
    /// ```
    /// assert!(letterxpress::InvoiceQuery::new().page.is_none());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects a page.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for page 0.
    ///
    /// ```
    /// # use letterxpress::InvoiceQuery;
    /// assert!(InvoiceQuery::new().with_page(0).is_err());
    /// assert!(InvoiceQuery::new().with_page(1).is_ok());
    /// ```
    pub fn with_page(mut self, page: u32) -> Result<Self> {
        validate_page(page)?;
        self.page = Some(page);
        Ok(self)
    }

    /// The query string, without `?`.
    ///
    /// ```
    /// # use letterxpress::InvoiceQuery;
    /// assert_eq!(InvoiceQuery::new().with_page(3)?.to_query_string().unwrap(), "page=3");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_query_string(&self) -> Option<String> {
        query_string(None, self.page)
    }
}

/// `GET /v3/journal` filter.
///
/// The documentation describes these as query parameters but shows them in a
/// `filter` object in the request body; the body form is used here, and it is
/// the only form sent.
///
/// ```
/// use letterxpress::{Date, JournalFilter, JournalType};
///
/// let filter = JournalFilter::new()
///     .with_type(JournalType::Print)
///     .with_search("4711")?
///     .with_range(Date::new(2026, 1, 1)?, Date::new(2026, 1, 31)?)?;
/// assert_eq!(filter.to_json()["type"], "print");
/// assert!(filter.to_json()["start_date"].is_string());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalFilter {
    /// `print`, `maildirect`, `mailsecure` or `mailplus`; `None` means "all".
    pub journal_type: Option<JournalType>,
    /// Job or letter number, or recipient address.
    pub searchstr: Option<String>,
    /// Range start.
    pub start_date: Option<Date>,
    /// Range end.
    pub end_date: Option<Date>,
}

impl JournalFilter {
    /// An unfiltered export.
    ///
    /// ```
    /// assert!(letterxpress::JournalFilter::new().to_json().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts to one shipping type.
    ///
    /// ```
    /// # use letterxpress::{JournalFilter, JournalType};
    /// assert_eq!(JournalFilter::new().with_type(JournalType::MailPlus).journal_type, Some(JournalType::MailPlus));
    /// ```
    #[must_use]
    pub fn with_type(mut self, journal_type: JournalType) -> Self {
        self.journal_type = Some(journal_type);
        self
    }

    /// Free-text search.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty or over-long search string.
    ///
    /// ```
    /// # use letterxpress::JournalFilter;
    /// assert!(JournalFilter::new().with_search("").is_err());
    /// assert!(JournalFilter::new().with_search("4711").is_ok());
    /// ```
    pub fn with_search(mut self, searchstr: impl Into<String>) -> Result<Self> {
        let searchstr = searchstr.into();
        if searchstr.trim().is_empty() {
            return Err(Error::invalid("journal.searchstr", "must not be empty"));
        }
        if searchstr.chars().count() > 255 {
            return Err(Error::invalid(
                "journal.searchstr",
                "must be at most 255 characters",
            ));
        }
        self.searchstr = Some(searchstr);
        Ok(self)
    }

    /// Restricts to a date range (inclusive).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `start` is after `end`.
    ///
    /// ```
    /// # use letterxpress::{Date, JournalFilter};
    /// let start = Date::new(2026, 1, 1)?;
    /// let end = Date::new(2026, 1, 31)?;
    /// assert!(JournalFilter::new().with_range(start, end).is_ok());
    /// assert!(JournalFilter::new().with_range(end, start).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_range(mut self, start: Date, end: Date) -> Result<Self> {
        if start.is_after(&end) {
            return Err(Error::invalid(
                "journal.start_date",
                "must not be after end_date",
            ));
        }
        self.start_date = Some(start);
        self.end_date = Some(end);
        Ok(self)
    }

    /// The `filter` object, as documented in the request example.
    ///
    /// ```
    /// # use letterxpress::{JournalFilter, JournalType};
    /// let json = JournalFilter::new().with_type(JournalType::Print).to_json();
    /// assert_eq!(json["type"], "print");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Map<String, Value> {
        let mut map = Map::new();
        if let Some(journal_type) = &self.journal_type {
            map.insert(
                "type".to_string(),
                Value::String(journal_type.as_str().to_string()),
            );
        }
        if let Some(searchstr) = &self.searchstr {
            map.insert("searchstr".to_string(), Value::String(searchstr.clone()));
        }
        if let Some(start) = &self.start_date {
            map.insert("start_date".to_string(), Value::String(start.to_string()));
        }
        if let Some(end) = &self.end_date {
            map.insert("end_date".to_string(), Value::String(end.to_string()));
        }
        map
    }
}

/// The mutable parts of a print job (`PUT /v3/printjobs/{id}`).
///
/// The PDF cannot be changed after submission; only these fields can, and only
/// within the first 15 minutes.
///
/// ```
/// use letterxpress::{PrintJobUpdate, Specification};
///
/// let update = PrintJobUpdate::new(Specification::new())
///     .with_notice("KdNr. 4711")?;
/// assert_eq!(update.to_json()["notice"], "KdNr. 4711");
/// assert!(update.to_json()["specification"].is_object());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintJobUpdate {
    specification: Specification,
    dispatch_date: Option<Date>,
    registered: Option<Registered>,
    notice: Option<String>,
}

impl PrintJobUpdate {
    /// A full replacement specification, nothing else.
    ///
    /// ```
    /// # use letterxpress::{PrintJobUpdate, Specification};
    /// let update = PrintJobUpdate::new(Specification::new());
    /// assert_eq!(update.to_json()["specification"]["color"], "1");
    /// ```
    #[must_use]
    pub fn new(specification: Specification) -> Self {
        Self {
            specification,
            dispatch_date: None,
            registered: None,
            notice: None,
        }
    }

    /// Plans dispatch for `date`, which must be after `today`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the date is not in the future.
    ///
    /// ```
    /// # use letterxpress::{Date, PrintJobUpdate, Specification};
    /// let update = PrintJobUpdate::new(Specification::new());
    /// let today = Date::new(2026, 10, 24)?;
    /// assert!(update.clone().with_dispatch_date(Date::new(2026, 10, 25)?, today).is_ok());
    /// assert!(update.with_dispatch_date(today, today).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_dispatch_date(mut self, date: Date, today: Date) -> Result<Self> {
        if !date.is_after(&today) {
            return Err(Error::invalid(
                "letter.dispatch_date",
                format!("{date} is not in the future (today is {today})"),
            ));
        }
        self.dispatch_date = Some(date);
        Ok(self)
    }

    /// Registers the job (national only).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the specification ships internationally.
    ///
    /// ```
    /// # use letterxpress::{PrintJobUpdate, Registered, Shipping, Specification};
    /// let national = Specification::new().with_shipping(Shipping::National);
    /// assert!(PrintJobUpdate::new(national).with_registered(Registered::Einschreiben).is_ok());
    ///
    /// let international = Specification::new().with_shipping(Shipping::International);
    /// assert!(PrintJobUpdate::new(international).with_registered(Registered::Einschreiben).is_err());
    /// ```
    pub fn with_registered(mut self, registered: Registered) -> Result<Self> {
        crate::letter::check_registered_shipping(
            "letter.registered",
            Some(&registered),
            &self.specification.shipping(),
        )?;
        self.registered = Some(registered);
        Ok(self)
    }

    /// Replaces the notice (max 255 characters).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when empty or too long.
    ///
    /// ```
    /// # use letterxpress::{PrintJobUpdate, Specification};
    /// let update = PrintJobUpdate::new(Specification::new());
    /// assert!(update.clone().with_notice("").is_err());
    /// assert!(update.with_notice("KdNr. 1").is_ok());
    /// ```
    pub fn with_notice(mut self, notice: impl Into<String>) -> Result<Self> {
        let notice = notice.into();
        if notice.trim().is_empty() {
            return Err(Error::invalid("letter.notice", "must not be empty"));
        }
        if notice.chars().count() > crate::MAX_NOTICE_CHARS {
            return Err(Error::invalid(
                "letter.notice",
                format!("must be at most {} characters", crate::MAX_NOTICE_CHARS),
            ));
        }
        if notice.chars().any(char::is_control) {
            return Err(Error::invalid(
                "letter.notice",
                "must not contain control characters",
            ));
        }
        self.notice = Some(notice);
        Ok(self)
    }

    /// The `letter` object for the `PUT` body.
    ///
    /// ```
    /// # use letterxpress::{PrintJobUpdate, Specification};
    /// let json = PrintJobUpdate::new(Specification::new()).to_json();
    /// assert!(json.get("dispatch_date").is_none());
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("specification".to_string(), self.specification.to_json());
        if let Some(date) = &self.dispatch_date {
            map.insert("dispatch_date".to_string(), Value::String(date.to_string()));
        }
        if let Some(registered) = &self.registered {
            map.insert(
                "registered".to_string(),
                Value::String(registered.as_str().to_string()),
            );
        }
        if let Some(notice) = &self.notice {
            map.insert("notice".to_string(), Value::String(notice.clone()));
        }
        Value::Object(map)
    }
}

/// The only mutable field of an e-mail job (`PUT /v3/emailjobs/{id}`).
///
/// ```
/// use letterxpress::EmailJobUpdate;
///
/// let update = EmailJobUpdate::new("neu@example.de")?;
/// assert_eq!(update.to_json()["email_receiver"], "neu@example.de");
/// assert!(EmailJobUpdate::new("nope").is_err());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailJobUpdate {
    email_receiver: String,
}

impl EmailJobUpdate {
    /// Validates the new recipient.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the address is not plausible.
    ///
    /// ```
    /// assert!(letterxpress::EmailJobUpdate::new("a@b.de").is_ok());
    /// ```
    pub fn new(email_receiver: impl Into<String>) -> Result<Self> {
        let email_receiver = email_receiver.into();
        validate_email("email.email_receiver", &email_receiver)?;
        Ok(Self { email_receiver })
    }

    /// The `email` object for the `PUT` body.
    ///
    /// ```
    /// # use letterxpress::EmailJobUpdate;
    /// assert_eq!(EmailJobUpdate::new("a@b.de")?.to_json()["email_receiver"], "a@b.de");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({ "email_receiver": self.email_receiver })
    }
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

impl<T: Transport> Client<T> {
    /// `GET /v3/balance` — remaining credit.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"message":"OK","data":{"balance":54.89,"currency":"EUR"}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.balance().await?.cents(), 5489);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn balance(&self) -> Result<Balance> {
        let envelope = self.call(Method::Get, "/balance", None, None).await?;
        decode_data("GET /v3/balance", envelope)
    }

    /// `GET /v3/price` — quote a letter without submitting one.
    ///
    /// This is the documented way to price a job; it costs nothing and creates
    /// nothing.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode, PriceRequest};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"price":0.81,"letter":{"specification":{"pages":1,"color":"1","mode":"simplex","shipping":"national","c4":0}}}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let quote = client.price(&PriceRequest::new(1)?).await?;
    /// assert_eq!(quote.price.cents(), 81);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject — a price
    /// query that pairs registered mail with international shipping is refused
    /// locally — otherwise the provider's answer mapped as described on
    /// [`Error`] — including [`Error::Ambiguous`] when a submit may have been
    /// processed.
    pub async fn price(&self, request: &PriceRequest) -> Result<Price> {
        request.validate()?;
        let body = json!({ "letter": request.to_json() });
        let envelope = self.call(Method::Get, "/price", None, Some(body)).await?;
        decode_data("GET /v3/price", envelope)
    }

    /// `GET /v3/printjobs` — one page of print jobs, newest first.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode, PrintJobQuery};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"printjobs":[{"id":1,"status":"done","items":[]}],"pagination":{"total":1,"current_page":1,"last_page":1,"per_page":15}}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let page = client.print_jobs(&PrintJobQuery::new()).await?;
    /// assert_eq!(page.jobs.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn print_jobs(&self, query: &PrintJobQuery) -> Result<PrintJobPage> {
        let envelope = self
            .call(Method::Get, "/printjobs", query.to_query_string(), None)
            .await?;
        decode_data("GET /v3/printjobs", envelope)
    }

    /// `GET /v3/printjobs/{id}`.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JobId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":6035138,"status":"done","items":[]}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let job = client.print_job(&JobId::new("6035138")?).await?;
    /// assert!(job.is_terminal());
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn print_job(&self, id: &JobId) -> Result<PrintJob> {
        let endpoint = "GET /v3/printjobs/{id}";
        let envelope = self
            .call(
                Method::Get,
                &format!("/printjobs/{}", id.as_str()),
                None,
                None,
            )
            .await?;
        decode_data(endpoint, envelope)
    }

    /// `POST /v3/printjobs` — submit a letter.
    ///
    /// This is the **only** call that spends money in live mode. It is not
    /// retried: a failure that may have been processed comes back as
    /// [`Error::Ambiguous`], to be reconciled before resubmitting.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Letter, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":6035143,"status":"queue","items":[{"pages":1,"amount":0.68,"vat":0.13,"status":"queue"}]}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_notice("dispatch-1")?;
    /// let job = client.submit_print_job(&letter).await?;
    /// assert_eq!(job.total().expect("total").cents(), 81);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject — a
    /// letter that pairs registered mail with international shipping is refused
    /// locally, before the upload — otherwise the provider's answer mapped as
    /// described on [`Error`] — including [`Error::Ambiguous`] when a submit may
    /// have been processed.
    pub async fn submit_print_job(&self, letter: &Letter) -> Result<PrintJob> {
        letter.validate()?;
        let body = json!({ "letter": letter.to_json() });
        let envelope = self
            .call(Method::Post, "/printjobs", None, Some(body))
            .await?;
        decode_submitted("POST /v3/printjobs", envelope)
    }

    /// `PUT /v3/printjobs/{id}` — change options within 15 minutes.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JobId, Mode, PrintJobUpdate, Specification};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":6035145,"status":"queue","notice":"KdNr. 4711","items":[]}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let update = PrintJobUpdate::new(Specification::new()).with_notice("KdNr. 4711")?;
    /// let job = client.update_print_job(&JobId::new("6035145")?, &update).await?;
    /// assert_eq!(job.notice.as_deref(), Some("KdNr. 4711"));
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn update_print_job(&self, id: &JobId, update: &PrintJobUpdate) -> Result<PrintJob> {
        let body = json!({ "letter": update.to_json() });
        let envelope = self
            .call(
                Method::Put,
                &format!("/printjobs/{}", id.as_str()),
                None,
                Some(body),
            )
            .await?;
        decode_data("PUT /v3/printjobs/{id}", envelope)
    }

    /// `DELETE /v3/printjobs/{id}` — cancel within 15 minutes.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JobId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"message":"Print job deleted successfully"}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let deletion = client.delete_print_job(&JobId::new("6035145")?).await?;
    /// assert_eq!(deletion.message, "Print job deleted successfully");
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn delete_print_job(&self, id: &JobId) -> Result<Deletion> {
        let envelope = self
            .call(
                Method::Delete,
                &format!("/printjobs/{}", id.as_str()),
                None,
                None,
            )
            .await?;
        Ok(envelope.deletion())
    }

    /// `POST /v3/emailjobs` — submit a SMART@MAIL job.
    ///
    /// The letter must either carry an `email_letter` object or a white-code
    /// PDF. The response shape depends on which (see
    /// [`EmailJobSubmission`]).
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, EmailDelivery, EmailJobSubmission, EmailOption, Letter, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":3096,"status":"queue"}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let letter = Letter::new(b"%PDF-1.4\n")?
    ///     .with_email(EmailDelivery::new("kunde@example.de", EmailOption::MailSecure)?)?;
    /// let submission = client.submit_email_job(&letter).await?;
    /// assert!(matches!(submission, EmailJobSubmission::Single(_)));
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject — a
    /// letter that pairs registered mail with international shipping is refused
    /// locally, before the upload — otherwise the provider's answer mapped as
    /// described on [`Error`] — including [`Error::Ambiguous`] when a submit may
    /// have been processed.
    pub async fn submit_email_job(&self, letter: &Letter) -> Result<EmailJobSubmission> {
        letter.validate()?;
        let body = json!({ "letter": letter.to_json() });
        let envelope = self
            .call(Method::Post, "/emailjobs", None, Some(body))
            .await?;
        // The job is created and billed before the body is decoded, so an
        // unreadable answer is an unknown outcome, not a shape error.
        let endpoint = "POST /v3/emailjobs";
        let http_status = envelope.http_status;
        let data = envelope
            .require_data(endpoint)
            .map_err(|error| promote_to_ambiguous(error, http_status))?;
        EmailJobSubmission::parse(data)
            .map_err(|source| promote_to_ambiguous(unexpected_shape(endpoint, source), http_status))
    }

    /// `GET /v3/emailjobs` — one page of e-mail jobs.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, EmailJobQuery, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"emailjobs":[{"id":1,"status":"queue"}]},"pagination":{"total":1,"current_page":1,"last_page":1}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let page = client.email_jobs(&EmailJobQuery::new()).await?;
    /// assert_eq!(page.jobs.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn email_jobs(&self, query: &EmailJobQuery) -> Result<EmailJobPage> {
        let endpoint = "GET /v3/emailjobs";
        let envelope = self
            .call(Method::Get, "/emailjobs", query.to_query_string(), None)
            .await?;
        let pagination = envelope.pagination.clone();
        let data = envelope.require_data(endpoint)?;
        EmailJobPage::parse(data, pagination).map_err(|source| unexpected_shape(endpoint, source))
    }

    /// `GET /v3/emailjobs/{id}`.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JobId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":918,"status":"success","email_receiver":"a@b.de"}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let job = client.email_job(&JobId::new("918")?).await?;
    /// assert_eq!(job.status, letterxpress::JobStatus::Success);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn email_job(&self, id: &JobId) -> Result<EmailJob> {
        let envelope = self
            .call(
                Method::Get,
                &format!("/emailjobs/{}", id.as_str()),
                None,
                None,
            )
            .await?;
        decode_data("GET /v3/emailjobs/{id}", envelope)
    }

    /// `PUT /v3/emailjobs/{id}` — change the recipient within 15 minutes.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, EmailJobUpdate, JobId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":3902,"status":"queue","email_receiver":"neu@example.de"}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let job = client.update_email_job(&JobId::new("3902")?, &EmailJobUpdate::new("neu@example.de")?).await?;
    /// assert_eq!(job.email_receiver.as_deref(), Some("neu@example.de"));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an invalid id or address, otherwise the
    /// provider's answer mapped as described on [`Error`].
    pub async fn update_email_job(&self, id: &JobId, update: &EmailJobUpdate) -> Result<EmailJob> {
        let body = json!({ "email": update.to_json() });
        let envelope = self
            .call(
                Method::Put,
                &format!("/emailjobs/{}", id.as_str()),
                None,
                Some(body),
            )
            .await?;
        decode_data("PUT /v3/emailjobs/{id}", envelope)
    }

    /// `DELETE /v3/emailjobs/{id}` — cancel within 15 minutes.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JobId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"message":"Email job deleted successfully"}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// assert!(client.delete_email_job(&JobId::new("3902")?).await?.message.contains("Email"));
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn delete_email_job(&self, id: &JobId) -> Result<Deletion> {
        let envelope = self
            .call(
                Method::Delete,
                &format!("/emailjobs/{}", id.as_str()),
                None,
                None,
            )
            .await?;
        Ok(envelope.deletion())
    }

    /// `GET /v3/journal` — the outbound-mail journal as CSV.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, JournalFilter, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"csv_base64":"SWQ7TmFtZQ=="}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let journal = client.journal(&JournalFilter::new()).await?;
    /// assert_eq!(journal.csv()?, "Id;Name");
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn journal(&self, filter: &JournalFilter) -> Result<Journal> {
        let body = json!({ "filter": filter.to_json() });
        let envelope = self.call(Method::Get, "/journal", None, Some(body)).await?;
        decode_data("GET /v3/journal", envelope)
    }

    /// `GET /v3/transactions` — ledger entries, newest first.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode, TransactionQuery};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"transactions":[{"amount":0.81,"currency":"EUR"}]}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let page = client.transactions(&TransactionQuery::new()).await?;
    /// assert_eq!(page.entries[0].amount.cents(), 81);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn transactions(&self, query: &TransactionQuery) -> Result<TransactionPage> {
        let envelope = self
            .call(Method::Get, "/transactions", query.to_query_string(), None)
            .await?;
        decode_data("GET /v3/transactions", envelope)
    }

    /// `GET /v3/invoices` — invoice summaries.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"invoices":[{"id":25402,"amount":0.67,"vat":0.13,"invoice_date":"2020-05-31"}]}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// assert_eq!(client.invoices(1).await?.invoices[0].total().expect("total").cents(), 80);
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn invoices(&self, page: u32) -> Result<InvoicePage> {
        let query = InvoiceQuery::new().with_page(page)?;
        let envelope = self
            .call(Method::Get, "/invoices", query.to_query_string(), None)
            .await?;
        decode_data("GET /v3/invoices", envelope)
    }

    /// `GET /v3/invoices/{id}` — one invoice with its PDF.
    ///
    /// ```
    /// use letterxpress::{Client, Credentials, InvoiceId, Mode};
    /// use letterxpress::transport::{MockTransport, Response};
    ///
    /// # async fn demo() -> Result<(), letterxpress::Error> {
    /// let mock = MockTransport::new([Ok(Response::json_response(200, r#"{"status":200,"data":{"id":25402,"amount":0.67,"vat":0.13,"base64_data":"JVBERi0xLjQK"}}"#))]);
    /// let client = Client::new(mock, Credentials::new("u", "k", Mode::Test)?);
    /// let document = client.invoice(&InvoiceId::new("25402")?).await?;
    /// assert!(document.pdf()?.starts_with(b"%PDF-"));
    /// # Ok(())
    /// # }
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] for an argument the provider would reject, otherwise
    /// the provider's answer mapped as described on [`Error`] — including
    /// [`Error::Ambiguous`] when a submit may have been processed.
    pub async fn invoice(&self, id: &InvoiceId) -> Result<InvoiceDocument> {
        let envelope = self
            .call(
                Method::Get,
                &format!("/invoices/{}", id.as_str()),
                None,
                None,
            )
            .await?;
        decode_data("GET /v3/invoices/{id}", envelope)
    }

    /// Sends one request: attaches the `auth` object, enforces the timeout and
    /// classifies failures.
    async fn call(
        &self,
        method: Method,
        path: &str,
        query: Option<String>,
        body: Option<Value>,
    ) -> Result<Envelope> {
        let url = match query {
            Some(query) => format!("{}{}?{}", self.base_url, path, query),
            None => format!("{}{}", self.base_url, path),
        };

        let mut body = match body {
            Some(Value::Object(map)) => map,
            Some(_) => {
                return Err(Error::invalid(
                    "body",
                    "internal: request body must be a JSON object",
                ))
            }
            None => Map::new(),
        };
        let auth = serde_json::to_value(self.credentials.auth_payload()).map_err(Error::from)?;
        body.insert("auth".to_string(), auth);
        let payload = serde_json::to_vec(&Value::Object(body)).map_err(Error::from)?;

        let request = Request::new(method, url, Some(payload))
            .with_header("Content-Type", "application/json")
            .with_header("Accept", "application/json")
            .with_header(
                "User-Agent",
                concat!("letterxpress/", env!("CARGO_PKG_VERSION")),
            )
            .with_timeout(Some(self.timeout));

        let endpoint = endpoint_label(method, path);
        let response = match self.transport.execute(request).await {
            Ok(response) => response,
            Err(error) => return Err(classify_transport(error, method)),
        };
        let http_status = response.status();
        let decoded = parse_envelope(endpoint, response);
        match decoded {
            Err(error) if !method.is_idempotent() => Err(promote_to_ambiguous(error, http_status)),
            other => other,
        }
    }
}

/// `METHOD /path` with the id placeholder kept, for error messages.
fn endpoint_label(method: Method, path: &str) -> &'static str {
    match (method, path) {
        (Method::Get, "/balance") => "GET /v3/balance",
        (Method::Get, "/price") => "GET /v3/price",
        (Method::Get, "/printjobs") => "GET /v3/printjobs",
        (Method::Post, "/printjobs") => "POST /v3/printjobs",
        (Method::Get, "/emailjobs") => "GET /v3/emailjobs",
        (Method::Post, "/emailjobs") => "POST /v3/emailjobs",
        (Method::Get, "/journal") => "GET /v3/journal",
        (Method::Get, "/transactions") => "GET /v3/transactions",
        (Method::Get, "/invoices") => "GET /v3/invoices",
        (Method::Get, _) if path.starts_with("/printjobs/") => "GET /v3/printjobs/{id}",
        (Method::Put, _) if path.starts_with("/printjobs/") => "PUT /v3/printjobs/{id}",
        (Method::Delete, _) if path.starts_with("/printjobs/") => "DELETE /v3/printjobs/{id}",
        (Method::Get, _) if path.starts_with("/emailjobs/") => "GET /v3/emailjobs/{id}",
        (Method::Put, _) if path.starts_with("/emailjobs/") => "PUT /v3/emailjobs/{id}",
        (Method::Delete, _) if path.starts_with("/emailjobs/") => "DELETE /v3/emailjobs/{id}",
        (Method::Get, _) if path.starts_with("/invoices/") => "GET /v3/invoices/{id}",
        _ => "letterxpress endpoint",
    }
}

/// Turns a dead transport into the right error: a submit that timed out *after*
/// being written may already exist, and the structured [`AmbiguousKind`] says
/// which of the two transport failures it was.
fn classify_transport(error: Error, method: Method) -> Error {
    if method.is_idempotent() {
        return error;
    }
    match &error {
        Error::Transport {
            kind: TransportKind::Timeout,
            source,
        } => Error::Ambiguous {
            kind: AmbiguousKind::Timeout,
            message: format!("{method} may have been processed: {source}"),
        },
        Error::Transport {
            kind: TransportKind::Io,
            source,
        } => Error::Ambiguous {
            kind: AmbiguousKind::Io,
            message: format!("{method} may have been processed: {source}"),
        },
        _ => error,
    }
}

/// A failed submit is ambiguous even when the failure is on our side: a 5xx, a
/// 1xx/3xx answer, or a 2xx whose body does not decode leaves the job possibly
/// created and billed. Definitive answers (4xx, auth, rate limit) stay terminal.
///
/// `http_status` is the status of the response that produced `error`, so the
/// 2xx-with-an-unreadable-body case can still report its code through
/// [`Error::status`] instead of burying it in the message.
fn promote_to_ambiguous(error: Error, http_status: u16) -> Error {
    match error {
        Error::Upstream { status, message } => Error::Ambiguous {
            kind: AmbiguousKind::Upstream { status },
            message: format!("provider answered http {status} after the submit: {message}"),
        },
        Error::UnexpectedStatus {
            status,
            endpoint,
            message,
        } => Error::Ambiguous {
            kind: AmbiguousKind::Upstream { status },
            message: format!(
                "provider answered http {status} for `{endpoint}` after the submit; the job may exist: {message}"
            ),
        },
        Error::Json { source } => Error::Ambiguous {
            kind: AmbiguousKind::Upstream {
                status: http_status,
            },
            message: format!("provider answered 2xx with a body that is not JSON: {source}"),
        },
        Error::MissingField { field, .. } => Error::Ambiguous {
            kind: AmbiguousKind::Upstream {
                status: http_status,
            },
            message: format!("provider answered 2xx without `{field}`: the job may exist"),
        },
        Error::UnexpectedShape {
            endpoint, message, ..
        } => Error::Ambiguous {
            kind: AmbiguousKind::Upstream {
                status: http_status,
            },
            message: format!(
                "provider answered http {http_status} for `{endpoint}` with an undecodable body: {message}"
            ),
        },
        other => other,
    }
}

/// Builds the typed shape error while keeping the decoder's cause reachable.
fn unexpected_shape(endpoint: &'static str, source: serde_json::Error) -> Error {
    Error::UnexpectedShape {
        endpoint,
        message: source.to_string(),
        source: Some(Box::new(source)),
    }
}

/// Decodes `data` into a typed model.
fn decode_data<D: DeserializeOwned>(endpoint: &'static str, envelope: Envelope) -> Result<D> {
    let data = envelope.require_data(endpoint)?;
    serde_json::from_value(data).map_err(|source| unexpected_shape(endpoint, source))
}

/// Decodes `data` for a submit that has already reached the provider.
///
/// The provider only answers 2xx here, and it creates the job before writing
/// the body: a body that does not decode — truncated by a proxy, missing
/// `data`, or a shape the client does not know — leaves the outcome unknown, so
/// it is promoted to [`Error::Ambiguous`] instead of a terminal shape error.
fn decode_submitted<D: DeserializeOwned>(endpoint: &'static str, envelope: Envelope) -> Result<D> {
    let http_status = envelope.http_status;
    decode_data(endpoint, envelope).map_err(|error| promote_to_ambiguous(error, http_status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MockTransport, Response};
    use proptest::prelude::*;

    fn client() -> Client<MockTransport> {
        Client::new(
            MockTransport::default(),
            Credentials::new("u", "k", Mode::Test).expect("credentials"),
        )
    }

    proptest! {
        #[test]
        fn base_url_authority_stops_before_the_path(
            host in "[a-z][a-z0-9.-]{0,62}",
            path in "[A-Za-z0-9._~/-]{0,64}",
        ) {
            let url = format!("HTTPS://{host}/{path}");
            prop_assert_eq!(base_url_authority(&url), Some(host.as_str()));
        }

        #[test]
        fn base_url_authority_never_panics(text in ".*") {
            if let Some(authority) = base_url_authority(&text) {
                prop_assert!(text.contains(authority));
            }
        }

        #[test]
        fn query_string_encoding_is_shared_by_filter_and_page(
            filter in proptest::option::of("[a-z][a-z0-9]{0,15}"),
            page in proptest::option::of(1u32..=u32::MAX),
        ) {
            let expected = match (&filter, page) {
                (None, None) => None,
                (Some(filter), None) => Some(format!("filter={filter}")),
                (None, Some(page)) => Some(format!("page={page}")),
                (Some(filter), Some(page)) => Some(format!("filter={filter}&page={page}")),
            };
            prop_assert_eq!(query_string(filter.as_deref(), page), expected);
        }
    }

    #[test]
    fn every_endpoint_has_a_stable_label() {
        let cases = [
            (Method::Get, "/balance", "GET /v3/balance"),
            (Method::Get, "/price", "GET /v3/price"),
            (Method::Get, "/printjobs", "GET /v3/printjobs"),
            (Method::Post, "/printjobs", "POST /v3/printjobs"),
            (Method::Get, "/printjobs/1", "GET /v3/printjobs/{id}"),
            (Method::Put, "/printjobs/1", "PUT /v3/printjobs/{id}"),
            (Method::Delete, "/printjobs/1", "DELETE /v3/printjobs/{id}"),
            (Method::Get, "/emailjobs", "GET /v3/emailjobs"),
            (Method::Post, "/emailjobs", "POST /v3/emailjobs"),
            (Method::Get, "/emailjobs/1", "GET /v3/emailjobs/{id}"),
            (Method::Put, "/emailjobs/1", "PUT /v3/emailjobs/{id}"),
            (Method::Delete, "/emailjobs/1", "DELETE /v3/emailjobs/{id}"),
            (Method::Get, "/journal", "GET /v3/journal"),
            (Method::Get, "/transactions", "GET /v3/transactions"),
            (Method::Get, "/invoices", "GET /v3/invoices"),
            (Method::Get, "/invoices/1", "GET /v3/invoices/{id}"),
            (Method::Post, "/anything", "letterxpress endpoint"),
        ];
        for (method, path, expected) in cases {
            assert_eq!(endpoint_label(method, path), expected, "{method} {path}");
        }
    }

    #[tokio::test]
    async fn a_non_object_body_is_refused_before_it_is_sent() {
        let client = client();
        let error = client
            .call(
                Method::Get,
                "/balance",
                None,
                Some(Value::String("not an object".into())),
            )
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::Validation { field: "body", .. }));
        assert_eq!(client.transport().remaining(), 0, "nothing was sent");
    }

    #[tokio::test]
    async fn a_rejected_submit_stays_a_rejection_not_an_ambiguous_result() {
        let client = Client::new(
            MockTransport::new([Ok(Response::json_response(
                400,
                r#"{"status":400,"message":"Bad Request"}"#,
            ))]),
            Credentials::new("u", "k", Mode::Live).expect("credentials"),
        );
        let letter = Letter::new(b"%PDF-1.4\n").expect("letter");
        let error = client
            .submit_print_job(&letter)
            .await
            .expect_err("must fail");
        assert!(matches!(error, Error::Rejected { status: 400, .. }));
        assert!(!error.is_ambiguous());
    }

    #[test]
    fn a_base_url_requires_a_host_and_a_valid_port() {
        for rejected in [
            "https://",
            "   ",
            "https:///v3",
            "https://:8443",
            "https:////",
            "https:////v3",
            "https://./v3",
            "https://api.letterxpress.de:65536/v3",
            "https://api.letterxpress.de:not-a-port/v3",
            "https://api.letterxpress.de:999999999999999999999/v3",
            "HTTPS:///v3",
        ] {
            let error = client()
                .with_base_url(rejected)
                .expect_err("an invalid base URL must be refused");
            assert!(
                matches!(
                    error,
                    Error::Validation {
                        field: "base_url",
                        ..
                    }
                ),
                "{rejected:?}: {error:?}"
            );
        }

        for accepted in [
            "https://a",
            "https://api.letterxpress.de/v3",
            "https://api.letterxpress.de:443/v3",
            "https://api.letterxpress.de:65535/v3",
            "https://[::1]:65535/v3",
            "HTTPS://api.letterxpress.de/v3",
            "hTtPs://api.letterxpress.de/v3",
        ] {
            assert!(
                client().with_base_url(accepted).is_ok(),
                "{accepted:?} must be accepted"
            );
        }
    }

    #[test]
    fn every_query_object_rejects_page_zero_and_accepts_page_one() {
        for result in [
            PrintJobQuery::new().with_page(1).map(|_| ()),
            EmailJobQuery::new().with_page(1).map(|_| ()),
            TransactionQuery::new().with_page(1).map(|_| ()),
            InvoiceQuery::new().with_page(1).map(|_| ()),
        ] {
            assert!(result.is_ok());
        }
        for result in [
            PrintJobQuery::new().with_page(0).map(|_| ()),
            EmailJobQuery::new().with_page(0).map(|_| ()),
            TransactionQuery::new().with_page(0).map(|_| ()),
            InvoiceQuery::new().with_page(0).map(|_| ()),
        ] {
            assert!(matches!(
                result,
                Err(Error::Validation { field: "page", .. })
            ));
        }
    }

    #[test]
    fn query_page_boundaries_share_one_validation_and_encoding_rule() {
        assert_eq!(
            PrintJobQuery::new()
                .with_page(u32::MAX)
                .expect("largest page")
                .to_query_string()
                .as_deref(),
            Some("page=4294967295")
        );
        assert_eq!(
            EmailJobQuery::new()
                .with_page(u32::MAX)
                .expect("largest page")
                .to_query_string()
                .as_deref(),
            Some("page=4294967295")
        );
        assert_eq!(
            TransactionQuery::new()
                .with_page(u32::MAX)
                .expect("largest page")
                .to_query_string()
                .as_deref(),
            Some("page=4294967295")
        );
        assert_eq!(
            InvoiceQuery::new()
                .with_page(u32::MAX)
                .expect("largest page")
                .to_query_string()
                .as_deref(),
            Some("page=4294967295")
        );
    }

    #[test]
    fn an_update_dispatch_date_must_be_in_the_future() {
        let today = Date::new(2026, 10, 24).expect("today");
        let update = PrintJobUpdate::new(Specification::new());
        assert!(update.clone().with_dispatch_date(today, today).is_err());
    }
}
