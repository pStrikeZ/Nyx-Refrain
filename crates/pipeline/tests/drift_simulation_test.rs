//! Accelerated simulation test for clock drift compensation and latency stability.
//!
//! Simulates:
//! - Artificial +200 ppm and -200 ppm rate offsets.
//! - Virtual clock drive (no real-time sleeping).
//! - 2 hours (7200 simulated seconds) continuous operation.
//! - Asserts buffer level stability and that latency does not accumulate.
//!
//! Ignored by default: debug builds take about 5 minutes. Run with
//! `cargo test --release -p pipeline --test drift_simulation_test -- --ignored` (about 1.5).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use capture::AudioChunk;
use pipeline::metrics::PipelineMetricsTracker;
use pipeline::pipeline::{ALAC_FRAME_SAMPLES, ALAC_PCM_SAMPLES, PipelineProcessor};

#[test]
#[ignore = "simulates hours of streaming (minutes of CPU); run with --ignored"]
fn test_accelerated_drift_simulation_positive_200_ppm_2_hours() {
    run_drift_simulation(200.0, 7200);
}

#[test]
#[ignore = "simulates hours of streaming (minutes of CPU); run with --ignored"]
fn test_accelerated_drift_simulation_negative_200_ppm_2_hours() {
    run_drift_simulation(-200.0, 7200);
}

fn run_drift_simulation(rate_offset_ppm: f64, simulated_seconds: u64) {
    let source_rate: u32 = 48000;
    let chunk_frames: usize = 480; // 10 ms at 48000 Hz
    let metrics = Arc::new(PipelineMetricsTracker::new(64));
    let mut processor = PipelineProcessor::new(source_rate, metrics.clone(), None).unwrap();

    let start_instant = Instant::now();
    let mut virtual_time = start_instant;

    // Rate multiplier: +200 ppm means real clock runs faster -> chunks arrive more frequently
    // interval = nominal_interval / (1 + ppm * 1e-6)
    let rate_multiplier = 1.0 + rate_offset_ppm * 1e-6;
    let nominal_interval_secs = (chunk_frames as f64) / (source_rate as f64); // 0.010 s
    let actual_interval_secs = nominal_interval_secs / rate_multiplier;
    let actual_interval = Duration::from_secs_f64(actual_interval_secs);

    // Consumer (AirPlay DAC / sender) consumes 1 packet (352 frames @ 44.1 kHz)
    let consumer_packet_duration_secs = (ALAC_FRAME_SAMPLES as f64) / 44100.0; // ~0.00798186 s

    let mut virtual_buffer: VecDeque<[i16; ALAC_PCM_SAMPLES]> = VecDeque::with_capacity(64);
    let mut packets_out = Vec::with_capacity(8);

    // Initial pre-buffer: prime with 16 packets (~128 ms)
    for _ in 0..16 {
        virtual_buffer.push_back([0i16; ALAC_PCM_SAMPLES]);
    }

    let mut next_consumer_time_secs = 0.0;
    let mut elapsed_secs = 0.0;
    let mut total_chunks = 0u64;
    let total_chunks_target = (simulated_seconds as f64 / nominal_interval_secs) as u64;

    // We step through simulated time in chunks
    let mut dummy_pcm = vec![0.0f32; chunk_frames * 2];
    for i in 0..chunk_frames {
        let val = ((i as f32) * 0.05).sin() * 0.2;
        dummy_pcm[i * 2] = val;
        dummy_pcm[i * 2 + 1] = val;
    }

    let mut max_buffer_fill = virtual_buffer.len();
    let mut min_buffer_fill = virtual_buffer.len();

    while total_chunks < total_chunks_target {
        total_chunks += 1;
        virtual_time += actual_interval;
        elapsed_secs += actual_interval_secs;

        let chunk = AudioChunk::new(dummy_pcm.clone(), source_rate, 2, virtual_time);

        packets_out.clear();
        let current_fill = virtual_buffer.len();
        processor
            .process_chunk(&chunk, current_fill, &mut packets_out)
            .unwrap();

        for pkt in packets_out.drain(..) {
            virtual_buffer.push_back(pkt);
        }

        // Consumer pulls packets as virtual time elapses
        while elapsed_secs >= next_consumer_time_secs && !virtual_buffer.is_empty() {
            virtual_buffer.pop_front();
            next_consumer_time_secs += consumer_packet_duration_secs;
        }

        let fill = virtual_buffer.len();
        if fill > max_buffer_fill {
            max_buffer_fill = fill;
        }
        if fill < min_buffer_fill {
            min_buffer_fill = fill;
        }

        // Safety assertion: buffer must never blow up or completely empty after initial settling
        if elapsed_secs > 5.0 {
            assert!(
                fill > 0,
                "Buffer underrun (empty) at simulated t = {:.1} s with ppm = {}",
                elapsed_secs,
                rate_offset_ppm
            );
            assert!(
                fill < 50,
                "Buffer overrun (fill = {}) at simulated t = {:.1} s with ppm = {}",
                fill,
                elapsed_secs,
                rate_offset_ppm
            );
        }
    }

    let final_fill = virtual_buffer.len();
    let snap = metrics.snapshot();

    // Verify buffer stability over 2 hours
    assert!(
        (8..=28).contains(&final_fill),
        "Buffer fill not stable after 2h: final_fill={}, min={}, max={}",
        final_fill,
        min_buffer_fill,
        max_buffer_fill
    );

    // Verify estimated drift ppm converged close to injected rate offset
    assert!(
        (snap.estimated_drift_ppm - rate_offset_ppm).abs() < 25.0,
        "Estimated drift {:.2} ppm did not match injected {} ppm",
        snap.estimated_drift_ppm,
        rate_offset_ppm
    );
}

/// Live-source configuration (WASAPI): target fill 3 packets, sender trims backlog above 6.
/// With a correct drift estimate the ring must neither underrun nor need trimming.
fn run_live_simulation(rate_offset_ppm: f64, simulated_seconds: u64) -> (u64, u64) {
    let source_rate: u32 = 48000;
    let chunk_frames: usize = 480;
    let metrics = Arc::new(PipelineMetricsTracker::new(32));
    let mut processor = PipelineProcessor::new(source_rate, metrics.clone(), None).unwrap();
    processor.set_target_fill_level(pipeline::pipeline::LIVE_TARGET_FILL_PACKETS);

    let start = Instant::now();
    let rate_multiplier = 1.0 + rate_offset_ppm * 1e-6;
    let chunk_secs = chunk_frames as f64 / source_rate as f64 / rate_multiplier;
    let packet_secs = ALAC_FRAME_SAMPLES as f64 / 44100.0;
    let mut pcm = vec![0.0f32; chunk_frames * 2];
    for (i, s) in pcm.iter_mut().enumerate() {
        *s = ((i as f32) * 0.01).sin() * 0.2;
    }

    let mut ring: VecDeque<[i16; ALAC_PCM_SAMPLES]> = VecDeque::new();
    for _ in 0..3 {
        ring.push_back([0i16; ALAC_PCM_SAMPLES]);
    }
    let (mut underruns, mut trimmed) = (0u64, 0u64);
    let mut t_chunk = 0.0f64;
    let mut t_consume = 0.0f64;
    let mut out = Vec::new();
    while t_consume < simulated_seconds as f64 {
        if t_chunk <= t_consume {
            t_chunk += chunk_secs;
            let ts = start + Duration::from_secs_f64(t_chunk);
            out.clear();
            processor
                .process_chunk(
                    &AudioChunk::new(pcm.clone(), source_rate, 2, ts),
                    ring.len(),
                    &mut out,
                )
                .unwrap();
            ring.extend(out.drain(..));
        } else {
            t_consume += packet_secs;
            // Sender side (ap2::session::start_with_source): trim, then pop one packet.
            while ring.len() > 6 {
                ring.pop_front();
                if t_consume > 10.0 {
                    trimmed += 1;
                }
            }
            if ring.pop_front().is_none() && t_consume > 10.0 {
                underruns += 1;
            }
        }
    }
    (underruns, trimmed)
}

#[test]
#[ignore = "simulates hours of streaming (minutes of CPU); run with --ignored"]
fn test_live_source_drift_plus_120_ppm_1_hour() {
    let (underruns, trimmed) = run_live_simulation(120.0, 3600);
    assert_eq!(
        (underruns, trimmed),
        (0, 0),
        "underruns / trimmed packets after settling"
    );
}

#[test]
#[ignore = "simulates hours of streaming (minutes of CPU); run with --ignored"]
fn test_live_source_drift_minus_120_ppm_1_hour() {
    let (underruns, trimmed) = run_live_simulation(-120.0, 3600);
    assert_eq!(
        (underruns, trimmed),
        (0, 0),
        "underruns / trimmed packets after settling"
    );
}

#[test]
#[ignore = "simulates hours of streaming (minutes of CPU); run with --ignored"]
fn test_live_source_drift_zero_and_300_ppm() {
    for ppm in [0.0, 300.0, -300.0] {
        let (underruns, trimmed) = run_live_simulation(ppm, 1800);
        assert_eq!(
            (underruns, trimmed),
            (0, 0),
            "{ppm} ppm: underruns / trimmed"
        );
    }
}
