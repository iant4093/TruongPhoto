//! Conversational search & query router (Tier 0 Classic, Tier 1–3 local GGUF sidecar/native,
//! and deterministic offline test backend).

pub mod classic;
#[cfg(feature = "llama-cpp")]
pub mod llama_native;
pub mod schema;
pub mod sidecar;
pub mod test_router;

pub use classic::ClassicChatRouter;
#[cfg(feature = "llama-cpp")]
pub use llama_native::NativeLlamaRouter;
pub use schema::{merge_router_plan, parse_and_validate_router_plan, ROUTER_PLAN_JSON_SCHEMA};
pub use sidecar::SidecarLlamaRouter;
pub use test_router::DeterministicTestChatRouter;

use crate::AiError;
use chrono::{DateTime, Utc};
use std::time::Duration;
use truongphoto_core::{ChatRole, ChatTier, RouterPlan};

/// RBAC-scoped high-level library overview passed to the conversational router so it can
/// ground open-ended, vague, and informational questions accurately.
#[derive(Debug, Clone, Default)]
pub struct LibraryChatContext {
    pub total_assets: i64,
    pub favorite_count: i64,
    pub kind_counts: Vec<(String, i64)>,
    pub date_range: Option<(String, String)>,
    pub known_folders: Vec<String>,
    pub top_tags: Vec<String>,
    pub person_counts: Vec<(String, i64)>,
    pub camera_counts: Vec<(String, i64)>,
    pub lens_counts: Vec<(String, i64)>,
    pub top_places: Vec<(String, i64)>,
    pub year_counts: Vec<(String, i64)>,
    pub top_months: Vec<(String, i64)>,
}

/// Context passed into [`ChatRouterBackend::route_turn`] for multi-turn query routing.
#[derive(Debug, Clone)]
pub struct ChatTurnInput {
    pub user_message: String,
    pub prior_filters: RouterPlan,
    pub recent_turns: Vec<(ChatRole, String)>,
    pub known_person_names: Vec<String>,
    pub known_cameras: Vec<String>,
    pub known_lenses: Vec<String>,
    pub library: LibraryChatContext,
    pub current_date_utc: DateTime<Utc>,
}

/// Candidate thumbnail passed to Tier 3 VLM visual reranking.
#[derive(Debug, Clone)]
pub struct RerankCandidate {
    pub asset_id: String,
    pub score: f32,
    pub thumb_bytes: Vec<u8>,
    pub caption_hint: Option<String>,
}

/// Visual reranking output returned by [`ChatRouterBackend::rerank_thumbnails`].
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResult {
    pub ordered_asset_ids: Vec<(String, f32)>,
    pub observation: Option<String>,
}

/// Pluggable conversational search router and Tier 3 VLM thumbnail reranker.
pub trait ChatRouterBackend: Send + Sync {
    fn route_turn(&self, tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError>;

    fn rerank_thumbnails(
        &self,
        tier: ChatTier,
        user_message: &str,
        candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, AiError>;

    fn evict_idle(&self, max_idle: Duration) -> usize;

    fn is_loaded(&self) -> bool;

    /// Checks whether the sidecar is currently alive and loaded for `tier`.
    fn is_loaded_for_tier(&self, tier: ChatTier) -> bool {
        tier != ChatTier::Tier0 && self.is_loaded()
    }

    /// Optionally pre-warms the local LLM runtime for `tier` in the background.
    /// Returns `true` if the runtime is warm/ready.
    fn warmup(&self, _tier: ChatTier) -> bool {
        false
    }

    /// Permanently shuts down the backend, preventing new sidecar spawns and killing any active runtime.
    fn shutdown(&self) {
        let _ = self.evict_idle(Duration::ZERO);
    }
}
