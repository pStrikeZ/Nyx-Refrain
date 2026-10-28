#![no_main]

use libfuzzer_sys::fuzz_target;
use raop::rtsp::parse_rtsp_response;

fuzz_target!(|data: &[u8]| {
    let _ = parse_rtsp_response(data);
});
