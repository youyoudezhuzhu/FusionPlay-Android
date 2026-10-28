//! Real ALSA device provider + capability probing.
//!
//! Probing is deliberately *measured*, never assumed:
//! - `/proc/asound/{cards,pcm}` provides the stable id (`hw:CARD=<cardid>,DEV=<n>`)
//!   and human name of every playback PCM;
//! - each candidate rate is negotiated with `set_rate_near` and then read back
//!   with `get_rate()` after `hw_params`; it is only reported as supported when
//!   the actual rate differs from the candidate by at most 1 Hz;
//! - channels are verified the same way;
//! - every probe opens and closes its own PCM and returns after closing it.
//!
//! `AlsaProvider` caches verified capabilities: while our own worker holds a
//! device open, a second probe would fail with EBUSY, so the cache keeps
//! `/api/devices` truthful during playback. A device that disappears is
//! dropped from the cache and re-probed on hot plug.
//!
//! Opening for playback negotiates S16_LE first, then S32_LE, then F32_LE, and
//! reads the actual rate/channels back. The blocking `writei` in
//! [`AlsaSink`] is the device clock for that device's worker thread.

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use super::device::{f32_to_i16, f32_to_i32, DeviceProvider, DeviceSink, OpenedDevice, SampleFormat};
use super::DeviceInfo;

/// Candidate rates probed against the hardware. Only read-back matches are
/// advertised.
const CANDIDATE_RATES: [u32; 4] = [44_100, 48_000, 96_000, 192_000];
/// Candidate channel counts verified for each playback PCM.
const CANDIDATE_CHANNELS: [u32; 2] = [2, 1];

/// Parse `/proc/asound/cards` into `card index -> (card id, card name)`.
fn scan_cards() -> HashMap<u32, (String, String)> {
    let mut cards = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/proc/asound/cards") else {
        return cards;
    };
    for line in text.lines() {
        // " 0 [PCH   ]: HDA-Intel - HDA Intel PCH"
        let Some((index, rest)) = line.trim().split_once('[') else { continue };
        let Ok(card_index) = index.trim().parse::<u32>() else { continue };
        let Some((card_id, tail)) = rest.split_once(']') else { continue };
        let name = tail.trim_start_matches(':').trim();
        cards.insert(card_index, (card_id.trim().to_string(), name.to_string()));
    }
    cards
}

/// Parse `/proc/asound/pcm` into playback device descriptors.
fn scan_playback_devices() -> Vec<(String, String, String, u32)> {
    let cards = scan_cards();
    let mut devices = Vec::new();
    let Ok(text) = std::fs::read_to_string("/proc/asound/pcm") else {
        return devices;
    };
    for line in text.lines() {
        // "00-00: ALC269VC Analog : playback 1 : capture 1"
        if !line.contains("playback") {
            continue;
        }
        let Some((pcm_id, rest)) = line.split_once(':') else { continue };
        let Some((card_num, dev_num)) = pcm_id.trim().split_once('-') else { continue };
        let Ok(card_index) = card_num.trim().parse::<u32>() else { continue };
        let Ok(device) = dev_num.trim().parse::<u32>() else { continue };
        let Some((card_id, card_name)) = cards.get(&card_index) else { continue };
        let label = rest
            .split(" : playback")
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_end_matches(':')
            .trim()
            .to_string();
        let id = format!("hw:CARD={card_id},DEV={device}");
        let name = format!("{label} ({card_name})");
        devices.push((id, name, card_id.clone(), device));
    }
    devices
}

/// Measured capabilities of one PCM.
#[derive(Debug, Clone)]
struct Capabilities {
    rates: Vec<u32>,
    channels: u16,
    online: bool,
}

impl Capabilities {
    fn offline() -> Self {
        Self { rates: Vec::new(), channels: 0, online: false }
    }
}

fn to_alsa(format: SampleFormat) -> Format {
    match format {
        SampleFormat::S16Le => Format::s16(),
        SampleFormat::S32Le => Format::s32(),
        SampleFormat::F32Le => Format::float(),
    }
}

/// Try to negotiate `channels` / `rate` / `format` on a *fresh* PCM and read
/// the actual values back. The PCM is closed before this function returns.
fn probe_one(id: &str, channels: u32, rate: u32, format: SampleFormat) -> Option<(u32, u32)> {
    let pcm = PCM::new(id, Direction::Playback, false).ok()?;
    let hwp = HwParams::any(&pcm).ok()?;
    hwp.set_channels(channels).ok()?;
    hwp.set_access(Access::RWInterleaved).ok()?;
    hwp.set_format(to_alsa(format)).ok()?;
    hwp.set_rate_near(rate, ValueOr::Nearest).ok()?;
    if pcm.hw_params(&hwp).is_err() {
        return None;
    }
    let actual_rate = hwp.get_rate().ok()?;
    let actual_channels = hwp.get_channels().ok()?;
    // `pcm` is dropped here (closed) before returning.
    Some((actual_rate, actual_channels))
}

/// Measure a device's real capabilities. Each candidate is negotiated and then
/// read back; a candidate counts as supported only when the read-back value is
/// within ±1 Hz of the candidate (and, for channels, exactly the candidate).
fn probe_capabilities(id: &str) -> Capabilities {
    // First make sure the PCM can be opened at all; this is also the "online"
    // signal. The handle is closed immediately.
    if PCM::new(id, Direction::Playback, false).is_err() {
        return Capabilities::offline();
    }

    let mut rates = Vec::new();
    for candidate in CANDIDATE_RATES {
        if let Some((actual_rate, actual_channels)) =
            probe_one(id, 2, candidate, SampleFormat::S16Le)
        {
            if (actual_rate as i64 - candidate as i64).abs() <= 1
                && actual_channels == 2
                && !rates.contains(&actual_rate)
            {
                rates.push(actual_rate);
            }
        }
    }
    rates.sort_unstable();

    let mut channels = 0u16;
    for candidate in CANDIDATE_CHANNELS {
        if let Some((_, actual_channels)) = probe_one(id, candidate, 48_000, SampleFormat::S16Le) {
            if actual_channels == candidate {
                channels = channels.max(candidate as u16);
            }
        }
    }

    Capabilities { rates, channels, online: true }
}

/// Enumerate playback PCMs and probe each one's real capabilities.
///
/// This is the function behind `fusionplay-fnos --probe`; it measures the
/// hardware instead of advertising a fixed list.
pub fn probe_devices() -> Vec<DeviceInfo> {
    scan_playback_devices()
        .into_iter()
        .map(|(id, name, card, device)| {
            let capabilities = probe_capabilities(&id);
            let online = capabilities.online && capabilities.channels >= 2;
            DeviceInfo {
                id,
                name,
                card,
                device,
                sample_rates: capabilities.rates,
                channels: capabilities.channels,
                is_default: false,
                online,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CachedCapabilities {
    rates: Vec<u32>,
    channels: u16,
    online: bool,
}

/// ALSA implementation of [`DeviceProvider`].
pub struct AlsaProvider {
    cache: Mutex<HashMap<String, CachedCapabilities>>,
}

impl AlsaProvider {
    pub fn new() -> Self {
        Self { cache: Mutex::new(HashMap::new()) }
    }
}

impl Default for AlsaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProvider for AlsaProvider {
    fn enumerate(&self) -> Vec<DeviceInfo> {
        let present = scan_playback_devices();
        let mut cache = match self.cache.lock() {
            Ok(guard) => guard,
            Err(_) => return Vec::new(),
        };
        let present_ids: HashSet<&str> = present.iter().map(|(id, _, _, _)| id.as_str()).collect();
        // Forget capabilities of unplugged devices so a re-plug is re-probed.
        cache.retain(|id, _| present_ids.contains(id.as_str()));
        present
            .into_iter()
            .map(|(id, name, card, device)| {
                let cached = cache.get(&id).cloned();
                let cached = match cached {
                    Some(cached) if cached.online => cached,
                    _ => {
                        let capabilities = probe_capabilities(&id);
                        let entry = CachedCapabilities {
                            rates: capabilities.rates.clone(),
                            channels: capabilities.channels,
                            online: capabilities.online,
                        };
                        if capabilities.online {
                            cache.insert(id.clone(), entry.clone());
                        }
                        entry
                    }
                };
                DeviceInfo {
                    id,
                    name,
                    card,
                    device,
                    sample_rates: cached.rates,
                    channels: cached.channels,
                    is_default: false,
                    online: cached.online && cached.channels >= 2,
                }
            })
            .collect()
    }

    fn open(&self, id: &str, wanted_rate: u32, wanted_channels: u16) -> Result<OpenedDevice> {
        open_alsa(id, wanted_rate, wanted_channels)
    }
}

/// Blocking `writei` sink. Format conversion happens here, once per batch.
struct AlsaSink {
    pcm: PCM,
    format: SampleFormat,
    channels: usize,
    i16_buffer: Vec<i16>,
    i32_buffer: Vec<i32>,
}

impl AlsaSink {
    fn write_i16(&mut self, samples: &[f32]) -> Result<()> {
        f32_to_i16(samples, &mut self.i16_buffer);
        let io = self.pcm.io_i16().context("无法获取 S16 输出通道")?;
        write_all(&self.pcm, &io, &self.i16_buffer, self.channels)
    }

    fn write_i32(&mut self, samples: &[f32]) -> Result<()> {
        f32_to_i32(samples, &mut self.i32_buffer);
        let io = self.pcm.io_i32().context("无法获取 S32 输出通道")?;
        write_all(&self.pcm, &io, &self.i32_buffer, self.channels)
    }
}

fn write_all<S: Copy>(
    pcm: &PCM,
    io: &alsa::pcm::IO<S>,
    buffer: &[S],
    channels: usize,
) -> Result<()> {
    let mut offset = 0usize;
    while offset < buffer.len() {
        match io.writei(&buffer[offset..]) {
            Ok(0) => break,
            Ok(frames) => offset += frames * channels,
            Err(error) => {
                // Underrun / suspend recovery: retry once the PCM is in a
                // workable state, otherwise report the device as failed.
                if pcm.try_recover(error, true).is_ok() {
                    continue;
                }
                return Err(error).context("ALSA writei 失败");
            }
        }
    }
    Ok(())
}

impl DeviceSink for AlsaSink {
    fn format(&self) -> SampleFormat {
        self.format
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        match self.format {
            SampleFormat::S16Le => self.write_i16(samples),
            SampleFormat::S32Le => self.write_i32(samples),
            SampleFormat::F32Le => {
                let io = self.pcm.io_f32().context("无法获取 F32 输出通道")?;
                write_all(&self.pcm, &io, samples, self.channels)
            }
        }
    }

    fn drain(&mut self) {
        let _ = self.pcm.drain();
    }
}

/// Open a playback device, negotiating S16_LE first, then S32_LE, then F32_LE.
/// The actual rate and channel count are read back from the hardware.
fn open_alsa(id: &str, wanted_rate: u32, wanted_channels: u16) -> Result<OpenedDevice> {
    let wanted_rate = wanted_rate.max(1);
    let wanted_channels = wanted_channels.max(1);
    let mut last_error: Option<anyhow::Error> = None;
    for format in [SampleFormat::S16Le, SampleFormat::S32Le, SampleFormat::F32Le] {
        let pcm = match PCM::new(id, Direction::Playback, false) {
            Ok(pcm) => pcm,
            Err(error) => {
                return Err(error).with_context(|| format!("打开 ALSA 设备 {id} 失败"));
            }
        };
        let hwp = match HwParams::any(&pcm) {
            Ok(hwp) => hwp,
            Err(error) => {
                last_error = Some(error.into());
                continue;
            }
        };
        if let Err(error) = hwp.set_channels(wanted_channels as u32) {
            last_error = Some(error.into());
            continue;
        }
        if let Err(error) = hwp.set_access(Access::RWInterleaved) {
            last_error = Some(error.into());
            continue;
        }
        if let Err(error) = hwp.set_format(to_alsa(format)) {
            last_error = Some(error.into());
            continue;
        }
        if let Err(error) = hwp.set_rate_near(wanted_rate, ValueOr::Nearest) {
            last_error = Some(error.into());
            continue;
        }
        // Best effort buffering: ~200 ms, 4 periods. Some devices reject these
        // hints; the defaults then still work, so failures are ignored.
        let _ = hwp.set_periods(4, ValueOr::Nearest);
        let _ = hwp.set_buffer_size_near((wanted_rate as i64 * 200 / 1000).max(512));
        if let Err(error) = pcm.hw_params(&hwp) {
            last_error = Some(error.into());
            continue;
        }
        let actual_rate = hwp.get_rate().unwrap_or(wanted_rate);
        let actual_channels = hwp.get_channels().unwrap_or(wanted_channels as u32) as u16;
        if actual_channels != wanted_channels {
            last_error = Some(anyhow!(
                "设备 {id} 协商到 {actual_channels} 声道（期望 {wanted_channels}），跳过 {format:?}"
            ));
            continue;
        }
        let _ = pcm.prepare();
        drop(hwp);
        eprintln!(
            "[audio] 设备 {id} 已打开：{actual_rate} Hz / {actual_channels} ch / {}",
            format.name()
        );
        return Ok(OpenedDevice {
            sink: Box::new(AlsaSink {
                pcm,
                format,
                channels: actual_channels as usize,
                i16_buffer: Vec::new(),
                i32_buffer: Vec::new(),
            }),
            rate: actual_rate,
            channels: actual_channels,
            format,
        });
    }
    Err(last_error.unwrap_or_else(|| anyhow!("设备 {id} 无法协商任何输出格式")))
}

/// Backwards-compatible name for the ALSA-backed multi-device engine.
///
/// `main.rs` and the HTTP API construct it through `AlsaBackend::new(...)`;
/// everything else lives in [`super::engine::MultiDeviceBackend`].
pub type AlsaBackend = super::engine::MultiDeviceBackend;

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

/// Human readable summary for `--probe`.
pub fn probe_summary() -> Vec<String> {
    probe_devices()
        .into_iter()
        .map(|device| {
            format!(
                "{} | {} | rates={:?} ch={} online={}",
                device.id, device.name, device.sample_rates, device.channels, device.online
            )
        })
        .collect()
}

/// Wait helper used by diagnostics/tests: is any playback PCM visible?
pub fn has_playback_devices() -> bool {
    !scan_playback_devices().is_empty()
}

/// Optional settle time used by tests that simulate hot plug timing.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(50);
