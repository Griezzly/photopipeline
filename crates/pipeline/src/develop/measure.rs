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
    /// 99th percentile. Stays meaningful until 1% of pixels clip, unlike
    /// `p999` which saturates to 1.0 as soon as >0.1% of pixels do — true of
    /// nearly any frame with sky or a specular highlight. This is the
    /// headroom measurement the exposure decision actually wants.
    pub p99: f32,
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
/// the returned percentiles are comparable across cameras. Defensive against
/// non-finite inputs — a corrupt `blacklevel`/`whitelevel` tag or a garbage
/// sample must never leak a NaN or infinity into the returned stats, since the
/// decision layer takes `log2` of these percentiles.
pub fn stats_from_samples(samples: &[f32], black: f32, white: f32) -> RawStats {
    // Sanitise the levels themselves: a zero-denominator Rational in a
    // corrupt blacklevel/whitelevel tag yields NaN or Inf, which would
    // poison every sample via subtraction/division below.
    let black = if black.is_finite() { black } else { 0.0 };
    let white = if white.is_finite() {
        white
    } else {
        u16::MAX as f32
    };

    if samples.is_empty() {
        return RawStats {
            p1: 0.0,
            p50: 0.0,
            p99: 0.0,
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
    let clipped = samples
        .iter()
        .filter(|v| v.is_finite() && **v >= white)
        .count();
    let blacked = samples
        .iter()
        .filter(|v| v.is_finite() && **v <= black)
        .count();

    // Normalise before sorting so the percentile read-out is already in 0..1.
    let range = (white - black).max(1.0);
    let mut norm: Vec<f32> = samples
        .iter()
        .map(|v| {
            // A NaN or infinite sample (sensor glitch, overflowed pixel) is
            // treated as black rather than allowed to propagate: `clamp`
            // does not sanitise NaN, so this substitution must happen first.
            let v = if v.is_finite() { *v } else { black };
            ((v - black) / range).clamp(0.0, 1.0)
        })
        .collect();
    norm.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    RawStats {
        p1: percentile(&norm, 0.01),
        p50: percentile(&norm, 0.50),
        p99: percentile(&norm, 0.99),
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

/// Collect up to `max_samples` values from `data`, restricted to `active_area`
/// when present (row-bounded, so masked black borders outside the rectangle
/// never enter the walk). Falls back to a flat whole-buffer stride when no
/// active area is reported.
fn collect_samples(
    data: &[f32],
    width: usize,
    height: usize,
    cpp: usize,
    active_area: Option<rawler::imgop::Rect>,
    max_samples: usize,
) -> Vec<f32> {
    let Some(area) = active_area else {
        let stride = (data.len() / max_samples).max(1);
        return data.iter().step_by(stride).copied().collect();
    };

    let x0 = area.p.x.min(width);
    let y0 = area.p.y.min(height);
    let x1 = area.p.x.saturating_add(area.d.w).min(width);
    let y1 = area.p.y.saturating_add(area.d.h).min(height);
    let row_len = x1.saturating_sub(x0) * cpp;
    let total = row_len * y1.saturating_sub(y0);
    let stride = (total / max_samples).max(1);

    let mut samples = Vec::with_capacity(max_samples.min(total));
    let mut counter: usize = 0;
    for y in y0..y1 {
        let row_start = y * width * cpp + x0 * cpp;
        let row_end = row_start + row_len;
        if row_end > data.len() {
            break;
        }
        for v in &data[row_start..row_end] {
            if counter.is_multiple_of(stride) {
                samples.push(*v);
            }
            counter += 1;
        }
    }
    samples
}

/// Build coarse linear-RGB triples by averaging 2×2 CFA cells.
///
/// Not a demosaic — each output pixel is a whole cell, so the result is
/// quarter resolution and has no interpolation artefacts. That is exactly
/// right for a white-balance estimate and far cheaper than demosaicing.
///
/// Restricted to `active_area` when present, for the same reason
/// `collect_samples` is: masked optically-black border rows carry no colour
/// information and would drag the estimate toward neutral.
fn cfa_cells_to_rgb(raw: &rawler::rawimage::RawImage, black: f32, white: f32) -> Vec<[f32; 3]> {
    use rawler::rawimage::RawPhotometricInterpretation;

    let (w, h) = (raw.width, raw.height);
    let (x0, y0, x1, y1) = match raw.active_area {
        Some(area) => (
            area.p.x.min(w),
            area.p.y.min(h),
            area.p.x.saturating_add(area.d.w).min(w),
            area.p.y.saturating_add(area.d.h).min(h),
        ),
        None => (0, 0, w, h),
    };

    let RawPhotometricInterpretation::Cfa(ref cfg) = raw.photometric else {
        // Already RGB (some DNGs, LinearRaw): take pixels directly.
        let data = raw.data.as_f32();
        if raw.cpp != 3 {
            return Vec::new();
        }
        let range = (white - black).max(1.0);
        let mut out = Vec::new();
        for y in y0..y1 {
            let row_start = y * w * 3 + x0 * 3;
            let row_end = row_start + (x1.saturating_sub(x0)) * 3;
            if row_end > data.len() {
                break;
            }
            for c in data[row_start..row_end].chunks_exact(3) {
                out.push([
                    ((c[0] - black) / range).clamp(0.0, 1.0),
                    ((c[1] - black) / range).clamp(0.0, 1.0),
                    ((c[2] - black) / range).clamp(0.0, 1.0),
                ]);
            }
        }
        return out;
    };

    let data = raw.data.as_f32();
    let range = (white - black).max(1.0);
    // Cap the walk: a full 60MP pass is pointless for a 3-vector estimate.
    let cell_stride = ((w * h) / (4 * 250_000)).max(1);

    let mut out = Vec::new();
    let mut cell_index = 0usize;
    for y in (y0..y1.saturating_sub(1)).step_by(2) {
        for x in (x0..x1.saturating_sub(1)).step_by(2) {
            cell_index += 1;
            if !cell_index.is_multiple_of(cell_stride) {
                continue;
            }
            let mut sum = [0.0f32; 3];
            let mut count = [0u32; 3];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let color = cfg.cfa.color_at(y + dy, x + dx);
                if color > 2 {
                    continue; // the E channel of an RGBE sensor
                }
                let v = data[(y + dy) * w + (x + dx)];
                sum[color] += ((v - black) / range).clamp(0.0, 1.0);
                count[color] += 1;
            }
            if count.contains(&0) {
                continue;
            }
            out.push([
                sum[0] / count[0] as f32,
                sum[1] / count[1] as f32,
                sum[2] / count[2] as f32,
            ]);
        }
    }
    out
}

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
    let mut white = raw.whitelevel.0.first().copied().unwrap_or(u16::MAX as u32) as f32;
    let mut black = raw
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);
    if !black.is_finite() || !white.is_finite() {
        tracing::warn!(path = %path.display(), "non-finite black/white levels; falling back to neutral");
        black = 0.0;
        white = u16::MAX as f32;
    }

    let samples = collect_samples(
        &data,
        raw.width,
        raw.height,
        raw.cpp,
        raw.active_area,
        MAX_SAMPLES,
    );

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

    let cells = cfa_cells_to_rgb(&raw, black, white);
    match crate::develop::illuminant::estimate_illuminant(&cells) {
        Some(e) => {
            stats.illum_r = Some(e[0]);
            stats.illum_g = Some(e[1]);
            stats.illum_b = Some(e[2]);
        }
        None => {
            tracing::debug!(
                path = %path.display(),
                "illuminant estimation declined; no trustworthy PCA direction"
            );
        }
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
        assert!((s.p99 - 0.99).abs() < 0.01, "p99 was {}", s.p99);
        assert!((s.p999 - 0.999).abs() < 0.01, "p999 was {}", s.p999);
        assert!(s.p1 <= s.p50 && s.p50 <= s.p99 && s.p99 <= s.p999);
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
        for v in [s.p1, s.p50, s.p99, s.p999] {
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

    /// A non-finite black level (e.g. a corrupt blacklevel tag with a
    /// zero-denominator Rational) must not poison the returned percentiles.
    #[test]
    fn non_finite_black_level_yields_finite_stats() {
        let s = stats_from_samples(&[1.0, 2.0, 3.0], f32::NAN, 100.0);
        for v in [s.p1, s.p50, s.p99, s.p999] {
            assert!(v.is_finite(), "value {v} is not finite");
        }
    }

    /// A non-finite white level must likewise not poison the stats.
    #[test]
    fn non_finite_white_level_yields_finite_stats() {
        let s = stats_from_samples(&[1.0, 2.0, 3.0], 0.0, f32::INFINITY);
        for v in [s.p1, s.p50, s.p99, s.p999] {
            assert!(v.is_finite(), "value {v} is not finite");
        }
    }

    /// Individual NaN/Infinity samples in the input must not poison the
    /// percentiles either — a single glitched photosite must not corrupt the
    /// whole frame's measurement.
    #[test]
    fn non_finite_samples_are_excluded_from_finite_stats() {
        let samples = vec![1.0, f32::NAN, 2.0, f32::INFINITY, 3.0, f32::NEG_INFINITY];
        let s = stats_from_samples(&samples, 0.0, 16383.0);
        for v in [s.p1, s.p50, s.p99, s.p999] {
            assert!(v.is_finite(), "value {v} is not finite");
        }
    }
}
