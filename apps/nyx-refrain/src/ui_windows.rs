//! Windows tray flyout front-end.
#![cfg(windows)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::{self, Cmd, DeviceEntry, Engine, Firewall, StartParams, State};
use crate::i18n::{
    Key, Lang, engine_error_detail, engine_error_title, format_firewall_error,
    format_stats_columns, format_stats_detail, t,
};
use crate::position::{
    Pos, Rect, Size, fallback_position, flyout_position, monitor_work_area_and_dpi_at,
    primary_work_area_and_dpi,
};
use crate::settings::{CaptureMode, LanguageChoice, Profile, Settings};
use crate::single_instance;

pub const FLYOUT_WIDTH: f32 = 350.0;
/// Initial height before the first frame has been measured.
pub const FLYOUT_HEIGHT: f32 = 330.0;

/// Measured content height (logical px, f32 bits); the flyout is sized to its content so
/// there is no empty area, and re-anchored when it grows or shrinks.
static CONTENT_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn flyout_height() -> f32 {
    let h = f32::from_bits(CONTENT_HEIGHT.load(std::sync::atomic::Ordering::Relaxed));
    if h > 50.0 { h } else { FLYOUT_HEIGHT }
}

/// Start parameters the tray menu can use while the flyout is hidden.
type SharedParams = Arc<Mutex<Option<StartParams>>>;

#[derive(Clone, Debug)]
pub struct FlyoutState {
    pub visible: bool,
    pub opened_at: Instant,
    pub closed_at: Instant,
    pub has_been_focused: bool,
    /// Tray icon rect the flyout is anchored to (`None` = default bottom-right position).
    pub anchor: Option<Rect>,
    /// Height the window currently has.
    pub shown_height: f32,
    /// Desktop position to apply on the event-loop thread, in physical pixels.
    pending_position: Option<Pos>,
}

impl Default for FlyoutState {
    fn default() -> Self {
        Self {
            visible: false,
            opened_at: Instant::now(),
            closed_at: Instant::now() - Duration::from_secs(10),
            has_been_focused: false,
            anchor: None,
            shown_height: FLYOUT_HEIGHT,
            pending_position: None,
        }
    }
}

impl FlyoutState {
    pub fn just_closed(&self) -> bool {
        self.closed_at.elapsed() < Duration::from_millis(250)
    }

    pub fn show_at(ctx: &egui::Context, state: &mut FlyoutState, pos_phys: Pos) {
        // Keep desktop coordinates physical when crossing monitors with different DPI.
        state.pending_position = Some(pos_phys);
        let height = flyout_height();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            FLYOUT_WIDTH,
            height,
        )));
        state.shown_height = height;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        state.visible = true;
        state.opened_at = Instant::now();
        state.has_been_focused = false;
        ctx.request_repaint();
    }

    pub fn show_fallback(ctx: &egui::Context, state: &mut FlyoutState) {
        let (work_area, dpi_scale) = primary_work_area_and_dpi();
        state.anchor = None;
        let panel_phys = Size {
            width: (FLYOUT_WIDTH * dpi_scale).round() as u32,
            height: (flyout_height() * dpi_scale).round() as u32,
        };
        let pos_phys = fallback_position(work_area, panel_phys);
        Self::show_at(ctx, state, pos_phys);
    }

    pub fn show_from_tray_rect(ctx: &egui::Context, state: &mut FlyoutState, icon: Rect) {
        let (work_area, dpi_scale) = monitor_work_area_and_dpi_at((icon.x, icon.y));
        state.anchor = Some(icon);
        let panel_phys = Size {
            width: (FLYOUT_WIDTH * dpi_scale).round() as u32,
            height: (flyout_height() * dpi_scale).round() as u32,
        };
        let pos_phys = flyout_position(icon, work_area, panel_phys);
        Self::show_at(ctx, state, pos_phys);
    }

    /// Re-anchors and resizes a visible flyout after its content height changed.
    pub fn reflow(ctx: &egui::Context, state: &mut FlyoutState) {
        if !state.visible || (flyout_height() - state.shown_height).abs() <= 1.0 {
            return;
        }
        match state.anchor {
            Some(icon) => Self::show_from_tray_rect(ctx, state, icon),
            None => Self::show_fallback(ctx, state),
        }
    }

    pub fn hide(ctx: &egui::Context, state: &mut FlyoutState) {
        if state.visible {
            state.visible = false;
            state.closed_at = Instant::now();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.request_repaint();
        }
    }

    pub fn toggle_from_tray(ctx: &egui::Context, state: &mut FlyoutState, icon: Rect) {
        if state.just_closed() {
            return;
        }
        if state.visible {
            Self::hide(ctx, state);
        } else {
            Self::show_from_tray_rect(ctx, state, icon);
        }
    }
}

struct App {
    engine: Engine,
    settings: Arc<Mutex<Settings>>,
    manual_addr: String,
    params: SharedParams,
    flyout: Arc<Mutex<FlyoutState>>,
    tray: Option<tray::Tray>,
    /// DWM rounds the window (Windows 11). Without it (Windows 10) a rounded panel would
    /// leave the clear color showing in the square window's corners.
    rounded_corners: bool,
}

impl App {
    fn new(ctx: &egui::Context, autostart: bool) -> Self {
        install_cjk_font(ctx);
        configure_styles(ctx);

        let settings = Arc::new(Mutex::new(Settings::load()));
        let manual_addr = settings
            .lock()
            .unwrap()
            .device_addr
            .clone()
            .unwrap_or_default();
        let _initial_lang = settings.lock().unwrap().language.resolve();
        let egui_ctx = ctx.clone();
        let engine = Engine::spawn(Arc::new(move || egui_ctx.request_repaint()));
        engine.attach_settings(settings.clone());
        let params: SharedParams = Arc::new(Mutex::new(None));
        let flyout = Arc::new(Mutex::new(FlyoutState::default()));

        {
            let c = ctx.clone();
            let f = flyout.clone();
            single_instance::listen_for_show(move || {
                if let Ok(mut st) = f.lock() {
                    FlyoutState::show_fallback(&c, &mut st);
                }
            });
        }

        // On first start, position the flyout near the bottom-right and show it unless launched with --autostart.
        if !autostart {
            let mut st = flyout.lock().unwrap();
            FlyoutState::show_fallback(ctx, &mut st);
        }

        Self {
            tray: tray::Tray::new(
                ctx.clone(),
                engine.clone(),
                params.clone(),
                settings.clone(),
                flyout.clone(),
                _initial_lang,
            )
            .map_err(|e| eprintln!("tray: {e}"))
            .ok(),
            manual_addr,
            engine,
            settings,
            params,
            flyout,
            rounded_corners: false,
        }
    }

    fn start_params(&self, devices: &[DeviceEntry], settings: &Settings) -> Option<StartParams> {
        crate::tray_menu::build_start_params(devices, settings)
    }
}

fn parse_addr(s: &str) -> Option<SocketAddr> {
    crate::tray_menu::parse_addr(s)
}

/// Volume change (percent) for this frame's mouse-wheel events; up is louder.
fn wheel_volume_step(events: &[egui::Event]) -> f32 {
    events
        .iter()
        .map(|e| match e {
            egui::Event::MouseWheel { unit, delta, .. } => match unit {
                egui::MouseWheelUnit::Line => delta.y * 2.0,
                egui::MouseWheelUnit::Page => delta.y * 10.0,
                egui::MouseWheelUnit::Point => delta.y / 10.0,
            },
            _ => 0.0,
        })
        .sum()
}

const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x50, 0x50);

fn state_text(st: &State, lang: Lang) -> (egui::Color32, String) {
    match st {
        State::Idle => (egui::Color32::GRAY, t(lang, Key::StateIdle).into()),
        State::Connecting => (
            egui::Color32::from_rgb(0xe0, 0xa0, 0x30),
            t(lang, Key::StateConnecting).into(),
        ),
        State::Streaming => (
            egui::Color32::from_rgb(0x9c, 0x7c, 0xf4),
            t(lang, Key::StateStreaming).into(),
        ),
        State::Error(err) => (ERROR_COLOR, engine_error_title(lang, err).into()),
    }
}

impl App {
    fn logic(&mut self, ctx: &egui::Context) {
        {
            if ctx.input(|i| i.viewport().close_requested()) && !tray::quit_requested() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if let Ok(mut st) = self.flyout.lock() {
                    FlyoutState::hide(ctx, &mut st);
                }
            }
            if let Some(t) = &self.tray {
                let snap = self.engine.snapshot();
                let settings = self.settings.lock().map(|s| s.clone()).unwrap_or_default();
                let lang = settings.language.resolve();
                let autostart = crate::autostart::is_enabled().unwrap_or(false);
                t.update(&snap, &settings, autostart, lang);
            }
        }

        // Focus loss and Esc handling for the flyout.
        if let Ok(mut st) = self.flyout.lock()
            && st.visible
        {
            let focused = ctx.input(|i| i.viewport().focused);
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                FlyoutState::hide(ctx, &mut st);
            } else {
                if focused == Some(true) {
                    st.has_been_focused = true;
                }
                let elapsed = st.opened_at.elapsed();
                let debounce_ready = st.has_been_focused || elapsed >= Duration::from_millis(300);
                if debounce_ready && elapsed >= Duration::from_millis(150) && focused == Some(false)
                {
                    FlyoutState::hide(ctx, &mut st);
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let snap = self.engine.snapshot();
        let active = matches!(snap.state, State::Streaming | State::Connecting);
        let mut settings = self.settings.lock().unwrap().clone();
        let lang = settings.language.resolve();
        let mut settings_changed = false;

        if let Some(addr) = &settings.device_addr
            && &self.manual_addr != addr
            && !ui.memory(|m| m.has_focus(egui::Id::new("manual_addr_edit")))
        {
            self.manual_addr = addr.clone();
        }

        let panel_frame = egui::Frame::window(ui.style())
            .corner_radius(if self.rounded_corners { 8 } else { 0 })
            .stroke(egui::Stroke::new(1.0, ui.visuals().window_stroke.color))
            .inner_margin(egui::Margin::symmetric(14, 10));

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                // 1. Header row: App title + State dot/text + Language menu
                ui.horizontal(|ui| {
                    ui.heading(t(lang, Key::AppName));
                    ui.add_space(4.0);
                    let (color, text) = state_text(&snap.state, lang);
                    status_dot(ui, color);
                    ui.label(egui::RichText::new(text).color(color).strong());

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⚙", |ui| {
                            ui.label(egui::RichText::new(t(lang, Key::TrayLanguage)).strong());
                            if ui
                                .selectable_label(
                                    settings.language == LanguageChoice::Auto,
                                    t(lang, Key::LangAuto),
                                )
                                .clicked()
                            {
                                settings.language = LanguageChoice::Auto;
                                settings_changed = true;
                                ui.close();
                            }
                            if ui
                                .selectable_label(
                                    settings.language == LanguageChoice::ZhCn,
                                    "简体中文",
                                )
                                .clicked()
                            {
                                settings.language = LanguageChoice::ZhCn;
                                settings_changed = true;
                                ui.close();
                            }
                            if ui
                                .selectable_label(
                                    settings.language == LanguageChoice::En,
                                    "English",
                                )
                                .clicked()
                            {
                                settings.language = LanguageChoice::En;
                                settings_changed = true;
                                ui.close();
                            }

                            ui.separator();

                            let mut send_now_playing = settings.send_now_playing;
                            if ui
                                .checkbox(&mut send_now_playing, t(lang, Key::SendNowPlaying))
                                .changed()
                            {
                                settings.send_now_playing = send_now_playing;
                                settings_changed = true;
                                self.engine.send(Cmd::SetSendNowPlaying(send_now_playing));
                            }

                            let mut remote_control = settings.remote_control;
                            if ui
                                .checkbox(&mut remote_control, t(lang, Key::RemoteControl))
                                .changed()
                            {
                                settings.remote_control = remote_control;
                                settings_changed = true;
                                self.engine.send(Cmd::SetRemoteControl(remote_control));
                            }

                            let mut resume_on_launch = settings.resume_on_launch;
                            if ui
                                .checkbox(&mut resume_on_launch, t(lang, Key::ResumeOnLaunch))
                                .changed()
                            {
                                settings.resume_on_launch = resume_on_launch;
                                settings_changed = true;
                            }

                            let mut autostart_enabled =
                                crate::autostart::is_enabled().unwrap_or(false);
                            if ui
                                .checkbox(&mut autostart_enabled, t(lang, Key::Autostart))
                                .changed()
                                && let Err(e) = crate::autostart::set_enabled(autostart_enabled)
                            {
                                eprintln!("autostart toggle failed: {e}");
                            }

                            // Unchecked = process loopback (the default). The source is chosen at
                            // start, so a running stream is restarted to apply the change.
                            let mut endpoint = settings.capture_mode == CaptureMode::Endpoint;
                            if ui
                                .checkbox(&mut endpoint, t(lang, Key::CaptureModeEndpoint))
                                .on_hover_text(t(lang, Key::CaptureModeEndpointHint))
                                .changed()
                            {
                                settings.capture_mode = if endpoint {
                                    CaptureMode::Endpoint
                                } else {
                                    CaptureMode::Process
                                };
                                settings_changed = true;
                                if snap.state == State::Streaming
                                    && let Some(p) = self.start_params(&snap.devices, &settings)
                                {
                                    self.engine.send(Cmd::Start(p));
                                }
                            }
                        });
                    });
                });

                // The full error (often a raw OS message) wraps under the header instead of
                // widening the header row past the fixed-width flyout.
                if let State::Error(err) = &snap.state {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(engine_error_detail(lang, err))
                                .size(11.0)
                                .color(ERROR_COLOR),
                        )
                        .wrap(),
                    );
                }

                ui.add_space(4.0);

                // Firewall banner if needed
                firewall_banner(ui, &self.engine, &snap.firewall, &snap.state, lang);

                // 2. Device row: discovered combo + refresh button
                ui.horizontal(|ui| {
                    let current = settings
                        .device_name
                        .clone()
                        .unwrap_or_else(|| t(lang, Key::NoDeviceSelected).into());
                    // Right to left: the refresh button sits flush with the right edge like
                    // the header's language button; the combo takes the remaining width.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        ui.add_enabled_ui(!active, |ui| {
                            let refresh_label = if snap.discovering { "⏳" } else { "🔄" };
                            if ui
                                .add_enabled(!snap.discovering, egui::Button::new(refresh_label))
                                .on_hover_text(t(lang, Key::Refresh))
                                .clicked()
                            {
                                self.engine.send(Cmd::Discover);
                            }
                            egui::ComboBox::from_id_salt("device_combo")
                                .selected_text(&current)
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    for d in &snap.devices {
                                        let label = format!("{} ({})", d.name, d.addr.ip());
                                        let selected = settings.device_name.as_deref()
                                            == Some(d.name.as_str());
                                        if ui.selectable_label(selected, label).clicked() {
                                            settings.device_name = Some(d.name.clone());
                                            self.manual_addr = d.addr.to_string();
                                            settings_changed = true;
                                        }
                                    }
                                    if snap.devices.is_empty() {
                                        ui.weak(t(lang, Key::NoDevicesDiscovered));
                                    }
                                });
                        });
                    });
                });

                // Manual address disclosure
                egui::CollapsingHeader::new(t(lang, Key::ManualAddressDisclosure))
                    .id_salt("manual_addr_disclosure")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(t(lang, Key::DeviceAddress));
                            let r = ui.add_enabled(
                                !active,
                                egui::TextEdit::singleline(&mut self.manual_addr)
                                    .id_salt("manual_addr_edit")
                                    .hint_text(t(lang, Key::AddressHint))
                                    .desired_width(170.0),
                            );
                            if r.lost_focus() {
                                settings_changed = true;
                            }
                        });
                    });

                ui.add_space(4.0);

                // 3. Latency profile as segmented control
                ui.horizontal(|ui| {
                    let est_ms = 237
                        + settings
                            .profile
                            .sync_latency_ms(settings.custom_sync_latency_ms);
                    ui.label(t(lang, Key::LatencyProfile));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(format!("≈{est_ms} ms"));
                    });
                });

                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!active, |ui| {
                        for p in Profile::ALL {
                            let label = match p {
                                Profile::Low => t(lang, Key::ProfileLow),
                                Profile::Balanced => t(lang, Key::ProfileBalanced),
                                Profile::Stable => t(lang, Key::ProfileStable),
                                Profile::Custom => t(lang, Key::ProfileCustom),
                            };
                            if ui
                                .selectable_value(&mut settings.profile, p, label)
                                .changed()
                            {
                                settings_changed = true;
                            }
                        }
                        if settings.profile == Profile::Custom {
                            let r = ui.add(
                                egui::DragValue::new(&mut settings.custom_sync_latency_ms)
                                    .range(raop::ap2::session::MIN_SYNC_LATENCY_MS..=1000)
                                    .suffix(" ms"),
                            );
                            settings_changed |= r.changed();
                        }
                    });
                });

                ui.add_space(6.0);

                // 4. Full-width volume slider with speaker icon and %
                ui.horizontal(|ui| {
                    let icon = if settings.volume_pct <= 0.0 {
                        "🔇"
                    } else if settings.volume_pct < 50.0 {
                        "🔉"
                    } else {
                        "🔊"
                    };
                    ui.label(icon);
                    // Right to left: the value box sits flush with the right edge and the
                    // rail takes whatever is left, so nothing overflows the panel.
                    let r = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let value = ui.add(
                                egui::DragValue::new(&mut settings.volume_pct)
                                    .range(0.0..=100.0)
                                    .suffix("%")
                                    .speed(1.0)
                                    .fixed_decimals(0),
                            );
                            ui.spacing_mut().slider_width = ui.available_width().max(80.0);
                            let rail = ui.add(
                                egui::Slider::new(&mut settings.volume_pct, 0.0..=100.0)
                                    .integer()
                                    .show_value(false),
                            );
                            rail | value
                        })
                        .inner;
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        settings_changed = true;
                        if snap.state == State::Streaming {
                            self.engine.send(Cmd::SetVolume(settings.volume_pct));
                        }
                    } else if r.changed() {
                        // Mid-drag: keep the value in memory so the % box (drawn before the
                        // rail) shows it live; saving and sending still wait for release.
                        if let Ok(mut s) = self.settings.lock() {
                            s.volume_pct = settings.volume_pct;
                        }
                    }
                    // Mouse wheel over the rail or the % box: 2 % per notch like the Windows
                    // volume flyout; touchpads scroll in points and adjust proportionally.
                    if r.hovered() {
                        let step = ui.input(|i| wheel_volume_step(&i.events));
                        if step != 0.0 {
                            settings.volume_pct = (settings.volume_pct + step).clamp(0.0, 100.0);
                            settings_changed = true;
                            if snap.state == State::Streaming {
                                self.engine.send(Cmd::SetVolume(settings.volume_pct));
                            }
                        }
                    }
                });

                ui.add_space(8.0);

                // 5. Large full-width Start/Stop button
                let (btn_label, btn_color) = if active {
                    let col = if snap.state == State::Connecting {
                        egui::Color32::from_rgb(0xd0, 0x8a, 0x20)
                    } else {
                        egui::Color32::from_rgb(0xb8, 0x38, 0x48)
                    };
                    (t(lang, Key::StopStreaming), col)
                } else {
                    (
                        t(lang, Key::StartStreaming),
                        egui::Color32::from_rgb(0x32, 0x75, 0xd0),
                    )
                };

                let btn = egui::Button::new(
                    egui::RichText::new(btn_label)
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(btn_color)
                .min_size(egui::vec2(ui.available_width(), 36.0));

                if ui.add(btn).clicked() {
                    if active {
                        self.engine.send(Cmd::Stop);
                    } else if let Some(p) = self.start_params(&snap.devices, &settings) {
                        self.engine.send(Cmd::Start(p));
                    }
                }

                // 6. Compact stats caption that is collapsible
                if snap.state == State::Streaming {
                    if let Some(title) = &snap.track_title {
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("♪ {title}"))
                                    .size(11.0)
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .truncate(),
                        );
                    }
                    ui.add_space(6.0);
                    let s = &snap.stats;
                    let secs = s.since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                    let columns = format_stats_columns(lang, secs, s.drift_ppm);
                    let id = ui.make_persistent_id("stats_collapsing");
                    egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        id,
                        false,
                    )
                    .show_header(ui, |ui| spread_row(ui, &columns))
                    .body(|ui| {
                        let detail = format_stats_detail(
                            lang,
                            s.buffer_fill,
                            s.packets_sent,
                            s.retransmits,
                            s.discontinuities,
                            s.dropped,
                        );
                        egui::Grid::new("stats_detail")
                            .num_columns(4)
                            .spacing([12.0, 2.0])
                            .show(ui, |ui| {
                                for (i, (label, value)) in detail.iter().enumerate() {
                                    ui.weak(*label);
                                    ui.label(value);
                                    if i % 2 == 1 {
                                        ui.end_row();
                                    }
                                }
                            });
                    });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.label(
                        egui::RichText::new(env!("NYX_VERSION"))
                            .size(10.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                });

                // Content height + frame inner margins (10 top/bottom) + stroke.
                // CentralPanel stretches min_rect to the window, so measure from the cursor.
                let content = ui.cursor().top() - ui.max_rect().top() - ui.spacing().item_spacing.y;
                let measured = content.max(0.0) + 2.0 * 10.0 + 2.0;
                CONTENT_HEIGHT.store(
                    measured.ceil().to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            });

        if let Ok(mut st) = self.flyout.lock() {
            FlyoutState::reflow(ui.ctx(), &mut st);
        }

        if settings_changed {
            if parse_addr(&self.manual_addr).is_some() {
                settings.device_addr = Some(self.manual_addr.trim().to_string());
            }
            if let Ok(mut s) = self.settings.lock() {
                // Owned by the engine (Start / Stop); this frame's copy may be stale.
                settings.was_streaming = s.was_streaming;
                *s = settings.clone();
            }
            settings.save();
            if let Some(t) = &self.tray {
                let autostart = crate::autostart::is_enabled().unwrap_or(false);
                t.update(&snap, &settings, autostart, settings.language.resolve());
            }
        }
        if let Ok(mut p) = self.params.lock() {
            *p = self.start_params(&snap.devices, &settings);
        }
        if active {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}

/// Lays texts out in one row: first left-aligned, last right-aligned, the rest evenly
/// spaced in between. Drops to the small text style when the body style does not fit.
fn spread_row(ui: &mut egui::Ui, texts: &[String]) {
    let width_of = |ui: &egui::Ui, style: &egui::TextStyle| -> f32 {
        texts
            .iter()
            .map(|s| {
                egui::WidgetText::from(s.as_str())
                    .into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        style.clone(),
                    )
                    .size()
                    .x
            })
            .sum()
    };
    let avail = ui.available_width();
    let gaps = texts.len().saturating_sub(1).max(1) as f32;
    let mut style = egui::TextStyle::Body;
    let mut used = width_of(ui, &style);
    if avail - used < 8.0 * gaps {
        style = egui::TextStyle::Small;
        used = width_of(ui, &style);
    }
    ui.spacing_mut().item_spacing.x = ((avail - used) / gaps).max(4.0);
    for s in texts {
        ui.label(egui::RichText::new(s).text_style(style.clone()));
    }
}

/// Small filled circle (painted, so it does not depend on the font having U+25CF).
fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let size = ui.text_style_height(&egui::TextStyle::Body) * 0.55;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
}

/// Shows a banner when Windows Firewall is likely to drop the receiver's connections, with a
/// one-click fix that asks for administrator rights (UAC).
fn firewall_banner(ui: &mut egui::Ui, engine: &Engine, fw: &Firewall, state: &State, lang: Lang) {
    let warn = egui::Color32::from_rgb(0xe0, 0xa0, 0x30);
    let bad = egui::Color32::from_rgb(0xe0, 0x50, 0x50);
    let setup_failed = matches!(state, State::Error(err) if engine::looks_like_firewall_error(err));

    let (color, text, button_label) = match fw {
        Firewall::NeedsFix { blocked: true } => (
            warn,
            t(lang, Key::FirewallBlocked).to_string(),
            Some(t(lang, Key::FirewallAllowButton)),
        ),
        Firewall::NeedsFix { blocked: false } => (
            warn,
            t(lang, Key::FirewallUnconfigured).to_string(),
            Some(t(lang, Key::FirewallAllowButton)),
        ),
        Firewall::Fixing => (warn, t(lang, Key::FirewallWaitingUac).to_string(), None),
        // A failed *check* (e.g. PowerShell unavailable) is not actionable; stay quiet
        // unless a connection failure points at the firewall (handled below).
        Firewall::Failed(err) if !matches!(err, engine::FirewallError::CheckFailed(_)) => (
            bad,
            format_firewall_error(lang, err),
            Some(t(lang, Key::FirewallRetryButton)),
        ),
        _ if setup_failed => (
            bad,
            t(lang, Key::FirewallSetupFailed).to_string(),
            Some(t(lang, Key::FirewallAllowButton)),
        ),
        _ => return,
    };

    egui::Frame::group(ui.style())
        .stroke(egui::Stroke::new(1.0, color))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, text);
                if let Some(label) = button_label
                    && ui.button(label).clicked()
                {
                    engine.send(Cmd::FixFirewall);
                }
            });
        });
    ui.add_space(4.0);
}

/// Sets denser, larger text styles and comfortable spacing.
fn configure_styles(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(19.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(15.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
    });
}

/// Windows build number (e.g. 22631) from the registry; `GetVersionEx` lies to
/// unmanifested programs.
fn windows_build() -> Option<u32> {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    let mut buf = [0u16; 16];
    let mut len = std::mem::size_of_val(&buf) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let chars = (len as usize / 2).saturating_sub(1); // drop the NUL
    String::from_utf16_lossy(&buf[..chars.min(buf.len())])
        .trim()
        .parse()
        .ok()
}

/// Requests rounded window corners via DWM; false where unsupported (before Windows 11,
/// build 22000). Windows 10 should reject the attribute, but the build check does not
/// rely on that.
fn try_set_window_corners(window: &winit::window::Window) -> bool {
    if windows_build().is_some_and(|b| b < 22000) {
        return false;
    }
    use raw_window_handle::HasWindowHandle;
    use windows::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    if let Ok(wh) = window.window_handle()
        && let raw_window_handle::RawWindowHandle::Win32(win32) = wh.as_raw()
    {
        let hwnd = windows::Win32::Foundation::HWND(win32.hwnd.get() as *mut _);
        let preference = DWMWCP_ROUND;
        unsafe {
            return DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &preference as *const _ as *const _,
                std::mem::size_of_val(&preference) as u32,
            )
            .is_ok();
        }
    }
    false
}

/// Loads a system CJK font as fallback so device names like 「客厅」 render (egui's bundled
/// fonts have no CJK glyphs). Nothing is bundled into the binary.
fn install_cjk_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\YuGothM.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];
    let Some(bytes) = candidates.iter().find_map(|p| map_font(p).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".into(), Arc::new(egui::FontData::from_static(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
}

mod tray {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tray_icon::menu::{
        CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    };
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{FlyoutState, SharedParams};
    use crate::engine::{Cmd, Engine, State};
    use crate::i18n::{Key, Lang, engine_error_title, format_stats_columns, t};
    use crate::position::Rect;
    use crate::settings::Settings;
    use crate::tray_menu::{self, MenuItemKind, TrayMenuAction, TrayMenuItem};

    static QUIT: AtomicBool = AtomicBool::new(false);

    pub fn quit_requested() -> bool {
        QUIT.load(Ordering::Relaxed)
    }

    type ActionMaps = (
        HashMap<MenuId, TrayMenuAction>,
        HashMap<MenuId, TrayMenuAction>,
    );

    enum MudaContainer<'a> {
        Menu(&'a Menu),
        Submenu(&'a Submenu),
    }

    impl<'a> MudaContainer<'a> {
        fn append(&self, item: &dyn tray_icon::menu::IsMenuItem) -> anyhow::Result<()> {
            match self {
                MudaContainer::Menu(m) => m.append(item)?,
                MudaContainer::Submenu(s) => s.append(item)?,
            }
            Ok(())
        }
    }

    fn append_muda_items(
        container: &MudaContainer<'_>,
        items: &[TrayMenuItem],
        action_map: &mut HashMap<MenuId, TrayMenuAction>,
    ) -> anyhow::Result<()> {
        for item in items {
            match &item.kind {
                MenuItemKind::Standard => {
                    let m = MenuItem::new(&item.label, item.enabled, None);
                    if let Some(action) = &item.action {
                        action_map.insert(m.id().clone(), action.clone());
                    }
                    container.append(&m)?;
                }
                MenuItemKind::Checkbox { checked } => {
                    let m = CheckMenuItem::new(&item.label, item.enabled, *checked, None);
                    if let Some(action) = &item.action {
                        action_map.insert(m.id().clone(), action.clone());
                    }
                    container.append(&m)?;
                }
                MenuItemKind::Radio { selected } => {
                    let m = CheckMenuItem::new(&item.label, item.enabled, *selected, None);
                    if let Some(action) = &item.action {
                        action_map.insert(m.id().clone(), action.clone());
                    }
                    container.append(&m)?;
                }
                MenuItemKind::Submenu(children) => {
                    let sub = Submenu::new(&item.label, item.enabled);
                    append_muda_items(&MudaContainer::Submenu(&sub), children, action_map)?;
                    container.append(&sub)?;
                }
                MenuItemKind::Separator => {
                    let sep = PredefinedMenuItem::separator();
                    container.append(&sep)?;
                }
            }
        }
        Ok(())
    }

    fn render_muda_menu(
        items: &[TrayMenuItem],
        action_map: &mut HashMap<MenuId, TrayMenuAction>,
    ) -> anyhow::Result<Menu> {
        let menu = Menu::new();
        append_muda_items(&MudaContainer::Menu(&menu), items, action_map)?;
        Ok(menu)
    }

    fn tooltip_text(snap: &crate::engine::Shared, lang: Lang) -> String {
        match &snap.state {
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
            State::Error(err) => {
                format!(
                    "{}: {}",
                    t(lang, Key::TrayTooltipError),
                    // Windows truncates tray tooltips (128 chars); the flyout shows the details.
                    engine_error_title(lang, err)
                )
            }
        }
    }

    pub struct Tray {
        icon: Mutex<TrayIcon>,
        /// (current, previous) generations of menu id → action.
        action_map: Arc<Mutex<ActionMaps>>,
        last_model: Mutex<Option<Vec<TrayMenuItem>>>,
        last_state: Mutex<Option<State>>,
        last_tooltip: Mutex<Option<String>>,
    }

    impl Tray {
        pub fn new(
            ctx: egui::Context,
            engine: Engine,
            _params: SharedParams,
            settings: Arc<Mutex<Settings>>,
            flyout: Arc<Mutex<FlyoutState>>,
            initial_lang: Lang,
        ) -> anyhow::Result<Self> {
            let snap = engine.snapshot();
            let sett = settings.lock().map(|s| s.clone()).unwrap_or_default();
            let autostart_initial = crate::autostart::is_enabled().unwrap_or(false);
            let initial_model = tray_menu::build_menu_model(
                &snap,
                &sett,
                autostart_initial,
                initial_lang,
                tray_menu::MenuPlatform::Windows,
            );

            let mut initial_action_map = HashMap::new();
            let menu = render_muda_menu(&initial_model, &mut initial_action_map)?;

            let action_map = Arc::new(Mutex::new((initial_action_map, HashMap::new())));
            let tip = tooltip_text(&snap, initial_lang);

            let icon = TrayIconBuilder::new()
                .with_tooltip(&tip)
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_icon(state_icon(&snap.state)?)
                .build()?;

            let c = ctx.clone();
            let e = engine.clone();
            let s_clone = settings.clone();
            let map = action_map.clone();

            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                let action = map
                    .lock()
                    .ok()
                    .and_then(|m| m.0.get(&ev.id).or_else(|| m.1.get(&ev.id)).cloned());
                if let Some(action) = action {
                    let c_quit = c.clone();
                    let e_quit = e.clone();
                    tray_menu::apply_action(&action, &e, &s_clone, move || {
                        QUIT.store(true, Ordering::Relaxed);
                        e_quit.send(Cmd::Shutdown);
                        c_quit.send_viewport_cmd(egui::ViewportCommand::Close);
                    });
                    c.request_repaint();
                }
            }));

            let c_icon = ctx.clone();
            let f_icon = flyout.clone();
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| match ev {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    rect,
                    ..
                } => {
                    if let Ok(mut f) = f_icon.lock() {
                        let icon = Rect {
                            x: rect.position.x as i32,
                            y: rect.position.y as i32,
                            width: rect.size.width,
                            height: rect.size.height,
                        };
                        FlyoutState::toggle_from_tray(&c_icon, &mut f, icon);
                    }
                }
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => {
                    if let Ok(mut f) = f_icon.lock()
                        && !f.visible
                    {
                        FlyoutState::show_fallback(&c_icon, &mut f);
                    }
                }
                _ => {}
            }));

            Ok(Self {
                icon: Mutex::new(icon),
                action_map,
                last_model: Mutex::new(Some(initial_model)),
                last_state: Mutex::new(Some(snap.state)),
                last_tooltip: Mutex::new(Some(tip)),
            })
        }

        pub fn update(
            &self,
            snap: &crate::engine::Shared,
            settings: &Settings,
            autostart: bool,
            lang: Lang,
        ) {
            // 1. Tooltip
            let tip = tooltip_text(snap, lang);
            if let Ok(mut last_tip) = self.last_tooltip.lock()
                && last_tip.as_deref() != Some(&tip)
            {
                if let Ok(icon) = self.icon.lock() {
                    let _ = icon.set_tooltip(Some(&tip));
                }
                *last_tip = Some(tip);
            }

            // 2. Icon
            if let Ok(mut last_st) = self.last_state.lock()
                && last_st.as_ref() != Some(&snap.state)
            {
                if let Ok(i) = state_icon(&snap.state)
                    && let Ok(icon) = self.icon.lock()
                {
                    let _ = icon.set_icon(Some(i));
                }
                *last_st = Some(snap.state.clone());
            }

            // 3. Menu model
            let model = tray_menu::build_menu_model(
                snap,
                settings,
                autostart,
                lang,
                tray_menu::MenuPlatform::Windows,
            );
            if let Ok(mut last_m) = self.last_model.lock()
                && last_m.as_ref() != Some(&model)
            {
                let mut new_action_map = HashMap::new();
                if let Ok(menu) = render_muda_menu(&model, &mut new_action_map) {
                    if let Ok(icon) = self.icon.lock() {
                        icon.set_menu(Some(Box::new(menu)));
                    }
                    if let Ok(mut map) = self.action_map.lock() {
                        // Keep the previous generation too: a menu that is open while the
                        // model changes still sends the old ids.
                        let previous = std::mem::replace(&mut map.0, new_action_map);
                        map.1 = previous;
                    }
                }
                *last_m = Some(model);
            }
        }
    }

    fn state_icon(state: &State) -> anyhow::Result<Icon> {
        let icon = &crate::icons::for_state(state)[3]; // 32px Windows tray icon
        Ok(Icon::from_rgba(icon.rgba.to_vec(), icon.size, icon.size)?)
    }
}

/// Keep the read-only view for the process lifetime, as required by FontData::from_static.
/// Closing the file/mapping handles does not invalidate a mapped view.
fn map_font(path: &str) -> windows::core::Result<&'static [u8]> {
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetFileSizeEx, OPEN_EXISTING,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY,
    };
    use windows::core::PCWSTR;

    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // Safety: path is NUL-terminated; all handles are closed on both success and failure.
    // The view is read-only and deliberately never unmapped, so the slice stays valid.
    unsafe {
        let file = CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?;
        let result = (|| {
            let mut len = 0;
            GetFileSizeEx(file, &mut len)?;
            let mapping = CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, PCWSTR::null())?;
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
            let result = if view.Value.is_null() {
                Err(windows::core::Error::from_win32())
            } else {
                Ok(std::slice::from_raw_parts(
                    view.Value.cast::<u8>(),
                    len as usize,
                ))
            };
            let _ = CloseHandle(mapping);
            result
        })();
        let _ = CloseHandle(file);
        result
    }
}

/// A repaint deadline and the egui pass which requested it. Requests made during a
/// completed pass are also represented by FullOutput; older requests are discarded.
struct Repaint {
    at: Instant,
    pass: u64,
}

struct WindowState {
    window: Arc<winit::window::Window>,
    surface: softbuffer::Surface<Arc<winit::window::Window>, Arc<winit::window::Window>>,
    input: egui_winit::State,
    renderer: crate::soft_render::Renderer,
    app: App,
    close_requested: bool,
}

struct Shell {
    ctx: egui::Context,
    state: Option<WindowState>,
    next_repaint: Option<Instant>,
    autostart: bool,
    error: Option<anyhow::Error>,
}

impl Shell {
    fn schedule(&mut self, at: Instant) {
        self.next_repaint = Some(self.next_repaint.map_or(at, |pending| pending.min(at)));
    }

    fn create_window(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
    ) -> anyhow::Result<()> {
        use winit::platform::windows::WindowAttributesExtWindows;
        use winit::window::{Icon, Window, WindowLevel};
        let icon = &crate::icons::for_state(&State::Streaming)[5];
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Nyx Refrain")
                    .with_window_icon(Some(Icon::from_rgba(
                        icon.rgba.to_vec(),
                        icon.size,
                        icon.size,
                    )?))
                    .with_decorations(false)
                    .with_skip_taskbar(true)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_resizable(false)
                    .with_visible(false)
                    .with_inner_size(winit::dpi::LogicalSize::new(FLYOUT_WIDTH, FLYOUT_HEIGHT)),
            )?,
        );
        let rounded_corners = try_set_window_corners(&window);
        let context =
            softbuffer::Context::new(window.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let input = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let mut app = App::new(&self.ctx, self.autostart);
        app.rounded_corners = rounded_corners;
        self.state = Some(WindowState {
            window,
            surface,
            input,
            app,
            renderer: Default::default(),
            close_requested: false,
        });
        self.schedule(Instant::now());
        Ok(())
    }

    fn frame(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) -> anyhow::Result<()> {
        let Some(state) = &mut self.state else {
            return Ok(());
        };
        self.next_repaint = None;
        let mut input = state.input.take_egui_input(&state.window);
        let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        egui_winit::update_viewport_info(info, &self.ctx, &state.window, false);
        if std::mem::take(&mut state.close_requested) {
            info.events.push(egui::ViewportEvent::Close);
        }
        let mut output = self.ctx.run_ui(input, |ui| {
            state.app.logic(ui.ctx());
            state.app.ui(ui);
            // Tray state and focus debounce must progress even while hidden/idle.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        });
        state
            .input
            .handle_platform_output(&state.window, output.platform_output);
        state.renderer.update_textures(&mut output.textures_delta);
        let mut show = false;
        let mut focus = false;
        let mut info = egui::ViewportInfo::default();
        let mut actions = Vec::new();
        if let Ok(mut flyout) = state.app.flyout.lock()
            && let Some(pos) = flyout.pending_position.take()
        {
            state
                .window
                .set_outer_position(winit::dpi::PhysicalPosition::new(pos.x, pos.y));
        }
        let root = &output.viewport_output[&egui::ViewportId::ROOT];
        for command in &root.commands {
            match command {
                egui::ViewportCommand::Visible(true) => show = true,
                egui::ViewportCommand::Visible(false) => {
                    show = false;
                    state.window.set_visible(false);
                }
                egui::ViewportCommand::Focus => focus = true,
                egui::ViewportCommand::Close => state.close_requested = true,
                egui::ViewportCommand::CancelClose => state.close_requested = false,
                other => egui_winit::process_viewport_commands(
                    &self.ctx,
                    &mut info,
                    [other.clone()],
                    &state.window,
                    &mut actions,
                ),
            }
        }
        if tray::quit_requested() {
            event_loop.exit();
            return Ok(());
        }
        let size = state.window.inner_size();
        if (show || state.window.is_visible() == Some(true))
            && let (Some(width), Some(height)) = (
                std::num::NonZeroU32::new(size.width),
                std::num::NonZeroU32::new(size.height),
            )
        {
            state
                .surface
                .resize(width, height)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
            let mut buffer = state
                .surface
                .buffer_mut()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            state.renderer.paint(
                &primitives,
                output.pixels_per_point,
                [size.width as usize, size.height as usize],
                // Match eframe's clear color behind the rounded egui panel.
                egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180),
                &mut buffer,
            );
            buffer.present().map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        state.renderer.free_textures(&mut output.textures_delta);
        // Present before showing: neither normal launch nor autostart can flash an empty window.
        if show {
            state.window.set_visible(true);
        }
        if focus && state.window.is_visible() == Some(true) {
            state.window.focus_window();
        }
        if state.close_requested {
            self.schedule(Instant::now());
        }
        if let Some(at) = Instant::now().checked_add(root.repaint_delay) {
            self.schedule(at);
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl winit::application::ApplicationHandler<Repaint> for Shell {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.state.is_none()
            && let Err(e) = self.create_window(event_loop)
        {
            self.fail(event_loop, e);
        }
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: Repaint) {
        if event.pass + 1 >= self.ctx.cumulative_pass_nr() {
            self.schedule(event.at);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        use winit::event::WindowEvent;
        let Some(state) = &mut self.state else { return };
        let response = state.input.on_window_event(&state.window, &event);
        if matches!(event, WindowEvent::CloseRequested) {
            state.close_requested = true;
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            if let Err(e) = self.frame(event_loop) {
                self.fail(event_loop, e);
            }
        } else if response.repaint
            || matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
            )
        {
            self.schedule(Instant::now());
        }
    }

    fn new_events(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _cause: winit::event::StartCause,
    ) {
        if self.next_repaint.is_some_and(|at| at <= Instant::now())
            && let Err(e) = self.frame(event_loop)
        {
            self.fail(event_loop, e);
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        event_loop.set_control_flow(self.next_repaint.map_or(
            winit::event_loop::ControlFlow::Wait,
            winit::event_loop::ControlFlow::WaitUntil,
        ));
    }
}

pub fn run() -> anyhow::Result<()> {
    let autostart = std::env::args().any(|arg| arg == "--autostart");
    let Some(_instance) = single_instance::acquire(!autostart) else {
        return Ok(());
    };
    let event_loop = winit::event_loop::EventLoop::<Repaint>::with_user_event().build()?;
    let ctx = egui::Context::default();
    let proxy = event_loop.create_proxy();
    ctx.set_request_repaint_callback(move |request| {
        if let Some(at) = Instant::now().checked_add(request.delay) {
            let _ = proxy.send_event(Repaint {
                at,
                pass: request.current_cumulative_pass_nr,
            });
        }
    });
    let mut shell = Shell {
        ctx,
        state: None,
        next_repaint: None,
        autostart,
        error: None,
    };
    event_loop.run_app(&mut shell)?;
    shell.error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_steps_volume() {
        let wheel = |unit, y| egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        use egui::MouseWheelUnit::{Line, Point};
        assert_eq!(wheel_volume_step(&[wheel(Line, 1.0)]), 2.0);
        assert_eq!(
            wheel_volume_step(&[wheel(Line, -1.0), wheel(Line, -1.0)]),
            -4.0
        );
        assert_eq!(wheel_volume_step(&[wheel(Point, 30.0)]), 3.0);
        assert_eq!(wheel_volume_step(&[egui::Event::Copy]), 0.0);
    }

    #[test]
    fn flyout_state_debounce_logic() {
        let mut st = FlyoutState::default();
        assert!(!st.visible);
        assert!(!st.just_closed());

        st.visible = true;
        st.closed_at = Instant::now();
        assert!(st.just_closed());

        st.closed_at = Instant::now() - Duration::from_millis(300);
        assert!(!st.just_closed());
    }
}
