//! PipeWire ClientNode interface implementation (port of remote-node.c client-side).

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use thiserror::Error;
use tracing::{debug, warn};

use crate::activation::{
    PW_NODE_ACTIVATION_FINISHED, PW_NODE_ACTIVATION_INACTIVE, PW_VERSION_NODE_ACTIVATION,
    PwNodeActivation, SPA_STATUS_NEED_DATA, SpaChunk, SpaIoBuffers, SpaIoPosition,
};

const SPA_ID_INVALID: u32 = u32::MAX;
/// `enum spa_io_type` SPA_IO_Position.
const SPA_IO_POSITION: u32 = 7;

/// `enum spa_node_command` (spa/node/command.h).
const SPA_NODE_COMMAND_SUSPEND: u32 = 0;
const SPA_NODE_COMMAND_PAUSE: u32 = 1;
const SPA_NODE_COMMAND_START: u32 = 2;
use crate::connection::{Connection, ConnectionError, Message};
use crate::core::{CoreError, MemTable};
use crate::pod::{
    PodBuilder, PodError, PodParser, PodValue, Property, SPA_AUDIO_CHANNEL_FL,
    SPA_AUDIO_CHANNEL_FR, SPA_AUDIO_FORMAT_DSP_F32, SPA_AUDIO_FORMAT_F32P, SPA_DATA_MEM_FD,
    SPA_DATA_MEM_ID, SPA_DATA_MEM_PTR, SPA_FORMAT_AUDIO_CHANNELS, SPA_FORMAT_AUDIO_FORMAT,
    SPA_FORMAT_AUDIO_POSITION, SPA_FORMAT_AUDIO_RATE, SPA_FORMAT_MEDIA_SUBTYPE,
    SPA_FORMAT_MEDIA_TYPE, SPA_IO_BUFFERS, SPA_MEDIA_SUBTYPE_DSP, SPA_MEDIA_SUBTYPE_RAW,
    SPA_MEDIA_TYPE_AUDIO, SPA_PARAM_BUFFERS, SPA_PARAM_BUFFERS_BLOCKS, SPA_PARAM_BUFFERS_BUFFERS,
    SPA_PARAM_BUFFERS_DATA_TYPE, SPA_PARAM_BUFFERS_SIZE, SPA_PARAM_BUFFERS_STRIDE,
    SPA_PARAM_ENUM_FORMAT, SPA_PARAM_FORMAT, SPA_PARAM_INFO_READ, SPA_PARAM_INFO_READWRITE,
    SPA_PARAM_INFO_SERIAL, SPA_PARAM_INFO_WRITE, SPA_PARAM_IO, SPA_PARAM_IO_ID, SPA_PARAM_IO_SIZE,
    SPA_PARAM_PROPS, SPA_TYPE_OBJECT_FORMAT, SPA_TYPE_OBJECT_PARAM_BUFFERS,
    SPA_TYPE_OBJECT_PARAM_IO, SPA_TYPE_OBJECT_PROPS, align8,
};
use crate::rt::{MAX_BUFFERS_PER_PORT, MAX_PEER_TARGETS, SharedRtData};

pub const PW_TYPE_INTERFACE_CLIENT_NODE: &str = "PipeWire:Interface:ClientNode";
pub const PW_VERSION_CLIENT_NODE: u32 = 6;

pub const PW_CLIENT_NODE_METHOD_UPDATE: u8 = 2;
pub const PW_CLIENT_NODE_METHOD_PORT_UPDATE: u8 = 3;
pub const PW_CLIENT_NODE_METHOD_SET_ACTIVE: u8 = 4;
pub const PW_CLIENT_NODE_METHOD_PORT_BUFFERS: u8 = 6;

pub const PW_CLIENT_NODE_EVENT_TRANSPORT: u8 = 0;
pub const PW_CLIENT_NODE_EVENT_SET_PARAM: u8 = 1;
pub const PW_CLIENT_NODE_EVENT_SET_IO: u8 = 2;
pub const PW_CLIENT_NODE_EVENT_EVENT: u8 = 3;
pub const PW_CLIENT_NODE_EVENT_COMMAND: u8 = 4;
pub const PW_CLIENT_NODE_EVENT_ADD_PORT: u8 = 5;
pub const PW_CLIENT_NODE_EVENT_REMOVE_PORT: u8 = 6;
pub const PW_CLIENT_NODE_EVENT_PORT_SET_PARAM: u8 = 7;
pub const PW_CLIENT_NODE_EVENT_PORT_USE_BUFFERS: u8 = 8;
pub const PW_CLIENT_NODE_EVENT_PORT_SET_IO: u8 = 9;
pub const PW_CLIENT_NODE_EVENT_SET_ACTIVATION: u8 = 10;
pub const PW_CLIENT_NODE_EVENT_PORT_SET_MIX_INFO: u8 = 11;

pub const SPA_DIRECTION_INPUT: i32 = 0;
pub const SPA_DIRECTION_OUTPUT: i32 = 1;

pub const PW_CLIENT_NODE_UPDATE_PARAMS: u32 = 1 << 0;
pub const PW_CLIENT_NODE_UPDATE_INFO: u32 = 1 << 1;
pub const PW_CLIENT_NODE_PORT_UPDATE_PARAMS: u32 = 1 << 0;
pub const PW_CLIENT_NODE_PORT_UPDATE_INFO: u32 = 1 << 1;

pub const SPA_NODE_CHANGE_MASK_FLAGS: u64 = 1 << 0;
pub const SPA_NODE_CHANGE_MASK_PROPS: u64 = 1 << 1;
pub const SPA_NODE_CHANGE_MASK_PARAMS: u64 = 1 << 2;

pub const SPA_PORT_CHANGE_MASK_FLAGS: u64 = 1 << 0;
pub const SPA_PORT_CHANGE_MASK_RATE: u64 = 1 << 1;
pub const SPA_PORT_CHANGE_MASK_PROPS: u64 = 1 << 2;
pub const SPA_PORT_CHANGE_MASK_PARAMS: u64 = 1 << 3;

/// `enum spa_param_type` entries not covered by `pod.rs`.
pub const SPA_PARAM_PORT_CONFIG: u32 = 11;

/// `enum spa_prop` audio keys (spa/param/props.h).
pub const SPA_PROP_VOLUME: u32 = 0x10003;
pub const SPA_PROP_MUTE: u32 = 0x10004;
pub const SPA_PROP_CHANNEL_VOLUMES: u32 = 0x10008;
pub const SPA_PROP_CHANNEL_MAP: u32 = 0x1000b;

pub const SPA_PORT_FLAG_PHYSICAL: u64 = 1 << 6;
pub const SPA_PORT_FLAG_TERMINAL: u64 = 1 << 7;

#[derive(Error, Debug)]
pub enum ClientNodeError {
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("Core error: {0}")]
    Core(#[from] CoreError),
    #[error("POD error: {0}")]
    Pod(#[from] PodError),
}

pub struct TransportInfo {
    pub readfd: RawFd,
    pub writefd: RawFd,
    pub activation_ptr: *mut PwNodeActivation,
}

unsafe impl Send for TransportInfo {}
unsafe impl Sync for TransportInfo {}

pub struct ClientNodeProxy {
    pub proxy_id: u32,
    pub node_name: String,
    pub node_description: String,
    pub transport: Option<TransportInfo>,
    pub rt_data: Arc<SharedRtData>,
    /// Current Props (what the desktop volume slider shows / sets).
    volumes: [f32; 2],
    mute: bool,
    /// Toggled on every Props change so the server emits a param change even when the
    /// flags are otherwise identical (SPA_PARAM_INFO_SERIAL).
    props_serial: bool,
    /// Format the server set on each port (PortSetParam). The server answers a port's
    /// Format query from what we announce, so it must be reported back: a link to an
    /// already configured port (a second stream while one plays) otherwise fails with
    /// "get input format: No such file or directory" and the new stream is killed.
    /// (Our ports only take DSP F32, so only whether one is set matters.)
    port_configured: [bool; 2],
}

impl ClientNodeProxy {
    pub fn new(proxy_id: u32, node_name: &str, node_description: &str) -> Self {
        Self {
            proxy_id,
            node_name: node_name.to_string(),
            node_description: node_description.to_string(),
            transport: None,
            rt_data: Arc::new(SharedRtData::default()),
            volumes: [1.0, 1.0],
            mute: false,
            props_serial: false,
            port_configured: [false, false],
        }
    }

    /// Send ClientNode.Update with 2 input ports and node properties.
    pub fn send_node_update(&self, conn: &mut Connection) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();

        let change_mask = PW_CLIENT_NODE_UPDATE_PARAMS | PW_CLIENT_NODE_UPDATE_INFO;
        b.write_int(change_mask as i32);
        b.write_int(2); // n_params: EnumFormat + Props

        // Param 0: EnumFormat for Node (Required by WirePlumber si-audio-adapter)
        let obj_fmt = b.push_object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_ENUM_FORMAT);
        b.write_prop(SPA_FORMAT_MEDIA_TYPE, 0);
        b.write_id(SPA_MEDIA_TYPE_AUDIO);
        b.write_prop(SPA_FORMAT_MEDIA_SUBTYPE, 0);
        b.write_id(SPA_MEDIA_SUBTYPE_RAW);
        b.write_prop(SPA_FORMAT_AUDIO_FORMAT, 0);
        b.write_id(SPA_AUDIO_FORMAT_F32P);
        b.write_prop(SPA_FORMAT_AUDIO_RATE, 0);
        b.write_int(48000);
        b.write_prop(SPA_FORMAT_AUDIO_CHANNELS, 0);
        b.write_int(2);
        b.write_prop(SPA_FORMAT_AUDIO_POSITION, 0);
        b.write_array_id(&[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR]);
        b.pop_object(obj_fmt);

        // Param 1: Props (volume state shown/set by desktop volume controls)
        self.write_props(&mut b);

        // Info struct
        let info_frame = b.push_struct();
        b.write_int(2); // max_input_ports = 2
        b.write_int(0); // max_output_ports = 0
        let node_change_mask =
            SPA_NODE_CHANGE_MASK_FLAGS | SPA_NODE_CHANGE_MASK_PROPS | SPA_NODE_CHANGE_MASK_PARAMS;
        b.write_long(node_change_mask as i64);
        b.write_long(0); // flags = 0
        b.write_dict_items(&[
            ("media.class", "Audio/Sink"),
            ("node.name", &self.node_name),
            ("node.description", &self.node_description),
            ("media.name", &self.node_description),
            // Keep running while nothing plays so the capture stream delivers silence at
            // wall-clock pace (like WASAPI loopback): never pause/suspend on idle.
            ("node.always-process", "true"),
            ("node.pause-on-idle", "false"),
            // Ask for a ~10 ms graph quantum (WASAPI cadence). The live AirPlay sender only
            // buffers ~48 ms, so large quanta (pw-play asks for 100 ms) deliver bursts that
            // overflow it and then underrun between bursts, which is audible as clicks.
            ("node.latency", "480/48000"),
            ("session.suspend-timeout-seconds", "0"),
            ("node.want-driver", "true"),
            ("audio.channels", "2"),
            ("audio.position", "[ FL, FR ]"),
            ("object.register", "true"),
            ("node.virtual", "false"),
            ("priority.session", "2000"),
            ("priority.driver", "2000"),
            // WirePlumber's restore-stream also covers route-less Audio/* nodes and would
            // re-apply a stored volume (e.g. 100%) over our initial one on every creation.
            ("state.restore-props", "false"),
        ]);
        // Param info. WirePlumber's si-audio-adapter sets PortConfig on activation and only
        // finishes (and starts linking streams to us) once it sees a Props param change,
        // see wireplumber modules/module-si-audio-adapter.c on_node_params_changed.
        let props_flags = SPA_PARAM_INFO_READWRITE
            | if self.props_serial {
                SPA_PARAM_INFO_SERIAL
            } else {
                0
            };
        b.write_int(3);
        b.write_id(SPA_PARAM_ENUM_FORMAT);
        b.write_int(SPA_PARAM_INFO_READ as i32);
        b.write_id(SPA_PARAM_PROPS);
        b.write_int(props_flags as i32);
        b.write_id(SPA_PARAM_PORT_CONFIG);
        b.write_int(SPA_PARAM_INFO_WRITE as i32);
        b.pop_struct(info_frame);

        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            self.proxy_id,
            PW_CLIENT_NODE_METHOD_UPDATE,
            seq,
            &[],
            b.as_bytes(),
        )?;
        debug!("ClientNode: sent Update (node info and props)");
        Ok(seq)
    }

    fn write_props(&self, b: &mut PodBuilder) {
        let obj = b.push_object(SPA_TYPE_OBJECT_PROPS, SPA_PARAM_PROPS);
        b.write_prop(SPA_PROP_VOLUME, 0);
        b.write_float(1.0);
        b.write_prop(SPA_PROP_MUTE, 0);
        b.write_bool(self.mute);
        b.write_prop(SPA_PROP_CHANNEL_VOLUMES, 0);
        b.write_array_float(&self.volumes);
        b.write_prop(SPA_PROP_CHANNEL_MAP, 0);
        b.write_array_id(&[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR]);
        b.pop_object(obj);
    }

    /// Apply a Props object from SetParam (mute / channelVolumes / volume) to our state.
    pub fn apply_props(&mut self, props: &[Property]) {
        for p in props {
            match (p.key, &p.value) {
                (SPA_PROP_MUTE, PodValue::Bool(m)) => self.mute = *m,
                (SPA_PROP_VOLUME, PodValue::Float(f)) => {
                    self.volumes = [f.max(0.0), f.max(0.0)];
                }
                (SPA_PROP_CHANNEL_VOLUMES, PodValue::Array { values, .. }) => {
                    for (i, v) in values.iter().take(2).enumerate() {
                        if let PodValue::Float(f) = v {
                            self.volumes[i] = f.max(0.0);
                        }
                    }
                    if values.len() == 1 {
                        self.volumes[1] = self.volumes[0];
                    }
                }
                _ => {}
            }
        }
    }

    /// Set volume and mute Props directly and toggle the serial flag.
    pub fn set_props(&mut self, volumes: [f32; 2], mute: bool) {
        self.volumes = volumes;
        self.mute = mute;
        self.props_serial = !self.props_serial;
    }

    /// Send ClientNode.PortUpdate for port_id (0: FL, 1: FR).
    pub fn send_port_update(
        &self,
        conn: &mut Connection,
        port_id: u32,
    ) -> Result<u32, ConnectionError> {
        let (channel, name) = if port_id == 0 {
            ("FL", "playback_FL")
        } else {
            ("FR", "playback_FR")
        };

        let mut b = PodBuilder::new();
        let frame = b.push_struct();

        b.write_int(SPA_DIRECTION_INPUT);
        b.write_int(port_id as i32);
        let change_mask = PW_CLIENT_NODE_PORT_UPDATE_PARAMS | PW_CLIENT_NODE_PORT_UPDATE_INFO;
        b.write_int(change_mask as i32);
        let configured = self
            .port_configured
            .get(port_id as usize)
            .copied()
            .unwrap_or(false);
        // EnumFormat, [Format], Buffers, IO
        b.write_int(if configured { 4 } else { 3 });

        // 1. Param EnumFormat: DSP Float Mono
        let obj_fmt = b.push_object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_ENUM_FORMAT);
        b.write_prop(SPA_FORMAT_MEDIA_TYPE, 0);
        b.write_id(SPA_MEDIA_TYPE_AUDIO);
        b.write_prop(SPA_FORMAT_MEDIA_SUBTYPE, 0);
        b.write_id(SPA_MEDIA_SUBTYPE_DSP);
        b.write_prop(SPA_FORMAT_AUDIO_FORMAT, 0);
        b.write_id(SPA_AUDIO_FORMAT_DSP_F32);
        b.pop_object(obj_fmt);

        if configured {
            let obj = b.push_object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_FORMAT);
            b.write_prop(SPA_FORMAT_MEDIA_TYPE, 0);
            b.write_id(SPA_MEDIA_TYPE_AUDIO);
            b.write_prop(SPA_FORMAT_MEDIA_SUBTYPE, 0);
            b.write_id(SPA_MEDIA_SUBTYPE_DSP);
            b.write_prop(SPA_FORMAT_AUDIO_FORMAT, 0);
            b.write_id(SPA_AUDIO_FORMAT_DSP_F32);
            b.pop_object(obj);
        }

        // 2. Param Buffers
        let obj_buf = b.push_object(SPA_TYPE_OBJECT_PARAM_BUFFERS, SPA_PARAM_BUFFERS);
        b.write_prop(SPA_PARAM_BUFFERS_BUFFERS, 0);
        b.write_choice_range_int(16, 1, 64);
        b.write_prop(SPA_PARAM_BUFFERS_BLOCKS, 0);
        b.write_int(1);
        b.write_prop(SPA_PARAM_BUFFERS_SIZE, 0);
        b.write_choice_range_int(16384, 256, 32768);
        b.write_prop(SPA_PARAM_BUFFERS_STRIDE, 0);
        b.write_int(4);
        b.write_prop(SPA_PARAM_BUFFERS_DATA_TYPE, 0);
        b.write_choice_flags_int((1 << SPA_DATA_MEM_FD) | (1 << SPA_DATA_MEM_PTR));
        b.pop_object(obj_buf);

        // 3. Param IO: Buffers
        let obj_io = b.push_object(SPA_TYPE_OBJECT_PARAM_IO, SPA_PARAM_IO);
        b.write_prop(SPA_PARAM_IO_ID, 0);
        b.write_id(SPA_IO_BUFFERS);
        b.write_prop(SPA_PARAM_IO_SIZE, 0);
        b.write_int(8);
        b.pop_object(obj_io);

        // Port Info struct
        let info_frame = b.push_struct();
        let port_change_mask = SPA_PORT_CHANGE_MASK_FLAGS
            | SPA_PORT_CHANGE_MASK_RATE
            | SPA_PORT_CHANGE_MASK_PROPS
            | SPA_PORT_CHANGE_MASK_PARAMS;
        b.write_long(port_change_mask as i64);
        let port_flags = SPA_PORT_FLAG_PHYSICAL | SPA_PORT_FLAG_TERMINAL;
        b.write_long(port_flags as i64);
        b.write_int(0); // rate.num = 0
        b.write_int(1); // rate.denom = 1
        let port_alias = format!("{}:{}", self.node_description, name);
        b.write_dict_items(&[
            ("port.name", name),
            ("port.direction", "in"),
            ("port.alias", &port_alias),
            ("audio.channel", channel),
            ("format.dsp", "32 bit float mono audio"),
            ("port.physical", "true"),
            ("port.terminal", "true"),
        ]);
        b.write_int(4); // n_params in info
        b.write_id(SPA_PARAM_ENUM_FORMAT);
        b.write_int(SPA_PARAM_INFO_READ as i32);
        b.write_id(SPA_PARAM_FORMAT);
        b.write_int(if configured {
            SPA_PARAM_INFO_READWRITE
        } else {
            SPA_PARAM_INFO_WRITE
        } as i32);
        b.write_id(SPA_PARAM_BUFFERS);
        b.write_int(SPA_PARAM_INFO_READ as i32);
        b.write_id(SPA_PARAM_IO);
        b.write_int(SPA_PARAM_INFO_READ as i32);
        b.pop_struct(info_frame);

        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            self.proxy_id,
            PW_CLIENT_NODE_METHOD_PORT_UPDATE,
            seq,
            &[],
            b.as_bytes(),
        )?;
        debug!(
            "ClientNode: sent PortUpdate for port {} ({})",
            port_id, name
        );
        Ok(seq)
    }

    /// Send ClientNode.SetActive(true) to activate the node.
    pub fn send_set_active(
        &self,
        conn: &mut Connection,
        active: bool,
    ) -> Result<u32, ConnectionError> {
        let mut b = PodBuilder::new();
        let frame = b.push_struct();
        b.write_bool(active);
        b.pop_struct(frame);

        let seq = conn.next_seq();
        conn.send_message(
            self.proxy_id,
            PW_CLIENT_NODE_METHOD_SET_ACTIVE,
            seq,
            &[],
            b.as_bytes(),
        )?;
        debug!("ClientNode: sent SetActive({})", active);
        Ok(seq)
    }

    /// Handle incoming message directed at ClientNode.
    pub fn handle_message(
        &mut self,
        conn: &mut Connection,
        mem_table: &mut MemTable,
        msg: &Message,
    ) -> Result<Option<ClientNodeEvent>, ClientNodeError> {
        if msg.id != self.proxy_id {
            return Ok(None);
        }

        match msg.opcode {
            PW_CLIENT_NODE_EVENT_TRANSPORT => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    // SPA_POD_Fd values are indices into the message's fd array.
                    if let (true, Some(readfd), Some(writefd)) = (
                        items.len() >= 5,
                        msg_fd(msg, &items[0]),
                        msg_fd(msg, &items[1]),
                    ) {
                        let mem_id = items[2].as_u32().unwrap_or(0);
                        let offset = items[3].as_u32().unwrap_or(0);
                        let size = items[4].as_u32().unwrap_or(0);

                        debug!(
                            "ClientNode Transport: readfd={}, writefd={}, mem_id={}, offset={}, size={}",
                            readfd, writefd, mem_id, offset, size
                        );

                        let map_ptr = mem_table.mmap_block(mem_id, (offset + size) as usize)?;
                        let activation_ptr =
                            unsafe { map_ptr.add(offset as usize) as *mut PwNodeActivation };

                        // Set client version on activation
                        unsafe {
                            (*activation_ptr).client_version = PW_VERSION_NODE_ACTIVATION;
                        }

                        self.transport = Some(TransportInfo {
                            readfd,
                            writefd,
                            activation_ptr,
                        });

                        return Ok(Some(ClientNodeEvent::TransportReady));
                    }
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_SET_PARAM => {
                // Struct { Id id, Int flags, Pod param }
                let mut parser = PodParser::new(&msg.body);
                let (id, param) = match parser.next()? {
                    PodValue::Struct(mut items) if items.len() >= 3 => {
                        let param = items.swap_remove(2);
                        (items[0].as_u32().unwrap_or(0), param)
                    }
                    _ => return Ok(None),
                };
                debug!("ClientNode: SetParam id={id}");
                if id == SPA_PARAM_PROPS
                    && let PodValue::Object { props, .. } = &param
                {
                    self.apply_props(props);
                }
                if id == SPA_PARAM_PROPS || id == SPA_PARAM_PORT_CONFIG {
                    // Our ports are fixed DSP ports; acknowledge by re-announcing Props.
                    self.props_serial = !self.props_serial;
                    self.send_node_update(conn)?;
                }
                if id == SPA_PARAM_PORT_CONFIG {
                    // WirePlumber suspends the node to set PortConfig; the graph recalc done
                    // during that suspend runs before it completes, so an always-process node
                    // without links would stay suspended until some unrelated graph change.
                    // Toggling active forces a fresh recalc ("node deactivate/activate").
                    self.send_set_active(conn, false)?;
                    self.send_set_active(conn, true)?;
                }
                if id == SPA_PARAM_PROPS {
                    return Ok(Some(ClientNodeEvent::PropsChanged {
                        volumes: self.volumes,
                        mute: self.mute,
                    }));
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_SET_IO => {
                // Struct { Id id, Int memid, Int offset, Int size }
                let mut parser = PodParser::new(&msg.body);
                let items = match parser.next()? {
                    PodValue::Struct(items) if items.len() >= 4 => items,
                    _ => return Ok(None),
                };
                let id = items[0].as_u32().unwrap_or(0);
                let mem_id = items[1]
                    .as_i32()
                    .map(|v| v as u32)
                    .unwrap_or(SPA_ID_INVALID);
                let offset = items[2].as_u32().unwrap_or(0);
                let size = items[3].as_u32().unwrap_or(0);
                debug!("ClientNode: SetIo id={id} mem={mem_id} offset={offset} size={size}");
                if id == SPA_IO_POSITION {
                    let pos = if mem_id == SPA_ID_INVALID
                        || (size as usize) < std::mem::size_of::<SpaIoPosition>()
                    {
                        std::ptr::null_mut()
                    } else {
                        let map = mem_table.mmap_block(mem_id, (offset + size) as usize)?;
                        unsafe { map.add(offset as usize) as *mut SpaIoPosition }
                    };
                    self.rt_data.position.store(pos, Ordering::Release);
                    // Tell the driver we follow it: it skips targets whose
                    // active_driver_id differs from its id (impl-node.c
                    // pw_impl_node_set_io / node_ready).
                    if !pos.is_null()
                        && let Some(t) = &self.transport
                    {
                        unsafe {
                            (*t.activation_ptr).active_driver_id = (*pos).clock.id;
                        }
                    }
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_PORT_SET_PARAM => {
                // Struct { Int direction, Int port_id, Id id, Int flags, Pod param }
                let mut parser = PodParser::new(&msg.body);
                let PodValue::Struct(mut items) = parser.next()? else {
                    return Ok(None);
                };
                if items.len() < 5 {
                    return Ok(None);
                }
                let param = items.swap_remove(4);
                let port_id = items[1].as_u32().unwrap_or(u32::MAX);
                let id = items[2].as_u32().unwrap_or(0);
                debug!("ClientNode: PortSetParam port={port_id} id={id}");
                if id == SPA_PARAM_FORMAT && (port_id as usize) < self.port_configured.len() {
                    // A None pod clears the format.
                    self.port_configured[port_id as usize] =
                        matches!(param, PodValue::Object { .. });
                    self.send_port_update(conn, port_id)?;
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_PORT_USE_BUFFERS => {
                self.handle_port_use_buffers(mem_table, &msg.body)?;
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_PORT_SET_IO => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 7 {
                        let port_id = items[1].as_u32().unwrap_or(0);
                        let mix_id = items[2].as_u32().unwrap_or(SPA_ID_INVALID);
                        let id = items[3].as_u32().unwrap_or(0);
                        let mem_id = items[4]
                            .as_i32()
                            .map(|v| v as u32)
                            .unwrap_or(SPA_ID_INVALID);
                        let offset = items[5].as_u32().unwrap_or(0);
                        let size = items[6].as_u32().unwrap_or(0);

                        let port = self.rt_data.ports.get(port_id as usize);
                        if id == SPA_IO_BUFFERS
                            && let Some(port) = port
                        {
                            if mem_id != SPA_ID_INVALID && size >= 8 {
                                let Some(mix) = port.mix(mix_id, true) else {
                                    warn!("ClientNode: port {port_id}: too many links");
                                    return Ok(None);
                                };
                                let map_ptr =
                                    mem_table.mmap_block(mem_id, (offset + size) as usize)?;
                                let io_ptr =
                                    unsafe { map_ptr.add(offset as usize) as *mut SpaIoBuffers };
                                unsafe {
                                    (*io_ptr).status = SPA_STATUS_NEED_DATA;
                                    (*io_ptr).buffer_id = SPA_ID_INVALID;
                                }
                                mix.io.store(io_ptr, Ordering::Release);
                                debug!(
                                    "ClientNode: port {port_id} mix {mix_id} IO Buffers set at {io_ptr:?}"
                                );
                            } else if let Some(mix) = port.mix(mix_id, false) {
                                mix.io.store(std::ptr::null_mut(), Ordering::Release);
                            }
                        }
                    }
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_SET_ACTIVATION => {
                let mut parser = PodParser::new(&msg.body);
                let val = parser.next()?;
                if let PodValue::Struct(items) = val {
                    if items.len() >= 5 {
                        let node_id = items[0].as_u32().unwrap_or(0);
                        let mem_id = items[2]
                            .as_i32()
                            .map(|v| v as u32)
                            .unwrap_or(SPA_ID_INVALID);
                        let offset = items[3].as_u32().unwrap_or(0);
                        let size = items[4].as_u32().unwrap_or(0);

                        if let (true, Some(signalfd)) =
                            (mem_id != SPA_ID_INVALID, msg_fd(msg, &items[1]))
                        {
                            let map_ptr = mem_table.mmap_block(mem_id, (offset + size) as usize)?;
                            let act_ptr =
                                unsafe { map_ptr.add(offset as usize) as *mut PwNodeActivation };

                            self.add_peer_target(node_id, signalfd, act_ptr);
                            debug!(
                                "ClientNode: SetActivation node_id={} (signalfd={})",
                                node_id, signalfd
                            );
                        } else {
                            self.remove_peer_target(node_id);
                            debug!("ClientNode: SetActivation remove node_id={}", node_id);
                        }
                    }
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_PORT_SET_MIX_INFO => {
                // Struct { Int direction, Int port_id, Int mix_id, Int peer_id, Struct props }
                let mut parser = PodParser::new(&msg.body);
                let PodValue::Struct(items) = parser.next()? else {
                    return Ok(None);
                };
                if items.len() < 4 {
                    return Ok(None);
                }
                let port_id = items[1].as_u32().unwrap_or(u32::MAX);
                let mix_id = items[2].as_u32().unwrap_or(SPA_ID_INVALID);
                let peer_id = items[3].as_u32().unwrap_or(SPA_ID_INVALID);
                debug!("ClientNode: PortSetMixInfo port={port_id} mix={mix_id} peer={peer_id}");
                if let Some(port) = self.rt_data.ports.get(port_id as usize) {
                    if peer_id == SPA_ID_INVALID {
                        port.release_mix(mix_id);
                    } else if port.mix(mix_id, true).is_none() {
                        warn!("ClientNode: port {port_id}: too many links");
                    }
                }
                Ok(None)
            }
            PW_CLIENT_NODE_EVENT_COMMAND => {
                // Struct { Pod command }; the command object's id is the spa_node_command.
                let mut parser = PodParser::new(&msg.body);
                let cmd = match parser.next()? {
                    PodValue::Struct(items) => match items.first() {
                        Some(PodValue::Object { id, .. }) => *id,
                        _ => return Ok(None),
                    },
                    _ => return Ok(None),
                };
                debug!("ClientNode: Command id={cmd}");
                // With client_version >= 1 the server leaves our activation status alone on
                // Start (impl-node.c do_node_prepare) and marks it INACTIVE on stop, so the
                // client must mark itself FINISHED to be scheduled — as the client-side
                // pw_impl_node does in do_node_prepare. Without this the driver never
                // triggers us.
                if let Some(t) = &self.transport {
                    let status = unsafe { &(*t.activation_ptr).status };
                    match cmd {
                        SPA_NODE_COMMAND_START => {
                            drain_eventfd(t.readfd);
                            status.store(PW_NODE_ACTIVATION_FINISHED, Ordering::Release);
                        }
                        SPA_NODE_COMMAND_SUSPEND | SPA_NODE_COMMAND_PAUSE => {
                            status.store(PW_NODE_ACTIVATION_INACTIVE, Ordering::Release);
                        }
                        _ => {}
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn add_peer_target(&self, node_id: u32, signalfd: RawFd, act_ptr: *mut PwNodeActivation) {
        let n = self.rt_data.n_targets.load(Ordering::Acquire) as usize;
        // Check if target already exists
        for i in 0..n.min(MAX_PEER_TARGETS) {
            let t = &self.rt_data.targets[i];
            if t.node_id.load(Ordering::Relaxed) == node_id {
                t.fd.store(signalfd, Ordering::Release);
                t.activation.store(act_ptr, Ordering::Release);
                t.active.store(true, Ordering::Release);
                return;
            }
        }
        if n < MAX_PEER_TARGETS {
            let target = &self.rt_data.targets[n];
            target.node_id.store(node_id, Ordering::Relaxed);
            target.fd.store(signalfd, Ordering::Relaxed);
            target.activation.store(act_ptr, Ordering::Relaxed);
            target.active.store(true, Ordering::Release);
            self.rt_data.n_targets.fetch_add(1, Ordering::Release);
        }
    }

    fn remove_peer_target(&self, node_id: u32) {
        let n = self.rt_data.n_targets.load(Ordering::Acquire) as usize;
        for i in 0..n.min(MAX_PEER_TARGETS) {
            let t = &self.rt_data.targets[i];
            if t.node_id.load(Ordering::Relaxed) == node_id {
                t.active.store(false, Ordering::Release);
            }
        }
    }

    fn handle_port_use_buffers(
        &mut self,
        mem_table: &mut MemTable,
        body: &[u8],
    ) -> Result<(), ClientNodeError> {
        let mut parser = PodParser::new(body);
        let val = parser.next()?;
        let items = match val {
            PodValue::Struct(items) => items,
            _ => return Ok(()),
        };

        if items.len() < 5 {
            return Ok(());
        }

        let port_id = items[1].as_u32().unwrap_or(0);
        let mix_id = items[2].as_u32().unwrap_or(SPA_ID_INVALID);
        let n_buffers = items[4].as_u32().unwrap_or(0) as usize;

        let Some(port) = self.rt_data.ports.get(port_id as usize) else {
            return Ok(());
        };
        let Some(mix) = port.mix(mix_id, n_buffers > 0) else {
            if n_buffers > 0 {
                warn!("ClientNode: port {port_id}: too many links");
            }
            return Ok(());
        };
        // Old buffers go away with this call: stop the RT thread using them first.
        mix.n_buffers.store(0, Ordering::Release);

        debug!("ClientNode: port {port_id} mix {mix_id} use_buffers (n_buffers={n_buffers})");
        let mut item_idx = 5;

        for buf_idx in 0..n_buffers.min(MAX_BUFFERS_PER_PORT) {
            if item_idx + 4 > items.len() {
                break;
            }
            let mem_id = items[item_idx].as_u32().unwrap_or(0);
            let offset = items[item_idx + 1].as_u32().unwrap_or(0);
            let size = items[item_idx + 2].as_u32().unwrap_or(0);
            let n_metas = items[item_idx + 3].as_u32().unwrap_or(0) as usize;
            item_idx += 4;

            let mut meta_bytes_total = 0;
            for _ in 0..n_metas {
                if item_idx + 2 > items.len() {
                    break;
                }
                let meta_size = items[item_idx + 1].as_u32().unwrap_or(0) as usize;
                meta_bytes_total += align8(meta_size);
                item_idx += 2;
            }

            if item_idx >= items.len() {
                break;
            }
            let n_datas = items[item_idx].as_u32().unwrap_or(0) as usize;
            item_idx += 1;

            let map_ptr = mem_table.mmap_block(mem_id, (offset + size) as usize)?;
            let buf_base = unsafe { map_ptr.add(offset as usize) };
            let chunk_ptr = unsafe { buf_base.add(meta_bytes_total) as *const SpaChunk };

            let mut data_ptr: *const f32 = std::ptr::null();
            for _ in 0..n_datas {
                if item_idx + 5 > items.len() {
                    break;
                }
                let data_type = items[item_idx].as_u32().unwrap_or(0);
                let data_id = items[item_idx + 1].as_u32().unwrap_or(0);
                let mapoffset = items[item_idx + 3].as_u32().unwrap_or(0);
                let maxsize = items[item_idx + 4].as_u32().unwrap_or(0);
                item_idx += 5;

                debug!(
                    "Port {} buffer {} data: type={}, data_id={}, mapoffset={}, maxsize={}",
                    port_id, buf_idx, data_type, data_id, mapoffset, maxsize
                );

                if data_type == SPA_DATA_MEM_PTR {
                    data_ptr = unsafe { buf_base.add(data_id as usize) as *const f32 };
                } else if data_type == SPA_DATA_MEM_FD || data_type == SPA_DATA_MEM_ID {
                    let d_map = mem_table.mmap_block(data_id, (mapoffset + maxsize) as usize)?;
                    data_ptr = unsafe { d_map.add(mapoffset as usize) as *const f32 };
                }
            }

            let buf_ref = &mix.buffers[buf_idx];
            buf_ref.chunk.store(chunk_ptr as *mut _, Ordering::Release);
            buf_ref.data.store(data_ptr as *mut _, Ordering::Release);
        }

        mix.n_buffers.store(
            n_buffers.min(MAX_BUFFERS_PER_PORT) as u32,
            Ordering::Release,
        );

        Ok(())
    }
}

/// Resolve a `SPA_POD_Fd` value (an index into the message's fd array) to the fd.
fn msg_fd(msg: &Message, v: &PodValue) -> Option<RawFd> {
    match v {
        PodValue::Fd(idx) if *idx >= 0 => msg.fds.get(*idx as usize).copied(),
        _ => None,
    }
}

/// Clear a stale wakeup left on the eventfd while the node was stopped.
fn drain_eventfd(fd: RawFd) {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut pfd, 1, 0) } > 0 && (pfd.revents & libc::POLLIN) != 0 {
        let mut v: u64 = 0;
        let _ = unsafe { libc::read(fd, &mut v as *mut u64 as *mut libc::c_void, 8) };
    }
}

#[derive(Debug)]
pub enum ClientNodeEvent {
    TransportReady,
    PropsChanged { volumes: [f32; 2], mute: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_props_pod_encode_decode_round_trip() {
        let mut client_node = ClientNodeProxy::new(4, "test_node", "Test Node");
        client_node.set_props([0.064, 0.064], false);

        let mut builder = PodBuilder::new();
        client_node.write_props(&mut builder);
        let bytes = builder.as_bytes();

        let mut parser = PodParser::new(bytes);
        let val = parser.next().expect("parse props pod");
        let props = match val {
            PodValue::Object { props, .. } => props,
            other => panic!("expected PodValue::Object, got {:?}", other),
        };

        let mut decoded_node = ClientNodeProxy::new(4, "test_node", "Test Node");
        decoded_node.apply_props(&props);

        assert!(!decoded_node.mute);
        assert!((decoded_node.volumes[0] - 0.064).abs() < 1e-6);
        assert!((decoded_node.volumes[1] - 0.064).abs() < 1e-6);

        // Test with mute = true and asymmetrical channel volumes
        client_node.set_props([0.125, 0.512], true);
        let mut builder2 = PodBuilder::new();
        client_node.write_props(&mut builder2);
        let mut parser2 = PodParser::new(builder2.as_bytes());
        let val2 = parser2.next().expect("parse props pod 2");
        let props2 = match val2 {
            PodValue::Object { props, .. } => props,
            other => panic!("expected PodValue::Object, got {:?}", other),
        };
        decoded_node.apply_props(&props2);
        assert!(decoded_node.mute);
        assert!((decoded_node.volumes[0] - 0.125).abs() < 1e-6);
        assert!((decoded_node.volumes[1] - 0.512).abs() < 1e-6);
    }
}
