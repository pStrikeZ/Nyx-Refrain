//! PipeWire native protocol connection and socket communication.
//!
//! Handles Unix domain socket connection, 16-byte protocol V3 framing,
//! and passing file descriptors over SCM_RIGHTS.

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{debug, trace};

pub const HEADER_SIZE: usize = 16;
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

#[derive(Error, Debug)]
pub enum ConnectionError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Socket path could not be resolved")]
    SocketNotFound,
    #[error("Connection closed by peer")]
    Disconnected,
    #[error("Protocol error: {0}")]
    Protocol(&'static str),
    #[error("Message too large: {0} bytes")]
    MessageTooLarge(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub id: u32,
    pub opcode: u8,
    pub size: u32,
    pub seq: u32,
    pub n_fds: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.id.to_ne_bytes());
        let word1 = ((self.opcode as u32) << 24) | (self.size & 0x00ff_ffff);
        buf[4..8].copy_from_slice(&word1.to_ne_bytes());
        buf[8..12].copy_from_slice(&self.seq.to_ne_bytes());
        buf[12..16].copy_from_slice(&self.n_fds.to_ne_bytes());
        buf
    }

    pub fn decode(bytes: &[u8; HEADER_SIZE]) -> Self {
        let id = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        let word1 = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
        let opcode = (word1 >> 24) as u8;
        let size = word1 & 0x00ff_ffff;
        let seq = u32::from_ne_bytes(bytes[8..12].try_into().unwrap());
        let n_fds = u32::from_ne_bytes(bytes[12..16].try_into().unwrap());
        Self {
            id,
            opcode,
            size,
            seq,
            n_fds,
        }
    }
}

#[derive(Debug)]
pub struct Message {
    pub id: u32,
    pub opcode: u8,
    pub seq: u32,
    pub fds: Vec<RawFd>,
    pub body: Vec<u8>,
}

pub struct Connection {
    stream: UnixStream,
    in_buf: Vec<u8>,
    in_fds: VecDeque<RawFd>,
    next_seq: u32,
}

impl AsRawFd for Connection {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

impl Connection {
    pub fn resolve_socket_path() -> Result<PathBuf, ConnectionError> {
        let remote = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".to_string());
        if remote.starts_with('/') {
            return Ok(PathBuf::from(remote));
        }

        if let Ok(dir) = std::env::var("PIPEWIRE_RUNTIME_DIR") {
            let p = PathBuf::from(dir).join(&remote);
            if p.exists() {
                return Ok(p);
            }
        }

        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            let p = PathBuf::from(dir).join(&remote);
            if p.exists() {
                return Ok(p);
            }
        }

        let run_pw = PathBuf::from("/run/pipewire").join(&remote);
        if run_pw.exists() {
            return Ok(run_pw);
        }

        // Fallback default
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            return Ok(PathBuf::from(dir).join(&remote));
        }

        Ok(run_pw)
    }

    pub fn connect(path: Option<&Path>) -> Result<Self, ConnectionError> {
        let resolved = match path {
            Some(p) => p.to_path_buf(),
            None => Self::resolve_socket_path()?,
        };
        debug!("Connecting to PipeWire daemon at {:?}", resolved);
        let stream = UnixStream::connect(&resolved)?;
        stream.set_nonblocking(true)?;

        Ok(Self {
            stream,
            in_buf: Vec::with_capacity(32 * 1024),
            in_fds: VecDeque::new(),
            next_seq: 1,
        })
    }

    pub fn next_seq(&mut self) -> u32 {
        let s = self.next_seq;
        self.next_seq = (self.next_seq + 1) & 0x7fff_ffff;
        s
    }

    pub fn send_message(
        &mut self,
        id: u32,
        opcode: u8,
        seq: u32,
        fds: &[RawFd],
        body: &[u8],
    ) -> Result<(), ConnectionError> {
        if body.len() > MAX_MESSAGE_SIZE {
            return Err(ConnectionError::MessageTooLarge(body.len()));
        }

        let hdr = Header {
            id,
            opcode,
            size: body.len() as u32,
            seq,
            n_fds: fds.len() as u32,
        };
        let hdr_bytes = hdr.encode();

        let mut iov = [
            libc::iovec {
                iov_base: hdr_bytes.as_ptr() as *mut libc::c_void,
                iov_len: hdr_bytes.len(),
            },
            libc::iovec {
                iov_base: body.as_ptr() as *mut libc::c_void,
                iov_len: body.len(),
            },
        ];

        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = iov.as_mut_ptr();
        msg.msg_iovlen = if body.is_empty() { 1 } else { 2 };

        let cmsg_space = if !fds.is_empty() {
            unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) }
        } else {
            0
        };
        let mut cmsg_buf = vec![0u8; cmsg_space as usize];

        if !fds.is_empty() {
            msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
            msg.msg_controllen = cmsg_space as _;
            let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
            if !cmsg.is_null() {
                unsafe {
                    (*cmsg).cmsg_level = libc::SOL_SOCKET;
                    (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                    (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as _;
                    let data_ptr = libc::CMSG_DATA(cmsg) as *mut RawFd;
                    std::ptr::copy_nonoverlapping(fds.as_ptr(), data_ptr, fds.len());
                }
            }
        }

        let sock_fd = self.stream.as_raw_fd();
        let mut total_sent = 0;
        let total_len = HEADER_SIZE + body.len();

        loop {
            let res = unsafe { libc::sendmsg(sock_fd, &msg, libc::MSG_NOSIGNAL) };
            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    // Poll until socket is writable
                    let mut pfd = libc::pollfd {
                        fd: sock_fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    unsafe { libc::poll(&mut pfd, 1, 1000) };
                    continue;
                }
                return Err(ConnectionError::Io(err));
            }
            total_sent += res as usize;
            if total_sent >= total_len {
                break;
            }
            // Once initial chunk is sent, FDs are already transferred; remaining is body
            msg.msg_control = std::ptr::null_mut();
            msg.msg_controllen = 0;
            if total_sent < HEADER_SIZE {
                iov[0].iov_base =
                    unsafe { (hdr_bytes.as_ptr() as *mut libc::c_void).add(total_sent) };
                iov[0].iov_len = HEADER_SIZE - total_sent;
                iov[1].iov_base = body.as_ptr() as *mut libc::c_void;
                iov[1].iov_len = body.len();
                msg.msg_iovlen = 2;
            } else {
                let body_sent = total_sent - HEADER_SIZE;
                iov[0].iov_base = unsafe { (body.as_ptr() as *mut libc::c_void).add(body_sent) };
                iov[0].iov_len = body.len() - body_sent;
                msg.msg_iovlen = 1;
            }
        }

        trace!(
            "Sent message id={}, opcode={}, size={}, seq={}, n_fds={}",
            id,
            opcode,
            body.len(),
            seq,
            fds.len()
        );
        Ok(())
    }

    /// Read available data and return next message if complete.
    /// Non-blocking. Returns `Ok(None)` if no complete message is available yet.
    pub fn try_recv_message(&mut self) -> Result<Option<Message>, ConnectionError> {
        // First check if already have a complete message in in_buf
        if let Some(msg) = self.try_pop_message()? {
            return Ok(Some(msg));
        }

        // Try reading more data from socket
        let mut read_chunk = [0u8; 16 * 1024];
        let mut cmsg_buf = [0u8; 1024];

        let mut iov = [libc::iovec {
            iov_base: read_chunk.as_mut_ptr() as *mut libc::c_void,
            iov_len: read_chunk.len(),
        }];

        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = iov.as_mut_ptr();
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len() as _;

        let n = unsafe { libc::recvmsg(self.stream.as_raw_fd(), &mut msg, 0) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock
                || err.kind() == std::io::ErrorKind::Interrupted
            {
                return Ok(None);
            }
            return Err(ConnectionError::Io(err));
        }
        if n == 0 {
            return Err(ConnectionError::Disconnected);
        }

        // Parse ancillary control message for SCM_RIGHTS file descriptors
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !cmsg.is_null() {
            unsafe {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let cmsg_len = (*cmsg).cmsg_len as usize;
                    let header_len = libc::CMSG_LEN(0) as usize;
                    if cmsg_len >= header_len {
                        let payload_len = cmsg_len - header_len;
                        let num_fds = payload_len / std::mem::size_of::<RawFd>();
                        let data_ptr = libc::CMSG_DATA(cmsg) as *const RawFd;
                        for i in 0..num_fds {
                            let fd = *data_ptr.add(i);
                            self.in_fds.push_back(fd);
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }

        self.in_buf.extend_from_slice(&read_chunk[..n as usize]);
        self.try_pop_message()
    }

    fn try_pop_message(&mut self) -> Result<Option<Message>, ConnectionError> {
        if self.in_buf.len() < HEADER_SIZE {
            return Ok(None);
        }

        let hdr_bytes: &[u8; HEADER_SIZE] = self.in_buf[..HEADER_SIZE].try_into().unwrap();
        let hdr = Header::decode(hdr_bytes);

        if hdr.size as usize > MAX_MESSAGE_SIZE {
            return Err(ConnectionError::MessageTooLarge(hdr.size as usize));
        }

        let total_size = HEADER_SIZE + hdr.size as usize;
        if self.in_buf.len() < total_size {
            return Ok(None);
        }

        if self.in_fds.len() < hdr.n_fds as usize {
            // Need more fds that might arrive in subsequent packets
            return Ok(None);
        }

        let mut fds = Vec::with_capacity(hdr.n_fds as usize);
        for _ in 0..hdr.n_fds {
            if let Some(fd) = self.in_fds.pop_front() {
                fds.push(fd);
            }
        }

        let body = self.in_buf[HEADER_SIZE..total_size].to_vec();
        self.in_buf.drain(..total_size);

        trace!(
            "Received message id={}, opcode={}, size={}, seq={}, n_fds={}",
            hdr.id,
            hdr.opcode,
            hdr.size,
            hdr.seq,
            fds.len()
        );
        Ok(Some(Message {
            id: hdr.id,
            opcode: hdr.opcode,
            seq: hdr.seq,
            fds,
            body,
        }))
    }

    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Read all currently available messages, polling with timeout if buffer is empty.
    pub fn read_messages_timeout(
        &mut self,
        timeout_ms: i32,
    ) -> Result<Vec<Message>, ConnectionError> {
        let mut msgs = Vec::new();
        while let Some(msg) = self.try_pop_message()? {
            msgs.push(msg);
        }
        if !msgs.is_empty() {
            return Ok(msgs);
        }

        let mut pfd = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ret < 0 {
            return Err(ConnectionError::Io(std::io::Error::last_os_error()));
        }
        if ret == 0 {
            return Ok(msgs);
        }

        while let Some(msg) = self.try_recv_message()? {
            msgs.push(msg);
        }

        Ok(msgs)
    }

    pub fn read_messages(&mut self) -> Result<Vec<Message>, ConnectionError> {
        self.read_messages_timeout(5000)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Clean up any remaining unconsumed file descriptors
        for fd in self.in_fds.drain(..) {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_round_trip() {
        let hdr = Header {
            id: 0,
            opcode: 1, // Core Hello
            size: 16,
            seq: 42,
            n_fds: 0,
        };
        let encoded = hdr.encode();
        let decoded = Header::decode(&encoded);
        assert_eq!(hdr, decoded);
    }

    #[test]
    fn test_header_opcode_packing() {
        let hdr = Header {
            id: 1234,
            opcode: 0xab,
            size: 0x123456,
            seq: 9999,
            n_fds: 4,
        };
        let encoded = hdr.encode();
        let word1 = u32::from_ne_bytes(encoded[4..8].try_into().unwrap());
        assert_eq!((word1 >> 24) as u8, 0xab);
        assert_eq!(word1 & 0x00ff_ffff, 0x123456);

        let decoded = Header::decode(&encoded);
        assert_eq!(hdr, decoded);
    }
}
