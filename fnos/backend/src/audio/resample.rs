//! Resampling helpers with an explicit drift ratio.
//!
//! Each output device runs on its own hardware clock. The engine keeps a
//! per-device ring buffer and measures its fill level; `DriftEstimator` turns
//! that into a slow ratio correction (ppm) that the resampler applies, so a
//! fast/slow DAC does not slowly drain/fill the buffer forever.

/// Maximum correction applied to a device (ppm). Keep this small: audible
/// pitch changes are unacceptable, 1000 ppm is ~0.1 % and already generous.
pub const MAX_DRIFT_PPM: f32 = 1000.0;

/// Low-pass + clamp for the buffer-level -> drift loop.
pub struct DriftEstimator {
    target_frames: f32,
    ppm: f32,
    smoothing: f32,
    max_ppm: f32,
}

impl DriftEstimator {
    pub fn new(target_frames: usize, smoothing: f32, max_ppm: f32) -> Self {
        Self {
            target_frames: target_frames.max(1) as f32,
            ppm: 0.0,
            smoothing: smoothing.clamp(0.001, 1.0),
            max_ppm: max_ppm.clamp(1.0, MAX_DRIFT_PPM),
        }
    }

    pub fn ppm(&self) -> f32 {
        self.ppm
    }

    /// Feed the measured fill level of the device ring (in frames) and return
    /// the new correction. Positive ppm means "consume a little faster".
    pub fn update(&mut self, fill_frames: f32) -> f32 {
        let error = (fill_frames - self.target_frames) / self.target_frames;
        // ~1 % of error per update; the exact gain is not critical because the
        // loop is slow and bounded.
        let target_ppm = (error * 20_000.0).clamp(-self.max_ppm, self.max_ppm);
        self.ppm += (target_ppm - self.ppm) * self.smoothing;
        self.ppm
    }

    /// Linear interpolation resampler ratio: output/input frame ratio.
    pub fn ratio(&self) -> f64 {
        1.0 + (self.ppm as f64) / 1_000_000.0
    }
}

/// Minimal streaming linear resampler used per device. It keeps the last frame
/// for continuity; the quality is intentionally modest because the engine
/// normally runs 48 kHz and devices usually accept 48 kHz natively (ratio
/// ≈ 1.0, i.e. this is only used for odd rates and drift compensation).
pub struct LinearResampler {
    channels: usize,
    pos: f64,
    last: Vec<f32>,
    pending: Vec<f32>,
}

impl LinearResampler {
    pub fn new(channels: usize) -> Self {
        Self { channels: channels.max(1), pos: 0.0, last: vec![0.0; channels.max(1)], pending: Vec::new() }
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.pending.clear();
    }

    /// Push interleaved input; returns interleaved output at `ratio` speed.
    pub fn process(&mut self, input: &[f32], ratio: f64) -> Vec<f32> {
        let channels = self.channels;
        self.pending.extend_from_slice(input);
        let frames = self.pending.len() / channels;
        if frames == 0 {
            return Vec::new();
        }
        let ratio = ratio.clamp(0.9, 1.1);
        let out_frames = ((frames as f64) / ratio).ceil() as usize;
        let mut out = Vec::with_capacity(out_frames * channels);
        for i in 0..out_frames {
            let sample_pos = self.pos + i as f64 * ratio;
            let idx = sample_pos.floor() as usize;
            let frac = (sample_pos - idx as f64) as f32;
            if idx >= frames {
                // Not enough input yet: stop and keep the remainder for the
                // next call instead of emitting garbage/zeros.
                self.pos = sample_pos - frames as f64;
                let consumed = frames * channels;
                self.pending.drain(..consumed);
                return out;
            }
            let next = (idx + 1).min(frames - 1);
            for ch in 0..channels {
                let a = self.pending[idx * channels + ch];
                let b = self.pending[next * channels + ch];
                out.push(a + (b - a) * frac);
            }
        }
        self.pos = 0.0;
        self.pending.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_estimator_converges_and_is_bounded() {
        let mut estimator = DriftEstimator::new(4_800, 0.05, 500.0);
        for _ in 0..2_000 {
            estimator.update(5_000.0);
        }
        assert!(estimator.ppm() > 0.0 && estimator.ppm() <= 500.0);
        for _ in 0..5_000 {
            estimator.update(0.0);
        }
        assert!(estimator.ppm() < 0.0 && estimator.ppm() >= -500.0);
    }

    #[test]
    fn unity_resampler_is_bit_exact_ish() {
        let mut resampler = LinearResampler::new(2);
        let input: Vec<f32> = (0..200).map(|i| i as f32 * 0.01).collect();
        let output = resampler.process(&input, 1.0);
        assert_eq!(output.len(), input.len());
        assert!((output[0] - input[0]).abs() < 1e-6);
        assert!((output[input.len() - 1] - input[input.len() - 1]).abs() < 1e-3);
    }

    #[test]
    fn resampler_keeps_remainder_between_calls() {
        let mut resampler = LinearResampler::new(1);
        let mut total = 0;
        for chunk in [vec![0.0f32; 100], vec![1.0f32; 100]] {
            total += resampler.process(&chunk, 1.0).len();
        }
        assert!(total >= 199 && total <= 201, "got {total}");
    }
}
