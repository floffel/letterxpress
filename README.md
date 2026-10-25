# letterxpress

Typed async Rust client for the **LetterXpress API v3** — physical letter
mailing (print jobs) and **SMART@MAIL** e-mail delivery, plus the accounting
surface around them.

LetterXpress (a product of A&O Fischer GmbH & Co. KG) prints PDFs you upload and
hands them to Deutsche Post. There is no recipient field on the wire: the
address lives in the PDF, and the API reports it back to you. What the API does
control is everything around the paper — colour, duplex, C4 envelopes,
registered mail, dispatch date, attachments, terms and conditions,
bank-transfer forms — and that is what this crate types.

- 16 of 16 documented endpoints
- validation before the wire (PDF size/magic, `notice` length, bank-form field
  limits, national-only registered mail, future dispatch dates)
- permissive decoding (unknown fields ignored, unknown enums carried, numbers
  or strings, `snake_case` or `camelCase`)
- typed errors with an explicit retryable/ambiguous split
- no socket needed to test: `mock` and `RecordingTransport` ship with the crate

## Install

```toml
[dependencies]
letterxpress = "0.1"
```

Rust 1.89 or newer. Default features enable `http` (reqwest + rustls).

## Quickstart

```rust,no_run
use letterxpress::{Client, Credentials, Letter, Mode, PriceRequest, Registered, Shipping};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `Mode::Test` files jobs in the Postbox: nothing is printed, nothing is billed.
    let client = Client::new(
        letterxpress::ReqwestTransport::new(),
        Credentials::new("kunde", std::env::var("LETTERXPRESS_APIKEY")?, Mode::Test)?,
    );

    // Price first — `GET /v3/price` is free and creates nothing.
    let quote = client.price(&PriceRequest::new(2)?).await?;
    println!("2 pages, B&W simplex: {}", quote.price);

    let letter = Letter::new(&std::fs::read("antrag.pdf")?)?
        .with_shipping(Shipping::National)
        .with_registered(Registered::EinschreibenEinwurf)?
        .with_notice("antrag-4711")?;

    let job = client.submit_print_job(&letter).await?;
    println!("job {} totals {}", job.id, job.total());
    Ok(())
}
```

`mode: "test"` and `mode: "live"` use the **same host**. There is no separate
sandbox endpoint — the mode field is the sandbox.

## What is covered

| Area | Methods | Endpoint |
| --- | --- | --- |
| Credit | `balance` | `GET /v3/balance` |
| Pricing | `price` | `GET /v3/price` |
| Print jobs | `print_jobs`, `print_job`, `submit_print_job`, `update_print_job`, `delete_print_job` | `GET/POST /v3/printjobs`, `GET/PUT/DELETE /v3/printjobs/{id}` |
| E-mail jobs | `email_jobs`, `email_job`, `submit_email_job`, `update_email_job`, `delete_email_job` | `GET/POST /v3/emailjobs`, `GET/PUT/DELETE /v3/emailjobs/{id}` |
| Journal | `journal` | `GET /v3/journal` |
| Ledger | `transactions` | `GET /v3/transactions` |
| Invoices | `invoices`, `invoice` | `GET /v3/invoices`, `GET /v3/invoices/{id}` |

## Contract verification

Every shape is traceable to the official **Dokumentation LetterXpress API v3**
(status July 2026, 34 pages):
`https://www.letterxpress.de/storage/files/dokumentation-letterxpress-api-v3_v0.4.pdf`.

[`docs/contracts.md`](docs/contracts.md) maps each shape to its page and marks
it; `tests/fixtures/` quotes the document's own examples, and `tests/contract.rs`
decodes every fixture.

| Shape | Status | Note |
| --- | --- | --- |
| Base URL, JSON body auth on every verb (incl. `GET`/`DELETE`) | **verified** | p. 4–6 |
| Test mode = Postbox, 7-day auto-delete, no billing | **verified** | p. 4 |
| 50 MB per PDF / request, 120 requests per minute | **verified** | p. 3 |
| `letter` object (all fields incl. `bank_form`, `terms_and_conditions`, `backgrounds`, `serial_letter`, `email_letter`) | **verified** | p. 7–9 |
| `base64_file_checksum` = MD5 hex of the **base64 text** | **verified** | p. 7 |
| All 16 endpoints, their request bodies and response objects | **verified** | p. 10–33 |
| Job statuses `queue`/`hold`/`done`/`canceled`/`draft` (+ item `sent`, e-mail `success`) | **verified** | p. 12, 13, 26 |
| `amount` + `vat` = the charged amount | **verified** | p. 13 vs p. 30 (0.68 + 0.13 = the 0.81 debit) |
| `429` + `Retry-After` | **unverified** | the limit is documented, the rejection code is not; mapped as a retryable error |
| `error`/`errors` keys instead of `message` | **unverified** | accepted when present, never required |
| `id` as a JSON string | **unverified** | accepted, documented as a number |
| `per_page` other than 15 | **unverified** | parsed, never sent |
| camelCase keys | **unverified** | not in the document; accepted so a parallel surface cannot break decoding |

### Why the checksum is MD5

The provider asks for the MD5 of the base64 string, not of the PDF. MD5 is
therefore used for exactly what the protocol specifies (integrity of the
transport encoding, not authentication); the crate does not use it for any
security decision.

## Deliberate exclusions

The API has no endpoint for these, so neither does the crate — inventing them
would be the failure mode this extraction exists to avoid:

- **Products / price list.** No catalogue endpoint exists; `GET /v3/price` is the
  only documented way to price a letter.
- **Separate document upload.** The PDF is embedded as `base64_file` in the
  `letter` object; there is no upload-then-reference flow.
- **Status list endpoint.** Status arrives with the job (`GET /v3/printjobs/{id}`)
  and there is no status vocabulary beyond the seven values above.
- **Bulk cancel.** Cancellation is `DELETE` on a single job, within 15 minutes.
- **Idempotency keys.** The API has none. `notice` (max 255 characters) is free
  text, not an index.
- **PDF rendering.** The recipient address is rendered into the PDF by the
  caller; this crate validates and ships bytes, it does not write them.
- **An address type.** There is no address field on the wire. `items[].address`
  is read-only and returned as a single line.

## Delivery contract

Submitting a letter is **at-most-once from this crate's point of view**: it sends
the request exactly once and never retries it.

| Failure | What it means | What to do |
| --- | --- | --- |
| `Error::RateLimited` | 429, rejected before processing | retry after `Retry-After` |
| `Error::Transport { kind: Connect }` | nothing was written | safe to retry |
| `Error::Transport { kind: Timeout \| Io }` on a **submit** | promoted to `Error::Ambiguous` | reconcile, then resubmit |
| `Error::Upstream` on a **submit** (5xx) | promoted to `Error::Ambiguous` | reconcile, then resubmit |
| `Error::Ambiguous` | the letter may already exist | list `print_jobs` and look for your `notice` |
| `Error::Rejected` (4xx) | the body was refused | fix the input |

`is_retryable()` is true only for connection failures and 429; `is_ambiguous()`
is true only for `Ambiguous`. Reads (`GET`), `PUT` and `DELETE` are treated as
idempotent and never promoted to `Ambiguous`.

## Feature matrix

| Feature | Default | Effect |
| --- | --- | --- |
| `http` | yes | `ReqwestTransport` (reqwest with rustls), `tokio/time` for the timeout |

Without `http` the crate is request building + response parsing only: no TLS
stack, no async runtime, no socket. Bring your own `Transport` implementation
(or the bundled `MockTransport`) and the payload types still compile and run.

## Security

- `#![forbid(unsafe_code)]`, `#![deny(missing_docs)]`.
- The API key is redacted in `Debug` for `Credentials`, `Client` and
  `transport::Request` (which prints a body *length*, never the body).
- `Credentials::new` fails closed on empty, whitespace-only, control-character
  or over-long values instead of sending a request the provider answers with an
  indistinguishable 401.
- Base URLs must be `https://`, without userinfo, query or fragment — credentials
  travel in every body, so a plaintext or ambiguous endpoint is refused rather
  than sanitised. Redirects are not followed, so a 302 cannot move the key to
  another host.
- Payload sizes are checked before allocation; decoded base64 is bounded by the
  documented 50 MB; error bodies are truncated to 512 characters before they
  reach a log line.
- There is no shared secret to compare in this protocol (the key is a bearer
  credential in the body, not a signature the client verifies), so there is no
  constant-time comparison to make. Nothing in the crate logs a body.

## What the monorepo crate got wrong

This crate replaces `repr/backend/aws/crates/letterxpress`. Reading that code
against the documentation turned up defects that the extraction does **not**
silently fix in the product repo (they are reported there):

1. **Auth failures were reported as upstream errors.** The old client treated a
   missing body `status` as success and never read the HTTP status. The
   documented auth failure is `{"message": "Unauthorized."}` with HTTP 401 and no
   `status` field, so a wrong API key surfaced as `Upstream("no data in
   response")` and looked retryable.
2. **Status mapping was guesswork.** Substring matching on `deliver`/`transit`/
   `print` mapped `canceled` to `Unknown` and collapsed `draft` and `hold` into
   `Submitted`.
3. **The recorded price was 19 % low.** `items[0].amount` is net; the charge is
   `amount + vat` (0.68 + 0.13 = the 0.81 that appears in the ledger), and the
   currency was hard-coded.
4. **`GET /v3/price` was believed not to exist.** The product's
   `quotePostalDispatch` says "LetterXpress v3 exposes no standalone quote
   endpoint" and quotes from a hard-coded 85 ct. The endpoint is documented
   (p. 10–11) and returns `{price, letter}`.
5. **The API key leaked through `Debug`.** `LetterXpressConfig` and
   `LetterXpressSender` derived `Debug` while holding the key as a `String`.
6. **The recipient struct never reached the wire.** `PostalRecipient` was carried
   in the request and only its `country` was used, to derive `shipping` — and
   always as `national`/`international`, never the documented `auto`.
7. **Registered mail was silently downgraded.** `registered` was dropped for
   non-German recipients instead of being rejected, so an international
   registered letter went out unregistered.
8. **Only 2 of 16 endpoints existed**, and `LetterXpressSender::new` could panic
   on a TLS/backend failure.

## Testing

```
cargo test --all-features     # 464 tests: fixtures, edge matrix, properties, 32-task concurrency, 295 doctests
cargo test --no-default-features
cargo llvm-cov --no-default-features --summary-only --fail-under-lines 95
cargo +nightly fuzz run parse_envelope
```

- `tests/contract.rs` decodes every documented example.
- `tests/edge_cases.rs` is the boundary matrix: empty body, oversized PDF, HTML
  error page, duplicate keys, unicode address, `401` without `status`, 5xx on a
  submit, unknown enum values, camelCase keys.
- `tests/properties.rs` (proptest) pins the invariants: checksum = MD5 of the
  base64 text, base64 round-trip, notice limits, money arithmetic, date
  ordering, "the response parser never panics".
- `tests/concurrency.rs` hammers one client from 32 tasks: one request per
  submit, no interleaved answers.
- `fuzz/fuzz_targets/parse_envelope.rs` feeds arbitrary bytes through the
  envelope parser, every model and every derived accessor, under success and
  error statuses. The corpus is seeded with all documentation fixtures.

Coverage is measured without the `http` feature: the reqwest adapter is the only
code that needs a socket, and everything else runs against the mock transport.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
