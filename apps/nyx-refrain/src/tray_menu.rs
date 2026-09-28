//! Platform-neutral tray menu model and shared action handling.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::engine::{self, Cmd, DeviceEntry, Engine, StartParams, State};
use crate::i18n::{Key, Lang, engine_error_title, format_stats_columns, t};
use crate::settings::{CaptureMode, LanguageChoice, Profile, Settings};

/// 0–100% volume presets available from the tray menu.
pub const VOLUME_PRESETS: [f32; 4] = [25.0, 50.0, 75.0, 100.0];

#[derive(Clone, Debug, PartialEq)]
pub enum TrayMenuAction {
    ToggleStreaming,
    SelectDevice(DeviceEntry),
    SelectProfile(Profile),
    SetVolume(f32),
    SelectLanguage(LanguageChoice),
    ToggleSendNowPlaying,
    ToggleRemoteControl,
    ToggleResumeOnLaunch,
    ToggleAutostart,
    ToggleCaptureMode,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuItemKind {
    Standard,
    Checkbox { checked: bool },
    Radio { selected: bool },
    Submenu(Vec<TrayMenuItem>),
    Separator,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrayMenuItem {
    pub label: String,
    pub enabled: bool,
    pub kind: MenuItemKind,
    pub action: Option<TrayMenuAction>,
}

pub fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse().ok().or_else(|| format!("{s}:7000").parse().ok())
}

pub fn selected_device(snap_devices: &[DeviceEntry], settings: &Settings) -> Option<DeviceEntry> {
    if let Some(name) = &settings.device_name
        && let Some(d) = snap_devices.iter().find(|d| &d.name == name)
    {
        return Some(d.clone());
    }
    if let Some(addr_str) = &settings.device_addr
        && let Some(addr) = parse_addr(addr_str)
    {
        return Some(DeviceEntry {
            name: settings
                .device_name
                .clone()
                .unwrap_or_else(|| addr.ip().to_string()),
            addr,
            features: None,
        });
    }
    None
}

pub fn build_start_params(
    snap_devices: &[DeviceEntry],
    settings: &Settings,
) -> Option<StartParams> {
    let device = selected_device(snap_devices, settings)?;
    Some(StartParams {
        device,
        sync_latency_ms: settings
            .profile
            .sync_latency_ms(settings.custom_sync_latency_ms),
        volume_pct: settings.volume_pct,
        send_now_playing: settings.send_now_playing,
        remote_control: settings.remote_control,
        capture_mode: settings.capture_mode,
    })
}

/// Which tray the model is rendered for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlatform {
    /// ksni asks for the menu whenever the host shows it, so live stats are cheap.
    Linux,
    /// muda menus are rebuilt only when the model changes, and a rebuild replaces the menu
    /// ids; a status row ticking every second would rebuild the menu under the user's
    /// cursor. Adds the capture-mode checkbox.
    Windows,
}

/// Builds the menu model tree representing the tray right-click menu.
pub fn build_menu_model(
    snap: &engine::Shared,
    settings: &Settings,
    autostart_enabled: bool,
    lang: Lang,
    platform: MenuPlatform,
) -> Vec<TrayMenuItem> {
    let include_capture_mode = platform == MenuPlatform::Windows;
    let active = matches!(snap.state, State::Streaming | State::Connecting);
    let has_device = selected_device(&snap.devices, settings).is_some();

    // 1. Status row (disabled)
    let status_text = match &snap.state {
        State::Idle => t(lang, Key::StateIdle).to_string(),
        State::Connecting => t(lang, Key::StateConnecting).to_string(),
        State::Streaming if platform == MenuPlatform::Windows => {
            t(lang, Key::StateStreaming).to_string()
        }
        State::Streaming => {
            let secs = snap.stats.since.map(|s| s.elapsed().as_secs()).unwrap_or(0);
            let cols = format_stats_columns(lang, secs, snap.stats.drift_ppm);
            format!(
                "{} — {} | {}",
                t(lang, Key::StateStreaming),
                cols[0],
                cols[1]
            )
        }
        State::Error(err) => {
            format!(
                "{}: {}",
                t(lang, Key::TrayTooltipError),
                // Short: a menu is as wide as its longest item.
                engine_error_title(lang, err)
            )
        }
    };
    let status_item = TrayMenuItem {
        label: status_text,
        enabled: false,
        kind: MenuItemKind::Standard,
        action: None,
    };

    // 2. Start / Stop item
    let (start_stop_label, can_toggle) = match snap.state {
        State::Connecting | State::Streaming => (t(lang, Key::TrayStop), true),
        State::Idle | State::Error(_) => (t(lang, Key::TrayStart), has_device),
    };
    let start_stop_item = TrayMenuItem {
        label: start_stop_label.to_string(),
        enabled: can_toggle,
        kind: MenuItemKind::Standard,
        action: Some(TrayMenuAction::ToggleStreaming),
    };

    // 3. Separator
    let sep1 = TrayMenuItem {
        label: String::new(),
        enabled: true,
        kind: MenuItemKind::Separator,
        action: None,
    };

    // 4. Target device Submenu
    let device_submenu_items: Vec<TrayMenuItem> = if snap.devices.is_empty() {
        vec![TrayMenuItem {
            label: t(lang, Key::NoDevicesDiscovered).to_string(),
            enabled: false,
            kind: MenuItemKind::Radio { selected: false },
            action: None,
        }]
    } else {
        snap.devices
            .iter()
            .map(|d| {
                let selected = settings.device_name.as_deref() == Some(&d.name)
                    || settings.device_addr.as_deref() == Some(&d.addr.to_string());
                TrayMenuItem {
                    label: format!("{} ({})", d.name, d.addr.ip()),
                    enabled: !active,
                    kind: MenuItemKind::Radio { selected },
                    action: Some(TrayMenuAction::SelectDevice(d.clone())),
                }
            })
            .collect()
    };
    let target_device_menu = TrayMenuItem {
        label: t(lang, Key::TargetDevice).to_string(),
        enabled: true,
        kind: MenuItemKind::Submenu(device_submenu_items),
        action: None,
    };

    // 5. Latency profile Submenu
    let profile_profiles = [
        (Profile::Low, Key::ProfileLow),
        (Profile::Balanced, Key::ProfileBalanced),
        (Profile::Stable, Key::ProfileStable),
    ];
    let mut profile_options: Vec<TrayMenuItem> = profile_profiles
        .iter()
        .map(|(p, key)| TrayMenuItem {
            label: t(lang, *key).to_string(),
            enabled: !active,
            kind: MenuItemKind::Radio {
                selected: settings.profile == *p,
            },
            action: Some(TrayMenuAction::SelectProfile(*p)),
        })
        .collect();
    if settings.profile == Profile::Custom {
        profile_options.push(TrayMenuItem {
            label: format!(
                "{}: {} ms",
                t(lang, Key::ProfileCustom),
                settings.custom_sync_latency_ms
            ),
            enabled: false,
            kind: MenuItemKind::Radio { selected: true },
            action: None,
        });
    }
    let profile_menu = TrayMenuItem {
        label: t(lang, Key::LatencyProfile).to_string(),
        enabled: true,
        kind: MenuItemKind::Submenu(profile_options),
        action: None,
    };

    // 6. Volume presets Submenu
    let selected_vol_idx = VOLUME_PRESETS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let diff_a = (settings.volume_pct - **a).abs();
            let diff_b = (settings.volume_pct - **b).abs();
            diff_a
                .partial_cmp(&diff_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(1);

    let current_vol_item = TrayMenuItem {
        label: match lang {
            Lang::ZhCn => format!("当前: {:.0}%", settings.volume_pct),
            Lang::En => format!("Current: {:.0}%", settings.volume_pct),
        },
        enabled: false,
        kind: MenuItemKind::Standard,
        action: None,
    };
    let mut volume_items = vec![current_vol_item];
    for (i, &preset) in VOLUME_PRESETS.iter().enumerate() {
        volume_items.push(TrayMenuItem {
            label: format!("{:.0}%", preset),
            enabled: true,
            kind: MenuItemKind::Radio {
                selected: i == selected_vol_idx,
            },
            action: Some(TrayMenuAction::SetVolume(preset)),
        });
    }
    let volume_menu = TrayMenuItem {
        label: t(lang, Key::Volume).to_string(),
        enabled: true,
        kind: MenuItemKind::Submenu(volume_items),
        action: None,
    };

    // 7. Language SubMenu
    let lang_options = vec![
        TrayMenuItem {
            label: t(lang, Key::LangAuto).to_string(),
            enabled: true,
            kind: MenuItemKind::Radio {
                selected: settings.language == LanguageChoice::Auto,
            },
            action: Some(TrayMenuAction::SelectLanguage(LanguageChoice::Auto)),
        },
        TrayMenuItem {
            label: t(lang, Key::LangZhCn).to_string(),
            enabled: true,
            kind: MenuItemKind::Radio {
                selected: settings.language == LanguageChoice::ZhCn,
            },
            action: Some(TrayMenuAction::SelectLanguage(LanguageChoice::ZhCn)),
        },
        TrayMenuItem {
            label: t(lang, Key::LangEn).to_string(),
            enabled: true,
            kind: MenuItemKind::Radio {
                selected: settings.language == LanguageChoice::En,
            },
            action: Some(TrayMenuAction::SelectLanguage(LanguageChoice::En)),
        },
    ];
    let language_menu = TrayMenuItem {
        label: t(lang, Key::TrayLanguage).to_string(),
        enabled: true,
        kind: MenuItemKind::Submenu(lang_options),
        action: None,
    };

    // 8. Send now playing item
    let send_now_playing_item = TrayMenuItem {
        label: t(lang, Key::SendNowPlaying).to_string(),
        enabled: true,
        kind: MenuItemKind::Checkbox {
            checked: settings.send_now_playing,
        },
        action: Some(TrayMenuAction::ToggleSendNowPlaying),
    };

    // 9. Remote control item
    let remote_control_item = TrayMenuItem {
        label: t(lang, Key::RemoteControl).to_string(),
        enabled: true,
        kind: MenuItemKind::Checkbox {
            checked: settings.remote_control,
        },
        action: Some(TrayMenuAction::ToggleRemoteControl),
    };

    // 10. Resume on launch item
    let resume_item = TrayMenuItem {
        label: t(lang, Key::ResumeOnLaunch).to_string(),
        enabled: true,
        kind: MenuItemKind::Checkbox {
            checked: settings.resume_on_launch,
        },
        action: Some(TrayMenuAction::ToggleResumeOnLaunch),
    };

    // 11. Autostart item
    let autostart_item = TrayMenuItem {
        label: t(lang, Key::Autostart).to_string(),
        enabled: true,
        kind: MenuItemKind::Checkbox {
            checked: autostart_enabled,
        },
        action: Some(TrayMenuAction::ToggleAutostart),
    };

    let mut items = vec![
        status_item,
        start_stop_item,
        sep1,
        target_device_menu,
        profile_menu,
        volume_menu,
        language_menu,
        send_now_playing_item,
        remote_control_item,
        resume_item,
        autostart_item,
    ];

    // 12. Windows capture mode item (optional)
    if include_capture_mode {
        items.push(TrayMenuItem {
            label: t(lang, Key::CaptureModeEndpoint).to_string(),
            enabled: true,
            kind: MenuItemKind::Checkbox {
                checked: settings.capture_mode == CaptureMode::Endpoint,
            },
            action: Some(TrayMenuAction::ToggleCaptureMode),
        });
    }

    // Version (disabled). Windows shows it in the flyout; Linux has only this menu.
    if platform == MenuPlatform::Linux {
        items.push(TrayMenuItem {
            label: concat!("Nyx Refrain ", env!("NYX_VERSION")).to_string(),
            enabled: false,
            kind: MenuItemKind::Standard,
            action: None,
        });
    }

    // 13. Separator
    items.push(TrayMenuItem {
        label: String::new(),
        enabled: true,
        kind: MenuItemKind::Separator,
        action: None,
    });

    // 14. Quit item
    items.push(TrayMenuItem {
        label: t(lang, Key::TrayQuit).to_string(),
        enabled: true,
        kind: MenuItemKind::Standard,
        action: Some(TrayMenuAction::Quit),
    });

    items
}

/// Applies an action selected from the tray menu.
pub fn apply_action<F>(
    action: &TrayMenuAction,
    engine: &Engine,
    settings: &Arc<Mutex<Settings>>,
    on_quit: F,
) where
    F: FnOnce(),
{
    match action {
        TrayMenuAction::ToggleStreaming => {
            let snap = engine.snapshot();
            match snap.state {
                State::Connecting | State::Streaming => {
                    engine.send(Cmd::Stop);
                }
                State::Idle | State::Error(_) => {
                    let s = match settings.lock() {
                        Ok(s) => s.clone(),
                        Err(_) => return,
                    };
                    if let Some(params) = build_start_params(&snap.devices, &s) {
                        engine.send(Cmd::Start(params));
                    }
                }
            }
        }
        TrayMenuAction::SelectDevice(device) => {
            if let Ok(mut s) = settings.lock() {
                s.device_name = Some(device.name.clone());
                s.device_addr = Some(device.addr.to_string());
                s.save();
            }
        }
        TrayMenuAction::SelectProfile(profile) => {
            if let Ok(mut s) = settings.lock() {
                s.profile = *profile;
                s.save();
            }
        }
        TrayMenuAction::SetVolume(vol) => {
            if let Ok(mut s) = settings.lock() {
                s.volume_pct = *vol;
                s.save();
            }
            engine.send(Cmd::SetVolume(*vol));
        }
        TrayMenuAction::SelectLanguage(choice) => {
            if let Ok(mut s) = settings.lock() {
                s.language = *choice;
                s.save();
            }
        }
        TrayMenuAction::ToggleSendNowPlaying => {
            let new_state = if let Ok(mut s) = settings.lock() {
                s.send_now_playing = !s.send_now_playing;
                s.save();
                s.send_now_playing
            } else {
                return;
            };
            engine.send(Cmd::SetSendNowPlaying(new_state));
        }
        TrayMenuAction::ToggleRemoteControl => {
            let new_state = if let Ok(mut s) = settings.lock() {
                s.remote_control = !s.remote_control;
                s.save();
                s.remote_control
            } else {
                return;
            };
            engine.send(Cmd::SetRemoteControl(new_state));
        }
        TrayMenuAction::ToggleResumeOnLaunch => {
            if let Ok(mut s) = settings.lock() {
                s.resume_on_launch = !s.resume_on_launch;
                s.save();
            }
        }
        TrayMenuAction::ToggleAutostart => {
            let current = crate::autostart::is_enabled().unwrap_or(false);
            if let Err(e) = crate::autostart::set_enabled(!current) {
                eprintln!("autostart toggle failed: {e}");
            }
        }
        TrayMenuAction::ToggleCaptureMode => {
            let snap = engine.snapshot();
            let restart_params = if let Ok(mut s) = settings.lock() {
                let endpoint = s.capture_mode == CaptureMode::Endpoint;
                s.capture_mode = if endpoint {
                    CaptureMode::Process
                } else {
                    CaptureMode::Endpoint
                };
                s.save();
                if snap.state == State::Streaming {
                    build_start_params(&snap.devices, &s)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(params) = restart_params {
                engine.send(Cmd::Start(params));
            }
        }
        TrayMenuAction::Quit => {
            on_quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_addr_valid_and_fallback() {
        assert_eq!(
            parse_addr("192.168.1.10:7000"),
            Some("192.168.1.10:7000".parse().unwrap())
        );
        assert_eq!(
            parse_addr("192.0.2.106"),
            Some("192.0.2.106:7000".parse().unwrap())
        );
        assert_eq!(parse_addr("   "), None);
        assert_eq!(parse_addr("invalid-address"), None);
    }

    #[test]
    fn selected_device_prefers_discovered_then_manual() {
        let snap_devices = vec![DeviceEntry {
            name: "HomePod".into(),
            addr: "192.0.2.100:7000".parse().unwrap(),
            features: None,
        }];
        // 1. Matched by name
        let settings = Settings {
            device_name: Some("HomePod".into()),
            device_addr: Some("192.0.2.200:7000".into()),
            ..Default::default()
        };
        let d = selected_device(&snap_devices, &settings).unwrap();
        assert_eq!(d.addr, "192.0.2.100:7000".parse().unwrap());

        // 2. Matched by fallback addr
        let settings = Settings {
            device_name: Some("Other".into()),
            device_addr: Some("192.0.2.200:7000".into()),
            ..Default::default()
        };
        let d = selected_device(&snap_devices, &settings).unwrap();
        assert_eq!(d.name, "Other");
        assert_eq!(d.addr, "192.0.2.200:7000".parse().unwrap());

        // 3. Neither matched
        let settings = Settings::default();
        assert!(selected_device(&snap_devices, &settings).is_none());
    }

    #[test]
    fn build_menu_model_empty_devices() {
        let snap = engine::Shared::default();
        let settings = Settings::default();
        let items = build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Linux);

        // Status row
        assert_eq!(items[0].label, t(Lang::ZhCn, Key::StateIdle));
        assert!(!items[0].enabled);

        // Start / Stop: disabled because no device is selected
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStart));
        assert!(!items[1].enabled);

        // Device submenu has "No devices discovered" item
        let device_sub = &items[3];
        if let MenuItemKind::Submenu(sub) = &device_sub.kind {
            assert_eq!(sub.len(), 1);
            assert_eq!(sub[0].label, t(Lang::ZhCn, Key::NoDevicesDiscovered));
            assert!(!sub[0].enabled);
            assert_eq!(sub[0].kind, MenuItemKind::Radio { selected: false });
        } else {
            panic!("expected Submenu");
        }
    }

    #[test]
    fn build_menu_model_populated_devices_and_selection() {
        let snap = engine::Shared {
            devices: vec![
                DeviceEntry {
                    name: "Living Room".into(),
                    addr: "192.0.2.101:7000".parse().unwrap(),
                    features: None,
                },
                DeviceEntry {
                    name: "Kitchen".into(),
                    addr: "192.0.2.102:7000".parse().unwrap(),
                    features: None,
                },
            ],
            ..Default::default()
        };
        let settings = Settings {
            device_name: Some("Kitchen".into()),
            ..Default::default()
        };

        let items = build_menu_model(&snap, &settings, true, Lang::En, MenuPlatform::Windows);

        // Start button is now enabled
        assert!(items[1].enabled);
        assert_eq!(items[1].label, t(Lang::En, Key::TrayStart));

        // Device submenu contains 2 entries, Kitchen is selected
        let device_sub = &items[3];
        if let MenuItemKind::Submenu(sub) = &device_sub.kind {
            assert_eq!(sub.len(), 2);
            assert_eq!(sub[0].label, "Living Room (192.0.2.101)");
            assert_eq!(sub[0].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[1].label, "Kitchen (192.0.2.102)");
            assert_eq!(sub[1].kind, MenuItemKind::Radio { selected: true });
        } else {
            panic!("expected Submenu");
        }

        // Profile submenu follows settings.profile
        let profile_sub = &items[4];
        if let MenuItemKind::Submenu(sub) = &profile_sub.kind {
            assert_eq!(sub.len(), 3);
            // Default profile is Balanced (index 1)
            assert_eq!(sub[0].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[1].kind, MenuItemKind::Radio { selected: true });
            assert_eq!(sub[2].kind, MenuItemKind::Radio { selected: false });
        } else {
            panic!("expected Submenu");
        }

        // Volume submenu
        let vol_sub = &items[5];
        if let MenuItemKind::Submenu(sub) = &vol_sub.kind {
            assert_eq!(sub.len(), 5); // 1 current + 4 presets
            assert_eq!(sub[0].label, "Current: 50%");
            assert!(!sub[0].enabled);
            // 50% preset is index 2 in presets (25%, 50%, 75%, 100%) -> sub[2]
            assert_eq!(sub[2].kind, MenuItemKind::Radio { selected: true });
        } else {
            panic!("expected Submenu");
        }

        // Checkboxes: send now-playing (true), remote control (true), resume (true), autostart (true), capture mode (false)
        assert_eq!(items[7].kind, MenuItemKind::Checkbox { checked: true });
        assert_eq!(items[8].kind, MenuItemKind::Checkbox { checked: true });
        assert_eq!(items[9].kind, MenuItemKind::Checkbox { checked: true }); // resume_on_launch
        assert_eq!(items[10].kind, MenuItemKind::Checkbox { checked: true }); // autostart
        assert_eq!(items[11].kind, MenuItemKind::Checkbox { checked: false }); // capture_mode endpoint
    }

    #[test]
    fn build_menu_model_streaming_state() {
        let snap = engine::Shared {
            state: State::Streaming,
            ..Default::default()
        };
        let settings = Settings::default();

        let items = build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Linux);

        // Status row indicates streaming
        assert!(items[0].label.contains(t(Lang::ZhCn, Key::StateStreaming)));

        // Start / Stop is now Stop
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStop));
        assert!(items[1].enabled);

        // Device and Profile options are disabled while active
        if let MenuItemKind::Submenu(sub) = &items[4].kind {
            assert!(!sub[0].enabled);
        }
    }

    #[test]
    fn windows_status_row_is_stable_while_streaming() {
        // The Windows menu is rebuilt whenever the model changes, so it must not tick.
        let snap = engine::Shared {
            state: State::Streaming,
            ..Default::default()
        };
        let settings = Settings::default();
        let items = build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Windows);
        assert_eq!(items[0].label, t(Lang::ZhCn, Key::StateStreaming));
        let linux = build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Linux);
        assert_ne!(linux[0].label, items[0].label);
    }

    #[test]
    fn test_radio_states_profile_and_language() {
        let snap = engine::Shared::default();
        let settings = Settings {
            profile: Profile::Custom,
            custom_sync_latency_ms: -120,
            language: LanguageChoice::En,
            ..Default::default()
        };

        let items = build_menu_model(&snap, &settings, false, Lang::En, MenuPlatform::Linux);

        // Profile submenu: Custom is 4th item, selected and disabled
        if let MenuItemKind::Submenu(sub) = &items[4].kind {
            assert_eq!(sub.len(), 4);
            assert_eq!(sub[0].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[1].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[2].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[3].kind, MenuItemKind::Radio { selected: true });
            assert!(!sub[3].enabled);
            assert_eq!(sub[3].label, "Custom: -120 ms");
        } else {
            panic!("expected Submenu");
        }

        // Language submenu: English is 3rd item, selected
        if let MenuItemKind::Submenu(sub) = &items[6].kind {
            assert_eq!(sub.len(), 3);
            assert_eq!(sub[0].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[1].kind, MenuItemKind::Radio { selected: false });
            assert_eq!(sub[2].kind, MenuItemKind::Radio { selected: true });
        } else {
            panic!("expected Submenu");
        }
    }

    #[test]
    fn test_volume_preset_selection_nearest() {
        let snap = engine::Shared::default();

        // 33% is closest to 25% (diff 8) vs 50% (diff 17)
        let settings_33 = Settings {
            volume_pct: 33.0,
            ..Default::default()
        };
        let items = build_menu_model(&snap, &settings_33, false, Lang::ZhCn, MenuPlatform::Linux);
        if let MenuItemKind::Submenu(sub) = &items[5].kind {
            assert_eq!(sub[0].label, "当前: 33%");
            assert_eq!(sub[1].kind, MenuItemKind::Radio { selected: true }); // 25%
            assert_eq!(sub[2].kind, MenuItemKind::Radio { selected: false }); // 50%
        } else {
            panic!("expected Submenu");
        }

        // 80% is closest to 75% (diff 5) vs 100% (diff 20)
        let settings_80 = Settings {
            volume_pct: 80.0,
            ..Default::default()
        };
        let items = build_menu_model(&snap, &settings_80, false, Lang::En, MenuPlatform::Linux);
        if let MenuItemKind::Submenu(sub) = &items[5].kind {
            assert_eq!(sub[0].label, "Current: 80%");
            assert_eq!(sub[3].kind, MenuItemKind::Radio { selected: true }); // 75%
        } else {
            panic!("expected Submenu");
        }
    }

    #[test]
    fn test_status_and_start_stop_per_state() {
        // 1. Idle without device
        let snap = engine::Shared::default();
        let settings = Settings::default();
        let items = build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Linux);
        assert_eq!(items[0].label, t(Lang::ZhCn, Key::StateIdle));
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStart));
        assert!(!items[1].enabled);

        // 2. Idle with device
        let settings_dev = Settings {
            device_name: Some("Living Room".into()),
            ..Default::default()
        };
        let snap_dev = engine::Shared {
            devices: vec![DeviceEntry {
                name: "Living Room".into(),
                addr: "192.0.2.106:7000".parse().unwrap(),
                features: None,
            }],
            ..Default::default()
        };
        let items = build_menu_model(
            &snap_dev,
            &settings_dev,
            false,
            Lang::ZhCn,
            MenuPlatform::Linux,
        );
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStart));
        assert!(items[1].enabled);

        // 3. Connecting
        let snap_conn = engine::Shared {
            state: State::Connecting,
            ..snap_dev.clone()
        };
        let items = build_menu_model(
            &snap_conn,
            &settings_dev,
            false,
            Lang::ZhCn,
            MenuPlatform::Linux,
        );
        assert_eq!(items[0].label, t(Lang::ZhCn, Key::StateConnecting));
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStop));
        assert!(items[1].enabled);

        // 4. Error
        let snap_err = engine::Shared {
            state: State::Error(crate::engine::EngineError::Connect("timeout".into())),
            ..snap_dev
        };
        let items = build_menu_model(
            &snap_err,
            &settings_dev,
            false,
            Lang::ZhCn,
            MenuPlatform::Linux,
        );
        assert!(
            items[0]
                .label
                .contains(t(Lang::ZhCn, Key::TrayTooltipError))
        );
        // Only the short title: the raw error would widen the whole menu.
        assert!(items[0].label.contains("连接失败"));
        assert!(!items[0].label.contains("timeout"));
        assert_eq!(items[1].label, t(Lang::ZhCn, Key::TrayStart));
        assert!(items[1].enabled);
    }

    #[test]
    fn test_capture_mode_presence() {
        let snap = engine::Shared::default();
        let settings = Settings {
            capture_mode: CaptureMode::Endpoint,
            ..Default::default()
        };

        // On Linux (include_capture_mode = false): no capture mode item but a version row,
        // total 14 items
        let linux_items =
            build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Linux);
        assert!(
            !linux_items
                .iter()
                .any(|it| it.action == Some(TrayMenuAction::ToggleCaptureMode))
        );
        assert_eq!(linux_items.len(), 14);
        assert!(
            linux_items[linux_items.len() - 3]
                .label
                .starts_with("Nyx Refrain v")
        );

        // On Windows (include_capture_mode = true): has capture mode item, total 14 items
        let win_items =
            build_menu_model(&snap, &settings, false, Lang::ZhCn, MenuPlatform::Windows);
        let cap_item = win_items
            .iter()
            .find(|it| it.action == Some(TrayMenuAction::ToggleCaptureMode));
        assert!(cap_item.is_some());
        assert_eq!(
            cap_item.unwrap().kind,
            MenuItemKind::Checkbox { checked: true }
        );
        assert_eq!(win_items.len(), 14);
    }

    #[test]
    fn test_apply_action_updates_settings() {
        let engine = Engine::spawn(Arc::new(|| {}));
        let settings = Arc::new(Mutex::new(Settings::default()));

        // Profile action
        apply_action(
            &TrayMenuAction::SelectProfile(Profile::Low),
            &engine,
            &settings,
            || {},
        );
        assert_eq!(settings.lock().unwrap().profile, Profile::Low);

        // Language action
        apply_action(
            &TrayMenuAction::SelectLanguage(LanguageChoice::En),
            &engine,
            &settings,
            || {},
        );
        assert_eq!(settings.lock().unwrap().language, LanguageChoice::En);

        // Volume action
        apply_action(&TrayMenuAction::SetVolume(75.0), &engine, &settings, || {});
        assert_eq!(settings.lock().unwrap().volume_pct, 75.0);

        // Checkbox actions
        apply_action(
            &TrayMenuAction::ToggleSendNowPlaying,
            &engine,
            &settings,
            || {},
        );
        assert!(!settings.lock().unwrap().send_now_playing);

        apply_action(
            &TrayMenuAction::ToggleRemoteControl,
            &engine,
            &settings,
            || {},
        );
        assert!(!settings.lock().unwrap().remote_control);

        apply_action(
            &TrayMenuAction::ToggleResumeOnLaunch,
            &engine,
            &settings,
            || {},
        );
        assert!(!settings.lock().unwrap().resume_on_launch);

        apply_action(
            &TrayMenuAction::ToggleCaptureMode,
            &engine,
            &settings,
            || {},
        );
        assert_eq!(settings.lock().unwrap().capture_mode, CaptureMode::Endpoint);

        // Quit action
        let quit_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let q_clone = quit_called.clone();
        apply_action(&TrayMenuAction::Quit, &engine, &settings, move || {
            q_clone.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        assert!(quit_called.load(std::sync::atomic::Ordering::Relaxed));

        engine.send(Cmd::Shutdown);
    }
}
