use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use image::DynamicImage;
use ndarray::Array4;

use crate::models::{BBox, DetectedSubject, SubjectClass, SubjectDetector};

const INPUT_SIZE: u32 = 640;
const NUM_QUERIES: usize = 300;
const NUM_CLASSES: usize = 80;
const CONF_THRESHOLD: f32 = 0.5;

/// RT-DETR R50VD subject detector.
///
/// Loads `rt_detr_l.onnx` (exported via `tools/export_rt_detr.py`) and runs a
/// forward pass under `ort`. Outputs are `(logits, pred_boxes)`; see
/// `decode_detections` for the postprocessing contract. Verified to load and
/// run under ort 2.0.0-rc.12 (`rtdetr_loads_and_runs` smoke test).
pub struct RtDetrDetector {
    session: Mutex<ort::session::Session>,
}

impl RtDetrDetector {
    pub fn load(path: &Path) -> Result<Self> {
        let session = crate::models::build_session(path)?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }
}

impl SubjectDetector for RtDetrDetector {
    fn detect(&self, img: &DynamicImage) -> Result<Vec<DetectedSubject>> {
        let tensor = preprocess(img);
        let ort_tensor =
            ort::value::Tensor::<f32>::from_array(tensor).map_err(|e| anyhow::anyhow!("{e}"))?;

        let mut session = self.session.lock().expect("session mutex poisoned");
        let outputs = session
            .run(ort::inputs![&ort_tensor])
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // Verified 2026-06-26 against models/rt_detr_l.onnx: outputs are
        // (logits [1,NQ,NC], pred_boxes [1,NQ,4]) in that order, cxcywh-normalized.
        let mut out_iter = outputs.iter();

        let logit_val = out_iter
            .next()
            .ok_or_else(|| anyhow::anyhow!("no logits output"))?
            .1;
        let (logit_shape, logit_data) = logit_val
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("logits not f32: {e}"))?;

        let box_val = out_iter
            .next()
            .ok_or_else(|| anyhow::anyhow!("no boxes output"))?
            .1;
        let (box_shape, box_data) = box_val
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("boxes not f32: {e}"))?;

        let nq = logit_shape.get(1).copied().unwrap_or(NUM_QUERIES as i64) as usize;
        let nc = logit_shape.get(2).copied().unwrap_or(NUM_CLASSES as i64) as usize;

        if box_shape.get(1).copied().unwrap_or(0) as usize != nq {
            anyhow::bail!("logits/boxes query count mismatch");
        }

        Ok(decode_detections(
            logit_data,
            box_data,
            nq,
            nc,
            CONF_THRESHOLD,
        ))
    }

    fn name(&self) -> &str {
        "rt-detr-l"
    }
}

// ── preprocessing ─────────────────────────────────────────────────────────────

/// Letterbox-resize `img` to `INPUT_SIZE`×`INPUT_SIZE`, normalize to [0, 1],
/// and return a CHW tensor of shape (1, 3, 640, 640).
pub(crate) fn preprocess(img: &DynamicImage) -> Array4<f32> {
    let (orig_w, orig_h) = (img.width(), img.height());
    let scale = (INPUT_SIZE as f32 / orig_w as f32).min(INPUT_SIZE as f32 / orig_h as f32);
    let new_w = (orig_w as f32 * scale).round() as u32;
    let new_h = (orig_h as f32 * scale).round() as u32;

    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle);
    let rgb = resized.to_rgb8();

    let pad_x = ((INPUT_SIZE - new_w) / 2) as usize;
    let pad_y = ((INPUT_SIZE - new_h) / 2) as usize;

    let mut tensor = Array4::<f32>::zeros((1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize));
    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let p = rgb.get_pixel(x as u32, y as u32);
            tensor[[0, 0, pad_y + y, pad_x + x]] = p[0] as f32 / 255.0;
            tensor[[0, 1, pad_y + y, pad_x + x]] = p[1] as f32 / 255.0;
            tensor[[0, 2, pad_y + y, pad_x + x]] = p[2] as f32 / 255.0;
        }
    }
    tensor
}

// ── postprocessing ────────────────────────────────────────────────────────────

/// Per-query argmax decode: for each of `num_queries` queries, pick the class
/// with the highest logit, apply sigmoid, and emit a `DetectedSubject` if the
/// score meets `conf_thresh`.  Boxes are cxcywh-normalized; we convert to
/// top-left xywh (still normalized, clamped to [0, 1]).
pub(crate) fn decode_detections(
    logits: &[f32],
    boxes: &[f32],
    num_queries: usize,
    num_classes: usize,
    conf_thresh: f32,
) -> Vec<DetectedSubject> {
    let mut detections = Vec::new();

    for q in 0..num_queries {
        let logit_row = &logits[q * num_classes..(q + 1) * num_classes];

        let (best_class, &best_logit) = logit_row
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, &f32::NEG_INFINITY));

        let confidence = sigmoid(best_logit);
        if confidence < conf_thresh {
            continue;
        }

        let bx = &boxes[q * 4..(q + 1) * 4];
        let (cx, cy, w, h) = (bx[0], bx[1], bx[2], bx[3]);
        let bbox = BBox {
            x: (cx - w / 2.0).clamp(0.0, 1.0),
            y: (cy - h / 2.0).clamp(0.0, 1.0),
            w: w.clamp(0.0, 1.0),
            h: h.clamp(0.0, 1.0),
        };

        detections.push(DetectedSubject {
            bbox,
            class: coco_id_to_subject_class(best_class),
            confidence,
        });
    }

    detections
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn coco_id_to_subject_class(id: usize) -> SubjectClass {
    match id {
        0 => SubjectClass::Person,
        1..=8 => SubjectClass::Vehicle,
        9..=13 => SubjectClass::Object,
        14..=23 => SubjectClass::Animal,
        24..=79 => SubjectClass::Object,
        _ => SubjectClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::Array4;

    fn model_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../models/rt_detr_l.onnx"
        ))
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build flat logit + box vecs for `num_queries` × `num_classes`.
    /// `hot`: (query_idx, class_idx, raw_logit).
    /// `boxes`: (query_idx, [cx, cy, w, h]).  All unspecified entries default
    /// to –∞ (logits) or 0.5 (boxes).
    fn make_inputs(
        num_queries: usize,
        num_classes: usize,
        hot: &[(usize, usize, f32)],
        boxes: &[(usize, [f32; 4])],
    ) -> (Vec<f32>, Vec<f32>) {
        let mut logits = vec![f32::NEG_INFINITY; num_queries * num_classes];
        let mut box_vec = vec![0.5_f32; num_queries * 4];
        for &(q, c, v) in hot {
            logits[q * num_classes + c] = v;
        }
        for &(q, b) in boxes {
            box_vec[q * 4..q * 4 + 4].copy_from_slice(&b);
        }
        (logits, box_vec)
    }

    // ── postprocessing unit tests (no model file needed) ──────────────────────

    #[test]
    fn decode_above_threshold_fires_below_does_not() {
        let (logits, boxes) = make_inputs(
            2,
            80,
            // query 0: logit 5.0 → sigmoid ≈ 0.993; query 1: logit −5.0 → ≈ 0.007
            &[(0, 0, 5.0), (1, 0, -5.0)],
            &[(0, [0.5, 0.5, 0.2, 0.2]), (1, [0.5, 0.5, 0.2, 0.2])],
        );
        let dets = decode_detections(&logits, &boxes, 2, 80, 0.5);
        assert_eq!(dets.len(), 1, "only the above-threshold query should fire");
    }

    #[test]
    fn decode_cxcywh_converts_to_topleft_bbox() {
        // cx=0.5, cy=0.5, w=0.4, h=0.3 → x=0.3, y=0.35, w=0.4, h=0.3
        let (logits, boxes) = make_inputs(1, 80, &[(0, 0, 10.0)], &[(0, [0.5, 0.5, 0.4, 0.3])]);
        let dets = decode_detections(&logits, &boxes, 1, 80, 0.5);
        assert_eq!(dets.len(), 1);
        let b = dets[0].bbox;
        assert!((b.x - 0.3).abs() < 1e-5, "x={} expected 0.3", b.x);
        assert!((b.y - 0.35).abs() < 1e-5, "y={} expected 0.35", b.y);
        assert!((b.w - 0.4).abs() < 1e-5, "w={} expected 0.4", b.w);
        assert!((b.h - 0.3).abs() < 1e-5, "h={} expected 0.3", b.h);
    }

    #[test]
    fn decode_coco_id_0_is_person() {
        let (logits, boxes) = make_inputs(1, 80, &[(0, 0, 10.0)], &[(0, [0.5, 0.5, 0.2, 0.2])]);
        let dets = decode_detections(&logits, &boxes, 1, 80, 0.5);
        assert_eq!(dets[0].class, crate::models::SubjectClass::Person);
    }

    #[test]
    fn decode_coco_id_16_is_animal() {
        // COCO id 16 = dog (range 14–23 = Animal)
        let (logits, boxes) = make_inputs(1, 80, &[(0, 16, 10.0)], &[(0, [0.5, 0.5, 0.2, 0.2])]);
        let dets = decode_detections(&logits, &boxes, 1, 80, 0.5);
        assert_eq!(dets[0].class, crate::models::SubjectClass::Animal);
    }

    #[test]
    fn decode_coco_id_3_is_vehicle() {
        // COCO id 3 = motorcycle (range 1–8 = Vehicle)
        let (logits, boxes) = make_inputs(1, 80, &[(0, 3, 10.0)], &[(0, [0.5, 0.5, 0.2, 0.2])]);
        let dets = decode_detections(&logits, &boxes, 1, 80, 0.5);
        assert_eq!(dets[0].class, crate::models::SubjectClass::Vehicle);
    }

    #[test]
    fn decode_coco_id_50_is_object() {
        // COCO id 50 = broccoli — not person/animal/vehicle
        let (logits, boxes) = make_inputs(1, 80, &[(0, 50, 10.0)], &[(0, [0.5, 0.5, 0.2, 0.2])]);
        let dets = decode_detections(&logits, &boxes, 1, 80, 0.5);
        assert_eq!(dets[0].class, crate::models::SubjectClass::Object);
    }

    // ── preprocessing unit test (no model file needed) ────────────────────────

    #[test]
    fn preprocess_produces_640x640_tensor_in_unit_range() {
        use image::{DynamicImage, ImageBuffer, Rgb};
        // Non-square source to exercise letterboxing.
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_fn(100, 150, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let tensor = preprocess(&DynamicImage::ImageRgb8(img));
        assert_eq!(tensor.shape(), &[1, 3, 640, 640]);
        let data = tensor.as_slice().unwrap();
        assert!(
            data.iter().all(|&v| (0.0..=1.0).contains(&v)),
            "all pixels should be in [0.0, 1.0]"
        );
    }

    // ── smoke test (gated on model file) ─────────────────────────────────────

    /// Path-C gate: confirm the pre-exported onnx-community/rtdetr_r50vd loads
    /// and runs a forward pass under our pinned ort 2.0.0-rc.12, and print the
    /// real I/O contract.  Gated on the model file; no-ops in CI.
    ///   cargo test -p pipeline rtdetr_loads_and_runs -- --nocapture
    #[test]
    fn rtdetr_loads_and_runs() {
        let path = model_path();
        if !path.exists() {
            eprintln!("skipping: model not present at {}", path.display());
            return;
        }

        let mut session = crate::models::build_session(&path).expect("failed to build ORT session");

        eprintln!("\n=== RT-DETR ONNX I/O contract ===\nINPUTS:");
        for i in session.inputs() {
            eprintln!("  {:?}  {:?}", i.name(), i.dtype());
        }
        eprintln!("OUTPUTS:");
        for o in session.outputs() {
            eprintln!("  {:?}  {:?}", o.name(), o.dtype());
        }

        // Dummy normalized image: pixels/255 in [0,1], shape (1,3,640,640).
        let dummy = Array4::<f32>::zeros((1, 3, 640, 640));
        let tensor = ort::value::Tensor::<f32>::from_array(dummy).expect("tensor build failed");

        eprintln!("\nrunning forward pass ...");
        let outputs = session
            .run(ort::inputs![&tensor])
            .expect("forward pass failed — Path C gate NOT cleared");

        eprintln!("forward pass OK. outputs:");
        for (name, val) in outputs.iter() {
            match val.try_extract_tensor::<f32>() {
                Ok((shape, data)) => eprintln!("  {name:?}: shape={shape:?}  len={}", data.len()),
                Err(e) => eprintln!("  {name:?}: (not f32: {e})"),
            }
        }
        eprintln!("=== Path C gate CLEARED ===\n");
    }
}
