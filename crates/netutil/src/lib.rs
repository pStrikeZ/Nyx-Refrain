//! Network utility functions, interface enumeration, and adapter management.
//!
//! Provides network interface enumeration, filtering (distinguishing physical
//! adapters from TUN/virtual adapters), explicit interface binding, and
//! IP_UNICAST_IF configuration on Windows.

pub mod bind;
pub mod classify;
pub mod firewall;
pub mod interface;

#[cfg(unix)]
mod enumerate_unix;

#[cfg(windows)]
mod enumerate_windows;

#[cfg(target_os = "linux")]
pub use bind::bind_to_device_linux_raw;
#[cfg(windows)]
pub use bind::set_ip_unicast_if_windows_raw;
pub use bind::{apply_interface_binding, bind_udp_socket};
pub use classify::{
    ClassificationConfig, ClassifiedInterface, LINUX_PREFIX_BLACKLIST, WINDOWS_NAME_BLACKLIST,
    classify_interface, select_interface_for_target,
};
pub use interface::{InterfaceInfo, InterfaceType, Ipv4Net};

/// Enumerates all network interfaces on the local system.
pub fn enumerate_interfaces() -> std::io::Result<Vec<InterfaceInfo>> {
    #[cfg(unix)]
    {
        enumerate_unix::enumerate_interfaces_unix()
    }

    #[cfg(windows)]
    {
        enumerate_windows::enumerate_interfaces_windows()
    }

    #[cfg(not(any(unix, windows)))]
    {
        Ok(Vec::new())
    }
}

/// Enumerates and classifies all network interfaces on the system.
pub fn enumerate_and_classify_interfaces(
    config: &ClassificationConfig,
) -> std::io::Result<Vec<ClassifiedInterface>> {
    let raw = enumerate_interfaces()?;
    let classified = raw
        .into_iter()
        .map(|iface| classify_interface(&iface, config))
        .collect();
    Ok(classified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enumerate_interfaces_native() {
        let ifaces = enumerate_interfaces();
        assert!(
            ifaces.is_ok(),
            "enumerate_interfaces failed: {:?}",
            ifaces.err()
        );
        let list = ifaces.unwrap();
        assert!(!list.is_empty(), "Expected at least one interface on host");

        let classified =
            enumerate_and_classify_interfaces(&ClassificationConfig::default()).unwrap();
        assert!(!classified.is_empty());

        // On this machine, eth0 should be classified as usable physical
        #[cfg(target_os = "linux")]
        {
            let eth0 = classified.iter().find(|c| c.iface.name == "eth0");
            if let Some(c) = eth0 {
                assert!(
                    c.is_usable_physical,
                    "eth0 should be usable physical, reason: {}",
                    c.reason
                );
            }
        }
    }
}
