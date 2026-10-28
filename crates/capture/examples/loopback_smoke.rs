//! Loopback capture smoke test under Windows/Wine.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use capture::AudioSource;
    use capture::wasapi::{WasapiLoopbackConfig, WasapiLoopbackSource};
    use std::time::Duration;

    println!("Creating WasapiLoopbackSource...");
    let config = WasapiLoopbackConfig::default();
    let mut source = match WasapiLoopbackSource::with_config(config) {
        Ok(s) => s,
        Err(e) => {
            println!("WasapiLoopbackSource initialization result: {e}");
            return Ok(());
        }
    };

    println!("Device: {}", source.device_info().friendly_name);
    println!(
        "Format: {} Hz, {} channels",
        source.sample_rate(),
        source.channels()
    );

    println!("Starting capture...");
    source.start()?;

    std::thread::sleep(Duration::from_millis(100));

    println!("Reading chunk...");
    let chunk = source.read_chunk(352)?;
    if let Some(c) = chunk {
        println!(
            "Read chunk: {} frames, duration {:?}",
            c.frame_count(),
            c.duration()
        );
    } else {
        println!("No chunk returned");
    }

    source.stop()?;
    println!("Stopped cleanly!");
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
