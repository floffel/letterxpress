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
