# Changelog

All notable changes to this project are documented here.

## Unreleased

### Breaking

- `Money` now stores signed `i64` cents as its source of truth. Euro and string
  conversion remain fallible at the input boundary, `cents()` is exact for the
  full `i64` range, and `euros()` is a documented lossy view.
- Billing fields on `PrintItem`, `EmailJob`, `Transaction`, and `Invoice` are
  now `Option<Money>`. The corresponding `net()`, `vat()`, `amount()`, and
  `total()` views preserve an omitted or `null` provider amount as unknown
  instead of silently turning it into zero. Totals return
  `Result<Option<Money>, MoneyError>`; `Some(Money::ZERO)` means a reported zero,
  while `None` means the amount was not reported.

- Split pre-write request-build failures (`TransportKind::Protocol`) from
  post-write response body failures (`TransportKind::ResponseBody`). A failed
  response body is retryable for idempotent reads and promoted to
  `Error::Ambiguous` for submits, so a truncated success response cannot invite
  a duplicate letter.
- Reject unknown `Color`, `PrintMode`, `Shipping`, `Registered` and
  `EmailOption` values in request builders and serde serialization. Unknown
  response tokens still decode into `Other(String)` for inspection; the explicit
  `to_request` conversion and the affected setters now return a typed validation
  error rather than sending provider-impossible values.
- Derive the wire string and display conversions for the closed request enums
  with `strum`, and use one `strum`-backed implementation for the macro-backed
  wire enums. Their public `FromStr` methods remain deliberately custom so
  trimming, ASCII case-insensitivity, and the crate's typed `Error` contract do
  not change.
- Decode status-like wire enums from JSON strings only. Unknown strings remain
  `Other(String)`, while a present `status` of `null`, a number, boolean,
  object or array is a typed decode error naming the field. An absent status
  still uses the model's default, and responses are never given a coerced or
  fabricated status.
- Send journal filters as percent-encoded query parameters, leaving only the
  provider-mandated `auth` object in the GET body. Reject accidental GET
  endpoint payloads except for the documented `/price` request.
- Add `CancellationBatch` and `Client::cancel_batch` for bounded, sequential
  cancellation of print and e-mail jobs with one typed result per job. Each
  entry is checked against the provider's fifteen-minute deletion window, and a
  failed deletion does not prevent later independent entries from being tried.
- Document and test the provider's single-job `DELETE` contract, the exact
  window boundary, partial and wholly failed batches, empty batches, and the
  120-entry per-minute request cap.
