use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use truongphoto_ai::{AiError, AiPipeline, HybridVectorIndex};
use truongphoto_core::{AiTier, StoragePaths};

const FACE_VECTOR_DIM: usize = 512;

fn tier_index_name(tier: AiTier) -> &'static str {
    match tier {
        AiTier::Tier1 => "vision_tier1",
        AiTier::Tier2 => "vision_tier2",
        AiTier::Tier3 => "vision_tier3",
    }
}

/// Coordinates local AI inference pipelines, per-tier hybrid vector indices,
/// active model download cancellation tokens, and Stage 2 background worker wakeups.
pub struct AiCoordinator {
    pipeline: Arc<AiPipeline>,
    vectors_dir: PathBuf,
    models_dir: PathBuf,
    vision_indices: RwLock<HashMap<AiTier, Arc<HybridVectorIndex>>>,
    faces_index: Arc<HybridVectorIndex>,
    downloads: Mutex<HashMap<String, CancellationToken>>,
    pub wake: Arc<Notify>,
    pub worker_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AiCoordinator {
    /// Constructs an `AiCoordinator` backed by the production ONNX Runtime pipeline.
    pub fn production(paths: &StoragePaths) -> Self {
        let models_dir = paths.data_dir.join("models");
        let coreml_cache_dir = paths.cache_dir.join("coreml");
        let vectors_dir = paths.data_dir.join("vectors");
        let pipeline = Arc::new(AiPipeline::production(&models_dir, &coreml_cache_dir));
        Self::new(vectors_dir, models_dir, pipeline)
    }

    /// Constructs an `AiCoordinator` with a caller-supplied `AiPipeline`.
    pub fn new(vectors_dir: PathBuf, models_dir: PathBuf, pipeline: Arc<AiPipeline>) -> Self {
        let faces_index = Arc::new(
            HybridVectorIndex::open(&vectors_dir, "faces", FACE_VECTOR_DIM)
                .expect("failed to open faces HybridVectorIndex"),
        );

        Self {
            pipeline,
            vectors_dir,
            models_dir,
            vision_indices: RwLock::new(HashMap::new()),
            faces_index,
            downloads: Mutex::new(HashMap::new()),
            wake: Arc::new(Notify::new()),
            worker_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Returns the active `AiPipeline`.
    pub fn pipeline(&self) -> &Arc<AiPipeline> {
        &self.pipeline
    }

    /// Returns the local ONNX models root directory.
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Returns the hybrid vector storage root directory (`<data_dir>/vectors`).
    pub fn vectors_dir(&self) -> &Path {
        &self.vectors_dir
    }

    /// Returns the `512`-dim faces `HybridVectorIndex`.
    pub fn faces_index(&self) -> &Arc<HybridVectorIndex> {
        &self.faces_index
    }

    /// Lazily opens and caches the `HybridVectorIndex` for `tier`.
    pub fn vision_index(&self, tier: AiTier) -> Result<Arc<HybridVectorIndex>, AiError> {
        if let Some(idx) = self.vision_indices.read().get(&tier) {
            return Ok(Arc::clone(idx));
        }

        let mut guard = self.vision_indices.write();
        if let Some(idx) = guard.get(&tier) {
            return Ok(Arc::clone(idx));
        }

        let opened = Arc::new(HybridVectorIndex::open(
            &self.vectors_dir,
            tier_index_name(tier),
            tier.dim(),
        )?);
        guard.insert(tier, Arc::clone(&opened));
        Ok(opened)
    }

    /// Wakes the Stage 2 background indexing worker.
    pub fn notify_work(&self) {
        self.wake.notify_one();
    }

    /// Registers an active download cancellation token for `model_id`.
    /// Returns `false` if a download for `model_id` is already in progress.
    pub fn try_register_download(&self, model_id: &str, token: CancellationToken) -> bool {
        let mut guard = self.downloads.lock();
        if guard.contains_key(model_id) {
            return false;
        }
        guard.insert(model_id.to_string(), token);
        true
    }

    /// Returns `true` if a download for `model_id` is currently tracked as active.
    pub fn is_downloading(&self, model_id: &str) -> bool {
        self.downloads.lock().contains_key(model_id)
    }

    /// Cancels an active download for `model_id`, returning `true` if one was active.
    pub fn cancel_download(&self, model_id: &str) -> bool {
        let token = self.downloads.lock().remove(model_id);
        if let Some(tok) = token {
            tok.cancel();
            true
        } else {
            false
        }
    }

    /// Clears tracking for a finished/failed/cancelled download.
    pub fn finish_download(&self, model_id: &str) {
        self.downloads.lock().remove(model_id);
    }

    /// Cancels active downloads, evicts all ONNX Runtime and Chat sessions,
    /// and ensures vector index state is clean for shutdown.
    /// (`HybridVectorIndex` persists `.tombstones.roar` atomically on every mutation.)
    pub fn shutdown(&self) {
        for (_, token) in self.downloads.lock().drain() {
            token.cancel();
        }
        self.pipeline.ort.evict_all_sessions();
        self.pipeline.chat.evict_idle(std::time::Duration::ZERO);
    }
}
