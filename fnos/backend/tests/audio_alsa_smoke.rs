//! Real-hardware smoke test (ignored by default: CI runners have no sound card).
//!
//! Run manually on a machine with ALSA devices:
//!
//! ```text
//! cargo test --manifest-path fnos/backend/Cargo.toml \
//!     --test audio_alsa_smoke -- --ignored --nocapture
//! ```
//!
//! It enumerates real playback PCMs with the same measured-capability probe used
//! by `--probe`, opens the first stereo device that supports 48 kHz, negotiates
//! the format (S16_LE preferred), writes 0.5 s of silence through the blocking
//! `writei` path and drains it. This validates the parts that the mock tests
//! cannot: real `HwParams` negotiation, read-back and `snd_pcm_writei`.
//! It is explicitly *not* a multi-device sync test (needs two physical clocks
//! and an audible/loopback measurement, which is still "待真机验证").

use fusionplay_fnos::audio::alsa::AlsaProvider;
use fusionplay_fnos::audio::device::DeviceProvider;

#[test]
#[ignore = "requires a real ALSA playback device"]
fn alsa_provider_opens_and_writes_to_a_real_device() {
    let provider = AlsaProvider::new();
    let devices = provider.enumerate();
    eprintln!("real playback devices: {devices:?}");
    let Some(info) = devices
        .iter()
        .find(|device| device.online && device.channels >= 2 && !device.sample_rates.is_empty())
    else {
        eprintln!("no stereo playback device available, nothing to do");
        return;
    };
    let rate = info
        .sample_rates
        .iter()
        .copied()
        .find(|rate| *rate == 48_000)
        .unwrap_or(info.sample_rates[0]);
    let mut opened = provider
        .open(&info.id, rate, 2)
        .unwrap_or_else(|error| panic!("open {} failed: {error:#}", info.id));
    assert_eq!(opened.channels, 2, "device negotiated a non-stereo format");
    eprintln!(
        "opened {} at {} Hz / 2 ch / {}",
        info.id,
        opened.rate,
        opened.format.name()
    );
    // Silence: the smoke test must not make noise on the host.
    let frames = (opened.rate as usize / 2).max(256); // 0.5 s
    let chunk = vec![0.0f32; frames * 2];
    opened.sink.write(&chunk).unwrap_or_else(|error| panic!("write failed: {error:#}"));
    opened.sink.drain();
    eprintln!("wrote {frames} frames of silence to {}", info.id);
}
