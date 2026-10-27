//! WASAPI audio capture backend for Windows.

pub mod com;
pub mod endpoint;
pub mod loopback;
pub mod notification;
pub mod process_loopback;
pub mod silence_stream;

pub use com::{ComMtaGuard, MmcssGuard};
pub use endpoint::{
    DefaultOutputGuard, DeviceInfo, DeviceSpec, EndpointMuteGuard, create_device_enumerator,
    get_default_render_device, get_device_friendly_name, get_device_id, list_render_endpoints,
    open_render_device,
};
pub use loopback::{WasapiLoopbackConfig, WasapiLoopbackSource};
pub use notification::{DeviceNotificationClient, NotificationRegistration};
pub use process_loopback::{ProcessLoopbackConfig, ProcessLoopbackMode, ProcessLoopbackSource};
pub use silence_stream::SilenceKeepAliveStream;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Writes as many whole frames of `samples` as fit into the capture ring in one block and
/// returns the number of samples dropped. Per-sample pushes could split a frame when the ring
/// filled, shifting every later frame by one channel.
pub(crate) fn push_frames(
    producer: &mut rtrb::Producer<f32>,
    samples: &[f32],
    channels: usize,
) -> usize {
    let channels = channels.max(1);
    let n = samples.len().min(producer.slots()) / channels * channels;
    if n > 0
        && let Ok(chunk) = producer.write_chunk_uninit(n)
    {
        chunk.fill_from_iter(samples[..n].iter().copied());
    }
    samples.len() - n
}

/// Appends up to `max_samples` queued samples, whole frames only, to `out` in one block.
pub(crate) fn pop_frames(
    consumer: &mut rtrb::Consumer<f32>,
    max_samples: usize,
    channels: usize,
    out: &mut Vec<f32>,
) {
    let channels = channels.max(1);
    let n = consumer.slots().min(max_samples) / channels * channels;
    if n > 0
        && let Ok(chunk) = consumer.read_chunk(n)
    {
        let (a, b) = chunk.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        chunk.commit_all();
    }
}

/// Test and verify WASAPI / COM initialization and linking.
pub fn init_wasapi() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx failed: {e}"))?;
        CoUninitialize();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_moves_whole_frames_only() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(5);
        // Room for 5 samples but only 2 stereo frames: the half frame is dropped.
        assert_eq!(
            push_frames(&mut producer, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2),
            2
        );
        let mut out = Vec::new();
        // Asking for 3 samples returns one whole frame.
        pop_frames(&mut consumer, 3, 2, &mut out);
        assert_eq!(out, [1.0, 2.0]);
        pop_frames(&mut consumer, 8, 2, &mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
    }
}
