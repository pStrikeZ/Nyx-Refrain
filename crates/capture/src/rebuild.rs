//! Device change rebuild state machine for audio capture streams.
//!
//! When an audio device change or removal notification occurs, transitions
//! the capture source into a `Rebuilding` state. While rebuilding, the source
//! produces digital silence chunks of the expected frame count so the downstream
//! streaming session (e.g. RAOP) remains continuous without underflow or disconnect.
//! Once the new device is initialized, it transitions back to `Active`.

use std::time::Instant;

use crate::AudioChunk;

/// States of the capture rebuild lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildState {
    /// Audio capture is running normally from an active endpoint.
    Active,
    /// Device change or removal detected; capture stream is being re-established.
    Rebuilding {
        started_at: Instant,
        frames_emitted: u64,
        reason: String,
    },
    /// Rebuilding failed and could not be recovered.
    Failed { error: String },
}

/// State machine coordinating endpoint rebuilds and silence chunk generation.
#[derive(Debug, Clone)]
pub struct RebuildStateMachine {
    state: RebuildState,
}

impl Default for RebuildStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl RebuildStateMachine {
    /// Create a new state machine in the `Active` state.
    pub fn new() -> Self {
        Self {
            state: RebuildState::Active,
        }
    }

    /// Current rebuild state.
    pub fn state(&self) -> &RebuildState {
        &self.state
    }

    /// Whether the capture source is currently rebuilding.
    pub fn is_rebuilding(&self) -> bool {
        matches!(self.state, RebuildState::Rebuilding { .. })
    }

    /// Whether the capture source is active.
    pub fn is_active(&self) -> bool {
        matches!(self.state, RebuildState::Active)
    }

    /// Trigger a rebuild due to device change, removal, or stream invalidation.
    pub fn trigger_rebuild(&mut self, reason: impl Into<String>) {
        let reason_str = reason.into();
        self.state = RebuildState::Rebuilding {
            started_at: Instant::now(),
            frames_emitted: 0,
            reason: reason_str,
        };
    }

    /// Mark the rebuild as complete and return to `Active`.
    pub fn mark_active(&mut self) {
        self.state = RebuildState::Active;
    }

    /// Mark the rebuild as failed.
    pub fn mark_failed(&mut self, error: impl Into<String>) {
        self.state = RebuildState::Failed {
            error: error.into(),
        };
    }

    /// Return total frames emitted as silence during the current rebuild.
    pub fn frames_emitted(&self) -> u64 {
        match &self.state {
            RebuildState::Rebuilding { frames_emitted, .. } => *frames_emitted,
            _ => 0,
        }
    }

    /// Generate an `AudioChunk` of digital silence to keep the downstream pipeline fed
    /// while rebuilding.
    pub fn generate_silence_chunk(
        &mut self,
        frame_count: usize,
        sample_rate: u32,
        channels: u16,
        timestamp: Instant,
    ) -> AudioChunk {
        let total_samples = frame_count * (channels as usize);
        let silence_data = vec![0.0f32; total_samples];

        if let RebuildState::Rebuilding {
            ref mut frames_emitted,
            ..
        } = self.state
        {
            *frames_emitted += frame_count as u64;
        }

        AudioChunk::new(silence_data, sample_rate, channels, timestamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rebuild_state_transitions() {
        let mut sm = RebuildStateMachine::new();
        assert!(sm.is_active());
        assert!(!sm.is_rebuilding());

        sm.trigger_rebuild("device removed");
        assert!(sm.is_rebuilding());
        assert!(!sm.is_active());

        let t0 = Instant::now();
        let chunk1 = sm.generate_silence_chunk(352, 44100, 2, t0);
        assert_eq!(chunk1.data.len(), 704);
        assert!(chunk1.data.iter().all(|&s| s == 0.0));
        assert_eq!(chunk1.sample_rate, 44100);
        assert_eq!(chunk1.channels, 2);
        assert_eq!(chunk1.timestamp, t0);
        assert_eq!(sm.frames_emitted(), 352);

        let chunk2 = sm.generate_silence_chunk(352, 44100, 2, t0);
        assert_eq!(sm.frames_emitted(), 704);
        assert_eq!(chunk2.data.len(), 704);

        sm.mark_active();
        assert!(sm.is_active());
        assert!(!sm.is_rebuilding());
    }

    #[test]
    fn test_rebuild_failure() {
        let mut sm = RebuildStateMachine::new();
        sm.trigger_rebuild("switch device");
        sm.mark_failed("device not found");
        assert_eq!(
            sm.state(),
            &RebuildState::Failed {
                error: "device not found".into()
            }
        );
    }
}
