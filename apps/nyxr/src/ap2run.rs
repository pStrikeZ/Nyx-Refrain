//! CLI glue for AirPlay 2 realtime streaming (latency profiles, run loop).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use capture::AudioSource;
use raop::ap2::session::{Ap2Config, Ap2Error, Ap2Session, MIN_SYNC_LATENCY_MS};
use tokio::io::AsyncBufReadExt;
use tracing::{info, warn};

#[cfg(target_os = "linux")]
type SinkVol = capture::pipewire::SinkVolume;
#[cfg(not(target_os = "linux"))]
type SinkVol = ();

/// AirPlay 2 latency presets. End-to-end values measured on a HomePod mini:
/// end-to-end ≈ 237 ms + sync latency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum LatencyProfile {
    /// Sync latency -140 ms (≈97 ms end-to-end); little headroom for Wi-Fi jitter.
    Low,
    /// Sync latency -100 ms (≈137 ms end-to-end). Default.
    Balanced,
    /// Sync latency 0 ms (≈237 ms end-to-end), the receiver's native buffering.
    Stable,
}

impl LatencyProfile {
    pub fn sync_latency_ms(self) -> i32 {
        match self {
            LatencyProfile::Low => -140,
            LatencyProfile::Balanced => -100,
            LatencyProfile::Stable => 0,
        }
    }
}

/// Resolves the effective sync latency; explicit override wins over the profile.
pub fn sync_latency_ms(profile: LatencyProfile, explicit: Option<i32>) -> anyhow::Result<i32> {
    let ms = explicit.unwrap_or_else(|| profile.sync_latency_ms());
    if ms < MIN_SYNC_LATENCY_MS {
        anyhow::bail!(
            "--ap2-sync-latency-ms {ms} is below {MIN_SYNC_LATENCY_MS} ms, the lowest value measured to play cleanly (-180 ms crackles, -195 ms is silent)"
        );
    }
    if ms > 3000 {
        anyhow::bail!("--ap2-sync-latency-ms {ms} is above 3000 ms");
    }
    Ok(ms)
}

pub struct Ap2Run {
    pub target: SocketAddr,
    pub interface: Option<String>,
    pub volume_db: f32,
    pub quiet: bool,
    pub sync_latency_ms: i32,
    pub duration: Option<u64>,
    pub stats: bool,
    pub transcript: Option<std::path::PathBuf>,
    pub now_playing: bool,
    pub remote_control: bool,
    /// Experiment hook: AP2 requests typed on stdin (`--debug-stdin`).
    pub debug_stdin: bool,
    pub features: Option<String>,
    /// Send the placeholder `progress` before RECORD (see `Ap2Config::initial_progress`).
    pub initial_progress: bool,
    /// Fixed track info (title, artist, album) sent once after the session starts.
    pub static_track: Option<raop::ap2::metadata::TrackInfo>,
    #[cfg(target_os = "linux")]
    pub volume_control: Option<capture::pipewire::SinkVolumeControl>,
}

/// Streams until `duration` elapses or Ctrl+C. Returns `Err` only if the session could not
/// be established.
pub async fn run_ap2(run: Ap2Run, source: Box<dyn AudioSource + Send>) -> Result<(), Ap2Error> {
    let cfg = Ap2Config {
        target: run.target,
        interface_name: run.interface.clone(),
        latency_frames: run.sync_latency_ms * 441 / 10,
        volume_db: run.volume_db,
        quiet_mode: run.quiet,
        transcript: run.transcript.clone(),
        initial_progress: run.initial_progress,
        ..Default::default()
    };
    let ring = pipeline::DEFAULT_RING_BUFFER_PACKETS;
    let metrics = Arc::new(pipeline::PipelineMetricsTracker::new(ring));
    info!(
        "AirPlay 2 realtime to {} (sync latency {} ms, ≈{} ms end-to-end on HomePod), quiet={}",
        run.target,
        run.sync_latency_ms,
        237 + run.sync_latency_ms,
        run.quiet
    );
    let mut session = Ap2Session::start_with_source(cfg, source, metrics.clone(), ring).await?;
    let stats = session.stats();

    if let Some(track) = &run.static_track {
        match session.send_track_info(track).await {
            Ok(()) => info!("Sent static track info {track:?}"),
            Err(e) => warn!("Static track info rejected: {e}"),
        }
    }

    let mut last_sent_track: Option<nowplaying::NowPlaying> = None;

    let mut now_playing_rx = if run.now_playing {
        let features = run
            .features
            .as_deref()
            .and_then(raop::ap2::metadata::Ap2Features::parse);
        session.apply_features(features);
        let watcher = nowplaying::NowPlayingWatcher::start();
        let rx = watcher.subscribe();
        let np = rx.borrow().clone();
        let decision = nowplaying::decide_metadata_send(last_sent_track.as_ref(), Some(&np), true);
        send_cli_metadata(&mut session, &decision, &mut last_sent_track).await;
        Some((watcher, rx))
    } else {
        None
    };

    // Receiver events arrive on the events channel (volume updates and playback commands).
    let mut remote_rx = session.take_remote_commands();
    if run.now_playing && run.remote_control {
        info!("Receiver playback controls enabled");
    }

    #[cfg(target_os = "linux")]
    let mut vol_rx = run.volume_control.as_ref().map(|ctrl| {
        let (tx, rx) = tokio::sync::watch::channel(None);
        ctrl.on_change(move |v| {
            let _ = tx.send(Some(v));
        });
        rx
    });
    #[cfg(target_os = "linux")]
    let mut last_receiver_vol: Option<f32> = None;
    #[cfg(target_os = "linux")]
    let mut quiet_logged = false;

    let deadline = run
        .duration
        .map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    let mut feedback = tokio::time::interval(Duration::from_secs(2));
    feedback.tick().await;
    let mut report = tokio::time::interval(Duration::from_secs(5));
    report.tick().await;
    let sleep_until_deadline = async {
        match deadline {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(sleep_until_deadline);
    let mut stdin_lines = run.debug_stdin.then(|| {
        info!("--debug-stdin: type `help` for the AP2 requests you can send");
        tokio::io::BufReader::new(tokio::io::stdin()).lines()
    });
    loop {
        tokio::select! {
            line = async {
                match stdin_lines.as_mut() {
                    Some(lines) => lines.next_line().await,
                    None => std::future::pending().await,
                }
            } => {
                match line {
                    Ok(Some(line)) => debug_stdin_command(&mut session, line.trim()).await,
                    _ => stdin_lines = None,
                }
            }
            _ = &mut sleep_until_deadline => {
                info!("Duration elapsed; stopping (FLUSH/TEARDOWN)");
                break;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C; stopping (TEARDOWN)");
                break;
            }
            _ = feedback.tick() => {
                if let Err(e) = session.feedback().await {
                    warn!("AirPlay 2 keep-alive failed: {e}; stopping");
                    break;
                }
            }
            _ = report.tick(), if run.stats => {
                println!("{}", metrics.snapshot().to_json_line());
            }
            _v = async {
                #[cfg(target_os = "linux")]
                if let Some(rx) = &mut vol_rx
                    && rx.changed().await.is_ok()
                {
                    return *rx.borrow_and_update();
                }
                std::future::pending::<Option<SinkVol>>().await
            } => {
                #[cfg(target_os = "linux")]
                if let Some(vol) = _v {
                    let is_echo = last_receiver_vol
                        .map(|last| (vol.pct - last).abs() < 1.0)
                        .unwrap_or(false);
                    last_receiver_vol = None;
                    if !is_echo {
                        if run.quiet {
                            if !quiet_logged {
                                quiet_logged = true;
                                warn!("Sink volume change ignored: quiet mode is active");
                            }
                        } else {
                            let db = raop::volume::pct_to_db(if vol.mute { 0.0 } else { vol.pct });
                            if let Err(e) = session.set_volume(db).await {
                                warn!("Failed to set receiver volume to {db:.1} dB: {e}");
                            }
                        }
                    }
                }
            }
            command = async {
                if let Some(rx) = &mut remote_rx {
                    return rx.recv().await;
                }
                std::future::pending().await
            } => {
                match command {
                    Some(raop::ap2::remote::Command::Volume(vol)) => {
                        if run.quiet {
                            warn!("Receiver volume update ignored: quiet mode is active");
                        } else {
                            let pct = (vol * 100.0).clamp(0.0, 100.0);
                            info!("Receiver volume: {pct:.0}%");
                            #[cfg(target_os = "linux")]
                            if let Some(ctrl) = &run.volume_control {
                                last_receiver_vol = Some(pct);
                                ctrl.set(pct, pct <= 0.0);
                            }
                        }
                    }
                    Some(cmd) => {
                        if run.now_playing && run.remote_control
                            && let Some((watcher, _)) = &now_playing_rx
                            && let Some(media_cmd) = nowplaying_command(cmd)
                        {
                            watcher.control(media_cmd);
                        }
                    }
                    None => {
                        remote_rx = None;
                    }
                }
            }
            _np = async {
                if let Some((_, rx)) = &mut now_playing_rx
                    && rx.changed().await.is_ok()
                {
                    return Some(rx.borrow_and_update().clone());
                }
                std::future::pending::<Option<nowplaying::NowPlaying>>().await
            } => {
                if let Some(np) = _np {
                    let decision = nowplaying::decide_metadata_send(
                        last_sent_track.as_ref(),
                        Some(&np),
                        true,
                    );
                    send_cli_metadata(
                        &mut session,
                        &decision,
                        &mut last_sent_track,
                    )
                    .await;
                }
            }
        }
    }
    let quiet_violations = session.quiet_violations();
    drop(now_playing_rx);
    session.stop().await;

    if run.stats {
        println!("--- AirPlay 2 Session Statistics ---");
        println!(
            "Packets Sent:          {}",
            stats.packets_sent.load(Relaxed)
        );
        println!(
            "Retransmit Requests:   {}",
            stats.retransmit_requests.load(Relaxed)
        );
        println!(
            "Retransmit Served:     {}",
            stats.retransmit_served.load(Relaxed)
        );
        println!(
            "Retransmit Missing:    {}",
            stats.retransmit_missing.load(Relaxed)
        );
        println!(
            "Timing Requests:       {}",
            stats.timing_requests.load(Relaxed)
        );
        println!(
            "Event Requests:        {}",
            stats.event_requests.load(Relaxed)
        );
        println!("Quiet Violations:      {quiet_violations}");
    }
    Ok(())
}

async fn send_cli_metadata(
    session: &mut Ap2Session,
    decision: &nowplaying::MetadataDecision,
    last_sent: &mut Option<nowplaying::NowPlaying>,
) {
    if !decision.has_action() {
        return;
    }

    // Artwork before text. A HomePod assigns a new artwork id as soon as the text changes and
    // Home Assistant fetches it at once; artwork arriving just after the text was then never
    // shown (seen when returning to the placeholder), artwork first always is. No progress is
    // sent (see nowplaying::decide_metadata_send).
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

/// One `--debug-stdin` line: an AP2 request to try against the receiver.
async fn debug_stdin_command(session: &mut Ap2Session, line: &str) {
    use plist::{Dictionary, Value};
    let mut words = line.split_whitespace();
    let Some(word) = words.next() else { return };
    let arg = words.next();
    let rtp = session.rtp_now();
    let (method, extra, body): (&str, Vec<(&str, String)>, Option<Dictionary>) = match word {
        "rate" | "rate+" => {
            let Some(rate) = arg.and_then(|a| a.parse::<i64>().ok()) else {
                warn!("usage: {word} <n>");
                return;
            };
            let mut d = Dictionary::new();
            d.insert("rate".into(), Value::Integer(rate.into()));
            if word == "rate+" {
                d.insert("rtpTime".into(), Value::Integer(u64::from(rtp).into()));
            }
            ("SETRATEANCHORTIME", Vec::new(), Some(d))
        }
        "flush" => (
            "FLUSH",
            vec![
                ("Range", "npt=0-".into()),
                ("Session", "0".into()),
                ("RTP-Info", format!("rtptime={rtp}")),
            ],
            None,
        ),
        _ => {
            info!(
                "--debug-stdin commands: `rate <n>` (SETRATEANCHORTIME {{rate}}), \
                 `rate+ <n>` (also rtpTime = now), `flush` (FLUSH at now)"
            );
            return;
        }
    };
    match session.debug_request(method, &extra, body).await {
        Ok(response) => info!("debug {line}: {method} -> {response}"),
        Err(e) => warn!("debug {line}: {method} failed: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_and_bounds() {
        assert_eq!(
            sync_latency_ms(LatencyProfile::Balanced, None).unwrap(),
            -100
        );
        assert_eq!(sync_latency_ms(LatencyProfile::Low, None).unwrap(), -140);
        assert_eq!(
            sync_latency_ms(LatencyProfile::Stable, Some(-50)).unwrap(),
            -50
        );
        assert!(sync_latency_ms(LatencyProfile::Stable, Some(-166)).is_err());
        assert!(sync_latency_ms(LatencyProfile::Stable, Some(3001)).is_err());
    }
}
