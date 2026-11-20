use crate::chat::{
    ChatRouterBackend, ChatTurnInput, ClassicChatRouter, RerankCandidate, RerankResult,
};
use crate::AiError;
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use truongphoto_core::{ChatTier, RouterPlan};

/// Pure-Rust deterministic chat router and Tier 3 VLM reranker for unit & integration tests.
///
/// Uses [`ClassicChatRouter`] for reproducible multi-turn filter routing, tracks simulated
/// model residency via `loaded_at` for real `evict_idle` testing, and provides deterministic
/// keyword/score-based thumbnail reranking on `ChatTier::Tier3`.
#[derive(Debug, Default)]
pub struct DeterministicTestChatRouter {
    classic: ClassicChatRouter,
    loaded_at: Mutex<Option<Instant>>,
}

impl DeterministicTestChatRouter {
    pub fn new() -> Self {
        Self {
            classic: ClassicChatRouter::new(),
            loaded_at: Mutex::new(None),
        }
    }
}

impl ChatRouterBackend for DeterministicTestChatRouter {
    fn route_turn(&self, tier: ChatTier, input: &ChatTurnInput) -> Result<RouterPlan, AiError> {
        if tier != ChatTier::Tier0 {
            *self.loaded_at.lock() = Some(Instant::now());
        }
        self.classic.route_turn(tier, input)
    }

    fn rerank_thumbnails(
        &self,
        tier: ChatTier,
        user_message: &str,
        candidates: &[RerankCandidate],
    ) -> Result<Option<RerankResult>, AiError> {
        if tier != ChatTier::Tier3 || candidates.is_empty() {
            return Ok(None);
        }

        *self.loaded_at.lock() = Some(Instant::now());

        let keywords: Vec<String> = user_message
            .split(|c: char| !c.is_alphanumeric())
            .map(|w| w.trim().to_ascii_lowercase())
            .filter(|w| {
                w.len() >= 2
                    && !matches!(
                        w.as_str(),
                        "the" | "and" | "for" | "with" | "only" | "from" | "in" | "at" | "on"
                    )
            })
            .collect();

        let mut scored: Vec<(String, f32)> = candidates
            .iter()
            .map(|c| {
                let hint_matches = c
                    .caption_hint
                    .as_deref()
                    .map(|hint| {
                        let lower = hint.to_ascii_lowercase();
                        keywords
                            .iter()
                            .filter(|kw| lower.contains(kw.as_str()))
                            .count() as f32
                    })
                    .unwrap_or(0.0);
                let combined_score = if hint_matches > 0.0 {
                    hint_matches + c.score * 0.1
                } else {
                    c.score
                };
                (c.asset_id.clone(), combined_score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let observation = Some(format!(
            "VLM reranked {} candidates for \"{}\".",
            scored.len(),
            user_message.trim()
        ));

        Ok(Some(RerankResult {
            ordered_asset_ids: scored,
            observation,
        }))
    }

    fn evict_idle(&self, max_idle: Duration) -> usize {
        let mut guard = self.loaded_at.lock();
        let Some(loaded) = *guard else {
            return 0;
        };
        if Instant::now().saturating_duration_since(loaded) >= max_idle {
            *guard = None;
            1
        } else {
            0
        }
    }

    fn is_loaded(&self) -> bool {
        self.loaded_at.lock().is_some()
    }
}
