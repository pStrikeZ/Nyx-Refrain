//! Device discovery module.
//!
//! Handles mDNS-based discovery of `_raop._tcp` and `_airplay._tcp` services,
//! network interface binding, manual target specification, and persistent device caching.

pub mod browser;
pub mod cache;
pub mod device;
pub mod manual;
pub mod mdns;

pub use browser::{DiscoveryError, DiscoveryOptions, discover_devices};
pub use cache::{DeviceCache, default_cache_path, default_config_dir};
pub use device::DiscoveredDevice;
pub use manual::{build_manual_target, parse_target_addr};
pub use mdns::{
    MDNS_MULTICAST_ADDR, MDNS_PORT, ParsedMdnsRecords, build_mdns_ptr_query, parse_mdns_response,
};
