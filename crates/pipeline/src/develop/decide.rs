//! The decision layer: a pure function from measurements to an `EditRecipe`.
//!
//! No image data, no I/O, no database. That is the single most important
//! testability property in this design — the tuning-sensitive logic is
//! exercised by table-driven unit tests over numbers, without fixtures.

use crate::develop::measure::RawStats;
use crate::ingest::exif::ExifData;

/// Bumped whenever any formula below changes. Stored in `edits.decider_version`
/// and part of the idempotency key, so a tuning change re-renders everything.
pub const DECIDER_VERSION: &str = "decide-3";

/// The sharpness measurement `decide` consumes. A struct rather than a bare f32
/// so adding subject/background terms later does not churn the signature.
#[derive(Debug, Clone, Copy)]
pub struct Sharpness {
    /// Where this frame's sharpness sits among comparable frames, 0..1.
    ///
    /// This is a *relative position*, not a raw score, and the distinction is
    /// load-bearing. The previous field was `s_global` and was documented as
    /// "roughly 0..1", but `defect::blur` computes it as the variance of the
    /// Laplacian, which is unbounded — real frames measure in the hundreds or
    /// thousands (128, 357 and 1491 on the three sample ARWs). Clamping that to
    /// 0..1 saturated every real photo to 1.0, so `sharpen_amount` was always
    /// exactly `SHARPEN_MAX` and the modulation below was dead in production
    /// while every unit test passed, because the tests fed fabricated 0..1
    /// values no photo produces.
    ///
    /// Computed by [`relative_sharpness`] against the calibrated
    /// `sharpness_baseline` percentiles, which is the same comparison the blur
    /// flagger already makes. `decide` stays pure: the caller does the lookup.
    pub s_relative: f32,
}

/// The relative sharpness to assume when no baseline comparison is possible —
/// a fresh library, a bucket below `min_samples_for_bucket` with no global
/// sentinel yet, or a frame whose `s_subject` is missing. Deliberately neutral:
/// half sharpening rather than either extreme, since an unknown frame is as
/// likely to be crisp as soft.
pub const NEUTRAL_RELATIVE_SHARPNESS: f32 = 0.5;

/// Map a raw sharpness score to its 0..1 position within a calibrated range.
///
/// `p10`/`p90` come from `sharpness_baseline` for this camera/lens/focal/
/// aperture bucket, or from the global sentinel row. A frame at or below the
/// 10th percentile scores 0 (soft for this lens — sharpen gently), one at or
/// above the 90th scores 1 (as crisp as this lens gets — sharpen fully).
///
/// Returns [`NEUTRAL_RELATIVE_SHARPNESS`] when the range is degenerate, which
/// happens when every sample in a bucket measured the same. Comparing against
/// percentiles of `s_subject` means the caller must pass `s_subject`, not
/// `s_global`: mixing the two would repeat the unit error this replaced.
pub fn relative_sharpness(s_subject: f32, p10: f32, p90: f32) -> f32 {
    let span = p90 - p10;
    if span <= f32::EPSILON {
        return NEUTRAL_RELATIVE_SHARPNESS;
    }
    ((s_subject - p10) / span).clamp(0.0, 1.0)
}

/// A complete, renderer-agnostic development recipe. Every field is normalised
/// 0..1 except `exposure_ev`, which is in stops.
///
/// Carries NO white-balance fields. v1 emits RawTherapee's `Setting=Camera`,
/// which applies the camera's own as-shot coefficients exactly. See `decide()`.
#[derive(Debug, Clone, PartialEq)]
pub struct EditRecipe {
    pub exposure_ev: f32,
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
    let wanted = lift.min(headroom).clamp(-3.0, 3.0);
    // Soft deadband. Modern camera metering is good, and the CHECKPOINT review
    // showed the unguarded correction actively made well-metered frames worse:
    // on three real frames the baseline render beat ours wherever a lift was
    // applied, because `MID_GREY` is a grey-card reflectance target and the
    // median of a scene full of dark conifers is legitimately far below it.
    // So correct outliers, not every frame — the same reasoning already applied
    // to white balance.
    //
    // Subtracting the deadband rather than thresholding on it keeps the response
    // continuous: a frame wanting 0.76 EV gets 0.01 rather than jumping to 0.76.
    // It is also inherently conservative, since the applied correction is always
    // smaller than the computed one.
    let exposure_ev = deadband(wanted, EXPOSURE_DEADBAND_EV);

    // ── white balance ──
    // Nothing to decide. v1 emits RawTherapee's `Setting=Camera`, so the camera's
    // own as-shot coefficients are applied exactly and no conversion error can
    // enter. `EditRecipe` therefore carries no white-balance field at all.
    //
    // An earlier revision converted the as-shot coefficients into RawTherapee's
    // Temperature/Green parameterisation and got it wrong twice: the temperature
    // relation was inverted (tungsten -> 8214 K, daylight -> 3713 K) and Green
    // landed near 0.5 against its 1.0 neutral, casting every frame magenta.
    //
    // The PCA illuminant estimate in `raw.illum_*` is measured and persisted for
    // the audit record but deliberately not acted on: overriding the camera needs
    // a conversion we can verify, which is deferred to its own spec.

    // ── highlight recovery ──
    // Scales with how much actually clipped. 5% clipped is already severe.
    let highlight_recovery = (raw.clipped_frac / 0.05).clamp(0.0, 1.0);

    // ── shadow lift ──
    // Driven only by genuinely crushed blacks, with its own deadband, and
    // throttled by a noise penalty since lifting shadows at high ISO only
    // reveals noise.
    //
    // The `p1` term this replaced was the same category of error as the exposure
    // target: a low 1st percentile means the scene *has* deep shadows, not that
    // it is broken. On a real frame p1 = 0.0018 drove shadow_lift to 0.46, which
    // flattened the image badly. `black_frac` already measures what matters —
    // how much actually hit the black level — so the deadband keys on that alone.
    let shadow_demand = ((raw.black_frac - SHADOW_DEADBAND_FRAC) / 0.045).clamp(0.0, 1.0);
    let noise_penalty = 1.0 - denoise_curve(iso);
    let shadow_lift = (shadow_demand * 0.5 * noise_penalty).clamp(0.0, 1.0);

    // ── denoise ──
    let denoise_luma = denoise_curve(iso);
    // Chroma noise is more objectionable and cheaper to remove than luma.
    let denoise_chroma = (denoise_luma * 1.2).clamp(0.0, 1.0);

    // ── sharpening ──
    // Modulated by measured sharpness and hard-capped, so a genuinely soft
    // frame is never sharpened into crunch. The cap is structural: the input is
    // clamped to 0..1 and then scaled, so the product cannot exceed SHARPEN_MAX.
    // An outer `.clamp(0.0, SHARPEN_MAX)` here would be a no-op and would make
    // the cap untestable, since removing it could not change any result.
    //
    // The clamp is a guard, not the normalisation: `s_relative` arrives already
    // 0..1 from `relative_sharpness`. It used to be handed a raw variance of
    // the Laplacian, which the clamp silently saturated to 1.0 on every real
    // photo — see `Sharpness::s_relative`.
    let sharpen_amount = sharp.s_relative.clamp(0.0, 1.0) * SHARPEN_MAX;

    EditRecipe {
        exposure_ev,
        highlight_recovery,
        shadow_lift,
        denoise_luma,
        denoise_chroma,
        sharpen_amount,
        lens_correct: exif.lens_model.is_some(),
    }
}

/// Exposure corrections smaller than this are not applied at all; larger ones
/// have it subtracted. Tuned at the CHECKPOINT against real frames.
const EXPOSURE_DEADBAND_EV: f32 = 0.75;

/// Shadow lift stays at zero until at least this fraction of the frame has
/// actually hit the black level.
const SHADOW_DEADBAND_FRAC: f32 = 0.005;

/// Subtract `dz` from the magnitude of `v`, preserving sign, floored at zero.
/// Continuous, and always returns something no larger than `v`.
fn deadband(v: f32, dz: f32) -> f32 {
    let out = (v.abs() - dz).max(0.0);
    if v.is_sign_negative() {
        -out
    } else {
        out
    }
}

/// Ceiling on capture-sharpening strength. Applied as a scale factor rather
/// than an outer clamp so the bound is structural and a change to it is
/// observable in the tests.
const SHARPEN_MAX: f32 = 0.8;

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
        Sharpness { s_relative: s }
    }

    /// A correctly exposed frame needs no correction: p50 already sits at
    /// middle grey and p99 leaves headroom.
    #[test]
    fn correct_exposure_is_left_alone() {
        let r = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        assert!(r.exposure_ev.abs() < 0.05, "ev was {}", r.exposure_ev);
    }

    /// An underexposed frame is lifted — but never past clipping, and the
    /// result still has the deadband subtracted. p50 at 0.045 wants +2 EV;
    /// p99 at 0.5 only allows +0.93, and the deadband takes off a further
    /// 0.75, leaving ~0.176.
    #[test]
    fn lift_is_bounded_by_available_headroom() {
        let mut s = neutral_stats();
        s.p50 = 0.045;
        s.p99 = 0.5;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let headroom = (0.95f32 / 0.5).log2();
        let expected = deadband(headroom, EXPOSURE_DEADBAND_EV);
        assert!(
            (r.exposure_ev - expected).abs() < 0.01,
            "expected the headroom-bound lift {headroom} minus the deadband ({expected}), got {}",
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
    ///
    /// `p99 = 0.42` is a constructed value, not the real measurement (the real
    /// frame's p99 is high enough that the deadband zeroes the correction — see
    /// the worked example in the plan). With the deadband in place the expected
    /// output is the headroom-bound lift minus `EXPOSURE_DEADBAND_EV`, not the
    /// raw headroom itself.
    #[test]
    fn saturated_p999_does_not_suppress_the_lift() {
        let mut s = neutral_stats();
        s.p50 = 0.05866;
        s.p99 = 0.42;
        s.p999 = 1.0;
        s.clipped_frac = 0.0212;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let headroom = (0.95f32 / 0.42).log2();
        let expected = deadband(headroom, EXPOSURE_DEADBAND_EV);
        assert!(
            (r.exposure_ev - expected).abs() < 0.01,
            "expected {expected} (headroom {headroom} minus the deadband), got {}",
            r.exposure_ev
        );
        assert!(
            r.exposure_ev > 0.0,
            "a dark frame with a few clipped highlights must still be lifted, got {}",
            r.exposure_ev
        );
    }

    /// The +3 EV clamp on `wanted` is reachable and must bind: a nearly black
    /// frame wants far more than three stops. The deadband is then subtracted
    /// from the clamped value, so the final result is `3.0 -
    /// EXPOSURE_DEADBAND_EV`, not `3.0` — proving the clamp bound first.
    #[test]
    fn extreme_underexposure_is_clamped_to_three_stops() {
        let mut s = neutral_stats();
        s.p50 = 0.0001;
        s.p99 = 0.001;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let expected = 3.0 - EXPOSURE_DEADBAND_EV;
        assert!(
            (r.exposure_ev - expected).abs() < 1e-4,
            "expected the upper clamp (3.0) minus the deadband = {expected}, got {}",
            r.exposure_ev
        );
    }

    /// The -3 EV clamp on `wanted` is UNREACHABLE by construction, and this
    /// test records why so nobody mistakes it for tested behaviour.
    /// Percentiles are normalised to 0..=1, so the most negative `lift`
    /// obtainable is log2(0.18 / 1.0) = -2.474 EV, and `headroom` only ever
    /// raises the minimum further. The clamp stays as defence-in-depth against
    /// a future change to the percentile range; what is asserted here is the
    /// real reachable floor after the deadband is subtracted.
    #[test]
    fn maximum_pull_down_is_the_reachable_floor_not_the_clamp() {
        let mut s = neutral_stats();
        s.p50 = 1.0;
        s.p99 = 1.0;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        let reachable_wanted = (0.18f32 / 1.0).log2(); // -2.474
        let reachable_floor = deadband(reachable_wanted, EXPOSURE_DEADBAND_EV);
        assert!(
            (r.exposure_ev - reachable_floor).abs() < 0.01,
            "expected the reachable floor {reachable_floor}, got {}",
            r.exposure_ev
        );
        assert!(
            r.exposure_ev > -3.0,
            "the -3 clamp should never be what binds"
        );
    }

    /// The whole point of the deadband: a frame close to correctly exposed
    /// gets exactly zero exposure correction, not a small nudge. `wanted` here
    /// is 0.5 EV, comfortably inside the 0.75 EV deadband.
    #[test]
    fn near_correct_frame_gets_exactly_zero_exposure_correction() {
        let mut s = neutral_stats();
        s.p50 = 0.18 / 2.0f32.powf(0.5); // lift = +0.5 EV
        s.p99 = 0.6; // headroom = log2(0.95/0.6) ~= 0.66, does not bind
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(
            r.exposure_ev, 0.0,
            "expected exactly zero, got {}",
            r.exposure_ev
        );
    }

    /// The deadband is continuous, not a hard threshold: a frame whose wanted
    /// correction sits just past the 0.75 EV boundary gets a small non-zero
    /// result rather than jumping straight to the full 0.76 EV.
    #[test]
    fn deadband_is_continuous_past_the_boundary() {
        let mut s = neutral_stats();
        s.p50 = 0.18 / 2.0f32.powf(0.76); // lift = +0.76 EV, just past the deadband
        s.p99 = 0.5; // headroom = log2(0.95/0.5) ~= 0.93, does not bind
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(
            r.exposure_ev > 0.0 && r.exposure_ev < 0.02,
            "expected a small non-zero nudge just past the boundary, got {}",
            r.exposure_ev
        );
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

    /// NEGATIVE percentiles are the case the LOG_FLOOR guards actually carry:
    /// `log2` of a negative number is NaN, and unlike an infinity a NaN
    /// survives `clamp`. Zeroed percentiles alone do not prove the guards are
    /// load-bearing, because they produce an infinity that the trailing clamp
    /// would rescue on its own.
    #[test]
    fn negative_percentiles_do_not_produce_nan() {
        let mut s = neutral_stats();
        s.p1 = -0.5;
        s.p50 = -0.5;
        s.p99 = -0.5;
        s.p999 = -0.5;
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        for (name, v) in [
            ("exposure_ev", r.exposure_ev),
            ("highlight_recovery", r.highlight_recovery),
            ("shadow_lift", r.shadow_lift),
            ("denoise_luma", r.denoise_luma),
            ("sharpen_amount", r.sharpen_amount),
        ] {
            assert!(v.is_finite(), "{name} was {v}");
        }
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

    /// Below the shadow deadband, black_frac alone must not trigger any lift
    /// — a scene with a genuinely tiny fraction of true blacks is not broken.
    #[test]
    fn shadow_lift_is_zero_below_the_deadband() {
        let mut s = neutral_stats();
        s.p1 = 0.01;
        s.black_frac = 0.003; // below SHADOW_DEADBAND_FRAC = 0.005
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(r.shadow_lift, 0.0, "shadow_lift was {}", r.shadow_lift);
    }

    /// Above the shadow deadband, black_frac drives a non-zero lift.
    #[test]
    fn shadow_lift_is_non_zero_above_the_deadband() {
        let mut s = neutral_stats();
        s.p1 = 0.01;
        s.black_frac = 0.01; // above SHADOW_DEADBAND_FRAC = 0.005
        let r = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert!(
            r.shadow_lift > 0.0,
            "expected a non-zero lift above the deadband, got {}",
            r.shadow_lift
        );
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

        // An interior point, so a wrong scale divisor cannot hide behind the
        // clamped extremes: clipped_frac 0.025 against the 0.05 scale is 0.5.
        let mut mid = neutral_stats();
        mid.clipped_frac = 0.025;
        let r_mid = decide(&mid, &exif_at_iso(100), &sharp(0.5));
        assert!(
            (r_mid.highlight_recovery - 0.5).abs() < 0.01,
            "expected 0.5 at the midpoint, got {}",
            r_mid.highlight_recovery
        );
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

    /// The regression the `s_relative` rename exists to prevent. `s_global` is
    /// a variance of the Laplacian, so real frames arrive in the hundreds or
    /// thousands; feeding those straight to `decide` saturated the clamp and
    /// pinned `sharpen_amount` to `SHARPEN_MAX` for every photo. These are the
    /// measured values from the three sample ARWs, mapped through the same
    /// baseline span, and they must stay distinct.
    #[test]
    fn real_scale_sharpness_does_not_saturate_to_the_cap() {
        // Baseline span standing in for a calibrated bucket.
        let (p10, p90) = (128.0, 1491.0);
        let amounts: Vec<f32> = [128.32298f32, 356.9223, 1490.7881]
            .into_iter()
            .map(|s| {
                decide(
                    &neutral_stats(),
                    &exif_at_iso(100),
                    &sharp(relative_sharpness(s, p10, p90)),
                )
                .sharpen_amount
            })
            .collect();

        assert!(
            amounts[0] < amounts[1] && amounts[1] < amounts[2],
            "sharpen_amount must track measured sharpness, got {amounts:?}"
        );
        assert!(
            amounts.iter().filter(|a| **a >= SHARPEN_MAX).count() <= 1,
            "only the frame at/above p90 may reach the cap, got {amounts:?}"
        );
        // The softest frame sits essentially at p10, so it gets the gentlest
        // treatment — near zero, not the 0.8 the old code gave it.
        assert!(amounts[0] < 0.01, "p10 frame got {}", amounts[0]);
    }

    /// A degenerate baseline — every sample in the bucket measured the same —
    /// must not divide by zero or land at an extreme.
    #[test]
    fn a_degenerate_baseline_span_is_neutral() {
        assert_eq!(
            relative_sharpness(500.0, 300.0, 300.0),
            NEUTRAL_RELATIVE_SHARPNESS
        );
    }

    /// Frames outside the calibrated range clamp rather than extrapolating past
    /// the 0..1 contract `decide` relies on.
    #[test]
    fn relative_sharpness_clamps_outside_the_baseline_span() {
        assert_eq!(relative_sharpness(10.0, 128.0, 1491.0), 0.0);
        assert_eq!(relative_sharpness(9000.0, 128.0, 1491.0), 1.0);
        // Midpoint maps to the middle of the range.
        assert!((relative_sharpness(600.0, 100.0, 1100.0) - 0.5).abs() < 1e-6);
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

    /// v1 has no white-balance override: the recipe carries no WB field and the
    /// illuminant estimate must not change any decision. A frame with a wildly
    /// disagreeing illuminant must produce the same recipe as one with none.
    #[test]
    fn illuminant_estimate_does_not_affect_the_recipe() {
        let plain = decide(&neutral_stats(), &exif_at_iso(100), &sharp(0.5));
        let mut s = neutral_stats();
        s.illum_r = Some(1.0);
        s.illum_g = Some(1.0);
        s.illum_b = Some(0.1);
        let with_estimate = decide(&s, &exif_at_iso(100), &sharp(0.5));
        assert_eq!(
            plain, with_estimate,
            "v1 must not act on the illuminant estimate"
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
