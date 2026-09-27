//! Persisted GUI settings (`gui.toml` next to `devices.toml` in the config directory).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Low,
    #[default]
    Balanced,
    Stable,
    Custom,
}

impl Profile {
    #[allow(dead_code)]
    pub const ALL: [Profile; 4] = [
        Profile::Low,
        Profile::Balanced,
        Profile::Stable,
        Profile::Custom,
    ];

    /// Sync latency presets (end-to-end ≈ 237 ms + value on a HomePod mini, tvOS 27).
    pub fn sync_latency_ms(self, custom: i32) -> i32 {
        match self {
            Profile::Low => -140,
            Profile::Balanced => -100,
            Profile::Stable => 0,
            Profile::Custom => custom,
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Profile::Low => "低延迟",
            Profile::Balanced => "均衡",
            Profile::Stable => "稳定",
            Profile::Custom => "自定义",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LanguageChoice {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en")]
    En,
}

impl LanguageChoice {
    pub fn resolve(self) -> crate::i18n::Lang {
        match self {
            LanguageChoice::Auto => crate::i18n::detect_system_language(),
            LanguageChoice::ZhCn => crate::i18n::Lang::ZhCn,
            LanguageChoice::En => crate::i18n::Lang::En,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    #[default]
    Process,
    Endpoint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Last selected device name (matched against discovery results).
    pub device_name: Option<String>,
    /// Last device address ("ip:port"), used when discovery does not find it.
    pub device_addr: Option<String>,
    pub profile: Profile,
    pub custom_sync_latency_ms: i32,
    pub volume_pct: f32,
    pub language: LanguageChoice,
    pub send_now_playing: bool,
    pub remote_control: bool,
    pub capture_mode: CaptureMode,
    /// Reconnect to the saved device on launch if the app last exited while streaming.
    pub resume_on_launch: bool,
    /// Whether streaming was left on: set by Start, cleared only by an explicit Stop (quitting,
    /// logging out or shutting down while streaming keeps it set).
    pub was_streaming: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: None,
            device_addr: None,
            profile: Profile::Balanced,
            custom_sync_latency_ms: -100,
            volume_pct: 50.0,
            language: LanguageChoice::Auto,
            send_now_playing: true,
            remote_control: true,
            capture_mode: CaptureMode::Process,
            resume_on_launch: true,
            was_streaming: false,
        }
    }
}

impl Settings {
    fn path() -> std::path::PathBuf {
        discovery::default_config_dir().join("gui.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let s = Settings {
            device_name: Some("Living Room".into()),
            device_addr: Some("192.0.2.106:7000".into()),
            profile: Profile::Custom,
            custom_sync_latency_ms: -120,
            volume_pct: 42.0,
            language: LanguageChoice::ZhCn,
            send_now_playing: false,
            remote_control: false,
            capture_mode: CaptureMode::Endpoint,
            resume_on_launch: false,
            was_streaming: true,
        };
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("language = \"zh-CN\""));
        assert!(text.contains("send_now_playing = false"));
        assert!(text.contains("capture_mode = \"endpoint\""));
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.device_name.as_deref(), Some("Living Room"));
        assert_eq!(back.profile, Profile::Custom);
        assert_eq!(
            back.profile.sync_latency_ms(back.custom_sync_latency_ms),
            -120
        );
        assert_eq!(back.language, LanguageChoice::ZhCn);
        assert!(!back.send_now_playing);
        assert!(!back.remote_control);
        assert_eq!(back.capture_mode, CaptureMode::Endpoint);
        assert!(!back.resume_on_launch);
        assert!(back.was_streaming);

        // Old gui.toml without language, send_now_playing or capture_mode must get the defaults
        let partial: Settings = toml::from_str("volume_pct = 10.0").unwrap();
        assert_eq!(partial.profile, Profile::Balanced);
        assert_eq!(Profile::Balanced.sync_latency_ms(0), -100);
        assert_eq!(partial.language, LanguageChoice::Auto);
        assert!(partial.send_now_playing);
        assert!(partial.remote_control);
        assert_eq!(partial.capture_mode, CaptureMode::Process);
        assert!(partial.resume_on_launch);
        assert!(!partial.was_streaming);

        // Explicit en
        let en: Settings = toml::from_str("language = \"en\"").unwrap();
        assert_eq!(en.language, LanguageChoice::En);
        assert!(en.send_now_playing);

        // Explicit auto
        let auto: Settings = toml::from_str("language = \"auto\"").unwrap();
        assert_eq!(auto.language, LanguageChoice::Auto);
        assert!(auto.send_now_playing);
    }
}
