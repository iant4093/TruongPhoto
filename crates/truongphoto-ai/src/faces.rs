use crate::alignment::align_face_112;
use crate::models::model_dir;
use crate::ort_runtime::{OrtRuntimeManager, SessionKey};
use crate::vector::l2_normalize;
use crate::AiError;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, RgbImage};
use ort::value::TensorRef;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

const YUNET_INPUT_SIZE: u32 = 640;
const YUNET_CONF_THRESHOLD: f32 = 0.75;
const YUNET_NMS_IOU_THRESHOLD: f32 = 0.3;
const AURAFACE_EMBED_DIM: usize = 512;
const FACE_THUMB_SIZE: u32 = 160;

/// Detected face bounding box (normalized `0..1`), 5 landmarks (pixel coords), and 512d embedding.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedFace {
    pub bbox_x: f64,
    pub bbox_y: f64,
    pub bbox_w: f64,
    pub bbox_h: f64,
    pub det_score: f64,
    pub landmarks: [(f32, f32); 5],
    pub embedding: Vec<f32>,
}

/// Trait abstracting face detection, landmark alignment, and embedding extraction.
pub trait FaceBackend: Send + Sync {
    fn detect_and_embed(&self, img: &RgbImage) -> Result<Vec<DetectedFace>, AiError>;
}

#[derive(Debug, Clone)]
struct RawCandidate {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    score: f32,
    landmarks: [(f32, f32); 5],
}

fn iou(a: &RawCandidate, b: &RawCandidate) -> f32 {
    let ix1 = a.x1.max(b.x1);
    let iy1 = a.y1.max(b.y1);
    let ix2 = a.x2.min(b.x2);
    let iy2 = a.y2.min(b.y2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = (a.x2 - a.x1).max(0.0) * (a.y2 - a.y1).max(0.0);
    let area_b = (b.x2 - b.x1).max(0.0) * (b.y2 - b.y1).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 1e-6 {
        0.0
    } else {
        inter / union
    }
}

fn nms(mut candidates: Vec<RawCandidate>, iou_threshold: f32) -> Vec<RawCandidate> {
    candidates.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept = Vec::new();
    for cand in candidates {
        if kept.iter().all(|k| iou(k, &cand) <= iou_threshold) {
            kept.push(cand);
        }
    }
    kept
}

/// Production face pipeline using YuNet (`640x640` BGR) + Umeyama 5-point alignment + AuraFace-v1 (`512d`).
pub struct OrtFaceBackend {
    models_dir: PathBuf,
    ort: Arc<OrtRuntimeManager>,
}

impl std::fmt::Debug for OrtFaceBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtFaceBackend")
            .field("models_dir", &self.models_dir)
            .finish()
    }
}

impl OrtFaceBackend {
    pub fn new(models_dir: impl Into<PathBuf>, ort: Arc<OrtRuntimeManager>) -> Self {
        Self {
            models_dir: models_dir.into(),
            ort,
        }
    }

    fn detect_faces(&self, img: &RgbImage) -> Result<Vec<RawCandidate>, AiError> {
        let orig_w = img.width();
        let orig_h = img.height();
        if orig_w == 0 || orig_h == 0 {
            return Ok(Vec::new());
        }

        let scale =
            (YUNET_INPUT_SIZE as f32 / orig_w as f32).min(YUNET_INPUT_SIZE as f32 / orig_h as f32);
        let scaled_w = ((orig_w as f32 * scale).round() as u32).clamp(1, YUNET_INPUT_SIZE);
        let scaled_h = ((orig_h as f32 * scale).round() as u32).clamp(1, YUNET_INPUT_SIZE);
        let resized = image::imageops::resize(img, scaled_w, scaled_h, FilterType::Triangle);

        let side = YUNET_INPUT_SIZE as usize;
        let hw = side * side;
        let mut bgr_chw = vec![0.0f32; 3 * hw];
        for y in 0..scaled_h as usize {
            for x in 0..scaled_w as usize {
                let px = resized.get_pixel(x as u32, y as u32).0;
                let idx = y * side + x;
                // YuNet expects BGR [0..255]
                bgr_chw[idx] = px[2] as f32;
                bgr_chw[hw + idx] = px[1] as f32;
                bgr_chw[2 * hw + idx] = px[0] as f32;
            }
        }

        let det_path =
            model_dir(&self.models_dir, "yunet").join("face_detection_yunet_2023mar.onnx");
        let shape = [1usize, 3, side, side];

        let candidates = self
            .ort
            .with_session(SessionKey::FaceDet, &det_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], bgr_chw.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["input" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;

                let mut out_cands = Vec::new();
                for stride in [8usize, 16, 32] {
                    let cls_key = format!("cls_{stride}");
                    let obj_key = format!("obj_{stride}");
                    let bbox_key = format!("bbox_{stride}");
                    let kps_key = format!("kps_{stride}");

                    let (Some(cls_v), Some(obj_v), Some(bbox_v), Some(kps_v)) = (
                        outputs.get(&cls_key),
                        outputs.get(&obj_key),
                        outputs.get(&bbox_key),
                        outputs.get(&kps_key),
                    ) else {
                        continue;
                    };

                    let (_, cls) = cls_v
                        .try_extract_tensor::<f32>()
                        .map_err(|e| AiError::Ort(e.to_string()))?;
                    let (_, obj) = obj_v
                        .try_extract_tensor::<f32>()
                        .map_err(|e| AiError::Ort(e.to_string()))?;
                    let (_, bbox) = bbox_v
                        .try_extract_tensor::<f32>()
                        .map_err(|e| AiError::Ort(e.to_string()))?;
                    let (_, kps) = kps_v
                        .try_extract_tensor::<f32>()
                        .map_err(|e| AiError::Ort(e.to_string()))?;

                    let cols = side / stride;
                    let rows = side / stride;
                    let anchors = rows * cols;
                    if cls.len() < anchors
                        || obj.len() < anchors
                        || bbox.len() < anchors * 4
                        || kps.len() < anchors * 10
                    {
                        continue;
                    }

                    let st = stride as f32;
                    for r in 0..rows {
                        for c in 0..cols {
                            let idx = r * cols + c;
                            let score =
                                (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                            if score < YUNET_CONF_THRESHOLD {
                                continue;
                            }
                            let b_off = idx * 4;
                            let cx = (c as f32 + bbox[b_off]) * st;
                            let cy = (r as f32 + bbox[b_off + 1]) * st;
                            let bw = bbox[b_off + 2].exp() * st;
                            let bh = bbox[b_off + 3].exp() * st;

                            let x1 = ((cx - 0.5 * bw) / scale).clamp(0.0, orig_w as f32);
                            let y1 = ((cy - 0.5 * bh) / scale).clamp(0.0, orig_h as f32);
                            let x2 = ((cx + 0.5 * bw) / scale).clamp(0.0, orig_w as f32);
                            let y2 = ((cy + 0.5 * bh) / scale).clamp(0.0, orig_h as f32);
                            if x2 <= x1 || y2 <= y1 {
                                continue;
                            }

                            let k_off = idx * 10;
                            let mut landmarks = [(0.0f32, 0.0f32); 5];
                            for (p, lm) in landmarks.iter_mut().enumerate() {
                                let lx = (c as f32 + kps[k_off + p * 2]) * st / scale;
                                let ly = (r as f32 + kps[k_off + p * 2 + 1]) * st / scale;
                                *lm = (lx, ly);
                            }

                            out_cands.push(RawCandidate {
                                x1,
                                y1,
                                x2,
                                y2,
                                score,
                                landmarks,
                            });
                        }
                    }
                }
                Ok(out_cands)
            })?;

        Ok(nms(candidates, YUNET_NMS_IOU_THRESHOLD))
    }

    fn embed_aligned_face(&self, aligned_112: &RgbImage) -> Result<Vec<f32>, AiError> {
        let hw = 112usize * 112usize;
        let mut chw = vec![0.0f32; 3 * hw];
        for (idx, px) in aligned_112.pixels().enumerate() {
            chw[idx] = (px.0[0] as f32 - 127.5) / 127.5;
            chw[hw + idx] = (px.0[1] as f32 - 127.5) / 127.5;
            chw[2 * hw + idx] = (px.0[2] as f32 - 127.5) / 127.5;
        }

        let rec_path = model_dir(&self.models_dir, "auraface-v1").join("glintr100.onnx");
        let shape = [1usize, 3, 112, 112];

        self.ort
            .with_session(SessionKey::FaceRec, &rec_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], chw.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["data" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let value = outputs.get("683").unwrap_or(&outputs[0]);
                let (_shape, data) = value
                    .try_extract_tensor::<f32>()
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                if data.len() < AURAFACE_EMBED_DIM {
                    return Err(AiError::Ort(format!(
                        "auraface output too short: expected >= {AURAFACE_EMBED_DIM}, got {}",
                        data.len()
                    )));
                }
                Ok(l2_normalize(&data[..AURAFACE_EMBED_DIM]))
            })
    }
}

impl FaceBackend for OrtFaceBackend {
    fn detect_and_embed(&self, img: &RgbImage) -> Result<Vec<DetectedFace>, AiError> {
        let orig_w = img.width() as f64;
        let orig_h = img.height() as f64;
        if orig_w <= 0.0 || orig_h <= 0.0 {
            return Ok(Vec::new());
        }

        let dets = self.detect_faces(img)?;
        let mut faces = Vec::with_capacity(dets.len());
        for cand in dets {
            let aligned = align_face_112(img, &cand.landmarks);
            let embedding = self.embed_aligned_face(&aligned)?;
            faces.push(DetectedFace {
                bbox_x: (cand.x1 as f64 / orig_w).clamp(0.0, 1.0),
                bbox_y: (cand.y1 as f64 / orig_h).clamp(0.0, 1.0),
                bbox_w: ((cand.x2 - cand.x1) as f64 / orig_w).clamp(0.0, 1.0),
                bbox_h: ((cand.y2 - cand.y1) as f64 / orig_h).clamp(0.0, 1.0),
                det_score: cand.score as f64,
                landmarks: cand.landmarks,
                embedding,
            });
        }
        Ok(faces)
    }
}

/// Pure-Rust deterministic face backend for offline CI and integration tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicTestFaceBackend;

impl DeterministicTestFaceBackend {
    pub fn new() -> Self {
        Self
    }
}

impl FaceBackend for DeterministicTestFaceBackend {
    fn detect_and_embed(&self, img: &RgbImage) -> Result<Vec<DetectedFace>, AiError> {
        let w = img.width();
        let h = img.height();
        if w < 32 || h < 32 {
            return Ok(Vec::new());
        }

        // Compute deterministic 512d embedding from quantized center RGB signature.
        let mut sum = [0.0f64; 3];
        let mut count = 0usize;
        let x0 = w / 4;
        let x1 = (w * 3) / 4;
        let y0 = h / 4;
        let y1 = (h * 3) / 4;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = img.get_pixel(x, y).0;
                sum[0] += p[0] as f64;
                sum[1] += p[1] as f64;
                sum[2] += p[2] as f64;
                count += 1;
            }
        }
        let n = count.max(1) as f64;
        let r_bucket = ((sum[0] / n) / 32.0).round() as u64;
        let g_bucket = ((sum[1] / n) / 32.0).round() as u64;
        let b_bucket = ((sum[2] / n) / 32.0).round() as u64;
        let seed = (r_bucket << 16) ^ (g_bucket << 8) ^ b_bucket ^ 0x4641_4345;

        let mut emb = vec![0.0f32; AURAFACE_EMBED_DIM];
        let mut state = seed;
        for slot in &mut emb {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let unit = ((z & 0xffff) as f32 / 32768.0) - 1.0;
            *slot = unit;
        }

        let wf = w as f32;
        let hf = h as f32;
        Ok(vec![DetectedFace {
            bbox_x: 0.25,
            bbox_y: 0.20,
            bbox_w: 0.50,
            bbox_h: 0.60,
            det_score: 0.99,
            landmarks: [
                (0.40 * wf, 0.42 * hf),
                (0.60 * wf, 0.42 * hf),
                (0.50 * wf, 0.55 * hf),
                (0.42 * wf, 0.68 * hf),
                (0.58 * wf, 0.68 * hf),
            ],
            embedding: l2_normalize(&emb),
        }])
    }
}

/// Generates a 160x160 WebP face crop thumbnail with 35% padding around the normalized `bbox`
/// `(bbox_x, bbox_y, bbox_w, bbox_h)`.
pub fn crop_face_thumbnail_webp(
    img: &RgbImage,
    bbox: (f64, f64, f64, f64),
) -> Result<Vec<u8>, AiError> {
    let iw = img.width();
    let ih = img.height();
    if iw == 0 || ih == 0 {
        return Err(AiError::InvalidInput(
            "cannot crop face thumbnail from empty image".to_string(),
        ));
    }

    let (bx, by, bw, bh) = bbox;
    let bx_px = bx.clamp(0.0, 1.0) * (iw as f64);
    let by_px = by.clamp(0.0, 1.0) * (ih as f64);
    let bw_px = (bw.clamp(0.01, 1.0)) * (iw as f64);
    let bh_px = (bh.clamp(0.01, 1.0)) * (ih as f64);

    let cx = bx_px + 0.5 * bw_px;
    let cy = by_px + 0.5 * bh_px;
    let side = bw_px.max(bh_px) * 1.70; // +35% padding on every side => 1.7x side

    let half = 0.5 * side;
    let x0 = (cx - half).floor().max(0.0) as u32;
    let y0 = (cy - half).floor().max(0.0) as u32;
    let x1 = (cx + half).ceil().min(iw as f64) as u32;
    let y1 = (cy + half).ceil().min(ih as f64) as u32;

    let crop_w = x1.saturating_sub(x0).max(1);
    let crop_h = y1.saturating_sub(y0).max(1);
    let safe_x0 = x0.min(iw - 1);
    let safe_y0 = y0.min(ih - 1);
    let safe_w = crop_w.min(iw - safe_x0);
    let safe_h = crop_h.min(ih - safe_y0);

    let cropped = image::imageops::crop_imm(img, safe_x0, safe_y0, safe_w, safe_h).to_image();
    let resized = image::imageops::resize(
        &cropped,
        FACE_THUMB_SIZE,
        FACE_THUMB_SIZE,
        FilterType::Triangle,
    );

    let mut out = Vec::new();
    DynamicImage::ImageRgb8(resized)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::WebP)
        .map_err(|e| AiError::Image(format!("encode face thumbnail WebP: {e}")))?;
    Ok(out)
}
