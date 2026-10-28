//! ALSA output backend.
//!
//! Real hardware probing (never name-only): `/proc/asound` enumerates cards and
//! playback PCMs, and each candidate is opened through libasound to read its
//! actual negotiable format. Each enabled device then gets its own worker
//! thread with a bounded ring buffer; blocking `writei` is the device clock.
//! The master `write()` never performs I/O: it only copies into rings.

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use super::resample::{DriftEstimator, LinearResampler};
use super::{
    AudioSettings, DeviceInfo, DeviceStatus, EngineStatus, OutputBackend,
};
use crate::config::DeviceSetting;
use crate::session::Source;
use crate::volume::{delay_ms_to_frames, device_db_to_gain, GainChain};

const DEVICE_RING_MS: usize = 300;
const MASTER_CHANNELS: usize = 2;

/// Parse `/proc/asound/cards` + `/proc/asound/pcm` into stable candidates.
pub fn probe_devices() -> Vec<DeviceInfo> {
    let mut cards = std::collections::HashMap::<String, (String, String)>::new();
    if let Ok(text) = std::fs::read_to_string("/proc/asound/cards") {
        for line in text.lines() {
            // " 0 [PCH   ]: HDA-Intel - HDA Intel PCH"
            let Some((idx, rest)) = line.trim().split_once('[') else { continue };
            let card_num = idx.trim();
            let Ok(card_index) = card_num.parse::<u32>() else { continue };
            let Some((card_id, name)) = rest.split_once(']') else { continue };
            let name = name.trim_start_matches(':').trim();
            // /proc/asound/cards uses "0", /proc/asound/pcm uses "00": normalise.
            cards.insert(card_index.to_string(), (card_id.trim().to_string(), name.to_string()));
        }
    }
    let mut devices = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/proc/asound/pcm") {
        for line in text.lines() {
            // "00-00: ALC269VC Analog : playback 1 : capture 1"
            if !line.contains("playback") {
                continue;
            }
            let Some((pcm_id, rest)) = line.split_once(':') else { continue };
            let pcm_id = pcm_id.trim();
            let Some((card_num, dev_num)) = pcm_id.split_once('-') else { continue };
            let Ok(card_index) = card_num.trim().parse::<u32>() else { continue };
            let card_num = card_index.to_string();
            let label = rest
                .split(" : playback")
                .next()
                .unwrap_or(rest)
                .trim()
                .trim_end_matches(':')
                .trim()
                .to_string();
            let Some((card_id, card_name)) = cards.get(&card_num) else { continue };
            let device: u32 = match dev_num.trim().parse() {
                Ok(value) => value,
                Err(_) => continue,
            };
            let id = format!("hw:CARD={card_id},DEV={device}");
            let (sample_rates, channels, online) = probe_capabilities(&id);
            devices.push(DeviceInfo {
                id,
                name: format!("{} ({})", label, card_name),
                card: card_id.clone(),
                device,
                sample_rates,
                channels,
                is_default: false,
                online,
            });
        }
    }
    devices
}

/// Open the PCM and read back the actual negotiated format. Returns
/// `(rates, channels, online)`; offline is not hidden from the UI.
fn probe_capabilities(id: &str) -> (Vec<u32>, u16, bool) {
    let Ok(pcm) = PCM::new(id, Direction::Playback, false) else {
        return (vec![], 0, false);
    };
    let mut rates = Vec::new();
    let mut channels = 0;
    for rate in [44_100u32, 48_000, 96_000, 192_000] {
        let Ok(hwp) = HwParams::any(&pcm) else { continue };
        if hwp.set_channels(2).is_err()
            || hwp.set_access(Access::RWInterleaved).is_err()
            || hwp.set_format(Format::s16()).is_err()
            || hwp.set_rate_near(rate, ValueOr::Nearest).is_err()
        {
            continue;
        }
        if pcm.hw_params(&hwp).is_err() {
            continue;
        }
        rates.push(hwp.get_rate().unwrap_or(rate));
        channels = channels.max(hwp.get_channels().unwrap_or(2));
    }
    rates.sort_unstable();
    rates.dedup();
    (rates, channels.max(2) as u16, true)
}

/// One enabled device: bounded ring + worker thread.
struct DeviceWorker {
    setting: DeviceSetting,
    info: DeviceInfo,
    ring: Arc<Mutex<VecDeque<f32>>>,
    control: Arc<DeviceControl>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    written: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    drift_ppm: Arc<Mutex<f32>>,
}

struct DeviceControl {
    gain: Mutex<f32>,
    target_rate: Mutex<u32>,
}

impl DeviceWorker {
    fn ring_capacity_frames(rate: u32) -> usize {
        (rate as usize * DEVICE_RING_MS / 1000).max(256)
    }
}

pub struct AlsaBackend {
    settings: RwLock<AudioSettings>,
    workers: Mutex<Vec<DeviceWorker>>,
    master_frames: AtomicU64,
    dropped_frames: AtomicU64,
    source: RwLock<Option<Source>>,
    /// Last accepted lease; set by the host through `set_lease`.
    lease_ok: Mutex<Option<(Source, u64)>>,
}

impl AlsaBackend {
    pub fn new(settings: &AudioSettings) -> Result<Arc<Self>> {
        let backend = Arc::new(Self {
            settings: RwLock::new(settings.clone()),
            workers: Mutex::new(Vec::new()),
            master_frames: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            source: RwLock::new(None),
            lease_ok: Mutex::new(None),
        });
        backend.rebuild_workers()?;
        Ok(backend)
    }

    /// Recreate device workers from the current settings (hotplug / config).
    fn rebuild_workers(&self) -> Result<()> {
        let settings = self.settings.read().unwrap().clone();
        let available = probe_devices();
        let mut workers = self.workers.lock().unwrap();
        for worker in workers.iter_mut() {
            worker.stop.store(true, Ordering::Release);
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
        workers.clear();
        for setting in settings.devices.iter().filter(|device| device.enabled) {
            let Some(info) = available.iter().find(|info| info.id == setting.id).cloned() else {
                // Keep an explicit offline worker so the UI shows the truth.
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
                workers.push(DeviceWorker {
                    setting: setting.clone(),
                    info: DeviceInfo {
                        id: setting.id.clone(),
                        name: setting.name.clone(),
                        card: String::new(),
                        device: 0,
                        sample_rates: vec![],
                        channels: 0,
                        is_default: false,
                        online: false,
                    },
                    ring: Arc::new(Mutex::new(VecDeque::new())),
                    control: Arc::new(DeviceControl {
                        gain: Mutex::new(1.0),
                        target_rate: Mutex::new(0),
                    }),
                    stop,
                    handle: None,
                    written: Arc::new(AtomicU64::new(0)),
                    underruns: Arc::new(AtomicU64::new(0)),
                    drift_ppm: Arc::new(Mutex::new(0.0)),
                });
                continue;
            };
            let rate = if setting.sample_rate != 0 {
                setting.sample_rate
            } else {
                info.sample_rates.iter().copied().find(|rate| *rate == 48_000).unwrap_or(48_000)
            };
            let gain = device_db_to_gain(setting.gain_db);
            let control = Arc::new(DeviceControl {
                gain: Mutex::new(gain),
                target_rate: Mutex::new(rate),
            });
            let ring = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
                DeviceWorker::ring_capacity_frames(rate) * MASTER_CHANNELS,
            )));
            // Calibration delay: pre-fill the ring with silence.
            let delay_frames = delay_ms_to_frames(setting.delay_ms, rate);
            if delay_frames > 0 {
                let mut guard = ring.lock().unwrap();
                for _ in 0..delay_frames * MASTER_CHANNELS {
                    guard.push_back(0.0);
                }
            }
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let written = Arc::new(AtomicU64::new(0));
            let underruns = Arc::new(AtomicU64::new(0));
            let drift_ppm = Arc::new(Mutex::new(0.0));
            let handle = {
                let id = setting.id.clone();
                let ring = Arc::clone(&ring);
                let stop = Arc::clone(&stop);
                let written = Arc::clone(&written);
                let underruns = Arc::clone(&underruns);
                let drift_ppm = Arc::clone(&drift_ppm);
                let control = Arc::clone(&control);
                thread::Builder::new()
                    .name(format!("alsa-{}", setting.id))
                    .spawn(move || {
                        if let Err(error) = device_thread(
                            &id, &ring, &stop, &written, &underruns, &drift_ppm, &control, rate,
                        ) {
                            eprintln!("[audio] 设备 {id} 输出线程退出：{error:#}");
                        }
                    })
                    .context("无法创建设备输出线程")?
            };
            workers.push(DeviceWorker {
                setting: setting.clone(),
                info,
                ring,
                control,
                stop,
                handle: Some(handle),
                written,
                underruns,
                drift_ppm,
            });
        }
        Ok(())
    }
}

fn device_thread(
    id: &str,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    stop: &Arc<std::sync::atomic::AtomicBool>,
    written: &Arc<AtomicU64>,
    underruns: &Arc<AtomicU64>,
    drift_ppm: &Arc<Mutex<f32>>,
    control: &Arc<DeviceControl>,
    initial_rate: u32,
) -> Result<()> {
    let pcm = PCM::new(id, Direction::Playback, false).with_context(|| format!("打开 {id} 失败"))?;
    let rate = {
        let mut hwp = HwParams::any(&pcm)?;
        hwp.set_channels(MASTER_CHANNELS as u32)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(Format::s16())?;
        hwp.set_rate_near(initial_rate, ValueOr::Nearest)?;
        pcm.hw_params(&hwp)?;
        hwp.get_rate().unwrap_or(initial_rate)
    };
    let mut io = pcm.io_i16()?;
    let mut resampler = LinearResampler::new(MASTER_CHANNELS);
    let target_fill = (rate as usize * 120 / 1000).max(256) as f32;
    let mut drift = DriftEstimator::new(target_fill as usize, 0.02, 500.0);
    let mut scratch: Vec<f32> = Vec::with_capacity(4096);
    let mut scratch_i16: Vec<i16> = Vec::with_capacity(4096);
    let mut last = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let gain = *control.gain.lock().unwrap();
        let mut guard = ring.lock().unwrap();
        let available = guard.len();
        let take = available.min(2048 * MASTER_CHANNELS);
        scratch.clear();
        scratch.extend(guard.drain(..take));
        drop(guard);
        if scratch.is_empty() {
            // Track underruns: a device that has been starving for a while is
            // reported instead of silently glitching.
            if last.elapsed() > Duration::from_millis(200) {
                underruns.fetch_add(1, Ordering::Relaxed);
                last = Instant::now();
            }
            thread::sleep(Duration::from_millis(3));
            continue;
        }
        last = Instant::now();
        let fill = ring.lock().unwrap().len() as f32 / MASTER_CHANNELS as f32;
        let ppm = drift.update(fill);
        *drift_ppm.lock().unwrap() = ppm;
        let ratio = if (rate as f64 - 0.0).abs() > 0.0 { drift.ratio() } else { 1.0 };
        let resampled = resampler.process(&scratch, ratio);
        scratch_i16.clear();
        scratch_i16.extend(resampled.iter().map(|sample| {
            let value = (sample * gain * 32767.0).clamp(-32768.0, 32767.0);
            value as i16
        }));
        if !scratch_i16.is_empty() {
            let frames = scratch_i16.len() / MASTER_CHANNELS;
            let _ = io.writei(&scratch_i16[..frames * MASTER_CHANNELS]);
            written.fetch_add(frames as u64, Ordering::Relaxed);
        }
    }
    let _ = pcm.drain();
    Ok(())
}

impl OutputBackend for AlsaBackend {
    fn configure(&self, settings: &AudioSettings) -> Result<()> {
        *self.settings.write().unwrap() = settings.clone();
        self.rebuild_workers()
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        probe_devices()
    }

    fn status(&self) -> EngineStatus {
        let workers = self.workers.lock().unwrap();
        let devices = workers
            .iter()
            .map(|worker| {
                let setting = &worker.setting;
                let rate = *worker.control.target_rate.lock().unwrap();
                let fill = worker.ring.lock().unwrap().len() / MASTER_CHANNELS;
                DeviceStatus {
                    id: setting.id.clone(),
                    name: worker.info.name.clone(),
                    online: worker.info.online,
                    enabled: setting.enabled,
                    gain_db: setting.gain_db,
                    delay_ms: setting.delay_ms,
                    sample_rate: rate,
                    channels: MASTER_CHANNELS as u16,
                    buffer_ms: (fill as f32) * 1000.0 / rate.max(1) as f32,
                    drift_ppm: *worker.drift_ppm.lock().unwrap(),
                    underruns: worker.underruns.load(Ordering::Relaxed),
                    written_frames: worker.written.load(Ordering::Relaxed),
                    state: if worker.info.online { "running".into() } else { "offline".into() },
                }
            })
            .collect();
        EngineStatus {
            mode: "alsa".to_string(),
            source: self.source.read().unwrap().map(|s| s.as_str().to_string()),
            master_frames: self.master_frames.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            devices,
        }
    }

    fn write(
        &self,
        source: Source,
        epoch: u64,
        sender_gain: f32,
        _sample_rate: u32,
        channels: u16,
        samples: &[f32],
    ) {
        if channels as usize != MASTER_CHANNELS || samples.is_empty() {
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Ok(guard) = self.lease_ok.lock() {
            if let Some((lease_source, lease_epoch)) = *guard {
                if lease_source != source || lease_epoch != epoch {
                    self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        *self.source.write().unwrap() = Some(source);
        let settings = self.settings.read().unwrap().clone();
        let chain = GainChain {
            global_attenuation_percent: settings.global_attenuation_percent,
            curve: settings.curve.clone(),
            muted: settings.muted,
        };
        let master_gain = chain.apply(sender_gain);
        self.master_frames.fetch_add((samples.len() / MASTER_CHANNELS) as u64, Ordering::Relaxed);
        let workers = self.workers.lock().unwrap();
        for worker in workers.iter() {
            if !worker.info.online || !worker.setting.enabled {
                continue;
            }
            let capacity = DeviceWorker::ring_capacity_frames(*worker.control.target_rate.lock().unwrap()) * MASTER_CHANNELS;
            let mut ring = worker.ring.lock().unwrap();
            for sample in samples {
                ring.push_back(sample * master_gain);
            }
            while ring.len() > capacity {
                ring.pop_front();
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn flush(&self, _source: Source) {
        let workers = self.workers.lock().unwrap();
        for worker in workers.iter() {
            worker.ring.lock().unwrap().clear();
        }
    }

    fn test_tone(&self, device_id: &str) -> Result<()> {
        let workers = self.workers.lock().unwrap();
        let worker = workers
            .iter()
            .find(|worker| worker.setting.id == device_id)
            .with_context(|| format!("设备未启用或不存在：{device_id}"))?;
        let rate = *worker.control.target_rate.lock().unwrap();
        let frames = rate as usize; // 1 s
        let mut ring = worker.ring.lock().unwrap();
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let sample = (t * 1000.0 * std::f32::consts::TAU).sin() * 0.2;
            ring.push_back(sample);
            ring.push_back(sample);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlsaDiagnostic {
    pub cards: String,
    pub pcm: String,
}

/// Raw ALSA diagnostics for the Web UI.
pub fn diagnostics() -> AlsaDiagnostic {
    AlsaDiagnostic {
        cards: std::fs::read_to_string("/proc/asound/cards").unwrap_or_default(),
        pcm: std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_panics_and_returns_stable_ids() {
        let devices = probe_devices();
        for device in &devices {
            assert!(device.id.starts_with("hw:CARD="), "unexpected id {}", device.id);
            assert!(device.id.contains(",DEV="));
        }
    }
}
