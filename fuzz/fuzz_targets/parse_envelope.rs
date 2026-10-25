//! Fuzz target: the envelope parser and every typed decoder that runs on top of
//! it must never panic, whatever bytes arrive from the endpoint — a hostile
//! proxy, a truncated body, a binary error page.
//!
//! Run with `cargo +nightly fuzz run parse_envelope` (see the README for the
//! corpus and how to reproduce a crash).

#![no_main]

use libfuzzer_sys::fuzz_target;
use letterxpress::transport::Response;
use letterxpress::{
    Balance, Date, EmailJob, EmailJobPage, EmailJobSubmission, InvoiceDocument, InvoicePage, JobId,
    JobStatus, Journal, Letter, Price, PriceEcho, PrintJob, PrintJobPage, TransactionPage,
};

fuzz_target!(|data: &[u8]| {
    // The same bytes under a success status and under every documented failure
    // status: classification must not panic either.
    for status in [200u16, 400, 401, 403, 404, 405, 429, 500, 503] {
        let response = Response::json_response(status, data.to_vec());
        let Ok(envelope) = letterxpress::parse_envelope("fuzz", response) else {
            continue;
        };

        let _ = envelope.status();
        let _ = envelope.message();
        let _ = envelope.pagination();

        let Some(value) = envelope.data().cloned() else {
            continue;
        };

        // Every documented model, against the same payload.
        let _ = serde_json::from_value::<Balance>(value.clone());
        let _ = serde_json::from_value::<Price>(value.clone());
        let _ = serde_json::from_value::<PrintJob>(value.clone());
        let _ = serde_json::from_value::<PrintJobPage>(value.clone());
        let _ = serde_json::from_value::<EmailJob>(value.clone());
        let _ = serde_json::from_value::<TransactionPage>(value.clone());
        let _ = serde_json::from_value::<InvoicePage>(value.clone());
        let _ = serde_json::from_value::<Journal>(value.clone());
        let _ = serde_json::from_value::<InvoiceDocument>(value.clone());
        let _ = PriceEcho::from_envelope(&value);
        let _ = EmailJobPage::parse(value.clone(), envelope.pagination().cloned());
        let _ = EmailJobSubmission::parse(value.clone());

        // Derived arithmetic on whatever decoded must not panic either.
        if let Ok(job) = serde_json::from_value::<PrintJob>(value.clone()) {
            let _ = job.total();
            let _ = job.net();
            let _ = job.is_terminal();
            let _ = job.tracking_code();
        }
        if let Ok(journal) = serde_json::from_value::<Journal>(value.clone()) {
            let _ = journal.csv();
        }
        if let Ok(document) = serde_json::from_value::<InvoiceDocument>(value) {
            let _ = document.pdf();
        }
    }

    // Text-shaped parsers get the same bytes.
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = JobStatus::from_wire(text);
        let _ = Date::parse(text);
        let _ = text.parse::<JobId>();
    }

    // Payload validation: arbitrary bytes may or may not be a PDF, but the
    // checks must never panic.
    let _ = Letter::new(data);
    if !data.is_empty() {
        let _ = Letter::new(b"%PDF-1.4\n").and_then(|letter| letter.with_attachment(data));
    }
});
