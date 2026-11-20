use crate::models::model_dir;
use crate::ort_runtime::{OrtRuntimeManager, SessionKey};
use crate::AiError;
use image::imageops::FilterType;
use image::RgbImage;
use ort::value::TensorRef;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

const OCR_DET_SIZE: u32 = 960;
const OCR_DET_THRESHOLD: f32 = 0.30;
const OCR_REC_HEIGHT: u32 = 48;

/// Recognized OCR text payload ready for `ocr_documents` persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrResult {
    pub full_text: String,
    pub lang_hint: Option<String>,
    pub model_id: String,
}

/// Trait abstracting local optical character recognition.
pub trait OcrBackend: Send + Sync {
    fn recognize(&self, img: &RgbImage) -> Result<OcrResult, AiError>;
}

/// Selects the smallest fixed PP-OCRv6 recognition width bucket (`320`, `640`, or `960`)
/// to avoid dynamic-shape CoreML ANE recompilations.
pub const fn select_rec_width_bucket(scaled_w: u32) -> u32 {
    if scaled_w <= 320 {
        320
    } else if scaled_w <= 640 {
        640
    } else {
        960
    }
}

/// Greedy CTC argmax decoder: collapses consecutive identical indices and drops blank index `0`.
pub fn ctc_greedy_decode(
    logits: &[f32],
    timesteps: usize,
    num_classes: usize,
    dict: &[String],
) -> String {
    if timesteps == 0 || num_classes == 0 || logits.len() < timesteps * num_classes {
        return String::new();
    }

    let mut out = String::new();
    let mut prev_idx: usize = 0;

    for t in 0..timesteps {
        let row = &logits[t * num_classes..(t + 1) * num_classes];
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for (c, &val) in row.iter().enumerate() {
            if val > best_val {
                best_val = val;
                best_idx = c;
            }
        }

        if best_idx != 0 && best_idx != prev_idx {
            let dict_idx = best_idx - 1;
            if let Some(ch) = dict.get(dict_idx) {
                out.push_str(ch);
            } else if dict_idx == dict.len() {
                out.push(' ');
            }
        }
        prev_idx = best_idx;
    }

    out.trim().to_string()
}

/// Parses `PostProcess.character_dict` entries from a PaddleOCR `inference.yml` string.
pub fn parse_paddle_character_dict(yaml_str: &str) -> Vec<String> {
    let mut dict = Vec::new();
    let mut in_dict = false;
    let mut dict_indent = 0usize;

    for raw_line in yaml_str.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw_line.len() - raw_line.trim_start().len();

        if !in_dict {
            if trimmed.starts_with("character_dict:") {
                in_dict = true;
                dict_indent = indent;
            }
            continue;
        }

        if indent <= dict_indent && !trimmed.starts_with('-') {
            break;
        }

        if let Some(rest) = trimmed.strip_prefix('-') {
            let item = rest.trim_start();
            let parsed = unquote_yaml_scalar(item);
            dict.push(parsed);
        } else if indent <= dict_indent {
            break;
        }
    }

    dict
}

fn unquote_yaml_scalar(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some(other) => out.push(other),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    s.to_string()
}

#[derive(Debug, Clone, Copy)]
struct TextBox {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

fn extract_db_boxes(prob_map: &[f32], side: usize, max_w: u32, max_h: u32) -> Vec<TextBox> {
    let mut visited = vec![false; side * side];
    let mut boxes = Vec::new();
    let mut queue = VecDeque::new();

    for sy in 0..side {
        for sx in 0..side {
            let start_idx = sy * side + sx;
            if visited[start_idx] || prob_map[start_idx] <= OCR_DET_THRESHOLD {
                continue;
            }
            visited[start_idx] = true;
            queue.push_back((sx, sy));

            let mut min_x = sx;
            let mut max_x = sx;
            let mut min_y = sy;
            let mut max_y = sy;
            let mut area = 0usize;

            while let Some((cx, cy)) = queue.pop_front() {
                area += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                for (dx, dy) in [(-1isize, 0), (1, 0), (0, -1isize), (0, 1)] {
                    let nx = cx as isize + dx;
                    let ny = cy as isize + dy;
                    if nx >= 0 && ny >= 0 && (nx as usize) < side && (ny as usize) < side {
                        let nidx = (ny as usize) * side + (nx as usize);
                        if !visited[nidx] && prob_map[nidx] > OCR_DET_THRESHOLD {
                            visited[nidx] = true;
                            queue.push_back((nx as usize, ny as usize));
                        }
                    }
                }
            }

            if area < 12 {
                continue;
            }

            let bw = (max_x - min_x + 1) as f32;
            let bh = (max_y - min_y + 1) as f32;
            if bw < 4.0 || bh < 4.0 {
                continue;
            }

            // Slight 1.5x DBNet unclip expansion
            let pad_x = (0.25 * bw).max(2.0);
            let pad_y = (0.25 * bh).max(2.0);
            let x0 = ((min_x as f32 - pad_x).floor().max(0.0) as u32).min(max_w.saturating_sub(1));
            let y0 = ((min_y as f32 - pad_y).floor().max(0.0) as u32).min(max_h.saturating_sub(1));
            let x1 = ((max_x as f32 + 1.0 + pad_x).ceil() as u32).min(max_w);
            let y1 = ((max_y as f32 + 1.0 + pad_y).ceil() as u32).min(max_h);

            if x1 > x0 + 2 && y1 > y0 + 2 {
                boxes.push(TextBox { x0, y0, x1, y1 });
            }
        }
    }

    // Sort in reading order (top-to-bottom row bands, left-to-right).
    boxes.sort_unstable_by(|a, b| {
        let band_a = a.y0 / 16;
        let band_b = b.y0 / 16;
        band_a.cmp(&band_b).then_with(|| a.x0.cmp(&b.x0))
    });
    boxes
}

/// Production PP-OCRv6 detector + bucketed recognizer using `OrtRuntimeManager`.
pub struct OrtOcrBackend {
    models_dir: PathBuf,
    ort: Arc<OrtRuntimeManager>,
    dict_cache: RwLock<Option<Vec<String>>>,
}

impl std::fmt::Debug for OrtOcrBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtOcrBackend")
            .field("models_dir", &self.models_dir)
            .finish()
    }
}

impl OrtOcrBackend {
    pub fn new(models_dir: impl Into<PathBuf>, ort: Arc<OrtRuntimeManager>) -> Self {
        Self {
            models_dir: models_dir.into(),
            ort,
            dict_cache: RwLock::new(None),
        }
    }

    fn get_or_load_dict(&self) -> Result<Vec<String>, AiError> {
        if let Some(d) = self.dict_cache.read().as_ref() {
            return Ok(d.clone());
        }
        let yml_path = model_dir(&self.models_dir, "pp-ocrv6-rec").join("inference.yml");
        if !yml_path.is_file() {
            return Err(AiError::ModelNotInstalled(format!(
                "PP-OCRv6 dict YAML missing: {}",
                yml_path.display()
            )));
        }
        let content = std::fs::read_to_string(&yml_path)?;
        let dict = parse_paddle_character_dict(&content);
        if dict.is_empty() {
            return Err(AiError::InvalidInput(format!(
                "empty character_dict in {}",
                yml_path.display()
            )));
        }
        *self.dict_cache.write() = Some(dict.clone());
        Ok(dict)
    }

    fn detect_text_boxes(&self, img: &RgbImage) -> Result<(Vec<TextBox>, f32), AiError> {
        let orig_w = img.width();
        let orig_h = img.height();
        if orig_w == 0 || orig_h == 0 {
            return Ok((Vec::new(), 1.0));
        }

        let scale = (OCR_DET_SIZE as f32 / orig_w as f32).min(OCR_DET_SIZE as f32 / orig_h as f32);
        let scaled_w = ((orig_w as f32 * scale).round() as u32).clamp(1, OCR_DET_SIZE);
        let scaled_h = ((orig_h as f32 * scale).round() as u32).clamp(1, OCR_DET_SIZE);
        let resized = image::imageops::resize(img, scaled_w, scaled_h, FilterType::Triangle);

        let side = OCR_DET_SIZE as usize;
        let hw = side * side;
        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];
        let mut chw = vec![0.0f32; 3 * hw];
        for y in 0..scaled_h as usize {
            for x in 0..scaled_w as usize {
                let px = resized.get_pixel(x as u32, y as u32).0;
                let idx = y * side + x;
                chw[idx] = (px[0] as f32 / 255.0 - mean[0]) / std[0];
                chw[hw + idx] = (px[1] as f32 / 255.0 - mean[1]) / std[1];
                chw[2 * hw + idx] = (px[2] as f32 / 255.0 - mean[2]) / std[2];
            }
        }

        let det_path = model_dir(&self.models_dir, "pp-ocrv6-det").join("inference.onnx");
        let shape = [1usize, 3, side, side];

        let boxes = self
            .ort
            .with_session(SessionKey::OcrDet, &det_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], chw.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["x" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let (_out_shape, prob_map) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                if prob_map.len() < hw {
                    return Err(AiError::Ort("pp-ocrv6-det output too short".to_string()));
                }
                Ok(extract_db_boxes(&prob_map[..hw], side, scaled_w, scaled_h))
            })?;

        Ok((boxes, scale))
    }

    fn recognize_crop(&self, crop: &RgbImage, dict: &[String]) -> Result<String, AiError> {
        let cw = crop.width();
        let ch = crop.height();
        if cw == 0 || ch == 0 {
            return Ok(String::new());
        }
        let scaled_w =
            (((cw as f32) * (OCR_REC_HEIGHT as f32 / ch as f32)).round() as u32).clamp(1, 960);
        let bucket_w = select_rec_width_bucket(scaled_w);
        let resized = image::imageops::resize(crop, scaled_w, OCR_REC_HEIGHT, FilterType::Triangle);

        let h = OCR_REC_HEIGHT as usize;
        let w = bucket_w as usize;
        let hw = h * w;
        // Pad right with neutral gray (0.0 after (128/255 - 0.5)/0.5 ≈ 0)
        let mut chw = vec![0.0f32; 3 * hw];
        for y in 0..h {
            for x in 0..scaled_w as usize {
                let px = resized.get_pixel(x as u32, y as u32).0;
                let idx = y * w + x;
                chw[idx] = (px[0] as f32 / 255.0 - 0.5) / 0.5;
                chw[hw + idx] = (px[1] as f32 / 255.0 - 0.5) / 0.5;
                chw[2 * hw + idx] = (px[2] as f32 / 255.0 - 0.5) / 0.5;
            }
        }

        let rec_path = model_dir(&self.models_dir, "pp-ocrv6-rec").join("inference.onnx");
        let shape = [1usize, 3, h, w];

        self.ort
            .with_session(SessionKey::OcrRec, &rec_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], chw.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["x" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let (out_shape, logits) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let dims: &[i64] = out_shape;
                if dims.len() < 3 {
                    return Err(AiError::Ort(format!(
                        "unexpected pp-ocrv6-rec output rank {}",
                        dims.len()
                    )));
                }
                let timesteps = dims[1].max(0) as usize;
                let num_classes = dims[2].max(0) as usize;
                Ok(ctc_greedy_decode(logits, timesteps, num_classes, dict))
            })
    }
}

impl OcrBackend for OrtOcrBackend {
    fn recognize(&self, img: &RgbImage) -> Result<OcrResult, AiError> {
        let dict = self.get_or_load_dict()?;
        let (boxes, scale) = self.detect_text_boxes(img)?;
        let iw = img.width();
        let ih = img.height();

        let mut lines = Vec::new();
        for b in boxes {
            let ox0 = ((b.x0 as f32 / scale).floor() as u32).min(iw.saturating_sub(1));
            let oy0 = ((b.y0 as f32 / scale).floor() as u32).min(ih.saturating_sub(1));
            let ox1 = ((b.x1 as f32 / scale).ceil() as u32).min(iw);
            let oy1 = ((b.y1 as f32 / scale).ceil() as u32).min(ih);
            let cw = ox1.saturating_sub(ox0);
            let ch = oy1.saturating_sub(oy0);
            if cw < 4 || ch < 4 {
                continue;
            }
            let crop = image::imageops::crop_imm(img, ox0, oy0, cw, ch).to_image();
            let text = self.recognize_crop(&crop, &dict)?;
            if text.chars().count() >= 2 {
                lines.push(text);
            }
        }

        Ok(OcrResult {
            full_text: lines.join("\n"),
            lang_hint: None,
            model_id: "pp-ocrv6".to_string(),
        })
    }
}

/// Pure-Rust deterministic OCR backend for offline CI and integration tests.
#[derive(Debug, Default, Clone)]
pub struct DeterministicTestOcrBackend {
    fixed_text: Option<String>,
}

impl DeterministicTestOcrBackend {
    pub fn new() -> Self {
        Self { fixed_text: None }
    }

    pub fn with_fixed_text(text: impl Into<String>) -> Self {
        Self {
            fixed_text: Some(text.into()),
        }
    }
}

impl OcrBackend for DeterministicTestOcrBackend {
    fn recognize(&self, img: &RgbImage) -> Result<OcrResult, AiError> {
        if let Some(text) = &self.fixed_text {
            return Ok(OcrResult {
                full_text: text.clone(),
                lang_hint: Some("en".to_string()),
                model_id: "pp-ocrv6-test".to_string(),
            });
        }

        // Heuristic for test images: bright document-like images yield deterministic receipt text.
        let total = (img.width() as usize).saturating_mul(img.height() as usize);
        if total == 0 {
            return Ok(OcrResult {
                full_text: String::new(),
                lang_hint: None,
                model_id: "pp-ocrv6-test".to_string(),
            });
        }

        let mut bright_count = 0usize;
        let mut dark_count = 0usize;
        let step = (total / 512).max(1);
        let mut sampled = 0usize;
        for (i, px) in img.pixels().enumerate() {
            if i % step == 0 {
                sampled += 1;
                let luma = (px.0[0] as u32 + px.0[1] as u32 + px.0[2] as u32) / 3;
                if luma >= 220 {
                    bright_count += 1;
                } else if luma <= 60 {
                    dark_count += 1;
                }
            }
        }

        let full_text = if sampled > 0 && bright_count * 2 >= sampled && dark_count > 0 {
            "RECEIPT TOTAL 42.00".to_string()
        } else {
            String::new()
        };

        Ok(OcrResult {
            full_text,
            lang_hint: Some("en".to_string()),
            model_id: "pp-ocrv6-test".to_string(),
        })
    }
}
