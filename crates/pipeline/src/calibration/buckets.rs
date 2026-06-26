//! Snap raw EXIF focal-length / aperture values to canonical calibration
//! buckets. Pure Rust (no DuckDB UDF). See IMPLEMENTATION_PLAN Appendix B.

/// Canonical focal-length buckets in millimetres (IMPLEMENTATION_PLAN App. B).
const FOCAL_BUCKETS: &[i32] = &[14, 18, 24, 28, 35, 50, 70, 85, 105, 135, 200, 300, 400, 600];

/// Snap a focal length (mm) to the nearest canonical bucket.
/// Values below 14 snap to 14; above 600 snap to 600.
pub fn focal_bucket(mm: f32) -> i32 {
    let mut best = FOCAL_BUCKETS[0];
    let mut best_d = (mm - best as f32).abs();
    for &b in &FOCAL_BUCKETS[1..] {
        let d = (mm - b as f32).abs();
        if d < best_d {
            best_d = d;
            best = b;
        }
    }
    best
}

/// Snap an f-number to the nearest 1/3 stop: `2^(round(log2(f) * 3) / 3)`.
/// Non-positive inputs clamp to f/1.0. Result is a float so the composite
/// primary key on `sharpness_baseline` works.
pub fn aperture_bucket(f: f32) -> f32 {
    if f <= 0.0 {
        return 1.0;
    }
    2.0_f32.powf((f.log2() * 3.0).round() / 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focal_exact_bucket_is_itself() {
        assert_eq!(focal_bucket(50.0), 50);
        assert_eq!(focal_bucket(14.0), 14);
        assert_eq!(focal_bucket(600.0), 600);
    }

    #[test]
    fn focal_snaps_to_nearest() {
        // 60 is between 50 and 70; |60-50|=10, |60-70|=10 → ties resolve to the
        // first-seen (50) because we only replace on strictly-smaller distance.
        assert_eq!(focal_bucket(60.0), 50);
        // 61 is closer to 70.
        assert_eq!(focal_bucket(61.0), 70);
        // 30 is closer to 28 (|30-28|=2) than 35 (|30-35|=5).
        assert_eq!(focal_bucket(30.0), 28);
    }

    #[test]
    fn focal_clamps_out_of_range() {
        assert_eq!(focal_bucket(8.0), 14);
        assert_eq!(focal_bucket(10000.0), 600);
    }

    #[test]
    fn aperture_round_trips_canonical_stops() {
        // f/2.0 = 2^(3/3) and f/4.0 = 2^(6/3) are exact canonical 1/3-stop values.
        assert!(
            (aperture_bucket(2.0) - 2.0).abs() < 0.05,
            "got {}",
            aperture_bucket(2.0)
        );
        assert!(
            (aperture_bucket(4.0) - 4.0).abs() < 0.05,
            "got {}",
            aperture_bucket(4.0)
        );
        // 2^(4/3) ≈ 2.5198 is a canonical stop; feeding it back should round-trip.
        let canonical = 2.0_f32.powf(4.0 / 3.0); // ≈ 2.5198
        assert!(
            (aperture_bucket(canonical) - canonical).abs() < 0.001,
            "got {}",
            aperture_bucket(canonical)
        );
    }

    #[test]
    fn aperture_snaps_to_nearest_third_stop() {
        // f/1.7 sits between 2^(1/3)≈1.26 and 2^(2/3)≈1.587; it's closer to 1.587.
        let expected = 2.0_f32.powf(2.0 / 3.0); // ≈ 1.5874
        assert!(
            (aperture_bucket(1.7) - expected).abs() < 0.01,
            "got {}",
            aperture_bucket(1.7)
        );
    }

    #[test]
    fn aperture_idempotent() {
        // Snapping an already-snapped value returns (within f32) itself.
        let once = aperture_bucket(3.3);
        let twice = aperture_bucket(once);
        assert!((once - twice).abs() < 1e-4, "{once} vs {twice}");
    }

    #[test]
    fn aperture_handles_nonpositive() {
        assert_eq!(aperture_bucket(0.0), 1.0);
        assert_eq!(aperture_bucket(-1.0), 1.0);
    }
}
