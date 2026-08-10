//! The decision layer: a pure function from measurements to an `EditRecipe`.
//!
//! No image data, no I/O, no database. That is the single most important
//! testability property in this design — the tuning-sensitive logic is
//! exercised by table-driven unit tests over numbers, without fixtures.

use crate::develop::measure::RawStats;
use crate::ingest::exif::ExifData;

/// Bumped whenever any formula below changes. Stored in `edits.decider_version`
/// and part of the idempotency key, so a tuning change re-renders everything.
pub const DECIDER_VERSION: &str = "decide-1";

/// The sharpness measurement `decide` consumes. A struct rather than a bare f32
/// so adding subject/background terms later does not churn the signature.
#[derive(Debug, Clone, Copy)]
pub struct Sharpness {
    /// Global sharpness score from the `sharpness` table, roughly 0..1.
    pub s_global: f32,
}

/// A complete, renderer-agnostic development recipe. Every field is normalised
/// 0..1 except `exposure_ev` (stops), `wb_temp_k` (kelvin) and `wb_green`.
#[derive(Debug, Clone, PartialEq)]
pub struct EditRecipe {
    pub exposure_ev: f32,
    pub wb_temp_k: f32,
    pub wb_green: f32,
    pub highlight_recovery: f32,
    pub shadow_lift: f32,
    pub denoise_luma: f32,
    pub denoise_chroma: f32,
    pub sharpen_amount: f32,
    pub lens_correct: bool,
}

impl EditRecipe {
    /// Content hash of the recipe, for idempotency. Fields are quantised before
    /// hashing so a float rounding difference of 1e-9 does not force a re-render.
    pub fn recipe_hash(&self) -> String {
        use std::hash::Hasher;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        for v in [
            self.exposure_ev,
            self.wb_temp_k,
            self.wb_green,
            self.highlight_recovery,
            self.shadow_lift,
            self.denoise_luma,
            self.denoise_chroma,
            self.sharpen_amount,
        ] {
            h.write_i64((v * 10_000.0).round() as i64);
        }
        h.write_u8(self.lens_correct as u8);
        format!("{:016x}", h.finish())
    }
}

/// Middle grey in a linear raw signal.
const MID_GREY: f32 = 0.18;
/// Target for the 99.9th percentile: just below clipping.
const HIGHLIGHT_TARGET: f32 = 0.95;
/// Floor for any percentile before a log is taken, so log2 never sees zero.
const LOG_FLOOR: f32 = 1e-6;

/// Decide how to develop one photo.
pub fn decide(raw: &RawStats, exif: &ExifData, sharp: &Sharpness) -> EditRecipe {
    let iso = exif.iso.unwrap_or(100) as f32;

    // ── exposure ──
    // Lift toward middle grey, but never past the point where the brightest
    // *recoverable* detail clips. An overexposed frame has negative headroom,
    // which pulls the exposure down.
    //
    // Headroom is measured from p99, NOT p999. p999 saturates to 1.0 as soon as
    // more than 0.1% of pixels clip — true of almost any frame containing sky, a
    // specular highlight, or a light source — after which it reports zero
    // headroom regardless of how dark the image actually is, and the lift is
    // silently thrown away. Measured on a real frame: p50 = 0.0587 (median 1.62
    // stops below middle grey), p999 = 1.0, clipped_frac = 2.1%; the p999 form
    // emitted -0.07 EV where +1.62 EV was wanted. Pixels that already clipped
    // are unrecoverable, so protecting them costs the rest of the image.
    let headroom = (HIGHLIGHT_TARGET / raw.p99.max(LOG_FLOOR)).log2();
    let lift = (MID_GREY / raw.p50.max(LOG_FLOOR)).log2();
    let exposure_ev = lift.min(headroom).clamp(-3.0, 3.0);

    // ── white balance ──
    let (as_shot_k, as_shot_g) = coeffs_to_temp_green(raw.wb_r, raw.wb_g, raw.wb_b);
    let (wb_temp_k, wb_green) = match (raw.illum_r, raw.illum_g, raw.illum_b) {
        (Some(r), Some(g), Some(b)) => {
            // The illuminant is an estimate of the light; the as-shot
            // coefficients are its reciprocal. Invert before comparing.
            let (est_k, est_g) = coeffs_to_temp_green(
                1.0 / r.max(LOG_FLOOR),
                1.0 / g.max(LOG_FLOOR),
                1.0 / b.max(LOG_FLOOR),
            );
            let angle =
                angular_distance([raw.wb_r, raw.wb_g, raw.wb_b], [1.0 / r, 1.0 / g, 1.0 / b]);
            if angle < AGREEMENT_RADIANS {
                // Cameras are usually right; a confirming estimate adds nothing.
                (as_shot_k, as_shot_g)
            } else {
                // Large disagreement means mixed or artificial light, where
                // neither estimator is trustworthy. Split the difference rather
                // than committing to either.
                (0.5 * as_shot_k + 0.5 * est_k, 0.5 * as_shot_g + 0.5 * est_g)
            }
        }
        _ => (as_shot_k, as_shot_g),
    };

    // ── highlight recovery ──
    // Scales with how much actually clipped. 5% clipped is already severe.
    let highlight_recovery = (raw.clipped_frac / 0.05).clamp(0.0, 1.0);

    // ── shadow lift ──
    // Driven by how much sits at the bottom, throttled by a noise penalty:
    // lifting shadows at high ISO only reveals noise.
    let shadow_demand =
        (raw.black_frac / 0.05).clamp(0.0, 1.0) + ((0.02 - raw.p1).max(0.0) / 0.02).clamp(0.0, 1.0);
    let noise_penalty = 1.0 - denoise_curve(iso);
    let shadow_lift = (shadow_demand * 0.5 * noise_penalty).clamp(0.0, 1.0);

    // ── denoise ──
    let denoise_luma = denoise_curve(iso);
    // Chroma noise is more objectionable and cheaper to remove than luma.
    let denoise_chroma = (denoise_luma * 1.2).clamp(0.0, 1.0);

    // ── sharpening ──
    // Modulated by measured sharpness and hard-capped, so a genuinely soft
    // frame is never sharpened into crunch.
    let sharpen_amount = (sharp.s_global.clamp(0.0, 1.0) * 0.8).clamp(0.0, 0.8);

    EditRecipe {
        exposure_ev,
        wb_temp_k,
        wb_green,
        highlight_recovery,
        shadow_lift,
        denoise_luma,
        denoise_chroma,
        sharpen_amount,
        lens_correct: exif.lens_model.is_some(),
    }
}

/// Angular distance below which the PCA estimate is treated as confirming the
/// camera rather than contradicting it. ~11°.
const AGREEMENT_RADIANS: f32 = 0.2;

/// Piecewise-linear denoise strength in ISO, through the spec's anchor points:
/// (100→0), (1600→0.3), (6400→0.6), (25600→0.85).
///
/// These anchors are a starting shape, not a validated claim — spec §13 open
/// item 2 calls for calibration against real high-ISO files before they can be
/// described as tuned.
fn denoise_curve(iso: f32) -> f32 {
    const ANCHORS: [(f32, f32); 4] = [(100.0, 0.0), (1600.0, 0.3), (6400.0, 0.6), (25600.0, 0.85)];
    if iso <= ANCHORS[0].0 {
        return ANCHORS[0].1;
    }
    for w in ANCHORS.windows(2) {
        let (x0, y0) = w[0];
        let (x1, y1) = w[1];
        if iso <= x1 {
            // Interpolate in log-ISO: a stop is a stop, whatever the absolute value.
            let t = (iso.log2() - x0.log2()) / (x1.log2() - x0.log2());
            return y0 + t * (y1 - y0);
        }
    }
    // Beyond the last anchor, approach but never reach 1.0.
    let last = ANCHORS[ANCHORS.len() - 1];
    (last.1 + (iso.log2() - last.0.log2()) * 0.05).clamp(0.0, 0.98)
}

/// Convert raw RGB white-balance coefficients into RawTherapee's
/// Temperature/Green parameterisation.
///
/// Approximate by design: RawTherapee re-derives its own multipliers from the
/// camera profile, so this needs to land in the right neighbourhood, not be
/// colorimetrically exact.
pub fn coeffs_to_temp_green(r: f32, g: f32, b: f32) -> (f32, f32) {
    let r = r.max(LOG_FLOOR);
    let g = g.max(LOG_FLOOR);
    let b = b.max(LOG_FLOOR);
    // A high red multiplier means the scene was blue (cool) and needs warming.
    let ratio = (b / r).max(LOG_FLOOR);
    let temp = (5000.0 * ratio.powf(0.85)).clamp(1500.0, 25000.0);
    // Green sits between the two chroma channels.
    let green = (g / (r * b).sqrt()).clamp(0.02, 5.0);
    (temp, green)
}

/// Angle between two coefficient vectors, in radians. The standard measure for
/// comparing illuminant estimates, since only the direction carries colour.
fn angular_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na = (a.iter().map(|x| x * x).sum::<f32>()).sqrt();
    let nb = (b.iter().map(|x| x * x).sum::<f32>()).sqrt();
    if na < LOG_FLOOR || nb < LOG_FLOOR {
        return 0.0;
    }
    (dot / (na * nb)).clamp(-1.0, 1.0).acos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_stats() -> RawStats {
        RawStats {
            p1: 0.01,
            p50: 0.18,
            p99: 0.90,
            p999: 0.95,
            clipped_frac: 0.0,
            black_frac: 0.0,
            wb_r: 2.0,
            wb_g: 1.0,
            wb_b: 1.5,
            illum_r: None,
            illum_g: None,
            illum_b: None,
        }
    }

    fn exif_at_iso(iso: u32) -> ExifData {
        ExifData {
            iso: Some(iso),
            lens_model: Some("FE 24-70mm F2.8 GM".into()),
            ..Default::default()
        }
    }

    fn sharp(s: f32) -> Sharpness {
        Sharpness { s_global: s }
    }

    /// A correctly exposed frame needs no correction: p50 already sits at
    /// middle grey and p99 leaves headroom.
    #[test]
    fn correct_exposure_is_left_alone() {
        let r = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev.abs() < 0.05, "ev was {}", r.exposure_ev);
    }

    /// An underexposed frame is lifted — but never past clipping. p50 at 0.045
    /// wants +2 EV; p99 at 0.5 only allows +0.93.
    #[test]
    fn lift_is_bounded_by_available_headroom() {
        let mut s = neutral_stats();
        s.p50 = 0.045;
        s.p99 = 0.5;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let headroom = (0.95f32 / 0.5).log2();
        assert!(
            (r.exposure_ev - headroom).abs() < 0.01,
            "expected the lift clamped to headroom {headroom}, got {}",
            r.exposure_ev
        );
    }

    /// An overexposed frame is pulled down: headroom goes negative.
    #[test]
    fn overexposure_produces_negative_ev() {
        let mut s = neutral_stats();
        s.p50 = 0.5;
        s.p99 = 1.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(
            r.exposure_ev < 0.0,
            "ev should be negative, was {}",
            r.exposure_ev
        );
    }

    /// REGRESSION GUARD. A dark frame containing a few blown specular
    /// highlights must still be lifted. These are the real measured values from
    /// `example-pictures/DSC03073.ARW`: p999 is fully saturated at 1.0 because
    /// 2.1% of pixels clip, but p99 shows plenty of headroom remains.
    ///
    /// Deriving headroom from p999 — as the original spec §6 did — emitted
    /// -0.07 EV here and threw away a wanted +1.62 EV lift. Any change that
    /// reintroduces a p999-based headroom will fail this test.
    #[test]
    fn saturated_p999_does_not_suppress_the_lift() {
        let mut s = neutral_stats();
        s.p50 = 0.05866;
        s.p99 = 0.42;
        s.p999 = 1.0;
        s.clipped_frac = 0.0212;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(
            r.exposure_ev > 1.0,
            "a dark frame with a few clipped highlights must still be lifted, got {}",
            r.exposure_ev
        );
    }

    /// Exposure is hard-clamped to ±3 EV whatever the measurements say.
    #[test]
    fn exposure_is_clamped_to_three_stops() {
        let mut s = neutral_stats();
        s.p50 = 0.0001;
        s.p99 = 0.001;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev <= 3.0, "ev was {}", r.exposure_ev);

        let mut s2 = neutral_stats();
        s2.p50 = 0.99;
        s2.p99 = 1.0;
        let r2 = decide(&s2, &exif_at_iso(100), &sharp(0.5));
        assert!(r2.exposure_ev >= -3.0, "ev was {}", r2.exposure_ev);
    }

    /// Degenerate percentiles must not produce NaN — log2(0) is -inf and would
    /// poison every downstream clamp and the .pp3 text.
    #[test]
    fn zero_percentiles_do_not_produce_nan() {
        let mut s = neutral_stats();
        s.p50 = 0.0;
        s.p99 = 0.0;
        s.p999 = 0.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev.is_finite(), "ev was {}", r.exposure_ev);
        assert!(r.shadow_lift.is_finite());
    }

    /// Denoise rises monotonically with ISO across the spec's anchor points.
    #[test]
    fn denoise_is_monotone_in_iso() {
        let isos = [100u32, 400, 1600, 6400, 25600, 102400];
        let mut prev_l = -1.0f32;
        let mut prev_c = -1.0f32;
        for iso in isos {
            let r = decide(&neutral_stats(), &exif_at_iso(iso), &sharp(0.5));
            assert!(r.denoise_luma >= prev_l, "luma dropped at ISO {iso}");
            assert!(r.denoise_chroma >= prev_c, "chroma dropped at ISO {iso}");
            assert!((0.0..=1.0).contains(&r.denoise_luma));
            assert!((0.0..=1.0).contains(&r.denoise_chroma));
            prev_l = r.denoise_luma;
            prev_c = r.denoise_chroma;
        }
    }

    /// The spec's anchors, checked at the anchor points themselves.
    #[test]
    fn denoise_hits_the_specified_anchors() {
        for (iso, expected) in [(100u32, 0.0f32), (1600, 0.3), (6400, 0.6), (25600, 0.85)] {
            let r = decide(&neutral_stats(), &exif_at_iso(iso), &sharp(0.5));
            assert!(
                (r.denoise_luma - expected).abs() < 0.02,
                "ISO {iso}: expected ~{expected}, got {}",
                r.denoise_luma
            );
        }
    }

    /// Missing ISO must not panic; treat it as base ISO.
    #[test]
    fn missing_iso_falls_back_to_base() {
        let exif = ExifData::default();
        let r = decide(&neutral_stats(), &exif, &sharp(0.5));
        assert_eq!(r.denoise_luma, 0.0);
    }

    /// Shadow lift is throttled at high ISO: lifting shadows there only
    /// reveals noise, so the ceiling falls as ISO rises.
    #[test]
    fn shadow_lift_ceiling_falls_with_iso() {
        let mut s = neutral_stats();
        s.p1 = 0.0;
        s.black_frac = 0.05;
        let low = decide(&s, &exif_at_iso(100), &sharp(0.5)).shadow_lift;
        let high = decide(&s, &exif_at_iso(25600), &sharp(0.5)).shadow_lift;
        assert!(
            low > high,
            "low-ISO lift {low} should exceed high-ISO {high}"
        );
        assert!((0.0..=1.0).contains(&high));
    }

    /// Highlight recovery scales with how much actually clipped.
    #[test]
    fn highlight_recovery_scales_with_clipping() {
        let mut none = neutral_stats();
        none.clipped_frac = 0.0;
        let mut heavy = neutral_stats();
        heavy.clipped_frac = 0.20;
        let r_none = decide(&none, &exif_at_iso(100), &sharp(0.5));
        let r_heavy = decide(&heavy, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(r_none.highlight_recovery, 0.0);
        assert!(r_heavy.highlight_recovery > 0.5);
        assert!(r_heavy.highlight_recovery <= 1.0);
    }

    /// A soft frame is never sharpened into crunch.
    #[test]
    fn soft_frames_are_not_over_sharpened() {
        let soft = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.05));
        let crisp = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.95));
        assert!(soft.sharpen_amount < crisp.sharpen_amount);
        assert!(
            crisp.sharpen_amount <= 0.8,
            "hard cap breached: {}",
            crisp.sharpen_amount
        );
    }

    /// Lens correction is on only when EXIF names a lens; RawTherapee no-ops
    /// if its own lensfun lookup fails.
    #[test]
    fn lens_correction_follows_exif_lens_presence() {
        let with = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(with.lens_correct);
        let without = decide(&neutral_stats(), &ExifData::default(), &sharp(0.5));
        assert!(!without.lens_correct);
    }

    /// With no illuminant estimate, as-shot coefficients are kept.
    #[test]
    fn absent_illuminant_keeps_as_shot_wb() {
        let r = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        let as_shot = coeffs_to_temp_green(2.0, 1.0, 1.5);
        assert!((r.wb_temp_k - as_shot.0).abs() < 1.0);
        assert!((r.wb_green - as_shot.1).abs() < 0.001);
    }

    /// An illuminant estimate that agrees with as-shot changes nothing.
    #[test]
    fn agreeing_illuminant_keeps_as_shot_wb() {
        let mut s = neutral_stats();
        // Same direction as the as-shot coefficients, so angular distance ~0.
        s.illum_r = Some(0.5);
        s.illum_g = Some(1.0);
        s.illum_b = Some(0.667);
        let agreed = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let as_shot = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!((agreed.wb_temp_k - as_shot.wb_temp_k).abs() < 50.0);
    }

    /// A wildly disagreeing estimate means mixed or artificial light, where
    /// neither estimator is trustworthy — blend 50/50 rather than commit.
    #[test]
    fn disagreeing_illuminant_blends_halfway() {
        let mut s = neutral_stats();
        s.illum_r = Some(1.0);
        s.illum_g = Some(1.0);
        s.illum_b = Some(0.1);
        let blended = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let as_shot = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(
            (blended.wb_temp_k - as_shot.wb_temp_k).abs() > 50.0,
            "a disagreeing estimate should move the temperature"
        );
    }

    /// The recipe hash is what idempotency keys on: identical recipes must
    /// hash identically, and any field change must move it.
    #[test]
    fn recipe_hash_is_stable_and_sensitive() {
        let a = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        let b = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert_eq!(a.recipe_hash(), b.recipe_hash());
        let mut c = a.clone();
        c.exposure_ev += 0.5;
        assert_ne!(a.recipe_hash(), c.recipe_hash());
    }
}
