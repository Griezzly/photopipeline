//! Illuminant estimation by principal component analysis of bright pixels
//! (Cheng et al., 2014, "Illuminant Estimation for Color Constancy: Why
//! spatial-domain methods work and the role of the color distribution").
//!
//! The insight: in a linear RGB scene the brightest chromatic pixels line up
//! along the illuminant direction, so the first principal component of that
//! subset *is* the illuminant. Cheap, no training, no model file.
//!
//! Fails soft by design. This is a cross-check on the camera's own white
//! balance, and the camera is usually right — returning `None` costs nothing
//! because nothing downstream currently blends against it; the estimate is
//! recorded purely as an audit record and input for a future WB-override
//! spec.

/// Fraction of the brightest pixels to run the PCA over.
const BRIGHT_FRACTION: f32 = 0.035;
/// Below this many usable pixels the principal component is noise.
const MIN_PIXELS: usize = 32;
/// Reject pixels at or above this: clipped channels have lost their ratios.
const CLIP_CEILING: f32 = 0.99;
/// Reject pixels below this: read noise dominates and the direction is random.
const BLACK_FLOOR: f32 = 0.01;

/// Estimate the scene illuminant as a unit RGB direction.
///
/// `pixels` must be linear RGB in 0..1. Returns `None` when the frame carries
/// no usable colour information.
pub fn estimate_illuminant(pixels: &[[f32; 3]]) -> Option<[f32; 3]> {
    // Keep only well-exposed, finite pixels.
    let mut usable: Vec<[f32; 3]> = pixels
        .iter()
        .copied()
        .filter(|p| {
            p.iter().all(|v| v.is_finite())
                && p.iter().all(|v| *v < CLIP_CEILING)
                && p.iter().any(|v| *v > BLACK_FLOOR)
        })
        .collect();
    if usable.len() < MIN_PIXELS {
        return None;
    }

    // Brightest first; the illuminant direction is clearest in the highlights.
    usable.sort_by(|a, b| {
        let sa: f32 = a.iter().sum();
        let sb: f32 = b.iter().sum();
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let take = ((usable.len() as f32 * BRIGHT_FRACTION) as usize).max(MIN_PIXELS);
    let bright = &usable[..take.min(usable.len())];

    // First principal component by power iteration on the 3×3 scatter matrix.
    // Uncentred on purpose: we want the direction from the origin, which is
    // what the illuminant is, not the direction of maximum variance.
    let mut scatter = [[0.0f64; 3]; 3];
    for p in bright {
        for i in 0..3 {
            for j in 0..3 {
                scatter[i][j] += (p[i] as f64) * (p[j] as f64);
            }
        }
    }

    let mut v = [1.0f64, 1.0, 1.0];
    for _ in 0..64 {
        let mut next = [0.0f64; 3];
        for i in 0..3 {
            for j in 0..3 {
                next[i] += scatter[i][j] * v[j];
            }
        }
        let norm = (next[0] * next[0] + next[1] * next[1] + next[2] * next[2]).sqrt();
        if !norm.is_finite() || norm < 1e-12 {
            return None;
        }
        for x in next.iter_mut() {
            *x /= norm;
        }
        v = next;
    }

    // The component may come out negated; an illuminant is positive.
    if v.iter().sum::<f64>() < 0.0 {
        for x in v.iter_mut() {
            *x = -*x;
        }
    }
    let out = [v[0] as f32, v[1] as f32, v[2] as f32];
    if out.iter().any(|x| !x.is_finite() || *x <= 0.0) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey scene under a neutral light: the estimate is neutral.
    #[test]
    fn neutral_scene_gives_neutral_illuminant() {
        let px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v, v, v]
            })
            .collect();
        let e = estimate_illuminant(&px).expect("neutral scene should estimate");
        assert!((e[0] - e[1]).abs() < 0.05, "not neutral: {e:?}");
        assert!((e[1] - e[2]).abs() < 0.05, "not neutral: {e:?}");
    }

    /// The same scene under a warm light: the estimate leans red.
    #[test]
    fn warm_cast_is_detected() {
        let px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v * 1.6, v, v * 0.6]
            })
            .collect();
        let e = estimate_illuminant(&px).expect("cast scene should estimate");
        assert!(e[0] > e[1], "red should dominate: {e:?}");
        assert!(e[1] > e[2], "blue should be weakest: {e:?}");
    }

    /// The result is a unit vector — only direction carries colour.
    #[test]
    fn estimate_is_normalised() {
        let px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v * 1.6, v, v * 0.6]
            })
            .collect();
        let e = estimate_illuminant(&px).unwrap();
        let norm = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }

    /// Too few usable pixels: fail soft rather than guess.
    #[test]
    fn insufficient_pixels_return_none() {
        assert!(estimate_illuminant(&[]).is_none());
        assert!(estimate_illuminant(&[[0.5, 0.5, 0.5]]).is_none());
    }

    /// An entirely clipped or entirely black frame carries no colour
    /// information; both must return None rather than a degenerate direction.
    #[test]
    fn degenerate_frames_return_none() {
        let white = vec![[1.0f32, 1.0, 1.0]; 500];
        assert!(estimate_illuminant(&white).is_none());
        let black = vec![[0.0f32, 0.0, 0.0]; 500];
        assert!(estimate_illuminant(&black).is_none());
    }

    /// Non-finite input must never escape into the result — a future
    /// consumer will take logs and reciprocals of these values.
    #[test]
    fn non_finite_pixels_are_rejected() {
        let mut px: Vec<[f32; 3]> = (1..200)
            .map(|i| {
                let v = i as f32 / 200.0;
                [v, v, v]
            })
            .collect();
        px.push([f32::NAN, 1.0, 1.0]);
        px.push([f32::INFINITY, 1.0, 1.0]);
        let e = estimate_illuminant(&px).expect("the valid majority should still estimate");
        assert!(e.iter().all(|v| v.is_finite()), "non-finite escaped: {e:?}");
    }
}
