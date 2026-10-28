//! Sender-master volume chain.
//!
//! The sender (AirPlay / DLNA / MiPlay) owns the *logical* master volume. The
//! fnOS side must never rewrite what the sender displays. It only post-processes
//! the sender's logical amplitude:
//!
//! `logical (0..=1) -> curve -> global attenuation -> per-device dB -> mute`
//!
//! Curves are defined over the logical 0..=1 range and are monotonic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveKind {
    Linear,
    Soft,
    Balanced,
    Custom,
}

impl Default for CurveKind {
    fn default() -> Self {
        Self::Balanced
    }
}

/// A monotonic volume curve over logical 0..=1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeCurve {
    pub kind: CurveKind,
    /// Custom control points `(x, y)` in 0..=1, sorted by x, monotonic in y.
    #[serde(default)]
    pub points: Vec<(f32, f32)>,
}

impl Default for VolumeCurve {
    fn default() -> Self {
        Self { kind: CurveKind::Balanced, points: Vec::new() }
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Curves are applied on a perceptual level, so intermediate curves are
/// evaluated in dB space and converted back; endpoints stay exact (0->0, 1->1).
fn perceptual(kind: CurveKind, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    match kind {
        CurveKind::Linear => x,
        CurveKind::Soft => smoothstep(x),
        CurveKind::Balanced => {
            // -60 dB .. 0 dB taper for the lower half, then gentle above.
            // 0 -> 0, 1 -> 1, monotonic, louder at the same slider position
            // than `Linear` in the top range (typical "balanced" feel).
            if x <= 0.0 {
                0.0
            } else {
                let db = -60.0 * (1.0 - x).powf(2.2);
                10f32.powf(db / 20.0)
            }
        }
        CurveKind::Custom => x,
    }
}

impl VolumeCurve {
    /// The "neutral" curve (balanced preset, no custom points). Used to detect
    /// un-migrated legacy configs: a device without an explicit curve still has
    /// this value after deserialization.
    pub fn is_default(&self) -> bool {
        self.kind == CurveKind::Balanced && self.points.is_empty()
    }

    /// Map logical sender amplitude (0..=1) to post-curve amplitude (0..=1).
    pub fn apply(&self, logical: f32) -> f32 {
        let x = logical.clamp(0.0, 1.0);
        match self.kind {
            CurveKind::Custom => self.apply_custom(x),
            kind => perceptual(kind, x).clamp(0.0, 1.0),
        }
    }

    /// Piecewise-linear custom curve. The points are normalised to the fixed
    /// endpoints (0,0) and (1,1); x must be non-decreasing and y non-decreasing
    /// (the UI enforces it; `validate` checks it).
    pub fn apply_custom(&self, x: f32) -> f32 {
        let mut pts: Vec<(f32, f32)> = self
            .points
            .iter()
            .copied()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
            .collect();
        pts.push((0.0, 0.0));
        pts.push((1.0, 1.0));
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let x = x.clamp(0.0, 1.0);
        if pts.len() < 2 {
            return x;
        }
        for pair in pts.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            if x >= x0 && x <= x1 {
                if (x1 - x0) <= f32::EPSILON {
                    return y1;
                }
                let t = (x - x0) / (x1 - x0);
                return (y0 + (y1 - y0) * t).clamp(0.0, 1.0);
            }
        }
        pts.last().map(|(_, y)| *y).unwrap_or(x)
    }

    /// Validate monotonicity and endpoint-free (not user-editable) custom points.
    pub fn validate(&self) -> Result<(), String> {
        if self.kind != CurveKind::Custom {
            return Ok(());
        }
        let mut last = (0.0f32, 0.0f32);
        for (x, y) in &self.points {
            if !x.is_finite() || !y.is_finite() {
                return Err("曲线控制点必须是有限数值".into());
            }
            if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                return Err("曲线控制点必须在 0%–100% 之间".into());
            }
            if *x < last.0 || *y < last.1 {
                return Err("曲线控制点必须单调递增（音量提高时输出不能下降）".into());
            }
            last = (*x, *y);
        }
        Ok(())
    }
}

/// Full gain chain for one protocol source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainChain {
    /// 0–100 %, calibration-only global attenuation (default 100).
    pub global_attenuation_percent: f32,
    pub curve: VolumeCurve,
    pub muted: bool,
}

impl Default for GainChain {
    fn default() -> Self {
        Self { global_attenuation_percent: 100.0, curve: VolumeCurve::default(), muted: false }
    }
}

impl GainChain {
    pub fn global_gain(&self) -> f32 {
        (self.global_attenuation_percent.clamp(0.0, 100.0) / 100.0).powi(1)
    }

    /// Sender logical amplitude -> final amplitude before per-device gains.
    pub fn apply(&self, sender_logical: f32) -> f32 {
        if self.muted {
            return 0.0;
        }
        self.curve.apply(sender_logical) * self.global_gain()
    }
}

/// dB value for a per-device balance setting (0 dB = unity, negative only).
pub fn device_db_to_gain(db: f32) -> f32 {
    let db = db.clamp(-60.0, 0.0);
    10f32.powf(db / 20.0)
}

/// Manual delay in ms -> frames at `rate` (used for per-device calibration).
pub fn delay_ms_to_frames(ms: f32, rate: u32) -> usize {
    ((ms.max(0.0) as f64 / 1000.0) * rate as f64).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_hit_endpoints_and_are_monotonic() {
        for kind in [CurveKind::Linear, CurveKind::Soft, CurveKind::Balanced] {
            let c = VolumeCurve { kind, points: vec![] };
            assert!((c.apply(0.0) - 0.0).abs() < 1e-6, "{kind:?} 0");
            assert!((c.apply(1.0) - 1.0).abs() < 1e-6, "{kind:?} 1");
            let mut last = -1.0;
            for i in 0..=100 {
                let y = c.apply(i as f32 / 100.0);
                assert!(y >= last - 1e-5, "{kind:?} must be monotonic at {i}");
                last = y;
            }
        }
    }

    #[test]
    fn custom_curve_interpolates_and_is_monotonic() {
        let c = VolumeCurve {
            kind: CurveKind::Custom,
            points: vec![(0.25, 0.1), (0.5, 0.35), (0.75, 0.75)],
        };
        assert!(c.validate().is_ok());
        assert!((c.apply(0.25) - 0.1).abs() < 1e-5);
        assert!((c.apply(0.5) - 0.35).abs() < 1e-5);
        assert!(c.apply(0.6) > c.apply(0.5));
        assert!((c.apply(0.0)).abs() < 1e-6);
        assert!((c.apply(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn custom_curve_rejects_non_monotonic() {
        let c = VolumeCurve { kind: CurveKind::Custom, points: vec![(0.4, 0.5), (0.6, 0.3)] };
        assert!(c.validate().is_err());
    }

    #[test]
    fn global_attenuation_and_mute_are_independent_from_sender() {
        let mut g = GainChain::default();
        assert!((g.apply(0.6) - g.curve.apply(0.6)).abs() < 1e-6);
        g.global_attenuation_percent = 80.0;
        assert!((g.apply(0.6) - g.curve.apply(0.6) * 0.8).abs() < 1e-5);
        g.muted = true;
        assert_eq!(g.apply(0.6), 0.0);
    }

    #[test]
    fn per_device_db_never_boosts() {
        assert!((device_db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!(device_db_to_gain(-6.0) < 0.51);
        assert!(device_db_to_gain(3.0) <= 1.0, "positive dB must be clamped to unity");
        assert!((delay_ms_to_frames(10.0, 48_000) as i64 - 480).abs() <= 1);
    }
}
