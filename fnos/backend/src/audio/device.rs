//! Device abstraction used by the multi-device output engine.
//!
//! The engine is deliberately independent of ALSA: it only knows about
//! [`DeviceProvider`] (enumeration + opening) and [`DeviceSink`] (a blocking,
//! device-clocked writer). This keeps the hot plug / drift / calibration logic
//! unit-testable on machines without a sound card (CI) while `alsa.rs` provides
//! the real hardware implementation.
//!
//! Contract:
//! - [`DeviceProvider::enumerate`] must only report devices it can really see.
//!   `sample_rates`/`channels` must be *measured* capabilities, never a
//!   hard-coded list of guesses.
//! - [`DeviceSink::write`] is the device clock: it blocks for roughly the
//!   duration of the PCM it was handed. Implementations must return promptly
//!   after an error (the worker thread then marks the device offline and lets
//!   the watchdog retry).

use anyhow::Result;

use super::DeviceInfo;

/// Sample format negotiated with a device. `S16_LE` is tried first, then
/// `S32_LE`, then `F32_LE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    S16Le,
    S32Le,
    F32Le,
}

impl SampleFormat {
    pub fn name(self) -> &'static str {
        match self {
            SampleFormat::S16Le => "S16_LE",
            SampleFormat::S32Le => "S32_LE",
            SampleFormat::F32Le => "F32_LE",
        }
    }
}

impl Default for SampleFormat {
    fn default() -> Self {
        SampleFormat::S16Le
    }
}

/// Blocking PCM writer owned by one device thread.
pub trait DeviceSink: Send {
    fn format(&self) -> SampleFormat;

    /// Write interleaved stereo f32 samples (amplitude already calibrated).
    /// Blocks in the device for about `frames / device_rate` seconds.
    fn write(&mut self, samples: &[f32]) -> Result<()>;

    /// Drain the device (best effort) when the worker exits.
    fn drain(&mut self);
}

/// One successfully opened device.
pub struct OpenedDevice {
    pub sink: Box<dyn DeviceSink>,
    /// Rate actually negotiated (read back from the hardware).
    pub rate: u32,
    /// Channels actually negotiated.
    pub channels: u16,
    pub format: SampleFormat,
}

/// Enumerates and opens output devices. Implemented for ALSA (`AlsaProvider`)
/// and by the mock provider used in `tests/audio_engine_mock.rs`.
pub trait DeviceProvider: Send + Sync {
    /// Currently visible playback devices with *measured* capabilities.
    fn enumerate(&self) -> Vec<DeviceInfo>;

    /// Open `id` at `wanted_rate` / `wanted_channels` and negotiate a format
    /// (S16_LE preferred). Returns the actual negotiated parameters.
    fn open(&self, id: &str, wanted_rate: u32, wanted_channels: u16) -> Result<OpenedDevice>;
}

/// Convert f32 samples to S16_LE. `out` is reused between writes.
pub fn f32_to_i16(samples: &[f32], out: &mut Vec<i16>) {
    out.clear();
    out.reserve(samples.len());
    out.extend(samples.iter().map(|sample| {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).clamp(-32768.0, 32767.0);
        value as i16
    }));
}

/// Convert f32 samples to S32_LE (full 32-bit scale).
pub fn f32_to_i32(samples: &[f32], out: &mut Vec<i32>) {
    out.clear();
    out.reserve(samples.len());
    out.extend(samples.iter().map(|sample| {
        let value = (sample.clamp(-1.0, 1.0) * 2_147_483_647.0)
            .clamp(-2_147_483_648.0, 2_147_483_647.0);
        value as i32
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_convert_without_overflow() {
        let mut i16 = Vec::new();
        f32_to_i16(&[-2.0, -1.0, 0.0, 1.0, 2.0], &mut i16);
        assert_eq!(i16, vec![-32767, -32767, 0, 32767, 32767]);

        let mut i32 = Vec::new();
        f32_to_i32(&[-2.0, 0.0, 2.0], &mut i32);
        assert_eq!(i32[1], 0);
        assert!(i32[0] >= i32::MIN && i32[2] <= i32::MAX);
        assert_eq!(i32[0], i32::MIN);
    }
}
