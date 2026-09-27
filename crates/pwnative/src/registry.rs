//! PipeWire Registry interface.

use std::collections::HashMap;
use thiserror::Error;
use tracing::{debug, trace};

use crate::connection::{Connection, ConnectionError, Message};
use crate::pod::{PodBuilder, PodError, PodParser, PodValue};

pub const PW_REGISTRY_EVENT_GLOBAL: u8 = 0;
pub const PW_REGISTRY_EVENT_GLOBAL_REMOVE: u8 = 1;

pub const PW_REGISTRY_METHOD_BIND: u8 = 1;
pub const PW_REGISTRY_METHOD_DESTROY: u8 = 2;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("POD error: {0}")]
    Pod(#[from] PodError),
}

#[derive(Debug, Clone)]
pub struct GlobalObject {
    pub id: u32,
    pub permissions: u32,
    pub type_: String,
    pub version: u32,
    pub props: HashMap<String, String>,
}

pub enum RegistryEvent {
    Global(GlobalObject),
    GlobalRemove(u32),
}

pub struct RegistryProxy {
    pub proxy_id: u32,
    pub globals: HashMap<u32, GlobalObject>,
}

impl RegistryProxy {
    pub fn new(proxy_id: u32) -> Self {
        Self {
            proxy_id,
            globals: HashMap::new(),
        }
    }

    pub fn bind(
        &self,
        conn: &mut Connection,
        global_id: u32,
        type_: &str,
        version: u32,
        new_id: u32,
    ) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(global_id as i32);
        b.write_string(type_);
        b.write_int(version as i32);
        b.write_int(new_id as i32);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            self.proxy_id,
            PW_REGISTRY_METHOD_BIND,
            seq,
            &[],
            b.as_bytes(),
        )?;
        debug!(
            "Registry: bind global {} ({}) version {} -> new_id {}",
            global_id, type_, version, new_id
        );
        Ok(seq)
    }

    pub fn handle_message(
        &mut self,
        msg: &Message,
    ) -> Result<Option<RegistryEvent>, RegistryError> {
        if msg.id != self.proxy_id {
            return Ok(None);
        }

        match msg.opcode {
            PW_REGISTRY_EVENT_GLOBAL => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 5 {
                        let id = items[0].as_u32().unwrap_or(0);
                        let permissions = items[1].as_u32().unwrap_or(0);
                        let type_ = items[2].as_str().unwrap_or("").to_string();
                        let version = items[3].as_u32().unwrap_or(0);
                        let dict_items = PodParser::parse_dict(&items[4]).unwrap_or_default();
                        let mut props = HashMap::new();
                        for (k, v) in dict_items {
                            props.insert(k, v);
                        }

                        let global = GlobalObject {
                            id,
                            permissions,
                            type_,
                            version,
                            props,
                        };
                        trace!("Registry Global: id={}, type={}", id, global.type_);
                        self.globals.insert(id, global.clone());
                        return Ok(Some(RegistryEvent::Global(global)));
                    }
                }
                Ok(None)
            }
            PW_REGISTRY_EVENT_GLOBAL_REMOVE => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if !items.is_empty() {
                        let id = items[0].as_u32().unwrap_or(0);
                        trace!("Registry GlobalRemove: id={}", id);
                        self.globals.remove(&id);
                        return Ok(Some(RegistryEvent::GlobalRemove(id)));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}
