use crate::models::model_dir;
use crate::ort_runtime::{OrtRuntimeManager, SessionKey};
use crate::tags_bank::{tier_tag_threshold, MAX_AUTO_TAGS, PHOTOGRAPHY_TAGS};
use crate::vector::l2_normalize;
use crate::AiError;
use image::imageops::FilterType;
use image::RgbImage;
use ort::value::TensorRef;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokenizers::Tokenizer;
use truongphoto_core::AiTier;

const SIGLIP2_MAX_SEQ_LEN: usize = 64;

type TagEmbeddingBank = Vec<(String, Vec<f32>)>;

/// Trait abstracting vision/text semantic embedding and zero-shot tagging.
pub trait VisionBackend: Send + Sync {
    fn embed_image(&self, tier: AiTier, img: &RgbImage) -> Result<Vec<f32>, AiError>;
    fn embed_text(&self, tier: AiTier, query: &str) -> Result<Vec<f32>, AiError>;
    fn score_zero_shot_tags(
        &self,
        tier: AiTier,
        img_vec: &[f32],
    ) -> Result<Vec<(String, f32)>, AiError>;
}

const fn siglip2_image_size(tier: AiTier) -> u32 {
    match tier {
        AiTier::Tier1 => 224,
        AiTier::Tier2 => 256,
        AiTier::Tier3 => 384,
    }
}

/// Production SigLIP2 ONNX Runtime backend supporting Tiers 1–3 (`224`, `256`, `384`).
pub struct OrtSiglip2Backend {
    models_dir: PathBuf,
    ort: Arc<OrtRuntimeManager>,
    tokenizers: RwLock<HashMap<AiTier, Arc<Tokenizer>>>,
    tag_bank_cache: RwLock<HashMap<AiTier, TagEmbeddingBank>>,
}

impl std::fmt::Debug for OrtSiglip2Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrtSiglip2Backend")
            .field("models_dir", &self.models_dir)
            .finish()
    }
}

impl OrtSiglip2Backend {
    pub fn new(models_dir: impl Into<PathBuf>, ort: Arc<OrtRuntimeManager>) -> Self {
        Self {
            models_dir: models_dir.into(),
            ort,
            tokenizers: RwLock::new(HashMap::new()),
            tag_bank_cache: RwLock::new(HashMap::new()),
        }
    }

    fn get_or_load_tokenizer(&self, tier: AiTier) -> Result<Arc<Tokenizer>, AiError> {
        if let Some(tok) = self.tokenizers.read().get(&tier) {
            return Ok(Arc::clone(tok));
        }
        let tok_path = model_dir(&self.models_dir, tier.vision_model_id()).join("tokenizer.json");
        if !tok_path.is_file() {
            return Err(AiError::ModelNotInstalled(format!(
                "SigLIP2 tokenizer missing: {}",
                tok_path.display()
            )));
        }
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| AiError::Tokenizer(format!("load {}: {e}", tok_path.display())))?;
        let arc = Arc::new(tokenizer);
        self.tokenizers.write().insert(tier, Arc::clone(&arc));
        Ok(arc)
    }

    fn embed_texts_batch(
        &self,
        tier: AiTier,
        queries: &[String],
    ) -> Result<Vec<Vec<f32>>, AiError> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let tokenizer = self.get_or_load_tokenizer(tier)?;
        let model_path = model_dir(&self.models_dir, tier.vision_model_id())
            .join("onnx")
            .join("text_model_fp16.onnx");
        let dim = tier.dim();
        const BATCH_SIZE: usize = 8;
        let mut results = Vec::with_capacity(queries.len());

        for chunk in queries.chunks(BATCH_SIZE) {
            let b = chunk.len();
            let mut input_ids = vec![0i64; b * SIGLIP2_MAX_SEQ_LEN];
            for (row_idx, q) in chunk.iter().enumerate() {
                let normalized_query = q.trim().to_lowercase();
                let encoding = tokenizer
                    .encode(normalized_query, true)
                    .map_err(|e| AiError::Tokenizer(e.to_string()))?;
                let row_offset = row_idx * SIGLIP2_MAX_SEQ_LEN;
                for (i, &tok) in encoding
                    .get_ids()
                    .iter()
                    .take(SIGLIP2_MAX_SEQ_LEN)
                    .enumerate()
                {
                    input_ids[row_offset + i] = tok as i64;
                }
            }

            let shape = [b, SIGLIP2_MAX_SEQ_LEN];
            let batch_vecs =
                self.ort
                    .with_session(SessionKey::VisionText(tier), &model_path, |session| {
                        let tensor = TensorRef::from_array_view((&shape[..], input_ids.as_slice()))
                            .map_err(|e| AiError::Ort(e.to_string()))?;
                        let outputs = session
                            .run(ort::inputs!["input_ids" => tensor])
                            .map_err(|e| AiError::Ort(e.to_string()))?;
                        let value = outputs
                            .get("pooler_output")
                            .or_else(|| outputs.get("text_embeds"))
                            .unwrap_or(&outputs[0]);
                        let (_out_shape, data) = value
                            .try_extract_tensor::<f32>()
                            .map_err(|e| AiError::Ort(e.to_string()))?;
                        if data.len() < b * dim {
                            return Err(AiError::Ort(format!(
                                "batched text output too short: expected >= {}, got {}",
                                b * dim,
                                data.len()
                            )));
                        }
                        let mut out = Vec::with_capacity(b);
                        for row_idx in 0..b {
                            let start = row_idx * dim;
                            out.push(l2_normalize(&data[start..start + dim]));
                        }
                        Ok(out)
                    })?;
            results.extend(batch_vecs);
        }

        Ok(results)
    }

    fn get_or_build_tag_embeddings(
        &self,
        tier: AiTier,
    ) -> Result<Vec<(String, Vec<f32>)>, AiError> {
        if let Some(cached) = self.tag_bank_cache.read().get(&tier) {
            return Ok(cached.clone());
        }

        let dim = tier.dim();
        let expected_bytes = PHOTOGRAPHY_TAGS.len() * dim * std::mem::size_of::<f32>();
        let cache_path =
            model_dir(&self.models_dir, tier.vision_model_id()).join("tag_bank_v2.bin");

        if let Ok(raw) = std::fs::read(&cache_path) {
            if raw.len() == expected_bytes {
                let mut entries = Vec::with_capacity(PHOTOGRAPHY_TAGS.len());
                let floats: &[f32] = bytemuck::cast_slice(&raw);
                for (idx, &tag) in PHOTOGRAPHY_TAGS.iter().enumerate() {
                    let start = idx * dim;
                    entries.push((tag.to_string(), floats[start..start + dim].to_vec()));
                }
                self.tag_bank_cache.write().insert(tier, entries.clone());
                return Ok(entries);
            }
        }

        let prompts: Vec<String> = PHOTOGRAPHY_TAGS
            .iter()
            .map(|&tag| format!("a photo of {tag}."))
            .collect();
        let vecs = self.embed_texts_batch(tier, &prompts)?;
        // Immediately release the large text encoder session so Stage 2 image indexing stays lean on RAM.
        self.ort.evict_session(SessionKey::VisionText(tier));

        let mut entries = Vec::with_capacity(PHOTOGRAPHY_TAGS.len());
        let mut flat = Vec::with_capacity(PHOTOGRAPHY_TAGS.len() * dim);
        for (&tag, vec) in PHOTOGRAPHY_TAGS.iter().zip(vecs.into_iter()) {
            flat.extend_from_slice(&vec);
            entries.push((tag.to_string(), vec));
        }

        let tmp_path = cache_path.with_extension("bin.tmp");
        if std::fs::write(&tmp_path, bytemuck::cast_slice(&flat)).is_ok() {
            let _ = std::fs::rename(&tmp_path, &cache_path);
        }

        self.tag_bank_cache.write().insert(tier, entries.clone());
        Ok(entries)
    }
}

impl VisionBackend for OrtSiglip2Backend {
    fn embed_image(&self, tier: AiTier, img: &RgbImage) -> Result<Vec<f32>, AiError> {
        let size = siglip2_image_size(tier);
        let resized = image::imageops::resize(img, size, size, FilterType::Triangle);
        let hw = (size as usize) * (size as usize);
        let mut chw = vec![0.0f32; 3 * hw];
        for (idx, px) in resized.pixels().enumerate() {
            chw[idx] = (px.0[0] as f32 / 255.0 - 0.5) / 0.5;
            chw[hw + idx] = (px.0[1] as f32 / 255.0 - 0.5) / 0.5;
            chw[2 * hw + idx] = (px.0[2] as f32 / 255.0 - 0.5) / 0.5;
        }

        let model_path = model_dir(&self.models_dir, tier.vision_model_id())
            .join("onnx")
            .join("vision_model_fp16.onnx");
        let shape = [1usize, 3, size as usize, size as usize];
        let dim = tier.dim();

        self.ort
            .with_session(SessionKey::VisionImage(tier), &model_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], chw.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["pixel_values" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let value = outputs
                    .get("pooler_output")
                    .or_else(|| outputs.get("image_embeds"))
                    .unwrap_or(&outputs[0]);
                let (_out_shape, data) = value
                    .try_extract_tensor::<f32>()
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                if data.len() < dim {
                    return Err(AiError::Ort(format!(
                        "vision output too short: expected >= {dim}, got {}",
                        data.len()
                    )));
                }
                Ok(l2_normalize(&data[..dim]))
            })
    }

    fn embed_text(&self, tier: AiTier, query: &str) -> Result<Vec<f32>, AiError> {
        let tokenizer = self.get_or_load_tokenizer(tier)?;
        let normalized_query = query.trim().to_lowercase();
        let encoding = tokenizer
            .encode(normalized_query, true)
            .map_err(|e| AiError::Tokenizer(e.to_string()))?;

        let mut input_ids = vec![0i64; SIGLIP2_MAX_SEQ_LEN];
        for (i, &tok) in encoding
            .get_ids()
            .iter()
            .take(SIGLIP2_MAX_SEQ_LEN)
            .enumerate()
        {
            input_ids[i] = tok as i64;
        }

        let model_path = model_dir(&self.models_dir, tier.vision_model_id())
            .join("onnx")
            .join("text_model_fp16.onnx");
        let shape = [1usize, SIGLIP2_MAX_SEQ_LEN];
        let dim = tier.dim();

        self.ort
            .with_session(SessionKey::VisionText(tier), &model_path, |session| {
                let tensor = TensorRef::from_array_view((&shape[..], input_ids.as_slice()))
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let outputs = session
                    .run(ort::inputs!["input_ids" => tensor])
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                let value = outputs
                    .get("pooler_output")
                    .or_else(|| outputs.get("text_embeds"))
                    .unwrap_or(&outputs[0]);
                let (_out_shape, data) = value
                    .try_extract_tensor::<f32>()
                    .map_err(|e| AiError::Ort(e.to_string()))?;
                if data.len() < dim {
                    return Err(AiError::Ort(format!(
                        "text output too short: expected >= {dim}, got {}",
                        data.len()
                    )));
                }
                Ok(l2_normalize(&data[..dim]))
            })
    }

    fn score_zero_shot_tags(
        &self,
        tier: AiTier,
        img_vec: &[f32],
    ) -> Result<Vec<(String, f32)>, AiError> {
        let norm_img = l2_normalize(img_vec);
        let bank = self.get_or_build_tag_embeddings(tier)?;
        let threshold = tier_tag_threshold(tier);
        let mut hits: Vec<(String, f32)> = bank
            .iter()
            .filter_map(|(tag, tag_vec)| {
                let sim: f32 = norm_img
                    .iter()
                    .zip(tag_vec.iter())
                    .map(|(&a, &b)| a * b)
                    .sum();
                if sim >= threshold {
                    Some((tag.clone(), sim))
                } else {
                    None
                }
            })
            .collect();
        hits.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        hits.truncate(MAX_AUTO_TAGS);
        Ok(hits)
    }
}

/// Pure-Rust deterministic vision backend for offline CI and server integration tests.
///
/// Produces L2-normalized semantic embeddings with color-concept alignment so warm/sunset,
/// blue/ocean, green/forest, and bright/document synthetic test images match corresponding queries.
#[derive(Debug, Default)]
pub struct DeterministicTestVisionBackend {
    tag_bank_cache: RwLock<HashMap<AiTier, TagEmbeddingBank>>,
}

impl DeterministicTestVisionBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn add_token_signal(vec: &mut [f32], token: &str, weight: f32) {
        let dim = vec.len();
        if dim == 0 || token.is_empty() {
            return;
        }
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        for b in token.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        for k in 0..16u64 {
            let mut z = h.wrapping_add(k.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let idx = (z as usize) % dim;
            let sign = if (z >> 63) == 0 { 1.0 } else { -1.0 };
            vec[idx] += sign * weight;
        }
    }
}

impl VisionBackend for DeterministicTestVisionBackend {
    fn embed_image(&self, tier: AiTier, img: &RgbImage) -> Result<Vec<f32>, AiError> {
        let dim = tier.dim();
        let mut out = vec![0.0f32; dim];
        let total_px = (img.width() as usize).saturating_mul(img.height() as usize);
        if total_px == 0 {
            out[0] = 1.0;
            return Ok(out);
        }

        let mut sum_r = 0.0f64;
        let mut sum_g = 0.0f64;
        let mut sum_b = 0.0f64;
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;

        let step = (total_px / 256).max(1);
        let mut sampled = 0usize;
        for (i, px) in img.pixels().enumerate() {
            if i % step == 0 {
                sum_r += px.0[0] as f64;
                sum_g += px.0[1] as f64;
                sum_b += px.0[2] as f64;
                sampled += 1;
                for &c in &px.0 {
                    hash ^= c as u64;
                    hash = hash.wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        let n = sampled.max(1) as f64;
        let avg_r = sum_r / n;
        let avg_g = sum_g / n;
        let avg_b = sum_b / n;

        // Project coarse visual color concepts into shared text semantic space.
        if avg_r > avg_b + 35.0 && avg_r > 130.0 {
            Self::add_token_signal(&mut out, "sunset", 2.0);
            Self::add_token_signal(&mut out, "golden", 1.0);
            Self::add_token_signal(&mut out, "sky", 0.8);
        }
        if avg_b > avg_r + 25.0 && avg_b > 110.0 {
            Self::add_token_signal(&mut out, "ocean", 2.0);
            Self::add_token_signal(&mut out, "beach", 1.2);
            Self::add_token_signal(&mut out, "sky", 1.0);
        }
        if avg_g > avg_r + 20.0 && avg_g > avg_b + 20.0 {
            Self::add_token_signal(&mut out, "forest", 2.0);
            Self::add_token_signal(&mut out, "trees", 1.2);
            Self::add_token_signal(&mut out, "mountains", 0.8);
        }
        if avg_r > 215.0 && avg_g > 215.0 && avg_b > 215.0 {
            Self::add_token_signal(&mut out, "document", 2.0);
            Self::add_token_signal(&mut out, "receipt", 1.5);
            Self::add_token_signal(&mut out, "whiteboard", 1.0);
        }
        if avg_r < 45.0 && avg_g < 45.0 && avg_b < 55.0 {
            Self::add_token_signal(&mut out, "night", 2.0);
            Self::add_token_signal(&mut out, "stars", 1.2);
        }

        // Add subtle per-image deterministic signature so distinct images stay separable.
        for k in 0..8u64 {
            let mut z = hash.wrapping_add(k.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            let idx = (z as usize) % dim;
            out[idx] += 0.25;
        }

        Ok(l2_normalize(&out))
    }

    fn embed_text(&self, tier: AiTier, query: &str) -> Result<Vec<f32>, AiError> {
        let dim = tier.dim();
        let mut out = vec![0.0f32; dim];
        let lower = query.trim().to_lowercase();
        let mut has_token = false;
        for token in lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            if matches!(token, "a" | "an" | "the" | "of" | "photo" | "picture") {
                continue;
            }
            has_token = true;
            Self::add_token_signal(&mut out, token, 1.0);
        }
        if !has_token {
            Self::add_token_signal(&mut out, &lower, 1.0);
        }
        Ok(l2_normalize(&out))
    }

    fn score_zero_shot_tags(
        &self,
        tier: AiTier,
        img_vec: &[f32],
    ) -> Result<Vec<(String, f32)>, AiError> {
        let cached = { self.tag_bank_cache.read().get(&tier).cloned() };
        let bank = match cached {
            Some(b) => b,
            None => {
                let mut entries = Vec::with_capacity(PHOTOGRAPHY_TAGS.len());
                for &tag in PHOTOGRAPHY_TAGS {
                    let vec = self.embed_text(tier, tag)?;
                    entries.push((tag.to_string(), vec));
                }
                self.tag_bank_cache.write().insert(tier, entries.clone());
                entries
            }
        };

        let norm_img = l2_normalize(img_vec);
        let threshold = tier_tag_threshold(tier);
        let mut hits: Vec<(String, f32)> = bank
            .iter()
            .filter_map(|(tag, tag_vec)| {
                let sim: f32 = norm_img
                    .iter()
                    .zip(tag_vec.iter())
                    .map(|(&a, &b)| a * b)
                    .sum();
                if sim >= threshold {
                    Some((tag.clone(), sim))
                } else {
                    None
                }
            })
            .collect();
        hits.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        hits.truncate(MAX_AUTO_TAGS);
        Ok(hits)
    }
}
