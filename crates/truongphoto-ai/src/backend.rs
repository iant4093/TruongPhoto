use crate::chat::{ChatRouterBackend, DeterministicTestChatRouter, SidecarLlamaRouter};
use crate::faces::{DeterministicTestFaceBackend, FaceBackend, OrtFaceBackend};
use crate::ocr::{DeterministicTestOcrBackend, OcrBackend, OrtOcrBackend};
use crate::ort_runtime::OrtRuntimeManager;
use crate::vision::{DeterministicTestVisionBackend, OrtSiglip2Backend, VisionBackend};
use std::path::PathBuf;
use std::sync::Arc;

/// Unified AI pipeline holding Vision, Face, OCR, Chat Router, and ONNX Runtime handles.
#[derive(Clone)]
pub struct AiPipeline {
    pub ort: Arc<OrtRuntimeManager>,
    pub vision: Arc<dyn VisionBackend>,
    pub faces: Arc<dyn FaceBackend>,
    pub ocr: Arc<dyn OcrBackend>,
    pub chat: Arc<dyn ChatRouterBackend>,
    pub models_dir: PathBuf,
}

impl std::fmt::Debug for AiPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiPipeline")
            .field("models_dir", &self.models_dir)
            .field("ort_available", &self.ort.ort_available())
            .field("ort_ep", &self.ort.ep_name())
            .field("chat_loaded", &self.chat.is_loaded())
            .finish()
    }
}

impl AiPipeline {
    /// Constructs the production ONNX Runtime + Sidecar GGUF AI pipeline.
    ///
    /// Gracefully reports `ort.ort_available() == false` if no ONNX Runtime dynamic library
    /// is installed yet without panicking, and falls back to `ClassicChatRouter` when no
    /// `llama-server` binary / GGUF model is installed.
    pub fn production(
        models_dir: impl Into<PathBuf>,
        coreml_cache_dir: impl Into<PathBuf>,
    ) -> Self {
        let models_dir = models_dir.into();
        let ort = Arc::new(OrtRuntimeManager::new(coreml_cache_dir));
        let vision: Arc<dyn VisionBackend> =
            Arc::new(OrtSiglip2Backend::new(&models_dir, Arc::clone(&ort)));
        let faces: Arc<dyn FaceBackend> =
            Arc::new(OrtFaceBackend::new(&models_dir, Arc::clone(&ort)));
        let ocr: Arc<dyn OcrBackend> = Arc::new(OrtOcrBackend::new(&models_dir, Arc::clone(&ort)));
        let chat: Arc<dyn ChatRouterBackend> = Arc::new(SidecarLlamaRouter::new(&models_dir));
        Self {
            ort,
            vision,
            faces,
            ocr,
            chat,
            models_dir,
        }
    }

    /// Constructs a 100% offline, deterministic pure-Rust AI pipeline for unit & integration tests.
    pub fn deterministic_test() -> Self {
        let ort = Arc::new(OrtRuntimeManager::new(std::env::temp_dir()));
        Self {
            ort,
            vision: Arc::new(DeterministicTestVisionBackend::new()),
            faces: Arc::new(DeterministicTestFaceBackend::new()),
            ocr: Arc::new(DeterministicTestOcrBackend::new()),
            chat: Arc::new(DeterministicTestChatRouter::new()),
            models_dir: std::env::temp_dir(),
        }
    }

    /// Constructs a pipeline with caller-supplied backend implementations (preserving Phase 2 signature).
    pub fn with_backends(
        models_dir: impl Into<PathBuf>,
        ort: Arc<OrtRuntimeManager>,
        vision: Arc<dyn VisionBackend>,
        faces: Arc<dyn FaceBackend>,
        ocr: Arc<dyn OcrBackend>,
    ) -> Self {
        let models_dir = models_dir.into();
        let chat: Arc<dyn ChatRouterBackend> = Arc::new(SidecarLlamaRouter::new(&models_dir));
        Self {
            ort,
            vision,
            faces,
            ocr,
            chat,
            models_dir,
        }
    }

    /// Replaces the conversational chat router backend on this pipeline.
    pub fn with_chat(mut self, chat: Arc<dyn ChatRouterBackend>) -> Self {
        self.chat = chat;
        self
    }
}
