//! Opt-in timing-only JSONL. Audio threads enqueue fixed-size samples; a worker
//! serializes/writes them. No PCM, authentication headers, keys, or metadata.
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 64 * 1024 * 1024;
static SINK: OnceLock<Arc<Sink>> = OnceLock::new();
static SESSION: AtomicU64 = AtomicU64::new(1);

struct Sink {
    tx: SyncSender<Sample>,
    lost: AtomicU64,
}

/// Enable before starting a receiver. Once per process, fail visibly on bad paths.
/// Android's JNI host opts in only when its identity directory contains the
/// explicit `enable-clock-observer` marker file. Desktop hosts may call this API.
pub fn enable(path: &Path) -> io::Result<()> {
    if SINK.get().is_some() {
        return Ok(());
    }
    let sink = start_writer(path)?;
    let _ = SINK.set(sink);
    Ok(())
}

fn start_writer(path: &Path) -> io::Result<Arc<Sink>> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut bytes = file.metadata()?.len();
    let (tx, rx) = mpsc::sync_channel::<Sample>(1024);
    std::thread::Builder::new().name("miplay-clock-jsonl".into()).spawn(move || {
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(sample) => {
                    if bytes >= MAX_BYTES { continue; }
                    let line = encode(sample).to_string();
                    if writeln!(file, "{line}").is_err() { break; }
                    bytes += line.len() as u64 + 1;
                    if bytes >= MAX_BYTES {
                        let _ = writeln!(file, "{}", json!({"event":"diagnostics_limit_reached","maximum_bytes":MAX_BYTES}));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => { if file.flush().is_err() { break; } }
                Err(mpsc::RecvTimeoutError::Disconnected) => { let _ = file.flush(); break; }
            }
        }
    })?;
    Ok(Arc::new(Sink {
        tx,
        lost: AtomicU64::new(0),
    }))
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Sample {
    pub event: &'static str,
    pub action: Option<&'static str>,
    pub session: u64,
    pub elapsed_us: u64,
    pub epoch_wall_us: u64,
    pub dropped_events: u64,
    pub local_clock_us: Option<i64>,
    pub media_pts_ticks: Option<u64>,
    pub media_pts_us: Option<f64>,
    pub time_offset_us: Option<i64>,
    pub server_now_us: Option<i64>,
    pub target_timer_us: Option<f64>,
    pub output_latency_us: Option<f64>,
    pub presentation_error_us: Option<f64>,
    pub queue_frames: Option<usize>,
    pub target_queue_frames: Option<usize>,
    pub playback_rate: Option<f64>,
    pub dropped_frames: Option<usize>,
    pub raw_timer: Option<[u8; 40]>,
    pub client_send_us: Option<i64>,
    pub client_receive_us: Option<i64>,
    pub server_send_us: Option<i64>,
    pub server_receive_us: Option<i64>,
    pub network_rtt_us: Option<i64>,
    pub server_minus_local_us: Option<i64>,
    pub frequency_ppm: Option<f64>,
    pub sequence: Option<u32>,
    pub accepted: Option<bool>,
    pub timer_server: Option<SocketAddr>,
    pub cseq: Option<u32>,
}

fn encode(s: Sample) -> serde_json::Value {
    json!({"schema":1,"event":s.event,"action":s.action,
        "process_id":std::process::id(),"session_id":s.session,
        "observer_elapsed_us":s.elapsed_us,"observer_epoch_wall_us":s.epoch_wall_us,
        "dropped_events_total":s.dropped_events,"local_clock_us":s.local_clock_us,
        "media_pts_ticks":s.media_pts_ticks,"media_pts_us":s.media_pts_us,
        "time_offset_us":s.time_offset_us,"server_now_us":s.server_now_us,
        "target_timer_us":s.target_timer_us,"output_latency_us":s.output_latency_us,
        "presentation_error_us":s.presentation_error_us,"queue_frames":s.queue_frames,
        "target_queue_frames":s.target_queue_frames,"playback_rate":s.playback_rate,
        "dropped_frames":s.dropped_frames,"timer_payload_hex":s.raw_timer.map(hex::encode),
        "client_send_us":s.client_send_us,"client_receive_us":s.client_receive_us,
        "server_send_us":s.server_send_us,"server_receive_us":s.server_receive_us,
        "network_rtt_us":s.network_rtt_us,"server_minus_local_us":s.server_minus_local_us,
        "frequency_ppm":s.frequency_ppm,"sequence":s.sequence,"accepted":s.accepted,
        "timer_server":s.timer_server.map(|v|v.to_string()),"cseq":s.cseq})
}

pub(crate) struct Observer {
    sink: Option<Arc<Sink>>,
    session: u64,
    epoch: Instant,
    epoch_wall_us: u64,
    callbacks: AtomicU64,
    media_frames: AtomicU64,
    last_pts: AtomicI64,
}

impl Observer {
    pub fn new() -> Self {
        let observer = Self {
            sink: SINK.get().cloned(),
            session: SESSION.fetch_add(1, Ordering::Relaxed),
            epoch: Instant::now(),
            epoch_wall_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros()
                .min(u64::MAX as u128) as u64,
            callbacks: AtomicU64::new(0),
            media_frames: AtomicU64::new(0),
            last_pts: AtomicI64::new(i64::MIN),
        };
        observer.emit(Sample {
            event: "session_created",
            ..Sample::default()
        });
        observer
    }

    pub fn enabled(&self) -> bool {
        self.sink.is_some()
    }

    pub fn emit(&self, mut sample: Sample) {
        let Some(sink) = &self.sink else {
            return;
        };
        sample.session = self.session;
        sample.elapsed_us = self.epoch.elapsed().as_micros().min(u64::MAX as u128) as u64;
        sample.epoch_wall_us = self.epoch_wall_us;
        sample.dropped_events = sink.lost.load(Ordering::Relaxed);
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = sink.tx.try_send(sample)
        {
            sink.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Log raw PES ticks independently from decoded/output-queue sample positions.
    pub fn pes(&self, ticks: Option<u64>) {
        if !self.enabled() {
            return;
        }
        let n = self.media_frames.fetch_add(1, Ordering::Relaxed);
        if let Some(current) = ticks {
            let previous = self.last_pts.swap(current as i64, Ordering::Relaxed);
            if previous != i64::MIN {
                let delta = (current as i64 - previous + (1_i64 << 32)).rem_euclid(1_i64 << 33)
                    - (1_i64 << 32);
                if delta.abs() > 9_000 {
                    self.emit(Sample {
                        event: "media_pts_discontinuity",
                        action: Some("correlate_with_control_log"),
                        media_pts_ticks: Some(current),
                        media_pts_us: Some((current * 1_000_000 / 90_000) as f64),
                        ..Sample::default()
                    });
                }
            }
        }
        if n < 4 || n % 32 == 0 {
            self.emit(Sample {
                event: "pes_received",
                media_pts_ticks: ticks,
                media_pts_us: ticks.map(|v| (v * 1_000_000 / 90_000) as f64),
                ..Sample::default()
            });
        }
    }

    pub fn callback(&self, sample: Sample) {
        if !self.enabled() {
            return;
        }
        let n = self.callbacks.fetch_add(1, Ordering::Relaxed);
        if n < 4 || n % 64 == 0 || sample.dropped_frames.unwrap_or(0) > 0 {
            self.emit(sample);
        }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.emit(Sample {
            event: "session_closed",
            ..Sample::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_keeps_absent_measurements_null_and_preserves_signed_offset() {
        let v = encode(Sample {
            event: "presentation_clock_sample",
            time_offset_us: Some(-42),
            ..Sample::default()
        });
        assert!(v["server_now_us"].is_null());
        assert!(v["media_pts_ticks"].is_null());
        assert_eq!(v["time_offset_us"], -42);
        assert!(v.get("auth_key").is_none());
    }
    #[test]
    fn bounded_channel_counts_lost_samples_without_waiting_for_a_reader() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let sink = Arc::new(Sink {
            tx,
            lost: AtomicU64::new(0),
        });
        let mut observer = Observer::new();
        observer.sink = Some(Arc::clone(&sink));
        observer.emit(Sample::default());
        observer.emit(Sample::default());
        assert_eq!(sink.lost.load(Ordering::Relaxed), 1);
    }
}
