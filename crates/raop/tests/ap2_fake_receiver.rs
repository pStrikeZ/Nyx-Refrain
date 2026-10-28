//! End-to-end test of the AirPlay 2 sender against an in-process fake receiver that plays
//! the accessory side: transient pair-setup (server SRP), HAP-encrypted RTSP, bplist SETUPs,
//! an events channel request, audio decryption, a timing probe and a retransmit request.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use plist::{Dictionary, Value};
use raop::ap2::hap::{self, HapSession};
use raop::ap2::metadata::{self, TrackInfo};
use raop::ap2::session::{Ap2Config, Ap2Session, SAMPLES_PER_PACKET};
use raop::ap2::srp::{SRP_USERNAME, TRANSIENT_PIN, server::SrpServer};
use raop::ap2::tlv8::{self, tag};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
struct ObservedSetParameter {
    content_type: Option<String>,
    rtp_info: Option<String>,
    body: Vec<u8>,
}

/// What the fake receiver observed, reported back to the test.
#[derive(Debug, Default)]
struct Observed {
    methods: Vec<String>,
    volume_body: Option<String>,
    volume_bodies: Vec<String>,
    set_parameter_requests: Vec<ObservedSetParameter>,
    shk: Option<Vec<u8>>,
    client_control_port: u16,
    client_timing_port: u16,
    event_reply_ok: bool,
    packets: Vec<Vec<u8>>,
    timing_reply_ok: bool,
    retransmit_matches: bool,
}

struct Req {
    method: String,
    uri: String,
    body: Vec<u8>,
    cseq: Option<String>,
    proto: String,
    content_type: Option<String>,
    rtp_info: Option<String>,
}

fn parse_req(buf: &mut Vec<u8>) -> Option<Req> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let clen = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    if buf.len() < end + 4 + clen {
        return None;
    }
    let body = buf[end + 4..end + 4 + clen].to_vec();
    buf.drain(..end + 4 + clen);
    let mut first = head.lines().next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_string();
    let uri = first.next().unwrap().to_string();
    let proto = first.next().unwrap().to_string();
    let cseq = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("cseq").then(|| v.trim().to_string())
    });
    let content_type = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("content-type")
            .then(|| v.trim().to_string())
    });
    let rtp_info = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("rtp-info")
            .then(|| v.trim().to_string())
    });
    Some(Req {
        method,
        uri,
        body,
        cseq,
        proto,
        content_type,
        rtp_info,
    })
}

fn error_response(req: &Req, status: u16, reason: &str) -> Vec<u8> {
    let mut out = format!(
        "{} {status} {reason}\r\nServer: AirTunes/980.77.2\r\n",
        req.proto
    );
    if let Some(c) = &req.cseq {
        out.push_str(&format!("CSeq: {c}\r\n"));
    }
    out.push_str("Content-Length: 0\r\n\r\n");
    out.into_bytes()
}

fn response(req: &Req, body: Option<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut out = format!("{} 200 OK\r\nServer: AirTunes/980.77.2\r\n", req.proto);
    if let Some(c) = &req.cseq {
        out.push_str(&format!("CSeq: {c}\r\n"));
    }
    let b = match body {
        Some((ct, b)) => {
            out.push_str(&format!("Content-Type: {ct}\r\n"));
            b
        }
        None => Vec::new(),
    };
    out.push_str(&format!("Content-Length: {}\r\n\r\n", b.len()));
    let mut v = out.into_bytes();
    v.extend_from_slice(&b);
    v
}

fn bplist(d: Dictionary) -> Vec<u8> {
    let mut out = Vec::new();
    Value::Dictionary(d).to_writer_binary(&mut out).unwrap();
    out
}

async fn run_events_side(listener: TcpListener, key: [u8; 64], done: mpsc::Sender<bool>) {
    let (mut s, _) = listener.accept().await.unwrap();
    // Receiver writes with Events-Write, reads with Events-Read (sender does the opposite).
    let mut h = HapSession::new(
        &hap::hkdf_expand(hap::EVENTS_SALT, hap::EVENTS_WRITE_INFO, &key),
        &hap::hkdf_expand(hap::EVENTS_SALT, hap::EVENTS_READ_INFO, &key),
    );
    let req = b"POST /command RTSP/1.0\r\nCSeq: 42\r\nContent-Length: 0\r\n\r\n";
    s.write_all(&h.encrypt(req)).await.unwrap();
    // A playback command shaped like the ones a HomePod (tvOS 27) forwards from Home Assistant.
    let mut cmd = Dictionary::new();
    cmd.insert("type".into(), "sendMediaRemoteCommand".into());
    cmd.insert("value".into(), "nitm".into());
    cmd.insert("modernMediaRemoteCommand".into(), "4".into());
    cmd.insert("params".into(), Value::Dictionary(Dictionary::new()));
    let body = bplist(cmd);
    let mut req = format!(
        "POST /command RTSP/1.0\r\nCSeq: 43\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(&body);
    s.write_all(&h.encrypt(&req)).await.unwrap();

    // Receiver volume update notification (dvlc) as captured from a HomePod on 2026-09-27.
    let mut vcmd = Dictionary::new();
    vcmd.insert("type".into(), "sendMediaRemoteCommand".into());
    vcmd.insert("value".into(), "dvlc".into());
    vcmd.insert("volume".into(), Value::Real(0.85));
    let mut vparams = Dictionary::new();
    vparams.insert("volume".into(), Value::Real(0.85));
    vcmd.insert("params".into(), Value::Dictionary(vparams));
    let vbody = bplist(vcmd);
    let mut vreq = format!(
        "POST /command RTSP/1.0\r\nCSeq: 44\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
        vbody.len()
    )
    .into_bytes();
    vreq.extend_from_slice(&vbody);
    s.write_all(&h.encrypt(&vreq)).await.unwrap();

    let mut buf = [0u8; 1024];
    let mut plain = Vec::new();
    let ok = loop {
        match tokio::time::timeout(Duration::from_secs(3), s.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                plain.extend(h.decrypt(&buf[..n]).unwrap());
                let text = String::from_utf8_lossy(&plain);
                if text.matches("\r\n\r\n").count() >= 3 {
                    break text.starts_with("RTSP/1.0 200 OK")
                        && text.contains("CSeq: 42")
                        && text.contains("CSeq: 43")
                        && text.contains("CSeq: 44");
                }
            }
            _ => break false,
        }
    };
    let _ = done.send(ok).await;
}

/// Runs the accessory side on `listener` until TEARDOWN; returns what it saw.
async fn fake_receiver(listener: TcpListener) -> Observed {
    fake_receiver_with_opts(listener, None, None).await
}

async fn fake_receiver_with_opts(
    listener: TcpListener,
    reject_artwork_status: Option<u16>,
    delay_artwork_response: Option<Duration>,
) -> Observed {
    let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
    audio
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    {
        let a = audio.try_clone().unwrap();
        let c = collected.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 2048];
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                // Only audio (RTP PT 0x60); sync packets share this port in the fake.
                if let Ok((n, _)) = a.recv_from(&mut b) {
                    if n < 100 || b[1] & 0x7f != 0x60 {
                        continue;
                    }
                    let mut v = c.lock().unwrap();
                    if v.len() < 60 {
                        v.push(b[..n].to_vec());
                    }
                }
            }
        });
    }
    let (probe_tx, mut probe_rx) = mpsc::channel::<(bool, bool)>(1);
    let (mut sock, peer) = listener.accept().await.unwrap();
    let mut obs = Observed::default();
    let mut buf = Vec::new();
    let mut hap: Option<HapSession> = None;
    let srp = SrpServer::new(&[0x5a; 16], &[0x42; 32], SRP_USERNAME, TRANSIENT_PIN);
    let mut key = [0u8; 64];
    let (ev_tx, mut ev_rx) = mpsc::channel(1);
    let mut rbuf = [0u8; 8192];
    loop {
        let n = sock.read(&mut rbuf).await.unwrap();
        if n == 0 {
            break;
        }
        match hap.as_mut() {
            Some(h) => buf.extend(h.decrypt(&rbuf[..n]).unwrap()),
            None => buf.extend_from_slice(&rbuf[..n]),
        }
        while let Some(req) = parse_req(&mut buf) {
            obs.methods.push(format!("{} {}", req.method, req.uri));
            let mut enable_hap = false;
            let resp = match (req.method.as_str(), req.uri.as_str()) {
                ("POST", "/pair-setup") => {
                    let t = tlv8::read(&req.body).unwrap();
                    if t[&tag::SEQ_NO] == [1] {
                        assert_eq!(t[&tag::FLAGS], [tlv8::FLAG_TRANSIENT]);
                        let b = srp.b_pub();
                        response(
                            &req,
                            Some((
                                "application/octet-stream",
                                tlv8::write(&[
                                    (tag::SEQ_NO, &[2]),
                                    (tag::SALT, &[0x5a; 16]),
                                    (tag::PUBLIC_KEY, &b),
                                ]),
                            )),
                        )
                    } else {
                        let (k, m2) = srp
                            .verify(&t[&tag::PUBLIC_KEY], &t[&tag::PROOF], SRP_USERNAME)
                            .expect("client SRP proof must verify");
                        key = k;
                        enable_hap = true;
                        response(
                            &req,
                            Some((
                                "application/octet-stream",
                                tlv8::write(&[(tag::SEQ_NO, &[4]), (tag::PROOF, &m2)]),
                            )),
                        )
                    }
                }
                ("SETUP", _) => {
                    let d: Dictionary = plist::from_bytes(&req.body).unwrap();
                    if let Some(streams) = d.get("streams") {
                        let st = streams.as_array().unwrap()[0].as_dictionary().unwrap();
                        assert_eq!(st.get("type").unwrap().as_unsigned_integer(), Some(0x60));
                        assert_eq!(st.get("ct").unwrap().as_unsigned_integer(), Some(1));
                        obs.shk = st.get("shk").unwrap().as_data().map(|d| d.to_vec());
                        obs.client_control_port = st
                            .get("controlPort")
                            .unwrap()
                            .as_unsigned_integer()
                            .unwrap() as u16;
                        let mut s0 = Dictionary::new();
                        s0.insert("type".into(), Value::Integer(96.into()));
                        s0.insert(
                            "dataPort".into(),
                            Value::Integer((audio.local_addr().unwrap().port() as u64).into()),
                        );
                        s0.insert(
                            "controlPort".into(),
                            Value::Integer((audio.local_addr().unwrap().port() as u64).into()),
                        );
                        s0.insert("arrivalToRenderLatencyMs".into(), Value::Integer(70.into()));
                        let mut root = Dictionary::new();
                        root.insert("streams".into(), Value::Array(vec![Value::Dictionary(s0)]));
                        response(
                            &req,
                            Some(("application/x-apple-binary-plist", bplist(root))),
                        )
                    } else {
                        assert_eq!(d.get("timingProtocol").unwrap().as_string(), Some("NTP"));
                        obs.client_timing_port =
                            d.get("timingPort").unwrap().as_unsigned_integer().unwrap() as u16;
                        let ev = TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let ev_port = ev.local_addr().unwrap().port();
                        tokio::spawn(run_events_side(ev, key, ev_tx.clone()));
                        let mut root = Dictionary::new();
                        root.insert("eventPort".into(), Value::Integer((ev_port as u64).into()));
                        root.insert("skipRecord".into(), Value::Boolean(true));
                        response(
                            &req,
                            Some(("application/x-apple-binary-plist", bplist(root))),
                        )
                    }
                }
                ("SET_PARAMETER", _) => {
                    let param = ObservedSetParameter {
                        content_type: req.content_type.clone(),
                        rtp_info: req.rtp_info.clone(),
                        body: req.body.clone(),
                    };
                    obs.set_parameter_requests.push(param);
                    let body = String::from_utf8_lossy(&req.body).to_string();
                    if body.starts_with("volume:") {
                        obs.volume_body = Some(body.clone());
                        obs.volume_bodies.push(body);
                    }
                    let is_artwork = matches!(
                        req.content_type.as_deref(),
                        Some("image/jpeg") | Some("image/png")
                    );
                    if is_artwork && let Some(d) = delay_artwork_response {
                        tokio::time::sleep(d).await;
                    }
                    if let Some(code) = reject_artwork_status
                        && is_artwork
                    {
                        error_response(&req, code, "Not Implemented")
                    } else {
                        response(&req, None)
                    }
                }
                _ => response(&req, None),
            };
            let wire = match hap.as_mut() {
                Some(h) => h.encrypt(&resp),
                None => resp,
            };
            sock.write_all(&wire).await.unwrap();
            if enable_hap {
                hap = Some(HapSession::new(
                    &hap::hkdf_expand(hap::CONTROL_SALT, hap::CONTROL_READ_INFO, &key),
                    &hap::hkdf_expand(hap::CONTROL_SALT, hap::CONTROL_WRITE_INFO, &key),
                ));
            }
            if req.method == "FLUSH" {
                let (tport, cport, c, tx) = (
                    obs.client_timing_port,
                    obs.client_control_port,
                    collected.clone(),
                    probe_tx.clone(),
                );
                tokio::task::spawn_blocking(move || {
                    std::thread::sleep(Duration::from_millis(400));
                    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
                    probe
                        .set_read_timeout(Some(Duration::from_millis(500)))
                        .unwrap();
                    let mut b = [0u8; 2048];
                    // Timing: 0x80 0xd2 request (32 bytes) -> expect 0x80 0xd3.
                    let mut treq = [0u8; 32];
                    treq[0] = 0x80;
                    treq[1] = 0xd2;
                    treq[3] = 7;
                    probe.send_to(&treq, ("127.0.0.1", tport)).unwrap();
                    let timing_ok = matches!(probe.recv_from(&mut b), Ok((32, _)) if b[1] == 0xd3);
                    // Retransmit: ask for the 3rd packet we received.
                    let wanted = c
                        .lock()
                        .unwrap()
                        .get(2)
                        .cloned()
                        .expect("audio packets arrived");
                    let seq = u16::from_be_bytes([wanted[2], wanted[3]]);
                    let mut rreq = vec![0x80, 0xd5, 0x00, 0x01];
                    rreq.extend_from_slice(&seq.to_be_bytes());
                    rreq.extend_from_slice(&1u16.to_be_bytes());
                    probe.send_to(&rreq, ("127.0.0.1", cport)).unwrap();
                    let rt_ok = loop {
                        match probe.recv_from(&mut b) {
                            Ok((n, _)) if b[1] == 0xd6 => {
                                break b[2..4] == seq.to_be_bytes() && b[4..n] == wanted[..];
                            }
                            Ok(_) => continue, // sync packets also arrive on this path? ignore
                            Err(_) => break false,
                        }
                    };
                    let _ = tx.blocking_send((timing_ok, rt_ok));
                });
            }
            if req.method == "TEARDOWN" {
                obs.event_reply_ok = ev_rx.try_recv().unwrap_or(false);
                if let Ok((t, r)) = probe_rx.try_recv() {
                    obs.timing_reply_ok = t;
                    obs.retransmit_matches = r;
                }
                obs.packets = collected.lock().unwrap().clone();
                let _ = peer;
                return obs;
            }
        }
    }
    obs
}

fn open_packet(shk: &[u8], pkt: &[u8]) -> Vec<i16> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(shk));
    let body_end = pkt.len() - 24;
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&pkt[pkt.len() - 8..]);
    let mut data = pkt[12..body_end].to_vec();
    let tag = Tag::clone_from_slice(&pkt[body_end..body_end + 16]);
    cipher
        .decrypt_in_place_detached(Nonce::from_slice(&nonce), &pkt[4..12], &mut data, &tag)
        .expect("audio packet must authenticate with shk");
    data.chunks_exact(2)
        .map(|b| i16::from_be_bytes([b[0], b[1]]))
        .collect()
}

async fn run_session(quiet: bool) -> (Observed, bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target: SocketAddr = listener.local_addr().unwrap();
    let recv = tokio::spawn(fake_receiver(listener));
    let cfg = Ap2Config {
        target,
        latency_frames: -100 * 441 / 10,
        volume_db: Some(-12.0),
        quiet_mode: quiet,
        ..Default::default()
    };
    let mut counter: i32 = 0;
    let mut session = Ap2Session::start(cfg, move |pcm: &mut [i16; SAMPLES_PER_PACKET]| {
        for s in pcm.iter_mut() {
            *s = (counter % 20_000) as i16;
            counter += 1;
        }
    })
    .await
    .expect("session starts against fake receiver");
    let mut commands = session.take_remote_commands().expect("command receiver");
    assert!(session.take_remote_commands().is_none());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        commands.try_recv().ok(),
        Some(raop::ap2::remote::Command::Next),
        "events-channel sendMediaRemoteCommand must reach the command receiver"
    );
    if !quiet {
        assert_eq!(
            commands.try_recv().ok(),
            Some(raop::ap2::remote::Command::Volume(0.85)),
            "events-channel dvlc volume update must reach the command receiver"
        );
    } else {
        assert_eq!(
            commands.try_recv().ok(),
            None,
            "quiet mode must drop dvlc volume updates"
        );
    }
    session.feedback().await.unwrap();
    let violated = session.quiet_violations() > 0;
    let stats = session.stats();
    session.stop().await;
    let obs = recv.await.unwrap();
    use std::sync::atomic::Ordering::Relaxed;
    if !quiet {
        assert!(
            stats.retransmit_served.load(Relaxed) >= 1,
            "retransmit must be served"
        );
        assert!(
            stats.timing_requests.load(Relaxed) >= 1,
            "timing request must be answered"
        );
    }
    (obs, violated)
}

#[tokio::test]
async fn ap2_sender_full_flow_against_fake_receiver() {
    let (obs, _) = run_session(false).await;
    let packets = &obs.packets;
    assert_eq!(
        obs.methods[..4],
        [
            "GET /info",
            "POST /pair-pin-start",
            "POST /pair-setup",
            "POST /pair-setup"
        ]
    );
    for m in ["SETUP", "RECORD", "FLUSH", "TEARDOWN"] {
        assert!(
            obs.methods.iter().any(|x| x.starts_with(m)),
            "missing {m}: {:?}",
            obs.methods
        );
    }
    assert!(
        obs.event_reply_ok,
        "events channel request must be answered with 200"
    );
    assert_eq!(obs.volume_body.as_deref(), Some("volume: -12.0"));
    let shk = obs.shk.expect("stream SETUP carried shk");
    // Decrypt and check the counter pattern is continuous across packets.
    let mut expected: i32 = 0;
    for p in packets {
        assert_eq!(p.len(), 12 + 1408 + 24);
        for s in open_packet(&shk, p) {
            assert_eq!(s, (expected % 20_000) as i16);
            expected += 1;
        }
    }
    assert!(
        packets.len() >= 20,
        "expected >= 20 audio packets, got {}",
        packets.len()
    );
    assert!(obs.timing_reply_ok, "timing request must get a 0xd3 reply");
    assert!(
        obs.retransmit_matches,
        "retransmitted packet must equal the original"
    );
}

#[tokio::test]
async fn ap2_quiet_mode_forces_mute_and_clamps() {
    let (obs, violated) = run_session(true).await;
    let packets = &obs.packets;
    assert_eq!(obs.volume_body.as_deref(), Some("volume: -144.0"));
    let shk = obs.shk.unwrap();
    let max = packets
        .iter()
        .flat_map(|p| open_packet(&shk, p))
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap();
    assert!(max <= 33, "quiet mode must clamp to <= -60 dBFS, got {max}");
    assert!(violated, "loud input must be counted as quiet violations");
}

/// A "live" source that returns data immediately and without limit, i.e. a capture device
/// with an arbitrarily large backlog. With backlog trimming the sender must drop packets.
struct BacklogLiveSource {
    frame: u64,
}

impl capture::AudioSource for BacklogLiveSource {
    fn sample_rate(&self) -> u32 {
        44_100
    }
    fn channels(&self) -> u16 {
        2
    }
    fn is_live(&self) -> bool {
        true
    }
    fn read_chunk(
        &mut self,
        max_frames: usize,
    ) -> Result<Option<capture::AudioChunk>, Box<dyn std::error::Error + Send + Sync>> {
        let mut data = Vec::with_capacity(max_frames * 2);
        for _ in 0..max_frames {
            let v = ((self.frame % 1000) as f32) / 32768.0;
            data.push(v);
            data.push(v);
            self.frame += 1;
        }
        Ok(Some(capture::AudioChunk {
            data,
            sample_rate: 44_100,
            channels: 2,
            timestamp: std::time::Instant::now(),
        }))
    }
}

#[tokio::test]
async fn ap2_live_source_backlog_is_dropped() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target: SocketAddr = listener.local_addr().unwrap();
    let recv = tokio::spawn(fake_receiver(listener));
    let cfg = Ap2Config {
        target,
        quiet_mode: false,
        volume_db: Some(-20.0),
        ..Default::default()
    };
    let metrics = std::sync::Arc::new(pipeline::PipelineMetricsTracker::new(32));
    let mut session = Ap2Session::start_with_source(
        cfg,
        Box::new(BacklogLiveSource { frame: 0 }),
        metrics.clone(),
        32,
    )
    .await
    .expect("session starts");
    tokio::time::sleep(Duration::from_millis(800)).await;
    session.feedback().await.unwrap();
    session.stop().await;
    let _ = recv.await.unwrap();
    let snap = metrics.snapshot();
    assert!(
        snap.packets_dropped > 0,
        "live backlog must be trimmed, snapshot: {snap:?}"
    );
}

#[tokio::test]
async fn ap2_live_set_volume_sends_set_parameter() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target: SocketAddr = listener.local_addr().unwrap();
    let recv = tokio::spawn(fake_receiver(listener));
    let cfg = Ap2Config {
        target,
        latency_frames: -100 * 441 / 10,
        volume_db: Some(-20.0),
        quiet_mode: false,
        ..Default::default()
    };
    let mut session = Ap2Session::start(cfg, |_pcm| {})
        .await
        .expect("session starts against fake receiver");
    tokio::time::sleep(Duration::from_millis(300)).await;
    session
        .set_volume(-10.0)
        .await
        .expect("set_volume succeeds");
    tokio::time::sleep(Duration::from_millis(300)).await;
    session.feedback().await.unwrap();
    session.stop().await;
    let obs = recv.await.unwrap();
    assert_eq!(
        obs.volume_bodies,
        vec!["volume: -20.0".to_string(), "volume: -10.0".to_string()]
    );
}

#[tokio::test]
async fn ap2_metadata_send_text_artwork_progress_and_error_handling() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target: SocketAddr = listener.local_addr().unwrap();
    // Configure fake receiver to reject artwork with 501 Not Implemented
    let recv = tokio::spawn(fake_receiver_with_opts(listener, Some(501), None));

    let cfg = Ap2Config {
        target,
        latency_frames: -100 * 441 / 10,
        volume_db: Some(-20.0),
        quiet_mode: false,
        ..Default::default()
    };
    let mut session = Ap2Session::start(cfg, |_pcm| {})
        .await
        .expect("session starts against fake receiver");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 1. Send track info (text via DAAP)
    let track = TrackInfo::new("夜に駆ける", "YOASOBI", "THE BOOK");
    session
        .send_track_info(&track)
        .await
        .expect("send_track_info succeeds");

    // 2. Send artwork (fake receiver responds with 501)
    let fake_jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46];
    let artwork_err = session
        .send_artwork(&fake_jpeg)
        .await
        .expect_err("fake receiver returns 501 for artwork");
    match artwork_err {
        raop::ap2::session::Ap2Error::Status { status, .. } => assert_eq!(status, 501),
        other => panic!("expected Status 501, got {other:?}"),
    }
    assert!(
        !session.is_artwork_enabled(),
        "artwork must be disabled after error"
    );

    // Calling send_artwork again must be a no-op (doesn't send to receiver, returns Ok)
    session
        .send_artwork(&fake_jpeg)
        .await
        .expect("disabled artwork returns Ok without sending");

    // 3. Send progress
    session
        .send_progress(Duration::from_secs(15), Duration::from_secs(260))
        .await
        .expect("send_progress succeeds");

    tokio::time::sleep(Duration::from_millis(300)).await;
    session.feedback().await.unwrap();
    session.stop().await;

    let obs = recv.await.unwrap();

    // Verify SET_PARAMETER requests:
    // Text (TrackInfo)
    let dmap_req = obs
        .set_parameter_requests
        .iter()
        .find(|r| r.content_type.as_deref() == Some("application/x-dmap-tagged"))
        .expect("dmap SET_PARAMETER received");
    assert!(
        dmap_req.rtp_info.is_some(),
        "RTP-Info must be present on text metadata"
    );
    let decoded = metadata::decode_dmap(&dmap_req.body).expect("decode dmap");
    assert_eq!(decoded, track);

    // Artwork (exactly 1 attempt)
    let artwork_reqs: Vec<_> = obs
        .set_parameter_requests
        .iter()
        .filter(|r| r.content_type.as_deref() == Some("image/jpeg"))
        .collect();
    assert_eq!(
        artwork_reqs.len(),
        1,
        "exactly 1 artwork request sent despite multiple calls"
    );
    assert!(
        artwork_reqs[0].rtp_info.is_some(),
        "RTP-Info must be present on artwork"
    );
    assert_eq!(artwork_reqs[0].body, fake_jpeg);

    // Progress
    let progress_reqs: Vec<_> = obs
        .set_parameter_requests
        .iter()
        .filter(|r| r.content_type.as_deref() == Some("text/parameters"))
        .collect();
    let updated_progress = progress_reqs
        .iter()
        .find(|r| {
            let s = String::from_utf8_lossy(&r.body);
            s.starts_with("progress: ") && r.rtp_info.is_some()
        })
        .expect("updated progress received");
    let s = String::from_utf8_lossy(&updated_progress.body);
    assert!(s.ends_with("\r\n"), "progress body must end with \\r\\n");

    // Audio stream kept streaming uninterrupted
    assert!(
        obs.packets.len() >= 5,
        "audio packets must keep streaming despite artwork error"
    );
}

/// A metadata response that arrives after the wait limit must not be mistaken for the
/// answer to the next request on the control channel (stale CSeq is dropped), and the
/// session keeps working.
#[tokio::test]
async fn ap2_late_metadata_response_does_not_desync_control_channel() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target: SocketAddr = listener.local_addr().unwrap();
    // Artwork is answered with 501 only after 3.5 s, past the 3 s metadata wait.
    let recv = tokio::spawn(fake_receiver_with_opts(
        listener,
        Some(501),
        Some(Duration::from_millis(3500)),
    ));

    let cfg = Ap2Config {
        target,
        latency_frames: -100 * 441 / 10,
        volume_db: Some(-20.0),
        quiet_mode: false,
        ..Default::default()
    };
    let mut session = Ap2Session::start(cfg, |_pcm| {})
        .await
        .expect("session starts against fake receiver");

    let fake_jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46];
    session
        .send_artwork(&fake_jpeg)
        .await
        .expect_err("artwork wait times out");
    assert!(!session.is_artwork_enabled());

    // The late 501 for the artwork arrives while this request waits; it must be skipped.
    session
        .set_volume(-15.0)
        .await
        .expect("volume succeeds despite the late artwork response");
    session.feedback().await.expect("feedback still in sync");
    session.stop().await;

    let obs = recv.await.unwrap();
    assert!(
        obs.volume_bodies
            .iter()
            .any(|b| b.starts_with("volume: -15.0")),
        "receiver got the volume change: {:?}",
        obs.volume_bodies
    );
}

#[tokio::test]
async fn ap2_initial_progress_default_false_and_opt_in_true() {
    // 1. Default config: initial_progress is false, no progress before RECORD
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target: SocketAddr = listener.local_addr().unwrap();
        let recv = tokio::spawn(fake_receiver(listener));
        let cfg = Ap2Config {
            target,
            latency_frames: -100 * 441 / 10,
            volume_db: Some(-20.0),
            quiet_mode: false,
            ..Default::default()
        };
        assert!(
            !cfg.initial_progress,
            "default initial_progress must be false"
        );
        let session = Ap2Session::start(cfg, |_pcm| {})
            .await
            .expect("session starts against fake receiver");
        tokio::time::sleep(Duration::from_millis(200)).await;
        session.stop().await;
        let obs = recv.await.unwrap();

        let record_pos = obs
            .methods
            .iter()
            .position(|m| m.starts_with("RECORD"))
            .expect("RECORD must be sent");
        let pre_record_set_params = &obs.methods[..record_pos];
        let pre_record_set_param_count = pre_record_set_params
            .iter()
            .filter(|m| m.starts_with("SET_PARAMETER"))
            .count();
        assert_eq!(
            pre_record_set_param_count, 1,
            "only volume SET_PARAMETER before RECORD by default"
        );
        let has_progress = obs
            .set_parameter_requests
            .iter()
            .any(|r| r.body.starts_with(b"progress:"));
        assert!(
            !has_progress,
            "by default no progress: SET_PARAMETER should be sent"
        );
    }

    // 2. Opt-in: initial_progress is true, progress: is sent before RECORD
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target: SocketAddr = listener.local_addr().unwrap();
        let recv = tokio::spawn(fake_receiver(listener));
        let cfg = Ap2Config {
            target,
            latency_frames: -100 * 441 / 10,
            volume_db: Some(-20.0),
            quiet_mode: false,
            initial_progress: true,
            ..Default::default()
        };
        let session = Ap2Session::start(cfg, |_pcm| {})
            .await
            .expect("session starts against fake receiver");
        tokio::time::sleep(Duration::from_millis(200)).await;
        session.stop().await;
        let obs = recv.await.unwrap();

        let record_pos = obs
            .methods
            .iter()
            .position(|m| m.starts_with("RECORD"))
            .expect("RECORD must be sent");
        let pre_record_set_params = &obs.methods[..record_pos];
        let pre_record_set_param_count = pre_record_set_params
            .iter()
            .filter(|m| m.starts_with("SET_PARAMETER"))
            .count();
        assert_eq!(
            pre_record_set_param_count, 2,
            "volume and progress SET_PARAMETER before RECORD when initial_progress is true"
        );
        let progress_req = obs
            .set_parameter_requests
            .iter()
            .find(|r| r.body.starts_with(b"progress:"))
            .expect("progress: SET_PARAMETER must be sent before RECORD");
        let text = String::from_utf8_lossy(&progress_req.body);
        assert!(
            text.starts_with("progress: "),
            "progress body must start with 'progress: '"
        );
    }
}
