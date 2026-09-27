//! Volume conversion utilities between linear factors, cubic percentages (0–100%),
//! and decibels (-30..0 dB, -144 dB = mute).

/// Maps percentage (0–100%) to receiver decibels (-30..0 dB), 0% = mute (-144 dB).
pub fn pct_to_db(pct: f32) -> f32 {
    if pct <= 0.0 {
        -144.0
    } else {
        -30.0 + 30.0 * (pct.min(100.0) / 100.0)
    }
}

/// Maps receiver decibels (-30..0 dB) to percentage (0–100%), <= -30 dB (including -144) = 0%.
pub fn db_to_pct(db: f32) -> f32 {
    if db <= -30.0 {
        0.0
    } else if db >= 0.0 {
        100.0
    } else {
        ((db + 30.0) / 30.0 * 100.0).clamp(0.0, 100.0)
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

/// Maps percentage and mute flag to receiver decibels (-30..0 dB or -144 dB).
pub fn volume_to_db(pct: f32, mute: bool) -> f32 {
    if mute || pct <= 0.0 {
        -144.0
    } else {
        pct_to_db(pct)
    }
}

/// Translates CLI volume argument (pct 0..=100 or negative dB < 0) to receiver decibels (-30..0 dB or -144 dB).
///
/// - `0` is treated as 0% = mute (-144 dB).
/// - Negative values are treated as decibels (e.g. `-20` -> -20 dB; `<= -144` clamped to -144 dB).
/// - `1..=100` are treated as percentages (`pct_to_db`).
/// - `> 100` is clamped to 0 dB.
pub fn cli_volume_to_db(vol: Option<f32>, effective_quiet: bool) -> f32 {
    if effective_quiet {
        -144.0
    } else if let Some(vol) = vol {
        if vol <= -144.0 {
            -144.0
        } else if vol < 0.0 {
            vol
        } else if vol <= 100.0 {
            pct_to_db(vol)
        } else {
            0.0
        }
    } else {
        -20.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pct_to_db_and_back() {
        assert_eq!(pct_to_db(0.0), -144.0);
        assert_eq!(pct_to_db(-10.0), -144.0);
        assert_eq!(pct_to_db(100.0), 0.0);
        assert_eq!(pct_to_db(150.0), 0.0);
        assert!((pct_to_db(50.0) - (-15.0)).abs() < 1e-6);

        assert_eq!(db_to_pct(-144.0), 0.0);
        assert_eq!(db_to_pct(-30.0), 0.0);
        assert_eq!(db_to_pct(-40.0), 0.0);
        assert_eq!(db_to_pct(0.0), 100.0);
        assert_eq!(db_to_pct(5.0), 100.0);
        assert!((db_to_pct(-15.0) - 50.0).abs() < 1e-5);

        for pct in 1..=100 {
            let p = pct as f32;
            let db = pct_to_db(p);
            let back = db_to_pct(db);
            assert!((back - p).abs() < 1e-4, "pct {p} -> db {db} -> back {back}");
        }
    }

    #[test]
    fn test_cubic_linear_mapping_round_trip() {
        assert_eq!(cubic_pct_to_linear(0.0), 0.0);
        assert_eq!(cubic_pct_to_linear(-5.0), 0.0);
        assert_eq!(cubic_pct_to_linear(100.0), 1.0);
        assert_eq!(linear_to_cubic_pct(0.0), 0.0);
        assert_eq!(linear_to_cubic_pct(-1.0), 0.0);
        assert_eq!(linear_to_cubic_pct(1.0), 100.0);

        // 40% volume -> 0.4^3 = 0.064 linear
        let lin_40 = cubic_pct_to_linear(40.0);
        assert!((lin_40 - 0.064).abs() < 1e-5);
        let pct_40 = linear_to_cubic_pct(lin_40);
        assert!((pct_40 - 40.0).abs() < 1e-4);

        for pct in 0..=100 {
            let p = pct as f32;
            let lin = cubic_pct_to_linear(p);
            let back = linear_to_cubic_pct(lin);
            assert!(
                (back - p).abs() < 1e-4,
                "pct {p} -> linear {lin} -> back {back}"
            );
        }
    }

    #[test]
    fn test_volume_to_db_with_mute() {
        assert_eq!(volume_to_db(50.0, true), -144.0);
        assert_eq!(volume_to_db(0.0, false), -144.0);
        assert_eq!(volume_to_db(100.0, false), 0.0);
        assert!((volume_to_db(50.0, false) - (-15.0)).abs() < 1e-6);
    }

    #[test]
    fn test_cli_volume_to_db() {
        // Quiet mode always forces -144.0 dB
        assert_eq!(cli_volume_to_db(Some(50.0), true), -144.0);
        assert_eq!(cli_volume_to_db(None, true), -144.0);

        // None defaults to -20.0 dB
        assert_eq!(cli_volume_to_db(None, false), -20.0);

        // 0% is mute (-144.0 dB), NOT 0 dB
        assert_eq!(cli_volume_to_db(Some(0.0), false), -144.0);
        assert_eq!(cli_volume_to_db(Some(-0.0), false), -144.0);

        // Negative values are passed as dB
        assert_eq!(cli_volume_to_db(Some(-15.0), false), -15.0);
        assert_eq!(cli_volume_to_db(Some(-30.0), false), -30.0);
        assert_eq!(cli_volume_to_db(Some(-144.0), false), -144.0);

        // Values <= -144.0 are clamped to -144.0 dB
        assert_eq!(cli_volume_to_db(Some(-150.0), false), -144.0);
        assert_eq!(cli_volume_to_db(Some(-200.0), false), -144.0);

        // Positive 1..=100 mapped via pct_to_db
        assert!((cli_volume_to_db(Some(1.0), false) - pct_to_db(1.0)).abs() < 1e-6);
        assert!((cli_volume_to_db(Some(33.0), false) - pct_to_db(33.0)).abs() < 1e-6);
        assert_eq!(cli_volume_to_db(Some(100.0), false), 0.0);

        // > 100 clamped to 0.0 dB
        assert_eq!(cli_volume_to_db(Some(105.0), false), 0.0);
        assert_eq!(cli_volume_to_db(Some(200.0), false), 0.0);
    }
}
