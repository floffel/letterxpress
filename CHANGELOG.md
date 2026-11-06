# Changelog

All notable changes to this project are documented here.

## Unreleased

- Add `CancellationBatch` and `Client::cancel_batch` for bounded, sequential
  cancellation of print and e-mail jobs with one typed result per job. Each
  entry is checked against the provider's fifteen-minute deletion window, and a
  failed deletion does not prevent later independent entries from being tried.
- Document and test the provider's single-job `DELETE` contract, the exact
  window boundary, partial and wholly failed batches, empty batches, and the
  120-entry per-minute request cap.
