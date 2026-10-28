//! DLNA media playback for the fnOS host.
//!
//! `airplay-receiver-core` owns the DLNA **control plane** (SSDP + AVTransport)
//! and emits `CoreEvent::DlnaMedia { uri, .. }` when a control point starts a
//! track. This module turns that URI into PCM for the unified engine:
//! HTTP(S) fetch -> symphonia decode (mp3/flac/wav/aac/mp4 audio) -> 48 kHz
//! stereo f32 -> engine. Unsupported containers are reported as errors instead
//! of pretending playback started.

use anyhow::{Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::audio::AudioEngine;
use crate::session::{Arbiter, Source};
use crate::state::SharedState;

pub struct DlnaPlayer {
    engine: Arc<AudioEngine>,
    arbiter: Arc<Arbiter>,
    state: SharedState,
    stop: AtomicBool,
    timeout: Duration,
}

impl DlnaPlayer {
    pub fn new(engine: Arc<AudioEngine>, arbiter: Arc<Arbiter>, state: SharedState) -> Arc<Self> {
        Arc::new(Self {
            engine,
            arbiter,
            state,
            stop: AtomicBool::new(false),
            timeout: Duration::from_secs(15),
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Start playback for a DLNA media URI. Blocking; call from a worker thread.
    /// `duration_ms`/`position_ms` come from the AVTransport metadata.
    pub fn play_uri(&self, uri: &str, duration_ms: Option<u64>) -> Result<()> {
        if uri.trim().is_empty() {
            anyhow::bail!("DLNA 媒体地址为空");
        }
        self.stop.store(false, Ordering::Release);
        let claim = self.arbiter.claim(Source::Dlna, uri);
        let epoch = claim.lease.epoch;
        {
            let mut state = self.state.state.write().unwrap();
            state.dlna.state = "buffering".into();
            state.dlna.detail = format!("拉取媒体：{uri}");
            state.media.duration_ms = duration_ms;
        }
        let source = open_source(uri, self.timeout)?;
        let decoded = decode_to_pcm(source, 48_000, 2, &self.stop, &self.engine, epoch)
            .with_context(|| format!("DLNA 媒体解码失败：{uri}"))?;
        {
            let mut state = self.state.state.write().unwrap();
            state.dlna.state = "idle".into();
            state.dlna.detail = format!("已播放 {} 帧", decoded);
        }
        let _ = self.arbiter.release(claim.lease);
        Ok(())
    }
}

/// Open a plain file path or `http://` URL. HTTPS is intentionally rejected
/// with an explicit error (no TLS stack bundled in the first fnOS release).
fn open_source(uri: &str, timeout: Duration) -> Result<Box<dyn Read + Send + Sync>> {
    if let Some(path) = uri.strip_prefix("file://") {
        let file = std::fs::File::open(path).with_context(|| format!("无法打开 {path}"))?;
        return Ok(Box::new(file));
    }
    if uri.starts_with("http://") {
        return Ok(Box::new(std::io::Cursor::new(http_get(uri, timeout)?)));
    }
    if uri.starts_with("https://") {
        anyhow::bail!("DLNA HTTPS 媒体暂不支持（首版未打包 TLS 栈）：{uri}");
    }
    anyhow::bail!("不支持的 DLNA 媒体地址：{uri}");
}

/// Very small HTTP/1.1 GET (LAN media servers only). Reads the whole body.
fn http_get(uri: &str, _timeout: Duration) -> Result<Vec<u8>> {
    use std::io::{BufRead, BufReader, Write};
    let without_scheme = uri.trim_start_matches("http://");
    let (authority, path) = match without_scheme.find('/') {
        Some(position) => (&without_scheme[..position], &without_scheme[position..]),
        None => (without_scheme, "/"),
    };
    let address = if authority.contains(':') { authority.to_string() } else { format!("{authority}:80") };
    let stream = std::net::TcpStream::connect(&address)
        .with_context(|| format!("无法连接 DLNA 媒体服务器 {address}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut stream = stream;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nUser-Agent: FusionPlay-fnOS/{}\r\n\r\n",
        crate::VERSION
    )?;
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    if !status.contains(" 200 ") {
        anyhow::bail!("DLNA 媒体服务器返回非 200：{}", status.trim());
    }
    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "content-length" {
                content_length = value.trim().parse().ok();
            } else if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line)? == 0 {
                break;
            }
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            reader.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else if let Some(length) = content_length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else {
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
    }
    Ok(body)
}

/// Decode an in-memory/streamed media into 48 kHz stereo f32 and push to the
/// engine (symphonia 0.6 probe + codec registry).
fn decode_to_pcm(
    source: Box<dyn Read + Send + Sync>,
    target_rate: u32,
    target_channels: usize,
    stop: &AtomicBool,
    engine: &Arc<AudioEngine>,
    epoch: u64,
) -> Result<u64> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource};
    use symphonia::core::meta::MetadataOptions;

    let media = ReadOnlySource::new(source);
    let mss = MediaSourceStream::new(Box::new(media), MediaSourceStreamOptions::default());
    let mut format = symphonia::default::get_probe()
        .probe(&Hint::new(), mss, FormatOptions::default(), MetadataOptions::default())
        .context("无法识别媒体格式（支持 mp3/flac/wav/aac/mp4）")?;
    let track = format
        .first_track_known_codec(TrackType::Audio)
        .cloned()
        .context("媒体中没有可解码的音轨")?;
    let track_id = track.id;
    let params = match track.codec_params.clone() {
        Some(CodecParameters::Audio(params)) => params,
        _ => anyhow::bail!("音轨缺少编解码参数"),
    };
    let source_rate = params.sample_rate.unwrap_or(target_rate);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("无法创建解码器")?;
    let source_channels = params.channels.map(|channels| channels.count()).unwrap_or(2).max(1);
    let mut resampler = crate::audio::resample::LinearResampler::new(target_channels);
    let ratio = target_rate as f64 / source_rate.max(1) as f64;
    let mut frames = 0u64;
    while !stop.load(Ordering::Acquire) {
        let Some(packet) = format.next_packet().context("读取媒体包失败")? else {
            break;
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(_) => continue,
        };
        let mut interleaved = vec![0.0f32; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut interleaved);
        let frames_in = interleaved.len() / source_channels;
        let mut stereo = Vec::with_capacity(frames_in * target_channels);
        for frame in 0..frames_in {
            for channel in 0..target_channels {
                let source_channel = channel.min(source_channels - 1);
                stereo.push(interleaved[frame * source_channels + source_channel]);
            }
        }
        let resampled = if (ratio - 1.0).abs() > 1e-9 {
            resampler.process(&stereo, ratio)
        } else {
            stereo
        };
        if !resampled.is_empty() {
            frames += (resampled.len() / target_channels) as u64;
            engine.push_pcm(Source::Dlna, epoch, 1.0, target_rate, target_channels as u16, &resampled);
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_https_and_unknown_schemes_explicitly() {
        match open_source("https://example.com/a.mp3", Duration::from_secs(1)) {
            Err(error) => assert!(format!("{error}").contains("HTTPS")),
            Ok(_) => panic!("https must be rejected explicitly"),
        }
        match open_source("ftp://example.com/a.mp3", Duration::from_secs(1)) {
            Err(error) => assert!(format!("{error}").contains("不支持")),
            Ok(_) => panic!("unsupported scheme must be rejected"),
        }
    }
}
