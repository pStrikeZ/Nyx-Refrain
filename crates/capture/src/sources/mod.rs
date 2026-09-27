//! Test audio sources for cross-platform validation.

pub mod silence;
pub mod sine;
pub mod stdin;
pub mod wav;

pub use silence::SilenceSource;
pub use sine::SineSource;
pub use stdin::{StdinFormat, StdinSource};
pub use wav::{FileWavSource, WavSource};
