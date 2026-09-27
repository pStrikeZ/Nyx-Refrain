//! PipeWire Core and Client interfaces and memory pool table.

use std::collections::HashMap;
use std::os::fd::RawFd;
use thiserror::Error;
use tracing::{debug, error, trace};

use crate::connection::{Connection, ConnectionError, Message};
use crate::pod::{PodBuilder, PodError, PodParser, PodValue};

pub const PW_CORE_PROXY_ID: u32 = 0;
pub const PW_CLIENT_PROXY_ID: u32 = 1;

pub const PW_CORE_METHOD_HELLO: u8 = 1;
pub const PW_CORE_METHOD_SYNC: u8 = 2;
pub const PW_CORE_METHOD_PONG: u8 = 3;
pub const PW_CORE_METHOD_ERROR: u8 = 4;
pub const PW_CORE_METHOD_GET_REGISTRY: u8 = 5;
pub const PW_CORE_METHOD_CREATE_OBJECT: u8 = 6;
pub const PW_CORE_METHOD_DESTROY: u8 = 7;

pub const PW_CORE_EVENT_INFO: u8 = 0;
pub const PW_CORE_EVENT_DONE: u8 = 1;
pub const PW_CORE_EVENT_PING: u8 = 2;
pub const PW_CORE_EVENT_ERROR: u8 = 3;
pub const PW_CORE_EVENT_REMOVE_ID: u8 = 4;
pub const PW_CORE_EVENT_BOUND_ID: u8 = 5;
pub const PW_CORE_EVENT_ADD_MEM: u8 = 6;
pub const PW_CORE_EVENT_REMOVE_MEM: u8 = 7;
pub const PW_CORE_EVENT_BOUND_PROPS: u8 = 8;

pub const PW_CLIENT_METHOD_UPDATE_PROPERTIES: u8 = 2;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("POD error: {0}")]
    Pod(#[from] PodError),
    #[error("PipeWire server error: id={id}, seq={seq}, res={res}: {message}")]
    ServerError {
        id: u32,
        seq: u32,
        res: i32,
        message: String,
    },
    #[error("Memory ID {0} not found in mem table")]
    MemNotFound(u32),
    #[error("Memory map failed: {0}")]
    MmapFailed(String),
}

pub struct MemBlock {
    pub id: u32,
    pub type_: u32,
    pub flags: u32,
    pub fd: RawFd,
    pub map_ptr: *mut u8,
    pub map_size: usize,
}

unsafe impl Send for MemBlock {}
unsafe impl Sync for MemBlock {}

impl Drop for MemBlock {
    fn drop(&mut self) {
        if !self.map_ptr.is_null() && self.map_size > 0 {
            unsafe {
                libc::munmap(self.map_ptr as *mut libc::c_void, self.map_size);
            }
        }
        if self.fd >= 0 {
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

#[derive(Default)]
pub struct MemTable {
    blocks: HashMap<u32, MemBlock>,
}

impl MemTable {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }

    pub fn insert(&mut self, id: u32, type_: u32, fd: RawFd, flags: u32) {
        debug!("MemTable: insert id={}, type={}, fd={}", id, type_, fd);
        self.blocks.insert(
            id,
            MemBlock {
                id,
                type_,
                flags,
                fd,
                map_ptr: std::ptr::null_mut(),
                map_size: 0,
            },
        );
    }

    pub fn remove(&mut self, id: u32) {
        debug!("MemTable: remove id={}", id);
        self.blocks.remove(&id);
    }

    pub fn get_fd(&self, id: u32) -> Option<RawFd> {
        self.blocks.get(&id).map(|b| b.fd)
    }

    /// Map or return existing mapped pointer for the entire memory block.
    pub fn mmap_block(&mut self, id: u32, min_size: usize) -> Result<*mut u8, CoreError> {
        let block = self.blocks.get_mut(&id).ok_or(CoreError::MemNotFound(id))?;

        if !block.map_ptr.is_null() {
            if block.map_size >= min_size {
                return Ok(block.map_ptr);
            }
            // Remap if needed
            unsafe {
                libc::munmap(block.map_ptr as *mut libc::c_void, block.map_size);
                block.map_ptr = std::ptr::null_mut();
            }
        }

        // Determine size from fstat if min_size is 0 or smaller
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let res = unsafe { libc::fstat(block.fd, &mut stat) };
        let size = if res == 0 && stat.st_size > 0 {
            stat.st_size as usize
        } else {
            min_size
        };

        if size == 0 {
            return Err(CoreError::MmapFailed(
                "Cannot mmap 0-sized block".to_string(),
            ));
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                block.fd,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(CoreError::MmapFailed(
                std::io::Error::last_os_error().to_string(),
            ));
        }

        block.map_ptr = ptr as *mut u8;
        block.map_size = size;
        debug!(
            "MemTable: mmapped block id={} at {:?} (size={})",
            id, ptr, size
        );
        Ok(block.map_ptr)
    }
}

pub struct CoreProxy {
    pub core_id: u32,
    pub client_id: u32,
    pub client_global_id: Option<u32>,
    pub server_version: String,
    pub mem_table: MemTable,
}

impl Default for CoreProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreProxy {
    pub fn new() -> Self {
        Self {
            core_id: 0,
            client_id: PW_CLIENT_PROXY_ID,
            client_global_id: None,
            server_version: String::new(),
            mem_table: MemTable::new(),
        }
    }

    pub fn send_hello(conn: &mut Connection) -> Result<(), ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(3); // PW_VERSION_CORE = 3
        b.pop_struct(frame);

        conn.send_message(PW_CORE_PROXY_ID, PW_CORE_METHOD_HELLO, 0, &[], b.as_bytes())
    }

    pub fn send_sync(conn: &mut Connection, id: u32, seq: i32) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(id as i32);
        b.write_int(seq);
        b.pop_struct(frame);

        let msg_seq = conn.next_seq();
        conn.send_message(
            PW_CORE_PROXY_ID,
            PW_CORE_METHOD_SYNC,
            msg_seq,
            &[],
            b.as_bytes(),
        )?;
        Ok(msg_seq)
    }

    pub fn send_pong(conn: &mut Connection, id: u32, seq: i32) -> Result<(), ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(id as i32);
        b.write_int(seq);
        b.pop_struct(frame);

        conn.send_message(PW_CORE_PROXY_ID, PW_CORE_METHOD_PONG, 0, &[], b.as_bytes())
    }

    pub fn send_get_registry(
        conn: &mut Connection,
        registry_id: u32,
    ) -> Result<(), ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(3); // version = 3
        b.write_int(registry_id as i32);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            PW_CORE_PROXY_ID,
            PW_CORE_METHOD_GET_REGISTRY,
            seq,
            &[],
            b.as_bytes(),
        )
    }

    pub fn send_create_object(
        conn: &mut Connection,
        factory_name: &str,
        type_: &str,
        version: u32,
        props: &[(&str, &str)],
        new_id: u32,
    ) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_string(factory_name);
        b.write_string(type_);
        b.write_int(version as i32);
        b.write_dict(props);
        b.write_int(new_id as i32);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            PW_CORE_PROXY_ID,
            PW_CORE_METHOD_CREATE_OBJECT,
            seq,
            &[],
            b.as_bytes(),
        )?;
        Ok(seq)
    }

    pub fn send_destroy(conn: &mut Connection, id: u32) -> Result<(), ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(id as i32);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            PW_CORE_PROXY_ID,
            PW_CORE_METHOD_DESTROY,
            seq,
            &[],
            b.as_bytes(),
        )
    }

    pub fn send_update_client_properties(
        conn: &mut Connection,
        client_id: u32,
        props: &[(&str, &str)],
    ) -> Result<(), ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_dict(props);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            client_id,
            PW_CLIENT_METHOD_UPDATE_PROPERTIES,
            seq,
            &[],
            b.as_bytes(),
        )
    }

    /// Process a message directed at Core proxy (id=0).
    pub fn handle_core_message(
        &mut self,
        conn: &mut Connection,
        msg: &Message,
    ) -> Result<Option<CoreEvent>, CoreError> {
        match msg.opcode {
            PW_CORE_EVENT_INFO => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 6 {
                        if let Some(id) = items[0].as_u32() {
                            self.core_id = id;
                        }
                        if let Some(v) = items[4].as_str() {
                            self.server_version = v.to_string();
                        }
                    }
                }
                debug!(
                    "Core Info: core_id={}, client_id={}, server_version={}",
                    self.core_id, self.client_id, self.server_version
                );
                Ok(Some(CoreEvent::Info))
            }
            PW_CORE_EVENT_DONE => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 2 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let seq = items[1].as_i32().unwrap_or(0);
                        trace!("Core Done: id={}, seq={}", id, seq);
                        return Ok(Some(CoreEvent::Done { id, seq }));
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_PING => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 2 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let seq = items[1].as_i32().unwrap_or(0);
                        trace!("Core Ping: id={}, seq={}, replying with Pong", id, seq);
                        Self::send_pong(conn, id, seq)?;
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_ERROR => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 4 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let seq = items[1].as_u32().unwrap_or(0);
                        let res = items[2].as_i32().unwrap_or(0);
                        let err_msg = items[3].as_str().unwrap_or("unknown error").to_string();
                        error!(
                            "Core Error: id={}, seq={}, res={}: {}",
                            id, seq, res, err_msg
                        );
                        return Err(CoreError::ServerError {
                            id,
                            seq,
                            res,
                            message: err_msg,
                        });
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_ADD_MEM => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    // Struct { Int id, Id type, Fd fd (index into msg fds), Int flags }
                    let fd = match items.get(2) {
                        Some(PodValue::Fd(idx)) if *idx >= 0 => msg.fds.get(*idx as usize).copied(),
                        _ => None,
                    };
                    if let (true, Some(fd)) = (items.len() >= 4, fd) {
                        let id = items[0].as_u32().unwrap_or(0);
                        let type_ = items[1].as_u32().unwrap_or(0);
                        let flags = items[3].as_u32().unwrap_or(0);
                        self.mem_table.insert(id, type_, fd, flags);
                        return Ok(Some(CoreEvent::AddMem { id }));
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_REMOVE_MEM => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if !items.is_empty() {
                        let id = items[0].as_u32().unwrap_or(0);
                        self.mem_table.remove(id);
                        return Ok(Some(CoreEvent::RemoveMem { id }));
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_BOUND_ID => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 2 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let global_id = items[1].as_u32().unwrap_or(0);
                        debug!("Core BoundId: proxy_id={} -> global_id={}", id, global_id);
                        if id == PW_CLIENT_PROXY_ID {
                            self.client_global_id = Some(global_id);
                        }
                        return Ok(Some(CoreEvent::BoundId { id, global_id }));
                    }
                }
                Ok(None)
            }
            PW_CORE_EVENT_BOUND_PROPS => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 2 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let global_id = items[1].as_u32().unwrap_or(0);
                        debug!(
                            "Core BoundProps: proxy_id={} -> global_id={}",
                            id, global_id
                        );
                        return Ok(Some(CoreEvent::BoundId { id, global_id }));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

pub enum CoreEvent {
    Info,
    Done { id: u32, seq: i32 },
    AddMem { id: u32 },
    RemoveMem { id: u32 },
    BoundId { id: u32, global_id: u32 },
}
