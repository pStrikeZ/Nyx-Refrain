#![no_main]

use libfuzzer_sys::fuzz_target;
use raop::control::{parse_retransmit_request, parse_sync_packet};

fuzz_target!(|data: &[u8]| {
    let _ = parse_sync_packet(data);
    let _ = parse_retransmit_request(data);
});
