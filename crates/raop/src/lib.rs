//! AirPlay 2 realtime protocol implementation.
//!
//! Provides encrypted RTSP control, timing synchronization, and RTP audio streaming.

pub mod ap2;
pub mod capabilities;
pub mod clock;
pub mod control;
pub mod rtp;
pub mod rtsp;
pub mod timing;
pub mod volume;

pub use volume::{
    cli_volume_to_db, cubic_pct_to_linear, db_to_pct, linear_to_cubic_pct, pct_to_db, volume_to_db,
};
