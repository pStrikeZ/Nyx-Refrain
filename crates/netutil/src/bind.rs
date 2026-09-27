//! Explicit interface binding helpers.
//!
//! Provides utilities to bind sockets to explicit local IPv4 addresses and
//! bind to specific network interfaces using:
//! - Windows: `IP_UNICAST_IF` (interface index in network byte order for IPv4, per MS docs)
//! - Linux: `SO_BINDTODEVICE` (best-effort, ignoring EPERM)

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use tracing::{debug, warn};

/// Binds a UDP socket to `local_ip:port` and applies interface binding.
pub fn bind_udp_socket(
    local_ip: Ipv4Addr,
    iface_index: Option<u32>,
    iface_name: Option<&str>,
    port: u16,
) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // Apply platform interface binding before or after socket bind
    apply_interface_binding(&socket, iface_index, iface_name)?;

    let bind_addr = SockAddr::from(SocketAddr::new(IpAddr::V4(local_ip), port));
    socket.bind(&bind_addr)?;

    Ok(socket.into())
}

/// Applies interface binding to a socket (`IP_UNICAST_IF` on Windows, `SO_BINDTODEVICE` on Linux).
pub fn apply_interface_binding(
    socket: &Socket,
    _iface_index: Option<u32>,
    _iface_name: Option<&str>,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        if let Some(name) = _iface_name {
            bind_to_device_linux(socket, name)?;
        }
    }

    #[cfg(windows)]
    {
        if let Some(index) = _iface_index {
            set_ip_unicast_if_windows(socket, index)?;
        }
    }

    Ok(())
}

/// Linux `SO_BINDTODEVICE` binding helper.
///
/// Binds the socket to the specified network interface name (e.g. "eth0").
/// This is best-effort: EPERM (Operation not permitted)
/// is logged and ignored to support unprivileged containers or non-root users.
#[cfg(target_os = "linux")]
pub fn bind_to_device_linux(socket: &Socket, iface_name: &str) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    bind_to_device_linux_raw(socket.as_raw_fd(), iface_name)
}

/// Linux `SO_BINDTODEVICE` binding on raw file descriptor.
#[cfg(target_os = "linux")]
pub fn bind_to_device_linux_raw(
    raw_fd: std::os::unix::io::RawFd,
    iface_name: &str,
) -> std::io::Result<()> {
    if iface_name.is_empty() {
        return Ok(());
    }

    let mut ifname_bytes = iface_name.as_bytes().to_vec();
    ifname_bytes.push(0); // null terminator

    let ret = unsafe {
        libc::setsockopt(
            raw_fd,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            ifname_bytes.as_ptr() as *const libc::c_void,
            ifname_bytes.len() as libc::socklen_t,
        )
    };

    if ret == 0 {
        debug!(
            "Bound socket to device '{}' via SO_BINDTODEVICE",
            iface_name
        );
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EPERM) {
            debug!(
                "SO_BINDTODEVICE to '{}' returned EPERM (missing CAP_NET_RAW?); ignoring (best-effort)",
                iface_name
            );
            Ok(())
        } else {
            warn!("SO_BINDTODEVICE to '{}' failed: {}", iface_name, err);
            Err(err)
        }
    }
}

/// Windows `IP_UNICAST_IF` binding helper.
///
/// Upstream Reference:
/// - Microsoft Docs: "IP_UNICAST_IF socket option"
///   (https://learn.microsoft.com/en-us/windows/win32/winsock/ipproto-ip-socket-options)
///   "For IPv4, the interface index must be passed in network byte order."
/// - `ws2ipdef.h`: `#define IP_UNICAST_IF 31`
#[cfg(windows)]
pub fn set_ip_unicast_if_windows(socket: &Socket, if_index: u32) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    set_ip_unicast_if_windows_raw(socket.as_raw_socket() as usize, if_index)
}

/// Windows `IP_UNICAST_IF` binding on raw socket handle.
#[cfg(windows)]
pub fn set_ip_unicast_if_windows_raw(raw_socket: usize, if_index: u32) -> std::io::Result<()> {
    use windows::Win32::Networking::WinSock::{IPPROTO_IP, SOCKET, setsockopt};

    const IP_UNICAST_IF: i32 = 31;
    let be_index = if_index.to_be();
    let raw_sock = SOCKET(raw_socket);

    let ret = unsafe {
        setsockopt(
            raw_sock,
            IPPROTO_IP.0,
            IP_UNICAST_IF,
            Some(&be_index.to_ne_bytes()),
        )
    };

    if ret != 0 {
        let err = std::io::Error::last_os_error();
        warn!(
            "setsockopt IP_UNICAST_IF failed for interface index {}: {}",
            if_index, err
        );
        return Err(err);
    }

    debug!(
        "Set IP_UNICAST_IF to interface index {} (network byte order: 0x{:08X})",
        if_index, be_index
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bind_udp_socket_loopback() {
        let socket = bind_udp_socket(Ipv4Addr::new(127, 0, 0, 1), None, None, 0);
        assert!(socket.is_ok());
        let sock = socket.unwrap();
        let local_addr = sock.local_addr().unwrap();
        assert_eq!(local_addr.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(local_addr.port() > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_bind_to_device_best_effort() {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        // Binding to lo should either succeed or return EPERM (which is ignored)
        let res = bind_to_device_linux(&socket, "lo");
        assert!(res.is_ok());

        // Empty interface name is a no-op
        let res_empty = bind_to_device_linux(&socket, "");
        assert!(res_empty.is_ok());
    }

    #[test]
    fn test_apply_interface_binding_empty_options() {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        let res = apply_interface_binding(&socket, None, None);
        assert!(res.is_ok());
    }
}
