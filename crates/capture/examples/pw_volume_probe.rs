//! PipeWire virtual sink volume probe and verification utility.
//!
//! Creates a virtual `Audio/Sink` ("nyx_refrain_airplay"), registers a volume
//! change callback, and processes commands via stdin or command-line arguments.
//!
//! Commands accepted on stdin:
//! - `set <pct>`: Push volume percent to sink (0.0..100.0)
//! - `mute <0|1>`: Push mute state to sink
//! - `rms`: Print latest audio capture RMS
//! - `events`: Print total count of volume change events received
//! - `quit`: Exit cleanly

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::{BufRead, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::time::Duration;

    use capture::AudioSource;
    use capture::pipewire::{PipeWireSinkConfig, PipeWireSinkSource, SinkVolume};

    let args: Vec<String> = std::env::args().collect();
    let set_default = args.iter().any(|a| a == "--set-default");

    let initial_pct = args
        .iter()
        .position(|a| a == "--initial-vol")
        .and_then(|idx| args.get(idx + 1))
        .and_then(|v| v.parse::<f32>().ok());

    let initial_volume = initial_pct.map(|pct| SinkVolume::new(pct, pct <= 0.0));

    let config = PipeWireSinkConfig {
        node_name: "nyx_refrain_airplay".to_string(),
        node_description: "Nyx Refrain (AirPlay)".to_string(),
        set_default,
        initial_volume,
    };

    println!(
        "PROBE: Creating virtual sink (set_default={})...",
        set_default
    );
    let mut source = PipeWireSinkSource::new(config)?;
    let ctrl = source.volume_control();

    let event_counter = Arc::new(AtomicUsize::new(0));
    let last_pct = Arc::new(AtomicU32::new(initial_pct.unwrap_or(100.0).to_bits()));
    let last_mute = Arc::new(AtomicBool::new(false));

    let ec = event_counter.clone();
    let lp = last_pct.clone();
    let lm = last_mute.clone();
    ctrl.on_change(move |v| {
        let count = ec.fetch_add(1, Ordering::SeqCst) + 1;
        lp.store(v.pct.to_bits(), Ordering::SeqCst);
        lm.store(v.mute, Ordering::SeqCst);
        println!("EVENT #{}: pct={:.2} mute={}", count, v.pct, v.mute);
        let _ = std::io::stdout().flush();
    });

    source.start()?;
    println!("PROBE: Sink ready.");
    let _ = std::io::stdout().flush();

    let latest_rms = Arc::new(AtomicU32::new(0));
    let running = Arc::new(AtomicBool::new(true));

    let running_rx = running.clone();
    let latest_rms_tx = latest_rms.clone();

    // Background thread capturing audio frames and measuring RMS
    let capture_handle = std::thread::spawn(move || {
        while running_rx.load(Ordering::Relaxed) {
            match source.read_chunk(1024) {
                Ok(Some(chunk)) if !chunk.data.is_empty() => {
                    let sum_sq: f32 = chunk.data.iter().map(|&s| s * s).sum();
                    let rms = (sum_sq / chunk.data.len() as f32).sqrt();
                    if rms > 0.001 {
                        latest_rms_tx.store(rms.to_bits(), Ordering::Relaxed);
                    }
                }
                Ok(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    break;
                }
            }
        }
    });

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts[0] {
            "set" => {
                if let Some(pct_str) = parts.get(1) {
                    if let Ok(pct) = pct_str.parse::<f32>() {
                        let mute = parts
                            .get(2)
                            .map(|&m| m == "mute" || m == "1")
                            .unwrap_or(false);
                        ctrl.set(pct, mute);
                        println!("OK: set pct={:.2} mute={}", pct, mute);
                    } else {
                        println!("ERR: invalid percentage");
                    }
                } else {
                    println!("ERR: set requires percentage argument");
                }
            }
            "mute" => {
                if let Some(m_str) = parts.get(1) {
                    let mute = *m_str == "1" || *m_str == "true";
                    let cur_pct = f32::from_bits(last_pct.load(Ordering::SeqCst));
                    ctrl.set(cur_pct, mute);
                    println!("OK: mute={}", mute);
                } else {
                    println!("ERR: mute requires 0 or 1");
                }
            }
            "rms" => {
                let rms = f32::from_bits(latest_rms.load(Ordering::Relaxed));
                println!("RMS: {:.6}", rms);
            }
            "rms-reset" => {
                latest_rms.store(0, Ordering::Relaxed);
                println!("OK: rms reset");
            }
            "events" => {
                let cnt = event_counter.load(Ordering::SeqCst);
                let cur_pct = f32::from_bits(last_pct.load(Ordering::SeqCst));
                let cur_mute = last_mute.load(Ordering::SeqCst);
                println!(
                    "EVENTS: count={} last_pct={:.2} last_mute={}",
                    cnt, cur_pct, cur_mute
                );
            }
            "quit" | "exit" => {
                println!("PROBE: Quitting...");
                break;
            }
            other => {
                println!("ERR: unknown command '{}'", other);
            }
        }
        let _ = std::io::stdout().flush();
    }

    running.store(false, Ordering::Relaxed);
    let _ = capture_handle.join();
    println!("PROBE: Exited cleanly.");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("pw_volume_probe is only supported on Linux");
}
