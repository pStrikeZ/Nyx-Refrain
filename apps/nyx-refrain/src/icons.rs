//! Pre-rendered Fluent Emoji tray icons, shared by both desktop front-ends.

use crate::engine::State;

pub struct Raster {
    pub size: u32,
    /// Straight-alpha RGBA8, one row after another, with no header or padding.
    pub rgba: &'static [u8],
}

macro_rules! icon_set {
    ($state:literal; $($size:literal),+) => {
        [$(Raster {
            size: $size,
            rgba: include_bytes!(concat!(
                "../../../assets/icons/tray/", $state, "-", $size, ".rgba"
            )),
        }),+]
    };
}

static IDLE: [Raster; 6] = icon_set!("idle"; 16, 22, 24, 32, 48, 64);
static CONNECTING: [Raster; 6] = icon_set!("connecting"; 16, 22, 24, 32, 48, 64);
static STREAMING: [Raster; 6] = icon_set!("streaming"; 16, 22, 24, 32, 48, 64);
static ERROR: [Raster; 6] = icon_set!("error"; 16, 22, 24, 32, 48, 64);

pub fn for_state(state: &State) -> &'static [Raster; 6] {
    match state {
        State::Idle => &IDLE,
        State::Connecting => &CONNECTING,
        State::Streaming => &STREAMING,
        State::Error(_) => &ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineError;

    #[test]
    fn every_embedded_raster_has_the_expected_dimensions() {
        for icons in [&IDLE, &CONNECTING, &STREAMING, &ERROR] {
            for (icon, size) in icons.iter().zip([16, 22, 24, 32, 48, 64]) {
                assert_eq!(icon.size, size);
                assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
                assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
                // Tray art is trimmed to the emoji's edge, but its rounded corners stay
                // (partially) transparent at every size.
                let n = size as usize;
                for corner in [0, n - 1, (n - 1) * n, n * n - 1] {
                    assert!(icon.rgba[corner * 4 + 3] < 255, "{size}px corner is opaque");
                }
            }
        }
    }

    #[test]
    fn state_mapping_and_status_badges() {
        assert!(std::ptr::eq(for_state(&State::Idle), &IDLE));
        assert!(std::ptr::eq(for_state(&State::Connecting), &CONNECTING));
        assert!(std::ptr::eq(for_state(&State::Streaming), &STREAMING));
        assert!(std::ptr::eq(
            for_state(&State::Error(EngineError::Interrupted("test".into()))),
            &ERROR
        ));
        for index in 0..IDLE.len() {
            let size = IDLE[index].size as usize;
            let dot_center = ((size * 3 / 4) * size + size * 3 / 4) * 4;
            assert_eq!(
                &CONNECTING[index].rgba[dot_center..dot_center + 4],
                &[0xf5, 0xc5, 0x18, 255]
            );
            assert_eq!(
                &ERROR[index].rgba[dot_center..dot_center + 4],
                &[0xe5, 0x48, 0x4d, 255]
            );
            // Badges leave the upper half of the milky way unchanged.
            let upper_half = (size / 2) * size * 4;
            assert_eq!(
                &IDLE[index].rgba[..upper_half],
                &CONNECTING[index].rgba[..upper_half]
            );
            assert_eq!(
                &IDLE[index].rgba[..upper_half],
                &ERROR[index].rgba[..upper_half]
            );
            assert_ne!(IDLE[index].rgba, STREAMING[index].rgba);
        }
    }
}
