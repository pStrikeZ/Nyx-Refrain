//! Persistent device caching in platform config dir.
//!
//! Stores and loads discovered AirPlay devices to/from `devices.toml`.
//! Provides fallback when mDNS browsing times out or is temporarily unavailable.

use serde::{Deserialize, Serialize};
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

use crate::device::DiscoveredDevice;

/// Structure representing `devices.toml` contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCache {
    #[serde(default)]
    pub devices: Vec<DiscoveredDevice>,
}

impl DeviceCache {
    /// Loads the device cache from the specified path.
    pub fn load_from_path(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        let cache: Self = toml::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        debug!(
            "Loaded {} cached device(s) from {:?}",
            cache.devices.len(),
            path
        );
        Ok(cache)
    }

    /// Saves the device cache to the specified path, creating directories if needed.
    pub fn save_to_path(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, content)?;
        debug!(
            "Saved {} cached device(s) to {:?}",
            self.devices.len(),
            path
        );
        Ok(())
    }

    /// Loads the cache from the default platform config path.
    pub fn load_default() -> Self {
        let path = default_cache_path();
        Self::load_from_path(&path).unwrap_or_else(|e| {
            warn!(
                "Failed to read device cache from {:?}: {e}; using empty cache",
                path
            );
            Self::default()
        })
    }

    /// Saves the cache to the default platform config path.
    pub fn save_default(&self) -> Result<(), std::io::Error> {
        let path = default_cache_path();
        self.save_to_path(&path)
    }

    /// Merges new discovered devices into the cache.
    ///
    /// Matches by instance name or MAC address. Updates existing entries and appends new ones.
    pub fn update(&mut self, new_devices: &[DiscoveredDevice]) {
        for new_dev in new_devices {
            if let Some(existing) = self.devices.iter_mut().find(|d| {
                d.instance_name == new_dev.instance_name
                    || (d.mac.is_some() && d.mac == new_dev.mac)
                    || d.name == new_dev.name
            }) {
                *existing = new_dev.clone();
            } else {
                self.devices.push(new_dev.clone());
            }
        }
    }

    /// Looks up a cached device by name (NFC normalized).
    pub fn find_by_name(&self, name: &str) -> Option<&DiscoveredDevice> {
        use unicode_normalization::UnicodeNormalization;
        let norm_name: String = name.nfc().collect();
        self.devices.iter().find(|d| {
            let d_norm: String = d.name.nfc().collect();
            d_norm == norm_name
        })
    }

    /// Looks up a cached device by IPv4 address.
    pub fn find_by_ip(&self, ip: Ipv4Addr) -> Option<&DiscoveredDevice> {
        self.devices.iter().find(|d| d.ip == ip)
    }
}

/// Computes the platform-specific directory for `devices.toml`.
///
/// - Windows: `%APPDATA%\nyx-refrain`
/// - Linux: `$XDG_CONFIG_HOME/nyx-refrain` or `~/.config/nyx-refrain`
/// - macOS: `~/Library/Application Support/nyx-refrain`
pub fn default_config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("nyx-refrain");
        }
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            return PathBuf::from(userprofile)
                .join("AppData")
                .join("Roaming")
                .join("nyx-refrain");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("nyx-refrain");
        }
    }

    // Linux and fallback
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("nyx-refrain");
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("nyx-refrain");
    }

    PathBuf::from(".nyx-refrain")
}

/// Returns the default platform path to `devices.toml`.
pub fn default_cache_path() -> PathBuf {
    default_config_dir().join("devices.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use raop::capabilities::DeviceCapabilities;
    use std::collections::HashMap;

    #[test]
    fn test_cache_save_and_load_roundtrip() {
        let temp_dir = std::env::temp_dir().join("nyx_refrain_test_cache");
        let cache_path = temp_dir.join("devices.toml");

        let mut cache = DeviceCache::default();
        let dev = DiscoveredDevice {
            name: "Living Room".to_string(),
            instance_name: "AABBCCDDEEFF@Living Room._raop._tcp.local.".to_string(),
            ip: Ipv4Addr::new(192, 0, 2, 106),
            port: 7000,
            host: Some("living-room.local.".to_string()),
            mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
            capabilities: DeviceCapabilities::from_txt_strings([
                "cn=0,1,2,3",
                "et=0,3,5",
                "am=AudioAccessory5,1",
            ]),
            txt_records: HashMap::from([("cn".to_string(), "0,1,2,3".to_string())]),
            last_seen_epoch_secs: 1727200000,
        };

        cache.update(std::slice::from_ref(&dev));
        assert_eq!(cache.devices.len(), 1);

        cache
            .save_to_path(&cache_path)
            .expect("Failed to save cache");
        assert!(cache_path.exists());

        let loaded = DeviceCache::load_from_path(&cache_path).expect("Failed to load cache");
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].name, "Living Room");
        assert_eq!(loaded.devices[0].ip, Ipv4Addr::new(192, 0, 2, 106));

        assert!(loaded.find_by_name("Living Room").is_some());
        assert!(loaded.find_by_ip(Ipv4Addr::new(192, 0, 2, 106)).is_some());

        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_dir(&temp_dir);
    }
}
