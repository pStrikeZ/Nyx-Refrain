//! Linux tray front-end implementing StatusNotifierItem via ksni.
#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip, Tray, TrayMethods};

use crate::engine::{Cmd, Engine, State};
use crate::i18n::{Key, Lang, format_engine_error, format_stats_columns, t};
use crate::settings::Settings;
use crate::single_instance;
use crate::tray_menu::{self, MenuItemKind, TrayMenuAction, TrayMenuItem};

struct LinuxTray {
    engine: Engine,
    settings: Arc<Mutex<Settings>>,
    shutdown_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

impl LinuxTray {
    fn current_lang(&self) -> Lang {
        self.settings
            .lock()
            .map(|s| s.language.resolve())
            .unwrap_or_else(|_| crate::i18n::detect_system_language())
    }
}

impl Tray for LinuxTray {
    fn id(&self) -> String {
        "nyx-refrain".into()
    }

    fn title(&self) -> String {
        "Nyx Refrain".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let snap = self.engine.snapshot();
        crate::icons::for_state(&snap.state)
            .iter()
            .map(|icon| Icon {
                width: icon.size as i32,
                height: icon.size as i32,
                // SNI uses ARGB32 in network byte order, while the assets are RGBA.
                data: icon
                    .rgba
                    .chunks_exact(4)
                    .flat_map(|p| [p[3], p[0], p[1], p[2]])
                    .collect(),
            })
            .collect()
    }

    fn icon_name(&self) -> String {
        // SNI hosts prefer names over pixmaps. Only the streaming state matches
        // the installed shooting star; the other states must use their pixmaps.
        if matches!(self.engine.snapshot().state, State::Streaming)
            && std::path::Path::new("/usr/share/icons/hicolor/32x32/apps/nyx-refrain.png").is_file()
        {
            "nyx-refrain".into()
        } else {
            String::new()
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let snap = self.engine.snapshot();
        let lang = self.current_lang();
        let description = match snap.state {
            State::Idle => t(lang, Key::TrayTooltipIdle).to_string(),
            State::Connecting => t(lang, Key::TrayTooltipConnecting).to_string(),
            State::Streaming => {
                let secs = snap.stats.since.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                let cols = format_stats_columns(lang, secs, snap.stats.drift_ppm);
                format!(
                    "{}\n{} | {}",
                    t(lang, Key::TrayTooltipStreaming),
                    cols[0],
                    cols[1]
                )
            }
            State::Error(ref err) => {
                format!(
                    "{}: {}",
                    t(lang, Key::TrayTooltipError),
                    format_engine_error(lang, err)
                )
            }
        };
        ToolTip {
            title: concat!("Nyx Refrain ", env!("NYX_VERSION")).into(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        tray_menu::apply_action(
            &TrayMenuAction::ToggleStreaming,
            &self.engine,
            &self.settings,
            || {},
        );
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let snap = self.engine.snapshot();
        let settings = self.settings.lock().map(|s| s.clone()).unwrap_or_default();
        let lang = settings.language.resolve();
        let autostart = crate::autostart::is_enabled().unwrap_or(false);
        let model = tray_menu::build_menu_model(
            &snap,
            &settings,
            autostart,
            lang,
            tray_menu::MenuPlatform::Linux,
        );
        render_ksni_items(&model, &self.engine, &self.settings, &self.shutdown_tx)
    }
}

fn render_ksni_items(
    items: &[TrayMenuItem],
    engine: &Engine,
    settings: &Arc<Mutex<Settings>>,
    shutdown_tx: &tokio::sync::mpsc::UnboundedSender<()>,
) -> Vec<MenuItem<LinuxTray>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < items.len() {
        match &items[i].kind {
            MenuItemKind::Radio { .. } => {
                let mut options = Vec::new();
                let mut actions = Vec::new();
                let mut selected_idx = usize::MAX;
                let start = i;
                while i < items.len() {
                    if let MenuItemKind::Radio { selected } = &items[i].kind {
                        if *selected && selected_idx == usize::MAX {
                            selected_idx = i - start;
                        }
                        options.push(RadioItem {
                            label: items[i].label.clone(),
                            enabled: items[i].enabled,
                            ..Default::default()
                        });
                        actions.push(items[i].action.clone());
                        i += 1;
                    } else {
                        break;
                    }
                }
                let eng = engine.clone();
                let sett = settings.clone();
                let sht = shutdown_tx.clone();
                result.push(
                    RadioGroup {
                        selected: selected_idx,
                        select: Box::new(move |_this, idx| {
                            if let Some(Some(a)) = actions.get(idx) {
                                let s_tx = sht.clone();
                                tray_menu::apply_action(a, &eng, &sett, move || {
                                    let _ = s_tx.send(());
                                });
                            }
                        }),
                        options,
                    }
                    .into(),
                );
            }
            MenuItemKind::Standard => {
                let act = items[i].action.clone();
                let eng = engine.clone();
                let sett = settings.clone();
                let sht = shutdown_tx.clone();
                result.push(
                    StandardItem {
                        label: items[i].label.clone(),
                        enabled: items[i].enabled,
                        activate: Box::new(move |_this| {
                            if let Some(a) = &act {
                                let s_tx = sht.clone();
                                tray_menu::apply_action(a, &eng, &sett, move || {
                                    let _ = s_tx.send(());
                                });
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
                i += 1;
            }
            MenuItemKind::Checkbox { checked } => {
                let act = items[i].action.clone();
                let eng = engine.clone();
                let sett = settings.clone();
                let sht = shutdown_tx.clone();
                result.push(
                    CheckmarkItem {
                        label: items[i].label.clone(),
                        checked: *checked,
                        enabled: items[i].enabled,
                        activate: Box::new(move |_this| {
                            if let Some(a) = &act {
                                let s_tx = sht.clone();
                                tray_menu::apply_action(a, &eng, &sett, move || {
                                    let _ = s_tx.send(());
                                });
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
                i += 1;
            }
            MenuItemKind::Submenu(children) => {
                let sub_items = render_ksni_items(children, engine, settings, shutdown_tx);
                result.push(
                    SubMenu {
                        label: items[i].label.clone(),
                        enabled: items[i].enabled,
                        submenu: sub_items,
                        ..Default::default()
                    }
                    .into(),
                );
                i += 1;
            }
            MenuItemKind::Separator => {
                result.push(MenuItem::Separator);
                i += 1;
            }
        }
    }
    result
}

/// Checks if `org.kde.StatusNotifierWatcher` is present on the session bus.
/// If absent, logs a warning and sends a desktop notification via `org.freedesktop.Notifications.Notify`.
async fn check_watcher_and_notify(lang: Lang) {
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(watcher_name) = "org.kde.StatusNotifierWatcher".try_into() else {
        return;
    };
    let has_watcher = dbus.name_has_owner(watcher_name).await.unwrap_or_default();
    if !has_watcher {
        eprintln!(
            "nyx-refrain: warning: org.kde.StatusNotifierWatcher is not present on D-Bus session bus. System tray may not appear without the 'AppIndicator and KStatusNotifierItem Support' GNOME extension."
        );
        let (summary, body) = match lang {
            Lang::ZhCn => (
                "Nyx Refrain 托盘提示",
                "系统未检测到 StatusNotifierWatcher。如果使用 GNOME 桌面，请安装并启用“AppIndicator and KStatusNotifierItem Support”扩展以显示托盘图标。",
            ),
            Lang::En => (
                "Nyx Refrain Tray Notice",
                "StatusNotifierWatcher was not found on D-Bus. If you are using GNOME, please install and enable the 'AppIndicator and KStatusNotifierItem Support' extension to view the tray icon.",
            ),
        };
        let empty_actions: [&str; 0] = [];
        let empty_hints: std::collections::HashMap<&str, zbus::zvariant::Value> =
            std::collections::HashMap::new();
        let _ = conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Nyx Refrain",
                    0u32,
                    "dialog-warning",
                    summary,
                    body,
                    empty_actions,
                    empty_hints,
                    10000i32,
                ),
            )
            .await;
    }
}

pub fn run() -> anyhow::Result<()> {
    // 1. Single instance check: acquire flock on $XDG_RUNTIME_DIR/nyx-refrain.lock
    let Some(_instance) = single_instance::acquire() else {
        println!("nyx-refrain: another instance is already running; exiting");
        return Ok(());
    };

    // 2. Build multi-thread tokio runtime for tray service and async tasks
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let settings = Arc::new(Mutex::new(Settings::load()));
    let initial_lang = settings
        .lock()
        .map(|s| s.language.resolve())
        .unwrap_or_else(|_| crate::i18n::detect_system_language());

    // Check StatusNotifierWatcher on session bus
    check_watcher_and_notify(initial_lang).await;

    // Wakeup channel for the engine notifier to request tray updates
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let notifier = Arc::new(move || {
        let _ = wake_tx.send(());
    });

    let engine = Engine::spawn(notifier);
    engine.attach_settings(settings.clone());

    // Shutdown coordination channel
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let tray = LinuxTray {
        engine: engine.clone(),
        settings: settings.clone(),
        shutdown_tx,
    };

    let handle = tray.assume_sni_available(true).spawn().await?;

    // Background task listening for engine notifications to update tray properties/menu
    let handle_for_updates = handle.clone();
    let update_task = tokio::spawn(async move {
        while wake_rx.recv().await.is_some() {
            while wake_rx.try_recv().is_ok() {}
            let _ = handle_for_updates.update(|_| {}).await;
        }
    });

    // Handle OS signals for clean shutdown (SIGINT, SIGTERM)
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        _ = sigint.recv() => {
            eprintln!("nyx-refrain: received SIGINT, stopping...");
        }
        _ = sigterm.recv() => {
            eprintln!("nyx-refrain: received SIGTERM, stopping...");
        }
        _ = shutdown_rx.recv() => {
            eprintln!("nyx-refrain: quit requested, stopping...");
        }
    }

    // Clean shutdown: stop engine so the virtual sink is dropped and default sink restored.
    // Shutdown (not Stop) keeps the session to resume on next launch.
    engine.send(Cmd::Shutdown);
    let start_wait = Instant::now();
    while engine.snapshot().state != State::Idle && start_wait.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    update_task.abort();
    let _ = handle.shutdown().await;

    Ok(())
}
