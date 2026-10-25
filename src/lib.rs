//! Typed async client for the [LetterXpress](https://www.letterxpress.de) API
//! v3 — physical letter mailing in Germany and abroad, plus SMART@MAIL e-mail
//! delivery.
//!
//! LetterXpress prints and posts PDFs you upload. There is no address field on
//! the wire: the recipient lives in the PDF, and the API only reports the
//! address back to you. What the API does control is everything around the
//! paper — colour, duplex, envelope format, registered mail, dispatch date,
//! attachments, terms and conditions, bank-transfer forms — and that is what
//! this crate types.
//!
//! # Quickstart
//!
//! ```no_run
//! # #[cfg(feature = "http")]
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use letterxpress::{Client, Credentials, Letter, Mode, PriceRequest, Registered, Shipping};
//!
//! // `Mode::Test` puts jobs in the Postbox: nothing is printed, nothing is billed.
//! let client = Client::new(
//!     letterxpress::ReqwestTransport::new(),
//!     Credentials::new("kunde", std::env::var("LETTERXPRESS_APIKEY")?, Mode::Test)?,
//! );
//!
//! // Price first: `GET /v3/price` costs nothing and creates nothing.
//! let quote = client.price(&PriceRequest::new(2)?).await?;
//! println!("2 pages, B&W simplex: {}", quote.price);
//!
//! let letter = Letter::new(&std::fs::read("antrag.pdf")?)?
//!     .with_shipping(Shipping::National)
//!     .with_registered(Registered::EinschreibenEinwurf)?
//!     .with_notice("antrag-4711")?;
//!
//! let job = client.submit_print_job(&letter).await?;
//! println!("job {} at {}", job.id, job.total());
//! # Ok(())
//! # }
//! ```
//!
//! # What it covers
//!
//! All 16 endpoints the provider documents, with typed builders:
//!
//! | Area | Methods |
//! | --- | --- |
//! | Account | [`Client::balance`] |
//! | Pricing | [`Client::price`] (`GET /v3/price`, `specification.pages`) |
//! | Print jobs | [`Client::print_jobs`], [`Client::print_job`], [`Client::submit_print_job`], [`Client::update_print_job`], [`Client::delete_print_job`] |
//! | E-mail jobs | [`Client::email_jobs`], [`Client::email_job`], [`Client::submit_email_job`], [`Client::update_email_job`], [`Client::delete_email_job`] |
//! | Accounting | [`Client::journal`], [`Client::transactions`], [`Client::invoices`], [`Client::invoice`] |
//!
//! Deliberate exclusions, all documented in the README: there is no products
//! endpoint, no separate document upload, no status-list endpoint and no
//! bulk-cancel — the provider has none of them. `notice` is not an idempotency
//! key (the API has none), and PDF rendering is the caller's job.
//!
//! # Design
//!
//! * **Validate before the wire.** [`Letter`] checks the PDF (magic bytes,
//!   50 MB ceiling, shared budget across the request), the 255-character
//!   `notice`, the documented bank-form field limits, the national-only
//!   registered-mail rule, future dispatch dates and the
//!   `email_letter`/`serial_letter` exclusivity. A typo costs a local error,
//!   not a billed letter.
//! * **Parse permissively.** Unknown fields are ignored, unknown enum values are
//!   carried ([`JobStatus::Other`]), scalars are accepted as strings or numbers,
//!   and both `snake_case` and `camelCase` keys decode. Strictness lives in the
//!   security checks, never in deserialization.
//! * **No socket needed.** The HTTP layer sits behind [`Transport`]; the tests
//!   drive [`MockTransport`](crate::transport::MockTransport) and
//!   [`RecordingTransport`](crate::transport::RecordingTransport).
//! * **No hidden retries.** A submit that may have reached the provider returns
//!   [`Error::Ambiguous`]; the caller reconciles before resubmitting.
//!
//! # Feature flags
//!
//! * `http` (default) — pulls in `reqwest` (rustls) and `tokio/time` and
//!   provides [`ReqwestTransport`]. Without it the crate is request building
//!   and response parsing only: no TLS stack, no runtime, no socket.
//!
//! # Security
//!
//! * `#![forbid(unsafe_code)]`, `#![deny(missing_docs)]`.
//! * Credentials and API keys are redacted in `Debug` — [`Credentials`],
//!   [`Client`] and [`Request`] print `<redacted>` or
//!   only a body *length*.
//! * [`Credentials::new`] fails closed on empty, whitespace-only or
//!   control-character values instead of sending a request the provider answers
//!   with an indistinguishable 401.
//! * The base URL must be `https://`, without userinfo, query or fragment:
//!   credentials travel in every body, so a plaintext or ambiguous endpoint is
//!   refused rather than sanitised.
//! * Payload sizes are checked before allocation, encoded base64 is bounded,
//!   and error bodies are truncated to 512 characters before they reach a log.
//! * There is no shared secret to compare in this protocol, so there is no
//!   constant-time comparison to make: the API key is a bearer credential sent
//!   in the body, not a signature the client verifies.
//!
//! # Delivery contract
//!
//! Submitting a letter is **at-most-once as far as this crate is concerned**:
//! it sends the request once and reports [`Error::Ambiguous`] when the outcome
//! is unknowable. The provider offers no idempotency key, so exactly-once is
//! only reachable by reconciling: set a unique [`Letter::with_notice`] value,
//! then on [`Error::Ambiguous`] list [`Client::print_jobs`] and look for it
//! before submitting again.
//!
//! # Contract verification
//!
//! Every shape in this crate is traceable to the official *Dokumentation
//! LetterXpress API v3* (status July 2026). [`docs/contracts.md`](https://github.com/floffel/letterxpress/blob/main/docs/contracts.md)
//! lists each shape with its page reference and marks what stays unverified;
//! the fixtures in `tests/fixtures/` quote the document's own examples.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod client;
mod credentials;
mod date;
mod error;
mod job;
mod letter;
mod money;
mod response;
mod types;

pub mod transport;

pub use client::{
    Client, EmailJobQuery, EmailJobUpdate, JournalFilter, PrintJobQuery, PrintJobUpdate,
    TransactionQuery, DEFAULT_BASE_URL, DEFAULT_TIMEOUT, RATE_LIMIT_PER_MINUTE,
};
pub use credentials::{Credentials, Mode};
pub use date::Date;
pub use error::{Error, TransportKind};
pub use job::{
    Balance, Deletion, EmailJob, EmailJobPage, EmailJobSubmission, Invoice, InvoiceDocument,
    InvoiceId, InvoicePage, JobId, Journal, Pagination, Price, PriceEcho, PrintItem, PrintJob,
    PrintJobPage, Transaction, TransactionPage,
};
pub use letter::{
    Backgrounds, BankForm, EmailDelivery, Letter, PriceRequest, Separator, Specification, Terms,
    MAX_BANK_AMOUNT_CHARS, MAX_BIC_CHARS, MAX_FILENAME_CHARS, MAX_IBAN_CHARS, MAX_NOTICE_CHARS,
    MAX_PAYEE_CHARS, MAX_PDF_BYTES, MAX_PURPOSE_CHARS,
};
pub use money::Money;
pub use response::{parse_envelope, Envelope};
#[cfg(feature = "http")]
pub use transport::ReqwestTransport;
pub use transport::{Method, Request, Response, Transport};
pub use types::{
    Color, EmailOption, JobFilter, JobStatus, JournalType, PrintMode, Registered, Shipping,
    TransactionFilter,
};

/// Everything fallible returns [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) use response::decode_base64;

/// Renders an MD5 digest as lowercase hex, the documented
/// `base64_file_checksum` form.
///
/// ```
/// let checksum = letterxpress::md5_hex(b"abc");
/// assert_eq!(checksum, "900150983cd24fb0d6963f7d28e17f72");
/// ```
#[must_use]
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::Digest as _;

    let digest = md5::Md5::digest(bytes);
    let mut hex = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
