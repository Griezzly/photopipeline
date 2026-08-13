//! Trilinear application of a 3D LUT.
//!
//! Done in Rust rather than by RawTherapee's Film Simulation tool, which
//! locates HaldCLUTs through a GUI preferences directory and has no documented
//! `.pp3` key for selecting one by path. Driving it headlessly would mean
//! writing into RawTherapee's own config. Applying here also puts the LUT in
//! exactly the domain it was trained on — sRGB (spec section 4).

use image::DynamicImage;

use crate::develop::lut::{Lut33, LUT_DIM};

/// Apply `lut` to every pixel of `img`.
///
/// Input is read at 16-bit and output at 16-bit, so the headroom the baseline
/// render preserved survives into the JPEG encoder's input.
pub fn apply_lut(img: &DynamicImage, lut: &Lut33) -> DynamicImage {
    let src = img.to_rgb16();
    let (w, h) = (src.width(), src.height());
    let mut out = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(w, h);

    for (dst_px, src_px) in out.pixels_mut().zip(src.pixels()) {
        let rgb = [
            src_px.0[0] as f32 / 65535.0,
            src_px.0[1] as f32 / 65535.0,
            src_px.0[2] as f32 / 65535.0,
        ];
        let looked = sample(lut, rgb);
        *dst_px = image::Rgb([
            (looked[0].clamp(0.0, 1.0) * 65535.0).round() as u16,
            (looked[1].clamp(0.0, 1.0) * 65535.0).round() as u16,
            (looked[2].clamp(0.0, 1.0) * 65535.0).round() as u16,
        ]);
    }
    DynamicImage::ImageRgb16(out)
}

/// Trilinear interpolation of one RGB triple through the lattice.
fn sample(lut: &Lut33, rgb: [f32; 3]) -> [f32; 3] {
    let last = (LUT_DIM - 1) as f32;
    // Position in lattice coordinates, split into cell index and fraction.
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    let mut frac = [0f32; 3];
    for c in 0..3 {
        let pos = (rgb[c].clamp(0.0, 1.0) * last).clamp(0.0, last);
        let f = pos.floor();
        lo[c] = f as usize;
        hi[c] = (lo[c] + 1).min(LUT_DIM - 1);
        frac[c] = pos - f;
    }

    // Eight corners of the enclosing cell, weighted by the complement of each
    // axis fraction. Standard trilinear interpolation.
    let mut acc = [0f32; 3];
    for corner in 0..8 {
        let (ir, wr) = pick(corner, 0, &lo, &hi, &frac);
        let (ig, wg) = pick(corner, 1, &lo, &hi, &frac);
        let (ib, wb) = pick(corner, 2, &lo, &hi, &frac);
        let weight = wr * wg * wb;
        if weight == 0.0 {
            continue;
        }
        let base = ((ib * LUT_DIM + ig) * LUT_DIM + ir) * 3;
        for (c, a) in acc.iter_mut().enumerate() {
            *a += weight * lut.data[base + c];
        }
    }
    acc
}

/// For corner `corner`, axis `axis`: which lattice index and what weight.
fn pick(
    corner: usize,
    axis: usize,
    lo: &[usize; 3],
    hi: &[usize; 3],
    frac: &[f32; 3],
) -> (usize, f32) {
    if corner & (1 << axis) == 0 {
        (lo[axis], 1.0 - frac[axis])
    } else {
        (hi[axis], frac[axis])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::develop::lut::Lut33;
    use image::{DynamicImage, Rgb, RgbImage};

    fn test_image() -> DynamicImage {
        let mut img = RgbImage::new(4, 4);
        for (i, px) in img.pixels_mut().enumerate() {
            let v = (i * 16) as u8;
            *px = Rgb([v, 255 - v, 128]);
        }
        DynamicImage::ImageRgb8(img)
    }

    /// The identity LUT round-trips an image bit-exactly. This is the single
    /// most important property: any interpolation error shows up here.
    #[test]
    fn identity_lut_round_trips_bit_exactly() {
        let src = test_image();
        let out = apply_lut(&src, &Lut33::identity());
        assert_eq!(out.to_rgb8().into_raw(), src.to_rgb8().into_raw());
    }

    /// A LUT that maps everything to black produces black, proving the lookup
    /// is actually consulted rather than the source being passed through.
    #[test]
    fn constant_lut_maps_everything_to_its_constant() {
        let mut black = Lut33::identity();
        black.data.iter_mut().for_each(|v| *v = 0.0);
        let out = apply_lut(&test_image(), &black);
        assert!(out.to_rgb8().into_raw().iter().all(|v| *v == 0));
    }

    /// An inverting LUT matches an independently computed reference.
    #[test]
    fn inverting_lut_matches_a_reference_computation() {
        let mut inv = Lut33::identity();
        inv.data.iter_mut().for_each(|v| *v = 1.0 - *v);
        let src = test_image();
        let out = apply_lut(&src, &inv);
        let src_raw = src.to_rgb8().into_raw();
        for (i, got) in out.to_rgb8().into_raw().iter().enumerate() {
            let want = 255 - src_raw[i];
            assert!(
                (*got as i16 - want as i16).abs() <= 1,
                "index {i}: got {got}, want {want}"
            );
        }
    }

    /// Dimensions and channel count survive.
    #[test]
    fn geometry_is_preserved() {
        let out = apply_lut(&test_image(), &Lut33::identity());
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 4);
    }

    /// A 16-bit source is accepted — the TIFF RawTherapee emits is 16-bit,
    /// and applying the look at 8-bit would throw away the headroom the
    /// baseline render exists to preserve.
    #[test]
    fn sixteen_bit_input_is_handled() {
        let mut img = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(2, 2);
        for px in img.pixels_mut() {
            *px = image::Rgb([65535u16, 0, 32768]);
        }
        let out = apply_lut(&DynamicImage::ImageRgb16(img), &Lut33::identity());
        assert_eq!(out.width(), 2);
    }
}
