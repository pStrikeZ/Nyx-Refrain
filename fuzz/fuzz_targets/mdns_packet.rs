#![no_main]

use discovery::mdns::{parse_mdns_response, read_dns_name};
use libfuzzer_sys::fuzz_target;
use raop::capabilities::DeviceCapabilities;

fuzz_target!(|data: &[u8]| {
    if let Some(records) = parse_mdns_response(data) {
        let _ = records.assemble_devices();
    }
    let _ = read_dns_name(data, 0);
    let _ = DeviceCapabilities::from_dns_txt_bytes(data);
});
