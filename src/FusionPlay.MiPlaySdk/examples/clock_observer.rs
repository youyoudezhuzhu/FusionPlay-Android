//! NAS host for the unmodified FusionPlay receiver algorithm plus opt-in traces.
use cpal::traits::{DeviceTrait, HostTrait};
use fusionplay_miplay_sdk::{
    EventEmitter, MiPlayReceiver, ReceiverConfig, ReceiverIdentity, clock_observer,
};
use serde_json::{Value, json};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|s| s == "--legacy-fixed-media") {
        fusionplay_miplay_sdk::media::enable_legacy_fixed_media_compatibility();
    }
    if args.iter().any(|s| s == "--list-outputs") {
        for device in cpal::default_host().output_devices()? {
            println!(
                "{}",
                json!({"id":device.id()?.to_string(),"name":device.description()?.name()})
            );
        }
        return Ok(());
    }
    let get = |key: &str| {
        args.windows(2)
            .find(|p| p[0] == key)
            .map(|p| p[1].clone())
            .ok_or_else(|| anyhow::anyhow!("missing {key}"))
    };
    let ip: Ipv4Addr = get("--bind")?.parse()?;
    let interface = get("--interface")?;
    let output = get("--output")?;
    let path = PathBuf::from(get("--jsonl")?);
    let stop = PathBuf::from(get("--stop-file")?);
    let seconds: u64 = get("--seconds")?.parse()?;
    anyhow::ensure!((10..=1800).contains(&seconds), "seconds must be 10..1800");
    anyhow::ensure!(
        !stop.exists(),
        "stop file already exists; choose a new run directory"
    );
    // Require an exact available output, instead of silently falling back to a
    // NAS default that may point at an unrelated AirPlay/ALSA pipeline.
    let outputs: Vec<String> = cpal::default_host()
        .output_devices()?
        .filter_map(|d| d.id().ok().map(|v| v.to_string()))
        .collect();
    anyhow::ensure!(
        outputs.iter().any(|id| id.eq_ignore_ascii_case(&output)),
        "requested output unavailable: {output}"
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    clock_observer::enable(&path)?;
    let events: EventEmitter = Arc::new(|value: Value| {
        let kind = value.get("event").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            kind,
            "receiver_ready"
                | "error"
                | "warning"
                | "audio_format"
                | "audio_output_ready"
                | "clock_synchronized"
                | "presentation_clock_updated"
                | "audio_synchronization_started"
                | "audio_output_signal_started"
                | "stream_started"
                | "stream_stopped"
                | "media_channel_closed"
                | "media_session_replaced"
                | "audio_output_callback_started"
        ) {
            return;
        }
        let mut safe = serde_json::Map::new();
        for key in [
            "event",
            "protocol",
            "name",
            "address",
            "port",
            "interface",
            "code",
            "message",
            "media_generation",
            "sample_rate",
            "channels",
            "source_codec",
            "synchronization_mode",
            "output_latency_ms",
            "output_sample_rate",
            "output_callback_frames",
            "device_name",
            "device_id",
            "time_offset_us",
            "clock_synchronized",
            "server_minus_local_us",
            "target_server_minus_local_us",
            "phase_error_us",
            "frequency_ppm",
            "round_trip_us",
            "filtered_samples",
            "fit_history_samples",
        ] {
            if let Some(v) = value.get(key) {
                safe.insert(key.to_owned(), v.clone());
            }
        }
        println!("{}", Value::Object(safe));
    });
    let mac = std::fs::read_to_string(format!("/sys/class/net/{interface}/address"))?
        .trim()
        .to_owned();
    let config = ReceiverConfig::new(
        "MiPlay Clock Observer NAS",
        ReceiverIdentity::new(
            "c448243f-754a-4af4-868c-193906f981c8",
            "4e415350",
            "nas-clock-observer",
        ),
        ip,
        interface,
    )
    .with_output_device(Some(output))
    .with_hardware_address(Some(mac))
    .with_initial_volume_percent(20);
    let receiver = MiPlayReceiver::start(config, events)?;
    let controller = receiver.controller();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline && !stop.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    controller.stop();
    drop(receiver);
    // Allow existing media worker references to observe shutdown and drain the
    // diagnostics queue before the standalone host exits.
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}
