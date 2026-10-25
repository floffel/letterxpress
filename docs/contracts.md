# Verified provider contract — LetterXpress API v3

Every shape in this crate traces back to the official documentation:

- **Primary source:** *Dokumentation LetterXpress API v3*, status **July 2026**,
  `https://www.letterxpress.de/storage/files/dokumentation-letterxpress-api-v3_v0.4.pdf`
  (34 pages, German/English, retrieved 2026-10-24).
- **Secondary source (not authoritative, used only to corroborate):**
  `https://www.letterxpress.de/versandwege/api` (product page, "LXP API") and
  `https://www.letterxpress.de/informationen/vorlagen/11/44` (LXP API v3 landing page).

Status vocabulary used below:

| Mark | Meaning |
| --- | --- |
| **verified** | Stated and exampled in the official PDF; a fixture in `tests/fixtures/` quotes it. |
| **unverified** | Plausible, mentioned only in passing, no example — the crate accepts it but does not rely on it. |
| **absent** | Not in the documentation at all. The crate does **not** model it. |

Fixtures quote the documentation's JSON verbatim except for typography: the PDF
renders JSON string quotes as `„…“` in several examples (a typesetting artefact,
not part of the payload); `tests/fixtures/*.json` use ASCII `"`. Nothing else was
changed — including the documented German values, the `null`s and the per-item
`// optional` comments, which were kept as trailing fields where a fixture needs
them (they are ordinary JSON once the comment is dropped; see the fixture notes).

---

## 1. Transport, authentication, limits

| Item | Value | Source | Mark |
| --- | --- | --- | --- |
| Base URL | `https://api.letterxpress.de/v3` | p. 6 curl example, p. 13/31/32 pagination URLs | verified |
| Content type | `Content-Type: application/json` | p. 3 "Technische Details" | verified |
| Auth carrier | JSON body field `auth` on **every** request — including `GET` and `DELETE` | p. 4 "Das auth-Objekt muss bei jedem Request mit übertragen werden", p. 6 | verified |
| `auth` shape | `{"username": string, "apikey": string, "mode": "test" \| "live"}` | p. 4/5 | verified |
| Other credentials | None. No bearer header, no basic auth, no cookie, no OAuth. | whole document | verified |
| `mode: "test"` | Job is **not processed**, stored in the Postbox (`Warenkorb`), auto-deleted after 7 days, not billed | p. 4 "Testmodus" | verified |
| `mode: "live"` | Job is dispatched directly, bypassing the Postbox, and billed | p. 4 "Livemodus" | verified |
| Separate sandbox host | **None.** Test mode is the same host with `"mode": "test"`. (`sandbox.letterxpress.de` appears only in a 2018 community forum post, never in the docs.) | p. 4 + absence | verified |
| Max size | "Maximale Dateigröße darf 50 MB je PDF / Request nicht überschreiten" | p. 3 | verified |
| Rate limit | "Rate Limit: 120 pro Minute" | p. 3 | verified |
| Idempotency | No idempotency key, no `Idempotency-Key` header, no client reference echoed by the server. Deduplication is the caller's job (`notice` is free text, max 255 chars, not an index). | p. 8, absent otherwise | verified |

**Auth failure envelope** (p. 5) — note it carries **no `status` field and no `data`**:

```json
{ "message": "Unauthorized." }
```

This is the single most important shape in the document: a client that only reads
the body's `status` field sees `status: null` here and cannot distinguish an
authentication failure from a malformed success response.

## 2. Envelope

| Shape | Example | Source | Mark |
| --- | --- | --- | --- |
| Success | `{"status": 200, "message": "OK", "data": <object>}` | p. 10 (balance), p. 14 (create) | verified |
| Deletion | `{"status": 200, "message": "Print job deleted successfully"}` (no `data`) | p. 18, p. 25 | verified |
| Error | `{"message": "Unauthorized."}` then `{"status": 400, "message": ..., "data": ...}` | p. 5, p. 34 | verified for 401; the 400 body shape is **unverified** (the status table lists the code, not an example body) |

HTTP status table (p. 34) — these are **HTTP** codes, and they are the only
reliable error signal:

| Code | Meaning | Mark |
| --- | --- | --- |
| 200 | OK | verified |
| 400 | Bad Request | verified |
| 401 | Unauthorized | verified |
| 403 | Forbidden | verified |
| 404 | Not Found | verified |
| 405 | Method Not Allowed | verified |
| 500 | Internal Server Error | verified |
| 429 | (not listed) rate-limit rejection | **unverified** — the limit is documented (p. 3), the status code is not. The crate maps 429 to `Error::RateLimited` and honours `Retry-After` when present. |

## 3. The `letter` object

Source: p. 7–9 ("Letter-Objekt"). Minimal configuration, verbatim:

```json
"letter": {
  "base64_file": "(string)",
  "base64_file_checksum": "(string)",
  "specification": {
    "color": "(string)",
    "mode": "(string)",
    "shipping": "(string)"
  }
}
```

| Field | Type | Rule | Source | Mark |
| --- | --- | --- | --- | --- |
| `base64_file` | string | PDF, base64-encoded | p. 7 | verified |
| `base64_file_checksum` | string | "md5 from base64 string" — MD5 **hex** of the base64 text, not of the PDF bytes | p. 7 | verified |
| `specification.color` | string | `"1"` = black/white, `"4"` = colour | p. 7 | verified |
| `specification.mode` | string | `"simplex"`, `"duplex"` | p. 7 | verified |
| `specification.shipping` | string | `"national"`, `"international"`, `"auto"` | p. 7 | verified |
| `specification.c4` | number | `0`, `1`; optional, "for C4 envelope under 9 sheets" | p. 7 | verified |
| `specification.pages` | int | **price query only**; "additional element pages is required" | p. 10 | verified |
| `filename_original` | string | optional, e.g. `Rechnung-123456.pdf`; echoed back on retrieval | p. 7 | verified |
| `registered` | string | `"r1"` = Einschreiben Einwurf, `"r2"` = Einschreiben; **national only** | p. 7 | verified |
| `dispatch_date` | string | `yyyy-mm-dd`; "Dispatch day must be in the future" | p. 7 | verified |
| `serial_letter.pages_separator_type` | string | `"string"` or `"number"` | p. 7 | verified |
| `serial_letter.pages_separator_value` | string or number | keyword when type is `string`, page count when `number` | p. 7 | verified |
| `email_letter.email_option` | string | `maildirect`, `mailplus`, `mailsecure` | p. 7 | verified |
| `email_letter.email_receiver` | string | valid e-mail address | p. 7 | verified |
| `email_letter` + `serial_letter` | — | "No use in combination with serial letter" | p. 7 | verified |
| `base64_attachments` | array of string | A4 portrait PDFs, order preserved | p. 8 | verified |
| `backgrounds.base64_background_first_page` | string | A4 portrait, one page | p. 8 | verified |
| `backgrounds.base64_background_other_pages` | string | A4 portrait, one page | p. 8 | verified |
| `terms_and_conditions.base64_terms` | string | A4 portrait, one page | p. 8 | verified |
| `terms_and_conditions.terms_on_all_pages` | number | `0`, `1` | p. 8 | verified |
| `bank_form.bank_form_included` | number | `0`, `1` | p. 8 | verified |
| `bank_form.payee` | string | max 27 characters, ignored when `bank_form_included = 1` | p. 8 | verified |
| `bank_form.iban` | string | max 34 characters | p. 8 | verified |
| `bank_form.bic` | string | max 11 characters | p. 8 | verified |
| `bank_form.amount` | string | max 12 characters | p. 8 | verified |
| `bank_form.purpose_of_payment` | string | max 27 characters | p. 8 | verified |
| `bank_form.purpose_of_payment2` | string | max 27 characters | p. 8 | verified |
| `notice` | string | free text, max 255 characters, stored for later retrieval | p. 8 | verified |
| Recipient address | — | **There is no address field.** The address is rendered into the PDF. The only address on the wire is `items[].address` in a *response*. | p. 7–9 + absence | verified |
| `subject` / `content` / `footer` (email job) | string | Only appear in `emailjobs` **responses**, derived from the PDF's white-code; not accepted as request input. | p. 20, p. 24 | verified (absence in request) |

## 4. Endpoints

All 16 below are documented. Path prefix `/v3` is part of the base URL in the examples.

| # | Method + path | Purpose | Request body | Response `data` | Source | Mark |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `GET /v3/balance` | credit | `{auth}` | `{balance, currency}` | p. 10 | verified |
| 2 | `GET /v3/price` | price quote | `{auth, letter:{specification:{pages,...}, registered?}}` | `{price, letter:{...}}` | p. 10–11 | verified |
| 3 | `GET /v3/printjobs?filter=&page=` | list print jobs | `{auth}` | `{printjobs:[...], pagination:{...}}` | p. 12–13 | verified |
| 4 | `POST /v3/printjobs` | submit print job | `{auth, letter}` | one print job object | p. 14–15 | verified |
| 5 | `GET /v3/printjobs/{id}` | get print job | `{auth}` | one print job object | p. 15–16 | verified |
| 6 | `PUT /v3/printjobs/{id}` | edit print job (≤ 15 min) | `{auth, letter:{dispatch_date?, registered?, notice?, specification?}}` | one print job object | p. 16–17 | verified |
| 7 | `DELETE /v3/printjobs/{id}` | delete (≤ 15 min, not when `done`) | `{auth}` | none, `message` only | p. 18 | verified |
| 8 | `POST /v3/emailjobs` | submit SMART@MAIL job | `{auth, letter}` (with `email_letter` **or** a white-code PDF) | email job, array, or `{printjobs, emailjobs}` | p. 18–23 | verified |
| 9 | `GET /v3/emailjobs/{id}` | get email job | `{auth}` | one email job (with nested `printjob` when MailPlus) | p. 24 | verified |
| 10 | `PUT /v3/emailjobs/{id}` | change recipient (≤ 15 min) | `{auth, email:{email_receiver}}` | one email job | p. 25 | verified |
| 11 | `DELETE /v3/emailjobs/{id}` | delete (≤ 15 min, not when `success`) | `{auth}` | none, `message` only | p. 25 | verified |
| 12 | `GET /v3/emailjobs?filter=&page=` | list email jobs | `{auth}` | `{...one job..., ...}, pagination` (the documented example is malformed: the jobs are not wrapped in an array) | p. 26–27 | verified, shape **unverified** |
| 13 | `GET /v3/journal?searchstr=&type=&start_date=&end_date=` | export journal | `{auth, filter:{type, searchstr, start_date, end_date}}` | `{csv_base64}` | p. 28–29 | verified |
| 14 | `GET /v3/transactions?filter=&page=` | list transactions | `{auth}` | `{transactions:[...], pagination}` | p. 29–31 | verified |
| 15 | `GET /v3/invoices?page=` | list invoices | `{auth}` | `{invoices:[...], pagination}` | p. 31–32 | verified |
| 16 | `GET /v3/invoices/{id}` | one invoice incl. PDF | `{auth}` | `{id, amount, vat, invoice_date, base64_data}` | p. 33 | verified |

Filter vocabularies:

| Endpoint | `filter` values | Source | Mark |
| --- | --- | --- | --- |
| `printjobs` | `queue`, `hold`, `done`, `canceled`, `draft` | p. 12 | verified |
| `transactions` | `payins`, `payouts`, `printjobs` | p. 30 | verified |
| `journal` (`filter.type`) | `print`, `maildirect`, `mailsecure`, `mailplus` | p. 29 | verified |

Not documented anywhere, and therefore **not** implemented: a products/catalogue
endpoint, a separate PDF/document upload endpoint, a status-list endpoint, and a
bulk-cancel endpoint. Cancellation is `DELETE` on the job within 15 minutes.

## 5. Response objects

### Print job (`printjobs` entries, p. 13/14/15/17)

| Field | Type | Notes |
| --- | --- | --- |
| `id` | int (`6035138`) | also accepted as a string by this crate (`unverified` that it ever is) |
| `shipping` | `national`/`international`/`auto` | verified |
| `mode` | `simplex`/`duplex` | verified |
| `color` | `"1"`/`"4"` | verified |
| `c4` | number `0`/`1` | verified |
| `registered` | `"r1"`/`"r2"`/`null` | verified; the p. 15 example prints `r1` **without quotes** — malformed JSON in the document, treated as a string |
| `bank_form` | number `0`/`1` | verified |
| `notice` | string/null | verified |
| `status` | string | see §6 |
| `group` | string/null | semantics **unverified** (only ever `"exampleGroup"`); carried through |
| `dispatch_date` | `yyyy-mm-dd`/null | verified |
| `filename_original` | string/null | verified |
| `created_at` / `updated_at` | `"2022-07-26 09:38:00"` (space, no zone) | verified; kept as the raw string, no timezone is documented |
| `items[]` | array | one entry per letter in the job |

`items[]`:

| Field | Type | Notes |
| --- | --- | --- |
| `address` | string | `"Jim Knopf, Bahnhofstr. 1, 21337 Lüneburg"` — one line, read-only |
| `pages` | int | verified |
| `amount` | number | net amount in EUR, e.g. `0.68` |
| `vat` | number | tax portion, e.g. `0.13` |
| `status` | string | `queue`, `sent` in examples |
| `tracking_code` | string, optional | `RC123456789DE` (registered mail) |
| `tracking_status` | string, optional | German carrier text |

**Gross vs net.** The document's own numbers settle this: for one B&W simplex
national page it reports `amount: 0.68, vat: 0.13`, and the same job appears in
the transaction ledger as `-0.81` — i.e. **`amount + vat` is what is charged**.
The crate therefore models `PrintItem::net()`, `vat()` and `total()` and never
presents `amount` as "the price".

### Email job (p. 20/24/25/26)

`id`, `email_sender`, `email_receiver`, `email_option`, `sent_at`, `amount`,
`vat`, `status`, `subject`, `content`, `footer`, `created_at`, optional
`group`, optional `printjob_id`, optional nested `printjob` (MailPlus).

`POST /v3/emailjobs` returns three different shapes (p. 20–23):

1. a single object (normal PDF, or a MailPlus job with a nested `printjob`),
2. an **array** of objects (serial letter, one per white-code),
3. an object with `printjobs[]` **and** `emailjobs[]` (serial letter with
   MailPlus, or pages without a white-code).

This crate models that as `EmailJobSubmission::{Single, Serial, Mixed}` and
never guesses which one arrived.

### Pagination (p. 13)

`total`, `count`, `current_page`, `last_page`, `per_page`, `first_page_url`,
`last_page_url`, `prev_page_url`, `next_page_url`. `per_page` is 15 in every
example; the document does not say whether it is configurable (**unverified** —
the crate has no per-page parameter).

### Balance / journal / transactions / invoices

| Object | Fields | Source |
| --- | --- | --- |
| balance | `balance` (number), `currency` (`"EUR"`) | p. 10 |
| journal | `csv_base64` (base64 of a CSV, `;`-separated, quoted) | p. 29 |
| transaction | `amount` (negative = charge), `currency`, `description`, `created_at` | p. 30 |
| invoice | `id` (int), `amount`, `vat`, `invoice_date` | p. 31 |
| invoice PDF | the above plus `base64_data` | p. 33 |

## 6. Job status values

The document gives the vocabulary only through the filter table (p. 12) and the
examples:

| Value | Seen in | Meaning |
| --- | --- | --- |
| `queue` | create/get examples (job and item) | queued for production |
| `hold` | filter list, email list | paused |
| `done` | get example (job) | processed |
| `canceled` | filter list only | canceled |
| `draft` | filter list | sitting in the Postbox (test mode) |
| `sent` | item example | item handed to the carrier |
| `success` | email job example | e-mail delivered |

There is **no** documented `printed`, `in_transit`, `delivered` or `failed` job
status, and no status-list endpoint. The crate carries unknown strings verbatim
in `JobStatus::Other(String)`.

## 7. Where the monorepo crate (`repr/backend/aws/crates/letterxpress`) deviates

Findings from reading the extracted crate against the document above. Per the
extraction rule, the product code is **not** modified here — these are reported
so the parent can file them against `entity-postaldispatch`.

| # | Monorepo behaviour | Document | Consequence |
| --- | --- | --- | --- |
| F1 | `check_status_code` treats `status: None` as success and never looks at the HTTP status. | Auth failures answer `{"message":"Unauthorized."}` with **no** `status` and HTTP 401 (p. 5). | A wrong API key or a suspended account is reported as `ProviderError::Upstream("no data in response")`, i.e. a retryable-looking upstream fault instead of a terminal credential error. The Lambda then logs and retries a permanently failing send. |
| F2 | `map_provider_status` guesses from substrings: `contains("deliver")`, `contains("transit")`, `contains("print")`, … | Fixed vocabulary: `queue`, `hold`, `done`, `canceled`, `draft` (+ item `sent`, email `success`). | `"canceled"` falls through to `ProviderStatus::Unknown`, `"draft"`/`"hold"` are collapsed into `Submitted`, `"success"` (e-mail) becomes `Unknown`. A cancelled letter looks like an unknown state in `PostalDispatch`. |
| F3 | `price_cents = items[0].amount * 100` and `currency = "EUR"` hard-coded. | `amount` and `vat` are separate; the charge is their sum (p. 13 vs p. 30), and `currency` is a field on balance/transaction objects, not on print jobs. | The recorded price is 19 % low (68 ct recorded for an 81 ct letter) and the currency is assumed. Booking/quoting logic built on it understates spend. |
| F4 | `quotePostalDispatch` states "LetterXpress v3 exposes no standalone quote endpoint" and quotes from a hard-coded 85 ct. | `GET /v3/price` with `specification.pages` is documented (p. 10–11) and returns `{price, letter}`. | The product quotes a made-up number and cannot reflect colour/duplex/registered pricing. This is the same class of defect as the `inwx-client` and `sipgate-flow` extractions: a documented endpoint missing from the client, papered over in the caller. |
| F5 | `#[derive(Debug)]` on `LetterXpressConfig` and `LetterXpressSender`, both holding `apikey: String`. | Credentials are the account's API key (p. 4). | Any `tracing` field or `{:?}` of the config prints the live API key. |
| F6 | `PostalRecipient` (first/last name, street, house number, zip, city) is carried in the request and **never** put on the wire — only `recipient.country` is used, to derive `shipping`. | The API has no address field; the address lives in the PDF (p. 7–9). | The struct invites callers to believe the address is transmitted. It also silently sends `shipping: national/international` instead of the documented `auto`. |
| F7 | `registered` is silently dropped for non-DE recipients. | "Registered mail can only be sent nationally" (p. 7). | Silent degradation: a registered international letter is sent unregistered instead of being rejected. |
| F8 | `md5` shipped as a hand-written `pub mod md5`; `minimal_a4_pdf()` shipped as public API. | MD5 of the base64 text is required (p. 7); PDF rendering is not the API client's job. | The checksum helper is fine but the PDF writer does not belong in a provider client. The new crate computes the checksum internally (`md-5`) and documents "bring your own PDF". |
| F9 | `LetterXpressSender::new` calls `.expect("reqwest client")`. | — | A TLS/backend initialisation failure panics the Lambda invocation instead of returning an error. |
| F10 | Only 2 of 16 endpoints are implemented (`balance`, `price`, `printjobs` CRUD are missing: no list, no update, no delete, no emailjobs, no journal, no transactions, no invoices). | 16 endpoints. | The crate is a stub by the quality-bar definition (§1): the quote path, the status path and every accounting surface are missing. |

## 8. Unverified shapes this crate tolerates but does not rely on

| Shape | Why tolerated |
| --- | --- |
| `id` as a JSON string instead of a number | Two parsers in the wild disagree; the document shows integers only. |
| `registered` unquoted (`r1`) | The p. 15 example is malformed JSON; the crate accepts a bare string. |
| `429` + `Retry-After` | Rate limit is documented (p. 3), the rejection code is not. |
| `error` object instead of `message` string | Several LetterXpress-adjacent APIs do this; no example exists for v3. |
| `pagination` absent on a filtered list | Only present in examples; the crate treats it as optional. |
| `per_page` other than 15 | Not documented as configurable; parsed, never sent. |
| camelCase keys (`emailReceiver`, `base64File`, …) | Not in the document at all. The crate accepts both spellings so a future/parallel API surface does not break decoding (house rule: permissive decoding). |
| `GET /v3/emailjobs` returning a proper array | The p. 26 example is malformed; the crate accepts object-with-array, bare array and single object. |
