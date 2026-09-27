//! System "now playing" info provider (title, artist, album, artwork, progress).
//!
//! Provides platform backends for Windows (WinRT GlobalSystemMediaTransportControlsSessionManager)
//! and Linux (MPRIS over D-Bus via zbus), with deduplication and change notifications.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, watch};

/// Artwork attached to a playing track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artwork {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Platform-neutral Now Playing metadata and playback state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork: Option<Artwork>,
    pub duration: Option<Duration>,
    pub position: Option<Duration>,
    pub playing: bool,
}

impl NowPlaying {
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.artist.is_empty() && self.album.is_empty()
    }
}

/// Embedded 1000x1000 JPEG placeholder album cover (45 KB).
pub const PLACEHOLDER_COVER_BYTES: &[u8] =
    include_bytes!("../../../assets/artwork/placeholder-cover.jpg");

/// 100-nanosecond intervals between January 1, 1601 (Windows epoch) and January 1, 1970 (Unix epoch).
pub const WINDOWS_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// Sanitizes a computer/host name by trimming whitespace and control characters.
pub fn sanitize_computer_name(raw: &str) -> String {
    raw.trim().to_string()
}

/// Chooses the first non-empty sanitized candidate from a list, or falls back to an empty string.
pub fn resolve_computer_name_from_candidates(candidates: &[Option<String>]) -> String {
    for s in candidates.iter().flatten() {
        let sanitized = sanitize_computer_name(s);
        if !sanitized.is_empty() {
            return sanitized;
        }
    }
    String::new()
}

#[cfg(windows)]
fn query_platform_computer_name() -> Option<String> {
    use windows::Win32::System::SystemInformation::{
        ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };
    use windows::core::PWSTR;

    let mut buf = vec![0u16; 256];
    let mut len = buf.len() as u32;
    let res = unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    if res.is_ok() {
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        let trimmed = sanitize_computer_name(&name);
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    } else if len > 0 {
        buf.resize(len as usize, 0);
        if unsafe {
            GetComputerNameExW(
                ComputerNamePhysicalDnsHostname,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .is_ok()
        {
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            let trimmed = sanitize_computer_name(&name);
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn query_platform_computer_name() -> Option<String> {
    if let Ok(content) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let trimmed = sanitize_computer_name(&content);
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let mut buf = [0u8; 256];
    let res = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if res == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        if let Ok(s) = std::str::from_utf8(&buf[..len]) {
            let trimmed = sanitize_computer_name(s);
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn query_platform_computer_name() -> Option<String> {
    None
}

/// Retrieves the computer/host name according to platform conventions with environment fallbacks:
/// - Windows: `GetComputerNameExW(ComputerNamePhysicalDnsHostname)` -> `COMPUTERNAME` env -> `""`
/// - Linux: `/proc/sys/kernel/hostname` or `libc::gethostname` -> `HOSTNAME` env -> `""`
pub fn computer_name() -> String {
    let platform = query_platform_computer_name();
    let env_var = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        std::env::var("HOSTNAME").ok()
    };
    let second_env_var = if cfg!(windows) {
        std::env::var("HOSTNAME").ok()
    } else {
        std::env::var("COMPUTERNAME").ok()
    };
    resolve_computer_name_from_candidates(&[platform, env_var, second_env_var])
}

/// Returns the placeholder `NowPlaying` state:
/// Title "Nyx Refrain", artist = computer name (empty if unavailable), album empty,
/// embedded placeholder cover artwork, and no progress.
pub fn placeholder() -> NowPlaying {
    placeholder_with_computer_name(&computer_name())
}

/// Constructs a placeholder `NowPlaying` with an explicit computer name (useful for unit tests).
pub fn placeholder_with_computer_name(name: &str) -> NowPlaying {
    let artist = sanitize_computer_name(name);
    NowPlaying {
        title: "Nyx Refrain".to_string(),
        artist,
        album: String::new(),
        artwork: Some(Artwork {
            mime: "image/jpeg".to_string(),
            bytes: PLACEHOLDER_COVER_BYTES.to_vec(),
        }),
        duration: None,
        position: None,
        playing: false,
    }
}

/// Inserts a JPEG comment (COM, `FF FE`) segment right after SOI. The image is unchanged;
/// only the bytes differ. Returns the input unchanged if it is not a JPEG.
pub fn with_jpeg_comment(jpeg: &[u8], comment: &[u8]) -> Vec<u8> {
    if !jpeg.starts_with(&[0xff, 0xd8]) || comment.len() > 0xfffd {
        return jpeg.to_vec();
    }
    let len = (comment.len() + 2) as u16;
    let mut out = Vec::with_capacity(jpeg.len() + comment.len() + 4);
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&[0xff, 0xfe]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(comment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// The placeholder cover with a unique comment each call. Receivers (HomePod / Home
/// Assistant) appear to ignore artwork identical to one they already showed, so returning to
/// the placeholder after a track kept the track's cover.
pub fn fresh_placeholder_artwork() -> Vec<u8> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    with_jpeg_comment(
        PLACEHOLDER_COVER_BYTES,
        format!("Nyx Refrain {n}").as_bytes(),
    )
}

/// Converts a Windows FILETIME / WinRT DateTime (100 ns ticks since 1601-01-01 UTC) to `SystemTime`.
/// Returns `None` if the timestamp precedes the Unix epoch or is non-positive.
pub fn filetime_to_system_time(ticks: i64) -> Option<SystemTime> {
    if ticks <= 0 {
        return None;
    }
    let u_ticks = ticks as u64;
    if u_ticks < WINDOWS_EPOCH_TICKS {
        return None;
    }
    let ticks_since_unix = u_ticks - WINDOWS_EPOCH_TICKS;
    let nanos_since_unix = (ticks_since_unix as u128) * 100;
    let secs = (nanos_since_unix / 1_000_000_000) as u64;
    let subsec_nanos = (nanos_since_unix % 1_000_000_000) as u32;
    Some(SystemTime::UNIX_EPOCH + Duration::new(secs, subsec_nanos))
}

/// Extrapolates SMTC position based on `LastUpdatedTime` and the current system clock.
///
/// When playing, reports `position + (now - LastUpdatedTime)` clamped to `[0, duration]`.
/// When paused or if `last_updated_ticks` is invalid, returns `position` clamped to `[0, duration]`.
pub fn extrapolate_position(
    position: Duration,
    last_updated_ticks: i64,
    now: SystemTime,
    duration: Option<Duration>,
    playing: bool,
) -> Duration {
    let mut pos = position;
    if playing
        && let Some(last_updated) = filetime_to_system_time(last_updated_ticks)
        && let Ok(elapsed) = now.duration_since(last_updated)
    {
        pos += elapsed;
    }
    if let Some(dur) = duration {
        pos = pos.min(dur);
    }
    pos
}

/// Action decision for publishing now-playing metadata to an AirPlay receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataDecision {
    /// The target track state that should be represented on the receiver.
    pub track: NowPlaying,
    /// Whether text metadata (title, artist, album) should be sent over SET_PARAMETER.
    pub send_track_info: bool,
    /// Whether artwork should be sent over SET_PARAMETER.
    pub send_artwork: bool,
    /// Whether the target track is the placeholder now-playing.
    pub is_placeholder: bool,
    /// Whether the artwork is the placeholder cover (the placeholder itself, or a track without
    /// artwork of its own); send it as [`fresh_placeholder_artwork`].
    pub placeholder_artwork: bool,
}

impl MetadataDecision {
    /// Whether any SET_PARAMETER request needs to be issued for this decision.
    pub fn has_action(&self) -> bool {
        self.send_track_info || self.send_artwork
    }
}

/// Decides what metadata to send given the previously sent track state, the latest candidate from
/// the system watcher, and the user toggle (e.g. GUI "send now playing" checkbox or CLI `--now-playing`).
pub fn decide_metadata_send(
    prev: Option<&NowPlaying>,
    current: Option<&NowPlaying>,
    toggle: bool,
) -> MetadataDecision {
    decide_metadata_send_with_placeholder(prev, current, toggle, &placeholder())
}

/// Decision function with an explicit placeholder (enables fully deterministic unit testing).
pub fn decide_metadata_send_with_placeholder(
    prev: Option<&NowPlaying>,
    current: Option<&NowPlaying>,
    toggle: bool,
    placeholder: &NowPlaying,
) -> MetadataDecision {
    let (mut target, is_placeholder) = if toggle
        && let Some(np) = current
        && !np.is_empty()
    {
        (np.clone(), false)
    } else {
        (placeholder.clone(), true)
    };
    // A track without artwork (none, or it failed to load) shows the placeholder cover instead
    // of leaving the previous track's cover on the receiver.
    let placeholder_artwork = is_placeholder || target.artwork.is_none();
    if target.artwork.is_none() {
        target.artwork = placeholder.artwork.clone();
    }

    let track_changed = match prev {
        Some(p) => p.title != target.title || p.artist != target.artist || p.album != target.album,
        None => true,
    };
    let artwork_changed = match prev {
        Some(p) => p.artwork != target.artwork,
        None => true,
    };

    let send_track_info = track_changed
        && (!target.title.is_empty() || !target.artist.is_empty() || !target.album.is_empty());
    let send_artwork = artwork_changed && target.artwork.is_some();
    // No progress is sent: only Apple Music reports a duration, and receivers then keep a stale progress bar.

    MetadataDecision {
        track: target,
        send_track_info,
        send_artwork,
        is_placeholder,
        placeholder_artwork,
    }
}

/// One-line human-readable summary of a now-playing state, for diagnostics
/// (`nyxr now-playing`): title, artist, album, artwork type/size/fingerprint, position,
/// duration and playback state.
pub fn describe(np: &NowPlaying, timestamp: String) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let art = match &np.artwork {
        Some(a) => {
            let mut h = DefaultHasher::new();
            a.bytes.hash(&mut h);
            format!("{} {}B #{:08x}", a.mime, a.bytes.len(), h.finish() as u32)
        }
        None => "none".into(),
    };
    let fmt = |d: Option<Duration>| match d {
        Some(d) => format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60),
        None => "-".into(),
    };
    format!(
        "{timestamp} {} title={:?} artist={:?} album={:?} art=[{art}] pos={} dur={}",
        if np.playing { "playing" } else { "paused " },
        np.title,
        np.artist,
        np.album,
        fmt(np.position),
        fmt(np.duration),
    )
}

/// Decides whether a polled state represents a meaningful change that should be published.
pub fn should_publish(
    prev: &Option<NowPlaying>,
    prev_time: Instant,
    next: &NowPlaying,
    now: Instant,
) -> bool {
    let Some(p) = prev else {
        return !next.is_empty();
    };

    if p.title != next.title || p.artist != next.artist || p.album != next.album {
        return true;
    }
    if p.artwork != next.artwork {
        return true;
    }
    if p.playing != next.playing {
        return true;
    }
    if p.duration != next.duration {
        return true;
    }

    if next.playing {
        // Detect seek (deviation > 2s from linear progression)
        if let (Some(p_pos), Some(n_pos)) = (p.position, next.position) {
            let elapsed = now.duration_since(prev_time);
            let expected = p_pos + elapsed;
            let diff = n_pos.abs_diff(expected);
            if diff > Duration::from_secs(2) {
                return true;
            }
        }

        // Periodic progress publish every 5 seconds while playing
        if now.duration_since(prev_time) >= Duration::from_secs(5) {
            return true;
        }
    }

    false
}

/// Playback actions directed at the session selected by a now-playing watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    Seek(Duration),
}

/// Resolves `PlayPause` from the player's reported state instead of relying on its own
/// toggle (some SMTC/MPRIS players do not implement one): playing → `Pause`, else `Play`.
fn resolve_toggle(command: MediaCommand, playing: bool) -> MediaCommand {
    match command {
        MediaCommand::PlayPause if playing => MediaCommand::Pause,
        MediaCommand::PlayPause => MediaCommand::Play,
        other => other,
    }
}

/// Handle to a running background now-playing watcher.
pub struct NowPlayingWatcher {
    rx: watch::Receiver<NowPlaying>,
    commands: mpsc::Sender<MediaCommand>,
    stop: Arc<AtomicBool>,
}

impl NowPlayingWatcher {
    /// Starts polling the system for now playing information.
    pub fn start() -> Self {
        let (tx, rx) = watch::channel(NowPlaying::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, command_rx) = mpsc::channel(16);

        #[cfg(target_os = "linux")]
        {
            let s = stop.clone();
            tokio::spawn(mpris::run_mpris_watcher(tx, s, command_rx));
        }

        #[cfg(windows)]
        {
            let s = stop.clone();
            std::thread::Builder::new()
                .name("nowplaying-win".into())
                .spawn(move || {
                    win::run_win_watcher(tx, s, command_rx);
                })
                .expect("spawn nowplaying thread");
        }

        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (tx, command_rx);
        }

        Self { rx, commands, stop }
    }

    /// Queue an action without blocking the caller. Unsupported actions are logged by the backend.
    pub fn control(&self, command: MediaCommand) -> bool {
        if self.commands.try_send(command).is_err() {
            tracing::debug!(
                ?command,
                "nowplaying: control queue full or watcher unavailable"
            );
            return false;
        }
        true
    }

    /// Obtains a watch receiver for now playing state changes.
    pub fn subscribe(&self) -> watch::Receiver<NowPlaying> {
        self.rx.clone()
    }
}

impl Drop for NowPlayingWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Helper to parse and percent-decode a `file://` URL into a local filesystem path.
pub fn parse_file_url(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path_str = if let Some(stripped) = rest.strip_prefix("localhost") {
        stripped
    } else {
        rest
    };
    let decoded = percent_decode(path_str)?;
    Some(std::path::PathBuf::from(decoded))
}

/// Pure percent-decoder (e.g. `%20` -> ` `).
pub fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = &input[i + 1..i + 3];
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Maximum artwork size handed to the receiver: 1 MiB.
pub const MAX_ARTWORK_BYTES: usize = 1024 * 1024;

/// Largest source image read from the OS media session / file (before [`fit_artwork`]).
pub const MAX_SOURCE_ARTWORK_BYTES: usize = 16 * 1024 * 1024;

/// Longest edge of artwork sent to receivers. Larger covers (e.g. 2560×2560 originals from
/// foobar2000) are downscaled; receivers and Home Assistant cope badly with huge images.
pub const MAX_ARTWORK_EDGE: u32 = 1000;

/// Image type from magic bytes (JPEG or PNG), regardless of size.
pub fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else {
        None
    }
}

/// Sniff image type from magic bytes (JPEG or PNG only, <= 1 MiB).
pub fn sniff_image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    image_type(bytes)
}

/// Makes source artwork receiver-friendly: JPEG/PNG within [`MAX_ARTWORK_EDGE`] and
/// [`MAX_ARTWORK_BYTES`] is passed through unchanged; anything larger is downscaled to fit
/// and re-encoded as JPEG. Returns `None` for unsupported or undecodable images.
pub fn fit_artwork(bytes: &[u8]) -> Option<Artwork> {
    use image::ImageDecoder;
    let mime = image_type(bytes)?;
    let format = if mime == "image/png" {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    let (width, height) = {
        let reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
        reader.into_decoder().ok()?.dimensions()
    };
    if width <= MAX_ARTWORK_EDGE && height <= MAX_ARTWORK_EDGE && bytes.len() <= MAX_ARTWORK_BYTES {
        return Some(Artwork {
            mime: mime.to_string(),
            bytes: bytes.to_vec(),
        });
    }
    let decoded = image::load_from_memory_with_format(bytes, format).ok()?;
    let scaled = decoded
        .resize(
            MAX_ARTWORK_EDGE,
            MAX_ARTWORK_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
        .to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .encode_image(&scaled)
        .ok()?;
    (out.len() <= MAX_ARTWORK_BYTES).then(|| Artwork {
        mime: "image/jpeg".into(),
        bytes: out,
    })
}

/// Remembers the last [`fit_artwork`] result so a watcher polling every second decodes and
/// scales a given cover only once.
#[derive(Default)]
pub struct ArtworkCache {
    key: Option<u64>,
    value: Option<Artwork>,
}

impl ArtworkCache {
    pub fn fit(&mut self, source: Option<Artwork>) -> Option<Artwork> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let source = source?;
        let mut h = DefaultHasher::new();
        source.bytes.hash(&mut h);
        let key = h.finish();
        if self.key != Some(key) {
            self.key = Some(key);
            self.value = fit_artwork(&source.bytes);
        }
        self.value.clone()
    }
}

/// Whether an `mpris:artUrl` points at a remote cover (fetched in the background on Linux).
pub fn is_remote_art_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Loads local artwork from an MPRIS `mpris:artUrl`: `file://` paths or base64 `data:` URIs
/// (e.g. Telegram Desktop). Remote `http(s)://` URLs are handled by the MPRIS watcher.
pub fn load_art_url(url: &str) -> Option<Artwork> {
    if url.starts_with("data:") {
        load_data_url_artwork(url)
    } else {
        load_file_artwork(url)
    }
}

/// Decodes a base64 `data:` URI if valid JPEG or PNG and <= 16 MiB (see [`fit_artwork`]).
pub fn load_data_url_artwork(url: &str) -> Option<Artwork> {
    use base64::Engine as _;
    let (header, payload) = url.strip_prefix("data:")?.split_once(',')?;
    if !header.ends_with(";base64") || payload.len() > MAX_SOURCE_ARTWORK_BYTES / 3 * 4 + 4 {
        return None;
    }
    let payload: String = payload
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    let mime = image_type(&bytes)?;
    Some(Artwork {
        mime: mime.to_string(),
        bytes,
    })
}

/// Loads artwork from a `file://` URL if valid JPEG or PNG and <= 16 MiB (see [`fit_artwork`]).
pub fn load_file_artwork(url: &str) -> Option<Artwork> {
    let path = parse_file_url(url)?;
    if !path.is_file() {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_SOURCE_ARTWORK_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let mime = image_type(&bytes)?;
    Some(Artwork {
        mime: mime.to_string(),
        bytes,
    })
}

#[cfg(target_os = "linux")]
mod mpris {
    use super::*;
    use tracing::debug;

    pub async fn run_mpris_watcher(
        tx: watch::Sender<NowPlaying>,
        stop: Arc<AtomicBool>,
        mut commands: mpsc::Receiver<MediaCommand>,
    ) {
        let Ok(conn) = zbus::Connection::session().await else {
            debug!("nowplaying: unable to connect to session D-Bus");
            return;
        };

        let mut prev: Option<NowPlaying> = None;
        let mut prev_time = Instant::now();
        let mut artwork = ArtworkCache::default();
        let mut remote = RemoteArtwork::default();
        let mut selected = None;
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !stop.load(Ordering::Relaxed) {
            tokio::select! {
                _ = poll.tick() => {
                    let player = tokio::time::timeout(Duration::from_secs(3), poll_mpris(&conn))
                        .await.ok().flatten();
                    let mut current = match player {
                        Some((owner, info, remote_url)) => {
                            selected = Some(owner);
                            let mut info = info;
                            if let Some(url) = remote_url {
                                info.artwork = remote.get(&url).await;
                            }
                            info
                        }
                        None => {
                            selected = None;
                            NowPlaying::default()
                        }
                    };
                    current.artwork = artwork.fit(current.artwork.take());
                    let now = Instant::now();
                    if should_publish(&prev, prev_time, &current, now) {
                        prev = Some(current.clone());
                        prev_time = now;
                        let _ = tx.send(current);
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    if stop.load(Ordering::Relaxed) { break }
                    let Some(owner) = selected.as_deref() else {
                        debug!(?command, "nowplaying: no selected MPRIS session; ignoring control");
                        continue;
                    };
                    match tokio::time::timeout(Duration::from_secs(3), control(&conn, owner, command)).await {
                        Ok(Ok(true)) => debug!(?command, "nowplaying: MPRIS command sent"),
                        Ok(Ok(false)) => debug!(?command, "nowplaying: MPRIS action unsupported or duration/track unavailable; ignored"),
                        Ok(Err(e)) => debug!(?command, "nowplaying: MPRIS control failed: {e}"),
                        Err(_) => debug!(?command, "nowplaying: MPRIS control timed out"),
                    }
                }
            }
        }
    }

    /// How long a failed remote cover download waits before being retried.
    const REMOTE_RETRY: Duration = Duration::from_secs(15);

    /// Remote `mpris:artUrl` cover for the current URL, downloaded on a blocking thread so the
    /// 1 s poll never waits on the network. Artwork is `None` until the download finishes.
    #[derive(Default)]
    struct RemoteArtwork {
        url: String,
        value: Option<Artwork>,
        failed_at: Option<Instant>,
        pending: Option<tokio::task::JoinHandle<Option<Artwork>>>,
    }

    impl RemoteArtwork {
        async fn get(&mut self, url: &str) -> Option<Artwork> {
            if self.url != url {
                *self = Self {
                    url: url.to_string(),
                    ..Self::default()
                };
            }
            if self.pending.as_ref().is_some_and(|h| h.is_finished()) {
                let result = self.pending.take()?.await.ok().flatten();
                self.failed_at = result.is_none().then(Instant::now);
                if result.is_none() {
                    debug!(url, "nowplaying: remote artwork download failed");
                }
                self.value = result;
            }
            let retry = self.failed_at.is_none_or(|t| t.elapsed() >= REMOTE_RETRY);
            if self.value.is_none() && self.pending.is_none() && retry {
                let url = url.to_string();
                self.failed_at = None;
                self.pending = Some(tokio::task::spawn_blocking(move || fetch_artwork(&url)));
            }
            self.value.clone()
        }
    }

    /// Downloads a JPEG or PNG cover, at most [`MAX_SOURCE_ARTWORK_BYTES`], within 10 s.
    fn fetch_artwork(url: &str) -> Option<Artwork> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let bytes = agent
            .get(url)
            .call()
            .ok()?
            .body_mut()
            .with_config()
            .limit(MAX_SOURCE_ARTWORK_BYTES as u64)
            .read_to_vec()
            .ok()?;
        let mime = image_type(&bytes)?;
        Some(Artwork {
            mime: mime.to_string(),
            bytes,
        })
    }

    /// Selected player's owner, its info, and a remote `mpris:artUrl` still to be fetched.
    async fn poll_mpris(conn: &zbus::Connection) -> Option<(String, NowPlaying, Option<String>)> {
        let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;
        let names = dbus.list_names().await.ok()?;
        let mut players = Vec::new();
        for name in names {
            if name.starts_with("org.mpris.MediaPlayer2.")
                && let Ok(owner) = dbus.get_name_owner(name.into()).await
                && let Ok((info, remote_url)) = read_player(conn, owner.as_str()).await
            {
                // A unique owner cannot be redirected to a replacement process after selection.
                players.push((owner.to_string(), info, remote_url));
            }
        }
        // Prefer a playing session, otherwise keep the existing first-session policy.
        let index = players.iter().position(|(_, p, _)| p.playing).unwrap_or(0);
        (!players.is_empty()).then(|| players.swap_remove(index))
    }

    async fn control(
        conn: &zbus::Connection,
        owner: &str,
        command: MediaCommand,
    ) -> Result<bool, zbus::Error> {
        let proxy = zbus::Proxy::new(
            conn,
            owner,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .await?;
        if !proxy
            .get_property::<bool>("CanControl")
            .await
            .unwrap_or(false)
        {
            return Ok(false);
        }
        let playing = proxy
            .get_property::<String>("PlaybackStatus")
            .await
            .is_ok_and(|s| s == "Playing");
        let command = resolve_toggle(command, playing);
        let (method, capability) = match command {
            MediaCommand::Play => ("Play", "CanPlay"),
            MediaCommand::Pause => ("Pause", "CanPause"),
            MediaCommand::PlayPause => ("PlayPause", "CanPause"),
            MediaCommand::Next => ("Next", "CanGoNext"),
            MediaCommand::Previous => ("Previous", "CanGoPrevious"),
            MediaCommand::Seek(_) => ("SetPosition", "CanSeek"),
        };
        if !proxy
            .get_property::<bool>(capability)
            .await
            .unwrap_or(false)
        {
            return Ok(false);
        }
        if let MediaCommand::Seek(position) = command {
            let meta: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
                proxy.get_property("Metadata").await?;
            let Some(length) = meta
                .get("mpris:length")
                .and_then(extract_i64)
                .filter(|l| *l > 0)
            else {
                return Ok(false);
            };
            let Some(track) = meta
                .get("mpris:trackid")
                .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok())
            else {
                return Ok(false);
            };
            if track.as_str() == "/org/mpris/MediaPlayer2/TrackList/NoTrack" {
                return Ok(false);
            }
            let position = position.as_micros().min(length as u128) as i64;
            proxy.call::<_, _, ()>(method, &(track, position)).await?;
        } else {
            proxy.call::<_, _, ()>(method, &()).await?;
        }
        Ok(true)
    }

    async fn read_player(
        conn: &zbus::Connection,
        name: &str,
    ) -> Result<(NowPlaying, Option<String>), zbus::Error> {
        let proxy = zbus::Proxy::new(
            conn,
            name,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .await?;

        let status: String = proxy
            .get_property("PlaybackStatus")
            .await
            .unwrap_or_default();
        let playing = status == "Playing";

        let pos_us: i64 = proxy.get_property("Position").await.unwrap_or(-1);
        let position = if pos_us >= 0 {
            Some(Duration::from_micros(pos_us as u64))
        } else {
            None
        };

        let meta: std::collections::HashMap<String, zbus::zvariant::OwnedValue> =
            proxy.get_property("Metadata").await.unwrap_or_default();

        let title = meta
            .get("xesam:title")
            .and_then(extract_str)
            .unwrap_or_default();
        let artist = meta
            .get("xesam:artist")
            .and_then(extract_artist)
            .unwrap_or_default();
        let album = meta
            .get("xesam:album")
            .and_then(extract_str)
            .unwrap_or_default();

        let duration = meta
            .get("mpris:length")
            .and_then(extract_i64)
            .filter(|&l| l > 0)
            .map(|l| Duration::from_micros(l as u64));

        let art_url = meta.get("mpris:artUrl").and_then(extract_str);
        let (artwork, remote_url) = match art_url {
            Some(url) if is_remote_art_url(&url) => (None, Some(url)),
            Some(url) => (load_art_url(&url), None),
            None => (None, None),
        };

        Ok((
            NowPlaying {
                title,
                artist,
                album,
                artwork,
                duration,
                position,
                playing,
            },
            remote_url,
        ))
    }

    fn extract_str(v: &zbus::zvariant::OwnedValue) -> Option<String> {
        if let Ok(s) = <String>::try_from(v.clone()) {
            Some(s)
        } else if let Ok(s) = <&str>::try_from(v) {
            Some(s.to_string())
        } else {
            None
        }
    }

    fn extract_artist(v: &zbus::zvariant::OwnedValue) -> Option<String> {
        if let Ok(list) = <Vec<String>>::try_from(v.clone()) {
            Some(list.join(", "))
        } else if let Ok(s) = <String>::try_from(v.clone()) {
            Some(s)
        } else if let Ok(s) = <&str>::try_from(v) {
            Some(s.to_string())
        } else {
            None
        }
    }

    fn extract_i64(v: &zbus::zvariant::OwnedValue) -> Option<i64> {
        if let Ok(val) = <i64>::try_from(v.clone()) {
            Some(val)
        } else if let Ok(val) = <u64>::try_from(v.clone()) {
            Some(val as i64)
        } else {
            None
        }
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };
    use windows::Storage::Streams::DataReader;

    pub fn run_win_watcher(
        tx: watch::Sender<NowPlaying>,
        stop: Arc<AtomicBool>,
        mut commands: mpsc::Receiver<MediaCommand>,
    ) {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
        }

        let mut prev: Option<NowPlaying> = None;
        let mut prev_time = Instant::now();
        let mut artwork = ArtworkCache::default();

        let mut selected = None;
        let mut next_poll = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            if Instant::now() >= next_poll {
                let mut current = match poll_windows_media() {
                    Some((session, info)) => {
                        selected = Some(session);
                        info
                    }
                    None => {
                        selected = None;
                        NowPlaying::default()
                    }
                };
                current.artwork = artwork.fit(current.artwork.take());
                let now = Instant::now();
                if should_publish(&prev, prev_time, &current, now) {
                    prev = Some(current.clone());
                    prev_time = now;
                    let _ = tx.send(current);
                }
                next_poll = now + Duration::from_secs(1);
            }
            while let Ok(command) = commands.try_recv() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Some(session) = &selected else {
                    tracing::debug!(
                        ?command,
                        "nowplaying: no selected SMTC session; ignoring control"
                    );
                    continue;
                };
                match control(session, command) {
                    Ok(true) => tracing::debug!(?command, "nowplaying: SMTC command sent"),
                    Ok(false) => tracing::debug!(
                        ?command,
                        "nowplaying: SMTC action unsupported, rejected, or duration unavailable; ignored"
                    ),
                    Err(e) => tracing::debug!(?command, "nowplaying: SMTC control failed: {e}"),
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }

    fn poll_windows_media() -> Option<(GlobalSystemMediaTransportControlsSession, NowPlaying)> {
        let op = GlobalSystemMediaTransportControlsSessionManager::RequestAsync().ok()?;
        let manager = op.get().ok()?;
        let session = manager.GetCurrentSession().ok()?;
        let mut np = NowPlaying::default();

        if let Ok(op) = session.TryGetMediaPropertiesAsync()
            && let Ok(props) = op.get()
        {
            if let Ok(t) = props.Title() {
                np.title = t.to_string();
            }
            if let Ok(a) = props.Artist() {
                np.artist = a.to_string();
            }
            if let Ok(al) = props.AlbumTitle() {
                np.album = al.to_string();
            }
            if let Ok(thumb_ref) = props.Thumbnail()
                && let Ok(stream_op) = thumb_ref.OpenReadAsync()
                && let Ok(stream) = stream_op.get()
            {
                let size = stream.Size().unwrap_or(0) as usize;
                if size > 0
                    && size <= MAX_SOURCE_ARTWORK_BYTES
                    && let Ok(reader) = DataReader::CreateDataReader(&stream)
                    && let Ok(load_op) = reader.LoadAsync(size as u32)
                    && load_op.get().is_ok()
                {
                    let content_type = stream
                        .ContentType()
                        .map(|h| h.to_string())
                        .unwrap_or_default();
                    let mut bytes = vec![0u8; size];
                    if reader.ReadBytes(&mut bytes).is_ok() {
                        // Trust the bytes: SMTC sources report lists such as
                        // "image/jpeg,image/jpe,image/jpg" (foobar2000).
                        let mime = match image_type(&bytes) {
                            Some(m) => m.to_string(),
                            None if content_type.starts_with("image/") => content_type
                                .split(',')
                                .next()
                                .unwrap_or_default()
                                .trim()
                                .to_string(),
                            None => String::new(),
                        };
                        if !mime.is_empty() {
                            np.artwork = Some(Artwork { mime, bytes });
                        }
                    }
                }
            }
        }

        if let Ok(playback) = session.GetPlaybackInfo()
            && let Ok(status) = playback.PlaybackStatus()
        {
            np.playing = status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        }

        if let Ok(timeline) = session.GetTimelineProperties() {
            let raw_pos = timeline.Position().ok().map(|pos| {
                let nanos = (pos.Duration * 100).max(0) as u64;
                Duration::from_nanos(nanos)
            });
            if let Ok(end) = timeline.EndTime() {
                let end_nanos = (end.Duration * 100).max(0) as u64;
                if end_nanos > 0 {
                    let start_nanos = timeline
                        .StartTime()
                        .map(|s| (s.Duration * 100).max(0) as u64)
                        .unwrap_or(0);
                    let dur_nanos = end_nanos.saturating_sub(start_nanos);
                    np.duration = Some(Duration::from_nanos(dur_nanos));
                }
            }
            if let Some(pos) = raw_pos {
                let last_updated = timeline.LastUpdatedTime().ok().map(|dt| dt.UniversalTime);
                np.position = Some(match last_updated {
                    Some(ticks) => {
                        extrapolate_position(pos, ticks, SystemTime::now(), np.duration, np.playing)
                    }
                    None => match np.duration {
                        Some(dur) => pos.min(dur),
                        None => pos,
                    },
                });
            }
        }

        Some((session, np))
    }

    fn control(
        session: &GlobalSystemMediaTransportControlsSession,
        command: MediaCommand,
    ) -> windows::core::Result<bool> {
        let info = session.GetPlaybackInfo()?;
        let playing = info.PlaybackStatus()?
            == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        let command = resolve_toggle(command, playing);
        let controls = info.Controls()?;
        let enabled = match command {
            MediaCommand::Play => controls.IsPlayEnabled()?,
            MediaCommand::Pause => controls.IsPauseEnabled()?,
            MediaCommand::PlayPause => controls.IsPlayPauseToggleEnabled()?,
            MediaCommand::Next => controls.IsNextEnabled()?,
            MediaCommand::Previous => controls.IsPreviousEnabled()?,
            MediaCommand::Seek(_) => controls.IsPlaybackPositionEnabled()?,
        };
        if !enabled {
            return Ok(false);
        }
        let op = match command {
            MediaCommand::Play => session.TryPlayAsync()?,
            MediaCommand::Pause => session.TryPauseAsync()?,
            MediaCommand::PlayPause => session.TryTogglePlayPauseAsync()?,
            MediaCommand::Next => session.TrySkipNextAsync()?,
            MediaCommand::Previous => session.TrySkipPreviousAsync()?,
            MediaCommand::Seek(position) => {
                let timeline = session.GetTimelineProperties()?;
                let start = timeline.StartTime()?.Duration;
                let end = timeline.EndTime()?.Duration;
                if start < 0 || end <= start {
                    return Ok(false);
                }
                let offset = (position.as_nanos() / 100).min((end - start) as u128) as i64;
                session.TryChangePlaybackPositionAsync(start + offset)?
            }
        };
        op.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_tests() {
        assert_eq!(percent_decode("hello%20world"), Some("hello world".into()));
        assert_eq!(
            percent_decode("%2Fpath%2Fto%2Ffile"),
            Some("/path/to/file".into())
        );
        assert_eq!(percent_decode("plain_text"), Some("plain_text".into()));
        assert_eq!(percent_decode("%ZZ_invalid"), None);
        assert_eq!(percent_decode("%2"), None);
    }

    #[test]
    fn parse_file_url_tests() {
        assert_eq!(
            parse_file_url("file:///home/user/Music/track.mp3"),
            Some(std::path::PathBuf::from("/home/user/Music/track.mp3"))
        );
        assert_eq!(
            parse_file_url("file://localhost/home/user/Music/album%20art.jpg"),
            Some(std::path::PathBuf::from("/home/user/Music/album art.jpg"))
        );
        assert_eq!(parse_file_url("https://example.com/art.jpg"), None);
        assert_eq!(parse_file_url("http://example.com/art.jpg"), None);
    }

    #[test]
    fn data_url_artwork_tests() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(PLACEHOLDER_COVER_BYTES);
        let art = load_art_url(&format!("data:image/jpeg;base64,{b64}")).unwrap();
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(art.bytes, PLACEHOLDER_COVER_BYTES);
        assert_eq!(load_art_url("data:image/jpeg;base64,!!!"), None);
        assert_eq!(load_art_url("data:text/plain,hello"), None);
        assert_eq!(load_art_url("data:image/gif;base64,R0lGODlh"), None);
        assert_eq!(load_art_url("https://example.com/art.jpg"), None);
    }

    #[test]
    fn image_sniff_tests() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(sniff_image_type(&jpeg), Some("image/jpeg"));

        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        assert_eq!(sniff_image_type(&png), Some("image/png"));

        let gif = b"GIF89a";
        assert_eq!(sniff_image_type(gif), None);

        let oversized = vec![0xff; MAX_ARTWORK_BYTES + 1];
        assert_eq!(sniff_image_type(&oversized), None);
    }

    #[test]
    fn deduplication_tests() {
        let t0 = Instant::now();
        let track1 = NowPlaying {
            title: "Track 1".into(),
            artist: "Artist 1".into(),
            album: "Album 1".into(),
            artwork: None,
            duration: Some(Duration::from_secs(200)),
            position: Some(Duration::from_secs(10)),
            playing: true,
        };

        // First publication
        assert!(should_publish(&None, t0, &track1, t0));

        let prev = Some(track1.clone());

        // Linear playback at +1s should NOT publish
        let t1 = t0 + Duration::from_secs(1);
        let track1_1s = NowPlaying {
            position: Some(Duration::from_secs(11)),
            ..track1.clone()
        };
        assert!(!should_publish(&prev, t0, &track1_1s, t1));

        // Periodic update at +5s SHOULD publish
        let t5 = t0 + Duration::from_secs(5);
        let track1_5s = NowPlaying {
            position: Some(Duration::from_secs(15)),
            ..track1.clone()
        };
        assert!(should_publish(&prev, t0, &track1_5s, t5));

        // Seek (jumping from 10s to 50s at +1s) SHOULD publish
        let track1_seek = NowPlaying {
            position: Some(Duration::from_secs(50)),
            ..track1.clone()
        };
        assert!(should_publish(&prev, t0, &track1_seek, t1));

        // Pause state change SHOULD publish
        let track1_paused = NowPlaying {
            playing: false,
            ..track1.clone()
        };
        assert!(should_publish(&prev, t0, &track1_paused, t1));

        // Track title change SHOULD publish
        let track2 = NowPlaying {
            title: "Track 2".into(),
            ..track1.clone()
        };
        assert!(should_publish(&prev, t0, &track2, t1));
    }

    #[test]
    fn computer_name_and_placeholder_tests() {
        assert_eq!(sanitize_computer_name("   "), "");
        assert_eq!(sanitize_computer_name("  MyPC \r\n"), "MyPC");

        let candidates = vec![
            None,
            Some("  \t\n".into()),
            Some("  AlphaHost  \n".into()),
            Some("BetaHost".into()),
        ];
        assert_eq!(
            resolve_computer_name_from_candidates(&candidates),
            "AlphaHost"
        );

        let empty_candidates: Vec<Option<String>> = vec![None, Some("".into())];
        assert_eq!(resolve_computer_name_from_candidates(&empty_candidates), "");

        let p = placeholder_with_computer_name("  TestMachine  ");
        assert_eq!(p.title, "Nyx Refrain");
        assert_eq!(p.artist, "TestMachine");
        assert_eq!(p.album, "");
        assert!(p.duration.is_none());
        assert!(p.position.is_none());
        assert!(!p.playing);
        let art = p.artwork.expect("placeholder has artwork");
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(art.bytes, PLACEHOLDER_COVER_BYTES);

        let p_empty = placeholder_with_computer_name("");
        assert_eq!(p_empty.artist, "");

        let p_default = placeholder();
        assert_eq!(p_default.title, "Nyx Refrain");
        assert!(p_default.artwork.is_some());
    }

    #[test]
    fn smtc_extrapolation_tests() {
        assert_eq!(filetime_to_system_time(0), None);
        assert_eq!(filetime_to_system_time(-500), None);
        assert_eq!(filetime_to_system_time(100), None);

        // Windows epoch tick matching Unix epoch
        let unix_epoch = filetime_to_system_time(WINDOWS_EPOCH_TICKS as i64);
        assert_eq!(unix_epoch, Some(SystemTime::UNIX_EPOCH));

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let three_secs_ago = now - Duration::from_secs(3);
        let elapsed_since_unix = three_secs_ago
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let ticks_3s_ago =
            (WINDOWS_EPOCH_TICKS + (elapsed_since_unix.as_nanos() / 100) as u64) as i64;

        // 1. Playing: 10s + 3s elapsed = 13s
        let extrapolated = extrapolate_position(
            Duration::from_secs(10),
            ticks_3s_ago,
            now,
            Some(Duration::from_secs(100)),
            true,
        );
        assert_eq!(extrapolated, Duration::from_secs(13));

        // 2. Clamped to duration (duration = 12s < 13s)
        let clamped = extrapolate_position(
            Duration::from_secs(10),
            ticks_3s_ago,
            now,
            Some(Duration::from_secs(12)),
            true,
        );
        assert_eq!(clamped, Duration::from_secs(12));

        // 3. Paused: no extrapolation
        let paused = extrapolate_position(
            Duration::from_secs(10),
            ticks_3s_ago,
            now,
            Some(Duration::from_secs(100)),
            false,
        );
        assert_eq!(paused, Duration::from_secs(10));

        // 4. Future timestamp (clock skew / jitter): reports snapshot position
        let one_sec_future = now + Duration::from_secs(1);
        let future_elapsed = one_sec_future
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let ticks_future = (WINDOWS_EPOCH_TICKS + (future_elapsed.as_nanos() / 100) as u64) as i64;
        let future_pos = extrapolate_position(
            Duration::from_secs(10),
            ticks_future,
            now,
            Some(Duration::from_secs(100)),
            true,
        );
        assert_eq!(future_pos, Duration::from_secs(10));

        // 5. Invalid tick
        let invalid = extrapolate_position(
            Duration::from_secs(10),
            -1,
            now,
            Some(Duration::from_secs(100)),
            true,
        );
        assert_eq!(invalid, Duration::from_secs(10));
    }

    #[test]
    fn smtc_extrapolation_with_should_publish_does_not_seek_every_poll() {
        let t0_instant = Instant::now();
        let t0_system = SystemTime::now();

        let track_base = NowPlaying {
            title: "Extrapolated Track".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            artwork: None,
            duration: Some(Duration::from_secs(300)),
            position: None,
            playing: true,
        };

        // Snapshot taken at t0_system
        let snapshot_pos = Duration::from_secs(20);
        let unix_dur = t0_system.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let last_updated_ticks = (WINDOWS_EPOCH_TICKS + (unix_dur.as_nanos() / 100) as u64) as i64;

        // Poll at t = 0s
        let pos0 = extrapolate_position(
            snapshot_pos,
            last_updated_ticks,
            t0_system,
            track_base.duration,
            track_base.playing,
        );
        let mut cur = NowPlaying {
            position: Some(pos0),
            ..track_base.clone()
        };
        assert!(should_publish(&None, t0_instant, &cur, t0_instant));
        let mut prev = Some(cur.clone());
        let mut prev_time = t0_instant;

        // Polls at 1s, 2s, 3s, 4s: must NOT publish (neither seek nor 5s periodic)
        for s in 1..=4 {
            let t_inst = t0_instant + Duration::from_secs(s);
            let t_sys = t0_system + Duration::from_secs(s);
            let pos = extrapolate_position(
                snapshot_pos,
                last_updated_ticks,
                t_sys,
                track_base.duration,
                track_base.playing,
            );
            cur.position = Some(pos);
            assert!(
                !should_publish(&prev, prev_time, &cur, t_inst),
                "poll at {s}s must NOT trigger should_publish (must not look like a seek)"
            );
        }

        // Poll at 5s: periodic progress MUST publish
        let t5_inst = t0_instant + Duration::from_secs(5);
        let t5_sys = t0_system + Duration::from_secs(5);
        let pos5 = extrapolate_position(
            snapshot_pos,
            last_updated_ticks,
            t5_sys,
            track_base.duration,
            track_base.playing,
        );
        cur.position = Some(pos5);
        assert!(
            should_publish(&prev, prev_time, &cur, t5_inst),
            "poll at 5s MUST trigger periodic progress"
        );

        // Update prev to 5s
        prev = Some(cur.clone());
        prev_time = t5_inst;

        // Poll at 6s without seek: must NOT publish
        let t6_inst = t0_instant + Duration::from_secs(6);
        let t6_sys = t0_system + Duration::from_secs(6);
        let pos6 = extrapolate_position(
            snapshot_pos,
            last_updated_ticks,
            t6_sys,
            track_base.duration,
            track_base.playing,
        );
        cur.position = Some(pos6);
        assert!(!should_publish(&prev, prev_time, &cur, t6_inst));

        // Seek at 6s to 120s: MUST publish immediately
        cur.position = Some(Duration::from_secs(120));
        assert!(should_publish(&prev, prev_time, &cur, t6_inst));
    }

    #[test]
    fn metadata_decision_table_tests() {
        let dummy_placeholder = placeholder_with_computer_name("TestComputer");
        let real_track = NowPlaying {
            title: "Song A".into(),
            artist: "Artist A".into(),
            album: "Album A".into(),
            artwork: Some(Artwork {
                mime: "image/jpeg".into(),
                bytes: vec![1, 2, 3],
            }),
            duration: Some(Duration::from_secs(200)),
            position: Some(Duration::from_secs(10)),
            playing: true,
        };

        // Case 1: Initial start with real track and toggle=true
        let d1 = decide_metadata_send_with_placeholder(
            None,
            Some(&real_track),
            true,
            &dummy_placeholder,
        );
        assert_eq!(d1.track, real_track);
        assert!(d1.send_track_info);
        assert!(d1.send_artwork);
        assert!(!d1.is_placeholder);
        assert!(d1.has_action());

        // Case 2: Initial start with no real track and toggle=true
        let d2 = decide_metadata_send_with_placeholder(None, None, true, &dummy_placeholder);
        assert_eq!(d2.track, dummy_placeholder);
        assert!(d2.send_track_info);
        assert!(d2.send_artwork);
        assert!(d2.is_placeholder);
        assert!(d2.has_action());

        // Case 3: Initial start with toggle=false
        let d3 = decide_metadata_send_with_placeholder(
            None,
            Some(&real_track),
            false,
            &dummy_placeholder,
        );
        assert_eq!(d3.track, dummy_placeholder);
        assert!(d3.send_track_info);
        assert!(d3.send_artwork);
        assert!(d3.is_placeholder);

        // Case 4: Placeholder active, subsequent poll still empty -> no-op
        let d4 = decide_metadata_send_with_placeholder(
            Some(&dummy_placeholder),
            None,
            true,
            &dummy_placeholder,
        );
        assert!(!d4.has_action());
        assert!(!d4.send_track_info);
        assert!(!d4.send_artwork);
        assert!(d4.is_placeholder);

        // Case 5: Placeholder active -> real track appears
        let d5 = decide_metadata_send_with_placeholder(
            Some(&dummy_placeholder),
            Some(&real_track),
            true,
            &dummy_placeholder,
        );
        assert_eq!(d5.track, real_track);
        assert!(d5.send_track_info);
        assert!(d5.send_artwork);
        assert!(!d5.is_placeholder);

        // Case 6: Real track active -> player stops (watcher returns empty) -> send placeholder again
        let empty_track = NowPlaying::default();
        let d6 = decide_metadata_send_with_placeholder(
            Some(&real_track),
            Some(&empty_track),
            true,
            &dummy_placeholder,
        );
        assert_eq!(d6.track, dummy_placeholder);
        assert!(d6.send_track_info);
        assert!(d6.send_artwork);
        assert!(d6.is_placeholder);

        // Case 7: Real track active -> toggle turned off mid-stream -> send placeholder
        let d7 = decide_metadata_send_with_placeholder(
            Some(&real_track),
            Some(&real_track),
            false,
            &dummy_placeholder,
        );
        assert_eq!(d7.track, dummy_placeholder);
        assert!(d7.send_track_info);
        assert!(d7.send_artwork);
        assert!(d7.is_placeholder);

        // Case 8: Real track active -> progress updates linearly
        let mut real_track_prog = real_track.clone();
        real_track_prog.position = Some(Duration::from_secs(15));
        let d8 = decide_metadata_send_with_placeholder(
            Some(&real_track),
            Some(&real_track_prog),
            true,
            &dummy_placeholder,
        );
        assert_eq!(d8.track, real_track_prog);
        assert!(!d8.has_action(), "a pure position change sends nothing");
        assert!(
            !d8.send_track_info,
            "track info should not be re-sent for pure progress change"
        );
        assert!(
            !d8.send_artwork,
            "artwork should not be re-sent for pure progress change"
        );
        assert!(!d8.is_placeholder);

        // Case 9: Real track without duration (e.g. Telegram Desktop) -> sends text and artwork
        let real_track_no_duration = NowPlaying {
            title: "Live Stream".into(),
            artist: "Host".into(),
            album: "".into(),
            artwork: Some(Artwork {
                mime: "image/jpeg".into(),
                bytes: vec![4, 5, 6],
            }),
            duration: None,
            position: Some(Duration::from_secs(50)),
            playing: true,
        };
        let d9 = decide_metadata_send_with_placeholder(
            None,
            Some(&real_track_no_duration),
            true,
            &dummy_placeholder,
        );
        assert!(d9.send_track_info);
        assert!(d9.send_artwork);
        assert!(!d9.is_placeholder);
    }
}

#[cfg(test)]
mod placeholder_return_tests {
    use super::*;

    #[test]
    fn leaving_a_track_resends_the_placeholder_artwork() {
        let ph = placeholder_with_computer_name("PC");
        let track = NowPlaying {
            title: "Song".into(),
            artist: "Artist".into(),
            artwork: Some(Artwork {
                mime: "image/jpeg".into(),
                bytes: vec![0xff, 0xd8, 0xff, 1, 2, 3],
            }),
            playing: true,
            ..Default::default()
        };
        let start = decide_metadata_send_with_placeholder(None, None, true, &ph);
        assert!(start.is_placeholder && start.send_artwork);
        let play =
            decide_metadata_send_with_placeholder(Some(&start.track), Some(&track), true, &ph);
        assert!(!play.is_placeholder && play.send_artwork);
        let gone = NowPlaying::default();
        let back = decide_metadata_send_with_placeholder(Some(&play.track), Some(&gone), true, &ph);
        assert!(back.is_placeholder);
        assert!(back.send_track_info, "title goes back to the placeholder");
        assert!(back.send_artwork, "placeholder artwork must be resent");
    }

    #[test]
    fn track_without_artwork_shows_the_placeholder_cover() {
        let ph = placeholder_with_computer_name("PC");
        let with_art = NowPlaying {
            title: "Song".into(),
            artist: "Artist".into(),
            artwork: Some(Artwork {
                mime: "image/jpeg".into(),
                bytes: vec![0xff, 0xd8, 0xff, 1, 2, 3],
            }),
            playing: true,
            ..Default::default()
        };
        let no_art = NowPlaying {
            title: "Trinity Departure".into(),
            artist: "Soleily".into(),
            playing: true,
            ..Default::default()
        };
        let first = decide_metadata_send_with_placeholder(None, Some(&with_art), true, &ph);
        assert!(!first.placeholder_artwork);

        // The previous cover must not stay: the placeholder cover replaces it.
        let bare =
            decide_metadata_send_with_placeholder(Some(&first.track), Some(&no_art), true, &ph);
        assert!(!bare.is_placeholder);
        assert!(bare.placeholder_artwork);
        assert!(bare.send_track_info && bare.send_artwork);
        assert_eq!(bare.track.title, "Trinity Departure");
        assert_eq!(bare.track.artwork, ph.artwork);

        // Another poll of the same track: nothing to resend.
        let again =
            decide_metadata_send_with_placeholder(Some(&bare.track), Some(&no_art), true, &ph);
        assert!(!again.has_action());

        // Its artwork shows up later (e.g. a remote cover finished downloading): send it.
        let late = NowPlaying {
            artwork: with_art.artwork.clone(),
            ..no_art.clone()
        };
        let loaded =
            decide_metadata_send_with_placeholder(Some(&again.track), Some(&late), true, &ph);
        assert!(loaded.send_artwork && !loaded.send_track_info);
        assert!(!loaded.placeholder_artwork);
    }
}

#[cfg(test)]
mod fit_artwork_tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn large_cover_is_downscaled_to_jpeg_keeping_aspect() {
        let src = png(1600, 1200);
        let art = fit_artwork(&src).expect("fits");
        assert_eq!(art.mime, "image/jpeg");
        assert!(art.bytes.len() <= MAX_ARTWORK_BYTES);
        let img = image::load_from_memory(&art.bytes).unwrap();
        assert_eq!((img.width(), img.height()), (1000, 750));
    }

    #[test]
    fn small_cover_passes_through_unchanged() {
        let src = png(300, 300);
        let art = fit_artwork(&src).expect("fits");
        assert_eq!(art.mime, "image/png");
        assert_eq!(art.bytes, src);
        assert!(fit_artwork(b"not an image").is_none());
    }

    #[test]
    fn cache_reuses_the_fitted_cover() {
        let mut cache = ArtworkCache::default();
        let src = Artwork {
            mime: "image/png".into(),
            bytes: png(1200, 1200),
        };
        let first = cache.fit(Some(src.clone())).unwrap();
        let key = cache.key;
        let second = cache.fit(Some(src)).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.key, key);
        assert!(cache.fit(None).is_none());
    }
}

#[cfg(test)]
mod placeholder_artwork_tests {
    use super::*;

    #[test]
    fn fresh_placeholder_artwork_differs_but_decodes_the_same() {
        let a = fresh_placeholder_artwork();
        let b = fresh_placeholder_artwork();
        assert_ne!(a, b);
        assert!(a.starts_with(&[0xff, 0xd8, 0xff, 0xfe]));
        assert_eq!(sniff_image_type(&a), Some("image/jpeg"));
        let img = image::load_from_memory(&a).unwrap();
        assert_eq!((img.width(), img.height()), (1000, 1000));
        assert_eq!(with_jpeg_comment(b"PNG?", b"x"), b"PNG?");
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;

    #[test]
    fn toggle_follows_reported_state() {
        assert_eq!(
            resolve_toggle(MediaCommand::PlayPause, true),
            MediaCommand::Pause
        );
        assert_eq!(
            resolve_toggle(MediaCommand::PlayPause, false),
            MediaCommand::Play
        );
        assert_eq!(
            resolve_toggle(MediaCommand::Pause, false),
            MediaCommand::Pause
        );
        assert_eq!(resolve_toggle(MediaCommand::Next, true), MediaCommand::Next);
    }
}
