//! Image-adaptive 3D LUT prediction (Zeng et al.).
//!
//! A predictor CNN under 600K parameters consumes a 256px downsample and emits
//! blend weights over N basis LUTs, which fuse into one image-specific 33³
//! table. Only the predictor is ONNX; the fuse and the apply are ours, because
//! the reference implementation's trilinear step is a custom CUDA extension
//! that will not trace (spec section 7).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::DynamicImage;
use ndarray::Array4;

use crate::develop::lut::{Lut33, LUT_DIM};
use crate::models::LookPredictor;

/// The predictor's fixed input size.
const INPUT_SIZE: u32 = 256;

pub struct Lut3dPredictor {
    session: Mutex<ort::session::Session>,
    basis: Vec<Lut33>,
}

impl Lut3dPredictor {
    pub fn load(onnx: &Path, basis_npy: &Path) -> Result<Self> {
        let basis = read_basis(basis_npy)
            .with_context(|| format!("cannot read basis LUTs from {}", basis_npy.display()))?;
        let session = crate::models::build_session(onnx)
            .with_context(|| format!("cannot load lut3d predictor {}", onnx.display()))?;
        Ok(Self {
            session: Mutex::new(session),
            basis,
        })
    }

    fn preprocess(img: &DynamicImage) -> Array4<f32> {
        let rgb = img
            .resize_exact(
                INPUT_SIZE,
                INPUT_SIZE,
                image::imageops::FilterType::Triangle,
            )
            .to_rgb8();
        let mut arr = Array4::<f32>::zeros((1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize));
        for (x, y, px) in rgb.enumerate_pixels() {
            for c in 0..3 {
                arr[[0, c, y as usize, x as usize]] = px.0[c] as f32 / 255.0;
            }
        }
        arr
    }
}

impl LookPredictor for Lut3dPredictor {
    fn predict(&self, img: &DynamicImage) -> Result<Lut33> {
        let input = Self::preprocess(img);
        let tensor =
            ort::value::Tensor::<f32>::from_array(input).map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;
        let outputs = session
            .run(ort::inputs!["image" => &tensor])
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (_, data) = outputs["weights"]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let weights: Vec<f32> = data.iter().copied().take(self.basis.len()).collect();
        Ok(Lut33::fuse(&self.basis, &weights))
    }

    fn name(&self) -> &str {
        "lut3d-fivek"
    }

    fn version(&self) -> &str {
        "1"
    }
}

/// Read the exporter's `[N, 3, 33, 33, 33]` float32 array.
///
/// A hand-rolled reader rather than a new dependency: the file this consumes is
/// written by our own `tools/export_lut3d.py`, so exactly one `.npy` dialect
/// needs supporting — v1.0, little-endian float32, C order.
pub(crate) fn read_basis(path: &Path) -> Result<Vec<Lut33>> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if bytes.len() < 10 || &bytes[0..6] != b"\x93NUMPY" {
        anyhow::bail!("{} is not a .npy file", path.display());
    }
    let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let header =
        std::str::from_utf8(&bytes[10..10 + header_len]).context("npy header is not UTF-8")?;
    if !header.contains("'<f4'") && !header.contains("\"<f4\"") {
        anyhow::bail!("basis must be little-endian float32, header said: {header}");
    }
    if header.contains("'fortran_order': True") {
        anyhow::bail!("basis must be in C order");
    }

    let shape: Vec<usize> = header
        .split("'shape':")
        .nth(1)
        .and_then(|s| s.split('(').nth(1))
        .and_then(|s| s.split(')').next())
        .map(|s| {
            s.split(',')
                .filter_map(|t| t.trim().parse::<usize>().ok())
                .collect()
        })
        .unwrap_or_default();

    let expected_tail = [3usize, LUT_DIM, LUT_DIM, LUT_DIM];
    if shape.len() != 5 || shape[1..] != expected_tail {
        anyhow::bail!("basis shape {shape:?} is not [N, 3, {LUT_DIM}, {LUT_DIM}, {LUT_DIM}]");
    }

    let n = shape[0];
    let per_lut = 3 * LUT_DIM * LUT_DIM * LUT_DIM;
    let data_start = 10 + header_len;
    let floats: Vec<f32> = bytes[data_start..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if floats.len() < n * per_lut {
        anyhow::bail!(
            "basis payload is short: {} of {}",
            floats.len(),
            n * per_lut
        );
    }

    // The exporter writes [N, 3, B, G, R]; Lut33 wants [(B,G,R) -> RGB] interleaved.
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let base = i * per_lut;
        let plane = LUT_DIM * LUT_DIM * LUT_DIM;
        let mut data = Vec::with_capacity(plane * 3);
        for p in 0..plane {
            for c in 0..3 {
                data.push(floats[base + c * plane + p]);
            }
        }
        out.push(Lut33 { data });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a minimal `.npy` v1.0 file with the given shape and a zero
    /// payload. Only the dialect our own exporter emits needs supporting:
    /// little-endian float32, C order.
    fn write_test_npy(path: &std::path::Path, shape: &[usize]) {
        let dims: Vec<String> = shape.iter().map(|d| d.to_string()).collect();
        let shape_tuple = if shape.len() == 1 {
            format!("({},)", dims[0])
        } else {
            format!("({})", dims.join(", "))
        };
        let mut header =
            format!("{{'descr': '<f4', 'fortran_order': False, 'shape': {shape_tuple}, }}");
        // numpy pads the header with spaces so the payload starts 64-byte
        // aligned, and terminates it with a newline.
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x93NUMPY");
        bytes.push(1); // major
        bytes.push(0); // minor
        bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend(std::iter::repeat_n(
            0u8,
            shape.iter().product::<usize>() * 4,
        ));
        std::fs::write(path, bytes).unwrap();
    }

    /// A missing model file is a typed error naming the path, so the caller
    /// can degrade to baseline-only rather than aborting the run.
    #[test]
    fn missing_files_produce_a_not_found_error() {
        // `expect_err` would need `Debug` on the predictor, which holds an ORT
        // session that does not implement it.
        let Err(err) = Lut3dPredictor::load(
            std::path::Path::new("/nonexistent/lut3d_predictor.onnx"),
            std::path::Path::new("/nonexistent/lut3d_basis.npy"),
        ) else {
            panic!("load should fail for missing files");
        };
        assert!(
            err.to_string().contains("lut3d"),
            "error should name the file: {err}"
        );
    }

    /// The .npy reader accepts the exact shape the exporter writes and
    /// rejects anything else — a silently mis-shaped basis would produce a
    /// plausible but wrong look.
    #[test]
    fn basis_shape_is_validated() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("basis.npy");
        // A well-formed header for the wrong shape.
        write_test_npy(&path, &[2, 3, 17, 17, 17]);
        let err = read_basis(&path).expect_err("wrong lattice size must be rejected");
        assert!(err.to_string().contains("33"), "{err}");
    }

    /// The shape the exporter really writes is accepted, and yields one
    /// `Lut33` per basis table. Without this the test above could pass while
    /// the reader rejected everything.
    #[test]
    fn the_exporters_shape_is_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("basis.npy");
        write_test_npy(&path, &[3, 3, LUT_DIM, LUT_DIM, LUT_DIM]);
        let basis = read_basis(&path).expect("the exporter's own shape must load");
        assert_eq!(basis.len(), 3);
        assert_eq!(basis[0].data.len(), LUT_DIM * LUT_DIM * LUT_DIM * 3);
    }

    /// Truncated payloads are an error rather than a panic or a half-filled
    /// table.
    #[test]
    fn a_short_payload_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("short.npy");
        write_test_npy(&path, &[3, 3, LUT_DIM, LUT_DIM, LUT_DIM]);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 4_000]).unwrap();
        assert!(read_basis(&path).is_err());
    }

    /// Not a .npy at all.
    #[test]
    fn garbage_is_not_mistaken_for_a_basis() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nope.npy");
        std::fs::write(&path, b"just some bytes").unwrap();
        assert!(read_basis(&path).is_err());
    }

    /// Weights that arrive as NaN or wildly out of range must not poison the
    /// fused LUT — Lut33::fuse clamps, and this confirms the path reaches it.
    #[test]
    fn non_finite_weights_yield_the_identity() {
        let basis = vec![crate::develop::lut::Lut33::identity()];
        let fused = crate::develop::lut::Lut33::fuse(&basis, &[f32::NAN]);
        assert_eq!(fused.data, crate::develop::lut::Lut33::identity().data);
    }

    /// The real basis, when present, must decode with its axes the right way
    /// round. The shape checks above cannot catch a transposition — every
    /// dimension is 33 — and the result would be a look that is wrong rather
    /// than broken.
    ///
    /// LUT 0 is trained starting from the identity and stays near it, so the
    /// distance to the identity separates the two readings. Measured against
    /// the exported basis with numpy:
    ///
    /// | interpretation        | mean abs deviation |
    /// |-----------------------|--------------------|
    /// | `[c, b, g, r]` (ours) | 0.119              |
    /// | `[c, r, g, b]`        | 0.242              |
    ///
    /// The threshold sits between them. It is not tighter because LUT 0 is a
    /// *trained* table, not the identity itself — its channel maximum is 0.83,
    /// so a real deviation of ~0.12 is expected and correct.
    #[test]
    fn the_real_basis_lut_zero_resembles_the_identity() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/lut3d_basis.npy");
        if !path.exists() {
            eprintln!("skipping: {} not present", path.display());
            return;
        }
        let basis = read_basis(&path).expect("the exported basis must load");
        assert_eq!(basis.len(), 3, "the sRGB release ships three basis LUTs");

        let identity = crate::develop::lut::Lut33::identity();
        let mean_abs_dev = basis[0]
            .data
            .iter()
            .zip(identity.data.iter())
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / identity.data.len() as f32;
        assert!(
            mean_abs_dev < 0.18,
            "basis LUT 0 should sit near the identity; mean |Δ| = {mean_abs_dev}. \
             ~0.12 is correct, ~0.24 means the axes were read transposed"
        );
    }
}
