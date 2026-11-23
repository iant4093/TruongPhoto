use crate::AiError;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use truongphoto_core::AiTier;

/// A single downloadable file belonging to a local AI model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFileSpec {
    pub filename: &'static str,
    pub url: &'static str,
    pub expected_sha256: Option<&'static str>,
    pub approx_bytes: u64,
}

/// Authoritative specification for an Apache-2.0 / MIT local ONNX model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub role: &'static str,
    pub license: &'static str,
    pub hf_repo: &'static str,
    pub tier: Option<AiTier>,
    pub approx_bytes: u64,
    pub files: &'static [ModelFileSpec],
}

const SIGLIP2_BASE_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "onnx/vision_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/main/onnx/vision_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 190_000_000,
    },
    ModelFileSpec {
        filename: "onnx/text_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/main/onnx/text_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 190_000_000,
    },
    ModelFileSpec {
        filename: "tokenizer.json",
        url: "https://huggingface.co/onnx-community/siglip2-base-patch16-224-ONNX/resolve/main/tokenizer.json",
        expected_sha256: None,
        approx_bytes: 2_500_000,
    },
];

const SIGLIP2_LARGE_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "onnx/vision_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-large-patch16-256-ONNX/resolve/main/onnx/vision_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 620_000_000,
    },
    ModelFileSpec {
        filename: "onnx/text_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-large-patch16-256-ONNX/resolve/main/onnx/text_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 580_000_000,
    },
    ModelFileSpec {
        filename: "tokenizer.json",
        url: "https://huggingface.co/onnx-community/siglip2-large-patch16-256-ONNX/resolve/main/tokenizer.json",
        expected_sha256: None,
        approx_bytes: 2_500_000,
    },
];

const SIGLIP2_SO400M_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "onnx/vision_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-so400m-patch14-384-ONNX/resolve/main/onnx/vision_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 860_000_000,
    },
    ModelFileSpec {
        filename: "onnx/text_model_fp16.onnx",
        url: "https://huggingface.co/onnx-community/siglip2-so400m-patch14-384-ONNX/resolve/main/onnx/text_model_fp16.onnx",
        expected_sha256: None,
        approx_bytes: 820_000_000,
    },
    ModelFileSpec {
        filename: "tokenizer.json",
        url: "https://huggingface.co/onnx-community/siglip2-so400m-patch14-384-ONNX/resolve/main/tokenizer.json",
        expected_sha256: None,
        approx_bytes: 2_500_000,
    },
];

const YUNET_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "face_detection_yunet_2023mar.onnx",
    url: "https://huggingface.co/opencv/face_detection_yunet/resolve/main/face_detection_yunet_2023mar.onnx",
    expected_sha256: None,
    approx_bytes: 230_000,
}];

const AURAFACE_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "glintr100.onnx",
    url: "https://huggingface.co/fal/AuraFace-v1/resolve/main/glintr100.onnx",
    expected_sha256: None,
    approx_bytes: 260_000_000,
}];

const PP_OCRV6_DET_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "inference.onnx",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/main/inference.onnx",
        expected_sha256: None,
        approx_bytes: 4_800_000,
    },
    ModelFileSpec {
        filename: "inference.yml",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx/resolve/main/inference.yml",
        expected_sha256: None,
        approx_bytes: 4_096,
    },
];

const PP_OCRV6_REC_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "inference.onnx",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.onnx",
        expected_sha256: None,
        approx_bytes: 11_000_000,
    },
    ModelFileSpec {
        filename: "inference.yml",
        url: "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.yml",
        expected_sha256: None,
        approx_bytes: 64_000,
    },
];

const GEMMA4_E2B_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "gemma-4-E2B-it-Q4_0.gguf",
    url:
        "https://huggingface.co/ggml-org/gemma-4-E2B-it-GGUF/resolve/main/gemma-4-E2B-it-Q4_0.gguf",
    expected_sha256: None,
    approx_bytes: 1_600_000_000,
}];

const GEMMA4_E4B_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "gemma-4-E4B-it-Q4_0.gguf",
    url:
        "https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-E4B-it-Q4_0.gguf",
    expected_sha256: None,
    approx_bytes: 2_800_000_000,
}];

const QWEN35_4B_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "Qwen3.5-4B-Q4_K_M.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3.5-4B-GGUF/resolve/main/Qwen3.5-4B-Q4_K_M.gguf",
    expected_sha256: None,
    approx_bytes: 2_700_000_000,
}];

const GEMMA4_26B_A4B_FILES: &[ModelFileSpec] = &[
    ModelFileSpec {
        filename: "gemma-4-26B-A4B-it-Q4_0.gguf",
        url: "https://huggingface.co/ggml-org/gemma-4-26B-A4B-it-GGUF/resolve/main/gemma-4-26B-A4B-it-Q4_0.gguf",
        expected_sha256: None,
        approx_bytes: 14_500_000_000,
    },
    ModelFileSpec {
        filename: "mmproj-gemma-4-26B-A4B-it-Q8_0.gguf",
        url: "https://huggingface.co/ggml-org/gemma-4-26B-A4B-it-GGUF/resolve/main/mmproj-gemma-4-26B-A4B-it-Q8_0.gguf",
        expected_sha256: None,
        approx_bytes: 600_000_000,
    },
];

const WORLD_BASEMAP_PMTILES_FILES: &[ModelFileSpec] = &[ModelFileSpec {
    filename: "world-z0-z6.pmtiles",
    url: "https://build.protomaps.com/world-z0-z6.pmtiles",
    expected_sha256: None,
    approx_bytes: 35_000_000,
}];

/// Verified 100% Apache-2.0 / MIT local ONNX / GGUF model and basemap manifest.
pub const MODEL_MANIFEST: &[ModelSpec] = &[
    ModelSpec {
        id: "siglip2-base",
        name: "SigLIP2 Base (Tier 1)",
        role: "vision_tier1",
        license: "Apache-2.0",
        hf_repo: "onnx-community/siglip2-base-patch16-224-ONNX",
        tier: Some(AiTier::Tier1),
        approx_bytes: 382_500_000,
        files: SIGLIP2_BASE_FILES,
    },
    ModelSpec {
        id: "siglip2-large",
        name: "SigLIP2 Large (Tier 2)",
        role: "vision_tier2",
        license: "Apache-2.0",
        hf_repo: "onnx-community/siglip2-large-patch16-256-ONNX",
        tier: Some(AiTier::Tier2),
        approx_bytes: 1_202_500_000,
        files: SIGLIP2_LARGE_FILES,
    },
    ModelSpec {
        id: "siglip2-so400m",
        name: "SigLIP2 So400m (Tier 3)",
        role: "vision_tier3",
        license: "Apache-2.0",
        hf_repo: "onnx-community/siglip2-so400m-patch14-384-ONNX",
        tier: Some(AiTier::Tier3),
        approx_bytes: 1_682_500_000,
        files: SIGLIP2_SO400M_FILES,
    },
    ModelSpec {
        id: "yunet",
        name: "YuNet Face Detector",
        role: "face_detector",
        license: "MIT",
        hf_repo: "opencv/face_detection_yunet",
        tier: None,
        approx_bytes: 230_000,
        files: YUNET_FILES,
    },
    ModelSpec {
        id: "auraface-v1",
        name: "AuraFace-v1 Face Embedder",
        role: "face_embedder",
        license: "Apache-2.0",
        hf_repo: "fal/AuraFace-v1",
        tier: None,
        approx_bytes: 260_000_000,
        files: AURAFACE_FILES,
    },
    ModelSpec {
        id: "pp-ocrv6-det",
        name: "PP-OCRv6 Small Detector",
        role: "ocr_det",
        license: "Apache-2.0",
        hf_repo: "PaddlePaddle/PP-OCRv6_small_det_onnx",
        tier: None,
        approx_bytes: 4_804_096,
        files: PP_OCRV6_DET_FILES,
    },
    ModelSpec {
        id: "pp-ocrv6-rec",
        name: "PP-OCRv6 Small Recognizer",
        role: "ocr_rec",
        license: "Apache-2.0",
        hf_repo: "PaddlePaddle/PP-OCRv6_small_rec_onnx",
        tier: None,
        approx_bytes: 11_064_000,
        files: PP_OCRV6_REC_FILES,
    },
    ModelSpec {
        id: "gemma4-e2b",
        name: "Gemma 4 E2B Chat (Tier 1)",
        role: "chat_tier1",
        license: "Apache-2.0",
        hf_repo: "ggml-org/gemma-4-E2B-it-GGUF",
        tier: Some(AiTier::Tier1),
        approx_bytes: 1_600_000_000,
        files: GEMMA4_E2B_FILES,
    },
    ModelSpec {
        id: "gemma4-e4b",
        name: "Gemma 4 E4B Chat (Tier 2 Default)",
        role: "chat_tier2",
        license: "Apache-2.0",
        hf_repo: "ggml-org/gemma-4-E4B-it-GGUF",
        tier: Some(AiTier::Tier2),
        approx_bytes: 2_800_000_000,
        files: GEMMA4_E4B_FILES,
    },
    ModelSpec {
        id: "qwen3.5-4b",
        name: "Qwen 3.5 4B Chat (Tier 2 Alt)",
        role: "chat_tier2_alt",
        license: "Apache-2.0",
        hf_repo: "ggml-org/Qwen3.5-4B-GGUF",
        tier: Some(AiTier::Tier2),
        approx_bytes: 2_700_000_000,
        files: QWEN35_4B_FILES,
    },
    ModelSpec {
        id: "gemma4-26b-a4b",
        name: "Gemma 4 26B-A4B Chat + VLM (Tier 3)",
        role: "chat_tier3",
        license: "Apache-2.0",
        hf_repo: "ggml-org/gemma-4-26B-A4B-it-GGUF",
        tier: Some(AiTier::Tier3),
        approx_bytes: 15_100_000_000,
        files: GEMMA4_26B_A4B_FILES,
    },
    ModelSpec {
        id: "world-basemap-pmtiles",
        name: "Offline World Basemap (PMTiles)",
        role: "map_basemap",
        license: "ODbL/BSD",
        hf_repo: "Protomaps / OpenStreetMap",
        tier: None,
        approx_bytes: 35_000_000,
        files: WORLD_BASEMAP_PMTILES_FILES,
    },
];

/// Looks up a model specification by its canonical ID.
pub fn find_model_spec(model_id: &str) -> Option<&'static ModelSpec> {
    MODEL_MANIFEST.iter().find(|spec| spec.id == model_id)
}

/// Returns the local directory for a given model ID under `models_dir`.
pub fn model_dir(models_dir: &Path, model_id: &str) -> PathBuf {
    models_dir.join(model_id)
}

/// Returns `true` when every file in `spec` exists as a regular file on disk.
pub fn is_model_installed(models_dir: &Path, spec: &ModelSpec) -> bool {
    let base = model_dir(models_dir, spec.id);
    spec.files.iter().all(|f| base.join(f.filename).is_file())
}

fn validate_rel_filename(filename: &str) -> Result<PathBuf, AiError> {
    let p = Path::new(filename);
    if p.is_absolute() || filename.is_empty() {
        return Err(AiError::InvalidInput(format!(
            "invalid model filename `{filename}`"
        )));
    }
    let mut clean = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Normal(c) => clean.push(c),
            _ => {
                return Err(AiError::InvalidInput(format!(
                    "unsafe path traversal in model filename `{filename}`"
                )));
            }
        }
    }
    Ok(clean)
}

/// Streaming HTTP downloader with incremental SHA-256 verification and atomic `.part` swap.
#[derive(Debug, Clone)]
pub struct ModelDownloader {
    client: reqwest::Client,
}

impl Default for ModelDownloader {
    fn default() -> Self {
        Self::new().expect("default reqwest client builds cleanly")
    }
}

impl ModelDownloader {
    /// Constructs a `ModelDownloader` with a rustls-backed streaming HTTP client.
    pub fn new() -> Result<Self, AiError> {
        let client = reqwest::Client::builder()
            .user_agent("TruongPhoto/0.1")
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| AiError::ModelDownload(e.to_string()))?;
        Ok(Self { client })
    }

    /// Constructs a `ModelDownloader` wrapping a caller-provided `reqwest::Client`.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Downloads all files for `spec` into `<models_dir>/<spec.id>/`, verifying optional
    /// SHA-256 digests and returning a `{ filename -> sha256_hex }` manifest.
    pub async fn download_model<F>(
        &self,
        models_dir: &Path,
        spec: &ModelSpec,
        cancel: CancellationToken,
        mut on_progress: F,
    ) -> Result<HashMap<String, String>, AiError>
    where
        F: FnMut(u64, u64) + Send,
    {
        let target_dir = model_dir(models_dir, spec.id);
        self.download_files_into(&target_dir, spec.files, cancel, &mut on_progress)
            .await
    }

    /// Downloads an arbitrary slice of `ModelFileSpec` items into `target_dir`.
    pub async fn download_files_into<F>(
        &self,
        target_dir: &Path,
        files: &[ModelFileSpec],
        cancel: CancellationToken,
        mut on_progress: F,
    ) -> Result<HashMap<String, String>, AiError>
    where
        F: FnMut(u64, u64) + Send,
    {
        tokio::fs::create_dir_all(target_dir).await?;
        let total_hint: u64 = files.iter().map(|f| f.approx_bytes).sum();
        let mut completed_bytes: u64 = 0;
        let mut manifest = HashMap::with_capacity(files.len());

        for file_spec in files {
            let (digest, file_bytes) = self
                .download_single_file(
                    target_dir,
                    file_spec.filename,
                    file_spec.url,
                    file_spec.expected_sha256,
                    file_spec.approx_bytes,
                    completed_bytes,
                    total_hint,
                    &cancel,
                    &mut on_progress,
                )
                .await?;
            completed_bytes = completed_bytes.saturating_add(file_bytes);
            manifest.insert(file_spec.filename.to_string(), digest);
        }

        Ok(manifest)
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_single_file<F>(
        &self,
        target_dir: &Path,
        filename: &str,
        url: &str,
        expected_sha256: Option<&str>,
        approx_bytes: u64,
        prior_bytes: u64,
        total_hint: u64,
        cancel: &CancellationToken,
        on_progress: &mut F,
    ) -> Result<(String, u64), AiError>
    where
        F: FnMut(u64, u64) + Send,
    {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }

        let rel_path = validate_rel_filename(filename)?;
        let final_path = target_dir.join(&rel_path);
        let part_path = PathBuf::from(format!("{}.part", final_path.display()));

        if expected_sha256.is_none() {
            if let Ok(meta) = tokio::fs::metadata(&final_path).await {
                if meta.is_file() && meta.len() > 0 {
                    let existing_len = meta.len();
                    let effective_total = total_hint.max(prior_bytes.saturating_add(existing_len));
                    on_progress(prior_bytes.saturating_add(existing_len), effective_total);
                    return Ok((String::new(), existing_len));
                }
            }
        }

        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Check whether a partial download exists so large model files can resume via HTTP Range.
        let mut existing_part_len = tokio::fs::metadata(&part_path)
            .await
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .unwrap_or(0);

        let mut req = self.client.get(url);
        if existing_part_len > 0 {
            req = req.header(
                reqwest::header::RANGE,
                format!("bytes={existing_part_len}-"),
            );
        }

        let mut resp = tokio::select! {
            _ = cancel.cancelled() => return Err(AiError::Cancelled),
            res = req.send() => {
                res.map_err(|e| AiError::ModelDownload(format!("GET {url}: {e}")))?
            }
        };

        // If the existing `.part` exceeded the remote file (416), discard it and retry from byte 0.
        if existing_part_len > 0 && resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let _ = tokio::fs::remove_file(&part_path).await;
            existing_part_len = 0;
            resp = tokio::select! {
                _ = cancel.cancelled() => return Err(AiError::Cancelled),
                res = self.client.get(url).send() => {
                    res.map_err(|e| AiError::ModelDownload(format!("GET {url}: {e}")))?
                }
            };
        }

        if !resp.status().is_success() {
            return Err(AiError::ModelDownload(format!(
                "HTTP {} downloading {url}",
                resp.status()
            )));
        }

        let is_resumed =
            existing_part_len > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let resumed_offset = if is_resumed { existing_part_len } else { 0 };

        let exact_content_len = resp
            .content_length()
            .map(|rem| rem.saturating_add(resumed_offset));
        let content_len = exact_content_len.unwrap_or(approx_bytes.max(resumed_offset));
        let effective_total = total_hint.max(prior_bytes.saturating_add(content_len));

        let mut hasher = Sha256::new();
        let mut downloaded_this_file: u64 = resumed_offset;

        let mut file = if is_resumed {
            // Hash existing partial bytes so end-to-end SHA-256 verification remains exact.
            let mut reader = tokio::fs::File::open(&part_path).await?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = reader.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            drop(reader);
            on_progress(
                prior_bytes.saturating_add(downloaded_this_file),
                effective_total,
            );
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .await?
        } else {
            let _ = tokio::fs::remove_file(&part_path).await;
            tokio::fs::File::create(&part_path).await?
        };
        let mut stream = resp.bytes_stream();

        loop {
            let next_chunk = tokio::select! {
                _ = cancel.cancelled() => {
                    drop(file);
                    // Keep `.part` on cancellation so resuming later does not re-download GBs.
                    return Err(AiError::Cancelled);
                }
                chunk = stream.next() => chunk,
            };

            let Some(chunk_res) = next_chunk else {
                break;
            };

            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    drop(file);
                    return Err(AiError::ModelDownload(format!(
                        "stream error for {url}: {e}"
                    )));
                }
            };

            if let Err(e) = file.write_all(&chunk).await {
                drop(file);
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(AiError::Io(e));
            }
            hasher.update(&chunk);
            downloaded_this_file = downloaded_this_file.saturating_add(chunk.len() as u64);
            on_progress(
                prior_bytes.saturating_add(downloaded_this_file),
                effective_total,
            );
        }

        if let Err(e) = file.flush().await {
            drop(file);
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(AiError::Io(e));
        }
        drop(file);

        if downloaded_this_file == 0 {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(AiError::ModelDownload(format!(
                "empty 0-byte download for {url}"
            )));
        }

        if let Some(expected_len) = exact_content_len {
            if downloaded_this_file != expected_len {
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(AiError::ModelDownload(format!(
                    "truncated download for {url}: expected {expected_len} bytes, got {downloaded_this_file}"
                )));
            }
        }

        let actual_sha256 = hex::encode(hasher.finalize());
        if let Some(expected) = expected_sha256 {
            let expected_norm = expected.trim().to_ascii_lowercase();
            if actual_sha256 != expected_norm {
                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(AiError::ChecksumMismatch {
                    expected: expected_norm,
                    actual: actual_sha256,
                });
            }
        }

        let part_for_rename = part_path.clone();
        let final_for_rename = final_path.clone();
        let rename_res = tokio::task::spawn_blocking(move || {
            truongphoto_core::process::rename_with_windows_retry(
                &part_for_rename,
                &final_for_rename,
            )
        })
        .await
        .map_err(|e| AiError::ModelDownload(format!("rename join error: {e}")))?;
        if let Err(e) = rename_res {
            let _ = tokio::fs::remove_file(&part_path).await;
            return Err(AiError::Io(e));
        }
        Ok((actual_sha256, downloaded_this_file))
    }
}
