//! Three-protocol hosts for the fnOS receiver.
//!
//! * AirPlay 2 + DLNA reuse `airplay-receiver-core` in-process. AirPlay PCM is
//!   forwarded through the core `PcmOutput` hook into the unified engine; DLNA
//!   media URLs arrive as `CoreEvent::DlnaMedia` and are fetched/decoded by
//!   [`crate::dlna_player`].
//! * MiPlay uses `fusionplay-miplay-sdk` with the process-wide PCM sink.
//!
//! Cross-protocol arbitration is owned here (`crate::session::Arbiter`): one
//! primary source at a time, the previous source is suspended (its protocol
//! session stays alive) and queued PCM is flushed.

use anyhow::{Context, Result};
use serde_json::Value;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fusionplay_core::{
    CoreEvent, EventSink, MediaSource, PcmOutput, PcmSource, PlaybackArbiter, Settings,
    run_receiver_with_arbiter,
};
use fusionplay_miplay_sdk::{
    EventEmitter, MiPlayPcmOutput, MiPlayReceiver, ReceiverConfig, ReceiverIdentity,
    ReceiverController,
};

use crate::audio::AudioEngine;
use crate::config::Config;
use crate::dlna_player::DlnaPlayer;
use crate::session::{Arbiter, Lease, Source, SuspendHook};
use crate::state::SharedState;

/// Shared PCM sink: claims the source on first block and gates PCM to the
/// engine through the arbiter.
pub struct EngineSink {
    engine: Arc<AudioEngine>,
    arbiter: Arc<Arbiter>,
    state: SharedState,
    lease: Mutex<Option<Lease>>,
    hooks: Mutex<std::collections::HashMap<Source, SuspendHook>>,
}

impl EngineSink {
    pub fn new(engine: Arc<AudioEngine>, arbiter: Arc<Arbiter>, state: SharedState) -> Arc<Self> {
        Arc::new(Self {
            engine,
            arbiter,
            state,
            lease: Mutex::new(None),
            hooks: Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn set_hook(&self, source: Source, hook: SuspendHook) {
        self.hooks.lock().unwrap().insert(source, hook);
    }

    fn hook(&self, source: Source) -> Option<SuspendHook> {
        self.hooks.lock().unwrap().get(&source).cloned()
    }

    fn claim(&self, source: Source) -> Lease {
        let mut lease = self.lease.lock().unwrap();
        if let Some(current) = *lease {
            if current.source == source {
                return current;
            }
        }
        let claim = self.arbiter.claim(source, "pcm");
        *lease = Some(claim.lease);
        let suspended = claim.suspended;
        drop(lease);
        if let Some(previous) = suspended {
            tracing::info!(
                "音源接管：{} -> {}",
                previous.source.as_str(),
                source.as_str()
            );
            if let Some(hook) = self.hook(previous.source) {
                hook(previous);
            }
        }
        self.state.set_current_source(Some(source));
        claim.lease
    }

    fn release(&self, source: Source) {
        let mut lease = self.lease.lock().unwrap();
        if let Some(current) = *lease {
            if current.source == source {
                self.engine.flush(source);
                self.arbiter.release(current);
                *lease = None;
                self.state.set_current_source(None);
            }
        }
    }
}

impl PcmOutput for EngineSink {
    fn write(
        &self,
        source: PcmSource,
        _epoch: u64,
        sender_gain: f32,
        sample_rate: u32,
        channels: u16,
        samples: &[f32],
    ) {
        let source = match source {
            PcmSource::AirPlay => Source::AirPlay,
            PcmSource::Dlna => Source::Dlna,
            PcmSource::MiPlay => Source::MiPlay,
        };
        let lease = self.claim(source);
        if !self.arbiter.accepts(lease) {
            return;
        }
        self.engine.push_pcm(
            source,
            lease.epoch,
            sender_gain.clamp(0.0, 1.0),
            sample_rate,
            channels,
            samples,
        );
        {
            let mut state = self.state.state.write().unwrap();
            match source {
                Source::AirPlay => {
                    state.sender_volume.airplay_percent = Some((sender_gain.clamp(0.0, 1.0) * 100.0).round() as u8);
                }
                Source::Dlna => {
                    state.sender_volume.dlna_percent = Some((sender_gain.clamp(0.0, 1.0) * 100.0).round() as u8);
                }
                Source::MiPlay => {
                    state.sender_volume.miplay_percent = Some((sender_gain.clamp(0.0, 1.0) * 100.0).round() as u8);
                }
            }
        }
    }

    fn flush(&self, source: PcmSource) {
        let source = match source {
            PcmSource::AirPlay => Source::AirPlay,
            PcmSource::Dlna => Source::Dlna,
            PcmSource::MiPlay => Source::MiPlay,
        };
        self.release(source);
    }
}

impl MiPlayPcmOutput for EngineSink {
    fn write(&self, sender_gain: f32, sample_rate: u32, channels: u16, samples: &[f32]) {
        <Self as PcmOutput>::write(
            self,
            PcmSource::MiPlay,
            0,
            sender_gain,
            sample_rate,
            channels,
            samples,
        );
    }

    fn flush(&self) {
        <Self as PcmOutput>::flush(self, PcmSource::MiPlay);
    }
}

struct CoreHandle {
    commands: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    thread: Option<JoinHandle<()>>,
}

struct MiPlayHandle {
    receiver: MiPlayReceiver,
    controller: ReceiverController,
}

/// Owns the three protocol hosts and applies config restarts.
pub struct ProtocolHosts {
    engine: Arc<AudioEngine>,
    state: SharedState,
    arbiter: Arc<Arbiter>,
    sink: Arc<EngineSink>,
    dlna: Arc<DlnaPlayer>,
    core: Mutex<Option<CoreHandle>>,
    miplay: Mutex<Option<MiPlayHandle>>,
    shutdown: Arc<AtomicBool>,
}

impl ProtocolHosts {
    pub fn start(config: &Config, engine: Arc<AudioEngine>, state: SharedState) -> Result<Arc<Self>> {
        let arbiter = Arc::new(Arbiter::new());
        let sink = EngineSink::new(Arc::clone(&engine), Arc::clone(&arbiter), Arc::clone(&state));
        let dlna = DlnaPlayer::new(Arc::clone(&engine), Arc::clone(&arbiter), Arc::clone(&state));
        sink.set_hook(
            Source::Dlna,
            Arc::new({
                let dlna = Arc::clone(&dlna);
                move |_lease| dlna.stop()
            }),
        );
        let hosts = Arc::new(Self {
            engine,
            state,
            arbiter,
            sink,
            dlna,
            core: Mutex::new(None),
            miplay: Mutex::new(None),
            shutdown: Arc::new(AtomicBool::new(false)),
        });
        hosts.apply(config)?;
        Ok(hosts)
    }

    fn apply(&self, config: &Config) -> Result<()> {
        self.stop_hosts();
        let state_dir = state_dir();
        self.state.state.write().unwrap().airplay.enabled = config.protocols.airplay.enabled;
        self.state.state.write().unwrap().dlna.enabled = config.protocols.dlna.enabled;
        self.state.state.write().unwrap().miplay.enabled = config.protocols.miplay.enabled;

        if config.protocols.airplay.enabled || config.protocols.dlna.enabled {
            self.start_core(config, &state_dir)?;
        }
        if config.protocols.miplay.enabled {
            if let Err(error) = self.start_miplay(config, &state_dir) {
                let message = format!("小米妙播启动失败：{error:#}");
                tracing::error!("{message}");
                self.state.record_error(message);
                let mut state = self.state.state.write().unwrap();
                state.miplay.state = "error".into();
                state.miplay.detail = "无法启动妙播接收（详见日志）".into();
            }
        } else {
            fusionplay_miplay_sdk::set_process_pcm_output(None);
            let mut state = self.state.state.write().unwrap();
            state.miplay.state = "disabled".into();
        }
        Ok(())
    }

    pub fn restart(&self, config: &Config) -> Result<()> {
        self.apply(config)
    }

    fn start_core(&self, config: &Config, state_dir: &Path) -> Result<()> {
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::clone(&self.state);
        let dlna = Arc::clone(&self.dlna);
        let events = Arc::new(EventSink::with_callback(Arc::new(move |json: String| {
            handle_core_event(json, Arc::clone(&state), Arc::clone(&dlna));
        })));
        let core_arbiter = Arc::new(PlaybackArbiter::new(Arc::clone(&events)));
        let sink: Arc<dyn PcmOutput> = Arc::clone(&self.sink) as Arc<dyn PcmOutput>;
        let settings = Settings {
            name: config.receiver.name.clone(),
            pin: config.receiver.pin.clone(),
            state_path: state_dir.join("airplay_state.json"),
            output_device: None,
            airplay_enabled: config.protocols.airplay.enabled,
            dlna_enabled: config.protocols.dlna.enabled,
            audio_output: Some(sink),
        };
        let state_for_thread = Arc::clone(&self.state);
        let thread = std::thread::Builder::new()
            .name("fusionplay-core".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        state_for_thread.record_error(format!("无法创建核心运行时：{error}"));
                        return;
                    }
                };
                runtime.block_on(async move {
                    if let Err(error) =
                        run_receiver_with_arbiter(settings, events, receiver, core_arbiter).await
                    {
                        tracing::error!("核心接收器退出：{error:#}");
                        state_for_thread.record_error(format!("接收器退出：{error:#}"));
                    }
                });
            })
            .context("无法启动核心接收线程")?;
        *self.core.lock().unwrap() = Some(CoreHandle {
            commands: Some(commands),
            thread: Some(thread),
        });
        Ok(())
    }

    fn start_miplay(&self, config: &Config, state_dir: &Path) -> Result<()> {
        let (interface, ip, mac) = pick_interface(&config.receiver.interface)?;
        let identity = load_identity(state_dir)?;
        let state = Arc::clone(&self.state);
        let sink = Arc::clone(&self.sink);
        let events: EventEmitter = Arc::new(move |value: Value| {
            handle_miplay_event(value, Arc::clone(&state), Arc::clone(&sink));
        });
        fusionplay_miplay_sdk::set_process_pcm_output(Some(
            Arc::clone(&self.sink) as Arc<dyn MiPlayPcmOutput>
        ));
        let receiver_config = ReceiverConfig::new(
            config.receiver.name.clone(),
            identity,
            ip,
            interface,
        )
        .with_initial_volume_percent(50)
        .with_hardware_address(Some(mac));
        let receiver = MiPlayReceiver::start(receiver_config, events)
            .context("启动小米妙播接收器失败（发现/鉴权/端口）")?;
        let controller = receiver.controller();
        {
            let mut state = self.state.state.write().unwrap();
            state.miplay.state = "running".into();
            state.miplay.detail = format!("广播名称 {}", config.receiver.name);
        }
        *self.miplay.lock().unwrap() = Some(MiPlayHandle { receiver, controller });
        Ok(())
    }

    fn stop_hosts(&self) {
        if let Some(mut handle) = self.miplay.lock().unwrap().take() {
            handle.controller.stop();
            drop(handle.receiver);
            // Give media workers a moment to observe shutdown before the next
            // session (config restart) re-claims discovery sockets.
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        fusionplay_miplay_sdk::set_process_pcm_output(None);
        if let Some(mut handle) = self.core.lock().unwrap().take() {
            if let Some(commands) = handle.commands.take() {
                let _ = commands.send(r#"{"command":"shutdown"}"#.to_owned());
            }
            if let Some(thread) = handle.thread.take() {
                let _ = thread.join();
            }
        }
        self.dlna.stop();
        self.sink.release(Source::AirPlay);
        self.sink.release(Source::Dlna);
        self.sink.release(Source::MiPlay);
    }

    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.stop_hosts();
    }

    pub fn dlna(&self) -> Arc<DlnaPlayer> {
        Arc::clone(&self.dlna)
    }

    pub fn arbiter(&self) -> Arc<Arbiter> {
        Arc::clone(&self.arbiter)
    }
}

fn state_dir() -> PathBuf {
    std::env::var_os("FUSIONPLAY_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TRIM_PKGVAR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn pick_interface(requested: &str) -> Result<(String, std::net::Ipv4Addr, String)> {
    let interfaces = if_addrs::get_if_addrs().context("无法枚举网络接口")?;
    let mut candidates: Vec<_> = interfaces
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) => Some((interface.name, ip)),
            IpAddr::V6(_) => None,
        })
        .collect();
    if !requested.trim().is_empty() {
        candidates.retain(|(name, _)| name == requested.trim());
    }
    let (name, ip) = candidates
        .into_iter()
        .next()
        .with_context(|| format!("没有可用的 IPv4 网络接口（请求：{requested}）"))?;
    let mac = std::fs::read_to_string(format!("/sys/class/net/{name}/address"))
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    Ok((name, ip, mac))
}

fn load_identity(state_dir: &Path) -> Result<ReceiverIdentity> {
    let path = state_dir.join("miplay_identity.json");
    #[derive(serde::Serialize, serde::Deserialize)]
    struct StoredIdentity {
        idm_instance_id: String,
        lyra_instance_id: String,
        media_device_id: String,
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(stored) = serde_json::from_str::<StoredIdentity>(&text) {
            return Ok(ReceiverIdentity::new(
                stored.idm_instance_id,
                stored.lyra_instance_id,
                stored.media_device_id,
            ));
        }
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_nanos(), std::process::id()).hash(&mut hasher);
    let base = hasher.finish();
    let idm = format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (base >> 32) as u32,
        (base >> 16) as u16,
        (base & 0xfff) as u16,
        ((base >> 12) & 0xfff) as u16,
        (base & 0xffffffffffff) as u64
    );
    let lyra = format!("{:08X}", (base >> 8) as u32);
    let identity = StoredIdentity {
        idm_instance_id: idm,
        lyra_instance_id: lyra,
        media_device_id: "fusionplay-nas".to_string(),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&identity)?);
    Ok(ReceiverIdentity::new(
        identity.idm_instance_id,
        identity.lyra_instance_id,
        identity.media_device_id,
    ))
}

fn handle_core_event(json: String, state: SharedState, dlna: Arc<DlnaPlayer>) {
    let Ok(value) = serde_json::from_str::<Value>(&json) else {
        return;
    };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(|v| v.to_string());
    match kind {
        "receiver_ready" => {
            let mut runtime = state.state.write().unwrap();
            runtime.airplay.state = "listening".into();
            runtime.airplay.port = value.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
            runtime.airplay.detail = format!(
                "名称 {} · 设备 {}",
                text("name").unwrap_or_default(),
                text("device_id").unwrap_or_default()
            );
            runtime.dlna.state = if value.get("port").and_then(Value::as_u64).unwrap_or(0) > 0 {
                "listening".into()
            } else {
                "idle".into()
            };
        }
        "status" => {
            let mut runtime = state.state.write().unwrap();
            if let Some(value) = text("state") {
                runtime.airplay.state = value;
            }
            if let Some(message) = text("message") {
                runtime.airplay.detail = message;
            }
        }
        "now_playing" => {
            let mut runtime = state.state.write().unwrap();
            runtime.media.title = text("title");
            runtime.media.artist = text("artist");
            runtime.media.album = text("album");
            runtime.media.cover_url = text("cover_url");
        }
        "volume" => {
            let mut runtime = state.state.write().unwrap();
            if value.get("source").and_then(Value::as_str) == Some("dlna") {
                runtime.sender_volume.dlna_percent =
                    value.get("percent").and_then(Value::as_u64).map(|v| v as u8);
            } else {
                runtime.sender_volume.airplay_db =
                    value.get("db").and_then(Value::as_f64).map(|v| v as f32);
                runtime.sender_volume.airplay_percent =
                    value.get("percent").and_then(Value::as_u64).map(|v| v as u8);
            }
        }
        "stream_started" => {
            let mut runtime = state.state.write().unwrap();
            let source = text("source").unwrap_or_default();
            if source == "dlna" {
                runtime.dlna.state = "playing".into();
            } else {
                runtime.airplay.state = "streaming".into();
            }
        }
        "stream_stopped" => {
            let mut runtime = state.state.write().unwrap();
            if text("source").as_deref() == Some("dlna") {
                runtime.dlna.state = "idle".into();
            } else {
                runtime.airplay.state = "listening".into();
            }
        }
        "client_connected" => {
            state.state.write().unwrap().airplay.clients += 1;
        }
        "client_disconnected" => {
            let mut runtime = state.state.write().unwrap();
            runtime.airplay.clients = runtime.airplay.clients.saturating_sub(1);
        }
        "dlna_ready" => {
            let mut runtime = state.state.write().unwrap();
            runtime.dlna.state = "listening".into();
            runtime.dlna.port = value.get("port").and_then(Value::as_u64).unwrap_or(0) as u16;
            runtime.dlna.detail = text("friendly_name").unwrap_or_default();
        }
        "dlna_media" => {
            let Some(url) = text("url") else { return };
            {
                let mut runtime = state.state.write().unwrap();
                runtime.dlna.state = "loading".into();
                runtime.dlna.detail = format!("拉取媒体：{url}");
                runtime.media.title = text("title");
                runtime.media.artist = text("artist");
                runtime.media.album = text("album");
                runtime.media.cover_url = text("artwork_url");
                runtime.media.duration_ms = value.get("duration_ms").and_then(Value::as_u64);
                runtime.media.position_ms = value.get("start_position_ms").and_then(Value::as_u64);
            }
            let state = Arc::clone(&state);
            std::thread::Builder::new()
                .name("dlna-media".into())
                .spawn(move || {
                    if let Err(error) = dlna.play_uri(&url) {
                        let message = format!("DLNA 播放失败：{error:#}");
                        tracing::error!("{message}");
                        state.record_error(message);
                        let mut runtime = state.state.write().unwrap();
                        runtime.dlna.state = "error".into();
                    }
                })
                .ok();
        }
        "dlna_volume" => {
            let mut runtime = state.state.write().unwrap();
            runtime.sender_volume.dlna_percent =
                value.get("percent").and_then(Value::as_u64).map(|v| v as u8);
        }
        "dlna_stop" => {
            dlna.stop();
            let mut runtime = state.state.write().unwrap();
            runtime.dlna.state = "listening".into();
        }
        "dlna_seek" => {
            let mut runtime = state.state.write().unwrap();
            let position = value.get("position_ms").and_then(Value::as_u64);
            runtime.media.position_ms = position;
            runtime.dlna.detail = "DLNA Seek 暂未支持（首版），播放位置由控制端显示".into();
        }
        "error" => {
            let message = text("message").unwrap_or_else(|| "未知错误".into());
            state.record_error(message);
        }
        "source_takeover" => {
            let mut runtime = state.state.write().unwrap();
            runtime.current_source = text("source");
        }
        "log" => {
            if let Some(message) = text("message") {
                tracing::info!("core: {message}");
            }
        }
        _ => {}
    }
}

fn handle_miplay_event(value: Value, state: SharedState, sink: Arc<EngineSink>) {
    let kind = value.get("event").and_then(Value::as_str).unwrap_or_default();
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(|v| v.to_string());
    match kind {
        "receiver_ready" | "discovery_backend_ready" => {
            let mut runtime = state.state.write().unwrap();
            runtime.miplay.state = "listening".into();
            runtime.miplay.detail = format!(
                "{}:{} · {}",
                text("address").unwrap_or_default(),
                value.get("port").and_then(Value::as_u64).unwrap_or(0),
                text("name").unwrap_or_default()
            );
        }
        "stream_started" => {
            let mut runtime = state.state.write().unwrap();
            runtime.miplay.state = "streaming".into();
            runtime.current_source = Some("miplay".into());
            runtime.media.title = text("title");
            runtime.media.artist = text("artist");
            runtime.media.album = text("album");
            runtime.miplay.detail = format!(
                "{} Hz · {} 声道",
                value.get("sample_rate").and_then(Value::as_u64).unwrap_or(0),
                value.get("channels").and_then(Value::as_u64).unwrap_or(0)
            );
        }
        "stream_stopped" | "media_channel_closed" | "media_session_replaced" => {
            let mut runtime = state.state.write().unwrap();
            runtime.miplay.state = "listening".into();
            runtime.miplay.detail = "等待妙播投放".into();
            if runtime.current_source.as_deref() == Some("miplay") {
                runtime.current_source = None;
            }
            sink.release(Source::MiPlay);
        }
        "playback_state" => {
            if let Some(playing) = value.get("playing").and_then(Value::as_bool) {
                let mut runtime = state.state.write().unwrap();
                runtime.miplay.state = if playing { "streaming".into() } else { "paused".into() };
            }
        }
        "audio_output_ready" => {
            let mut runtime = state.state.write().unwrap();
            runtime.miplay.detail = format!(
                "统一音频引擎 · {}",
                text("device_name").unwrap_or_default()
            );
        }
        "audio_format" => {
            let mut runtime = state.state.write().unwrap();
            if let Some(codec) = text("source_codec") {
                runtime.media.detail_codec = Some(codec);
            }
        }
        "error" => {
            let message = text("message").unwrap_or_else(|| "妙播错误".into());
            state.record_error(message);
            let mut runtime = state.state.write().unwrap();
            runtime.miplay.state = "error".into();
        }
        "warning" => {
            if let Some(message) = text("message") {
                tracing::warn!("miplay: {message}");
            }
        }
        _ => {}
    }
}

/// Start the AirPlay/DLNA/MiPlay hosts.
pub fn start(config: &Config, engine: Arc<AudioEngine>, state: SharedState) -> Result<Arc<ProtocolHosts>> {
    ProtocolHosts::start(config, engine, state)
}

/// Map a core media source (used for diagnostics only).
pub fn core_source_name(source: MediaSource) -> &'static str {
    source.protocol_name()
}

/// Map an internal source to the core event source string.
pub fn source_name(source: Source) -> &'static str {
    source.as_str()
}

/// Ensure `CoreEvent` is referenced so the import stays meaningful for hosts
/// that inspect event names; keeps the module self-documenting.
pub fn core_event_kind(event: &CoreEvent<'_>) -> &'static str {
    match event {
        CoreEvent::ReceiverReady { .. } => "receiver_ready",
        CoreEvent::DlnaMedia { .. } => "dlna_media",
        _ => "other",
    }
}
