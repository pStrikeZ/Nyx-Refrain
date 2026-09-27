//! Lock-free history of sent (already encrypted) AirPlay 2 audio packets for retransmits.
//!
//! Single writer (the audio thread) and any number of readers (the control thread).
//! Slots store variable-length packets up to `MAX_PACKET` using a seqlock-style protocol.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicI32, AtomicU16, Ordering};

/// Upper bound for an AirPlay 2 realtime packet (12 + 1408 PCM + 16 tag + 8 nonce = 1444).
pub const MAX_PACKET: usize = 1500;
/// ~4 s of packets at 125 packets/s.
pub const CAPACITY: usize = 512;

struct Slot {
    seq: AtomicI32,
    len: AtomicU16,
    data: UnsafeCell<[u8; MAX_PACKET]>,
}

// Safety: `data` is only read after checking `seq` before and after the copy (seqlock).
unsafe impl Sync for Slot {}

pub struct PacketHistory {
    slots: Box<[Slot]>,
}

impl Default for PacketHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketHistory {
    pub fn new() -> Self {
        let slots = (0..CAPACITY)
            .map(|_| Slot {
                seq: AtomicI32::new(-1),
                len: AtomicU16::new(0),
                data: UnsafeCell::new([0u8; MAX_PACKET]),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { slots }
    }

    /// Stores a packet (writer side; no allocation). Packets longer than `MAX_PACKET` are ignored.
    pub fn insert(&self, seq: u16, packet: &[u8]) {
        if packet.len() > MAX_PACKET {
            return;
        }
        let slot = &self.slots[seq as usize % CAPACITY];
        slot.seq.store(-1, Ordering::Release);
        // Safety: single writer; readers detect the concurrent write via `seq`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                packet.as_ptr(),
                (*slot.data.get()).as_mut_ptr(),
                packet.len(),
            );
        }
        slot.len.store(packet.len() as u16, Ordering::Relaxed);
        slot.seq.store(seq as i32, Ordering::Release);
    }

    /// Copies the packet with sequence number `seq` into `out`; returns its length.
    pub fn get(&self, seq: u16, out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        let slot = &self.slots[seq as usize % CAPACITY];
        if slot.seq.load(Ordering::Acquire) != seq as i32 {
            return None;
        }
        let len = slot.len.load(Ordering::Relaxed) as usize;
        // Safety: validated by re-reading `seq` after the copy.
        unsafe {
            std::ptr::copy_nonoverlapping((*slot.data.get()).as_ptr(), out.as_mut_ptr(), len);
        }
        if slot.seq.load(Ordering::Acquire) != seq as i32 {
            return None;
        }
        Some(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_evicts() {
        let h = PacketHistory::new();
        let mut out = [0u8; MAX_PACKET];
        h.insert(7, &[1, 2, 3]);
        assert_eq!(h.get(7, &mut out), Some(3));
        assert_eq!(&out[..3], &[1, 2, 3]);
        assert_eq!(h.get(8, &mut out), None);
        h.insert(7 + CAPACITY as u16, &[9; 1444]);
        assert_eq!(h.get(7, &mut out), None, "evicted by wraparound");
        assert_eq!(h.get(7 + CAPACITY as u16, &mut out), Some(1444));
    }

    #[test]
    fn oversized_packets_are_ignored() {
        let h = PacketHistory::new();
        let mut out = [0u8; MAX_PACKET];
        h.insert(1, &[0; MAX_PACKET + 1]);
        assert_eq!(h.get(1, &mut out), None);
    }
}
