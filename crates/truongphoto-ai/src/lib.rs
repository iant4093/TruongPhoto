//! Local AI inference pipelines, hybrid vector engine (`vectors.f16` + USearch HNSW),
//! model downloader, and host hardware tier detection.

pub mod alignment;
pub mod backend;
pub mod chat;
pub mod clustering;
pub mod faces;
pub mod hardware;
pub mod keyframes;
pub mod models;
pub mod ocr;
pub mod ort_runtime;
pub mod tags_bank;
pub mod vector;
pub mod vision;

pub use backend::AiPipeline;
pub use chat::{
    merge_router_plan, parse_and_validate_router_plan, ChatRouterBackend, ChatTurnInput,
    ClassicChatRouter, DeterministicTestChatRouter, RerankCandidate, RerankResult,
    SidecarLlamaRouter, ROUTER_PLAN_JSON_SCHEMA,
};
pub use clustering::{
    chinese_whispers_cluster, decode_centroid_le, encode_centroid_le, match_face_to_centroids,
    update_running_centroid, DEFAULT_CW_SIM_THRESHOLD, DEFAULT_FACE_MATCH_THRESHOLD,
};
pub use faces::{
    crop_face_thumbnail_webp, DetectedFace, DeterministicTestFaceBackend, FaceBackend,
    OrtFaceBackend,
};
pub use hardware::{detect_hardware_info, recommend_tiers_for_ram};
pub use keyframes::{sample_video_keyframes, SampledKeyframe};
pub use models::{
    find_model_spec, is_model_installed, model_dir, ModelDownloader, ModelFileSpec, ModelSpec,
    MODEL_MANIFEST,
};
pub use ocr::{
    ctc_greedy_decode, parse_paddle_character_dict, select_rec_width_bucket,
    DeterministicTestOcrBackend, OcrBackend, OcrResult, OrtOcrBackend,
};
pub use ort_runtime::{discover_ort_dylib, OrtRuntimeManager, SessionKey};
pub use tags_bank::{tier_tag_threshold, MAX_AUTO_TAGS, PHOTOGRAPHY_TAGS};
pub use vector::{
    collapse_video_keyframes, l2_normalize, overfetch_k, CollapsedAssetHit, FlatVectorStore,
    HybridVectorIndex, KeyframeHit, DELTA_COMPACT_THRESHOLD, EXACT_SCAN_THRESHOLD, HNSW_EF_SEARCH,
};
pub use vision::{DeterministicTestVisionBackend, OrtSiglip2Backend, VisionBackend};

use std::path::Path;

/// Errors returned by AI inference, model download, and vector indexing pipelines.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not implemented in Phase 0: {0}")]
    Unimplemented(&'static str),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("model not installed: {0}")]
    ModelNotInstalled(String),

    #[error("model download failed: {0}")]
    ModelDownload(String),

    #[error("SHA-256 checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("operation cancelled")]
    Cancelled,

    #[error("ONNX Runtime error: {0}")]
    Ort(String),

    #[error("vector index error: {0}")]
    VectorIndex(String),

    #[error("tokenizer error: {0}")]
    Tokenizer(String),

    #[error("image error: {0}")]
    Image(String),

    #[error("external tool error: {0}")]
    ExternalTool(String),
}

/// Vision embedding generator (CLIP/SigLIP family, Phase 0 compatibility trait).
pub trait VisionEmbedder: Send + Sync {
    fn embedding_dim(&self) -> usize;
    fn embed_image(&self, image_path: &Path) -> Result<Vec<f32>, AiError>;
    fn embed_text(&self, query: &str) -> Result<Vec<f32>, AiError>;
}

/// Face detection and face recognition embedding engine (Phase 0 compatibility trait).
pub trait FaceEngine: Send + Sync {
    fn detect_and_embed(&self, image_path: &Path) -> Result<Vec<Vec<f32>>, AiError>;
}

/// Local optical character recognition engine (Phase 0 compatibility trait).
pub trait OcrEngine: Send + Sync {
    fn recognize_text(&self, image_path: &Path) -> Result<String, AiError>;
}

/// Local LLM router for natural-language photo search and Q&A.
pub trait LlmRouter: Send + Sync {
    fn complete(&self, prompt: &str) -> Result<String, AiError>;
}

/// Approximate nearest-neighbor vector index (Phase 0 compatibility trait).
pub trait VectorIndex: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>, AiError>;
}

/// Phase 0 stub implementation of `VisionEmbedder`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubVisionEmbedder;

impl VisionEmbedder for StubVisionEmbedder {
    fn embedding_dim(&self) -> usize {
        768
    }

    fn embed_image(&self, _image_path: &Path) -> Result<Vec<f32>, AiError> {
        Err(AiError::Unimplemented("VisionEmbedder::embed_image"))
    }

    fn embed_text(&self, _query: &str) -> Result<Vec<f32>, AiError> {
        Err(AiError::Unimplemented("VisionEmbedder::embed_text"))
    }
}

/// Phase 0 stub implementation of `FaceEngine`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubFaceEngine;

impl FaceEngine for StubFaceEngine {
    fn detect_and_embed(&self, _image_path: &Path) -> Result<Vec<Vec<f32>>, AiError> {
        Err(AiError::Unimplemented("FaceEngine::detect_and_embed"))
    }
}

/// Phase 0 stub implementation of `OcrEngine`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubOcrEngine;

impl OcrEngine for StubOcrEngine {
    fn recognize_text(&self, _image_path: &Path) -> Result<String, AiError> {
        Err(AiError::Unimplemented("OcrEngine::recognize_text"))
    }
}

/// Phase 0 stub implementation of `LlmRouter`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubLlmRouter;

impl LlmRouter for StubLlmRouter {
    fn complete(&self, _prompt: &str) -> Result<String, AiError> {
        Err(AiError::Unimplemented("LlmRouter::complete"))
    }
}

/// Phase 0 empty `VectorIndex` stub.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubVectorIndex;

impl VectorIndex for StubVectorIndex {
    fn len(&self) -> usize {
        0
    }

    fn search(&self, _query: &[f32], _k: usize) -> Result<Vec<(u64, f32)>, AiError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase0_ai_stubs_behave_predictably() {
        let vision = StubVisionEmbedder;
        assert_eq!(vision.embedding_dim(), 768);
        assert!(matches!(
            vision.embed_text("sunset"),
            Err(AiError::Unimplemented(_))
        ));

        let faces = StubFaceEngine;
        assert!(matches!(
            faces.detect_and_embed(Path::new("a.jpg")),
            Err(AiError::Unimplemented(_))
        ));

        let ocr = StubOcrEngine;
        assert!(matches!(
            ocr.recognize_text(Path::new("a.jpg")),
            Err(AiError::Unimplemented(_))
        ));

        let llm = StubLlmRouter;
        assert!(matches!(llm.complete("hi"), Err(AiError::Unimplemented(_))));

        let index = StubVectorIndex;
        assert!(index.is_empty());
        assert!(index.search(&[0.0; 4], 10).unwrap().is_empty());
    }
}
