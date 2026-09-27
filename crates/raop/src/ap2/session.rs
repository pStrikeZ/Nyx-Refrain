//! AirPlay 2 realtime-audio sender session (type 0x60, NTP timing, transient pairing).
//!
//! The request sequence mirrors an AirPlay 2 sender session verified against tvOS:
//! GET /info → POST /pair-pin-start → POST /pair-setup (M1, M3) → [HAP encryption on]
//! → SETUP (session, bplist) → events channel → SETUP (stream, bplist)
//! → SET_PARAMETER volume → SET_PARAMETER progress → RECORD → POST /feedback → FLUSH
//! → audio (+ sync / timing) … → TEARDOWN.
//! Source for field names/values: pyatv/protocols/raop/protocols/airplayv2.py (MIT).

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use plist::{Dictionary, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use super::audio::{AUDIO_TRAILER_LEN, AudioCipher};
use super::hap::{self, HapSession};
use super::metadata::{
    Ap2Features, TrackInfo, encode_dmap, format_progress_body, sniff_image_type,
};
use super::remote::{self, Command as RemoteCommand};
use super::srp;
use super::tlv8::{self, tag};
use crate::clock::{Clock, frames_to_duration};
use crate::control::{
    RETRANSMIT_REQ_SIZE, SYNC_PACKET_SIZE, SyncFlag, build_sync_packet, parse_retransmit_request,
};
use crate::rtp::write_rtp_header;
use crate::rtsp::{RtspResponse, parse_rtsp_response};
use crate::timing::{TIMING_PACKET_SIZE, build_timing_response, parse_timing_request};

/// Frames per packet (`spf`).
pub const FRAMES_PER_PACKET: usize = 352;
/// Interleaved stereo samples per packet.
pub const SAMPLES_PER_PACKET: usize = FRAMES_PER_PACKET * 2;
const PCM_BYTES: usize = SAMPLES_PER_PACKET * 2;
const SAMPLE_RATE: u64 = 44_100;

// Source: pyatv airplayv2.py HEADERS / support/rtsp.py USER_AGENT.
const RTSP_USER_AGENT: &str = "AirPlay/550.10";
// Source: pyatv protocols/airplay/auth/hap_transient.py _AIRPLAY_HEADERS.
const PAIR_USER_AGENT: &str = "AirPlay/320.20";

#[derive(Debug, thiserror::Error)]
pub enum Ap2Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("RTSP parse: {0}")]
    Rtsp(String),
    #[error("{method} {uri} failed: {status} {reason}")]
    Status {
        method: String,
        uri: String,
        status: u16,
        reason: String,
    },
    #[error("pairing: {0}")]
    Pairing(String),
    /// The receiver does not speak AirPlay 2 transient pairing (error status on a pairing
    /// endpoint, or an M2 that is not a valid pair-setup response).
    #[error("receiver does not support AirPlay 2 pairing: {0}")]
    NotSupported(String),
    #[error("plist: {0}")]
    Plist(String),
    #[error("connection closed by receiver")]
    Closed,
}

#[derive(Debug, Clone)]
pub struct Ap2Config {
    pub target: SocketAddr,
    pub local_ip: Option<Ipv4Addr>,
    /// Network interface to use (name or friendly name); auto-selected when both this and
    /// `local_ip` are unset.
    pub interface_name: Option<String>,
    /// Latency in frames written into sync packets (`dac = rtp_now - latency`). Negative
    /// values move the play point earlier. Measured on tvOS receivers:
    /// end-to-end ≈ 237 ms + latency; ≥ -165 ms (-7276 frames) plays cleanly, -180 ms crackles.
    pub latency_frames: i32,
    /// Values for the stream SETUP (`latencyMin` / `latencyMax`, frames).
    pub latency_min: u32,
    pub latency_max: u32,
    /// `volume: <dB>` (-30..0, -144 = mute) sent before audio.
    pub volume_db: f32,
    /// Send packets this far ahead of their nominal time (burst at start).
    pub prefill_ms: u32,
    /// Bytes 2..3 of sync packets (0x0007 iTunes / 0x0004 AirPlay).
    pub sync_flag: SyncFlag,
    /// Night/safety mode: force `volume: -144` and clamp samples above -60 dBFS (counted as
    /// quiet violations).
    pub quiet_mode: bool,
    pub transcript: Option<PathBuf>,
    /// Send a placeholder `progress: rtp0/rtp0/rtp0+1h` before RECORD (opt-in escape hatch).
    /// Default `false`. When enabled, receivers show a one-hour progress bar until real progress
    /// (with a known duration) arrives.
    pub initial_progress: bool,
}

/// For live sources, packets buffered between capture pipeline and sender beyond this are
/// dropped (6 packets ≈ 48 ms; the pipeline steers the fill to 3, WASAPI delivers ~10 ms
/// chunks ≈ 1.25 packets, so normal peaks stay below the cap).
pub const LIVE_MAX_BUFFERED_PACKETS: usize = 6;

/// Lowest sync latency accepted (verified clean in receiver testing).
pub const MIN_SYNC_LATENCY_MS: i32 = -165;

impl Default for Ap2Config {
    fn default() -> Self {
        Self {
            target: SocketAddr::from(([127, 0, 0, 1], 7000)),
            local_ip: None,
            interface_name: None,
            latency_frames: 11_025,
            latency_min: 11_025,
            latency_max: 88_200,
            volume_db: -144.0,
            prefill_ms: 0,
            sync_flag: SyncFlag::ITunes,
            quiet_mode: true,
            transcript: None,
            initial_progress: false,
        }
    }
}

/// Counters readable while streaming.
#[derive(Debug, Default)]
pub struct Ap2Stats {
    pub packets_sent: AtomicU64,
    pub timing_requests: AtomicU64,
    pub control_packets_received: AtomicU64,
    pub event_requests: AtomicU64,
    pub retransmit_requests: AtomicU64,
    pub retransmit_served: AtomicU64,
    pub retransmit_missing: AtomicU64,
}

/// RTSP/HTTP connection that switches to HAP encryption after pairing.
struct Conn {
    stream: TcpStream,
    hap: Option<HapSession>,
    plain: Vec<u8>,
    cseq: u32,
    dacp_id: String,
    active_remote: u32,
    transcript: Option<std::fs::File>,
}

/// How long a metadata `SET_PARAMETER` may wait for its response before that kind of
/// metadata is disabled for the session.
const METADATA_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

impl Conn {
    fn log(&mut self, dir: &str, text: &[u8]) {
        let printable = String::from_utf8_lossy(text);
        debug!("AP2 {dir}:\n{printable}");
        if let Some(f) = self.transcript.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{dir}:\n{printable}\n");
        }
    }

    async fn request(
        &mut self,
        http: bool,
        method: &str,
        uri: &str,
        extra: &[(&str, String)],
        body: Option<(&str, Vec<u8>)>,
    ) -> Result<RtspResponse, Ap2Error> {
        self.send(http, method, uri, extra, body).await?;
        self.read_response(http, method, uri).await
    }

    /// Like [`Conn::request`], but gives up waiting for the response after `limit`.
    ///
    /// Only the wait is bounded: the request itself is always written completely, because
    /// abandoning a half-written HAP frame (the nonce counter has already advanced) would
    /// break the encrypted control channel and with it the whole session. A response that
    /// arrives after the limit is discarded later by its stale CSeq.
    async fn request_bounded(
        &mut self,
        method: &str,
        uri: &str,
        extra: &[(&str, String)],
        body: Option<(&str, Vec<u8>)>,
        limit: Duration,
    ) -> Result<Result<RtspResponse, Ap2Error>, tokio::time::error::Elapsed> {
        if let Err(e) = self.send(false, method, uri, extra, body).await {
            return Ok(Err(e));
        }
        tokio::time::timeout(limit, self.read_response(false, method, uri)).await
    }

    async fn send(
        &mut self,
        http: bool,
        method: &str,
        uri: &str,
        extra: &[(&str, String)],
        body: Option<(&str, Vec<u8>)>,
    ) -> Result<(), Ap2Error> {
        let proto = if http { "HTTP/1.1" } else { "RTSP/1.0" };
        let mut head = format!("{method} {uri} {proto}\r\n");
        if http {
            head.push_str(&format!(
                "User-Agent: {PAIR_USER_AGENT}\r\nConnection: keep-alive\r\nX-Apple-HKP: 4\r\n"
            ));
        } else {
            self.cseq += 1;
            head.push_str(&format!(
                "User-Agent: {RTSP_USER_AGENT}\r\nCSeq: {}\r\nDACP-ID: {}\r\nActive-Remote: {}\r\nClient-Instance: {}\r\n",
                self.cseq, self.dacp_id, self.active_remote, self.dacp_id
            ));
        }
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        let body_bytes = match body {
            Some((ct, b)) => {
                head.push_str(&format!(
                    "Content-Type: {ct}\r\nContent-Length: {}\r\n",
                    b.len()
                ));
                b
            }
            None => Vec::new(),
        };
        head.push_str("\r\n");
        let mut msg = head.into_bytes();
        msg.extend_from_slice(&body_bytes);
        self.log(">>>", &msg);
        let wire = match self.hap.as_mut() {
            Some(h) => h.encrypt(&msg),
            None => msg,
        };
        self.stream.write_all(&wire).await?;
        Ok(())
    }

    /// Reads the response to the request sent last. RTSP responses carrying an older CSeq
    /// (late answers to requests whose wait was abandoned) are skipped.
    async fn read_response(
        &mut self,
        http: bool,
        method: &str,
        uri: &str,
    ) -> Result<RtspResponse, Ap2Error> {
        let mut buf = [0u8; 8192];
        loop {
            if let Some((resp, used)) =
                parse_rtsp_response(&self.plain).map_err(|e| Ap2Error::Rtsp(e.to_string()))?
            {
                let raw = self.plain[..used].to_vec();
                self.plain.drain(..used);
                self.log("<<<", &raw);
                if !http
                    && let Some(cseq) = resp
                        .header("CSeq")
                        .and_then(|v| v.trim().parse::<u32>().ok())
                    && cseq != self.cseq
                {
                    debug!(
                        "AP2 dropping stale response CSeq {cseq} (waiting for {})",
                        self.cseq
                    );
                    continue;
                }
                if !resp.is_success() {
                    return Err(Ap2Error::Status {
                        method: method.into(),
                        uri: uri.into(),
                        status: resp.status_code,
                        reason: resp.reason_phrase.clone(),
                    });
                }
                return Ok(resp);
            }
            let n = self.stream.read(&mut buf).await?;
            if n == 0 {
                return Err(Ap2Error::Closed);
            }
            match self.hap.as_mut() {
                Some(h) => {
                    let p = h
                        .decrypt(&buf[..n])
                        .map_err(|e| Ap2Error::Rtsp(e.to_string()))?;
                    self.plain.extend_from_slice(&p);
                }
                None => self.plain.extend_from_slice(&buf[..n]),
            }
        }
    }
}

fn bplist(dict: Dictionary) -> Result<Vec<u8>, Ap2Error> {
    let mut out = Vec::new();
    Value::Dictionary(dict)
        .to_writer_binary(&mut out)
        .map_err(|e| Ap2Error::Plist(e.to_string()))?;
    Ok(out)
}

fn parse_plist(body: &[u8]) -> Result<Dictionary, Ap2Error> {
    match plist::from_bytes::<Value>(body).map_err(|e| Ap2Error::Plist(e.to_string()))? {
        Value::Dictionary(d) => Ok(d),
        other => Err(Ap2Error::Plist(format!(
            "expected dictionary, got {other:?}"
        ))),
    }
}

fn int(v: u64) -> Value {
    Value::Integer(v.into())
}

fn random_uuid_upper() -> String {
    let b: [u8; 16] = rand::random();
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

fn not_supported_if_status(e: Ap2Error) -> Ap2Error {
    match e {
        Ap2Error::Status {
            method,
            uri,
            status,
            reason,
        } => Ap2Error::NotSupported(format!("{method} {uri} -> {status} {reason}")),
        other => other,
    }
}

/// Picks the local IPv4 address / interface index to use for `target`.
fn resolve_local(
    target: SocketAddr,
    local_ip: Option<Ipv4Addr>,
    iface: Option<&str>,
) -> (Option<Ipv4Addr>, Option<u32>) {
    if local_ip.is_some() {
        return (local_ip, None);
    }
    let std::net::IpAddr::V4(t) = target.ip() else {
        return (None, None);
    };
    if t.is_loopback() {
        return (Some(Ipv4Addr::LOCALHOST), None);
    }
    if let Some(name) = iface {
        if let Ok(ifaces) = netutil::enumerate_interfaces()
            && let Some(f) = ifaces
                .iter()
                .find(|i| i.name == name || i.friendly_name.as_deref() == Some(name))
        {
            return (f.primary_ipv4(), Some(f.index));
        }
        warn!("interface '{name}' not found; falling back to automatic selection");
    }
    if let Ok(classified) =
        netutil::enumerate_and_classify_interfaces(&netutil::ClassificationConfig::default())
        && let Some(sel) = netutil::select_interface_for_target(&classified, t)
    {
        // Only pin the connection to an interface that is on the receiver's subnet. A
        // receiver reached through a VPN / TUN route is on no local subnet; binding to the
        // fallback Wi-Fi / Ethernet address while the route goes through the tunnel makes
        // Windows fail the connect with WSAEADDRNOTAVAIL (10049). Let the OS route instead.
        if !sel.iface.contains_ipv4(t) {
            debug!(
                "AP2: no local interface on {t}'s subnet; using OS routing (not binding to '{}')",
                sel.iface.display_name()
            );
            return (None, None);
        }
        debug!(
            "AP2 auto-selected interface '{}' ({})",
            sel.iface.display_name(),
            sel.reason
        );
        return (sel.iface.primary_ipv4(), Some(sel.iface.index));
    }
    (None, None)
}

/// Log target for the events channel, so `RUST_LOG=raop::ap2::events=debug` shows the
/// receiver's requests without the RTSP dumps of `raop::ap2::session=debug`.
const EVENTS_LOG: &str = "raop::ap2::events";

/// Handles the receiver-initiated events connection: answer every request with 200 and
/// forward playback commands (see [`super::remote`]).
/// Source: pyatv/protocols/airplay/channels.py EventChannel.handle_received.
async fn run_events(
    mut stream: TcpStream,
    mut hap: HapSession,
    stats: Arc<Ap2Stats>,
    stop: Arc<AtomicBool>,
    quiet_guard: Arc<pipeline::quiet::QuietGuard>,
    commands: tokio::sync::mpsc::Sender<RemoteCommand>,
) {
    let mut plain: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        let n = match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await
        {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(n)) => n,
            Err(_) => continue,
        };
        match hap.decrypt(&buf[..n]) {
            Ok(p) => plain.extend_from_slice(&p),
            Err(e) => {
                warn!("events channel decrypt failed: {e}");
                break;
            }
        }
        while let Some(end) = plain.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&plain[..end]).to_string();
            let clen = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Content-Length:")
                        .or_else(|| l.strip_prefix("content-length:"))
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if plain.len() < end + 4 + clen {
                break;
            }
            let body: Vec<u8> = plain.drain(..end + 4 + clen).skip(end + 4).collect();
            stats.event_requests.fetch_add(1, Ordering::Relaxed);
            debug!(target: EVENTS_LOG, "AP2 event request:\n{head}");
            if !body.is_empty() {
                match plist::from_bytes::<plist::Value>(&body) {
                    Ok(v) => {
                        debug!(target: EVENTS_LOG, "AP2 event body: {v:?}");
                        match remote::parse_event(&v) {
                            remote::Event::Command(command) => {
                                match command {
                                    remote::Command::Volume(vol) => {
                                        if quiet_guard.is_enabled() {
                                            warn!(
                                                target: EVENTS_LOG,
                                                volume = vol,
                                                "receiver volume update ignored: quiet mode is active"
                                            );
                                            continue;
                                        }
                                        info!(target: EVENTS_LOG, volume = vol, "receiver volume update");
                                    }
                                    _ => {
                                        info!(target: EVENTS_LOG, ?command, "receiver playback command");
                                    }
                                }
                                if commands.try_send(command).is_err() {
                                    debug!(target: EVENTS_LOG, ?command, "receiver command dropped (not consumed)");
                                }
                            }
                            remote::Event::Unsupported { value, number } => info!(
                                target: EVENTS_LOG,
                                ?value,
                                ?number,
                                "unsupported receiver playback command"
                            ),
                            remote::Event::Other => {}
                        }
                    }
                    Err(_) => debug!(
                        target: EVENTS_LOG,
                        "AP2 event body ({} bytes): {:02x?}",
                        body.len(),
                        body
                    ),
                }
            }
            let proto = head
                .split_whitespace()
                .nth(2)
                .unwrap_or("RTSP/1.0")
                .to_string();
            let mut resp = format!("{proto} 200 OK\r\nContent-Length: 0\r\nAudio-Latency: 0\r\n");
            for l in head.lines() {
                if l.to_ascii_lowercase().starts_with("cseq:")
                    || l.to_ascii_lowercase().starts_with("server:")
                {
                    resp.push_str(l);
                    resp.push_str("\r\n");
                }
            }
            resp.push_str("\r\n");
            if stream
                .write_all(&hap.encrypt(resp.as_bytes()))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// A running AirPlay 2 realtime audio session.
pub struct Ap2Session {
    conn: Conn,
    clock: Clock,
    start_at: Instant,
    uri: String,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    stats: Arc<Ap2Stats>,
    quiet_guard: Arc<pipeline::quiet::QuietGuard>,
    feedback: Option<tokio::task::JoinHandle<()>>,
    /// Stop flags of helper threads owned by the session (e.g. the pipeline feeder).
    aux_stops: Vec<Arc<AtomicBool>>,
    text_disabled: bool,
    artwork_disabled: bool,
    progress_disabled: bool,
    last_artwork_hash: Option<u64>,
    remote_commands: Option<tokio::sync::mpsc::Receiver<RemoteCommand>>,
}

impl Ap2Session {
    /// Pairs, sets up and starts streaming PCM produced by `source` (called once per
    /// 352-frame packet on the sender thread; must not block).
    pub async fn start<F>(cfg: Ap2Config, mut source: F) -> Result<Self, Ap2Error>
    where
        F: FnMut(&mut [i16; SAMPLES_PER_PACKET]) + Send + 'static,
    {
        let (bind_ip, iface_index) =
            resolve_local(cfg.target, cfg.local_ip, cfg.interface_name.as_deref());
        let stream = match bind_ip {
            Some(ip) => {
                let sock = tokio::net::TcpSocket::new_v4()?;
                sock.bind(SocketAddr::new(ip.into(), 0))?;
                sock.connect(cfg.target).await?
            }
            None => TcpStream::connect(cfg.target).await?,
        };
        stream.set_nodelay(true)?;
        let local_ip = match stream.local_addr()?.ip() {
            std::net::IpAddr::V4(v4) => v4,
            _ => Ipv4Addr::UNSPECIFIED,
        };
        let transcript = match &cfg.transcript {
            Some(p) => Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?,
            ),
            None => None,
        };
        let mut conn = Conn {
            stream,
            hap: None,
            plain: Vec::new(),
            cseq: 0,
            dacp_id: format!("{:016X}", rand::random::<u64>()),
            active_remote: rand::random::<u32>(),
            transcript,
        };
        let session_id: u32 = rand::random();
        let uri = format!("rtsp://{local_ip}/{session_id}");
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Ap2Stats::default());
        let history = Arc::new(super::history::PacketHistory::new());
        let quiet_guard = Arc::new(pipeline::quiet::QuietGuard::new(cfg.quiet_mode));

        // Plain: GET /info (pyatv does this first; response is informational).
        let _ = conn.request(false, "GET", "/info", &[], None).await;

        // Transient pair-setup (M1..M4). Source: pyatv hap_transient.py verify_credentials.
        conn.request(true, "POST", "/pair-pin-start", &[], None)
            .await
            .map_err(not_supported_if_status)?;
        let m1 = tlv8::write(&[
            (tag::METHOD, &[0]),
            (tag::SEQ_NO, &[1]),
            (tag::FLAGS, &[tlv8::FLAG_TRANSIENT]),
        ]);
        let m2 = conn
            .request(
                true,
                "POST",
                "/pair-setup",
                &[],
                Some(("application/octet-stream", m1)),
            )
            .await
            .map_err(not_supported_if_status)?;
        let m2 = tlv8::read(&m2.body)
            .map_err(|e| Ap2Error::NotSupported(format!("M2 is not TLV8: {e}")))?;
        if let Some(err) = m2.get(&tag::ERROR) {
            return Err(Ap2Error::Pairing(format!("M2 error TLV {err:?}")));
        }
        let salt = m2
            .get(&tag::SALT)
            .ok_or_else(|| Ap2Error::NotSupported("M2 without salt".into()))?;
        let b_pub = m2
            .get(&tag::PUBLIC_KEY)
            .ok_or_else(|| Ap2Error::NotSupported("M2 without public key".into()))?;
        let a_private: [u8; 32] = rand::random();
        let proof = srp::client_proof(
            &a_private,
            salt,
            b_pub,
            srp::SRP_USERNAME,
            srp::TRANSIENT_PIN,
        )
        .map_err(|e| Ap2Error::Pairing(e.to_string()))?;
        let m3 = tlv8::write(&[
            (tag::SEQ_NO, &[3]),
            (tag::PUBLIC_KEY, &proof.a_pub),
            (tag::PROOF, &proof.m1),
        ]);
        let m4 = conn
            .request(
                true,
                "POST",
                "/pair-setup",
                &[],
                Some(("application/octet-stream", m3)),
            )
            .await?;
        let m4 = tlv8::read(&m4.body).map_err(|e| Ap2Error::Pairing(e.into()))?;
        if let Some(err) = m4.get(&tag::ERROR) {
            return Err(Ap2Error::Pairing(format!(
                "M4 error TLV {err:?} (wrong SRP proof?)"
            )));
        }
        match m4.get(&tag::PROOF) {
            Some(p) if p.as_slice() == proof.m2.as_slice() => debug!("M4 proof verified"),
            Some(_) => warn!("M4 proof mismatch (continuing like pyatv, which does not verify it)"),
            None => debug!("M4 without proof"),
        }
        let k = proof.session_key;
        conn.hap = Some(HapSession::new(
            &hap::hkdf_expand(hap::CONTROL_SALT, hap::CONTROL_WRITE_INFO, &k),
            &hap::hkdf_expand(hap::CONTROL_SALT, hap::CONTROL_READ_INFO, &k),
        ));
        info!("AP2 transient pairing done; RTSP channel encrypted");

        // UDP sockets on the same local address as the TCP connection.
        let bind_udp = || -> std::io::Result<UdpSocket> {
            netutil::bind_udp_socket(local_ip, iface_index, cfg.interface_name.as_deref(), 0)
        };
        let timing_sock = bind_udp()?;
        let control_sock = bind_udp()?;
        let audio_sock = bind_udp()?;
        let clock = Clock::init();

        let mut threads: Vec<std::thread::JoinHandle<()>> = Vec::new();
        // Timing responder must run before SETUP (the receiver probes during SETUP).
        {
            let sock = timing_sock.try_clone()?;
            let stop = stop.clone();
            let stats = stats.clone();
            sock.set_read_timeout(Some(Duration::from_millis(100)))?;
            threads.push(
                std::thread::Builder::new()
                    .name("ap2-timing".into())
                    .spawn(move || {
                        let mut buf = [0u8; 128];
                        let mut out = [0u8; TIMING_PACKET_SIZE];
                        while !stop.load(Ordering::Relaxed) {
                            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                                let recv = clock.now_ntp().to_u64();
                                if let Ok(req) = parse_timing_request(&buf[..n]) {
                                    stats.timing_requests.fetch_add(1, Ordering::Relaxed);
                                    build_timing_response(
                                        &req,
                                        recv,
                                        clock.now_ntp().to_u64(),
                                        &mut out,
                                    );
                                    let _ = sock.send_to(&out, from);
                                }
                            }
                        }
                    })?,
            );
        }

        // SETUP #1 (session).
        let mut d = Dictionary::new();
        d.insert("deviceID".into(), Value::String("AA:BB:CC:DD:EE:FF".into()));
        d.insert("sessionUUID".into(), Value::String(random_uuid_upper()));
        d.insert(
            "timingPort".into(),
            int(timing_sock.local_addr()?.port() as u64),
        );
        d.insert("timingProtocol".into(), Value::String("NTP".into()));
        d.insert("isMultiSelectAirPlay".into(), Value::Boolean(true));
        d.insert("groupContainsGroupLeader".into(), Value::Boolean(false));
        d.insert(
            "macAddress".into(),
            Value::String("AA:BB:CC:DD:EE:FF".into()),
        );
        d.insert("model".into(), Value::String("iPhone14,3".into()));
        d.insert("name".into(), Value::String("Nyx Refrain".into()));
        d.insert("osBuildVersion".into(), Value::String("20F66".into()));
        d.insert("osName".into(), Value::String("iPhone OS".into()));
        d.insert("osVersion".into(), Value::String("16.5".into()));
        d.insert("senderSupportsRelay".into(), Value::Boolean(false));
        d.insert("sourceVersion".into(), Value::String("690.7.1".into()));
        d.insert("statsCollectionEnabled".into(), Value::Boolean(false));
        let r = conn
            .request(
                false,
                "SETUP",
                &uri,
                &[],
                Some(("application/x-apple-binary-plist", bplist(d)?)),
            )
            .await?;
        let setup1 = parse_plist(&r.body)?;
        info!("AP2 session SETUP response: {setup1:?}");
        let event_port = setup1
            .get("eventPort")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0) as u16;

        // Events channel: our write key = Events-Read info, read key = Events-Write info
        // (pyatv airplayv2.py setup_channel(..., EVENTS_READ_INFO, EVENTS_WRITE_INFO)).
        let (remote_tx, remote_rx) = tokio::sync::mpsc::channel(16);
        if event_port != 0 {
            let mut ev = None;
            for _ in 0..5 {
                match TcpStream::connect(SocketAddr::new(cfg.target.ip(), event_port)).await {
                    Ok(s) => {
                        ev = Some(s);
                        break;
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }
            match ev {
                Some(s) => {
                    let h = HapSession::new(
                        &hap::hkdf_expand(hap::EVENTS_SALT, hap::EVENTS_READ_INFO, &k),
                        &hap::hkdf_expand(hap::EVENTS_SALT, hap::EVENTS_WRITE_INFO, &k),
                    );
                    tokio::spawn(run_events(
                        s,
                        h,
                        stats.clone(),
                        stop.clone(),
                        quiet_guard.clone(),
                        remote_tx.clone(),
                    ));
                }
                None => warn!("could not connect events channel on port {event_port}"),
            }
        }

        // SETUP #2 (realtime audio stream).
        let shk: [u8; 32] = rand::random();
        let mut s = Dictionary::new();
        s.insert("audioFormat".into(), int(0x800)); // PCM/44100/16/2
        s.insert("audioMode".into(), Value::String("default".into()));
        s.insert(
            "controlPort".into(),
            int(control_sock.local_addr()?.port() as u64),
        );
        s.insert("ct".into(), int(1)); // raw PCM
        s.insert("isMedia".into(), Value::Boolean(true));
        s.insert("latencyMax".into(), int(cfg.latency_max as u64));
        s.insert("latencyMin".into(), int(cfg.latency_min as u64));
        s.insert("shk".into(), Value::Data(shk.to_vec()));
        s.insert("spf".into(), int(FRAMES_PER_PACKET as u64));
        s.insert("sr".into(), int(SAMPLE_RATE));
        s.insert("type".into(), int(0x60));
        s.insert("supportsDynamicStreamID".into(), Value::Boolean(false));
        s.insert("streamConnectionID".into(), int(session_id as u64));
        let mut root = Dictionary::new();
        root.insert("streams".into(), Value::Array(vec![Value::Dictionary(s)]));
        let r = conn
            .request(
                false,
                "SETUP",
                &uri,
                &[],
                Some(("application/x-apple-binary-plist", bplist(root)?)),
            )
            .await?;
        let setup2 = parse_plist(&r.body)?;
        info!("AP2 stream SETUP response: {setup2:?}");
        let st = setup2
            .get("streams")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| Ap2Error::Plist("stream SETUP response without streams".into()))?;
        let data_port = st
            .get("dataPort")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0) as u16;
        let remote_control = st
            .get("controlPort")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0) as u16;
        let remote_audio = SocketAddr::new(cfg.target.ip(), data_port);
        let remote_ctrl = SocketAddr::new(cfg.target.ip(), remote_control);

        let seq0: u16 = rand::random();
        let start_at = Instant::now() + Duration::from_millis(150);
        let rtp0 = clock.rtp_ts(start_at);
        let rtp_info = format!("seq={seq0};rtptime={rtp0}");

        conn.request(
            false,
            "SET_PARAMETER",
            &uri,
            &[],
            Some((
                "text/parameters",
                format!(
                    "volume: {:.1}",
                    if cfg.quiet_mode {
                        -144.0
                    } else {
                        cfg.volume_db
                    }
                )
                .into_bytes(),
            )),
        )
        .await?;
        if cfg.initial_progress {
            conn.request(
                false,
                "SET_PARAMETER",
                &uri,
                &[],
                Some((
                    "text/parameters",
                    format!(
                        "progress: {rtp0}/{rtp0}/{}",
                        rtp0.wrapping_add(3600 * 44_100)
                    )
                    .into_bytes(),
                )),
            )
            .await?;
        }
        conn.request(false, "RECORD", &uri, &[], None).await?;
        conn.request(false, "POST", "/feedback", &[], None).await?;
        conn.request(
            false,
            "FLUSH",
            &uri,
            &[
                ("Range", "npt=0-".into()),
                ("Session", "0".into()),
                ("RTP-Info", rtp_info),
            ],
            None,
        )
        .await?;

        // Control: sync packets once per second (first with extension bit), count inbound.
        {
            let sock = control_sock.try_clone()?;
            let stop = stop.clone();
            let stats = stats.clone();
            let latency = cfg.latency_frames as u32; // two's complement for negative values
            let sync_flag = cfg.sync_flag;
            let history = history.clone();
            sock.set_read_timeout(Some(Duration::from_millis(50)))?;
            threads.push(
                std::thread::Builder::new()
                    .name("ap2-control".into())
                    .spawn(move || {
                        let mut out = [0u8; SYNC_PACKET_SIZE];
                        let mut buf = [0u8; 2048];
                        let mut pkt = [0u8; super::history::MAX_PACKET];
                        let mut resp = [0u8; 4 + super::history::MAX_PACKET];
                        let mut first = true;
                        let mut next = start_at;
                        while !stop.load(Ordering::Relaxed) {
                            let now = Instant::now();
                            if now >= next {
                                let rtp_now = clock.rtp_ts(now.max(start_at));
                                build_sync_packet(
                                    first,
                                    sync_flag,
                                    rtp_now,
                                    latency,
                                    clock.ntp_at(now).to_u64(),
                                    &mut out,
                                );
                                let _ = sock.send_to(&out, remote_ctrl);
                                first = false;
                                next = now + Duration::from_secs(1);
                            }
                            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                                let c = stats
                                    .control_packets_received
                                    .fetch_add(1, Ordering::Relaxed);
                                // Retransmit request: 0x80 0xd5 (marker bit masked), lost seq, count.
                                // Response: 0x80 0xd6 + original seq + the packet as sent.
                                // Source: pyatv stream_client.py ControlClient._retransmit_lost_packets.
                                if n >= RETRANSMIT_REQ_SIZE && buf[1] & 0x7f == 0x55 {
                                    if let Ok(req) = parse_retransmit_request(&buf[..n]) {
                                        stats.retransmit_requests.fetch_add(1, Ordering::Relaxed);
                                        for i in 0..req.lost_packets {
                                            let seq = req.lost_seqno.wrapping_add(i);
                                            match history.get(seq, &mut pkt) {
                                                Some(len) => {
                                                    resp[0] = 0x80;
                                                    resp[1] = 0xd6;
                                                    resp[2..4].copy_from_slice(&seq.to_be_bytes());
                                                    resp[4..4 + len].copy_from_slice(&pkt[..len]);
                                                    let _ = sock.send_to(&resp[..4 + len], from);
                                                    stats
                                                        .retransmit_served
                                                        .fetch_add(1, Ordering::Relaxed);
                                                }
                                                None => {
                                                    stats
                                                        .retransmit_missing
                                                        .fetch_add(1, Ordering::Relaxed);
                                                }
                                            }
                                        }
                                    }
                                } else if c < 5 {
                                    debug!(
                                        "AP2 control rx #{c} ({n} bytes): {:02x?}",
                                        &buf[..n.min(16)]
                                    );
                                }
                            }
                        }
                    })?,
            );
        }

        // Audio sender (no allocation in the loop).
        {
            let stop = stop.clone();
            let stats = stats.clone();
            let prefill = Duration::from_millis(cfg.prefill_ms as u64);
            let history = history.clone();
            let guard = quiet_guard.clone();
            threads.push(std::thread::Builder::new().name("ap2-audio".into()).spawn(
                move || {
                    let mut cipher = AudioCipher::new(&shk);
                    let mut pcm = [0i16; SAMPLES_PER_PACKET];
                    let mut payload = [0u8; PCM_BYTES];
                    let mut header = [0u8; 12];
                    let mut packet = [0u8; 12 + PCM_BYTES + AUDIO_TRAILER_LEN];
                    let ssrc: u32 = rand::random();
                    let mut idx: u64 = 0;
                    while !stop.load(Ordering::Relaxed) {
                        let nominal = start_at
                            + frames_to_duration(
                                idx * FRAMES_PER_PACKET as u64,
                                SAMPLE_RATE as u32,
                            );
                        let send_at = nominal.checked_sub(prefill).unwrap_or(nominal);
                        let now = Instant::now();
                        if send_at > now {
                            pipeline::wait_until(send_at);
                        }
                        source(&mut pcm);
                        guard.process_s16(&mut pcm);
                        for (i, s) in pcm.iter().enumerate() {
                            payload[2 * i..2 * i + 2].copy_from_slice(&s.to_be_bytes());
                        }
                        let seq = seq0.wrapping_add(idx as u16);
                        let ts = rtp0.wrapping_add((idx * FRAMES_PER_PACKET as u64) as u32);
                        write_rtp_header(idx == 0, seq, ts, ssrc, &mut header);
                        let n = cipher.seal(&header, &payload, &mut packet);
                        history.insert(seq, &packet[..n]);
                        let _ = audio_sock.send_to(&packet[..n], remote_audio);
                        stats.packets_sent.fetch_add(1, Ordering::Relaxed);
                        idx += 1;
                    }
                },
            )?);
        }

        info!(
            "AP2 streaming to {remote_audio} (control {remote_ctrl}), rtp0={rtp0}, latency={} frames",
            cfg.latency_frames
        );
        Ok(Self {
            conn,
            clock,
            start_at,
            uri,
            stop,
            threads,
            stats,
            quiet_guard,
            feedback: None,
            aux_stops: Vec::new(),
            text_disabled: false,
            artwork_disabled: false,
            progress_disabled: false,
            last_artwork_hash: None,
            remote_commands: Some(remote_rx),
        })
    }

    /// Starts a session fed by a capture/test `AudioSource` through the shared audio pipeline
    /// (format conversion, resampling, drift compensation, 352-frame packetizing, underrun
    /// silence).
    pub async fn start_with_source(
        cfg: Ap2Config,
        source: Box<dyn capture::AudioSource + Send>,
        metrics: pipeline::SharedPipelineMetrics,
        ring_capacity: usize,
    ) -> Result<Self, Ap2Error> {
        let source_is_live = source.is_live();
        let (producer, mut consumer) =
            rtrb::RingBuffer::<[i16; SAMPLES_PER_PACKET]>::new(ring_capacity);
        let feeder_stop = Arc::new(AtomicBool::new(false));
        let feeder = pipeline::start_pipeline_feeder(
            source,
            producer,
            metrics.clone(),
            feeder_stop.clone(),
            Duration::from_millis(50),
        )?;
        let rt_metrics = metrics.clone();
        let live = source_is_live;
        let started = Self::start(cfg, move |pcm: &mut [i16; SAMPLES_PER_PACKET]| {
            if live {
                // Keep latency bounded: drop backlog beyond a few packets (see
                // AudioSource::is_live). No allocation, no locks.
                let mut dropped = 0u64;
                while consumer.slots() > LIVE_MAX_BUFFERED_PACKETS {
                    let _ = consumer.pop();
                    dropped += 1;
                }
                if dropped > 0 {
                    rt_metrics.record_packets_dropped(dropped);
                }
            }
            match consumer.pop() {
                Ok(p) => *pcm = p,
                Err(_) => {
                    pcm.fill(0);
                    rt_metrics.record_discontinuity();
                }
            }
            rt_metrics.record_packet_sent();
        })
        .await;
        match started {
            Ok(mut s) => {
                s.aux_stops.push(feeder_stop);
                s.threads.push(feeder);
                Ok(s)
            }
            Err(e) => {
                feeder_stop.store(true, Ordering::Relaxed);
                let _ = feeder.join();
                Err(e)
            }
        }
    }

    /// Monotonic instant at which packet 0 (RTP-Info rtptime) is nominally due; packet `i`
    /// is due at `start_instant() + i * 352 / 44100 s`.
    pub fn start_instant(&self) -> Instant {
        self.start_at
    }

    /// Samples clamped by the quiet guard (must stay 0 in quiet mode).
    pub fn quiet_violations(&self) -> u64 {
        self.quiet_guard.violations()
    }

    /// Whether quiet-mode clamping is active.
    pub fn is_quiet(&self) -> bool {
        self.quiet_guard.is_enabled()
    }

    pub fn stats(&self) -> Arc<Ap2Stats> {
        self.stats.clone()
    }

    /// Changes the receiver volume while streaming (`volume: <dB>`, -30..0, -144 = mute).
    /// Refused in quiet mode so the night-time guard cannot be bypassed.
    pub async fn set_volume(&mut self, volume_db: f32) -> Result<(), Ap2Error> {
        if self.quiet_guard.is_enabled() {
            return Err(Ap2Error::NotSupported(
                "volume changes are disabled in quiet mode".into(),
            ));
        }
        let uri = self.uri.clone();
        let body = format!("volume: {:.1}", volume_db.clamp(-144.0, 0.0)).into_bytes();
        self.conn
            .request(
                false,
                "SET_PARAMETER",
                &uri,
                &[],
                Some(("text/parameters", body)),
            )
            .await
            .map(|_| ())
    }

    /// Sends `POST /feedback` (keep-alive, pyatv every 2 s).
    pub async fn feedback(&mut self) -> Result<(), Ap2Error> {
        self.conn
            .request(false, "POST", "/feedback", &[], None)
            .await
            .map(|_| ())
    }

    /// Returns the 44.1 kHz RTP timestamp for "now" aligned to the session timeline.
    pub fn rtp_now(&self) -> u32 {
        self.clock.rtp_ts(Instant::now().max(self.start_at))
    }

    /// Helper giving the RTP timestamp for "now".
    pub fn current_rtp_timestamp(&self) -> u32 {
        self.rtp_now()
    }

    /// Applies device features bitmask to gate metadata kinds.
    /// If `features` is `None` (manual address), all metadata kinds remain enabled.
    pub fn apply_features(&mut self, features: Option<Ap2Features>) {
        if let Some(f) = features {
            if !f.supports_text() {
                self.text_disabled = true;
            }
            if !f.supports_artwork() {
                self.artwork_disabled = true;
            }
            if !f.supports_progress() {
                self.progress_disabled = true;
            }
        }
    }

    pub fn is_text_enabled(&self) -> bool {
        !self.text_disabled
    }

    pub fn is_artwork_enabled(&self) -> bool {
        !self.artwork_disabled
    }

    pub fn is_progress_enabled(&self) -> bool {
        !self.progress_disabled
    }

    pub fn disable_text(&mut self) {
        self.text_disabled = true;
    }

    pub fn disable_artwork(&mut self) {
        self.artwork_disabled = true;
    }

    pub fn disable_progress(&mut self) {
        self.progress_disabled = true;
    }

    /// Sends track text metadata (`application/x-dmap-tagged` holding `minm`/`asar`/`asal`)
    /// via RTSP `SET_PARAMETER`. Gated by device features and disabled on error.
    pub async fn send_track_info(&mut self, info: &TrackInfo) -> Result<(), Ap2Error> {
        if self.text_disabled {
            return Ok(());
        }
        let body = encode_dmap(info);
        let rtp_now = self.rtp_now();
        let rtp_info = format!("rtptime={rtp_now}");
        let uri = self.uri.clone();
        let res = self
            .conn
            .request_bounded(
                "SET_PARAMETER",
                &uri,
                &[("RTP-Info", rtp_info)],
                Some(("application/x-dmap-tagged", body)),
                METADATA_RESPONSE_TIMEOUT,
            )
            .await;

        match res {
            Ok(Ok(_)) => {
                info!(
                    "AP2 track info sent: {:?} / {:?} / {:?}",
                    info.title, info.artist, info.album
                );
                Ok(())
            }
            Ok(Err(e)) => {
                self.text_disabled = true;
                warn!("AP2 text metadata failed ({e}); disabling for session");
                Err(e)
            }
            Err(_) => {
                self.text_disabled = true;
                let e = Ap2Error::Rtsp("timed out waiting for SET_PARAMETER text response".into());
                warn!("AP2 text metadata timed out; disabling for session");
                Err(e)
            }
        }
    }

    /// Sends track artwork (`image/jpeg` or `image/png`, <= 1 MiB) via RTSP `SET_PARAMETER`.
    /// Dedupes identical artwork by hashing. Gated by device features and disabled on error.
    pub async fn send_artwork(&mut self, image_bytes: &[u8]) -> Result<(), Ap2Error> {
        if self.artwork_disabled {
            return Ok(());
        }
        let Some(mime) = sniff_image_type(image_bytes) else {
            info!(
                "AP2 artwork skipped: unsupported image format or exceeds 1 MiB ({} bytes)",
                image_bytes.len()
            );
            return Ok(());
        };
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        image_bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_artwork_hash == Some(hash) {
            info!("AP2 artwork #{:08x} unchanged; not resent", hash as u32);
            return Ok(());
        }

        let rtp_now = self.rtp_now();
        let rtp_info = format!("rtptime={rtp_now}");
        let uri = self.uri.clone();
        let res = self
            .conn
            .request_bounded(
                "SET_PARAMETER",
                &uri,
                &[("RTP-Info", rtp_info)],
                Some((mime, image_bytes.to_vec())),
                METADATA_RESPONSE_TIMEOUT,
            )
            .await;

        match res {
            Ok(Ok(_)) => {
                self.last_artwork_hash = Some(hash);
                info!(
                    "AP2 artwork sent ({mime}, {} bytes, #{:08x})",
                    image_bytes.len(),
                    hash as u32
                );
                Ok(())
            }
            Ok(Err(e)) => {
                self.artwork_disabled = true;
                warn!("AP2 artwork failed ({e}); disabling for session");
                Err(e)
            }
            Err(_) => {
                self.artwork_disabled = true;
                let e =
                    Ap2Error::Rtsp("timed out waiting for SET_PARAMETER artwork response".into());
                warn!("AP2 artwork timed out; disabling for session");
                Err(e)
            }
        }
    }

    /// Sends playback progress (`text/parameters`, `progress: <start>/<current>/<end>\r\n`)
    /// via RTSP `SET_PARAMETER`. Gated by device features and disabled on error.
    pub async fn send_progress(
        &mut self,
        position: Duration,
        duration: Duration,
    ) -> Result<(), Ap2Error> {
        if self.progress_disabled {
            return Ok(());
        }
        let rtp_now = self.rtp_now();
        let rtp_info = format!("rtptime={rtp_now}");
        let body = format_progress_body(rtp_now, position, duration).into_bytes();
        let body_log = body.clone();
        let uri = self.uri.clone();
        let res = self
            .conn
            .request_bounded(
                "SET_PARAMETER",
                &uri,
                &[("RTP-Info", rtp_info)],
                Some(("text/parameters", body)),
                METADATA_RESPONSE_TIMEOUT,
            )
            .await;

        match res {
            Ok(Ok(_)) => {
                info!(
                    "AP2 progress sent: {}",
                    String::from_utf8_lossy(&body_log).trim_end()
                );
                Ok(())
            }
            Ok(Err(e)) => {
                self.progress_disabled = true;
                warn!("AP2 progress failed ({e}); disabling for session");
                Err(e)
            }
            Err(_) => {
                self.progress_disabled = true;
                let e =
                    Ap2Error::Rtsp("timed out waiting for SET_PARAMETER progress response".into());
                warn!("AP2 progress timed out; disabling for session");
                Err(e)
            }
        }
    }

    /// Experiment hook for `nyxr stream --debug-stdin`: sends one RTSP request on the
    /// encrypted control channel and returns the status line and a printable body.
    pub async fn debug_request(
        &mut self,
        method: &str,
        extra: &[(&str, String)],
        body: Option<Dictionary>,
    ) -> Result<String, Ap2Error> {
        let body = match body {
            Some(d) => Some(("application/x-apple-binary-plist", bplist(d)?)),
            None => None,
        };
        let uri = self.uri.clone();
        let r = self
            .conn
            .request_bounded(method, &uri, extra, body, Duration::from_secs(3))
            .await
            .map_err(|_| Ap2Error::Rtsp(format!("timed out waiting for {method} response")))??;
        let body = if r.body.is_empty() {
            String::new()
        } else {
            match plist::from_bytes::<Value>(&r.body) {
                Ok(v) => format!("{v:?}"),
                Err(_) => String::from_utf8_lossy(&r.body).into_owned(),
            }
        };
        Ok(format!("{} {} {body}", r.status_code, r.reason_phrase))
    }

    /// Playback commands the receiver sends over the events channel; the first call
    /// takes the receiver. Commands arriving while nobody holds it are dropped.
    pub fn take_remote_commands(&mut self) -> Option<tokio::sync::mpsc::Receiver<RemoteCommand>> {
        self.remote_commands.take()
    }

    /// Stops audio and tears down the session.
    pub async fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for s in &self.aux_stops {
            s.store(true, Ordering::Relaxed);
        }
        if let Some(f) = self.feedback.take() {
            f.abort();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        let uri = self.uri.clone();
        if let Err(e) = self
            .conn
            .request(false, "TEARDOWN", &uri, &[("Session", "0".into())], None)
            .await
        {
            warn!("AP2 TEARDOWN failed: {e}");
        }
    }
}
