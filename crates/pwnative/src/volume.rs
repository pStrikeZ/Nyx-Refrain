//! PipeWire virtual sink volume control and synchronization.

use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};

/// Represents volume state of the PipeWire audio sink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SinkVolume {
    /// Volume percentage (0.0 to 100.0%).
    pub pct: f32,
    /// Mute state.
    pub mute: bool,
}

impl SinkVolume {
    pub fn new(pct: f32, mute: bool) -> Self {
        Self { pct, mute }
    }
}

impl Default for SinkVolume {
    fn default() -> Self {
        Self {
            pct: 100.0,
            mute: false,
        }
    }
}

/// Maps linear volume factor to cubic displayed percentage (0–100%), matching KDE/GNOME sliders.
pub fn linear_to_cubic_pct(linear: f32) -> f32 {
    if linear <= 0.0 {
        0.0
    } else {
        (linear.cbrt() * 100.0).clamp(0.0, 100.0)
    }
}

/// Maps cubic displayed percentage (0–100%) to linear volume factor: (pct / 100)^3.
pub fn cubic_pct_to_linear(pct: f32) -> f32 {
    if pct <= 0.0 {
        0.0
    } else {
        (pct.clamp(0.0, 100.0) / 100.0).powi(3)
    }
}

#[derive(Debug)]
pub(crate) enum SinkCommand {
    SetVolume { pct: f32, mute: bool },
}

type VolumeListener = Box<dyn Fn(SinkVolume) + Send + Sync + 'static>;

struct SinkVolumeControlInner {
    current: Mutex<SinkVolume>,
    last_pushed: Mutex<Option<SinkVolume>>,
    sender: Mutex<Option<(std::sync::mpsc::Sender<SinkCommand>, RawFd)>>,
    listeners: Mutex<Vec<VolumeListener>>,
    subscribers: Mutex<Vec<std::sync::mpsc::Sender<SinkVolume>>>,
}

/// Handle for observing and controlling the PipeWire sink volume.
///
/// Thread-safe (`Clone + Send + Sync`).
#[derive(Clone)]
pub struct SinkVolumeControl {
    inner: Arc<SinkVolumeControlInner>,
}

impl std::fmt::Debug for SinkVolumeControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SinkVolumeControl")
            .field("current", &self.current())
            .finish()
    }
}

impl SinkVolumeControl {
    pub fn new(initial: SinkVolume) -> Self {
        Self {
            inner: Arc::new(SinkVolumeControlInner {
                current: Mutex::new(initial),
                last_pushed: Mutex::new(None),
                sender: Mutex::new(None),
                listeners: Mutex::new(Vec::new()),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Current sink volume.
    pub fn current(&self) -> SinkVolume {
        *self.inner.current.lock().unwrap()
    }

    /// Set sink volume and mute state, pushing new Props to PipeWire.
    ///
    /// If `pct <= 0.0`, mute is automatically treated as true.
    /// The update is registered to prevent echo loops when PipeWire re-announces Props.
    pub fn set(&self, pct: f32, mute: bool) {
        let mute = mute || pct <= 0.0;
        let pct = if pct <= 0.0 {
            0.0
        } else {
            pct.clamp(0.0, 100.0)
        };
        let vol = SinkVolume { pct, mute };

        *self.inner.last_pushed.lock().unwrap() = Some(vol);
        *self.inner.current.lock().unwrap() = vol;

        if let Some((tx, fd)) = self.inner.sender.lock().unwrap().as_ref() {
            let _ = tx.send(SinkCommand::SetVolume { pct, mute });
            let val: u64 = 1;
            unsafe {
                libc::write(*fd, &val as *const u64 as *const libc::c_void, 8);
            }
        }
    }

    /// Register a callback to be called whenever desktop slider / mute changes.
    pub fn on_change<F: Fn(SinkVolume) + Send + Sync + 'static>(&self, callback: F) {
        self.inner
            .listeners
            .lock()
            .unwrap()
            .push(Box::new(callback));
    }

    /// Subscribe to volume changes via a standard mpsc channel.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<SinkVolume> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.inner.subscribers.lock().unwrap().push(tx);
        rx
    }

    pub(crate) fn attach(&self, tx: std::sync::mpsc::Sender<SinkCommand>, command_fd: RawFd) {
        let mut sender = self.inner.sender.lock().unwrap();
        *sender = Some((tx, command_fd));
    }

    pub(crate) fn detach(&self) {
        let mut sender = self.inner.sender.lock().unwrap();
        *sender = None;
    }

    /// Called by the main loop when Props are received from PipeWire.
    pub(crate) fn notify_from_pipewire(&self, new_vol: SinkVolume) {
        // 1. Dedupe against last_pushed: ignore Props equal to what we just pushed
        {
            let mut pushed = self.inner.last_pushed.lock().unwrap();
            if let Some(p) = *pushed {
                let mute_match = p.mute == new_vol.mute;
                let pct_match = (p.pct - new_vol.pct).abs() < 0.5;
                if mute_match && (p.mute || pct_match) {
                    // Echo of what we just pushed. Ignore and clear guard.
                    *pushed = None;
                    return;
                }
            }
        }

        // 2. Dedupe against current volume
        {
            let mut cur = self.inner.current.lock().unwrap();
            let mute_match = cur.mute == new_vol.mute;
            let pct_match = (cur.pct - new_vol.pct).abs() < 0.1;
            if mute_match && (cur.mute || pct_match) {
                return;
            }
            *cur = new_vol;
        }

        // 3. Notify listeners
        {
            let listeners = self.inner.listeners.lock().unwrap();
            for cb in listeners.iter() {
                cb(new_vol);
            }
        }

        // 4. Notify subscribers
        {
            let mut subs = self.inner.subscribers.lock().unwrap();
            subs.retain(|tx| tx.send(new_vol).is_ok());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_cubic_linear_conversions() {
        assert_eq!(cubic_pct_to_linear(0.0), 0.0);
        assert_eq!(cubic_pct_to_linear(100.0), 1.0);
        assert_eq!(linear_to_cubic_pct(0.0), 0.0);
        assert_eq!(linear_to_cubic_pct(1.0), 100.0);

        let lin_40 = cubic_pct_to_linear(40.0);
        assert!((lin_40 - 0.064).abs() < 1e-5);
        let pct_40 = linear_to_cubic_pct(lin_40);
        assert!((pct_40 - 40.0).abs() < 1e-4);
    }

    #[test]
    fn test_dedupe_echo_from_set() {
        let ctrl = SinkVolumeControl::new(SinkVolume {
            pct: 25.0,
            mute: false,
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        ctrl.on_change(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        });

        // App sets 50%
        ctrl.set(50.0, false);
        assert_eq!(ctrl.current().pct, 50.0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // PipeWire echoes 50% back (floating point slight discrepancy e.g. 50.000004)
        ctrl.notify_from_pipewire(SinkVolume {
            pct: 50.000004,
            mute: false,
        });
        // Must be dropped as echo!
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // User changes slider to 75%
        ctrl.notify_from_pipewire(SinkVolume {
            pct: 75.0,
            mute: false,
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(ctrl.current().pct, 75.0);

        // Duplicate notification from PipeWire (e.g. repeated Props)
        ctrl.notify_from_pipewire(SinkVolume {
            pct: 75.0,
            mute: false,
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // User mutes
        ctrl.notify_from_pipewire(SinkVolume {
            pct: 75.0,
            mute: true,
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(ctrl.current().mute);
    }
}
