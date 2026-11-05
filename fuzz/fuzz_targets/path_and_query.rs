#![no_main]

use letterxpress::transport::{Method, RecordedRequest};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|url: &str| {
    let recorded = RecordedRequest {
        method: Method::Get,
        url: url.to_owned(),
        body: Vec::new(),
        headers: Vec::new(),
    };
    // Must never panic: text that is not an absolute URL is the only failure
    // mode, and it is reported as `None`.
    let _ = recorded.path_and_query();
});
