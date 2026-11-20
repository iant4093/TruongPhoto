use std::collections::HashMap;

/// Individual vector search hit mapped back to its logical `(asset_id, timestamp_ms)`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeHit {
    pub vector_id: u32,
    pub asset_id: String,
    pub timestamp_ms: i64,
    pub score: f32,
}

/// Deduplicated asset hit after collapsing multiple video keyframe embeddings.
#[derive(Debug, Clone, PartialEq)]
pub struct CollapsedAssetHit {
    pub asset_id: String,
    pub score: f32,
    pub timestamp_sec: Option<f64>,
}

/// Over-fetch count used before collapsing multiple video keyframes per asset.
pub fn overfetch_k(k: usize) -> usize {
    k.saturating_mul(4).max(40)
}

/// Collapses multiple keyframe hits belonging to the same `asset_id` into a single
/// hit retaining the highest similarity score and its associated `timestamp_sec`.
pub fn collapse_video_keyframes(hits: Vec<KeyframeHit>, k: usize) -> Vec<CollapsedAssetHit> {
    if k == 0 || hits.is_empty() {
        return Vec::new();
    }

    let mut best_by_asset: HashMap<String, (f32, i64)> = HashMap::new();
    for hit in hits {
        match best_by_asset.get_mut(&hit.asset_id) {
            Some(existing) => {
                let better = hit.score > existing.0
                    || (hit.score == existing.0 && hit.timestamp_ms < existing.1);
                if better {
                    *existing = (hit.score, hit.timestamp_ms);
                }
            }
            None => {
                best_by_asset.insert(hit.asset_id, (hit.score, hit.timestamp_ms));
            }
        }
    }

    let mut collapsed: Vec<CollapsedAssetHit> = best_by_asset
        .into_iter()
        .map(|(asset_id, (score, timestamp_ms))| CollapsedAssetHit {
            asset_id,
            score,
            timestamp_sec: if timestamp_ms > 0 {
                Some(timestamp_ms as f64 / 1000.0)
            } else {
                None
            },
        })
        .collect();

    collapsed.sort_unstable_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.asset_id.cmp(&b.asset_id))
    });
    collapsed.truncate(k);
    collapsed
}
