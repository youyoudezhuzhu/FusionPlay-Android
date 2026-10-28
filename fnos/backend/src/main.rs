//! FusionPlay fnOS host binary.
//!
//! CLI:
//!   fusionplay-fnos --config <path> [--state-dir <dir>] [--www <dir>]
//!                   [--listen-tcp <addr>] [--listen-unix <path>]
//!                   [--version] [--probe]
//!
//! The process serves the Web UI/API and (in the next integration step) hosts
//! the three protocol receivers. `FUSIONPLAY_AUDIO_BACKEND=null` forces the
//! no-hardware backend (CI / diagnostics).

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use fusionplay_fnos::audio::alsa::AlsaBackend;
use fusionplay_fnos::audio::{AudioEngine, NullBackend, OutputBackend};
use fusionplay_fnos::config::Config;
use fusionplay_fnos::http::{self, HttpApp};
use fusionplay_fnos::{logging, state::AppState, VERSION};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_signal: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

struct Args {
    config: PathBuf,
    state_dir: PathBuf,
    www: PathBuf,
    listen_tcp: Option<String>,
    listen_unix: Option<PathBuf>,
    probe: bool,
}

fn parse_args() -> Result<Option<Args>> {
    let mut args = std::env::args().skip(1);
    let mut config = fusionplay_fnos::config::default_config_path();
    let mut state_dir = std::env::var_os("TRIM_PKGVAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    let mut www = std::env::var_os("TRIM_APPDEST")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("www"));
    if std::env::var_os("FUSIONPLAY_WWW").is_some() {
        www = PathBuf::from(std::env::var_os("FUSIONPLAY_WWW").unwrap());
    }
    let mut listen_tcp = None;
    let mut listen_unix = std::env::var_os("GATEWAY_SOCKET").map(PathBuf::from);
    let mut probe = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = PathBuf::from(args.next().context("--config 缺少值")?),
            "--state-dir" => state_dir = PathBuf::from(args.next().context("--state-dir 缺少值")?),
            "--www" => www = PathBuf::from(args.next().context("--www 缺少值")?),
            "--listen-tcp" => listen_tcp = Some(args.next().context("--listen-tcp 缺少值")?),
            "--listen-unix" => listen_unix = Some(PathBuf::from(args.next().context("--listen-unix 缺少值")?)),
            "--probe" => probe = true,
            "--version" | "-V" => {
                println!("{VERSION}");
                return Ok(None);
            }
            "--help" | "-h" => {
                println!(
                    "fusionplay-fnos {VERSION}\n\
                     --config <path> --state-dir <dir> --www <dir>\n\
                     --listen-tcp <addr> --listen-unix <path> --probe --version"
                );
                return Ok(None);
            }
            other => anyhow::bail!("未知参数：{other}"),
        }
    }
    if !www.exists() {
        // Development fallback: repo `fnos/frontend`.
        let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend");
        if fallback.exists() {
            www = fallback;
        }
    }
    Ok(Some(Args { config, state_dir, www, listen_tcp, listen_unix, probe }))
}

fn build_engine(config: &Config) -> Result<Arc<AudioEngine>> {
    let mode = std::env::var("FUSIONPLAY_AUDIO_BACKEND").unwrap_or_default();
    if mode.eq_ignore_ascii_case("null") {
        tracing::warn!("音频后端强制为 null（不输出声音）");
        return AudioEngine::new(config.audio.clone(), NullBackend::new());
    }
    match AlsaBackend::new(&config.audio) {
        Ok(backend) => {
            let count = backend.status().devices.len();
            tracing::info!("ALSA 后端已启动，启用设备 {count} 个");
            Ok(AudioEngine::new(config.audio.clone(), backend as Arc<dyn OutputBackend>)?)
        }
        Err(error) => {
            tracing::error!("ALSA 后端启动失败，降级为 null（无声音）：{error:#}");
            AudioEngine::new(config.audio.clone(), NullBackend::new())
        }
    }
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else { return Ok(()) };
    std::fs::create_dir_all(&args.state_dir)
        .with_context(|| format!("无法创建数据目录 {}", args.state_dir.display()))?;
    let config = Config::load(&args.config)?;
    logging::init(&args.state_dir, &config.logging.level, config.logging.max_bytes, config.logging.keep_bytes)?;
    if args.probe {
        let mut rows = Vec::new();
        for device in fusionplay_fnos::audio::alsa::probe_devices() {
            rows.push(format!(
                "{} | {} | rates={:?} ch={} online={}",
                device.id, device.name, device.sample_rates, device.channels, device.online
            ));
        }
        println!("{}", rows.join("\n"));
        return Ok(());
    }

    // SAFETY: installing a minimal async-signal-safe handler before threads start.
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as libc::sighandler_t);
    }

    tracing::info!(
        "FusionPlay fnOS {} 启动：config={} state={} www={}",
        VERSION,
        args.config.display(),
        args.state_dir.display(),
        args.www.display()
    );
    let state = AppState::new();
    let engine = build_engine(&config)?;
    let gateway_prefix = std::env::var("GATEWAY_PREFIX").unwrap_or_else(|_| "/app/fusionplay-fnos".into());
    let app = Arc::new(HttpApp {
        config_path: args.config.clone(),
        config: std::sync::RwLock::new(config.clone()),
        state: Arc::clone(&state),
        engine: Arc::clone(&engine),
        www_dir: args.www.clone(),
        gateway_prefix,
        started: std::time::Instant::now(),
    });
    let tcp = match args.listen_tcp {
        Some(address) => Some(address.parse().with_context(|| format!("非法 TCP 地址 {address}"))?),
        None => None,
    };
    let _servers = http::start(Arc::clone(&app), tcp, args.listen_unix.clone())?;
    tracing::info!(
        "HTTP 已启动：tcp={:?} unix={:?}",
        app.config.read().unwrap().receiver.name,
        args.listen_unix
    );
    while !SHUTDOWN.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    tracing::info!("收到退出信号，正在停止");
    Ok(())
}
