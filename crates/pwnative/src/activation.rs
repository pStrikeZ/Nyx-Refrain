//! C-compatible layout and definitions for PipeWire node activation and SPA IO structures.
//!
//! Layout matches PipeWire 1.6.x (and 64-bit LP64 Linux systems).
//! Offsets and sizes are statically asserted at compile time.

use std::sync::atomic::{AtomicI32, AtomicU32};

pub const PW_VERSION_NODE_ACTIVATION: u32 = 1;

pub const PW_NODE_ACTIVATION_NOT_TRIGGERED: u32 = 0;
pub const PW_NODE_ACTIVATION_TRIGGERED: u32 = 1;
pub const PW_NODE_ACTIVATION_AWAKE: u32 = 2;
pub const PW_NODE_ACTIVATION_FINISHED: u32 = 3;
pub const PW_NODE_ACTIVATION_INACTIVE: u32 = 4;

pub const SPA_STATUS_OK: u32 = 0;
pub const SPA_STATUS_NEED_DATA: u32 = 1 << 0;
pub const SPA_STATUS_HAVE_DATA: u32 = 1 << 1;
pub const SPA_STATUS_STOPPED: u32 = 1 << 2;
pub const SPA_STATUS_DRAINED: u32 = 1 << 3;

#[repr(C)]
#[derive(Debug)]
pub struct PwNodeActivationState {
    pub status: i32,
    pub required: i32,
    pub pending: AtomicI32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpaFraction {
    pub num: u32,
    pub denom: u32,
}

#[repr(C)]
#[derive(Debug)]
pub struct SpaIoClock {
    pub flags: u32,
    pub id: u32,
    pub name: [u8; 64],
    pub nsec: u64,
    pub rate: SpaFraction,
    pub position: u64,
    pub duration: u64,
    pub delay: i64,
    pub rate_diff: f64,
    pub next_nsec: u64,
    pub target_rate: SpaFraction,
    pub target_duration: u64,
    pub target_seq: u32,
    pub cycle: u32,
    pub padding: [u8; 8],
}

#[repr(C)]
#[derive(Debug)]
pub struct SpaIoSegment {
    pub version: u32,
    pub flags: u32,
    pub start: u64,
    pub duration: u64,
    pub rate: f64,
    pub position: i64,
    pub bar: [u8; 64],
    pub video: [u8; 80],
}

#[repr(C)]
#[derive(Debug)]
pub struct SpaIoVideoSize {
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub framerate: SpaFraction,
    pub padding: [u8; 20],
}

pub const SPA_IO_POSITION_MAX_SEGMENTS: usize = 8;

#[repr(C)]
#[derive(Debug)]
pub struct SpaIoPosition {
    pub clock: SpaIoClock,
    pub video: SpaIoVideoSize,
    pub offset: i64,
    pub state: u32,
    pub n_segments: u32,
    pub segments: [SpaIoSegment; SPA_IO_POSITION_MAX_SEGMENTS],
}

#[repr(C)]
#[derive(Debug)]
pub struct PwNodeActivation {
    pub status: AtomicU32,
    pub version_flags: u32,
    pub state: [PwNodeActivationState; 2],
    pub signal_time: u64,
    pub awake_time: u64,
    pub finish_time: u64,
    pub prev_signal_time: u64,
    pub reposition: SpaIoSegment,
    pub segment: SpaIoSegment,
    pub segment_owner: [u32; 16],
    pub prev_awake_time: u64,
    pub prev_finish_time: u64,
    pub padding: [u32; 7],
    pub client_version: u32,
    pub server_version: u32,
    pub active_driver_id: u32,
    pub driver_id: u32,
    pub flags: u32,
    pub position: SpaIoPosition,
    pub sync_timeout: u64,
    pub sync_left: u64,
    pub cpu_load: [f32; 3],
    pub xrun_count: u32,
    pub xrun_time: u64,
    pub xrun_delay: u64,
    pub max_delay: u64,
    pub command: u32,
    pub reposition_owner: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpaIoBuffers {
    pub status: u32,
    pub buffer_id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpaChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpaData {
    pub type_: u32,
    pub flags: u32,
    pub fd: i64,
    pub mapoffset: u32,
    pub maxsize: u32,
    pub data: *mut u8,
    pub chunk: *mut SpaChunk,
}

// Compile-time static assertions for ABI layout
const _: () = {
    use std::mem::{offset_of, size_of};

    assert!(size_of::<PwNodeActivationState>() == 12);
    assert!(size_of::<SpaIoClock>() == 160);
    assert!(size_of::<SpaIoSegment>() == 184);
    assert!(size_of::<SpaIoVideoSize>() == 40);
    assert!(size_of::<SpaIoPosition>() == 1688);
    assert!(size_of::<PwNodeActivation>() == 2312);
    assert!(size_of::<SpaIoBuffers>() == 8);
    assert!(size_of::<SpaChunk>() == 16);

    assert!(offset_of!(PwNodeActivation, status) == 0);
    assert!(offset_of!(PwNodeActivation, state) == 8);
    assert!(offset_of!(PwNodeActivation, signal_time) == 32);
    assert!(offset_of!(PwNodeActivation, awake_time) == 40);
    assert!(offset_of!(PwNodeActivation, finish_time) == 48);
    assert!(offset_of!(PwNodeActivation, prev_signal_time) == 56);
    assert!(offset_of!(PwNodeActivation, reposition) == 64);
    assert!(offset_of!(PwNodeActivation, segment) == 248);
    assert!(offset_of!(PwNodeActivation, segment_owner) == 432);
    assert!(offset_of!(PwNodeActivation, client_version) == 540);
    assert!(offset_of!(PwNodeActivation, server_version) == 544);
    assert!(offset_of!(PwNodeActivation, active_driver_id) == 548);
    assert!(offset_of!(PwNodeActivation, driver_id) == 552);
    assert!(offset_of!(PwNodeActivation, flags) == 556);
    assert!(offset_of!(PwNodeActivation, position) == 560);
    assert!(offset_of!(PwNodeActivation, sync_timeout) == 2248);
    assert!(offset_of!(PwNodeActivation, command) == 2304);

    assert!(offset_of!(SpaIoPosition, clock) == 0);
    assert!(offset_of!(SpaIoPosition, video) == 160);
    assert!(offset_of!(SpaIoPosition, offset) == 200);
    assert!(offset_of!(SpaIoPosition, state) == 208);
    assert!(offset_of!(SpaIoPosition, n_segments) == 212);
    assert!(offset_of!(SpaIoPosition, segments) == 216);

    assert!(offset_of!(SpaIoClock, nsec) == 72);
    assert!(offset_of!(SpaIoClock, rate) == 80);
    assert!(offset_of!(SpaIoClock, duration) == 96);

    assert!(offset_of!(SpaIoBuffers, status) == 0);
    assert!(offset_of!(SpaIoBuffers, buffer_id) == 4);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activation_layout() {
        assert_eq!(std::mem::size_of::<PwNodeActivation>(), 2312);
        assert_eq!(std::mem::size_of::<SpaIoPosition>(), 1688);
        assert_eq!(std::mem::size_of::<SpaIoClock>(), 160);
        assert_eq!(std::mem::size_of::<SpaIoBuffers>(), 8);
    }
}
