//! Minimal AirPlay 2 realtime tone sender used to validate `raop::ap2` on a real device.
//! Usage: ap2_tone <ip[:port]> <freq_hz> <seconds> <latency_ms> <volume_db> [transcript]
//! Refuses to run unless NYX_REFRAIN_AUDIBLE_OK=1 is set (audible output).

use std::net::SocketAddr;
use std::time::Duration;

use raop::ap2::session::{Ap2Config, Ap2Session, SAMPLES_PER_PACKET};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!(
            "usage: ap2_tone <ip[:port]> <freq> <seconds> <latency_ms> <volume_db> [transcript]"
        );
        std::process::exit(2);
    }
    if std::env::var("NYX_REFRAIN_AUDIBLE_OK").as_deref() != Ok("1") {
        eprintln!("refusing: set NYX_REFRAIN_AUDIBLE_OK=1 to allow audible output");
        std::process::exit(2);
    }
    let target: SocketAddr = if args[1].contains(':') {
        args[1].parse().unwrap()
    } else {
        format!("{}:7000", args[1]).parse().unwrap()
    };
    let freq: f64 = args[2].parse().unwrap();
    let secs: u64 = args[3].parse().unwrap();
    let latency_ms: u32 = args[4].parse().unwrap();
    let volume_db: f32 = args[5].parse().unwrap();
    let cfg = Ap2Config {
        target,
        latency_frames: (latency_ms * 441 / 10) as i32,
        volume_db: Some(volume_db),
        quiet_mode: false, // audible tool; guarded by NYX_REFRAIN_AUDIBLE_OK
        transcript: args.get(6).map(Into::into),
        ..Default::default()
    };
    let amp = 32767.0 * 10f64.powf(-20.0 / 20.0);
    let mut phase = 0.0f64;
    let step = 2.0 * std::f64::consts::PI * freq / 44_100.0;
    let session = Ap2Session::start(cfg, move |pcm: &mut [i16; SAMPLES_PER_PACKET]| {
        for frame in pcm.chunks_exact_mut(2) {
            let v = (amp * phase.sin()) as i16;
            frame[0] = v;
            frame[1] = v;
            phase += step;
        }
    })
    .await;
    let mut session = match session {
        Ok(s) => s,
        Err(e) => {
            eprintln!("AP2 session failed: {e}");
            std::process::exit(1);
        }
    };
    let stats = session.stats();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Err(e) = session.feedback().await {
            eprintln!("feedback failed: {e}");
            break;
        }
    }
    session.stop().await;
    use std::sync::atomic::Ordering::Relaxed;
    println!(
        "packets_sent={} timing_requests={} control_rx={} event_requests={}",
        stats.packets_sent.load(Relaxed),
        stats.timing_requests.load(Relaxed),
        stats.control_packets_received.load(Relaxed),
        stats.event_requests.load(Relaxed)
    );
}
