//! File logging with bounded size and an in-place tail trim.
//!
//! Renders `tracing` events (from our code and the reused protocol crates) to
//! `$TRIM_PKGVAR/fusionplay.log`. Rotation rewrites the same inode (tail-trim),
//! so a long-running daemon never needs a restart and keeps its fd.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer as _;
use tracing_subscriber::Registry;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LIMITS: OnceLock<(u64, u64)> = OnceLock::new();
static LEVEL: AtomicU8 = AtomicU8::new(2); // 0=error 1=warn 2=info 3=debug 4=trace

/// Runtime-adjustable global level filter (the Web UI can change it without a
/// restart; `tracing` has no built-in runtime level switch without `reload`).
struct DynamicLevel;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DynamicLevel {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _context: LayerContext<'_, S>) -> bool {
        let max = LEVEL.load(Ordering::Relaxed);
        let needed = match *metadata.level() {
            tracing::Level::ERROR => 0,
            tracing::Level::WARN => 1,
            tracing::Level::INFO => 2,
            tracing::Level::DEBUG => 3,
            tracing::Level::TRACE => 4,
        };
        needed <= max
    }
}

pub fn path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

fn level_from_str(level: &str) -> (u8, &'static str) {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => (0, "error"),
        "warn" => (1, "warn"),
        "debug" => (3, "debug"),
        "trace" => (4, "trace"),
        _ => (2, "info"),
    }
}

pub fn init(dir: &Path, level: &str, max_bytes: u64, keep_bytes: u64) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("无法创建日志目录 {}", dir.display()))?;
    let log_file = dir.join("fusionplay.log");
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&log_file);
    LOG_PATH.set(log_file.clone()).ok();
    let max = max_bytes.max(1024 * 1024);
    let keep = keep_bytes.clamp(64 * 1024, max / 2);
    LIMITS.set((max, keep)).ok();
    let (numeric, _name) = level_from_str(level);
    LEVEL.store(numeric, Ordering::Relaxed);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(RotatingWriter);
    Registry::default()
        .with(DynamicLevel)
        .with(file_layer)
        .try_init()
        .ok(); // a second init (tests) is harmless
    // Background size guard: tail-trim in place, never rename.
    let limits = (max, keep);
    std::thread::Builder::new()
        .name("log-rotate".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if let Some(path) = path() {
                rotate_if_needed(&path, limits.0, limits.1);
            }
        })
        .ok();
    Ok(())
}

pub fn set_level(level: &str) -> Result<()> {
    let (numeric, _name) = level_from_str(level);
    LEVEL.store(numeric, Ordering::Relaxed);
    Ok(())
}

pub fn level_name() -> &'static str {
    match LEVEL.load(Ordering::Relaxed) {
        0 => "error",
        1 => "warn",
        3 => "debug",
        4 => "trace",
        _ => "info",
    }
}

fn rotate_if_needed(path: &Path, max: u64, keep: u64) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() <= max {
        return;
    }
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(path) else { return };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(keep);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).is_err() {
        return;
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return;
    }
    if file.set_len(0).is_err() {
        return;
    }
    let _ = file.write_all(&tail);
    let _ = file.flush();
}

pub fn recent_lines(lines: usize) -> Vec<String> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(meta) = std::fs::metadata(&path) else { return Vec::new() };
    let read_len = (meta.len() as usize).min(256 * 1024);
    let Ok(mut file) = File::open(&path) else { return Vec::new() };
    let _ = file.seek(SeekFrom::End(-(read_len as i64)));
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    let mut rows: Vec<&str> = text.lines().collect();
    if rows.len() > lines {
        rows = rows.split_off(rows.len() - lines);
    }
    rows.into_iter().map(|line| line.to_string()).collect()
}

struct RotatingWriter;

struct Guard(Mutex<Option<File>>);

impl<'a> MakeWriter<'a> for RotatingWriter {
    type Writer = Guard;

    fn make_writer(&'a self) -> Self::Writer {
        let file = path().and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());
        Guard(Mutex::new(file))
    }
}

impl Write for Guard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.get_mut() {
            Ok(guard) => match guard.as_mut() {
                Some(file) => file.write(buf),
                None => Ok(buf.len()),
            },
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Ok(guard) = self.0.get_mut() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_inode_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let mut file = OpenOptions::new().create(true).write(true).open(&path).unwrap();
        let inode_before = {
            use std::os::unix::fs::MetadataExt;
            file.metadata().unwrap().ino()
        };
        write!(file, "head\n{}\n", "x".repeat(4096)).unwrap();
        file.flush().unwrap();
        rotate_if_needed(&path, 1024, 128);
        let inode_after = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).unwrap().ino()
        };
        assert_eq!(inode_before, inode_after, "rotation must rewrite the same inode");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() <= 160, "tail-trim kept {} bytes", text.len());
        assert!(text.trim_end().ends_with('x'));
    }
}
