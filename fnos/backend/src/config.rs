//! Persistent JSON configuration for the fnOS host.
//!
//! Layout mirrors the Web UI: receiver identity, per-protocol switches,
//! audio output/calibration, logging. The file lives in `$TRIM_PKGHOME`
//! (`config.json`) and is written atomically.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::volume::VolumeCurve;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub receiver: ReceiverSettings,
    pub protocols: ProtocolSettings,
    pub audio: AudioSettings,
    pub logging: LoggingSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            receiver: ReceiverSettings::default(),
            protocols: ProtocolSettings::default(),
            audio: AudioSettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReceiverSettings {
    /// Name advertised over AirPlay / DLNA / MiPlay.
    pub name: String,
    /// Optional AirPlay pairing PIN. `None` = transient pairing (no PIN).
    pub pin: Option<String>,
    /// Network interface name, empty = auto.
    pub interface: String,
}

impl Default for ReceiverSettings {
    fn default() -> Self {
        Self { name: "FusionPlay".to_string(), pin: None, interface: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolSettings {
    pub airplay: ProtocolSwitch,
    pub dlna: ProtocolSwitch,
    pub miplay: ProtocolSwitch,
}

impl Default for ProtocolSettings {
    fn default() -> Self {
        Self {
            airplay: ProtocolSwitch { enabled: true },
            dlna: ProtocolSwitch { enabled: true },
            miplay: ProtocolSwitch { enabled: true },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolSwitch {
    pub enabled: bool,
}

impl Default for ProtocolSwitch {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Master mix rate. The engine runs at this rate and resamples per device.
    pub sample_rate: u32,
    /// Global calibration attenuation, 0–100 % (100 = no extra attenuation).
    pub global_attenuation_percent: f32,
    pub curve: VolumeCurve,
    pub muted: bool,
    pub devices: Vec<DeviceSetting>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            global_attenuation_percent: 100.0,
            curve: VolumeCurve::default(),
            muted: false,
            devices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSetting {
    /// Stable ALSA id, e.g. `hw:CARD=ultra,DEV=0`.
    pub id: String,
    /// Human label captured at selection time (informational only).
    pub name: String,
    pub enabled: bool,
    /// Per-device loudness balance, -30..=0 dB (0 = unity).
    pub gain_db: f32,
    /// Manual calibration delay, ms (>= 0).
    pub delay_ms: f32,
    /// Preferred device rate, 0 = device default.
    pub sample_rate: u32,
}

impl Default for DeviceSetting {
    fn default() -> Self {
        Self { id: String::new(), name: String::new(), enabled: false, gain_db: 0.0, delay_ms: 0.0, sample_rate: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingSettings {
    /// `error|warn|info|debug|trace`
    pub level: String,
    pub max_bytes: u64,
    pub keep_bytes: u64,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self { level: "info".to_string(), max_bytes: 5 * 1024 * 1024, keep_bytes: 1024 * 1024 }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = serde_json::from_str(&text)
                    .with_context(|| format!("无法解析配置 {}", path.display()))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                cfg.save(path)?;
                Ok(cfg)
            }
            Err(error) => Err(error).with_context(|| format!("无法读取配置 {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建配置目录 {}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text).with_context(|| format!("无法写临时配置 {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("无法落盘配置 {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.receiver.name.trim().is_empty() {
            anyhow::bail!("接收器名称不能为空");
        }
        if self.audio.sample_rate != 44_100 && self.audio.sample_rate != 48_000 {
            anyhow::bail!("音源采样率仅支持 44100/48000");
        }
        if !(0.0..=100.0).contains(&self.audio.global_attenuation_percent) {
            anyhow::bail!("全局衰减必须在 0–100%");
        }
        self.audio.curve.validate().map_err(anyhow::Error::msg)?;
        for device in &self.audio.devices {
            if !(-30.0..=0.0).contains(&device.gain_db) {
                anyhow::bail!("设备 {} 的响度平衡必须在 -30–0 dB", device.id);
            }
            if device.delay_ms < 0.0 || device.delay_ms > 2_000.0 {
                anyhow::bail!("设备 {} 的延迟补偿必须在 0–2000 ms", device.id);
            }
        }
        Ok(())
    }

    /// Replace everything except identity fields that must survive re-imports.
    pub fn apply_import(&mut self, imported: Config) -> Result<()> {
        imported.validate()?;
        self.protocols = imported.protocols;
        self.audio = imported.audio;
        self.logging = imported.logging;
        // Keep the local receiver identity name? No: config import is explicit.
        self.receiver = imported.receiver;
        Ok(())
    }
}

/// Default config path for the app: `$TRIM_PKGHOME/config.json`, else cwd.
pub fn default_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("TRIM_PKGHOME") {
        return PathBuf::from(home).join("config.json");
    }
    if let Some(data) = std::env::var_os("TRIM_PKGVAR") {
        return PathBuf::from(data).join("config.json");
    }
    PathBuf::from("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_atomic_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config::default();
        cfg.receiver.name = "测试接收器".into();
        cfg.audio.global_attenuation_percent = 80.0;
        cfg.audio.curve.kind = crate::volume::CurveKind::Custom;
        cfg.audio.curve.points = vec![(0.3, 0.2), (0.7, 0.6)];
        cfg.audio.devices.push(DeviceSetting {
            id: "hw:CARD=ultra,DEV=0".into(),
            name: "极境-ultra".into(),
            enabled: true,
            gain_db: -3.0,
            delay_ms: 12.0,
            sample_rate: 48_000,
        });
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.receiver.name, "测试接收器");
        assert!((loaded.audio.global_attenuation_percent - 80.0).abs() < 1e-6);
        assert_eq!(loaded.audio.devices.len(), 1);
        assert_eq!(loaded.audio.curve.points, vec![(0.3, 0.2), (0.7, 0.6)]);
        // Default file is created on first load.
        let missing = dir.path().join("new.json");
        let created = Config::load(&missing).unwrap();
        assert_eq!(created.receiver.name, "FusionPlay");
        assert!(missing.exists());
    }

    #[test]
    fn rejects_invalid_calibration() {
        let mut cfg = Config::default();
        cfg.audio.curve.kind = crate::volume::CurveKind::Custom;
        cfg.audio.curve.points = vec![(0.5, 0.6), (0.4, 0.2)];
        assert!(cfg.validate().is_err());
        cfg.audio.curve.points.clear();
        cfg.audio.devices.push(DeviceSetting { id: "x".into(), gain_db: 3.0, ..Default::default() });
        assert!(cfg.validate().is_err());
    }
}
