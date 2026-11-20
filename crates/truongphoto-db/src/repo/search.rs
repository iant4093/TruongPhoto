use crate::error::DbResult;
use crate::rbac_sql;
use crate::repo::assets::{row_to_asset_summary, ASSET_SUMMARY_COLUMNS};
use roaring::RoaringBitmap;
use rusqlite::{params_from_iter, types::Value, Connection};
use std::collections::{HashMap, HashSet};
use truongphoto_core::models::{AiTier, AssetSummary, SearchRequest};
use truongphoto_core::rbac::RbacScope;

const DEFAULT_SEARCH_LIMIT: u32 = 60;
const MAX_SEARCH_LIMIT: u32 = 500;

/// RBAC- and filter-scoped candidate assets and vision vector slots for hybrid search.
#[derive(Debug, Clone, Default)]
pub struct FilteredSearchCandidates {
    pub asset_ids: HashSet<String>,
    pub vector_ids: RoaringBitmap,
}

fn escape_like_lower(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.trim().to_ascii_lowercase().chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            c => out.push(c),
        }
    }
    out
}

fn append_deterministic_filters(
    scope: &RbacScope,
    req: &SearchRequest,
    params: &mut Vec<Value>,
    where_clauses: &mut Vec<String>,
) -> bool {
    if let Some(folder_id) = req.folder_id.as_deref() {
        if !scope.allows_folder(folder_id) {
            return false;
        }
        let idx = params.len() + 1;
        params.push(Value::Text(folder_id.to_string()));
        where_clauses.push(format!("v.folder_id = ?{idx}"));
    }

    where_clauses.push("v.deleted_at_ms IS NULL".to_string());
    where_clauses.push("v.is_hidden = 0".to_string());
    where_clauses.push(rbac_sql::folder_clause("v", scope, params));

    if let Some(person_ids) = req.person_ids.as_ref() {
        let mut seen = HashSet::new();
        for pid in person_ids {
            let trimmed = pid.trim();
            if trimmed.is_empty() || !seen.insert(trimmed) {
                continue;
            }
            let idx = params.len() + 1;
            params.push(Value::Text(trimmed.to_string()));
            where_clauses.push(format!(
                "EXISTS (SELECT 1 FROM faces f WHERE f.asset_id = v.id AND f.person_id = ?{idx})"
            ));
        }
    }

    if let Some(tags) = req.tags.as_ref() {
        let mut seen = HashSet::new();
        for tag in tags {
            let trimmed = tag.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if !seen.insert(lower) {
                continue;
            }
            let idx = params.len() + 1;
            params.push(Value::Text(trimmed.to_string()));
            where_clauses.push(format!(
                "EXISTS (SELECT 1 FROM media_tags mt WHERE mt.asset_id = v.id AND mt.state = 'active' AND mt.tag = ?{idx})"
            ));
        }
    }

    if let Some(camera_model) = req.camera_model.as_deref() {
        let trimmed = camera_model.trim();
        if !trimmed.is_empty() {
            let idx = params.len() + 1;
            params.push(Value::Text(format!("%{}%", escape_like_lower(trimmed))));
            where_clauses.push(format!(
                "LOWER(COALESCE(v.camera_model, '')) LIKE ?{idx} ESCAPE '\\'"
            ));
        }
    }

    if let Some(geo_query) = req.geo_query.as_deref() {
        let trimmed = geo_query.trim();
        if !trimmed.is_empty() {
            let idx = params.len() + 1;
            params.push(Value::Text(format!("%{}%", escape_like_lower(trimmed))));
            where_clauses.push(format!(
                "(LOWER(COALESCE(v.effective_geo_city, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_geo_admin1, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_geo_country, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_geo_country_code, '')) LIKE ?{idx} ESCAPE '\\')"
            ));
        }
    }

    if let Some(kind) = req.kind {
        let idx = params.len() + 1;
        params.push(Value::Text(kind.as_str().to_string()));
        where_clauses.push(format!("v.kind = ?{idx}"));
    }

    if let Some(favorite) = req.favorite {
        let idx = params.len() + 1;
        params.push(Value::Integer(i64::from(favorite)));
        where_clauses.push(format!("v.is_favorite = ?{idx}"));
    }

    if let Some(date_from_ms) = req.date_from_ms {
        let idx = params.len() + 1;
        params.push(Value::Integer(date_from_ms));
        where_clauses.push(format!("v.effective_capture_at_ms >= ?{idx}"));
    }

    if let Some(date_to_ms) = req.date_to_ms {
        let idx = params.len() + 1;
        params.push(Value::Integer(date_to_ms));
        where_clauses.push(format!("v.effective_capture_at_ms <= ?{idx}"));
    }

    true
}

/// Builds the RBAC- and filter-scoped candidate asset set and `vision_embeddings` `RoaringBitmap`.
pub fn build_filtered_candidates(
    conn: &Connection,
    scope: &RbacScope,
    req: &SearchRequest,
    active_tier: AiTier,
) -> DbResult<FilteredSearchCandidates> {
    let mut params: Vec<Value> = vec![Value::Text(active_tier.as_str().to_string())];
    let mut where_clauses = Vec::new();
    if !append_deterministic_filters(scope, req, &mut params, &mut where_clauses) {
        return Ok(FilteredSearchCandidates::default());
    }

    let sql = format!(
        "SELECT v.id, ve.vector_id
         FROM v_assets_effective v
         LEFT JOIN vision_embeddings ve
           ON ve.asset_id = v.id AND ve.model_tier = ?1
         WHERE {}",
        where_clauses.join(" AND ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;
    let mut candidates = FilteredSearchCandidates::default();

    while let Some(row) = rows.next()? {
        let asset_id: String = row.get(0)?;
        let vector_id: Option<i64> = row.get(1)?;
        if !candidates.asset_ids.contains(&asset_id) {
            candidates.asset_ids.insert(asset_id);
        }
        if let Some(vid) = vector_id.and_then(|v| u32::try_from(v).ok()) {
            candidates.vector_ids.insert(vid);
        }
    }

    Ok(candidates)
}

/// Sanitizes arbitrary user text into a safe FTS5 `MATCH` query expression.
///
/// Each whitespace-separated token containing at least one alphanumeric character is quoted
/// as an FTS5 string literal (`"..."` with internal `"` escaped as `""`), preventing FTS5
/// operator/punctuation syntax errors.
fn sanitize_fts5_query(raw_query: &str) -> Option<String> {
    let mut terms = Vec::new();
    for token in raw_query.split_whitespace() {
        let cleaned: String = token.chars().filter(|&c| c != '\0').collect();
        if !cleaned.chars().any(char::is_alphanumeric) {
            continue;
        }
        let escaped = cleaned.replace('"', "\"\"");
        terms.push(format!("\"{escaped}\""));
    }

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Runs an RBAC-scoped FTS5 `MATCH` query over `ocr_fts`, optionally restricted to `allowed_asset_ids`.
///
/// Returns `(asset_id, positive_relevance_score, snippet)` ordered best-match first.
pub fn search_ocr_fts(
    conn: &Connection,
    scope: &RbacScope,
    raw_query: &str,
    allowed_asset_ids: Option<&HashSet<String>>,
    limit: usize,
) -> DbResult<Vec<(String, f32, String)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if let Some(allowed) = allowed_asset_ids {
        if allowed.is_empty() {
            return Ok(Vec::new());
        }
    }
    let Some(fts_query) = sanitize_fts5_query(raw_query) else {
        return Ok(Vec::new());
    };

    let mut params: Vec<Value> = vec![Value::Text(fts_query)];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);

    let sql = format!(
        "SELECT
            d.asset_id,
            bm25(ocr_fts) AS rank,
            snippet(ocr_fts, 1, '', '', '...', 12) AS snip
         FROM ocr_fts
         JOIN ocr_documents d ON d.rowid = ocr_fts.rowid
         JOIN assets a ON a.id = d.asset_id
         WHERE ocr_fts MATCH ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {rbac_clause}
         ORDER BY rank ASC, d.asset_id ASC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;
    let mut results = Vec::with_capacity(limit.min(64));

    while let Some(row) = rows.next()? {
        let asset_id: String = row.get(0)?;
        if let Some(allowed) = allowed_asset_ids {
            if !allowed.contains(&asset_id) {
                continue;
            }
        }
        let bm25_rank: f64 = row.get(1)?;
        let snippet: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
        // FTS5 bm25() returns negative scores (more negative = stronger match).
        let mag = (-bm25_rank).max(0.0);
        let score = ((mag + 0.1) / (mag + 1.0)) as f32;
        results.push((asset_id, score, snippet));
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

/// Lists active, RBAC-scoped assets matching deterministic metadata filters (`sql_only` / `filters` mode).
///
/// Returns `(items, total_count)`.
pub fn list_sql_filtered_assets(
    conn: &Connection,
    scope: &RbacScope,
    req: &SearchRequest,
) -> DbResult<(Vec<AssetSummary>, i64)> {
    let mut params: Vec<Value> = Vec::new();
    let mut where_clauses = Vec::new();
    if !append_deterministic_filters(scope, req, &mut params, &mut where_clauses) {
        return Ok((Vec::new(), 0));
    }

    if let Some(query) = req.query.as_deref() {
        let trimmed = query.trim();
        if !trimmed.is_empty() {
            let idx = params.len() + 1;
            params.push(Value::Text(format!("%{}%", escape_like_lower(trimmed))));
            where_clauses.push(format!(
                "(LOWER(COALESCE(v.effective_title, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_description, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.camera_model, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_geo_city, '')) LIKE ?{idx} ESCAPE '\\'
                  OR LOWER(COALESCE(v.effective_geo_country, '')) LIKE ?{idx} ESCAPE '\\'
                  OR EXISTS (
                      SELECT 1 FROM media_tags mtq
                      WHERE mtq.asset_id = v.id
                        AND mtq.state = 'active'
                        AND LOWER(mtq.tag) LIKE ?{idx} ESCAPE '\\'
                  ))"
            ));
        }
    }

    let where_sql = where_clauses.join(" AND ");
    let count_sql = format!("SELECT COUNT(*) FROM v_assets_effective v WHERE {where_sql}");
    let total_count: i64 = conn.query_row(&count_sql, params_from_iter(params.clone()), |row| {
        row.get(0)
    })?;

    if total_count == 0 {
        return Ok((Vec::new(), 0));
    }

    let limit = req
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let offset = req.offset.unwrap_or(0);

    let limit_idx = params.len() + 1;
    params.push(Value::Integer(i64::from(limit)));
    let offset_idx = params.len() + 1;
    params.push(Value::Integer(i64::from(offset)));

    let list_sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         WHERE {where_sql}
         ORDER BY v.effective_capture_at_ms DESC, v.id DESC
         LIMIT ?{limit_idx} OFFSET ?{offset_idx}"
    );

    let mut stmt = conn.prepare(&list_sql)?;
    let items = stmt
        .query_map(params_from_iter(params), row_to_asset_summary)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok((items, total_count))
}

/// Loads RBAC-scoped `AssetSummary` records for a list of `asset_ids`.
///
/// Deleted, hidden, or out-of-scope assets are omitted.
pub fn load_asset_summaries_by_ids(
    conn: &Connection,
    scope: &RbacScope,
    asset_ids: &[String],
) -> DbResult<HashMap<String, AssetSummary>> {
    if asset_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_ids: Vec<&str> = Vec::with_capacity(asset_ids.len());
    let mut seen: HashSet<&str> = HashSet::with_capacity(asset_ids.len());
    for id in asset_ids {
        if seen.insert(id.as_str()) {
            unique_ids.push(id.as_str());
        }
    }

    let mut map = HashMap::with_capacity(unique_ids.len());
    for chunk in unique_ids.chunks(400) {
        let mut params: Vec<Value> = Vec::with_capacity(chunk.len());
        let mut placeholders = Vec::with_capacity(chunk.len());
        for (idx, id) in chunk.iter().enumerate() {
            params.push(Value::Text((*id).to_string()));
            placeholders.push(format!("?{}", idx + 1));
        }
        let rbac_clause = rbac_sql::folder_clause("v", scope, &mut params);
        let sql = format!(
            "SELECT {ASSET_SUMMARY_COLUMNS}
             FROM v_assets_effective v
             WHERE v.id IN ({})
               AND v.deleted_at_ms IS NULL
               AND v.is_hidden = 0
               AND {rbac_clause}",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), row_to_asset_summary)?;
        for row in rows {
            let summary = row?;
            map.insert(summary.id.clone(), summary);
        }
    }

    Ok(map)
}
