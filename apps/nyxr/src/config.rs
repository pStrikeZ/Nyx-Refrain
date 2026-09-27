//! Nyx Refrain configuration file management.
//!
//! Handles loading and saving `%APPDATA%\nyx-refrain\config.toml` (Windows),
//! `~/.config/nyx-refrain/config.toml` (Linux), or Application Support (macOS).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Main Nyx Refrain configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NyxRefrainConfig {
    /// Default target IP, IP:PORT, or mDNS device name (e.g. "Living Room").
    #[serde(default)]
    pub target: Option<String>,

    /// Network interface name to bind streaming traffic to.
    #[serde(default)]
    pub interface: Option<String>,

    /// Initial volume (0 - 100 percentage or dB).
    #[serde(default)]
    pub volume: Option<f32>,

    /// Audio capture source: "pipewire", "wasapi", "sine", "silence", "wav", or "stdin".
    #[serde(default = "default_capture")]
    pub capture: String,

    /// Tracing log level filter (e.g. "info", "debug", "warn").
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_capture() -> String {
    #[cfg(windows)]
    {
        "wasapi".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        "pipewire".to_string()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "sine".to_string()
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for NyxRefrainConfig {
    fn default() -> Self {
        Self {
            target: None,
            interface: None,
            volume: None,
            capture: default_capture(),
            log_level: default_log_level(),
        }
    }
}

impl NyxRefrainConfig {
    /// Platform-specific path to `config.toml`.
    pub fn default_path() -> PathBuf {
        discovery::default_config_dir().join("config.toml")
    }

    /// Load configuration from a specific path. If the file does not exist, returns default config.
    pub fn load_from_path(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(config)
    }

    /// Load configuration from default platform path, logging a warning on parse error.
    pub fn load_default() -> Self {
        let path = Self::default_path();
        Self::load_from_path(&path).unwrap_or_else(|e| {
            warn!("Failed to read config from {:?}: {e}; using defaults", path);
            Self::default()
        })
    }

    /// Save configuration to path, creating parent directories if necessary.
    #[allow(dead_code)]
    pub fn save_to_path(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let cfg = NyxRefrainConfig::default();
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn test_toml_roundtrip() {
        let toml_str = r#"
            target = "Living Room"
            volume = 20.0
            capture = "sine"
            log_level = "debug"
        "#;
        let cfg: NyxRefrainConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.target.as_deref(), Some("Living Room"));
        assert_eq!(cfg.volume, Some(20.0));
        assert_eq!(cfg.capture, "sine");
        assert_eq!(cfg.log_level, "debug");
    }

    #[test]
    fn test_legacy_airplay1_fields_are_ignored() {
        let cfg: NyxRefrainConfig = toml::from_str(
            r#"
            target = "loopback"
            latency_ms = 350
            prefill_ms = 50
            lead_ms = 60
            encryption = "rsa"
            silence_keepalive = false
            future_option = true
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg,
            NyxRefrainConfig {
                target: Some("loopback".to_string()),
                ..Default::default()
            }
        );
        let saved = toml::to_string(&cfg).unwrap();
        for field in [
            "latency_ms",
            "prefill_ms",
            "lead_ms",
            "encryption",
            "silence_keepalive",
        ] {
            assert!(!saved.contains(field), "removed field persisted: {field}");
        }
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let temp_dir =
            std::env::temp_dir().join(format!("nyx_refrain_test_{}", std::process::id()));
        let config_path = temp_dir.join("test_config.toml");
        let cfg = NyxRefrainConfig {
            target: Some("Living Room".to_string()),
            ..Default::default()
        };
        cfg.save_to_path(&config_path).unwrap();

        let loaded = NyxRefrainConfig::load_from_path(&config_path).unwrap();
        assert_eq!(loaded, cfg);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
