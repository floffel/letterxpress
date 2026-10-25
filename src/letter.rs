//! The `letter` object, built and validated before it reaches the wire.
//!
//! Everything the provider constrains is checked here — PDF size and magic,
//! the documented 255-character `notice`, the per-field bank-form limits, the
//! national-only registered-mail rule, dispatch dates in the future, and the
//! fact that `email_letter` and `serial_letter` cannot be combined. A violation
//! is a local [`Error::Validation`], never a 400 that still cost a request.
//!
//! ```
//! use letterxpress::{Color, Letter, PrintMode, Shipping};
//!
//! let letter = Letter::new(b"%PDF-1.4\n%fake-but-shaped-like-a-pdf\n")?
//!     .with_color(Color::BlackAndWhite)
//!     .with_print_mode(PrintMode::Duplex)
//!     .with_shipping(Shipping::National)
//!     .with_notice("invoice-4711")?;
//!
//! let body = letter.to_json();
//! assert_eq!(body["specification"]["color"], "1");
//! assert_eq!(body["specification"]["mode"], "duplex");
//! assert_eq!(body["notice"], "invoice-4711");
//! assert!(body["base64_file_checksum"].as_str().unwrap().len() == 32);
//! # Ok::<(), letterxpress::Error>(())
//! ```

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use md5::{Digest, Md5};
use serde_json::{json, Map, Value};

use crate::{Color, Date, EmailOption, Error, PrintMode, Registered, Shipping};

/// Documented maximum: "Maximale Dateigröße darf 50 MB je PDF / Request nicht
/// überschreiten" (p. 3 of the v3 documentation).
///
/// Interpreted as 50 000 000 bytes (decimal megabytes) — the conservative
/// reading of "50 MB".
pub const MAX_PDF_BYTES: usize = 50_000_000;

/// Documented maximum for `notice`: 255 characters.
pub const MAX_NOTICE_CHARS: usize = 255;

/// Documented maximum for `filename_original`, in characters. The API
/// documents no limit; this is a sanity bound that keeps the request small.
pub const MAX_FILENAME_CHARS: usize = 255;

/// Documented maximum for `bank_form.payee`.
pub const MAX_PAYEE_CHARS: usize = 27;
/// Documented maximum for `bank_form.iban`.
pub const MAX_IBAN_CHARS: usize = 34;
/// Documented maximum for `bank_form.bic`.
pub const MAX_BIC_CHARS: usize = 11;
/// Documented maximum for `bank_form.amount`.
pub const MAX_BANK_AMOUNT_CHARS: usize = 12;
/// Documented maximum for `bank_form.purpose_of_payment`.
pub const MAX_PURPOSE_CHARS: usize = 27;

/// A PDF that has been checked, base64-encoded and checksummed.
#[derive(Debug, Clone)]
struct EncodedPdf {
    base64: String,
}

/// The printable specification of a letter (`specification`).
///
/// ```
/// use letterxpress::{Color, PrintMode, Shipping, Specification};
///
/// let spec = Specification::new()
///     .with_color(Color::Color)
///     .with_print_mode(PrintMode::Duplex)
///     .with_shipping(Shipping::International);
///
/// assert_eq!(spec.color(), Color::Color);
/// assert!(!spec.c4_envelope());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specification {
    color: Color,
    print_mode: PrintMode,
    shipping: Shipping,
    c4_envelope: bool,
}

impl Specification {
    /// Black and white, simplex, automatic shipping — the cheapest documented
    /// combination, so an omitted setting never silently costs more.
    ///
    /// ```
    /// use letterxpress::{Color, PrintMode, Shipping, Specification};
    ///
    /// let spec = Specification::new();
    /// assert_eq!(spec.color(), Color::BlackAndWhite);
    /// assert_eq!(spec.print_mode(), PrintMode::Simplex);
    /// assert_eq!(spec.shipping(), Shipping::Auto);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            color: Color::BlackAndWhite,
            print_mode: PrintMode::Simplex,
            shipping: Shipping::Auto,
            c4_envelope: false,
        }
    }

    /// Sets the colour (`"1"` / `"4"`).
    ///
    /// ```
    /// use letterxpress::{Color, Specification};
    ///
    /// assert_eq!(Specification::new().with_color(Color::Color).color(), Color::Color);
    /// ```
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets simplex/duplex.
    ///
    /// ```
    /// use letterxpress::{PrintMode, Specification};
    ///
    /// assert_eq!(Specification::new().with_print_mode(PrintMode::Duplex).print_mode(), PrintMode::Duplex);
    /// ```
    #[must_use]
    pub fn with_print_mode(mut self, print_mode: PrintMode) -> Self {
        self.print_mode = print_mode;
        self
    }

    /// Sets the shipping class.
    ///
    /// ```
    /// use letterxpress::{Shipping, Specification};
    ///
    /// assert_eq!(Specification::new().with_shipping(Shipping::National).shipping(), Shipping::National);
    /// ```
    #[must_use]
    pub fn with_shipping(mut self, shipping: Shipping) -> Self {
        self.shipping = shipping;
        self
    }

    /// Requests a C4 envelope (documented as valid "under 9 sheets").
    ///
    /// ```
    /// use letterxpress::Specification;
    ///
    /// assert!(Specification::new().with_c4_envelope(true).c4_envelope());
    /// ```
    #[must_use]
    pub fn with_c4_envelope(mut self, c4_envelope: bool) -> Self {
        self.c4_envelope = c4_envelope;
        self
    }

    /// The configured colour.
    ///
    /// ```
    /// assert_eq!(letterxpress::Specification::new().color(), letterxpress::Color::BlackAndWhite);
    /// ```
    #[must_use]
    pub fn color(&self) -> Color {
        self.color.clone()
    }

    /// The configured print mode.
    ///
    /// ```
    /// assert_eq!(letterxpress::Specification::new().print_mode(), letterxpress::PrintMode::Simplex);
    /// ```
    #[must_use]
    pub fn print_mode(&self) -> PrintMode {
        self.print_mode.clone()
    }

    /// The configured shipping class.
    ///
    /// ```
    /// assert_eq!(letterxpress::Specification::new().shipping(), letterxpress::Shipping::Auto);
    /// ```
    #[must_use]
    pub fn shipping(&self) -> Shipping {
        self.shipping.clone()
    }

    /// Whether a C4 envelope was requested.
    ///
    /// ```
    /// assert!(!letterxpress::Specification::new().c4_envelope());
    /// ```
    #[must_use]
    pub fn c4_envelope(&self) -> bool {
        self.c4_envelope
    }

    /// The wire shape.
    ///
    /// ```
    /// use letterxpress::Specification;
    ///
    /// let json = Specification::new().to_json();
    /// assert_eq!(json["shipping"], "auto");
    /// assert_eq!(json["c4"], 0);
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "color": self.color.as_str(),
            "mode": self.print_mode.as_str(),
            "shipping": self.shipping.as_str(),
            "c4": u8::from(self.c4_envelope),
        })
    }
}

impl Default for Specification {
    fn default() -> Self {
        Self::new()
    }
}

/// Page separator for a serial letter (`serial_letter`).
///
/// ```
/// use letterxpress::Separator;
///
/// let by_pages = Separator::after_pages(2)?;
/// assert_eq!(by_pages.to_json()["pages_separator_value"], 2);
///
/// let by_keyword = Separator::at_keyword(":: NEXT ::")?;
/// assert_eq!(by_keyword.to_json()["pages_separator_value"], ":: NEXT ::");
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Separator {
    /// A fixed number of pages per letter.
    Pages(u32),
    /// A keyword printed in the PDF that starts the next letter.
    Keyword(String),
}

impl Separator {
    /// Splits every `pages` pages. Zero is rejected.
    ///
    /// ```
    /// use letterxpress::Separator;
    ///
    /// assert!(Separator::after_pages(0).is_err());
    /// assert!(Separator::after_pages(1).is_ok());
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn after_pages(pages: u32) -> Result<Self, Error> {
        if pages == 0 {
            return Err(Error::invalid(
                "letter.serial_letter.pages_separator_value",
                "must be at least 1 page",
            ));
        }
        Ok(Self::Pages(pages))
    }

    /// Splits at `keyword`. Empty keywords and control characters are
    /// rejected, because an empty separator would split between every page.
    ///
    /// ```
    /// use letterxpress::Separator;
    ///
    /// assert!(Separator::at_keyword("").is_err());
    /// assert!(Separator::at_keyword(":: cut ::").is_ok());
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn at_keyword(keyword: impl Into<String>) -> Result<Self, Error> {
        let keyword = keyword.into();
        if keyword.trim().is_empty() {
            return Err(Error::invalid(
                "letter.serial_letter.pages_separator_value",
                "keyword must not be empty",
            ));
        }
        if keyword.chars().any(char::is_control) {
            return Err(Error::invalid(
                "letter.serial_letter.pages_separator_value",
                "keyword must not contain control characters",
            ));
        }
        if keyword.chars().count() > 255 {
            return Err(Error::invalid(
                "letter.serial_letter.pages_separator_value",
                "keyword must be at most 255 characters",
            ));
        }
        Ok(Self::Keyword(keyword))
    }

    /// The wire shape.
    ///
    /// ```
    /// assert_eq!(letterxpress::Separator::after_pages(3).unwrap().to_json()["pages_separator_type"], "number");
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::Pages(pages) => json!({
                "pages_separator_type": "number",
                "pages_separator_value": pages,
            }),
            Self::Keyword(keyword) => json!({
                "pages_separator_type": "string",
                "pages_separator_value": keyword,
            }),
        }
    }
}

/// SMART@MAIL delivery for a single job (`email_letter`).
///
/// Cannot be combined with a serial letter — the provider documents that
/// explicitly, and [`Letter::with_email`] enforces it.
///
/// ```
/// use letterxpress::{EmailDelivery, EmailOption};
///
/// let delivery = EmailDelivery::new("kunde@example.de", EmailOption::MailSecure)?;
/// assert_eq!(delivery.to_json()["email_option"], "mailsecure");
/// assert!(EmailDelivery::new("not-an-address", EmailOption::MailDirect).is_err());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailDelivery {
    receiver: String,
    option: EmailOption,
}

impl EmailDelivery {
    /// Builds the delivery, validating the address.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `receiver` is not a plausible e-mail address
    /// (missing `@`, empty local part or domain, whitespace, control
    /// characters, longer than 254 characters).
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption};
    ///
    /// assert!(EmailDelivery::new("a@b.de", EmailOption::MailPlus).is_ok());
    /// assert!(EmailDelivery::new("a b@c.de", EmailOption::MailPlus).is_err());
    /// ```
    pub fn new(receiver: impl Into<String>, option: EmailOption) -> Result<Self, Error> {
        let receiver = receiver.into();
        validate_email("letter.email_letter.email_receiver", &receiver)?;
        Ok(Self { receiver, option })
    }

    /// The recipient address.
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption};
    ///
    /// assert_eq!(EmailDelivery::new("a@b.de", EmailOption::MailDirect)?.receiver(), "a@b.de");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn receiver(&self) -> &str {
        &self.receiver
    }

    /// The delivery option.
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption};
    ///
    /// assert_eq!(EmailDelivery::new("a@b.de", EmailOption::MailPlus)?.option(), EmailOption::MailPlus);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn option(&self) -> EmailOption {
        self.option.clone()
    }

    /// The wire shape.
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption};
    ///
    /// let json = EmailDelivery::new("a@b.de", EmailOption::MailDirect)?.to_json();
    /// assert_eq!(json["email_receiver"], "a@b.de");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "email_option": self.option.as_str(),
            "email_receiver": self.receiver,
        })
    }
}

/// Letterhead pages printed behind the letter (`backgrounds`).
///
/// ```
/// use letterxpress::Backgrounds;
///
/// let backgrounds = Backgrounds::new()
///     .with_first_page(b"%PDF-1.4\nfirst\n")?
///     .with_other_pages(b"%PDF-1.4\nrest\n")?;
/// assert!(backgrounds.to_json()["base64_background_first_page"].is_string());
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct Backgrounds {
    first_page: Option<EncodedPdf>,
    other_pages: Option<EncodedPdf>,
    bytes: usize,
}

impl Backgrounds {
    /// No background pages yet.
    ///
    /// ```
    /// assert!(letterxpress::Backgrounds::new().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Background for page one.
    ///
    /// ```
    /// # use letterxpress::Backgrounds;
    /// let backgrounds = Backgrounds::new().with_first_page(b"%PDF-1.4\nx\n")?;
    /// assert!(!backgrounds.is_empty());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn with_first_page(mut self, pdf: &[u8]) -> Result<Self, Error> {
        let mut budget = MAX_PDF_BYTES - self.bytes;
        self.first_page = Some(encode_pdf(
            "letter.backgrounds.base64_background_first_page",
            pdf,
            &mut budget,
        )?);
        self.bytes += pdf.len();
        Ok(self)
    }

    /// Background for every other page.
    ///
    /// ```
    /// # use letterxpress::Backgrounds;
    /// let backgrounds = Backgrounds::new().with_other_pages(b"%PDF-1.4\nx\n")?;
    /// assert!(!backgrounds.is_empty());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn with_other_pages(mut self, pdf: &[u8]) -> Result<Self, Error> {
        let mut budget = MAX_PDF_BYTES - self.bytes;
        self.other_pages = Some(encode_pdf(
            "letter.backgrounds.base64_background_other_pages",
            pdf,
            &mut budget,
        )?);
        self.bytes += pdf.len();
        Ok(self)
    }

    /// Whether no background was set.
    ///
    /// ```
    /// assert!(letterxpress::Backgrounds::default().is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.first_page.is_none() && self.other_pages.is_none()
    }

    /// The wire shape; omitted keys are simply absent.
    ///
    /// ```
    /// use letterxpress::Backgrounds;
    ///
    /// let json = Backgrounds::new().with_first_page(b"%PDF-1.4\nx\n")?.to_json();
    /// assert!(json.get("base64_background_other_pages").is_none());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(first) = &self.first_page {
            map.insert(
                "base64_background_first_page".to_string(),
                Value::String(first.base64.clone()),
            );
        }
        if let Some(other) = &self.other_pages {
            map.insert(
                "base64_background_other_pages".to_string(),
                Value::String(other.base64.clone()),
            );
        }
        Value::Object(map)
    }
}

/// Terms and conditions appended to the letter (`terms_and_conditions`).
///
/// Printing is automatically duplex; the provider inserts blank pages when the
/// terms are only attached behind page one.
///
/// ```
/// use letterxpress::Terms;
///
/// let terms = Terms::new(b"%PDF-1.4\nterms\n")?.on_all_pages(true);
/// assert_eq!(terms.to_json()["terms_on_all_pages"], 1);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Terms {
    pdf: EncodedPdf,
    bytes: usize,
    on_all_pages: bool,
}

impl Terms {
    /// One-page A4 portrait PDF with the terms.
    ///
    /// ```
    /// assert!(letterxpress::Terms::new(b"not a pdf").is_err());
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn new(pdf: &[u8]) -> Result<Self, Error> {
        let mut budget = MAX_PDF_BYTES;
        let encoded = encode_pdf("letter.terms_and_conditions.base64_terms", pdf, &mut budget)?;
        Ok(Self {
            pdf: encoded,
            bytes: pdf.len(),
            on_all_pages: false,
        })
    }

    /// Whether the terms go behind every page (`true`) or only behind page one.
    ///
    /// ```
    /// # use letterxpress::Terms;
    /// let terms = Terms::new(b"%PDF-1.4\nx\n")?.on_all_pages(true);
    /// assert_eq!(terms.to_json()["terms_on_all_pages"], 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn on_all_pages(mut self, on_all_pages: bool) -> Self {
        self.on_all_pages = on_all_pages;
        self
    }

    /// The wire shape.
    ///
    /// ```
    /// # use letterxpress::Terms;
    /// assert!(letterxpress::Terms::new(b"%PDF-1.4\nx\n")?.to_json()["base64_terms"].is_string());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "base64_terms": self.pdf.base64,
            "terms_on_all_pages": u8::from(self.on_all_pages),
        })
    }
}

/// Bank-transfer form for the last sheet (`bank_form`).
///
/// The documentation's wording is ambiguous: `bank_form_included = 1` means the
/// payment details in this object are **ignored**, which the crate reads as "the
/// supplied PDF already carries the form". Two constructors make the choice
/// explicit instead of leaving a `0`/`1` for the caller to guess:
///
/// * [`BankForm::included_in_pdf`] — `bank_form_included = 1`, no fields.
/// * [`BankForm::generated`] — `bank_form_included = 0`, details printed by the
///   provider.
///
/// ```
/// use letterxpress::BankForm;
///
/// let form = BankForm::generated("A&O Fischer GmbH", "DE12500105170648489890", "INGDDEFFXXX", "81,00", "Rechnung 4711")?;
/// let json = form.to_json();
/// assert_eq!(json["bank_form_included"], 0);
/// assert_eq!(json["iban"], "DE12500105170648489890");
/// assert_eq!(BankForm::included_in_pdf().to_json()["bank_form_included"], 1);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BankForm {
    included: bool,
    payee: Option<String>,
    iban: Option<String>,
    bic: Option<String>,
    amount: Option<String>,
    purpose: Option<String>,
    purpose2: Option<String>,
}

impl BankForm {
    /// The PDF already contains the form; payment fields stay empty.
    ///
    /// ```
    /// assert_eq!(letterxpress::BankForm::included_in_pdf().to_json()["bank_form_included"], 1);
    /// ```
    #[must_use]
    pub fn included_in_pdf() -> Self {
        Self {
            included: true,
            payee: None,
            iban: None,
            bic: None,
            amount: None,
            purpose: None,
            purpose2: None,
        }
    }

    /// The provider prints the payment details onto the blank last sheet.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for an empty or over-long field (payee and
    /// purpose 27 characters, IBAN 34, BIC 11, amount 12), an IBAN or BIC with
    /// characters outside `A-Z0-9`, or an amount that is not a decimal.
    ///
    /// ```
    /// use letterxpress::BankForm;
    ///
    /// assert!(BankForm::generated("A", "DE00", "INGDDEFF", "1,00", "R").is_err()); // IBAN too short
    /// assert!(BankForm::generated("", "DE12500105170648489890", "INGDDEFF", "1,00", "R").is_err());
    /// ```
    pub fn generated(
        payee: impl Into<String>,
        iban: impl Into<String>,
        bic: impl Into<String>,
        amount: impl Into<String>,
        purpose: impl Into<String>,
    ) -> Result<Self, Error> {
        let payee = payee.into();
        let iban = iban.into();
        let bic = bic.into();
        let amount = amount.into();
        let purpose = purpose.into();

        check_chars("letter.bank_form.payee", &payee, MAX_PAYEE_CHARS)?;
        check_chars("letter.bank_form.iban", &iban, MAX_IBAN_CHARS)?;
        check_chars("letter.bank_form.bic", &bic, MAX_BIC_CHARS)?;
        check_chars("letter.bank_form.amount", &amount, MAX_BANK_AMOUNT_CHARS)?;
        check_chars(
            "letter.bank_form.purpose_of_payment",
            &purpose,
            MAX_PURPOSE_CHARS,
        )?;

        if payee.trim().is_empty() {
            return Err(Error::invalid(
                "letter.bank_form.payee",
                "must not be empty",
            ));
        }
        if purpose.trim().is_empty() {
            return Err(Error::invalid(
                "letter.bank_form.purpose_of_payment",
                "must not be empty",
            ));
        }

        let iban_clean = iban.trim();
        if !(15..=MAX_IBAN_CHARS).contains(&iban_clean.len())
            || !iban_clean.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(Error::invalid(
                "letter.bank_form.iban",
                "must be 15 to 34 characters, letters and digits only",
            ));
        }
        if !matches!(bic.trim().len(), 8 | 11) || !bic.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::invalid(
                "letter.bank_form.bic",
                "must be 8 or 11 letters/digits",
            ));
        }
        if !is_decimal_amount(amount.trim()) {
            return Err(Error::invalid(
                "letter.bank_form.amount",
                "must be a decimal amount such as 81,00 or 81.00",
            ));
        }

        Ok(Self {
            included: false,
            payee: Some(payee),
            iban: Some(iban),
            bic: Some(bic),
            amount: Some(amount),
            purpose: Some(purpose),
            purpose2: None,
        })
    }

    /// Adds a second payment-reference line (27 characters).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] on an over-long value.
    ///
    /// ```
    /// use letterxpress::BankForm;
    ///
    /// let form = BankForm::included_in_pdf().with_purpose2("Rechnung 4711")?;
    /// assert_eq!(form.to_json()["purpose_of_payment2"], "Rechnung 4711");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_purpose2(mut self, purpose2: impl Into<String>) -> Result<Self, Error> {
        let purpose2 = purpose2.into();
        check_chars(
            "letter.bank_form.purpose_of_payment2",
            &purpose2,
            MAX_PURPOSE_CHARS,
        )?;
        self.purpose2 = Some(purpose2);
        Ok(self)
    }

    /// The wire shape, with unused keys omitted.
    ///
    /// ```
    /// let json = letterxpress::BankForm::included_in_pdf().to_json();
    /// assert!(json.get("iban").is_none());
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "bank_form_included".to_string(),
            Value::Number(u8::from(self.included).into()),
        );
        for (key, value) in [
            ("payee", &self.payee),
            ("iban", &self.iban),
            ("bic", &self.bic),
            ("amount", &self.amount),
            ("purpose_of_payment", &self.purpose),
            ("purpose_of_payment2", &self.purpose2),
        ] {
            if let Some(value) = value {
                map.insert(key.to_string(), Value::String(value.clone()));
            }
        }
        Value::Object(map)
    }
}

/// The complete `letter` object sent to `POST /v3/printjobs` and
/// `POST /v3/emailjobs`.
///
/// The PDF carries the recipient address; the API has no address field. Every
/// setter validates, and the size budget (50 MB in total across the letter, its
/// attachments and its backgrounds) is enforced before anything is encoded.
///
/// ```
/// use letterxpress::{Color, Letter, Shipping};
///
/// let letter = Letter::new(b"%PDF-1.4\n...")?
///     .with_shipping(Shipping::National)
///     .with_color(Color::Color)
///     .with_filename_original("Rechnung-123456.pdf")?
///     .with_attachment(b"%PDF-1.4\nattachment\n")?;
///
/// assert_eq!(letter.attachments(), 1);
/// assert!(letter.total_pdf_bytes() < 1_000);
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Letter {
    pdf: EncodedPdf,
    pdf_bytes: usize,
    specification: Specification,
    filename_original: Option<String>,
    registered: Option<Registered>,
    dispatch_date: Option<Date>,
    separator: Option<Separator>,
    email: Option<EmailDelivery>,
    attachments: Vec<EncodedPdf>,
    attachment_bytes: usize,
    backgrounds: Option<Backgrounds>,
    terms: Option<Terms>,
    bank_form: Option<BankForm>,
    notice: Option<String>,
}

impl Letter {
    /// Checks the PDF, encodes it and computes the documented checksum (MD5 of
    /// the **base64 text**).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the payload is empty or does not start with
    /// `%PDF-`; [`Error::TooLarge`] when it exceeds
    /// [`MAX_PDF_BYTES`](crate::MAX_PDF_BYTES).
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// assert!(Letter::new(b"").is_err());
    /// assert!(Letter::new(b"Hello, not a PDF").is_err());
    /// assert!(Letter::new(b"%PDF-1.7\nbody\n").is_ok());
    /// ```
    pub fn new(pdf: &[u8]) -> Result<Self, Error> {
        let mut budget = MAX_PDF_BYTES;
        let encoded = encode_pdf("letter.base64_file", pdf, &mut budget)?;
        Ok(Self {
            pdf: encoded,
            pdf_bytes: pdf.len(),
            specification: Specification::new(),
            filename_original: None,
            registered: None,
            dispatch_date: None,
            separator: None,
            email: None,
            attachments: Vec::new(),
            attachment_bytes: 0,
            backgrounds: None,
            terms: None,
            bank_form: None,
            notice: None,
        })
    }

    /// Replaces the whole specification.
    ///
    /// ```
    /// use letterxpress::{Color, Letter, Specification};
    ///
    /// let spec = Specification::new().with_color(Color::Color);
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_specification(spec);
    /// assert_eq!(letter.specification().color(), Color::Color);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_specification(mut self, specification: Specification) -> Self {
        self.specification = specification;
        self
    }

    /// Sets the colour.
    ///
    /// ```
    /// use letterxpress::{Color, Letter};
    ///
    /// assert_eq!(Letter::new(b"%PDF-1.4\n")?.with_color(Color::Color).specification().color(), Color::Color);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.specification = self.specification.with_color(color);
        self
    }

    /// Sets simplex/duplex.
    ///
    /// ```
    /// use letterxpress::{Letter, PrintMode};
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_print_mode(PrintMode::Duplex);
    /// assert_eq!(letter.to_json()["specification"]["mode"], "duplex");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_print_mode(mut self, print_mode: PrintMode) -> Self {
        self.specification = self.specification.with_print_mode(print_mode);
        self
    }

    /// Sets the shipping class.
    ///
    /// ```
    /// use letterxpress::{Letter, Shipping};
    ///
    /// assert_eq!(Letter::new(b"%PDF-1.4\n")?.with_shipping(Shipping::International).specification().shipping(), Shipping::International);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_shipping(mut self, shipping: Shipping) -> Self {
        self.specification = self.specification.with_shipping(shipping);
        self
    }

    /// Requests a C4 envelope.
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_c4_envelope(true);
    /// assert_eq!(letter.to_json()["specification"]["c4"], 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_c4_envelope(mut self, c4_envelope: bool) -> Self {
        self.specification = self.specification.with_c4_envelope(c4_envelope);
        self
    }

    /// Sets the name the provider stores with the job.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the name is empty, longer than
    /// [`MAX_FILENAME_CHARS`](crate::MAX_FILENAME_CHARS), contains a path
    /// separator or a control character.
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// assert!(Letter::new(b"%PDF-1.4\n")?.with_filename_original("../etc/passwd").is_err());
    /// assert!(Letter::new(b"%PDF-1.4\n")?.with_filename_original("Rechnung-1.pdf").is_ok());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_filename_original(mut self, filename: impl Into<String>) -> Result<Self, Error> {
        let filename = filename.into();
        if filename.trim().is_empty() {
            return Err(Error::invalid(
                "letter.filename_original",
                "must not be empty",
            ));
        }
        check_chars("letter.filename_original", &filename, MAX_FILENAME_CHARS)?;
        if filename.contains(['/', '\\']) || filename.chars().any(char::is_control) {
            return Err(Error::invalid(
                "letter.filename_original",
                "must be a bare file name (no path separator, no control characters)",
            ));
        }
        self.filename_original = Some(filename);
        Ok(self)
    }

    /// Registers the letter.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the letter ships internationally — the
    /// provider documents registered mail as national only, and silently
    /// sending it unregistered is not an option.
    ///
    /// ```
    /// use letterxpress::{Letter, Registered, Shipping};
    ///
    /// let national = Letter::new(b"%PDF-1.4\n")?.with_shipping(Shipping::National);
    /// assert!(national.with_registered(Registered::EinschreibenEinwurf).is_ok());
    ///
    /// let international = Letter::new(b"%PDF-1.4\n")?.with_shipping(Shipping::International);
    /// assert!(international.with_registered(Registered::Einschreiben).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_registered(mut self, registered: Registered) -> Result<Self, Error> {
        if self.specification.shipping() == Shipping::International {
            return Err(Error::invalid(
                "letter.registered",
                "registered mail is national only; use Shipping::National",
            ));
        }
        self.registered = Some(registered);
        Ok(self)
    }

    /// Plans dispatch for `date`, which must be strictly after `today`.
    ///
    /// The clock is passed in rather than read, so the check is deterministic
    /// and testable; callers normally pass today's date in UTC.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `date` is not in the future.
    ///
    /// ```
    /// use letterxpress::{Date, Letter};
    ///
    /// let today = Date::new(2026, 10, 24)?;
    /// let tomorrow = Date::new(2026, 10, 25)?;
    /// assert!(Letter::new(b"%PDF-1.4\n")?.with_dispatch_date(tomorrow, today).is_ok());
    /// assert!(Letter::new(b"%PDF-1.4\n")?.with_dispatch_date(today, today).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_dispatch_date(mut self, date: Date, today: Date) -> Result<Self, Error> {
        if !date.is_after(&today) {
            return Err(Error::invalid(
                "letter.dispatch_date",
                format!("{date} is not in the future (today is {today})"),
            ));
        }
        self.dispatch_date = Some(date);
        Ok(self)
    }

    /// Sets the free-text `notice` (max 255 characters).
    ///
    /// This is the only correlation handle the API offers — there is no
    /// idempotency key — so it is worth filling with your own reference.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when empty, longer than
    /// [`MAX_NOTICE_CHARS`](crate::MAX_NOTICE_CHARS), or containing a control
    /// character.
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_notice("dispatch-4711")?;
    /// assert_eq!(letter.notice(), Some("dispatch-4711"));
    /// assert!(Letter::new(b"%PDF-1.4\n")?.with_notice("   ").is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_notice(mut self, notice: impl Into<String>) -> Result<Self, Error> {
        let notice = notice.into();
        if notice.trim().is_empty() {
            return Err(Error::invalid("letter.notice", "must not be empty"));
        }
        check_chars("letter.notice", &notice, MAX_NOTICE_CHARS)?;
        if notice.chars().any(char::is_control) {
            return Err(Error::invalid(
                "letter.notice",
                "must not contain control characters",
            ));
        }
        self.notice = Some(notice);
        Ok(self)
    }

    /// Adds an attachment (A4 portrait PDF). Order is preserved.
    ///
    /// # Errors
    ///
    /// The same checks as [`Letter::new`], plus the shared 50 MB request budget.
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_attachment(b"%PDF-1.4\nattachment\n")?;
    /// assert_eq!(letter.attachments(), 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_attachment(mut self, pdf: &[u8]) -> Result<Self, Error> {
        let mut budget = MAX_PDF_BYTES
            .saturating_sub(self.pdf_bytes + self.attachment_bytes + self.extra_bytes());
        let encoded = encode_pdf("letter.base64_attachments", pdf, &mut budget)?;
        self.attachment_bytes += pdf.len();
        self.attachments.push(encoded);
        Ok(self)
    }

    /// Adds background pages.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the background set is empty, or the PDFs fail
    /// the usual checks.
    ///
    /// ```
    /// use letterxpress::{Backgrounds, Letter};
    ///
    /// let backgrounds = Backgrounds::new().with_first_page(b"%PDF-1.4\nbg\n")?;
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_backgrounds(backgrounds)?;
    /// assert!(letter.to_json()["backgrounds"].is_object());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_backgrounds(mut self, backgrounds: Backgrounds) -> Result<Self, Error> {
        if backgrounds.is_empty() {
            return Err(Error::invalid(
                "letter.backgrounds",
                "at least one background page is required",
            ));
        }
        let total = self.pdf_bytes + self.attachment_bytes + self.extra_bytes() + backgrounds.bytes;
        if total > MAX_PDF_BYTES {
            return Err(Error::TooLarge {
                field: "letter",
                len: total,
                max: MAX_PDF_BYTES,
            });
        }
        self.backgrounds = Some(backgrounds);
        Ok(self)
    }

    /// Adds terms and conditions.
    ///
    /// ```
    /// use letterxpress::{Letter, Terms};
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_terms(Terms::new(b"%PDF-1.4\nt\n")?)?;
    /// assert!(letter.to_json()["terms_and_conditions"].is_object());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn with_terms(mut self, terms: Terms) -> Result<Self, Error> {
        let total = self.pdf_bytes + self.attachment_bytes + self.extra_bytes() + terms.bytes();
        if total > MAX_PDF_BYTES {
            return Err(Error::TooLarge {
                field: "letter",
                len: total,
                max: MAX_PDF_BYTES,
            });
        }
        self.terms = Some(terms);
        Ok(self)
    }

    /// Adds a bank-transfer form.
    ///
    /// ```
    /// use letterxpress::{BankForm, Letter};
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?.with_bank_form(BankForm::included_in_pdf())?;
    /// assert_eq!(letter.to_json()["bank_form"]["bank_form_included"], 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    /// # Errors
    ///
    /// [`Error::Validation`] when the value would be rejected by the provider,
    /// [`Error::TooLarge`] when it exceeds a documented ceiling.
    pub fn with_bank_form(mut self, bank_form: BankForm) -> Result<Self, Error> {
        self.bank_form = Some(bank_form);
        Ok(self)
    }

    /// Splits the PDF into individual letters.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when an `email_letter` is already set — the
    /// provider documents the two as mutually exclusive.
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption, Letter, Separator};
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?;
    /// assert!(letter.clone().with_separator(Separator::after_pages(2)?).is_ok());
    ///
    /// let email = letter.with_email(EmailDelivery::new("a@b.de", EmailOption::MailDirect)?)?;
    /// assert!(email.with_separator(Separator::after_pages(2)?).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_separator(mut self, separator: Separator) -> Result<Self, Error> {
        if self.email.is_some() {
            return Err(Error::invalid(
                "letter.serial_letter",
                "cannot be combined with email_letter (documented as mutually exclusive)",
            ));
        }
        self.separator = Some(separator);
        Ok(self)
    }

    /// Delivers this letter by e-mail instead of print (`email_letter`).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when a serial-letter separator is already set.
    ///
    /// ```
    /// use letterxpress::{EmailDelivery, EmailOption, Letter};
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?;
    /// let email = letter.with_email(EmailDelivery::new("a@b.de", EmailOption::MailSecure)?)?;
    /// assert!(email.has_email_delivery());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_email(mut self, email: EmailDelivery) -> Result<Self, Error> {
        if self.separator.is_some() {
            return Err(Error::invalid(
                "letter.email_letter",
                "cannot be combined with serial_letter (documented as mutually exclusive)",
            ));
        }
        self.email = Some(email);
        Ok(self)
    }

    /// The wire shape: exactly the keys that are set, nothing more.
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// let json = Letter::new(b"%PDF-1.4\n")?.to_json();
    /// assert!(json.get("notice").is_none());
    /// assert_eq!(json["specification"]["color"], "1");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "base64_file".to_string(),
            Value::String(self.pdf.base64.clone()),
        );
        map.insert(
            "base64_file_checksum".to_string(),
            Value::String(checksum_of(&self.pdf.base64)),
        );
        map.insert("specification".to_string(), self.specification.to_json());
        if let Some(filename) = &self.filename_original {
            map.insert(
                "filename_original".to_string(),
                Value::String(filename.clone()),
            );
        }
        if let Some(registered) = &self.registered {
            map.insert(
                "registered".to_string(),
                Value::String(registered.as_str().to_string()),
            );
        }
        if let Some(date) = &self.dispatch_date {
            map.insert("dispatch_date".to_string(), Value::String(date.to_string()));
        }
        if let Some(separator) = &self.separator {
            map.insert("serial_letter".to_string(), separator.to_json());
        }
        if let Some(email) = &self.email {
            map.insert("email_letter".to_string(), email.to_json());
        }
        if !self.attachments.is_empty() {
            map.insert(
                "base64_attachments".to_string(),
                Value::Array(
                    self.attachments
                        .iter()
                        .map(|attachment| Value::String(attachment.base64.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(backgrounds) = &self.backgrounds {
            map.insert("backgrounds".to_string(), backgrounds.to_json());
        }
        if let Some(terms) = &self.terms {
            map.insert("terms_and_conditions".to_string(), terms.to_json());
        }
        if let Some(bank_form) = &self.bank_form {
            map.insert("bank_form".to_string(), bank_form.to_json());
        }
        if let Some(notice) = &self.notice {
            map.insert("notice".to_string(), Value::String(notice.clone()));
        }
        Value::Object(map)
    }

    /// The print specification.
    ///
    /// ```
    /// assert_eq!(letterxpress::Letter::new(b"%PDF-1.4\n").unwrap().specification().shipping(), letterxpress::Shipping::Auto);
    /// ```
    #[must_use]
    pub fn specification(&self) -> &Specification {
        &self.specification
    }

    /// The `notice`, when set.
    ///
    /// ```
    /// # use letterxpress::Letter;
    /// assert_eq!(Letter::new(b"%PDF-1.4\n")?.notice(), None);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The MD5 checksum the provider expects (hex, of the base64 text).
    ///
    /// ```
    /// use letterxpress::Letter;
    ///
    /// let letter = Letter::new(b"%PDF-1.4\n")?;
    /// assert_eq!(letter.checksum(), letter.to_json()["base64_file_checksum"].as_str().unwrap());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn checksum(&self) -> String {
        checksum_of(&self.pdf.base64)
    }

    /// Number of attachments.
    ///
    /// ```
    /// assert_eq!(letterxpress::Letter::new(b"%PDF-1.4\n").unwrap().attachments(), 0);
    /// ```
    #[must_use]
    pub fn attachments(&self) -> usize {
        self.attachments.len()
    }

    /// Total PDF bytes in the request (body, attachments, backgrounds, terms).
    ///
    /// ```
    /// assert!(letterxpress::Letter::new(b"%PDF-1.4\n").unwrap().total_pdf_bytes() > 0);
    /// ```
    #[must_use]
    pub fn total_pdf_bytes(&self) -> usize {
        self.pdf_bytes + self.attachment_bytes + self.extra_bytes()
    }

    /// Whether an e-mail delivery was configured.
    ///
    /// ```
    /// assert!(!letterxpress::Letter::new(b"%PDF-1.4\n").unwrap().has_email_delivery());
    /// ```
    #[must_use]
    pub fn has_email_delivery(&self) -> bool {
        self.email.is_some()
    }

    /// Whether a serial-letter separator was configured.
    ///
    /// ```
    /// assert!(!letterxpress::Letter::new(b"%PDF-1.4\n").unwrap().has_separator());
    /// ```
    #[must_use]
    pub fn has_separator(&self) -> bool {
        self.separator.is_some()
    }

    fn extra_bytes(&self) -> usize {
        // `Backgrounds` and `Terms` carry their own byte counts; they are not
        // part of the attachment budget, but they do count towards the per
        // request ceiling.
        self.backgrounds
            .as_ref()
            .map_or(0, |backgrounds| backgrounds.bytes)
            + self.terms.as_ref().map_or(0, Terms::bytes)
    }
}

/// A price query (`GET /v3/price`), which needs `specification.pages`.
///
/// ```
/// use letterxpress::{Color, PriceRequest, PrintMode, Shipping};
///
/// let request = PriceRequest::new(3)?
///     .with_color(Color::Color)
///     .with_print_mode(PrintMode::Duplex)
///     .with_shipping(Shipping::National);
///
/// assert_eq!(request.to_json()["specification"]["pages"], 3);
/// assert_eq!(request.to_json()["specification"]["color"], "4");
/// # Ok::<(), letterxpress::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct PriceRequest {
    specification: Specification,
    pages: u32,
    registered: Option<Registered>,
    email_option: Option<EmailOption>,
}

impl PriceRequest {
    /// A quote for `pages` pages (1 to 10 000).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for zero pages or an implausible page count.
    ///
    /// ```
    /// use letterxpress::PriceRequest;
    ///
    /// assert!(PriceRequest::new(0).is_err());
    /// assert!(PriceRequest::new(1).is_ok());
    /// ```
    pub fn new(pages: u32) -> Result<Self, Error> {
        if pages == 0 {
            return Err(Error::invalid(
                "price.specification.pages",
                "must be at least 1",
            ));
        }
        if pages > 10_000 {
            return Err(Error::invalid(
                "price.specification.pages",
                "must be at most 10000",
            ));
        }
        Ok(Self {
            specification: Specification::new(),
            pages,
            registered: None,
            email_option: None,
        })
    }

    /// Sets the colour.
    ///
    /// ```
    /// use letterxpress::{Color, PriceRequest};
    ///
    /// assert_eq!(PriceRequest::new(1)?.with_color(Color::Color).to_json()["specification"]["color"], "4");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.specification = self.specification.with_color(color);
        self
    }

    /// Sets simplex/duplex.
    ///
    /// ```
    /// use letterxpress::{PriceRequest, PrintMode};
    ///
    /// let request = PriceRequest::new(1)?.with_print_mode(PrintMode::Duplex);
    /// assert_eq!(request.to_json()["specification"]["mode"], "duplex");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_print_mode(mut self, print_mode: PrintMode) -> Self {
        self.specification = self.specification.with_print_mode(print_mode);
        self
    }

    /// Sets the shipping class.
    ///
    /// ```
    /// use letterxpress::{PriceRequest, Shipping};
    ///
    /// assert_eq!(PriceRequest::new(1)?.with_shipping(Shipping::National).shipping(), Shipping::National);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_shipping(mut self, shipping: Shipping) -> Self {
        self.specification = self.specification.with_shipping(shipping);
        self
    }

    /// Requests a C4 envelope price.
    ///
    /// ```
    /// use letterxpress::PriceRequest;
    ///
    /// let request = PriceRequest::new(1)?.with_c4_envelope(true);
    /// assert_eq!(request.to_json()["specification"]["c4"], 1);
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_c4_envelope(mut self, c4_envelope: bool) -> Self {
        self.specification = self.specification.with_c4_envelope(c4_envelope);
        self
    }

    /// Prices the registered-mail surcharge.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for international shipping — registered mail is
    /// national only.
    ///
    /// ```
    /// use letterxpress::{PriceRequest, Registered, Shipping};
    ///
    /// let national = PriceRequest::new(1)?.with_shipping(Shipping::National);
    /// assert!(national.with_registered(Registered::Einschreiben).is_ok());
    ///
    /// let international = PriceRequest::new(1)?.with_shipping(Shipping::International);
    /// assert!(international.with_registered(Registered::Einschreiben).is_err());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    pub fn with_registered(mut self, registered: Registered) -> Result<Self, Error> {
        if self.specification.shipping() == Shipping::International {
            return Err(Error::invalid(
                "price.registered",
                "registered mail is national only; use Shipping::National",
            ));
        }
        self.registered = Some(registered);
        Ok(self)
    }

    /// Prices a SMART@MAIL delivery.
    ///
    /// ```
    /// use letterxpress::{EmailOption, PriceRequest};
    ///
    /// let request = PriceRequest::new(1)?.with_email_option(EmailOption::MailSecure);
    /// assert_eq!(request.to_json()["specification"]["email_option"], "mailsecure");
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn with_email_option(mut self, email_option: EmailOption) -> Self {
        self.email_option = Some(email_option);
        self
    }

    /// The page count being priced.
    ///
    /// ```
    /// assert_eq!(letterxpress::PriceRequest::new(4).unwrap().pages(), 4);
    /// ```
    #[must_use]
    pub fn pages(&self) -> u32 {
        self.pages
    }

    /// The shipping class being priced.
    ///
    /// ```
    /// assert_eq!(letterxpress::PriceRequest::new(1).unwrap().shipping(), letterxpress::Shipping::Auto);
    /// ```
    #[must_use]
    pub fn shipping(&self) -> Shipping {
        self.specification.shipping()
    }

    /// The wire shape (`specification` with `pages`, plus optional keys).
    ///
    /// ```
    /// use letterxpress::PriceRequest;
    ///
    /// let json = PriceRequest::new(2)?.to_json();
    /// assert_eq!(json["specification"]["pages"], 2);
    /// assert!(json.get("registered").is_none());
    /// # Ok::<(), letterxpress::Error>(())
    /// ```
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut specification = self.specification.to_json();
        if let Some(map) = specification.as_object_mut() {
            map.insert("pages".to_string(), Value::Number(self.pages.into()));
            if let Some(email_option) = &self.email_option {
                map.insert(
                    "email_option".to_string(),
                    Value::String(email_option.as_str().to_string()),
                );
            }
        }
        let mut map = Map::new();
        map.insert("specification".to_string(), specification);
        if let Some(registered) = &self.registered {
            map.insert(
                "registered".to_string(),
                Value::String(registered.as_str().to_string()),
            );
        }
        Value::Object(map)
    }
}

/// Checks a PDF and encodes it, charging the shared request budget before
/// allocating anything.
fn encode_pdf(field: &'static str, pdf: &[u8], budget: &mut usize) -> Result<EncodedPdf, Error> {
    if pdf.is_empty() {
        return Err(Error::invalid(field, "must not be empty"));
    }
    if pdf.len() > *budget || pdf.len() > MAX_PDF_BYTES {
        return Err(Error::TooLarge {
            field,
            len: pdf.len(),
            max: (*budget).min(MAX_PDF_BYTES),
        });
    }
    if !pdf.starts_with(b"%PDF-") {
        return Err(Error::invalid(
            field,
            "must be a PDF (payload has to start with \"%PDF-\")",
        ));
    }
    // The provider caps the *request*; base64 inflates by 4/3. Because
    // `pdf.len() <= MAX_PDF_BYTES` is enforced above, the encoded payload is
    // bounded by `MAX_PDF_BYTES * 4/3 + 4` without a second branch — and only
    // the bounded input is encoded, so nothing oversized is ever allocated.
    *budget -= pdf.len();
    Ok(EncodedPdf {
        base64: STANDARD.encode(pdf),
    })
}

/// MD5 of the base64 text, hex-encoded — the documented `base64_file_checksum`.
fn checksum_of(base64: &str) -> String {
    let digest = Md5::digest(base64.as_bytes());
    let mut hex = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Character-count check (not byte count: the provider limits characters).
fn check_chars(field: &'static str, value: &str, max: usize) -> Result<(), Error> {
    let len = value.chars().count();
    if len > max {
        return Err(Error::TooLarge { field, len, max });
    }
    Ok(())
}

/// Plausibility check for an e-mail address, stricter than the provider's
/// silence but weaker than RFC 5322 — it rejects what the provider would
/// bounce, without pretending to be a validator.
pub(crate) fn validate_email(field: &'static str, value: &str) -> Result<(), Error> {
    if value.len() > 254 {
        return Err(Error::invalid(field, "must be at most 254 characters"));
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(char::is_control) {
        return Err(Error::invalid(
            field,
            "must not contain whitespace or control characters",
        ));
    }
    let Some((local, domain)) = value.split_once('@') else {
        return Err(Error::invalid(field, "must contain exactly one '@'"));
    };
    if local.is_empty() || local.len() > 64 {
        return Err(Error::invalid(
            field,
            "the part before '@' must be 1 to 64 characters",
        ));
    }
    if domain.contains('@') {
        return Err(Error::invalid(field, "must contain exactly one '@'"));
    }
    if !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return Err(Error::invalid(
            field,
            "the domain must contain a dot and no empty label",
        ));
    }
    Ok(())
}

/// Accepts `81`, `81,00`, `81.00`.
fn is_decimal_amount(value: &str) -> bool {
    let mut seen_separator = false;
    let mut digits = 0;
    for character in value.chars() {
        if character.is_ascii_digit() {
            digits += 1;
        } else if (character == ',' || character == '.') && !seen_separator {
            seen_separator = true;
        } else {
            return false;
        }
    }
    digits > 0
}

impl Terms {
    /// Original PDF size, for the request budget.
    fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf() -> Vec<u8> {
        b"%PDF-1.4\n".to_vec()
    }

    #[test]
    fn email_validation_covers_every_rejection() {
        for bad in [
            "",
            "no-at-sign",
            "a@b",
            "a@@b.de",
            "a b@c.de",
            "a@b..de",
            "a@.de",
            "a@b.",
            "\u{0}@b.de",
        ] {
            assert!(
                EmailDelivery::new(bad, EmailOption::MailDirect).is_err(),
                "{bad:?}"
            );
        }
        let long_local = format!("{}@b.de", "a".repeat(65));
        assert!(EmailDelivery::new(long_local, EmailOption::MailDirect).is_err());
        let long_address = format!("a@{}", "b".repeat(253));
        assert!(EmailDelivery::new(long_address, EmailOption::MailDirect).is_err());
        assert!(EmailDelivery::new("kunde@example.de", EmailOption::MailDirect).is_ok());
    }

    #[test]
    fn decimal_amounts_accept_both_separators_and_nothing_else() {
        assert!(is_decimal_amount("81"));
        assert!(is_decimal_amount("81,00"));
        assert!(is_decimal_amount("81.00"));
        assert!(!is_decimal_amount(""));
        assert!(!is_decimal_amount(","));
        assert!(!is_decimal_amount("8,1,0"));
        assert!(!is_decimal_amount("8 1"));
        assert!(!is_decimal_amount("EUR 81"));
    }

    #[test]
    fn keyword_separators_are_bounded() {
        assert!(Separator::at_keyword("x".repeat(255)).is_ok());
        assert!(Separator::at_keyword("x".repeat(256)).is_err());
    }

    #[test]
    fn bank_forms_cover_both_documented_modes() {
        assert_eq!(
            BankForm::included_in_pdf().to_json()["bank_form_included"],
            1
        );
        assert!(BankForm::included_in_pdf().to_json().get("payee").is_none());

        let generated = BankForm::generated(
            "A",
            "DE12500105170648489890",
            "INGDDEFF",
            "1,00",
            "Rechnung",
        )
        .expect("generated");
        assert_eq!(generated.to_json()["bank_form_included"], 0);
        assert_eq!(generated.to_json()["bic"], "INGDDEFF");

        assert!(BankForm::generated("A", "DE12500105170648489890", "ING", "1,00", "R").is_err());
        assert!(
            BankForm::generated("A", "DE12500105170648489890", "INGDDEFFXXX1", "1,00", "R")
                .is_err()
        );
        assert!(BankForm::generated("A", "DE1250010517", "INGDDEFFXXX", "1,00", "R").is_err());
    }

    #[test]
    fn the_letter_budget_covers_backgrounds_and_terms() {
        // 30 MB body + 25 MB background: each is individually valid, together
        // they exceed the documented per-request ceiling.
        let body = {
            let mut bytes = pdf();
            bytes.resize(30_000_000, b'x');
            bytes
        };
        let background = {
            let mut bytes = pdf();
            bytes.resize(25_000_000, b'x');
            bytes
        };
        let letter = Letter::new(&body).expect("letter");
        let backgrounds = Backgrounds::new()
            .with_first_page(&background)
            .expect("background");
        assert!(matches!(
            letter.with_backgrounds(backgrounds),
            Err(Error::TooLarge {
                field: "letter",
                ..
            })
        ));

        let letter = Letter::new(&body).expect("letter");
        let terms = Terms::new(&background).expect("terms");
        assert!(matches!(
            letter.with_terms(terms),
            Err(Error::TooLarge {
                field: "letter",
                ..
            })
        ));
    }

    #[test]
    fn attachments_count_towards_the_same_budget() {
        let body = {
            let mut bytes = pdf();
            bytes.resize(30_000_000, b'x');
            bytes
        };
        let attachment = {
            let mut bytes = pdf();
            bytes.resize(25_000_000, b'x');
            bytes
        };
        let letter = Letter::new(&body).expect("letter");
        assert!(matches!(
            letter.with_attachment(&attachment),
            Err(Error::TooLarge { .. })
        ));
    }

    #[test]
    fn checksums_match_the_documented_md5() {
        assert_eq!(
            checksum_of("abc"),
            "900150983cd24fb0d6963f7d28e17f72",
            "MD5 of the base64 text, hex encoded"
        );
    }
}
