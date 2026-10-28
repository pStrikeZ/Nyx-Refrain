#![no_main]

use libfuzzer_sys::fuzz_target;
use raop::timing::{parse_timing_request, parse_timing_response};

fuzz_target!(|data: &[u8]| {
    let _ = parse_timing_request(data);
    let _ = parse_timing_response(data);
});
