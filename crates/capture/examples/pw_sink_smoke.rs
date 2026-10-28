//! PipeWire virtual sink capture smoke test on Linux.
//!
//! Creates a virtual `Audio/Sink` ("Nyx Refrain (AirPlay)"), sets it as default audio sink,
//! captures incoming audio, prints RMS and Peak volume levels every 100ms, and on exit
//! destroys the sink and restores the previous default audio sink. `PW_SINK_NAME=<name>`
//! creates a separately named, non-default sink instead.

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use capture::AudioSource;
    use capture::pipewire::{PipeWireSinkConfig, PipeWireSinkSource};
    use std::time::{Duration, Instant};

    tracing_subscriber::fmt::init();

    let duration_secs = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(10.0);

    println!("=== PipeWire Virtual Sink Smoke Test ===");
    println!("Creating virtual sink and setting as default audio sink...");

    // PW_SINK_NAME=<name> creates a separately named sink that is not made the default
    // (for testing next to a running Nyx Refrain).
    let custom_name = std::env::var("PW_SINK_NAME").ok();
    let config = PipeWireSinkConfig {
        node_name: custom_name
            .clone()
            .unwrap_or_else(|| "nyx_refrain_airplay".to_string()),
        node_description: custom_name
            .clone()
            .unwrap_or_else(|| "Nyx Refrain (AirPlay)".to_string()),
        set_default: custom_name.is_none(),
        initial_volume: None,
    };

    let mut source = PipeWireSinkSource::new(config)?;
    source.start()?;

    let rate = source.sample_rate();
    let channels = source.channels();
    println!("Virtual sink created successfully!");
    println!("Rate: {} Hz, Channels: {}", rate, channels);
    println!(
        "Capturing for {:.1} seconds (pass duration as argv[1] to change)...",
        duration_secs
    );
    println!(
        "Play audio using `pw-play <file>` or select 'Nyx Refrain (AirPlay)' in sound settings.\n"
    );

    let start_time = Instant::now();
    let total_duration = Duration::from_secs_f64(duration_secs);

    while start_time.elapsed() < total_duration {
        let interval_start = Instant::now();
        let mut interval_samples = Vec::new();

        // Read all available chunks in this 100ms window
        while interval_start.elapsed() < Duration::from_millis(100) {
            match source.read_chunk(480)? {
                Some(chunk) => {
                    interval_samples.extend_from_slice(&chunk.data);
                }
                None => {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        let n = interval_samples.len();
        let (rms, peak) = if n > 0 {
            let mut sum_sq = 0.0f64;
            let mut max_abs = 0.0f32;
            for &s in &interval_samples {
                let abs_s = s.abs();
                if abs_s > max_abs {
                    max_abs = abs_s;
                }
                sum_sq += (s as f64) * (s as f64);
            }
            let rms_val = (sum_sq / n as f64).sqrt() as f32;
            (rms_val, max_abs)
        } else {
            (0.0f32, 0.0f32)
        };

        let rms_db = if rms > 1e-6 {
            20.0 * rms.log10()
        } else {
            -96.0
        };
        let peak_db = if peak > 1e-6 {
            20.0 * peak.log10()
        } else {
            -96.0
        };

        let elapsed = start_time.elapsed().as_secs_f64();
        let frames = n / (channels as usize);
        println!(
            "[{:5.1}s] frames: {:5}, RMS: {:6.4} ({:5.1} dBFS), Peak: {:6.4} ({:5.1} dBFS)",
            elapsed, frames, rms, rms_db, peak, peak_db
        );
    }

    println!("\nTest duration finished. Stopping source...");
    source.stop()?;
    println!("Virtual sink stopped. Default audio sink restored cleanly.");

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("pw_sink_smoke is only supported on Linux.");
}
