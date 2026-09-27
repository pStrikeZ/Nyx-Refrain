//! Pure-Rust PipeWire native protocol client and virtual audio sink.
//!
//! Connects to the PipeWire daemon via Unix domain socket using native protocol V3,
//! registers a virtual `Audio/Sink` client node, sets/restores default audio sink via
//! WirePlumber metadata, and captures audio frames in real time with zero allocations
//! on the audio processing path.
//!
//! Built for static musl compilation without libpipewire or C dependencies.

#![cfg(target_os = "linux")]
#![allow(clippy::collapsible_if)]

pub mod activation;
pub mod client_node;
pub mod connection;
pub mod core;
pub mod metadata;
pub mod pod;
pub mod registry;
pub mod rt;
pub mod volume;

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use client_node::ClientNodeProxy;
use connection::{Connection, ConnectionError};
use core::{CoreEvent, CoreProxy, PW_CORE_PROXY_ID};
use metadata::MetadataProxy;
use registry::RegistryProxy;
pub use rt::CycleMeta;
pub use volume::{SinkVolume, SinkVolumeControl, cubic_pct_to_linear, linear_to_cubic_pct};

#[derive(Error, Debug)]
pub enum SinkError {
    #[error("Connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("Core error: {0}")]
    Core(#[from] core::CoreError),
    #[error("Client node error: {0}")]
    ClientNode(#[from] client_node::ClientNodeError),
    #[error("Registry error: {0}")]
    Registry(#[from] registry::RegistryError),
    #[error("Metadata error: {0}")]
    Metadata(#[from] metadata::MetadataError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Handshake timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("Disconnected from PipeWire server")]
    Disconnected,
}

/// Configuration options for the PipeWire virtual sink.
#[derive(Debug, Clone)]
pub struct SinkOptions {
    /// Node name in PipeWire graph. Default: `"nyx_refrain_airplay"`.
    pub node_name: String,
    /// Node description shown in audio settings. Default: `"Nyx Refrain (AirPlay)"`.
    pub node_description: String,
    /// Whether to configure this sink as the default audio output in metadata.
    pub set_default: bool,
    /// Optional initial volume. Defaults to 100% unmute.
    pub initial_volume: Option<SinkVolume>,
    /// Optional existing volume control handle to reuse across reconnects.
    pub volume_control: Option<SinkVolumeControl>,
}

impl Default for SinkOptions {
    fn default() -> Self {
        Self {
            node_name: "nyx_refrain_airplay".to_string(),
            node_description: "Nyx Refrain (AirPlay)".to_string(),
            set_default: true,
            initial_volume: None,
            volume_control: None,
        }
    }
}

/// A chunk of audio samples captured from the virtual sink.
#[derive(Debug, Clone)]
pub struct CapturedChunk {
    /// Interleaved 32-bit floating-point samples ([L0, R0, L1, R1, ...]).
    pub samples: Vec<f32>,
    /// Monotonic clock instant of the cycle in nanoseconds (`CLOCK_MONOTONIC`).
    pub clock_nsec: u64,
    /// Negotiated sampling rate in Hz (e.g. 48000).
    pub rate: u32,
    /// Number of audio channels (always 2 for stereo FL/FR).
    pub channels: u16,
    /// Number of audio frames still queued in the ring buffer after this chunk.
    pub queued_frames: usize,
}

/// Consumer interface for reading captured audio frames from the virtual sink.
pub struct SinkConsumer {
    sample_consumer: rtrb::Consumer<f32>,
    meta_consumer: rtrb::Consumer<CycleMeta>,
    disconnected: Arc<AtomicBool>,
    current_meta: Option<CycleMeta>,
    last_rate: u32,
}

impl SinkConsumer {
    /// Whether the PipeWire connection or sink node has been disconnected.
    pub fn is_disconnected(&self) -> bool {
        self.disconnected.load(Ordering::Acquire)
    }

    /// Current negotiated sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.last_rate
    }

    /// Number of channels (2).
    pub fn channels(&self) -> u16 {
        2
    }

    /// Non-blocking pop of up to `max_frames` interleaved f32 frames.
    ///
    /// Returns `None` if no audio frames are currently available.
    pub fn pop_chunk(&mut self, max_frames: usize) -> Option<CapturedChunk> {
        // The RT thread pushes a cycle's samples before its metadata; popping samples whose
        // metadata has not arrived yet would give them the wrong clock, so wait for it.
        if self.current_meta.is_none() {
            self.current_meta = Some(self.meta_consumer.pop().ok()?);
        }
        let avail_samples = self.sample_consumer.slots();
        let avail_frames = avail_samples / 2;
        if avail_frames == 0 {
            return None;
        }

        let frames_to_pop = avail_frames.min(max_frames);
        let samples_to_pop = frames_to_pop * 2;
        let mut samples = Vec::with_capacity(samples_to_pop);
        for _ in 0..samples_to_pop {
            if let Ok(s) = self.sample_consumer.pop() {
                samples.push(s);
            } else {
                break;
            }
        }

        if samples.is_empty() {
            return None;
        }

        // Timestamp of the chunk's first frame: the clock of the oldest cycle still being
        // consumed, advanced by the frames already taken from it. Then walk the per-cycle
        // metadata forward by the popped frame count (a chunk may span several cycles).
        if self.current_meta.is_none() {
            self.current_meta = self.meta_consumer.pop().ok();
        }
        let (clock_nsec, rate) = match self.current_meta {
            Some(meta) => (meta.clock_nsec, meta.rate),
            None => {
                let mut ts = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
                let now_ns = (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64);
                (now_ns, self.last_rate)
            }
        };
        let mut left = (samples.len() / 2) as u32;
        while left > 0 {
            let Some(meta) = self.current_meta.as_mut() else {
                break;
            };
            self.last_rate = meta.rate;
            let take = left.min(meta.n_frames);
            left -= take;
            meta.n_frames -= take;
            meta.clock_nsec += take as u64 * 1_000_000_000 / meta.rate.max(1) as u64;
            if meta.n_frames == 0 {
                self.current_meta = self.meta_consumer.pop().ok();
            }
        }

        let queued_frames = self.sample_consumer.slots() / 2;

        Some(CapturedChunk {
            samples,
            clock_nsec,
            rate,
            channels: 2,
            queued_frames,
        })
    }
}

/// Handle to the active virtual sink.
///
/// Dropping this struct restores original default sink configuration in metadata,
/// signals the realtime thread and main loop thread to stop, and closes the connection.
pub struct VirtualSink {
    shutdown_fd: RawFd,
    command_fd: RawFd,
    volume_control: SinkVolumeControl,
    main_thread: Option<std::thread::JoinHandle<()>>,
    data_thread: Option<std::thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    readfd: RawFd,
    writefd: RawFd,
}

impl VirtualSink {
    /// Return the volume control handle for this sink.
    pub fn volume_control(&self) -> SinkVolumeControl {
        self.volume_control.clone()
    }

    /// Create and register a virtual audio sink node with PipeWire.
    pub fn create(opts: SinkOptions) -> Result<(Self, SinkConsumer), SinkError> {
        info!(
            "Creating PipeWire virtual sink: name='{}', desc='{}', set_default={}",
            opts.node_name, opts.node_description, opts.set_default
        );

        let mut conn = Connection::connect(None)?;
        let mut core = CoreProxy::new();

        // 1. Hello
        CoreProxy::send_hello(&mut conn)?;

        // 2. Sync to get client id and core info
        CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 1)?;
        let mut got_sync_1 = false;
        while !got_sync_1 {
            let msgs = conn.read_messages()?;
            for msg in msgs {
                if msg.id == PW_CORE_PROXY_ID {
                    if let Some(CoreEvent::Done { seq, .. }) =
                        core.handle_core_message(&mut conn, &msg)?
                    {
                        if seq == 1 {
                            got_sync_1 = true;
                        }
                    }
                }
            }
        }

        // 3. Set client properties
        CoreProxy::send_update_client_properties(
            &mut conn,
            core.client_id,
            &[
                ("application.name", "Nyx Refrain"),
                ("application.process.binary", "nyx-refrain"),
            ],
        )?;

        // 4. Registry
        let registry_id = 2;
        let mut registry = RegistryProxy::new(registry_id);
        CoreProxy::send_get_registry(&mut conn, registry_id)?;
        CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 2)?;
        let mut got_sync_2 = false;
        while !got_sync_2 {
            let msgs = conn.read_messages()?;
            for msg in msgs {
                if msg.id == registry_id {
                    registry.handle_message(&msg)?;
                } else if msg.id == PW_CORE_PROXY_ID {
                    if let Some(CoreEvent::Done { seq, .. }) =
                        core.handle_core_message(&mut conn, &msg)?
                    {
                        if seq == 2 {
                            got_sync_2 = true;
                        }
                    }
                }
            }
        }

        // 5. Metadata proxy (find global of type Metadata with metadata.name="default")
        let mut metadata_proxy: Option<MetadataProxy> = None;
        let metadata_global = registry.globals.values().find(|g| {
            g.type_ == metadata::PW_TYPE_INTERFACE_METADATA
                && g.props.get("metadata.name").map(|s| s.as_str()) == Some("default")
        });

        if let Some(g) = metadata_global {
            let metadata_id = 3;
            registry.bind(
                &mut conn,
                g.id,
                metadata::PW_TYPE_INTERFACE_METADATA,
                3,
                metadata_id,
            )?;
            let mut meta = MetadataProxy::new(metadata_id);
            CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 3)?;
            let mut got_sync_3 = false;
            while !got_sync_3 {
                let msgs = conn.read_messages()?;
                for msg in msgs {
                    if msg.id == metadata_id {
                        meta.handle_message(&msg)?;
                    } else if msg.id == PW_CORE_PROXY_ID {
                        if let Some(CoreEvent::Done { seq, .. }) =
                            core.handle_core_message(&mut conn, &msg)?
                        {
                            if seq == 3 {
                                got_sync_3 = true;
                            }
                        }
                    } else if msg.id == registry_id {
                        registry.handle_message(&msg)?;
                    }
                }
            }

            if opts.set_default {
                meta.set_default_sink(&mut conn, &opts.node_name)?;
                CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 4)?;
                let mut got_sync_4 = false;
                while !got_sync_4 {
                    let msgs = conn.read_messages()?;
                    for msg in msgs {
                        if msg.id == metadata_id {
                            meta.handle_message(&msg)?;
                        } else if msg.id == PW_CORE_PROXY_ID {
                            if let Some(CoreEvent::Done { seq, .. }) =
                                core.handle_core_message(&mut conn, &msg)?
                            {
                                if seq == 4 {
                                    got_sync_4 = true;
                                }
                            }
                        }
                    }
                }
            }
            metadata_proxy = Some(meta);
        } else {
            warn!(
                "PipeWire 'default' metadata global not found; default sink configuration disabled"
            );
        }

        // 6. Create client-node object
        let client_node_proxy_id = 4;
        let props = [
            ("media.class", "Audio/Sink"),
            ("node.name", opts.node_name.as_str()),
            ("node.description", opts.node_description.as_str()),
            ("media.name", opts.node_description.as_str()),
            ("node.always-process", "true"),
            ("node.want-driver", "true"),
            ("audio.channels", "2"),
            ("audio.position", "[ FL FR ]"),
            ("object.register", "true"),
            ("node.virtual", "false"),
            // Volume is owned by the app; see ClientNodeProxy::send_node_update.
            ("state.restore-props", "false"),
        ];

        CoreProxy::send_create_object(
            &mut conn,
            "client-node",
            "PipeWire:Interface:ClientNode",
            6, // PW_VERSION_CLIENT_NODE
            &props,
            client_node_proxy_id,
        )?;

        let initial_vol = opts.initial_volume.unwrap_or_default();
        let volume_control = opts
            .volume_control
            .unwrap_or_else(|| SinkVolumeControl::new(initial_vol));

        // 7. Initialize client node and ports
        let mut client_node = ClientNodeProxy::new(
            client_node_proxy_id,
            &opts.node_name,
            &opts.node_description,
        );
        let init_linear = cubic_pct_to_linear(initial_vol.pct);
        client_node.set_props([init_linear, init_linear], initial_vol.mute);
        client_node.send_node_update(&mut conn)?;
        client_node.send_port_update(&mut conn, 0)?; // playback_FL
        client_node.send_port_update(&mut conn, 1)?; // playback_FR
        client_node.send_set_active(&mut conn, true)?;

        // 8. Wait for Transport event
        CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 5)?;
        let mut retries = 0;
        while client_node.transport.is_none() && retries < 50 {
            retries += 1;
            let msgs = conn.read_messages_timeout(200)?;
            for msg in msgs {
                if msg.id == PW_CORE_PROXY_ID {
                    let _ = core.handle_core_message(&mut conn, &msg);
                } else if msg.id == client_node_proxy_id {
                    let _ = client_node.handle_message(&mut conn, &mut core.mem_table, &msg);
                } else if let Some(ref mut meta) = metadata_proxy {
                    if msg.id == meta.proxy_id {
                        let _ = meta.handle_message(&msg);
                    }
                }
            }
        }

        let transport = client_node
            .transport
            .as_ref()
            .ok_or(SinkError::Timeout("ClientNode Transport"))?;

        let data_readfd = transport.readfd;
        let data_writefd = transport.writefd;
        let activation_ptr = transport.activation_ptr;

        // 9. Allocate ring buffers: 96,000 samples (1 s stereo @ 48kHz)
        let (sample_producer, sample_consumer) = rtrb::RingBuffer::new(96000);
        let (meta_producer, meta_consumer) = rtrb::RingBuffer::new(1024);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let disconnected = Arc::new(AtomicBool::new(false));

        let shutdown_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if shutdown_fd < 0 {
            return Err(SinkError::Io(std::io::Error::last_os_error()));
        }

        let command_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if command_fd < 0 {
            unsafe { libc::close(shutdown_fd) };
            return Err(SinkError::Io(std::io::Error::last_os_error()));
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        volume_control.attach(cmd_tx, command_fd);

        // 10. Spawn realtime audio data thread
        let rt_data = client_node.rt_data.clone();
        let stop_flag_rt = stop_flag.clone();
        let act_usize = activation_ptr as usize;
        let data_thread = std::thread::Builder::new()
            .name("pwnative-rt".to_string())
            .spawn(move || {
                let act_ptr = act_usize as *mut activation::PwNodeActivation;
                unsafe {
                    rt::run_data_thread(
                        data_readfd,
                        act_ptr,
                        rt_data,
                        sample_producer,
                        meta_producer,
                        stop_flag_rt,
                    );
                }
            })?;

        // 11. Spawn main loop thread
        let stop_flag_main = stop_flag.clone();
        let disconnected_main = disconnected.clone();
        let volume_control_main = volume_control.clone();
        let main_thread = std::thread::Builder::new()
            .name("pwnative-main".to_string())
            .spawn(move || {
                run_main_loop(
                    conn,
                    core,
                    client_node,
                    metadata_proxy,
                    shutdown_fd,
                    command_fd,
                    cmd_rx,
                    volume_control_main,
                    disconnected_main,
                    stop_flag_main,
                );
            })?;

        let consumer = SinkConsumer {
            sample_consumer,
            meta_consumer,
            disconnected,
            current_meta: None,
            last_rate: 48000,
        };

        let sink = Self {
            shutdown_fd,
            command_fd,
            volume_control,
            main_thread: Some(main_thread),
            data_thread: Some(data_thread),
            stop_flag,
            readfd: data_readfd,
            writefd: data_writefd,
        };

        info!(
            "PipeWire virtual sink '{}' created and active",
            opts.node_name
        );
        Ok((sink, consumer))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_main_loop(
    mut conn: Connection,
    mut core: CoreProxy,
    mut client_node: ClientNodeProxy,
    mut metadata: Option<MetadataProxy>,
    shutdown_fd: RawFd,
    command_fd: RawFd,
    cmd_rx: std::sync::mpsc::Receiver<volume::SinkCommand>,
    volume_control: SinkVolumeControl,
    disconnected: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
) {
    let conn_fd = conn.raw_fd();
    let mut pfds = [
        libc::pollfd {
            fd: conn_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: command_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    while !stop_flag.load(Ordering::Relaxed) {
        pfds[0].revents = 0;
        pfds[1].revents = 0;
        pfds[2].revents = 0;

        let ret = unsafe { libc::poll(pfds.as_mut_ptr(), 3, 200) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            error!("pwnative-main: poll error: {}", err);
            disconnected.store(true, Ordering::Release);
            break;
        }

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Check shutdown eventfd
        if (pfds[1].revents & libc::POLLIN) != 0 {
            debug!("pwnative-main: shutdown signal received");
            break;
        }

        // Check command eventfd
        if (pfds[2].revents & libc::POLLIN) != 0 {
            let mut counter: u64 = 0;
            unsafe {
                libc::read(
                    command_fd,
                    &mut counter as *mut u64 as *mut libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    volume::SinkCommand::SetVolume { pct, mute } => {
                        let linear = cubic_pct_to_linear(pct);
                        client_node.set_props([linear, linear], mute);
                        if let Err(e) = client_node.send_node_update(&mut conn) {
                            error!("pwnative-main: send_node_update on SetVolume failed: {e}");
                        }
                    }
                }
            }
        }

        // Check PipeWire socket
        if (pfds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
            match conn.read_messages_timeout(0) {
                Ok(msgs) => {
                    for msg in msgs {
                        if msg.id == PW_CORE_PROXY_ID {
                            if let Err(e) = core.handle_core_message(&mut conn, &msg) {
                                error!("pwnative-main: core error: {}", e);
                                disconnected.store(true, Ordering::Release);
                                break;
                            }
                        } else if msg.id == client_node.proxy_id {
                            match client_node.handle_message(&mut conn, &mut core.mem_table, &msg) {
                                Ok(Some(client_node::ClientNodeEvent::PropsChanged {
                                    volumes,
                                    mute,
                                })) => {
                                    let linear = (volumes[0] + volumes[1]) * 0.5;
                                    let pct = linear_to_cubic_pct(linear);
                                    volume_control.notify_from_pipewire(SinkVolume { pct, mute });
                                }
                                Ok(Some(client_node::ClientNodeEvent::TransportReady)) => {}
                                Ok(None) => {}
                                Err(e) => {
                                    error!("pwnative-main: client_node error: {}", e);
                                }
                            }
                        } else if let Some(ref mut meta) = metadata {
                            if msg.id == meta.proxy_id {
                                if let Err(e) = meta.handle_message(&msg) {
                                    error!("pwnative-main: metadata error: {}", e);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("pwnative-main: connection closed or error: {}", e);
                    disconnected.store(true, Ordering::Release);
                    break;
                }
            }
        }
    }

    // On shutdown, restore default sink if configured
    if let Some(ref mut meta) = metadata {
        if meta.current_sink.is_some() {
            info!("pwnative-main: restoring original default audio sink");
            if meta.restore_default_sink(&mut conn).is_ok() {
                let _ = CoreProxy::send_sync(&mut conn, PW_CORE_PROXY_ID, 999);
                let start = std::time::Instant::now();
                while start.elapsed() < std::time::Duration::from_millis(200) {
                    if let Ok(msgs) = conn.read_messages_timeout(20) {
                        let mut done = false;
                        for msg in msgs {
                            if msg.id == PW_CORE_PROXY_ID {
                                if let Ok(Some(CoreEvent::Done { seq, .. })) =
                                    core.handle_core_message(&mut conn, &msg)
                                {
                                    if seq == 999 {
                                        done = true;
                                        break;
                                    }
                                }
                            }
                        }
                        if done {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }

    debug!("pwnative-main: thread finished");
}

impl Drop for VirtualSink {
    fn drop(&mut self) {
        debug!("VirtualSink: dropping, stopping threads");
        self.stop_flag.store(true, Ordering::Release);
        self.volume_control.detach();

        // Wake up rt thread and join it FIRST, before main thread unmaps memory
        if self.readfd >= 0 {
            let val: u64 = 1;
            unsafe {
                libc::write(
                    self.readfd,
                    &val as *const u64 as *const libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        }
        if let Some(h) = self.data_thread.take() {
            let _ = h.join();
        }

        // Wake up main loop thread and join it
        if self.shutdown_fd >= 0 {
            let val: u64 = 1;
            unsafe {
                libc::write(
                    self.shutdown_fd,
                    &val as *const u64 as *const libc::c_void,
                    std::mem::size_of::<u64>(),
                );
            }
        }
        if let Some(h) = self.main_thread.take() {
            let _ = h.join();
        }

        if self.shutdown_fd >= 0 {
            unsafe { libc::close(self.shutdown_fd) };
            self.shutdown_fd = -1;
        }
        if self.command_fd >= 0 {
            unsafe { libc::close(self.command_fd) };
            self.command_fd = -1;
        }
        if self.readfd >= 0 {
            unsafe { libc::close(self.readfd) };
            self.readfd = -1;
        }
        if self.writefd >= 0 {
            unsafe { libc::close(self.writefd) };
            self.writefd = -1;
        }
        debug!("VirtualSink: shutdown complete");
    }
}
