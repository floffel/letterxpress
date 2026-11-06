//! Surface coverage: one assertion for every public item, so the documented API
//! is exercised rather than merely compiled.
//!
//! This file is deliberately exhaustive — it is the test that would fail if a
//! builder were added without a wire shape, or an accessor without a value.

mod common;

use common::{client_with, pdf};
use letterxpress::transport::{
    Method, MockTransport, RecordedRequest, RecordingTransport, Request, Response, Transport,
};
use letterxpress::{
    AmbiguousKind, Backgrounds, Balance, BankForm, Color, Date, Deletion, EmailDelivery, EmailJob,
    EmailJobPage, EmailJobQuery, EmailJobSubmission, EmailJobUpdate, EmailOption, Error,
    InvoiceDocument, InvoiceId, InvoicePage, InvoiceQuery, JobFilter, JobId, JobStatus, Journal,
    JournalFilter, JournalType, Letter, Mode, Money, Pagination, Price, PriceEcho, PriceRequest,
    PrintItem, PrintJob, PrintJobPage, PrintJobQuery, PrintJobUpdate, PrintMode, Registered,
    Separator, Shipping, Specification, Terms, TransactionFilter, TransactionPage,
    TransactionQuery, TransportKind,
};
use serde_json::json;
use strum::IntoEnumIterator;

// ---------------------------------------------------------------------------
// Wire vocabularies
// ---------------------------------------------------------------------------

#[test]
fn colour_covers_both_documented_values_and_unknown_ones() {
    assert_eq!(Color::BlackAndWhite.as_str(), "1");
    assert_eq!(Color::Color.as_str(), "4");
    assert_eq!(Color::from_wire(" 1 "), Color::BlackAndWhite);
    assert_eq!(Color::from_wire("BW"), Color::BlackAndWhite);
    assert_eq!(Color::from_wire("black_and_white"), Color::BlackAndWhite);
    assert_eq!(Color::from_wire("4"), Color::Color);
    assert_eq!(Color::from_wire("colour"), Color::Color);
    assert_eq!(Color::from_wire("full_color"), Color::Color);
    assert_eq!(Color::from_wire("9"), Color::Other("9".into()));
    assert_eq!(Color::Color.to_string(), "4");
    assert_eq!(serde_json::to_string(&Color::Color).unwrap(), "\"4\"");
    assert_eq!(
        serde_json::from_str::<Color>("\"1\"").unwrap(),
        Color::BlackAndWhite
    );
    assert_eq!(
        serde_json::from_str::<Color>("\"9\"").unwrap(),
        Color::Other("9".into())
    );
    for invalid in ["0", "true", "null", "{}", "[]"] {
        assert!(
            serde_json::from_str::<Color>(invalid).is_err(),
            "{invalid} is not a wire enum string"
        );
    }
}

#[test]
fn request_vocabularies_serialize_only_documented_values() {
    assert_eq!(
        serde_json::to_string(&Color::BlackAndWhite).unwrap(),
        "\"1\""
    );
    assert_eq!(serde_json::to_string(&Color::Color).unwrap(), "\"4\"");
    assert_eq!(
        serde_json::to_string(&PrintMode::Simplex).unwrap(),
        "\"simplex\""
    );
    assert_eq!(
        serde_json::to_string(&PrintMode::Duplex).unwrap(),
        "\"duplex\""
    );
    assert_eq!(
        serde_json::to_string(&Shipping::National).unwrap(),
        "\"national\""
    );
    assert_eq!(
        serde_json::to_string(&Shipping::International).unwrap(),
        "\"international\""
    );
    assert_eq!(serde_json::to_string(&Shipping::Auto).unwrap(), "\"auto\"");
    assert_eq!(
        serde_json::to_string(&Registered::EinschreibenEinwurf).unwrap(),
        "\"r1\""
    );
    assert_eq!(
        serde_json::to_string(&Registered::Einschreiben).unwrap(),
        "\"r2\""
    );
    assert_eq!(
        serde_json::to_string(&EmailOption::MailDirect).unwrap(),
        "\"maildirect\""
    );
    assert_eq!(
        serde_json::to_string(&EmailOption::MailPlus).unwrap(),
        "\"mailplus\""
    );
    assert_eq!(
        serde_json::to_string(&EmailOption::MailSecure).unwrap(),
        "\"mailsecure\""
    );

    assert!(serde_json::to_string(&Color::Other("9".into())).is_err());
    assert!(serde_json::to_string(&PrintMode::Other("booklet".into())).is_err());
    assert!(serde_json::to_string(&Shipping::Other("mars".into())).is_err());
    assert!(serde_json::to_string(&Registered::Other("r9".into())).is_err());
    assert!(serde_json::to_string(&EmailOption::Other("fax".into())).is_err());

    assert_eq!(Color::BlackAndWhite.to_request("color").unwrap(), "1");
    assert_eq!(PrintMode::Duplex.to_request("mode").unwrap(), "duplex");
    assert_eq!(Shipping::Auto.to_request("shipping").unwrap(), "auto");
    assert_eq!(
        Registered::Einschreiben.to_request("registered").unwrap(),
        "r2"
    );
    assert_eq!(
        EmailOption::MailSecure.to_request("email_option").unwrap(),
        "mailsecure"
    );
    assert!(matches!(
        Color::from_wire("9").to_request("color"),
        Err(Error::Validation { field: "color", .. })
    ));
}

#[test]
fn request_vocabularies_still_decode_unknown_values() {
    assert_eq!(
        serde_json::from_value::<Color>(json!("9")).unwrap(),
        Color::Other("9".into())
    );
    assert_eq!(
        serde_json::from_value::<PrintMode>(json!("booklet")).unwrap(),
        PrintMode::Other("booklet".into())
    );
    assert_eq!(
        serde_json::from_value::<Shipping>(json!("mars")).unwrap(),
        Shipping::Other("mars".into())
    );
    assert_eq!(
        serde_json::from_value::<Registered>(json!("r9")).unwrap(),
        Registered::Other("r9".into())
    );
    assert_eq!(
        serde_json::from_value::<EmailOption>(json!("fax")).unwrap(),
        EmailOption::Other("fax".into())
    );
}

#[test]
fn print_mode_and_shipping_cover_their_values() {
    assert_eq!(PrintMode::Simplex.to_string(), "simplex");
    assert_eq!(PrintMode::Duplex.to_string(), "duplex");
    assert_eq!(PrintMode::from_wire(" DUPLEX "), PrintMode::Duplex);
    assert_eq!(
        PrintMode::from_wire("booklet"),
        PrintMode::Other("booklet".into())
    );

    assert_eq!(Shipping::National.to_string(), "national");
    assert_eq!(Shipping::International.to_string(), "international");
    assert_eq!(Shipping::Auto.to_string(), "auto");
    assert_eq!(Shipping::from_wire("AUTO"), Shipping::Auto);
    assert_eq!(Shipping::from_wire("mars"), Shipping::Other("mars".into()));
    assert_eq!(
        serde_json::from_str::<Shipping>("\"auto\"").unwrap(),
        Shipping::Auto
    );
}

#[test]
fn registered_mail_and_email_options_cover_their_values() {
    assert_eq!(Registered::EinschreibenEinwurf.to_string(), "r1");
    assert_eq!(Registered::Einschreiben.to_string(), "r2");
    assert_eq!(Registered::from_wire("R1"), Registered::EinschreibenEinwurf);
    assert_eq!(Registered::from_wire("r9"), Registered::Other("r9".into()));

    assert_eq!(EmailOption::MailDirect.to_string(), "maildirect");
    assert_eq!(EmailOption::MailPlus.to_string(), "mailplus");
    assert_eq!(EmailOption::MailSecure.to_string(), "mailsecure");
    assert_eq!(EmailOption::from_wire("MailPlus"), EmailOption::MailPlus);
    assert_eq!(
        EmailOption::from_wire("fax"),
        EmailOption::Other("fax".into())
    );
}

#[test]
fn every_job_status_is_classified() {
    let cases = [
        ("queue", JobStatus::Queue, false, false),
        ("hold", JobStatus::Hold, false, false),
        ("done", JobStatus::Done, true, false),
        ("canceled", JobStatus::Canceled, true, false),
        ("cancelled", JobStatus::Canceled, true, false),
        ("draft", JobStatus::Draft, false, true),
        ("sent", JobStatus::Sent, false, false),
        ("success", JobStatus::Success, true, false),
    ];
    for (raw, expected, terminal, postbox) in cases {
        let status = JobStatus::from_wire(raw);
        assert_eq!(status, expected, "{raw}");
        assert_eq!(status.is_terminal(), terminal, "{raw}");
        assert_eq!(status.is_postbox(), postbox, "{raw}");
        assert_eq!(status.to_string(), expected.as_str());
    }
    assert_eq!(JobStatus::default(), JobStatus::Other(String::new()));
    assert!(!JobStatus::Other("weird".into()).is_terminal());
    assert!(!JobStatus::Other("weird".into()).is_postbox());
}

#[test]
fn unknown_wire_values_normalize_case_padding_and_hash_equally() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn assert_equivalent<T: std::fmt::Debug + Eq + Hash>(left: &T, right: &T) {
        assert_eq!(left, right);

        let hash = |value: &T| {
            let mut hasher = DefaultHasher::new();
            value.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(left), hash(right));
    }

    let expected = JobStatus::from_wire("foo");
    for raw in ["FOO", "foo", " Foo ", "\tFoo\n"] {
        assert_equivalent(&JobStatus::from_wire(raw), &expected);
    }
    assert_eq!(JobStatus::from_wire("FOO"), JobStatus::from_wire("foo"));

    assert_equivalent(&Color::from_wire(" A1 "), &Color::Other("a1".to_owned()));
    assert_equivalent(
        &PrintMode::from_wire(" Booklet "),
        &PrintMode::Other("booklet".to_owned()),
    );
    assert_equivalent(
        &Shipping::from_wire(" Mars "),
        &Shipping::Other("mars".to_owned()),
    );
    assert_equivalent(
        &Registered::from_wire(" R9 "),
        &Registered::Other("r9".to_owned()),
    );
    assert_equivalent(
        &EmailOption::from_wire(" FAX "),
        &EmailOption::Other("fax".to_owned()),
    );
}

#[test]
fn unknown_wire_values_preserve_non_ascii_characters() {
    let status = JobStatus::from_wire("  ÄRGER  ");
    assert_eq!(status.as_str(), "Ärger");
    assert_eq!(status, JobStatus::from_wire("Ärger"));
    assert_ne!(status, JobStatus::from_wire("ärger"));
}

#[test]
fn request_side_filters_are_closed_and_parseable() {
    for filter in JobFilter::ALL {
        assert_eq!(filter.to_string(), filter.as_str());
        assert_eq!(filter.as_str().parse::<JobFilter>().unwrap(), filter);
        assert_eq!(
            filter.as_str().to_uppercase().parse::<JobFilter>().unwrap(),
            filter
        );
    }
    assert!(JobFilter::Draft.is_postbox());
    assert!("everything".parse::<JobFilter>().is_err());

    for filter in TransactionFilter::ALL {
        assert_eq!(filter.to_string(), filter.as_str());
        assert_eq!(
            filter.as_str().parse::<TransactionFilter>().unwrap(),
            filter
        );
    }
    assert!("refunds".parse::<TransactionFilter>().is_err());

    for journal_type in JournalType::ALL {
        assert_eq!(journal_type.to_string(), journal_type.as_str());
        assert_eq!(
            journal_type.as_str().parse::<JournalType>().unwrap(),
            journal_type
        );
    }
    assert!("cheque".parse::<JournalType>().is_err());
}

#[test]
fn request_side_filters_keep_the_custom_error_contract() {
    let job_error = "everything"
        .parse::<JobFilter>()
        .expect_err("unknown filter");
    assert!(matches!(
        job_error,
        Error::Validation {
            field: "filter",
            ..
        }
    ));

    let transaction_error = "refunds"
        .parse::<TransactionFilter>()
        .expect_err("unknown transaction filter");
    assert!(matches!(
        transaction_error,
        Error::Validation {
            field: "filter",
            ..
        }
    ));

    let journal_error = "cheque"
        .parse::<JournalType>()
        .expect_err("unknown journal type");
    assert!(matches!(
        journal_error,
        Error::Validation {
            field: "filter.type",
            ..
        }
    ));
}

#[test]
fn every_wire_alias_displays_canonically_in_any_case_and_padding() {
    for (raw, expected, canonical) in [
        ("1", Color::BlackAndWhite, "1"),
        ("bw", Color::BlackAndWhite, "1"),
        ("black_and_white", Color::BlackAndWhite, "1"),
        ("4", Color::Color, "4"),
        ("colour", Color::Color, "4"),
        ("full_color", Color::Color, "4"),
    ] {
        let padded = format!(" \t{}\r\n", raw.to_ascii_uppercase());
        let parsed = Color::from_wire(&padded);
        assert_eq!(parsed, expected, "{padded:?}");
        assert_eq!(parsed.to_string(), canonical, "{padded:?}");
        assert_eq!(parsed.as_ref(), canonical, "{padded:?}");
    }

    for (raw, expected) in [
        ("r1", Registered::EinschreibenEinwurf),
        ("r2", Registered::Einschreiben),
    ] {
        let padded = format!(" \t{}\r\n", raw.to_ascii_uppercase());
        let parsed = Registered::from_wire(&padded);
        assert_eq!(parsed, expected, "{padded:?}");
        assert_eq!(parsed.to_string(), raw, "{padded:?}");
        assert_eq!(parsed.as_ref(), raw, "{padded:?}");
    }

    for (raw, expected) in [
        ("canceled", JobStatus::Canceled),
        ("cancelled", JobStatus::Canceled),
    ] {
        let padded = format!(" \t{}\r\n", raw.to_ascii_uppercase());
        let parsed = JobStatus::from_wire(&padded);
        assert_eq!(parsed, expected, "{padded:?}");
        assert_eq!(parsed.to_string(), "canceled", "{padded:?}");
        assert_eq!(parsed.as_ref(), "canceled", "{padded:?}");
    }
}

#[test]
fn request_side_filter_lists_contain_every_variant() {
    assert_eq!(
        JobFilter::iter().collect::<Vec<_>>(),
        JobFilter::ALL.to_vec()
    );
    assert_eq!(
        TransactionFilter::iter().collect::<Vec<_>>(),
        TransactionFilter::ALL.to_vec()
    );
    assert_eq!(
        JournalType::iter().collect::<Vec<_>>(),
        JournalType::ALL.to_vec()
    );
}

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

#[test]
fn money_supports_every_documented_shape() {
    assert!(Money::ZERO.euros().abs() < f64::EPSILON);
    assert!(Money::ZERO.is_zero());
    assert!(!Money::from_cents(1).is_zero());
    assert_eq!(
        Money::from_cents(50)
            .checked_add(Money::from_cents(25))
            .expect("sum")
            .cents(),
        75
    );
    assert_eq!(Money::from_cents(1_000_000).to_string(), "10000.00");
    assert_eq!(Money::from_cents(-1_000_000).to_string(), "-10000.00");

    assert_eq!(
        serde_json::to_string(&Money::from_euros(0.68).unwrap()).unwrap(),
        "0.68"
    );
    assert_eq!(serde_json::from_str::<Money>("0.68").unwrap().cents(), 68);
    assert_eq!(
        serde_json::from_str::<Money>("\"0.68\"").unwrap().cents(),
        68
    );
    assert_eq!(
        serde_json::from_str::<Money>("\"0,68\"").unwrap().cents(),
        68
    );
    assert_eq!(serde_json::from_str::<Money>("1").unwrap().cents(), 100);
    assert_eq!(serde_json::from_str::<Money>("-1").unwrap().cents(), -100);
    assert!(serde_json::from_str::<Money>("null").is_err());
    assert!(serde_json::from_str::<Money>("\"not money\"").is_err());
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

#[test]
fn date_exposes_its_parts_and_rejects_impossible_calendars() {
    let date = Date::new(2026, 10, 24).unwrap();
    assert_eq!((date.year(), date.month(), date.day()), (2026, 10, 24));
    assert_eq!(date.to_string(), date.to_string());
    assert_eq!(format!("{date}"), "2026-10-24");
    assert_eq!("2026-10-24".parse::<Date>().unwrap(), date);
    assert!(Date::new(2026, 1, 31).unwrap().days_since_epoch() > 0);
    assert!(!date.is_after(&date));

    assert!(Date::new(0, 1, 1).is_err());
    assert!(Date::new(2026, 1, 0).is_err());
    assert!(Date::new(2026, 4, 31).is_err());
    assert!(Date::new(2026, 12, 32).is_err());
    assert!(Date::new(2026, 13, 1).is_err());
    assert!(Date::parse("2026-10-24 ").is_err());
    assert!(Date::parse("abcd-ef-gh").is_err());
    assert!(Date::parse("2026-1x-24").is_err());
}

// ---------------------------------------------------------------------------
// Letter builders
// ---------------------------------------------------------------------------

#[test]
fn every_letter_builder_lands_on_the_wire() {
    let backgrounds = Backgrounds::new()
        .with_first_page(b"%PDF-1.4\nfirst\n")
        .unwrap()
        .with_other_pages(b"%PDF-1.4\nrest\n")
        .unwrap();
    assert!(!backgrounds.is_empty());
    assert!(backgrounds.to_json()["base64_background_first_page"].is_string());
    assert!(backgrounds.to_json()["base64_background_other_pages"].is_string());

    let terms = Terms::new(b"%PDF-1.4\nterms\n").unwrap().on_all_pages(true);
    assert_eq!(terms.to_json()["terms_on_all_pages"], 1);
    assert_eq!(
        Terms::new(b"%PDF-1.4\nterms\n").unwrap().to_json()["terms_on_all_pages"],
        0
    );

    let bank_form = BankForm::generated(
        "A&O Fischer GmbH",
        "DE12500105170648489890",
        "INGDDEFFXXX",
        "81,00",
        "Rechnung 4711",
    )
    .unwrap()
    .with_purpose2("KdNr. 4711")
    .unwrap();
    assert_eq!(bank_form.to_json()["payee"], "A&O Fischer GmbH");
    assert_eq!(bank_form.to_json()["bic"], "INGDDEFFXXX");
    assert_eq!(bank_form.to_json()["amount"], "81,00");
    assert_eq!(bank_form.to_json()["purpose_of_payment"], "Rechnung 4711");
    assert_eq!(bank_form.to_json()["purpose_of_payment2"], "KdNr. 4711");
    assert_eq!(bank_form.to_json()["bank_form_included"], 0);
    assert!(bank_form.to_json().get("bic").is_some());
    assert_eq!(
        BankForm::generated("A", "DE12500105170648489890", "INGDDEFFXXX", "81.00", "R")
            .unwrap()
            .to_json()["bank_form_included"],
        0
    );

    let email = EmailDelivery::new("kunde@example.de", EmailOption::MailSecure).unwrap();
    assert_eq!(email.receiver(), "kunde@example.de");
    assert_eq!(email.option(), EmailOption::MailSecure);
    assert_eq!(email.to_json()["email_option"], "mailsecure");

    let specification = Specification::new()
        .with_color(Color::Color)
        .expect("color")
        .with_print_mode(PrintMode::Duplex)
        .expect("print mode")
        .with_shipping(Shipping::National)
        .expect("shipping")
        .with_c4_envelope(true);
    assert_eq!(specification.color(), Color::Color);
    assert_eq!(specification.print_mode(), PrintMode::Duplex);
    assert_eq!(specification.shipping(), Shipping::National);
    assert!(specification.c4_envelope());
    assert_eq!(specification.to_json()["color"], "4");
    assert_eq!(Specification::default(), Specification::new());

    let today = Date::new(2026, 10, 24).unwrap();
    let letter = Letter::new(&pdf())
        .unwrap()
        .with_specification(specification)
        .unwrap()
        .with_registered(Registered::EinschreibenEinwurf)
        .unwrap()
        .with_dispatch_date(Date::new(2026, 11, 2).unwrap(), today)
        .unwrap()
        .with_notice("dispatch-1")
        .unwrap()
        .with_filename_original("Rechnung-1.pdf")
        .unwrap()
        .with_attachment(b"%PDF-1.4\nattachment\n")
        .unwrap()
        .with_backgrounds(backgrounds)
        .unwrap()
        .with_terms(terms)
        .unwrap()
        .with_bank_form(bank_form)
        .unwrap()
        .with_separator(Separator::after_pages(2).unwrap())
        .unwrap();

    assert!(letter.has_separator());
    assert!(!letter.has_email_delivery());
    assert_eq!(letter.attachments(), 1);
    assert!(letter.total_pdf_bytes() > 0);
    assert_eq!(letter.checksum().len(), 32);
    assert_eq!(letter.specification().color(), Color::Color);

    let json = letter.to_json();
    assert_eq!(json["specification"]["c4"], 1);
    assert_eq!(json["registered"], "r1");
    assert_eq!(json["dispatch_date"], "2026-11-02");
    assert_eq!(json["serial_letter"]["pages_separator_type"], "number");
    assert_eq!(json["serial_letter"]["pages_separator_value"], 2);
    assert_eq!(json["base64_attachments"].as_array().unwrap().len(), 1);
    assert!(json["backgrounds"].is_object());
    assert!(json["terms_and_conditions"].is_object());
    assert!(json["bank_form"].is_object());
    assert_eq!(json["notice"], "dispatch-1");
    assert_eq!(json["filename_original"], "Rechnung-1.pdf");
}

#[test]
fn keyword_separators_and_email_delivery_round_trip() {
    let separator = Separator::at_keyword(":: next ::").unwrap();
    assert_eq!(separator.to_json()["pages_separator_type"], "string");
    assert_eq!(separator.to_json()["pages_separator_value"], ":: next ::");

    let letter = Letter::new(&pdf())
        .unwrap()
        .with_email(EmailDelivery::new("a@b.de", EmailOption::MailPlus).unwrap())
        .unwrap();
    assert!(letter.has_email_delivery());
    assert_eq!(letter.to_json()["email_letter"]["email_option"], "mailplus");
}

#[test]
fn background_and_terms_budgets_are_checked() {
    let empty = Backgrounds::new();
    assert!(Letter::new(&pdf())
        .unwrap()
        .with_backgrounds(empty)
        .is_err());

    let huge = vec![b'x'; letterxpress::MAX_PDF_BYTES];
    let mut big = b"%PDF-1.4\n".to_vec();
    big.extend_from_slice(&huge);
    assert!(Backgrounds::new().with_first_page(&big).is_err());
    assert!(Terms::new(&big).is_err());
    assert!(Letter::new(&pdf()).unwrap().with_attachment(&big).is_err());
}

#[test]
fn price_request_builders_produce_the_documented_body() {
    let request = PriceRequest::new(2)
        .unwrap()
        .with_color(Color::Color)
        .expect("color")
        .with_print_mode(PrintMode::Duplex)
        .expect("print mode")
        .with_shipping(Shipping::National)
        .unwrap()
        .with_c4_envelope(true)
        .with_registered(Registered::Einschreiben)
        .unwrap()
        .with_email_option(EmailOption::MailSecure)
        .expect("email option");

    assert_eq!(request.pages(), 2);
    assert_eq!(request.shipping(), Shipping::National);
    let json = request.to_json();
    assert_eq!(json["specification"]["pages"], 2);
    assert_eq!(json["specification"]["color"], "4");
    assert_eq!(json["specification"]["mode"], "duplex");
    assert_eq!(json["specification"]["c4"], 1);
    assert_eq!(json["specification"]["email_option"], "mailsecure");
    assert_eq!(json["registered"], "r2");
}

// ---------------------------------------------------------------------------
// Response models
// ---------------------------------------------------------------------------

#[test]
fn job_models_expose_their_derived_totals() {
    let job = PrintJob {
        id: JobId::new("1").unwrap(),
        items: vec![
            PrintItem {
                pages: 2,
                net: Some(Money::from_cents(100)),
                vat: Some(Money::from_cents(19)),
                tracking_code: Some("RC1".into()),
                ..PrintItem::default()
            },
            PrintItem {
                pages: 3,
                net: Some(Money::from_cents(50)),
                vat: Some(Money::from_cents(10)),
                ..PrintItem::default()
            },
        ],
        ..PrintJob::default()
    };

    assert_eq!(job.pages(), 5);
    assert_eq!(job.net().expect("net").map(Money::cents), Some(150));
    assert_eq!(job.total().expect("total").map(Money::cents), Some(179));
    assert_eq!(job.tracking_code(), Some("RC1"));

    let email = EmailJob {
        net: Some(Money::from_cents(5)),
        vat: Some(Money::from_cents(1)),
        ..EmailJob::default()
    };
    assert_eq!(email.net().map(Money::cents), Some(5));
    assert_eq!(email.vat().map(Money::cents), Some(1));
    assert_eq!(email.total().expect("total").map(Money::cents), Some(6));

    let empty = EmailJob::default();
    assert_eq!(empty.total().expect("total"), None);
    assert_eq!(PrintJob::default().tracking_code(), None);
}

#[test]
fn pagination_knows_when_a_next_page_exists() {
    assert!(!Pagination::default().has_next());
    let page = Pagination {
        current_page: 1,
        last_page: 3,
        ..Pagination::default()
    };
    assert!(page.has_next());
    let page = Pagination {
        current_page: 3,
        last_page: 3,
        ..Pagination::default()
    };
    assert!(!page.has_next());
}

#[test]
fn email_job_submission_reports_its_collections() {
    let single = EmailJobSubmission::parse(json!({"id": 1})).unwrap();
    assert_eq!(single.email_jobs().len(), 1);
    assert_eq!(single.print_jobs().len(), 0);

    let serial = EmailJobSubmission::parse(json!([{"id": 1}, {"id": 2}])).unwrap();
    assert_eq!(serial.email_jobs().len(), 2);
    assert_eq!(serial.print_jobs().len(), 0);

    let mixed = EmailJobSubmission::parse(json!({
        "printjobs": [{"id": 3}],
        "emailjobs": [{"id": 4}],
    }))
    .unwrap();
    assert_eq!(mixed.email_jobs().len(), 1);
    assert_eq!(mixed.print_jobs().len(), 1);

    assert!(EmailJobSubmission::parse(json!({})).is_err());
    assert!(EmailJobSubmission::parse(json!({"printjobs": [], "emailjobs": []})).is_err());
    assert!(EmailJobSubmission::parse(json!(42)).is_err());
    assert!(EmailJobSubmission::parse(json!([{"nope": true}])).is_err());

    let via_serde: EmailJobSubmission = serde_json::from_value(json!({"id": 7})).unwrap();
    assert!(matches!(via_serde, EmailJobSubmission::Single(_)));
    assert!(serde_json::from_value::<EmailJobSubmission>(json!("nope")).is_err());
}

#[test]
fn email_job_page_accepts_the_three_documented_shapes() {
    let single = EmailJobPage::parse(json!({"id": 1, "status": "queue"}), None).unwrap();
    assert_eq!(single.jobs.len(), 1);
    assert!(single.pagination.is_none());

    let wrapped = EmailJobPage::parse(
        json!({"emailjobs": [{"id": 1}, {"id": 2}], "pagination": {"total": 2}}),
        None,
    )
    .unwrap();
    assert_eq!(wrapped.jobs.len(), 2);
    assert_eq!(wrapped.pagination.unwrap().total, 2);

    let bare = EmailJobPage::parse(json!([{"id": 1}]), None).unwrap();
    assert_eq!(bare.jobs.len(), 1);

    let with_envelope_pagination =
        EmailJobPage::parse(json!({"id": 1}), Some(json!({"total": 9}))).unwrap();
    assert_eq!(with_envelope_pagination.pagination.unwrap().total, 9);

    let null_pagination = EmailJobPage::parse(json!({"id": 1}), Some(json!(null))).unwrap();
    assert!(null_pagination.pagination.is_none());

    assert!(EmailJobPage::parse(json!({"nothing": true}), None).is_err());
    assert!(EmailJobPage::parse(json!("nope"), None).is_err());
    assert!(EmailJobPage::parse(json!({"emailjobs": "nope"}), None).is_err());
}

#[test]
fn price_echo_and_invoice_documents_decode_their_payloads() {
    let echo = PriceEcho::from_envelope(&json!({
        "specification": {"pages": 1, "registered": "r1"},
        "registered": "r2",
    }))
    .unwrap();
    assert_eq!(echo.registered, Some(Registered::Einschreiben));
    assert_eq!(echo.pages, 1);

    // No specification is no echo: the read is refused instead of answering
    // "zero pages, nothing registered".
    assert!(PriceEcho::from_envelope(&json!({"registered": "r2"})).is_err());

    let price: Price = serde_json::from_value(json!({"price": 1.25})).unwrap();
    assert_eq!(price.price.cents(), 125);
    assert!(price.echo.is_none());
    // A `letter` that carries a malformed specification is a decode error; a
    // non-object is simply ignored.
    assert!(
        serde_json::from_value::<Price>(json!({"letter": {"specification": {"pages": "x"}}}))
            .is_err()
    );
    assert!(serde_json::from_value::<Price>(json!({"letter": "nope"})).is_ok());
    assert!(serde_json::from_value::<Price>(json!({"letter": {"registered": "r2"}})).is_err());

    let document: InvoiceDocument =
        serde_json::from_value(json!({"id": 1, "base64_data": "JVBERi0xLjQK"})).unwrap();
    assert!(document.pdf().unwrap().starts_with(b"%PDF-"));

    let journal: Journal = serde_json::from_value(json!({"csv_base64": "SWQ7TmFtZQ=="})).unwrap();
    assert_eq!(journal.csv().unwrap(), "Id;Name");
    assert!(
        serde_json::from_value::<Journal>(json!({"csv_base64": null}))
            .unwrap()
            .csv()
            .is_err()
    );

    let balance: Balance =
        serde_json::from_value(json!({"balance": "12.34", "currency": "EUR"})).unwrap();
    assert_eq!(balance.cents(), 1_234);
    assert_eq!(Balance::default().cents(), 0);
    assert_eq!(InvoicePage::default().invoices.len(), 0);
    assert_eq!(TransactionPage::default().entries.len(), 0);
    assert_eq!(PrintJobPage::default().jobs.len(), 0);
    assert_eq!(Deletion::default().message, String::new());
}

#[test]
fn job_and_invoice_ids_validate_and_display() {
    let id = JobId::new("6035138").unwrap();
    assert_eq!(id.to_string(), "6035138");
    assert_eq!("6035138".parse::<JobId>().unwrap(), id);
    assert_eq!(serde_json::to_string(&id).unwrap(), "\"6035138\"");
    assert_eq!(serde_json::from_str::<JobId>("6035138").unwrap(), id);
    assert_eq!(serde_json::from_str::<JobId>("\"6035138\"").unwrap(), id);
    assert!(serde_json::from_str::<JobId>("\"\"").is_err());
    assert!(serde_json::from_str::<JobId>("null").is_err());
    assert_eq!(JobId::default().as_str(), "");

    let invoice = InvoiceId::new("25402").unwrap();
    assert_eq!(invoice.as_u64(), Some(25_402));
    assert_eq!(invoice.to_string(), "25402");
    assert!(InvoiceId::new("25-402").is_err());
}

#[test]
fn print_item_defaults_are_unknown() {
    let item = PrintItem::default();
    assert_eq!(item.net(), None);
    assert_eq!(item.vat(), None);
    assert_eq!(item.total().expect("total"), None);
    assert_eq!(item.address, None);
    assert_eq!(item.status, JobStatus::Other(String::new()));
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)] // one row per error variant, on purpose
fn every_error_variant_renders_and_classifies() {
    let cases: Vec<(Error, Option<u16>, bool, bool)> = vec![
        (Error::invalid("letter.notice", "empty"), None, false, false),
        (
            Error::Unauthorized {
                message: "Unauthorized.".into(),
            },
            Some(401),
            false,
            false,
        ),
        (
            Error::Forbidden {
                message: "Forbidden".into(),
            },
            Some(403),
            false,
            false,
        ),
        (
            Error::NotFound {
                message: "Not Found".into(),
            },
            Some(404),
            false,
            false,
        ),
        (
            Error::MethodNotAllowed {
                message: "nope".into(),
            },
            Some(405),
            false,
            false,
        ),
        (
            Error::RateLimited {
                retry_after: Some(std::time::Duration::from_secs(12)),
            },
            Some(429),
            true,
            false,
        ),
        (
            Error::RateLimited { retry_after: None },
            Some(429),
            true,
            false,
        ),
        (
            Error::Rejected {
                status: 400,
                message: "bad".into(),
            },
            Some(400),
            false,
            false,
        ),
        (
            Error::Upstream {
                status: 502,
                message: "bad gateway".into(),
            },
            Some(502),
            false,
            false,
        ),
        (
            Error::UnexpectedStatus {
                status: 302,
                endpoint: "GET /v3/x",
                message: "Found".into(),
            },
            Some(302),
            false,
            false,
        ),
        (
            Error::Ambiguous {
                kind: AmbiguousKind::Upstream { status: 500 },
                message: "provider answered http 500".into(),
            },
            Some(500),
            false,
            true,
        ),
        (
            Error::Ambiguous {
                kind: AmbiguousKind::Timeout,
                message: "timeout".into(),
            },
            None,
            false,
            true,
        ),
        (
            Error::Ambiguous {
                kind: AmbiguousKind::Io,
                message: "reset".into(),
            },
            None,
            false,
            true,
        ),
        (
            Error::Transport {
                kind: TransportKind::Connect,
                source: Box::new(std::io::Error::other("dns")),
            },
            None,
            true,
            false,
        ),
        (
            Error::Transport {
                kind: TransportKind::Timeout,
                source: Box::new(std::io::Error::other("timeout")),
            },
            None,
            false,
            false,
        ),
        (
            Error::Transport {
                kind: TransportKind::Io,
                source: Box::new(std::io::Error::other("reset")),
            },
            None,
            false,
            false,
        ),
        (
            Error::Transport {
                kind: TransportKind::ResponseBody,
                source: Box::new(std::io::Error::other("truncated response body")),
            },
            None,
            false,
            false,
        ),
        (
            Error::Transport {
                kind: TransportKind::Protocol,
                source: Box::new(std::io::Error::other("adapter")),
            },
            None,
            false,
            false,
        ),
        (
            Error::Json {
                source: serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            },
            None,
            false,
            false,
        ),
        (
            Error::MissingField {
                endpoint: "GET /v3/x",
                field: "data",
            },
            None,
            false,
            false,
        ),
        (
            Error::UnexpectedShape {
                endpoint: "GET /v3/x",
                message: "nope".into(),
                source: None,
            },
            None,
            false,
            false,
        ),
        (
            Error::TooLarge {
                field: "letter.base64_file",
                len: 10,
                max: 5,
            },
            None,
            false,
            false,
        ),
        (
            Error::Base64 {
                field: "invoice.base64_data",
                source: base64::DecodeError::InvalidByte(0, b'!'),
            },
            None,
            false,
            false,
        ),
        (
            Error::Utf8 {
                field: "journal.csv_base64",
            },
            None,
            false,
            false,
        ),
    ];

    for (error, status, retryable, ambiguous) in cases {
        assert_eq!(error.status(), status, "{error:?}");
        assert_eq!(error.is_retryable(), retryable, "{error:?}");
        assert_eq!(
            error.is_retryable_for(false),
            retryable,
            "{error:?}: `is_retryable()` is the submit answer"
        );
        assert_eq!(error.is_ambiguous(), ambiguous, "{error:?}");
        assert!(!error.to_string().is_empty(), "{error:?}");
    }

    assert!(Error::RateLimited {
        retry_after: Some(std::time::Duration::from_secs(12))
    }
    .to_string()
    .contains("12s"));
    assert!(!Error::RateLimited { retry_after: None }
        .to_string()
        .contains("retry after"));
}

#[test]
fn transport_kinds_display_and_classify() {
    assert_eq!(TransportKind::Connect.to_string(), "connect");
    assert_eq!(TransportKind::Timeout.to_string(), "timeout");
    assert_eq!(TransportKind::Io.to_string(), "io");
    assert_eq!(TransportKind::ResponseBody.to_string(), "response body");
    assert_eq!(TransportKind::Protocol.to_string(), "protocol");
    assert!(TransportKind::Connect.is_retryable());
    assert!(!TransportKind::Timeout.is_retryable());
    assert!(!TransportKind::Io.is_retryable());
    assert!(!TransportKind::ResponseBody.is_retryable());
    assert!(!TransportKind::Protocol.is_retryable());
}

#[test]
fn transport_kinds_pair_across_the_idempotency_boundary() {
    for (kind, submit, read) in [
        (TransportKind::Connect, true, true),
        (TransportKind::Timeout, false, true),
        (TransportKind::Io, false, true),
        (TransportKind::ResponseBody, false, true),
        (TransportKind::Protocol, false, false),
    ] {
        assert_eq!(kind.is_retryable_for(false), submit, "{kind}");
        assert_eq!(kind.is_retryable_for(true), read, "{kind}");
    }
}

#[test]
fn the_retry_decision_depends_on_the_request_it_belongs_to() {
    let upstream = Error::Upstream {
        status: 503,
        message: "service unavailable".into(),
    };
    assert!(upstream.is_retryable_for(Method::Get.is_idempotent()));
    assert!(!upstream.is_retryable_for(Method::Post.is_idempotent()));
    assert!(!upstream.is_retryable(), "the submit answer is the default");

    for kind in [
        TransportKind::Timeout,
        TransportKind::Io,
        TransportKind::ResponseBody,
    ] {
        let broken = Error::Transport {
            kind,
            source: Box::new(std::io::Error::other("mid-flight failure")),
        };
        assert!(!broken.is_retryable_for(false), "{kind}");
        assert!(broken.is_retryable_for(true), "{kind}");
    }

    let refused = Error::Transport {
        kind: TransportKind::Connect,
        source: Box::new(std::io::Error::other("dns")),
    };
    assert!(refused.is_retryable_for(false), "nothing was written");
    assert!(refused.is_retryable_for(true));
}

#[test]
fn retry_after_reports_only_the_providers_own_hint() {
    use std::time::Duration;

    let limited = Error::RateLimited {
        retry_after: Some(Duration::from_secs(30)),
    };
    assert_eq!(limited.retry_after(), Some(Duration::from_secs(30)));
    assert!(limited.is_retryable_for(true));
    assert!(limited.is_retryable_for(false));

    assert_eq!(
        Error::RateLimited {
            retry_after: Some(Duration::ZERO)
        }
        .retry_after(),
        Some(Duration::ZERO),
        "a zero-second hint still means \"wait none\""
    );
    assert_eq!(Error::RateLimited { retry_after: None }.retry_after(), None);
    assert_eq!(
        Error::Forbidden {
            message: "no".into()
        }
        .retry_after(),
        None
    );
}

// ---------------------------------------------------------------------------
// Transport primitives
// ---------------------------------------------------------------------------

#[test]
fn methods_display_and_declare_idempotency() {
    assert_eq!(Method::Get.to_string(), "GET");
    assert_eq!(Method::Post.to_string(), "POST");
    assert_eq!(Method::Put.to_string(), "PUT");
    assert_eq!(Method::Delete.to_string(), "DELETE");
    assert!(Method::Get.is_idempotent());
    assert!(Method::Put.is_idempotent());
    assert!(Method::Delete.is_idempotent());
    assert!(!Method::Post.is_idempotent());
}

#[test]
fn request_debug_hides_the_body_and_paths_extract_correctly() {
    let request = Request::new(
        Method::Post,
        "https://example.test/v3/printjobs",
        Some(b"secret".to_vec()),
    )
    .with_header("Content-Type", "application/json")
    .with_timeout(Some(std::time::Duration::from_secs(3)));
    let debug = format!("{request:?}");
    assert!(!debug.contains("secret"));
    assert!(debug.contains("6 bytes"));
    assert_eq!(request.header("content-type"), Some("application/json"));

    let recorded = RecordedRequest::new(
        Method::Get,
        "https://api.letterxpress.de/v3/printjobs?filter=hold",
        b"{}".to_vec(),
        Vec::new(),
    );
    assert_eq!(
        recorded.path_and_query().as_deref(),
        Some("/v3/printjobs?filter=hold")
    );

    let bare_host = RecordedRequest::new(
        Method::Get,
        "https://api.letterxpress.de",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(bare_host.path_and_query().as_deref(), Some("/"));

    // Just inside the boundary: a pathless URL still has a query, and the
    // slash inside the userinfo is not the start of the path.
    let pathless_with_query = RecordedRequest::new(
        Method::Get,
        "https://api.letterxpress.de?filter=hold",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        pathless_with_query.path_and_query().as_deref(),
        Some("/?filter=hold")
    );

    // Just inside the boundary: the fragment is dropped, and a slash in the
    // userinfo does not shift the split point.
    let userinfo = RecordedRequest::new(
        Method::Get,
        "https://user:pa%2Fss@api.letterxpress.de/v3/printjobs/5?x=1#frag",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        userinfo.path_and_query().as_deref(),
        Some("/v3/printjobs/5?x=1")
    );

    // A raw slash in the userinfo makes the URL invalid; fail closed instead of
    // splitting at it.
    let invalid_userinfo = RecordedRequest::new(
        Method::Get,
        "https://user:pa/ss@api.letterxpress.de/v3/printjobs/5?x=1",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(invalid_userinfo.path_and_query(), None);

    // Just outside the boundary: anything that is not an absolute URL is
    // rejected instead of being returned whole.
    let relative = RecordedRequest::new(Method::Get, "/v3/balance", Vec::new(), Vec::new());
    assert_eq!(relative.path_and_query(), None);

    let empty = RecordedRequest::new(Method::Get, String::new(), Vec::new(), Vec::new());
    assert_eq!(empty.path_and_query(), None);

    // A malformed URL is rejected as well, and a multi-byte path is returned
    // unchanged rather than sliced on a byte offset.
    let malformed = RecordedRequest::new(
        Method::Get,
        "https://exa mple.test/v3/printjobs",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(malformed.path_and_query(), None);

    // A multi-byte path is percent-encoded by the parser, never sliced on a
    // byte offset.
    let unicode = RecordedRequest::new(
        Method::Get,
        "https://api.letterxpress.de/v3/bücher?q=ä",
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        unicode.path_and_query().as_deref(),
        Some("/v3/b%C3%BCcher?q=%C3%A4")
    );

    assert!(
        RecordedRequest::new(Method::Get, String::new(), b"not json".to_vec(), Vec::new())
            .json()
            .is_err()
    );

    let recorded_debug = format!(
        "{:?}",
        RecordedRequest::new(
            Method::Post,
            "https://example.test/v3/printjobs",
            br#"{"auth":{"apikey":"secret"}}"#.to_vec(),
            Vec::new(),
        )
    );
    assert!(!recorded_debug.contains("secret"));
    assert!(recorded_debug.contains("28 bytes"));
}

#[tokio::test]
async fn recording_transport_can_be_drained() {
    let recorder = RecordingTransport::new(MockTransport::new([Ok(Response::json_response(
        200,
        r#"{"status":200,"data":{}}"#,
    ))]));
    let transport = recorder.clone();
    transport
        .execute(Request::new(Method::Get, "https://example.test/x", None))
        .await
        .unwrap();
    assert_eq!(recorder.requests().len(), 1);
    let drained = recorder.take_requests();
    assert_eq!(drained.len(), 1);
    assert_eq!(recorder.requests().len(), 0);
}

#[test]
fn responses_expose_headers_body_and_parsed_json() {
    let response = Response::new(
        201,
        [
            ("Retry-After".to_string(), "5".to_string()),
            ("retry-after".to_string(), "6".to_string()),
        ],
        br#"{"status":201}"#.to_vec(),
    );
    assert_eq!(response.status(), 201);
    assert!(response.is_success());
    assert_eq!(response.header("RETRY-AFTER"), Some("5"));
    assert_eq!(response.body(), br#"{"status":201}"#);
    assert_eq!(response.json().unwrap()["status"], 201);
    assert!(!Response::new(300, std::iter::empty(), Vec::new()).is_success());
    assert!(Response::new(200, std::iter::empty(), Vec::new())
        .json()
        .unwrap()
        .is_null());
}

#[test]
fn mock_transport_reports_remaining_script_and_supports_push() {
    let mock = MockTransport::default();
    assert_eq!(mock.remaining(), 0);
    mock.push(Ok(Response::json_response(200, "{}")));
    assert_eq!(mock.remaining(), 1);
}

// ---------------------------------------------------------------------------
// Client accessors and queries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_accessors_report_their_configuration() {
    let client = common::always_client(r#"{"status":200,"data":{"balance":0,"currency":"EUR"}}"#)
        .with_timeout(std::time::Duration::from_secs(9))
        .unwrap()
        .with_base_url("https://staging.letterxpress.de/v3")
        .unwrap();

    assert_eq!(client.base_url(), "https://staging.letterxpress.de/v3");
    assert_eq!(client.timeout(), std::time::Duration::from_secs(9));
    assert_eq!(client.mode(), Mode::Test);
    assert_eq!(client.transport().remaining(), 0);
    assert!(format!("{client:?}").contains("staging"));

    let clone = client.clone();
    assert_eq!(clone.base_url(), client.base_url());
}

#[test]
fn query_objects_serialise_only_what_was_set() {
    assert_eq!(PrintJobQuery::new().to_query_string(), None);
    assert_eq!(
        PrintJobQuery::new()
            .with_filter(JobFilter::Queue)
            .to_query_string()
            .unwrap(),
        "filter=queue"
    );
    assert_eq!(
        PrintJobQuery::new()
            .with_page(4)
            .unwrap()
            .to_query_string()
            .unwrap(),
        "page=4"
    );

    assert_eq!(EmailJobQuery::new().to_query_string(), None);
    assert_eq!(
        EmailJobQuery::new()
            .with_filter(JobFilter::Done)
            .with_page(2)
            .unwrap()
            .to_query_string()
            .unwrap(),
        "filter=done&page=2"
    );

    assert_eq!(InvoiceQuery::new().to_query_string(), None);
    assert_eq!(
        InvoiceQuery::new()
            .with_page(2)
            .unwrap()
            .to_query_string()
            .unwrap(),
        "page=2"
    );

    assert_eq!(TransactionQuery::new().to_query_string(), None);
    assert_eq!(
        TransactionQuery::new()
            .with_filter(TransactionFilter::PayOuts)
            .to_query_string()
            .unwrap(),
        "filter=payouts"
    );
}

#[test]
fn journal_filter_encodes_every_field_once() {
    assert_eq!(JournalFilter::new().to_query_string(), None);
    let filter = JournalFilter::new()
        .with_type(JournalType::MailPlus)
        .with_search("4711 & Größe")
        .unwrap()
        .with_range(
            Date::new(2026, 1, 1).unwrap(),
            Date::new(2026, 1, 31).unwrap(),
        )
        .unwrap();
    assert_eq!(
        filter.to_query_string().as_deref(),
        Some(
            "type=mailplus&searchstr=4711+%26+Gr%C3%B6%C3%9Fe&start_date=2026-01-01&end_date=2026-01-31"
        )
    );

    let json = filter.to_json();
    assert_eq!(json["searchstr"], "4711 & Größe");
}

#[test]
fn update_bodies_only_carry_the_mutable_fields() {
    let update = PrintJobUpdate::new(Specification::new())
        .with_dispatch_date(
            Date::new(2026, 12, 1).unwrap(),
            Date::new(2026, 10, 24).unwrap(),
        )
        .unwrap()
        .with_registered(Registered::Einschreiben)
        .unwrap()
        .with_notice("KdNr. 1")
        .unwrap();
    let json = update.to_json();
    assert_eq!(json["dispatch_date"], "2026-12-01");
    assert_eq!(json["registered"], "r2");
    assert_eq!(json["notice"], "KdNr. 1");
    assert!(json.get("base64_file").is_none());

    assert!(PrintJobUpdate::new(Specification::new())
        .with_notice("")
        .is_err());
    assert!(PrintJobUpdate::new(Specification::new())
        .with_notice("x".repeat(256))
        .is_err());
    assert!(PrintJobUpdate::new(Specification::new())
        .with_notice("x\u{0}")
        .is_err());
    assert!(PrintJobUpdate::new(
        Specification::new()
            .with_shipping(Shipping::International)
            .expect("international shipping"),
    )
    .with_registered(Registered::Einschreiben)
    .is_err());

    assert_eq!(
        EmailJobUpdate::new("a@b.de").unwrap().to_json()["email_receiver"],
        "a@b.de"
    );
    assert!(EmailJobUpdate::new("a@b").is_err());
}

#[tokio::test]
async fn a_client_can_be_pointed_at_a_mock_endpoint() {
    let client = client_with(vec![Response::json_response(
        200,
        r#"{"status":200,"data":{"balance":1,"currency":"EUR"}}"#,
    )]);
    assert_eq!(client.balance().await.unwrap().cents(), 100);
}

#[tokio::test]
async fn price_request_rejects_zero_pages_before_the_wire() {
    let client = client_with(vec![]);
    assert!(PriceRequest::new(0).is_err());
    let _ = client;
}

#[test]
fn invoice_and_journal_accessors_reject_empty_payloads() {
    let document: InvoiceDocument = serde_json::from_value(json!({"id": 1})).unwrap();
    assert!(matches!(document.pdf(), Err(Error::MissingField { .. })));
    let journal: Journal = serde_json::from_value(json!({})).unwrap();
    assert!(matches!(journal.csv(), Err(Error::MissingField { .. })));
}
