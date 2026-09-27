//! Realtime audio data processing thread.
//!
//! Polls on PipeWire's transport readfd (eventfd), sets activation AWAKE,
//! extracts F32 samples from FL and FR input ports, interleaves them into an
//! rtrb SPSC ring buffer, updates status to FINISHED, and triggers peer targets.
//!
//! Red line: Zero heap allocations on the data cycle path.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};
use tracing::debug;

use crate::activation::{
    PW_NODE_ACTIVATION_AWAKE, PW_NODE_ACTIVATION_FINISHED, PW_NODE_ACTIVATION_NOT_TRIGGERED,
    PW_NODE_ACTIVATION_TRIGGERED, PwNodeActivation, SPA_STATUS_NEED_DATA, SpaChunk, SpaIoBuffers,
    SpaIoPosition,
};

pub const MAX_BUFFERS_PER_PORT: usize = 64;
pub const MAX_PEER_TARGETS: usize = 64;
/// Upper bound for one synthesized silence quantum (PipeWire's max quantum is 8192).
const MAX_SILENCE_FRAMES: usize = 8192;

pub struct BufferRef {
    pub chunk: AtomicPtr<SpaChunk>,
    pub data: AtomicPtr<f32>,
}

impl BufferRef {
    pub const fn new() -> Self {
        Self {
            chunk: AtomicPtr::new(std::ptr::null_mut()),
            data: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

impl Default for BufferRef {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PortRef {
    pub io: AtomicPtr<SpaIoBuffers>,
    pub n_buffers: AtomicU32,
    pub buffers: [BufferRef; MAX_BUFFERS_PER_PORT],
}

impl Default for PortRef {
    fn default() -> Self {
        Self {
            io: AtomicPtr::new(std::ptr::null_mut()),
            n_buffers: AtomicU32::new(0),
            buffers: [const { BufferRef::new() }; MAX_BUFFERS_PER_PORT],
        }
    }
}

pub struct TargetRef {
    pub node_id: AtomicU32,
    pub fd: AtomicI32,
    pub activation: AtomicPtr<PwNodeActivation>,
    pub active: AtomicBool,
}

impl TargetRef {
    pub const fn new() -> Self {
        Self {
            node_id: AtomicU32::new(0),
            fd: AtomicI32::new(-1),
            activation: AtomicPtr::new(std::ptr::null_mut()),
            active: AtomicBool::new(false),
        }
    }
}

impl Default for TargetRef {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SharedRtData {
    pub ports: [PortRef; 2],
    pub n_targets: AtomicU32,
    pub targets: [TargetRef; MAX_PEER_TARGETS],
    /// The driver's `spa_io_position` from SetIo(Position); null until assigned.
    pub position: AtomicPtr<SpaIoPosition>,
}

unsafe impl Send for SharedRtData {}
unsafe impl Sync for SharedRtData {}

impl Default for SharedRtData {
    fn default() -> Self {
        Self {
            ports: [PortRef::default(), PortRef::default()],
            n_targets: AtomicU32::new(0),
            targets: [const { TargetRef::new() }; MAX_PEER_TARGETS],
            position: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CycleMeta {
    pub clock_nsec: u64,
    pub rate: u32,
    pub n_frames: u32,
}

/// Run the realtime audio processing data thread.
///
/// # Safety
///
/// `activation` must be a valid pointer to shared `PwNodeActivation` or null.
pub unsafe fn run_data_thread(
    readfd: RawFd,
    activation: *mut PwNodeActivation,
    rt_data: Arc<SharedRtData>,
    mut sample_producer: rtrb::Producer<f32>,
    mut meta_producer: rtrb::Producer<CycleMeta>,
    stop_flag: Arc<AtomicBool>,
) {
    debug!("Realtime data thread started on readfd={}", readfd);

    // The activation status is moved INACTIVE -> FINISHED by the Start command handler
    // (client_node.rs), not here: the node must not be scheduled before it is started.

    while !stop_flag.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd: readfd,
            events: libc::POLLIN,
            revents: 0,
        };

        // Poll with 100ms timeout to periodically check stop_flag
        let ret = unsafe { libc::poll(&mut pfd, 1, 100) };
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        if ret <= 0 {
            continue;
        }

        if (pfd.revents & libc::POLLIN) == 0 {
            continue;
        }

        // 1. Read eventfd counter
        let mut counter: u64 = 0;
        let r = unsafe {
            libc::read(
                readfd,
                &mut counter as *mut u64 as *mut libc::c_void,
                std::mem::size_of::<u64>(),
            )
        };
        if r < 8 {
            continue;
        }

        // 2. Monotonic clock time in nanoseconds
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        let now_ns = (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64);

        if activation.is_null() {
            continue;
        }

        // 3. Atomically update status: TRIGGERED -> AWAKE
        let act = unsafe { &*activation };
        let _ = act.status.compare_exchange(
            PW_NODE_ACTIVATION_TRIGGERED,
            PW_NODE_ACTIVATION_AWAKE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        unsafe {
            (*activation).awake_time = now_ns;
        }

        // Clock metadata
        // Followers read the driver's clock through the Position io area, like
        // pw_impl_node's rt.position; fall back to our own activation copy.
        let pos = rt_data.position.load(Ordering::Acquire);
        let clock = if pos.is_null() {
            unsafe { &(*activation).position.clock }
        } else {
            unsafe { &(*pos).clock }
        };
        let clock_nsec = clock.nsec;
        let rate_denom = clock.rate.denom;
        let rate = if rate_denom == 0 { 48000 } else { rate_denom };

        // 4. Read audio from input ports 0 (FL) and 1 (FR)
        let io0 = rt_data.ports[0].io.load(Ordering::Acquire);
        let io1 = rt_data.ports[1].io.load(Ordering::Acquire);

        let mut samples_pushed = 0;
        if !io0.is_null() && !io1.is_null() {
            let (_st0, id0) = unsafe { ((*io0).status, (*io0).buffer_id) };
            let (_st1, id1) = unsafe { ((*io1).status, (*io1).buffer_id) };

            let n_buf0 = rt_data.ports[0].n_buffers.load(Ordering::Acquire);
            let n_buf1 = rt_data.ports[1].n_buffers.load(Ordering::Acquire);

            if id0 < n_buf0
                && id1 < n_buf1
                && (id0 as usize) < MAX_BUFFERS_PER_PORT
                && (id1 as usize) < MAX_BUFFERS_PER_PORT
            {
                let buf0 = &rt_data.ports[0].buffers[id0 as usize];
                let buf1 = &rt_data.ports[1].buffers[id1 as usize];

                let chunk0_ptr = buf0.chunk.load(Ordering::Acquire);
                let chunk1_ptr = buf1.chunk.load(Ordering::Acquire);
                let data0_ptr = buf0.data.load(Ordering::Acquire);
                let data1_ptr = buf1.data.load(Ordering::Acquire);

                if !chunk0_ptr.is_null()
                    && !chunk1_ptr.is_null()
                    && !data0_ptr.is_null()
                    && !data1_ptr.is_null()
                {
                    let chunk0 = unsafe { &*chunk0_ptr };
                    let chunk1 = unsafe { &*chunk1_ptr };

                    let n_samples0 = (chunk0.size / 4) as usize;
                    let n_samples1 = (chunk1.size / 4) as usize;
                    let n_frames = n_samples0.min(n_samples1);

                    let offset0 = (chunk0.offset / 4) as usize;
                    let offset1 = (chunk1.offset / 4) as usize;

                    let fl_ptr = unsafe { data0_ptr.add(offset0) };
                    let fr_ptr = unsafe { data1_ptr.add(offset1) };

                    let available_slots = sample_producer.slots() / 2;
                    let frames_to_push = n_frames.min(available_slots);

                    for i in 0..frames_to_push {
                        let s_fl = unsafe { *fl_ptr.add(i) };
                        let s_fr = unsafe { *fr_ptr.add(i) };
                        let _ = sample_producer.push(s_fl);
                        let _ = sample_producer.push(s_fr);
                    }
                    samples_pushed = frames_to_push;
                }
            }

            // Return buffers back to host on every cycle
            unsafe {
                (*io0).status = SPA_STATUS_NEED_DATA;
                (*io1).status = SPA_STATUS_NEED_DATA;
            }
        }

        // No linked input (or no buffer this cycle): deliver one quantum of silence so the
        // capture stream keeps wall-clock pace like WASAPI loopback does while idle
        // (node.always-process keeps us scheduled).
        if samples_pushed == 0 {
            let quantum = (clock.duration as usize).min(MAX_SILENCE_FRAMES);
            let frames = quantum.min(sample_producer.slots() / 2);
            for _ in 0..frames * 2 {
                let _ = sample_producer.push(0.0);
            }
            samples_pushed = frames;
        }

        if samples_pushed > 0 {
            let _ = meta_producer.push(CycleMeta {
                clock_nsec,
                rate,
                n_frames: samples_pushed as u32,
            });
        }

        // 5. Complete cycle: AWAKE -> FINISHED
        let mut ts_fin = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts_fin) };
        let fin_ns = (ts_fin.tv_sec as u64) * 1_000_000_000 + (ts_fin.tv_nsec as u64);
        unsafe {
            (*activation).finish_time = fin_ns;
        }
        act.status
            .store(PW_NODE_ACTIVATION_FINISHED, Ordering::Release);

        // 6. Trigger peer targets (exact PipeWire trigger_target_v1 logic)
        let n_targets = rt_data.n_targets.load(Ordering::Acquire) as usize;
        for i in 0..n_targets.min(MAX_PEER_TARGETS) {
            let target = &rt_data.targets[i];
            if target.active.load(Ordering::Acquire) {
                let act_ptr = target.activation.load(Ordering::Acquire);
                let fd = target.fd.load(Ordering::Acquire);
                if !act_ptr.is_null() && fd >= 0 {
                    let peer_act = unsafe { &*act_ptr };
                    let pending = peer_act.state[0].pending.fetch_sub(1, Ordering::AcqRel);
                    if pending == 1 {
                        // Pending signals reached 0
                        if peer_act
                            .status
                            .compare_exchange(
                                PW_NODE_ACTIVATION_NOT_TRIGGERED,
                                PW_NODE_ACTIVATION_TRIGGERED,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            unsafe {
                                (*act_ptr).signal_time = now_ns;
                            }
                            let val: u64 = 1;
                            let _ = unsafe {
                                libc::write(
                                    fd,
                                    &val as *const u64 as *const libc::c_void,
                                    std::mem::size_of::<u64>(),
                                )
                            };
                        }
                    }
                }
            }
        }
    }

    debug!("Realtime data thread exited cleanly");
}
