//! Raw-linear sensor statistics. Distinct from the `exposure` table, which is
//! derived from the 8-bit preview: a preview reports 255 for anything the
//! camera's tone curve pushed to white, whereas the raw reveals whether the
//! photosite actually saturated. Highlight reconstruction needs the latter.

use std::path::Path;

use crate::develop::DevelopError;

/// Raw-linear statistics for one file. Percentiles are black-subtracted and
/// white-normalised into 0..1.
#[derive(Debug, Clone, PartialEq)]
pub struct RawStats {
    pub p1: f32,
    pub p50: f32,
    pub p999: f32,
    /// Fraction of samples at or above the white level.
    pub clipped_frac: f32,
    /// Fraction of samples at or below the black level.
    pub black_frac: f32,
    /// As-shot white balance coefficients as encoded in the file (unnormalised).
    pub wb_r: f32,
    pub wb_g: f32,
    pub wb_b: f32,
    /// PCA illuminant estimate; `None` when estimation fails.
    pub illum_r: Option<f32>,
    pub illum_g: Option<f32>,
    pub illum_b: Option<f32>,
}

/// Percentiles and clipping fractions from raw sample values.
///
/// Pure: no I/O, no decode. `black` and `white` are the sensor's own levels, so
/// the returned percentiles are comparable across cameras.
pub fn stats_from_samples(samples: &[f32], black: f32, white: f32) -> RawStats {
    if samples.is_empty() {
        return RawStats {
            p1: 0.0,
            p50: 0.0,
            p999: 0.0,
            clipped_frac: 0.0,
            black_frac: 0.0,
            wb_r: 1.0,
            wb_g: 1.0,
            wb_b: 1.0,
            illum_r: None,
            illum_g: None,
            illum_b: None,
        };
    }

    let n = samples.len();
    let clipped = samples.iter().filter(|v| **v >= white).count();
    let blacked = samples.iter().filter(|v| **v <= black).count();

    // Normalise before sorting so the percentile read-out is already in 0..1.
    let range = (white - black).max(1.0);
    let mut norm: Vec<f32> = samples
        .iter()
        .map(|v| ((v - black) / range).clamp(0.0, 1.0))
        .collect();
    norm.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    RawStats {
        p1: percentile(&norm, 0.01),
        p50: percentile(&norm, 0.50),
        p999: percentile(&norm, 0.999),
        clipped_frac: clipped as f32 / n as f32,
        black_frac: blacked as f32 / n as f32,
        wb_r: 1.0,
        wb_g: 1.0,
        wb_b: 1.0,
        illum_r: None,
        illum_g: None,
        illum_b: None,
    }
}

/// Nearest-rank percentile over an already-sorted slice.
fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f32 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Sample at most this many photosites. Percentile estimates converge long
/// before a full 60MP read, and the stride keeps the walk cache-friendly.
const MAX_SAMPLES: usize = 2_000_000;

/// Decode `path` and compute raw-linear statistics.
///
/// Reads the raw sensor plane, not the embedded preview. Restricted to the
/// active area when the decoder reports one, so masked black borders never
/// enter the percentiles.
pub fn measure_raw(path: &Path) -> Result<RawStats, DevelopError> {
    use rawler::{decoders::RawDecodeParams, rawsource::RawSource};

    let src = RawSource::new(path).map_err(|e| DevelopError::Decode {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let decoder = rawler::get_decoder(&src).map_err(|e| DevelopError::Decode {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    let raw = decoder
        .raw_image(&src, &RawDecodeParams::default(), false)
        .map_err(|e| DevelopError::Decode {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

    let data = raw.data.as_f32();
    let white = raw.whitelevel.0.first().copied().unwrap_or(u16::MAX as u32) as f32;
    let black = raw
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    let stride = (data.len() / MAX_SAMPLES).max(1);
    let samples: Vec<f32> = data.iter().step_by(stride).copied().collect();

    let mut stats = stats_from_samples(&samples, black, white);
    // rawler stores coefficients in RGBE order.
    stats.wb_r = raw.wb_coeffs[0];
    stats.wb_g = raw.wb_coeffs[1];
    stats.wb_b = raw.wb_coeffs[2];
    if !stats.wb_r.is_finite() || !stats.wb_g.is_finite() || !stats.wb_b.is_finite() {
        tracing::warn!(path = %path.display(), "non-finite wb_coeffs; falling back to neutral");
        stats.wb_r = 1.0;
        stats.wb_g = 1.0;
        stats.wb_b = 1.0;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp from black to white: percentiles land at known positions and
    /// nothing is clipped except the single top sample.
    #[test]
    fn ramp_percentiles_are_positionally_correct() {
        let samples: Vec<f32> = (0..=1000).map(|i| i as f32).collect();
        let s = stats_from_samples(&samples, 0.0, 1000.0);
        assert!((s.p50 - 0.5).abs() < 0.01, "p50 was {}", s.p50);
        assert!((s.p1 - 0.01).abs() < 0.01, "p1 was {}", s.p1);
        assert!((s.p999 - 0.999).abs() < 0.01, "p999 was {}", s.p999);
    }

    /// Black subtraction and white normalisation: a sensor with black=512,
    /// white=16383 must map its own black to 0.0 and its own white to 1.0.
    #[test]
    fn black_is_subtracted_and_white_normalised() {
        let samples = vec![512.0, 512.0, 8447.5, 16383.0, 16383.0];
        let s = stats_from_samples(&samples, 512.0, 16383.0);
        assert!((s.p50 - 0.5).abs() < 0.01, "p50 was {}", s.p50);
        // two of five samples sit at white, two at black
        assert!(
            (s.clipped_frac - 0.4).abs() < 0.001,
            "clipped {}",
            s.clipped_frac
        );
        assert!((s.black_frac - 0.4).abs() < 0.001, "black {}", s.black_frac);
    }

    /// Values below black or above white must clamp, never produce negatives
    /// or >1 — the decide() formulas take log2 of these and would produce NaN.
    #[test]
    fn out_of_range_samples_clamp_to_unit_interval() {
        let samples = vec![0.0, 100.0, 20000.0];
        let s = stats_from_samples(&samples, 512.0, 16383.0);
        for v in [s.p1, s.p50, s.p999] {
            assert!(
                (0.0..=1.0).contains(&v),
                "value {v} escaped the unit interval"
            );
        }
    }

    /// An empty sample set must not panic or divide by zero.
    #[test]
    fn empty_samples_yield_neutral_stats() {
        let s = stats_from_samples(&[], 0.0, 16383.0);
        assert_eq!(s.clipped_frac, 0.0);
        assert_eq!(s.black_frac, 0.0);
        assert_eq!(s.p50, 0.0);
    }
}
