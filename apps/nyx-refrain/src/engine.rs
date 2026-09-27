//! Streaming engine for the GUI: a background thread with its own tokio runtime that owns
//! the AirPlay 2 session. The UI sends [`Cmd`]s and reads [`Shared`] state.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait Notifier: Send + Sync + 'static {
    fn wake(&self);
}

impl<F: Fn() + Send + Sync + 'static> Notifier for F {
    fn wake(&self) {
        (self)();
    }
}

impl<N: Notifier + ?Sized> Notifier for Arc<N> {
    fn wake(&self) {
        (**self).wake();
    }
}
use raop::ap2::session::{Ap2Config, Ap2Error, Ap2Session, MIN_SYNC_LATENCY_MS};
use tokio::sync::mpsc;

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceEntry {
    pub name: String,
    pub addr: SocketAddr,
    pub features: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StartParams {
    pub device: DeviceEntry,
    pub sync_latency_ms: i32,
    pub volume_pct: f32,
    pub send_now_playing: bool,
    pub remote_control: bool,
    pub capture_mode: crate::settings::CaptureMode,
}

pub enum Cmd {
    /// User started streaming (remembered for [`Settings::resume_on_launch`]).
    ///
    /// [`Settings::resume_on_launch`]: crate::settings::Settings::resume_on_launch
    Start(StartParams),
    /// User stopped streaming (forgets the session to resume).
    Stop,
    /// App is quitting: stop streaming but keep the session to resume on next launch.
    Shutdown,
    SetVolume(f32),
    SetSendNowPlaying(bool),
    SetRemoteControl(bool),
    Discover,
    CheckFirewall,
    #[allow(dead_code)]
    FixFirewall,
    #[allow(dead_code)]
    AttachSettings(Arc<Mutex<crate::settings::Settings>>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineError {
    Capture(String),
    NotSupported(String),
    Connect(String),
    /// Connecting failed and the receiver is on none of this PC's LAN subnets (e.g. it is
    /// reached through a VPN / TUN route). AirPlay needs the receiver to connect back to
    /// the sender (events, timing, retransmits), which such routes normally prevent.
    NotLocal(String),
    Interrupted(String),
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub enum FirewallError {
    Cancelled,
    InstallFailed(String),
    CheckFailed(String),
    Other(String),
}

/// Windows Firewall state for this program (see `netutil::firewall`).
// Variants are platform specific (NotApplicable only off Windows, the rest only on Windows).
#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Firewall {
    #[default]
    Unknown,
    Checking,
    Ok,
    /// Inbound traffic is likely dropped; `blocked` = a block rule exists.
    NeedsFix {
        blocked: bool,
    },
    Fixing,
    Failed(FirewallError),
    /// Not Windows.
    NotApplicable,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Idle,
    Connecting,
    Streaming,
    Error(EngineError),
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub since: Option<Instant>,
    pub packets_sent: u64,
    pub retransmits: u64,
    pub buffer_fill: usize,
    pub drift_ppm: f64,
    pub dropped: u64,
    pub discontinuities: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Shared {
    pub state: State,
    pub devices: Vec<DeviceEntry>,
    pub discovering: bool,
    pub stats: Stats,
    pub firewall: Firewall,
    pub track_title: Option<String>,
}

#[derive(Clone)]
pub struct Engine {
    tx: mpsc::UnboundedSender<Cmd>,
    pub shared: Arc<Mutex<Shared>>,
}

impl Engine {
    pub fn spawn(notifier: Arc<dyn Notifier>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let s = shared.clone();
        std::thread::Builder::new()
            .name("nyx-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(run(rx, s, notifier));
            })
            .expect("spawn engine thread");
        let engine = Self { tx, shared };
        engine.send(Cmd::Discover);
        engine.send(Cmd::CheckFirewall);
        engine
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    #[allow(dead_code)]
    pub fn attach_settings(&self, settings: Arc<Mutex<crate::settings::Settings>>) {
        let _ = self.tx.send(Cmd::AttachSettings(settings));
    }

    pub fn snapshot(&self) -> Shared {
        self.shared.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// 0–100 % → receiver dB (-30..0), 0 % = mute (-144).
pub use raop::volume::pct_to_db;

fn set_state(shared: &Arc<Mutex<Shared>>, notifier: &dyn Notifier, st: State) {
    if let Ok(mut g) = shared.lock() {
        g.state = st;
    }
    notifier.wake();
}

/// How long a launch keeps trying to resume the last session (network / mDNS may still be
/// coming up right after login).
const RESUME_WINDOW: Duration = Duration::from_secs(120);
/// After this long without discovering the saved device by name, try its saved address.
const RESUME_ADDR_GRACE: Duration = Duration::from_secs(20);
const RESUME_FIRST_BACKOFF: Duration = Duration::from_secs(3);
const RESUME_MAX_BACKOFF: Duration = Duration::from_secs(15);

/// In-progress resume of the session that was streaming when the app last exited.
struct Resume {
    since: Instant,
    next_attempt: Instant,
    backoff: Duration,
    last_error: Option<EngineError>,
}

impl Resume {
    fn new(now: Instant) -> Self {
        Self {
            since: now,
            next_attempt: now,
            backoff: RESUME_FIRST_BACKOFF,
            last_error: None,
        }
    }
}

/// Start parameters for resuming: the saved device once discovery finds it by name, or (after
/// [`RESUME_ADDR_GRACE`]) its saved address.
fn resume_params(
    s: &crate::settings::Settings,
    devices: &[DeviceEntry],
    allow_addr: bool,
) -> Option<StartParams> {
    let discovered = s
        .device_name
        .as_ref()
        .and_then(|name| devices.iter().find(|d| &d.name == name).cloned());
    let device = discovered.or_else(|| {
        let addr = s
            .device_addr
            .as_deref()?
            .parse()
            .ok()
            .filter(|_| allow_addr)?;
        Some(DeviceEntry {
            name: s.device_name.clone().unwrap_or_else(|| format!("{addr}")),
            addr,
            features: None,
        })
    })?;
    Some(StartParams {
        device,
        sync_latency_ms: s.profile.sync_latency_ms(s.custom_sync_latency_ms),
        volume_pct: s.volume_pct,
        send_now_playing: s.send_now_playing,
        remote_control: s.remote_control,
        capture_mode: s.capture_mode,
    })
}

/// Whether `addr` is on the subnet of one of this machine's physical (non-VPN) interfaces.
fn on_local_subnet(addr: SocketAddr) -> bool {
    let std::net::IpAddr::V4(ip) = addr.ip() else {
        return true; // not checked; keep the original error
    };
    if ip.is_loopback() {
        return true;
    }
    netutil::enumerate_and_classify_interfaces(&netutil::ClassificationConfig::default())
        .map(|ifaces| {
            ifaces
                .iter()
                .any(|c| c.is_usable_physical && c.iface.contains_ipv4(ip))
        })
        .unwrap_or(true)
}

fn explain_not_local(addr: SocketAddr, err: EngineError) -> EngineError {
    match err {
        EngineError::Connect(msg) if !on_local_subnet(addr) => EngineError::NotLocal(msg),
        other => other,
    }
}

/// Quick TCP reachability check of a receiver before starting a session.
async fn reachable(addr: SocketAddr) -> Result<(), EngineError> {
    match tokio::time::timeout(
        Duration::from_millis(1500),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(EngineError::Connect(e.to_string())),
        Err(_) => Err(EngineError::Connect(format!(
            "{addr} unreachable (timed out)"
        ))),
    }
}

fn remember_streaming(settings: &Option<Arc<Mutex<crate::settings::Settings>>>, streaming: bool) {
    if let Some(s) = settings
        && let Ok(mut s) = s.lock()
        && s.was_streaming != streaming
    {
        s.was_streaming = streaming;
        s.save();
    }
}

#[cfg(target_os = "linux")]
type SinkVol = capture::pipewire::SinkVolume;
#[cfg(not(target_os = "linux"))]
type SinkVol = ();

#[cfg(windows)]
fn make_source(
    _initial_volume_pct: f32,
    capture_mode: crate::settings::CaptureMode,
) -> anyhow::Result<(Box<dyn capture::AudioSource + Send>, Option<()>)> {
    use capture::wasapi::{
        ProcessLoopbackConfig, ProcessLoopbackMode, ProcessLoopbackSource, WasapiLoopbackSource,
    };
    match capture_mode {
        crate::settings::CaptureMode::Process => {
            let config = ProcessLoopbackConfig {
                mode: ProcessLoopbackMode::ExcludeProcessTree,
                ..ProcessLoopbackConfig::new(std::process::id())
            };
            match ProcessLoopbackSource::with_config(config) {
                Ok(src) => {
                    eprintln!("nyx-refrain: capturing via WASAPI process loopback");
                    Ok((Box::new(src), None))
                }
                Err(e) => {
                    tracing_like_warn(&format!(
                        "process loopback failed ({e}), falling back to endpoint loopback"
                    ));
                    let src = WasapiLoopbackSource::with_config(Default::default())
                        .map_err(|e| anyhow::anyhow!("WASAPI endpoint loopback: {e}"))?;
                    Ok((Box::new(src), None))
                }
            }
        }
        crate::settings::CaptureMode::Endpoint => {
            let src = WasapiLoopbackSource::with_config(Default::default())
                .map_err(|e| anyhow::anyhow!("WASAPI endpoint loopback: {e}"))?;
            eprintln!("nyx-refrain: capturing via WASAPI endpoint loopback");
            Ok((Box::new(src), None))
        }
    }
}

#[cfg(target_os = "linux")]
fn make_source(
    initial_volume_pct: f32,
    _capture_mode: crate::settings::CaptureMode,
) -> anyhow::Result<(
    Box<dyn capture::AudioSource + Send>,
    Option<capture::pipewire::SinkVolumeControl>,
)> {
    let mute = initial_volume_pct <= 0.0;
    let initial_volume = Some(capture::pipewire::SinkVolume::new(initial_volume_pct, mute));
    let src = capture::pipewire::PipeWireSinkSource::new(capture::pipewire::PipeWireSinkConfig {
        node_name: "nyx_refrain_airplay".into(),
        node_description: "Nyx Refrain (AirPlay)".into(),
        set_default: true,
        initial_volume,
    })
    .map_err(|e| anyhow::anyhow!("PipeWire sink: {e}"))?;
    let vc = src.volume_control();
    Ok((Box::new(src), Some(vc)))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn make_source(
    _initial_volume_pct: f32,
    _capture_mode: crate::settings::CaptureMode,
) -> anyhow::Result<(Box<dyn capture::AudioSource + Send>, Option<()>)> {
    // Development stand-in on other platforms: a quiet 440 Hz tone.
    Ok((
        Box::new(capture::sources::sine::SineSource::new(
            440.0, -30.0, 44_100, 2,
        )),
        None,
    ))
}

struct Running {
    session: Ap2Session,
    metrics: pipeline::SharedPipelineMetrics,
    #[allow(dead_code)]
    send_now_playing: bool,
    _now_playing_watcher: Option<nowplaying::NowPlayingWatcher>,
    /// Forward receiver playback commands to the now-playing session (needs the watcher).
    remote_control: bool,
    #[cfg(target_os = "linux")]
    vol_ctrl: Option<capture::pipewire::SinkVolumeControl>,
    #[cfg(target_os = "linux")]
    vol_rx: Option<tokio::sync::watch::Receiver<Option<capture::pipewire::SinkVolume>>>,
}

impl Running {
    async fn stop(mut self) {
        self._now_playing_watcher = None;
        self.session.stop().await;
    }
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    shared: Arc<Mutex<Shared>>,
    notifier: Arc<dyn Notifier>,
) {
    let mut running: Option<Running> = None;
    let mut now_playing_rx: Option<tokio::sync::watch::Receiver<nowplaying::NowPlaying>> = None;
    let mut remote_rx: Option<mpsc::Receiver<raop::ap2::remote::Command>> = None;
    let mut last_sent_track: Option<nowplaying::NowPlaying> = None;
    let mut settings_ref: Option<Arc<Mutex<crate::settings::Settings>>> = None;
    #[cfg(target_os = "linux")]
    let mut last_receiver_vol: Option<f32> = None;
    let mut feedback = tokio::time::interval(Duration::from_secs(2));
    let mut refresh = tokio::time::interval(Duration::from_millis(500));
    let (disc_tx, mut disc_rx) = mpsc::unbounded_channel::<Vec<DeviceEntry>>();
    let (fw_tx, mut fw_rx) = mpsc::unbounded_channel::<Firewall>();
    // Commands issued by the engine itself (resume attempts); never closes.
    let (internal_tx, mut internal_rx) = mpsc::unbounded_channel::<Cmd>();
    let mut resume: Option<Resume> = None;
    let mut resume_tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            cmd = async {
                tokio::select! {
                    biased;
                    Some(c) = internal_rx.recv() => Some((c, true)),
                    c = rx.recv() => c.map(|c| (c, false)),
                }
            } => {
                let Some((cmd, internal)) = cmd else { break };
                let user_stop = matches!(cmd, Cmd::Stop);
                match cmd {
                    Cmd::AttachSettings(s) => {
                        let wants_resume = s
                            .lock()
                            .map(|s| {
                                s.resume_on_launch
                                    && s.was_streaming
                                    && (s.device_name.is_some() || s.device_addr.is_some())
                            })
                            .unwrap_or(false);
                        settings_ref = Some(s);
                        if wants_resume && running.is_none() && resume.is_none() {
                            resume = Some(Resume::new(Instant::now()));
                            set_state(&shared, &*notifier, State::Connecting);
                        }
                    }
                    Cmd::Start(p) => {
                        if internal && resume.is_none() {
                            continue; // resume was cancelled since this attempt was queued
                        }
                        if !internal {
                            resume = None;
                            remember_streaming(&settings_ref, true);
                        }
                        if let Some(r) = running.take() {
                            r.stop().await;
                        }
                        #[cfg(target_os = "linux")]
                        {
                            last_receiver_vol = None;
                        }
                        now_playing_rx = None;
                        remote_rx = None;
                        last_sent_track = None;
                        if let Ok(mut g) = shared.lock() {
                            g.track_title = None;
                        }
                        set_state(&shared, &*notifier, State::Connecting);
                        // Probe first: starting creates the Linux virtual sink (the new default
                        // output) before connecting, so an unreachable device would silence the
                        // desktop for a whole connect timeout.
                        let started = match reachable(p.device.addr).await {
                            Ok(()) => start(&p).await,
                            Err(e) => Err(e),
                        }
                        .map_err(|e| explain_not_local(p.device.addr, e));
                        match started {
                            Ok((mut r, np_rx, r_rx)) => {
                                if let Ok(mut g) = shared.lock() {
                                    g.stats = Stats {
                                        since: Some(Instant::now()),
                                        ..Default::default()
                                    };
                                }
                                let np_candidate = np_rx.as_ref().map(|rx| rx.borrow().clone());
                                let decision = nowplaying::decide_metadata_send(
                                    last_sent_track.as_ref(),
                                    np_candidate.as_ref(),
                                    p.send_now_playing,
                                );
                                send_engine_metadata(
                                    &mut r.session,
                                    &decision,
                                    &mut last_sent_track,
                                )
                                .await;
                                if let Ok(mut g) = shared.lock() {
                                    g.track_title = if decision.is_placeholder
                                        || decision.track.title.is_empty()
                                    {
                                        None
                                    } else {
                                        Some(decision.track.title.clone())
                                    };
                                }
                                now_playing_rx = np_rx;
                                remote_rx = r_rx;
                                running = Some(r);
                                resume = None;
                                set_state(&shared, &*notifier, State::Streaming);
                            }
                            Err(e) => match resume.as_mut() {
                                // Resume attempt failed: stay "connecting" and retry.
                                Some(r) if internal => {
                                    r.last_error = Some(e);
                                    set_state(&shared, &*notifier, State::Connecting);
                                }
                                _ => set_state(&shared, &*notifier, State::Error(e)),
                            },
                        }
                    }
                    Cmd::Stop | Cmd::Shutdown => {
                        if user_stop {
                            remember_streaming(&settings_ref, false);
                        }
                        resume = None;
                        if let Some(r) = running.take() {
                            r.stop().await;
                        }
                        now_playing_rx = None;
                        remote_rx = None;
                        last_sent_track = None;
                        if let Ok(mut g) = shared.lock() {
                            g.track_title = None;
                        }
                        set_state(&shared, &*notifier, State::Idle);
                    }
                    Cmd::SetSendNowPlaying(enabled) => {
                        if let Some(r) = running.as_mut() {
                            r.send_now_playing = enabled;
                            if enabled {
                                if now_playing_rx.is_none() {
                                    let watcher = nowplaying::NowPlayingWatcher::start();
                                    let rx = watcher.subscribe();
                                    let np = rx.borrow().clone();
                                    let decision = nowplaying::decide_metadata_send(
                                        last_sent_track.as_ref(),
                                        Some(&np),
                                        true,
                                    );
                                    send_engine_metadata(
                                        &mut r.session,
                                        &decision,
                                        &mut last_sent_track,
                                    )
                                    .await;
                                    if let Ok(mut g) = shared.lock() {
                                        g.track_title = if decision.is_placeholder
                                            || decision.track.title.is_empty()
                                        {
                                            None
                                        } else {
                                            Some(decision.track.title.clone())
                                        };
                                    }
                                    now_playing_rx = Some(rx);
                                    r._now_playing_watcher = Some(watcher);
                                    notifier.wake();
                                }
                            } else {
                                now_playing_rx = None;
                                r._now_playing_watcher = None;
                                let decision = nowplaying::decide_metadata_send(
                                    last_sent_track.as_ref(),
                                    None,
                                    false,
                                );
                                send_engine_metadata(
                                    &mut r.session,
                                    &decision,
                                    &mut last_sent_track,
                                )
                                .await;
                                if let Ok(mut g) = shared.lock() {
                                    g.track_title = None;
                                }
                                notifier.wake();
                            }
                        }
                    }
                    Cmd::SetRemoteControl(enabled) => {
                        if let Some(r) = running.as_mut() {
                            r.remote_control = enabled;
                        }
                    }
                    Cmd::SetVolume(pct) => {
                        if let Some(r) = running.as_mut() {
                            let result = r.session.set_volume(pct_to_db(pct)).await;
                            if let Err(e) = result {
                                tracing_like_warn(&format!("set volume failed: {e}"));
                            }
                            #[cfg(target_os = "linux")]
                            if let Some(vc) = &r.vol_ctrl {
                                vc.set(pct, pct <= 0.0);
                            }
                        }
                    }
                    Cmd::CheckFirewall | Cmd::FixFirewall => {
                        let fix = matches!(cmd, Cmd::FixFirewall);
                        if let Ok(mut g) = shared.lock() {
                            g.firewall = if fix { Firewall::Fixing } else { Firewall::Checking };
                        }
                        notifier.wake();
                        let tx = fw_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let _ = tx.send(firewall_task(fix));
                        });
                    }
                    Cmd::Discover => {
                        if let Ok(mut g) = shared.lock() {
                            g.discovering = true;
                        }
                        notifier.wake();
                        let tx = disc_tx.clone();
                        tokio::spawn(async move {
                            let opts = discovery::browser::DiscoveryOptions::default();
                            let found = discovery::browser::discover_devices(&opts).await.unwrap_or_default();
                            let mut list: Vec<DeviceEntry> = found
                                .into_iter()
                                .map(|d| {
                                    let features = d
                                        .txt_records
                                        .get("features")
                                        .or_else(|| d.txt_records.get("ft"))
                                        .cloned();
                                    DeviceEntry {
                                        name: d.name,
                                        addr: SocketAddr::new(d.ip.into(), d.port),
                                        features,
                                    }
                                })
                                .collect();
                            list.sort_by(|a, b| a.name.cmp(&b.name));
                            list.dedup_by(|a, b| a.name == b.name && a.addr == b.addr);
                            let _ = tx.send(list);
                        });
                    }
                }
            }
            _ = resume_tick.tick(), if resume.is_some() && running.is_none() => {
                let now = Instant::now();
                if let Some(r) = resume.as_mut() {
                    let elapsed = now.duration_since(r.since);
                    if elapsed >= RESUME_WINDOW {
                        let e = r.last_error.take().unwrap_or_else(|| {
                            EngineError::Connect("saved device not found on the network".into())
                        });
                        resume = None;
                        set_state(&shared, &*notifier, State::Error(e));
                    } else if now >= r.next_attempt {
                        let settings = settings_ref
                            .as_ref()
                            .and_then(|s| s.lock().ok().map(|s| s.clone()));
                        let (devices, discovering) = shared
                            .lock()
                            .map(|g| (g.devices.clone(), g.discovering))
                            .unwrap_or_default();
                        let params = settings.and_then(|s| {
                            resume_params(&s, &devices, elapsed >= RESUME_ADDR_GRACE)
                        });
                        match params {
                            Some(p) => {
                                r.next_attempt = now + r.backoff;
                                r.backoff = (r.backoff * 2).min(RESUME_MAX_BACKOFF);
                                let _ = internal_tx.send(Cmd::Start(p));
                            }
                            None if !discovering => {
                                let _ = internal_tx.send(Cmd::Discover);
                            }
                            None => {}
                        }
                    }
                }
            }
            Some(fw) = fw_rx.recv() => {
                if let Ok(mut g) = shared.lock() {
                    g.firewall = fw;
                }
                notifier.wake();
            }
            Some(list) = disc_rx.recv() => {
                if let Ok(mut g) = shared.lock() {
                    g.devices = list;
                    g.discovering = false;
                }
                notifier.wake();
            }
            _v = async {
                #[cfg(target_os = "linux")]
                {
                    match running.as_mut().and_then(|r| r.vol_rx.as_mut()) {
                        Some(rx) => {
                            if rx.changed().await.is_ok() {
                                *rx.borrow_and_update()
                            } else {
                                std::future::pending::<Option<SinkVol>>().await
                            }
                        }
                        None => std::future::pending::<Option<SinkVol>>().await,
                    }
                }
                #[cfg(not(target_os = "linux"))]
                std::future::pending::<Option<SinkVol>>().await
            } => {
                #[cfg(target_os = "linux")]
                if let Some(vol) = _v
                    && let Some(r) = running.as_mut()
                {
                    let is_echo = last_receiver_vol
                        .map(|last| (vol.pct - last).abs() < 1.0)
                        .unwrap_or(false);
                    last_receiver_vol = None;
                    if !is_echo {
                        let db = if vol.mute { -144.0 } else { pct_to_db(vol.pct) };
                        if let Err(e) = r.session.set_volume(db).await {
                            tracing_like_warn(&format!("set volume failed: {e}"));
                        }
                        if let Some(s) = &settings_ref
                            && let Ok(mut s) = s.lock()
                        {
                            s.volume_pct = vol.pct;
                            s.save();
                        }
                        notifier.wake();
                    }
                }
            }
            command = async {
                match remote_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match (command, running.as_ref()) {
                    (Some(raop::ap2::remote::Command::Volume(vol)), Some(r)) => {
                        if r.session.is_quiet() {
                            // Quiet mode: ignore receiver volume change
                        } else {
                            let pct = (vol * 100.0).clamp(0.0, 100.0);
                            if let Some(s) = &settings_ref
                                && let Ok(mut s) = s.lock()
                            {
                                s.volume_pct = pct;
                                s.save();
                            }
                            #[cfg(target_os = "linux")]
                            if let Some(vc) = &r.vol_ctrl {
                                last_receiver_vol = Some(pct);
                                vc.set(pct, pct <= 0.0);
                            }
                            notifier.wake();
                        }
                    }
                    (Some(command), Some(r)) => {
                        if r.remote_control
                            && let Some(watcher) = &r._now_playing_watcher
                            && let Some(cmd) = nowplaying_command(command)
                        {
                            watcher.control(cmd);
                        }
                    }
                    (None, _) => remote_rx = None,
                    _ => {}
                }
            }
            _np = async {
                match now_playing_rx.as_mut() {
                    Some(rx) => {
                        if rx.changed().await.is_ok() {
                            Some(rx.borrow_and_update().clone())
                        } else {
                            std::future::pending::<Option<nowplaying::NowPlaying>>().await
                        }
                    }
                    None => std::future::pending::<Option<nowplaying::NowPlaying>>().await,
                }
            } => {
                if let Some(np) = _np
                    && let Some(r) = running.as_mut()
                {
                    let decision = nowplaying::decide_metadata_send(
                        last_sent_track.as_ref(),
                        Some(&np),
                        r.send_now_playing,
                    );
                    send_engine_metadata(
                        &mut r.session,
                        &decision,
                        &mut last_sent_track,
                    )
                    .await;
                    if let Ok(mut g) = shared.lock() {
                        g.track_title = if decision.is_placeholder || decision.track.title.is_empty() {
                            None
                        } else {
                            Some(decision.track.title.clone())
                        };
                    }
                    notifier.wake();
                }
            }
            _ = feedback.tick(), if running.is_some() => {
                let failed = match running.as_mut() {
                    Some(r) => r.session.feedback().await.err(),
                    None => None,
                };
                if let Some(e) = failed {
                    if let Some(r) = running.take() {
                        r.stop().await;
                    }
                    now_playing_rx = None;
                    remote_rx = None;
                    last_sent_track = None;
                    if let Ok(mut g) = shared.lock() {
                        g.track_title = None;
                    }
                    set_state(&shared, &*notifier, State::Error(EngineError::Interrupted(e.to_string())));
                }
            }
            _ = refresh.tick(), if running.is_some() => {
                if let Some(r) = running.as_ref() {
                    let m = r.metrics.snapshot();
                    let st = r.session.stats();
                    if let Ok(mut g) = shared.lock() {
                        g.stats.packets_sent = st.packets_sent.load(std::sync::atomic::Ordering::Relaxed);
                        g.stats.retransmits = st.retransmit_requests.load(std::sync::atomic::Ordering::Relaxed);
                        g.stats.buffer_fill = m.buffer_fill_packets;
                        g.stats.drift_ppm = m.estimated_drift_ppm;
                        g.stats.dropped = m.packets_dropped;
                        g.stats.discontinuities = m.capture_discontinuities;
                    }
                    notifier.wake();
                }
            }
        }
    }
    if let Some(r) = running.take() {
        r.stop().await;
    }
}

async fn send_engine_metadata(
    session: &mut Ap2Session,
    decision: &nowplaying::MetadataDecision,
    last_sent: &mut Option<nowplaying::NowPlaying>,
) {
    if !decision.has_action() {
        return;
    }

    // Artwork before text (see nyxr's send_cli_metadata: artwork sent right after a text
    // change is not picked up by some AirPlay 2 receivers). No progress is sent.
    if decision.send_artwork
        && let Some(art) = &decision.track.artwork
    {
        if decision.placeholder_artwork {
            let _ = session
                .send_artwork(&nowplaying::fresh_placeholder_artwork())
                .await;
        } else {
            let _ = session.send_artwork(&art.bytes).await;
        }
    }

    if decision.send_track_info {
        let info = raop::ap2::metadata::TrackInfo::new(
            &decision.track.title,
            &decision.track.artist,
            &decision.track.album,
        );
        let _ = session.send_track_info(&info).await;
    }

    *last_sent = Some(decision.track.clone());
}

async fn start(
    p: &StartParams,
) -> Result<
    (
        Running,
        Option<tokio::sync::watch::Receiver<nowplaying::NowPlaying>>,
        Option<mpsc::Receiver<raop::ap2::remote::Command>>,
    ),
    EngineError,
> {
    let latency = p.sync_latency_ms.max(MIN_SYNC_LATENCY_MS);
    let cfg = Ap2Config {
        target: p.device.addr,
        latency_frames: latency * 441 / 10,
        volume_db: pct_to_db(p.volume_pct),
        quiet_mode: false,
        ..Default::default()
    };
    let (source, _vol_ctrl) = make_source(p.volume_pct, p.capture_mode)
        .map_err(|e| EngineError::Capture(e.to_string()))?;
    #[cfg(target_os = "linux")]
    let (vol_tx, vol_rx) = tokio::sync::watch::channel(None);
    #[cfg(target_os = "linux")]
    if let Some(ref vc) = _vol_ctrl {
        let tx = vol_tx;
        vc.on_change(move |v| {
            let _ = tx.send(Some(v));
        });
    }
    let ring = pipeline::DEFAULT_RING_BUFFER_PACKETS;
    let metrics: pipeline::SharedPipelineMetrics =
        Arc::new(pipeline::PipelineMetricsTracker::new(ring));
    match Ap2Session::start_with_source(cfg, source, metrics.clone(), ring).await {
        Ok(mut session) => {
            let (watcher, rx) = if p.send_now_playing {
                let features = p
                    .device
                    .features
                    .as_deref()
                    .and_then(raop::ap2::metadata::Ap2Features::parse);
                session.apply_features(features);
                let w = nowplaying::NowPlayingWatcher::start();
                let rx = w.subscribe();
                (Some(w), Some(rx))
            } else {
                (None, None)
            };
            let mut running = Running {
                session,
                metrics,
                send_now_playing: p.send_now_playing,
                _now_playing_watcher: watcher,
                remote_control: p.remote_control,
                #[cfg(target_os = "linux")]
                vol_ctrl: _vol_ctrl,
                #[cfg(target_os = "linux")]
                vol_rx: Some(vol_rx),
            };
            let remote_rx = running.session.take_remote_commands();
            Ok((running, rx, remote_rx))
        }
        Err(Ap2Error::NotSupported(m)) => Err(EngineError::NotSupported(m)),
        Err(e) => Err(EngineError::Connect(e.to_string())),
    }
}

/// Checks (and with `fix`, repairs via UAC) the firewall for this program. Blocking.
#[cfg(windows)]
fn firewall_task(fix: bool) -> Firewall {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Firewall::Failed(FirewallError::Other(e.to_string())),
    };
    if fix {
        match netutil::firewall::install_elevated(&netutil::firewall::sibling_programs()) {
            Ok(true) => {}
            Ok(false) => return Firewall::Failed(FirewallError::Cancelled),
            Err(e) => return Firewall::Failed(FirewallError::InstallFailed(e.to_string())),
        }
    }
    match netutil::firewall::check(&exe) {
        Ok(st) if st.needs_fix() => Firewall::NeedsFix {
            blocked: st.blocked,
        },
        Ok(_) => Firewall::Ok,
        Err(e) => Firewall::Failed(FirewallError::CheckFailed(e.to_string())),
    }
}

#[cfg(not(windows))]
fn firewall_task(_fix: bool) -> Firewall {
    Firewall::NotApplicable
}

/// A SETUP that fails with 500 after a long wait is the classic symptom of Windows Firewall
/// dropping the receiver's timing requests.
#[allow(dead_code)]
pub fn looks_like_firewall_error(err: &EngineError) -> bool {
    match err {
        EngineError::Connect(msg) | EngineError::Interrupted(msg) => {
            msg.contains("SETUP") && msg.contains("500")
        }
        _ => false,
    }
}

fn nowplaying_command(command: raop::ap2::remote::Command) -> Option<nowplaying::MediaCommand> {
    use nowplaying::MediaCommand;
    use raop::ap2::remote::Command;
    Some(match command {
        Command::Play => MediaCommand::Play,
        // The HomePod never learns that the player paused (SETRATEANCHORTIME is rejected
        // for realtime streams), so Home Assistant keeps showing "pause" and sends `paus`
        // again to resume: treat it as a toggle on the player's actual state.
        Command::Pause => MediaCommand::PlayPause,
        Command::PlayPause => MediaCommand::PlayPause,
        Command::Next => MediaCommand::Next,
        Command::Previous => MediaCommand::Previous,
        Command::Volume(_) => return None,
    })
}

fn tracing_like_warn(msg: &str) {
    eprintln!("nyx-refrain: {msg}");
}

#[cfg(test)]
mod tests {
    use super::{EngineError, explain_not_local, pct_to_db};

    #[test]
    fn connect_errors_off_the_lan_get_the_vpn_hint() {
        // TEST-NET-1 is on no real interface; loopback always counts as local.
        let off_lan = "192.0.2.1:7000".parse().unwrap();
        let local = "127.0.0.1:7000".parse().unwrap();
        let err = || EngineError::Connect("timed out".into());
        assert_eq!(
            explain_not_local(off_lan, err()),
            EngineError::NotLocal("timed out".into())
        );
        assert_eq!(explain_not_local(local, err()), err());
        // Other errors are left alone.
        let capture = EngineError::Capture("x".into());
        assert_eq!(explain_not_local(off_lan, capture.clone()), capture);
    }

    #[test]
    fn volume_mapping() {
        assert_eq!(pct_to_db(0.0), -144.0);
        assert_eq!(pct_to_db(100.0), 0.0);
        assert_eq!(pct_to_db(150.0), 0.0);
        assert!((pct_to_db(50.0) + 15.0).abs() < 1e-6);
    }

    #[test]
    fn resume_prefers_discovered_device_then_saved_address() {
        use super::{DeviceEntry, resume_params};
        let settings = crate::settings::Settings {
            device_name: Some("Living Room".into()),
            device_addr: Some("192.0.2.106:7000".into()),
            volume_pct: 36.0,
            ..Default::default()
        };
        let discovered = DeviceEntry {
            name: "Living Room".into(),
            addr: "192.0.2.107:7000".parse().unwrap(),
            features: Some("0x4A7FCA00,0x3C354BD0".into()),
        };

        // Found by name: use discovery's address and features, even before the grace period.
        let p = resume_params(&settings, std::slice::from_ref(&discovered), false).unwrap();
        assert_eq!(p.device, discovered);
        assert_eq!(p.volume_pct, 36.0);

        // Not discovered: wait, then fall back to the saved address.
        assert!(resume_params(&settings, &[], false).is_none());
        let p = resume_params(&settings, &[], true).unwrap();
        assert_eq!(p.device.name, "Living Room");
        assert_eq!(p.device.addr, "192.0.2.106:7000".parse().unwrap());
        assert_eq!(p.device.features, None);

        // Nothing saved: nothing to resume.
        let empty = crate::settings::Settings::default();
        assert!(resume_params(&empty, &[], true).is_none());
    }
}
