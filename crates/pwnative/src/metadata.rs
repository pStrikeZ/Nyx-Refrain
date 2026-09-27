//! PipeWire Metadata interface.
//!
//! Handles setting and restoring default audio sink (`default.configured.audio.sink`).

use std::collections::HashMap;
use thiserror::Error;
use tracing::{debug, info, trace};

use crate::connection::{Connection, ConnectionError, Message};
use crate::pod::{PodBuilder, PodError, PodParser, PodValue};

pub const PW_TYPE_INTERFACE_METADATA: &str = "PipeWire:Interface:Metadata";

pub const PW_METADATA_EVENT_PROPERTY: u8 = 0;
pub const PW_METADATA_METHOD_SET_PROPERTY: u8 = 1;
pub const PW_METADATA_METHOD_CLEAR: u8 = 2;

pub const DEFAULT_SINK_KEY: &str = "default.configured.audio.sink";
pub const DEFAULT_SINK_TYPE: &str = "Spa:String:JSON";

#[derive(Error, Debug)]
pub enum MetadataError {
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("POD error: {0}")]
    Pod(#[from] PodError),
}

pub struct MetadataProxy {
    pub proxy_id: u32,
    pub original_sink: Option<String>,
    pub had_original_sink: bool,
    pub current_sink: Option<String>,
    pub properties: HashMap<(u32, String), (String, String)>,
}

impl MetadataProxy {
    pub fn new(proxy_id: u32) -> Self {
        Self {
            proxy_id,
            original_sink: None,
            had_original_sink: false,
            current_sink: None,
            properties: HashMap::new(),
        }
    }

    pub fn set_property(
        &self,
        conn: &mut Connection,
        subject: u32,
        key: Option<&str>,
        type_: Option<&str>,
        value: Option<&str>,
    ) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_int(subject as i32);
        match key {
            Some(k) => b.write_string(k),
            None => b.write_none(),
        }
        match type_ {
            Some(t) => b.write_string(t),
            None => b.write_none(),
        }
        match value {
            Some(v) => b.write_string(v),
            None => b.write_none(),
        }
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            self.proxy_id,
            PW_METADATA_METHOD_SET_PROPERTY,
            seq,
            &[],
            b.as_bytes(),
        )?;
        debug!(
            "Metadata: set_property subject={}, key={:?}, type={:?}, value={:?}",
            subject, key, type_, value
        );
        Ok(seq)
    }

    pub fn set_default_sink(
        &mut self,
        conn: &mut Connection,
        node_name: &str,
    ) -> Result<u32, ConnectionError> {
        let json_value = format!("{{\"name\":\"{}\"}}", node_name);
        self.current_sink = Some(json_value.clone());
        self.set_property(
            conn,
            0,
            Some(DEFAULT_SINK_KEY),
            Some(DEFAULT_SINK_TYPE),
            Some(&json_value),
        )
    }

    pub fn restore_default_sink(&mut self, conn: &mut Connection) -> Result<(), ConnectionError> {
        if self.current_sink.is_none() {
            return Ok(());
        }
        if self.had_original_sink {
            if let Some(ref orig) = self.original_sink {
                info!("Restoring original default audio sink to {}", orig);
                self.set_property(
                    conn,
                    0,
                    Some(DEFAULT_SINK_KEY),
                    Some(DEFAULT_SINK_TYPE),
                    Some(orig),
                )?;
            }
        } else {
            info!("Removing default configured audio sink setting");
            self.set_property(conn, 0, Some(DEFAULT_SINK_KEY), None, None)?;
        }
        self.current_sink = None;
        Ok(())
    }

    pub fn handle_message(&mut self, msg: &Message) -> Result<(), MetadataError> {
        if msg.id != self.proxy_id {
            return Ok(());
        }

        if msg.opcode == PW_METADATA_EVENT_PROPERTY {
            let mut parser = PodParser::new(&msg.body);
            let val = parser.next()?;
            if let PodValue::Struct(items) = val {
                if items.len() >= 4 {
                    let subject = items[0].as_u32().unwrap_or(0);
                    let key = items[1].as_str().map(|s| s.to_string());
                    let type_ = items[2].as_str().map(|s| s.to_string());
                    let value = items[3].as_str().map(|s| s.to_string());

                    trace!(
                        "Metadata Property: subject={}, key={:?}, type={:?}, val={:?}",
                        subject, key, type_, value
                    );

                    if let Some(ref k) = key {
                        if subject == 0 && k == DEFAULT_SINK_KEY {
                            // If we haven't saved original sink yet, save it
                            if self.current_sink.is_none() && !self.had_original_sink {
                                if let Some(ref v) = value {
                                    self.original_sink = Some(v.clone());
                                    self.had_original_sink = true;
                                    debug!("Captured original default audio sink: {}", v);
                                }
                            }
                        }

                        if let (Some(t), Some(v)) = (type_, value) {
                            self.properties.insert((subject, k.clone()), (t, v));
                        } else {
                            self.properties.remove(&(subject, k.clone()));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
