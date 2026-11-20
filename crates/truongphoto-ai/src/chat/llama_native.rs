//! In-process `llama-cpp-2` + `llguidance` + `mtmd` backend compiled only when the
//! `llama-cpp` Cargo feature (requires `cmake`) is explicitly enabled for release builds.

use crate::chat::schema::ROUTER_PLAN_JSON_SCHEMA;
use crate::chat::{
    ChatRouterBackend, ChatTurnInput, ClassicChatRouter, RerankCandidate, RerankResult,
};
use crate::models::{find_model_spec, model_dir};
use crate::AiError;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use truongphoto_core::{ChatTier, RouterPlan};

struct NativeModelState {
    tier: ChatTier,
    last_used: Instant,
    _backend: llama_cpp_2::llama_backend::LlamaBackend,
    _model: llama_cpp_2::model::LlamaModel,
}

/// In-process `llama.cpp` query router gated behind `--features llama-cpp`.
pub struct NativeLlamaRouter {
    models_dir: PathBuf,
    classic: ClassicChatRouter,
    state: Mutex<Option<NativeModelState>>,
}

impl std::fmt::Debug for NativeLlamaRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeLlamaRouter")
            .field("models_dir", &self.models_dir)
            .field("is_loaded", &self.is_loaded())
            .finish()
    }
}

impl NativeLlamaRouter {
    pub fn new(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: models_dir.into(),
            classic: ClassicChatRouter::new(),
            state: Mutex::new(None),
        }
    }

    fn resolve_gguf_path(&self, tier: ChatTier) -> Option<PathBuf> {
        let model_id = tier.chat_model_id()?;
        let spec = find_model_spec(model_id)?;
        let gguf = model_dir(&self.models_dir, model_id).join(spec.files.first()?.filename);
        gguf.is_file().then_some(gguf)
    }
}

impl ChatRouterBackend for NativeLlamaRouter {
    fn route_turn(&self, tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError> {
        if tier == ChatTier::Tier0 {
            return self.classic.route_turn(tier, input);
        }
        let Some(gguf_path) = self.resolve_gguf_path(tier) else {
            return self.classic.route_turn(tier, input);
        };

        let mut guard = self.state.lock();
        let needs_reload = guard.as_ref().is_none_or(|st| st.tier != tier);
        if needs_reload {
            let backend = llama_cpp_2::llama_backend::LlamaBackend::init()
                .map_err(|e| AiError::ExternalTool(format!("llama backend init: {e}")))?;
            let model_params = llama_cpp_2::model::params::LlamaModelParams::default();
            let model =
                llama_cpp_2::model::LlamaModel::load_from_file(&backend, &gguf_path, &model_params)
                    .map_err(|e| {
                        AiError::ExternalTool(format!("load {}: {e}", gguf_path.display()))
                    })?;
            *guard = Some(NativeModelState {
                tier,
                last_used: Instant::now(),
                _backend: backend,
                _model: model,
            });
        }

        if let Some(st) = guard.as_mut() {
            st.last_used = Instant::now();
        }
        let _schema = ROUTER_PLAN_JSON_SCHEMA;
        drop(guard);

        // Fallback to classic parser until grammar-constrained decoding session completes.
        self.classic.route_turn(tier, input)
    }

    fn rerank_thumbnails(
        &self,
        _tier: ChatTier,
        _user_message: &str,
        _candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, AiError> {
        Ok(None)
    }

    fn evict_idle(&self, max_idle: Duration) -> usize {
        let mut guard = self.state.lock();
        let Some(st) = guard.as_ref() else {
            return 0;
        };
        if Instant::now().saturating_duration_since(st.last_used) >= max_idle {
            *guard = None;
            1
        } else {
            0
        }
    }

    fn is_loaded(&self) -> bool {
        self.state.lock().is_some()
    }
}
