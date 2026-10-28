//! Unified audio engine interfaces.
//!
//! The protocol hosts (AirPlay+DLNA via `airplay-receiver-core`, MiPlay via
//! `fusionplay-miplay-sdk`) push decoded f32 PCM here. The engine applies the
//! sender-master gain chain and fans the master mix out to every enabled ALSA
//! output device (per-device ring buffer, resampling, drift compensation,
//! calibration delay). A `NullBackend` keeps CI and `--no-audio` usable.

pub mod alsa;
pub mod device;
pub mod engine;
pub mod resample;

pub use engine::MultiDeviceBackend;

use anyhow::Result;
use serde::Serialize;
use std::sync::{Arc, RwLock};

use crate::config::AudioSettings;
use crate::session::Source;

/// A probed hardware output device.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// Stable id, e.g. `hw:CARD=ultra,DEV=0`.
    pub id: String,
    pub name: String,
    pub card: String,
    pub device: u32,
    pub sample_rates: Vec<u32>,
    pub channels: u16,
    pub is_default: bool,
    pub online: bool,
}

/// Live per-device status.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub enabled: bool,
    pub gain_db: f32,
    pub delay_ms: f32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Current device ring-buffer fill in ms.
    pub buffer_ms: f32,
    /// Measured clock drift in ppm (positive = device runs fast).
    pub drift_ppm: f32,
    pub underruns: u64,
    pub written_frames: u64,
    /// `offline|starting|running|idle|error`
    pub state: String,
    /// Negotiated sample format (`S16_LE`, `S32_LE`, `F32_LE`), empty when offline.
    pub format: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineStatus {
    pub mode: String,
    pub source: Option<String>,
    pub master_frames: u64,
    pub dropped_frames: u64,
    pub devices: Vec<DeviceStatus>,
}

/// Output backend contract. Implementations must be lock-free on the audio
/// path (no blocking I/O in `write`): PCM is copied into per-device rings and
/// drained by dedicated device threads.
pub trait OutputBackend: Send + Sync {
    fn configure(&self, settings: &AudioSettings) -> Result<()>;
    fn devices(&self) -> Vec<DeviceInfo>;
    fn status(&self) -> EngineStatus;
    fn write(
        &self,
        source: Source,
        epoch: u64,
        sender_gain: f32,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
    );
    fn flush(&self, source: Source);
    fn test_tone(&self, device_id: &str) -> Result<()>;
}

pub struct AudioEngine {
    backend: Arc<dyn OutputBackend>,
    settings: RwLock<AudioSettings>,
}

impl AudioEngine {
    pub fn new(settings: AudioSettings, backend: Arc<dyn OutputBackend>) -> Result<Arc<Self>> {
        backend.configure(&settings)?;
        Ok(Arc::new(Self { backend, settings: RwLock::new(settings) }))
    }

    pub fn backend(&self) -> Arc<dyn OutputBackend> {
        Arc::clone(&self.backend)
    }

    pub fn configure(&self, settings: AudioSettings) -> Result<()> {
        settings.curve.validate().map_err(anyhow::Error::msg)?;
        self.backend.configure(&settings)?;
        *self.settings.write().unwrap() = settings;
        Ok(())
    }

    pub fn settings(&self) -> AudioSettings {
        self.settings.read().unwrap().clone()
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.backend.devices()
    }

    pub fn status(&self) -> EngineStatus {
        self.backend.status()
    }

    pub fn test_tone(&self, device_id: &str) -> Result<()> {
        self.backend.test_tone(device_id)
    }

    /// Entry point used by the protocol sink adapters.
    pub fn push_pcm(
        &self,
        source: Source,
        epoch: u64,
        sender_gain: f32,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
    ) {
        self.backend.write(source, epoch, sender_gain, sample_rate, channels, samples);
    }

    pub fn flush(&self, source: Source) {
        self.backend.flush(source);
    }
}

/// Backend that consumes PCM without touching hardware. Used by CI, `--no-audio`
/// and as a safe fallback when no ALSA device is available. It still counts
/// frames so tests can assert the fan-out path.
#[derive(Default)]
pub struct NullBackend {
    master_frames: RwLock<u64>,
    dropped_frames: RwLock<u64>,
    source: RwLock<Option<Source>>,
    configured: RwLock<AudioSettings>,
    writes: RwLock<u64>,
}

impl NullBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn writes(&self) -> u64 {
        *self.writes.read().unwrap()
    }

    pub fn master_frames(&self) -> u64 {
        *self.master_frames.read().unwrap()
    }
}

impl OutputBackend for NullBackend {
    fn configure(&self, settings: &AudioSettings) -> Result<()> {
        *self.configured.write().unwrap() = settings.clone();
        Ok(())
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        // Report configured devices as offline placeholders: never pretend a
        // hardware device exists when we cannot probe it.
        self.configured
            .read()
            .unwrap()
            .devices
            .iter()
            .map(|device| DeviceInfo {
                id: device.id.clone(),
                name: device.name.clone(),
                card: String::new(),
                device: 0,
                sample_rates: vec![],
                channels: 0,
                is_default: false,
                online: false,
            })
            .collect()
    }

    fn status(&self) -> EngineStatus {
        EngineStatus {
            mode: "null".to_string(),
            source: self.source.read().unwrap().map(|s| s.as_str().to_string()),
            master_frames: self.master_frames(),
            dropped_frames: *self.dropped_frames.read().unwrap(),
            devices: vec![],
        }
    }

    fn write(
        &self,
        source: Source,
        _epoch: u64,
        _sender_gain: f32,
        _sample_rate: u32,
        channels: u16,
        samples: &[f32],
    ) {
        if channels == 0 {
            *self.dropped_frames.write().unwrap() += 1;
            return;
        }
        *self.source.write().unwrap() = Some(source);
        *self.master_frames.write().unwrap() += (samples.len() / channels as usize) as u64;
        *self.writes.write().unwrap() += 1;
    }

    fn flush(&self, _source: Source) {}

    fn test_tone(&self, _device_id: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backend_counts_frames_and_sources() {
        let backend = NullBackend::new();
        let engine = AudioEngine::new(AudioSettings::default(), backend.clone()).unwrap();
        engine.push_pcm(Source::AirPlay, 1, 0.5, 48_000, 2, &[0.0; 96 * 2]);
        engine.push_pcm(Source::AirPlay, 1, 0.5, 48_000, 2, &[0.0; 96 * 2]);
        assert_eq!(backend.master_frames(), 192);
        assert_eq!(backend.writes(), 2);
        assert_eq!(engine.status().source.as_deref(), Some("airplay"));
    }
}
