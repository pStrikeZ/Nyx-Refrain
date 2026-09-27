//! AirPlay 2 sender support (realtime audio, type 0x60) for receivers such as HomePods on
//! tvOS versions that no longer play AirPlay 1 audio.

pub mod audio;
pub mod hap;
pub mod history;
pub mod metadata;
pub mod remote;
pub mod session;
pub mod srp;
pub mod tlv8;
