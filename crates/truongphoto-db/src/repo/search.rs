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

fn tokenize_search_words(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '-' && c != '.'))
        .map(|s| s.trim_end_matches('.'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn geo_synonym_variant(tok: &str) -> Option<&'static str> {
    match tok {
        "mount" => Some("mt"),
        "mt" => Some("mount"),
        "saint" => Some("st"),
        "st" => Some("saint"),
        "fort" => Some("ft"),
        "ft" => Some("fort"),
        "grad" => Some("graduation"),
        "graduation" => Some("grad"),
        "bday" => Some("birthday"),
        "birthday" => Some("bday"),
        "xmas" => Some("christmas"),
        "christmas" => Some("xmas"),
        _ => None,
    }
}

fn parse_focal_mm_token(tok: &str) -> Option<f64> {
    let lower = tok.trim().to_ascii_lowercase();
    let num_str = lower.strip_suffix("mm")?;
    let val = num_str.parse::<f64>().ok()?;
    if val.is_finite() && (1.0..=2000.0).contains(&val) {
        Some(val)
    } else {
        None
    }
}

fn parse_aperture_f_token(tok: &str) -> Option<f64> {
    let lower = tok.trim().to_ascii_lowercase();
    let num_str = lower
        .strip_prefix("f/")
        .or_else(|| lower.strip_prefix('f'))?;
    if num_str.is_empty() || !num_str.chars().next()?.is_ascii_digit() {
        return None;
    }
    let val = num_str.parse::<f64>().ok()?;
    if val.is_finite() && (0.7..=64.0).contains(&val) {
        Some(val)
    } else {
        None
    }
}

fn parse_iso_token(tok: &str) -> Option<i64> {
    let lower = tok.trim().to_ascii_lowercase();
    let num_str = lower.strip_prefix("iso")?;
    if num_str.is_empty() {
        return None;
    }
    let val = num_str.parse::<i64>().ok()?;
    if (25..=1_000_000).contains(&val) {
        Some(val)
    } else {
        None
    }
}

fn parse_shutter_speed_token(tok: &str) -> Option<f64> {
    let lower = tok.trim().to_ascii_lowercase();
    let has_unit = lower.ends_with("sec") || lower.ends_with('s');
    let stripped = lower
        .strip_suffix("sec")
        .or_else(|| lower.strip_suffix('s'))
        .unwrap_or(&lower);
    if let Some((num, den)) = stripped.split_once('/') {
        let n = num.parse::<f64>().ok()?;
        let d = den.parse::<f64>().ok()?;
        if n > 0.0 && d >= 1.0 && n / d <= 3600.0 {
            return Some(n / d);
        }
    } else if has_unit && !stripped.is_empty() {
        let val = stripped.parse::<f64>().ok()?;
        if val.is_finite() && val > 0.0 && val <= 3600.0 {
            return Some(val);
        }
    }
    None
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

    let is_admin = matches!(scope, RbacScope::AdminAll);
    if let Some(person_ids) = req.person_ids.as_ref() {
        let mut seen = HashSet::new();
        for pid in person_ids {
            let trimmed = pid.trim();
            if trimmed.is_empty() || !seen.insert(trimmed) {
                continue;
            }
            let idx = params.len() + 1;
            params.push(Value::Text(trimmed.to_string()));
            if is_admin {
                where_clauses.push(format!(
                    "EXISTS (SELECT 1 FROM faces f WHERE f.asset_id = v.id AND f.person_id = ?{idx})"
                ));
            } else {
                where_clauses.push(format!(
                    "EXISTS (SELECT 1 FROM faces f JOIN people p ON p.id = f.person_id WHERE f.asset_id = v.id AND f.person_id = ?{idx} AND p.is_hidden = 0)"
                ));
            }
        }
    }

    if let Some(has_people) = req.has_people {
        if has_people {
            if is_admin {
                where_clauses.push(
                    "EXISTS (SELECT 1 FROM faces f_hp WHERE f_hp.asset_id = v.id)".to_string(),
                );
            } else {
                where_clauses.push(
                    "EXISTS (SELECT 1 FROM faces f_hp JOIN people p_hp ON p_hp.id = f_hp.person_id WHERE f_hp.asset_id = v.id AND p_hp.is_hidden = 0)"
                        .to_string(),
                );
            }
        } else if is_admin {
            where_clauses.push(
                "NOT EXISTS (SELECT 1 FROM faces f_np WHERE f_np.asset_id = v.id)".to_string(),
            );
        } else {
            where_clauses.push(
                "NOT EXISTS (SELECT 1 FROM faces f_np JOIN people p_np ON p_np.id = f_np.person_id WHERE f_np.asset_id = v.id AND p_np.is_hidden = 0)"
                    .to_string(),
            );
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
                "EXISTS (SELECT 1 FROM media_tags mt WHERE mt.asset_id = v.id AND mt.state = 'active' AND LOWER(mt.tag) = LOWER(?{idx}))"
            ));
        }
    }

    if let Some(camera_model) = req.camera_model.as_deref() {
        let tokens = tokenize_search_words(camera_model);
        if !tokens.is_empty() {
            let mut token_checks = Vec::with_capacity(tokens.len());
            for tok in tokens {
                let idx = params.len() + 1;
                params.push(Value::Text(format!("%{}%", escape_like_lower(&tok))));
                token_checks.push(format!(
                    "LOWER(COALESCE(dpm_c.camera_make, '') || ' ' || COALESCE(dpm_c.camera_model, '') || ' ' || COALESCE(v.camera_model, '')) LIKE ?{idx} ESCAPE '\\'"
                ));
            }
            where_clauses.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_c WHERE dpm_c.asset_id = v.id AND {})",
                token_checks.join(" AND ")
            ));
        }
    }

    let has_photo_exif_filters = req
        .lens_model
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || req.focal_length_min_mm.is_some()
        || req.focal_length_max_mm.is_some()
        || req.aperture_min_f.is_some()
        || req.aperture_max_f.is_some()
        || req.iso_min.is_some()
        || req.iso_max.is_some()
        || req.shutter_speed_min_sec.is_some()
        || req.shutter_speed_max_sec.is_some();

    if has_photo_exif_filters {
        let mut exif_checks = Vec::new();

        if let Some(lens) = req.lens_model.as_deref() {
            for tok in tokenize_search_words(lens) {
                let norm_tok = tok.replace("f/", "f");
                let idx = params.len() + 1;
                params.push(Value::Text(format!("%{}%", escape_like_lower(&norm_tok))));
                exif_checks.push(format!(
                    "REPLACE(LOWER(COALESCE(dpm_e.lens_model, '')), 'f/', 'f') LIKE ?{idx} ESCAPE '\\'"
                ));
            }
        }

        let (fl_min, fl_max) = match (req.focal_length_min_mm, req.focal_length_max_mm) {
            (Some(lo), Some(hi)) if (hi - lo).abs() < 0.2 => (Some(lo - 0.6), Some(hi + 0.6)),
            pair => pair,
        };
        if let Some(lo) = fl_min {
            let idx = params.len() + 1;
            params.push(Value::Real(lo));
            exif_checks.push(format!("dpm_e.focal_length_mm >= ?{idx}"));
        }
        if let Some(hi) = fl_max {
            let idx = params.len() + 1;
            params.push(Value::Real(hi));
            exif_checks.push(format!("dpm_e.focal_length_mm <= ?{idx}"));
        }

        let (ap_min, ap_max) = match (req.aperture_min_f, req.aperture_max_f) {
            (Some(lo), Some(hi)) if (hi - lo).abs() < 0.1 => (Some(lo - 0.15), Some(hi + 0.15)),
            pair => pair,
        };
        if let Some(lo) = ap_min {
            let idx = params.len() + 1;
            params.push(Value::Real(lo));
            exif_checks.push(format!("dpm_e.aperture_f >= ?{idx}"));
        }
        if let Some(hi) = ap_max {
            let idx = params.len() + 1;
            params.push(Value::Real(hi));
            exif_checks.push(format!("dpm_e.aperture_f <= ?{idx}"));
        }

        if let Some(lo) = req.iso_min {
            let idx = params.len() + 1;
            params.push(Value::Integer(i64::from(lo)));
            exif_checks.push(format!("dpm_e.iso >= ?{idx}"));
        }
        if let Some(hi) = req.iso_max {
            let idx = params.len() + 1;
            params.push(Value::Integer(i64::from(hi)));
            exif_checks.push(format!("dpm_e.iso <= ?{idx}"));
        }

        let (sh_min, sh_max) = match (req.shutter_speed_min_sec, req.shutter_speed_max_sec) {
            (Some(lo), Some(hi)) if lo > 0.0 && ((hi - lo).abs() / lo) < 0.05 => {
                (Some(lo * 0.85), Some(hi * 1.15))
            }
            (Some(lo), None) => (Some(lo * 0.97), None),
            (None, Some(hi)) => (None, Some(hi * 1.03)),
            pair => pair,
        };
        if let Some(lo) = sh_min {
            let idx = params.len() + 1;
            params.push(Value::Real(lo));
            exif_checks.push(format!("dpm_e.shutter_speed_sec >= ?{idx}"));
        }
        if let Some(hi) = sh_max {
            let idx = params.len() + 1;
            params.push(Value::Real(hi));
            exif_checks.push(format!("dpm_e.shutter_speed_sec <= ?{idx}"));
        }

        if !exif_checks.is_empty() {
            where_clauses.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_e WHERE dpm_e.asset_id = v.id AND {})",
                exif_checks.join(" AND ")
            ));
        }
    }

    if let Some(orientation) = req.orientation.as_deref().map(str::trim) {
        match orientation.to_ascii_lowercase().as_str() {
            "portrait" | "vertical" => {
                where_clauses
                    .push("(v.width > 0 AND v.height > 0 AND v.height > v.width)".to_string());
            }
            "landscape" | "horizontal" => {
                where_clauses
                    .push("(v.width > 0 AND v.height > 0 AND v.width > v.height)".to_string());
            }
            "square" => {
                where_clauses
                    .push("(v.width > 0 AND v.height > 0 AND v.width = v.height)".to_string());
            }
            _ => {}
        }
    }

    if let Some(geo_query) = req.geo_query.as_deref() {
        let tokens = tokenize_search_words(geo_query);
        if !tokens.is_empty() {
            let mut geo_token_checks = Vec::with_capacity(tokens.len());
            for tok in tokens {
                let idx = params.len() + 1;
                params.push(Value::Text(format!("%{}%", escape_like_lower(&tok))));
                let mut per_tok = vec![
                    format!("LOWER(COALESCE(v.effective_geo_city, '')) LIKE ?{idx} ESCAPE '\\'"),
                    format!("LOWER(COALESCE(v.effective_geo_admin1, '')) LIKE ?{idx} ESCAPE '\\'"),
                    format!("LOWER(COALESCE(v.effective_geo_country, '')) LIKE ?{idx} ESCAPE '\\'"),
                    format!(
                        "LOWER(COALESCE(v.effective_geo_country_code, '')) LIKE ?{idx} ESCAPE '\\'"
                    ),
                    format!("LOWER(COALESCE(v.effective_title, '')) LIKE ?{idx} ESCAPE '\\'"),
                    format!("LOWER(COALESCE(v.effective_description, '')) LIKE ?{idx} ESCAPE '\\'"),
                    format!(
                        "EXISTS (SELECT 1 FROM asset_files af_g WHERE af_g.asset_id = v.id AND LOWER(af_g.rel_path) LIKE ?{idx} ESCAPE '\\')"
                    ),
                ];
                if let Some(alt) = geo_synonym_variant(&tok) {
                    let alt_idx = params.len() + 1;
                    params.push(Value::Text(format!("%{}%", escape_like_lower(alt))));
                    per_tok.push(format!(
                        "LOWER(COALESCE(v.effective_geo_city, '')) LIKE ?{alt_idx} ESCAPE '\\'"
                    ));
                    per_tok.push(format!(
                        "LOWER(COALESCE(v.effective_geo_admin1, '')) LIKE ?{alt_idx} ESCAPE '\\'"
                    ));
                    per_tok.push(format!(
                        "LOWER(COALESCE(v.effective_title, '')) LIKE ?{alt_idx} ESCAPE '\\'"
                    ));
                    per_tok.push(format!(
                        "LOWER(COALESCE(v.effective_description, '')) LIKE ?{alt_idx} ESCAPE '\\'"
                    ));
                    per_tok.push(format!(
                        "EXISTS (SELECT 1 FROM asset_files af_ga WHERE af_ga.asset_id = v.id AND LOWER(af_ga.rel_path) LIKE ?{alt_idx} ESCAPE '\\')"
                    ));
                }
                geo_token_checks.push(format!("({})", per_tok.join(" OR ")));
            }
            where_clauses.push(format!("({})", geo_token_checks.join(" AND ")));
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

fn append_free_text_metadata_clauses(
    query: &str,
    params: &mut Vec<Value>,
    where_clauses: &mut Vec<String>,
) -> bool {
    let raw_tokens = tokenize_search_words(query);
    if raw_tokens.is_empty() {
        return false;
    }

    // Merge "iso" + "6400" into a single "iso6400" token so "iso 6400" matches dpm.iso cleanly.
    let mut tokens: Vec<String> = Vec::with_capacity(raw_tokens.len());
    let mut i = 0usize;
    while i < raw_tokens.len() {
        if raw_tokens[i] == "iso" {
            if let Some(next) = raw_tokens.get(i + 1) {
                if next.chars().all(|c| c.is_ascii_digit()) {
                    tokens.push(format!("iso{next}"));
                    i += 2;
                    continue;
                }
            }
        }
        tokens.push(raw_tokens[i].clone());
        i += 1;
    }

    // Ignore overly long queries or pure punctuation so metadata matching stays precise.
    let meaningful: Vec<String> = tokens
        .into_iter()
        .filter(|t| t.chars().any(|c| c.is_alphanumeric()))
        .take(6)
        .collect();
    if meaningful.is_empty() {
        return false;
    }

    for tok in meaningful {
        let mut ors = Vec::new();
        if let Some(fl) = parse_focal_mm_token(&tok) {
            let lo_idx = params.len() + 1;
            params.push(Value::Real(fl - 0.6));
            let hi_idx = params.len() + 1;
            params.push(Value::Real(fl + 0.6));
            ors.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_q WHERE dpm_q.asset_id = v.id AND dpm_q.focal_length_mm BETWEEN ?{lo_idx} AND ?{hi_idx})"
            ));
        }
        if let Some(ap) = parse_aperture_f_token(&tok) {
            let lo_idx = params.len() + 1;
            params.push(Value::Real(ap - 0.15));
            let hi_idx = params.len() + 1;
            params.push(Value::Real(ap + 0.15));
            ors.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_a WHERE dpm_a.asset_id = v.id AND dpm_a.aperture_f BETWEEN ?{lo_idx} AND ?{hi_idx})"
            ));
        }
        if let Some(iso_val) = parse_iso_token(&tok) {
            let iso_idx = params.len() + 1;
            params.push(Value::Integer(iso_val));
            ors.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_i WHERE dpm_i.asset_id = v.id AND dpm_i.iso = ?{iso_idx})"
            ));
        }
        if let Some(shutter_sec) = parse_shutter_speed_token(&tok) {
            let lo_idx = params.len() + 1;
            params.push(Value::Real(shutter_sec * 0.85));
            let hi_idx = params.len() + 1;
            params.push(Value::Real(shutter_sec * 1.15));
            ors.push(format!(
                "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_s WHERE dpm_s.asset_id = v.id AND dpm_s.shutter_speed_sec BETWEEN ?{lo_idx} AND ?{hi_idx})"
            ));
        }

        let idx = params.len() + 1;
        params.push(Value::Text(format!("%{}%", escape_like_lower(&tok))));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_title, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_description, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.camera_model, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_geo_city, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_geo_admin1, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_geo_country, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "LOWER(COALESCE(v.effective_geo_country_code, '')) LIKE ?{idx} ESCAPE '\\'"
        ));
        ors.push(format!(
            "EXISTS (SELECT 1 FROM deterministic_photo_meta dpm_t WHERE dpm_t.asset_id = v.id AND (LOWER(COALESCE(dpm_t.camera_make, '')) LIKE ?{idx} ESCAPE '\\' OR LOWER(COALESCE(dpm_t.lens_model, '')) LIKE ?{idx} ESCAPE '\\'))"
        ));
        ors.push(format!(
            "EXISTS (SELECT 1 FROM asset_files afq WHERE afq.asset_id = v.id AND LOWER(afq.rel_path) LIKE ?{idx} ESCAPE '\\')"
        ));
        if let Some(alt) = geo_synonym_variant(&tok) {
            let alt_idx = params.len() + 1;
            params.push(Value::Text(format!("%{}%", escape_like_lower(alt))));
            ors.push(format!(
                "EXISTS (SELECT 1 FROM asset_files afqa WHERE afqa.asset_id = v.id AND LOWER(afqa.rel_path) LIKE ?{alt_idx} ESCAPE '\\')"
            ));
        }
        ors.push(format!(
            "EXISTS (SELECT 1 FROM media_tags mtq WHERE mtq.asset_id = v.id AND mtq.state = 'active' AND LOWER(mtq.tag) LIKE ?{idx} ESCAPE '\\')"
        ));
        ors.push(format!(
            "EXISTS (SELECT 1 FROM faces fq JOIN people pq ON pq.id = fq.person_id WHERE fq.asset_id = v.id AND pq.is_hidden = 0 AND LOWER(COALESCE(pq.name, '')) LIKE ?{idx} ESCAPE '\\')"
        ));

        where_clauses.push(format!("({})", ors.join(" OR ")));
    }

    true
}

/// Returns RBAC-scoped asset IDs matching free-text deterministic metadata (camera make/model,
/// lens, focal length, aperture, ISO, folder/filename, title, tags, and named people) for
/// hybrid search fusion.
pub fn search_metadata_matches(
    conn: &Connection,
    scope: &RbacScope,
    req: &SearchRequest,
    raw_query: &str,
    limit: usize,
) -> DbResult<Vec<String>> {
    if limit == 0 || raw_query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut params: Vec<Value> = Vec::new();
    let mut where_clauses = Vec::new();
    if !append_deterministic_filters(scope, req, &mut params, &mut where_clauses) {
        return Ok(Vec::new());
    }
    if !append_free_text_metadata_clauses(raw_query, &mut params, &mut where_clauses) {
        return Ok(Vec::new());
    }

    let limit_idx = params.len() + 1;
    params.push(Value::Integer(limit.min(MAX_SEARCH_LIMIT as usize) as i64));

    let sql = format!(
        "SELECT v.id
         FROM v_assets_effective v
         WHERE {}
         ORDER BY v.effective_capture_at_ms DESC, v.id DESC
         LIMIT ?{limit_idx}",
        where_clauses.join(" AND ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(params), |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Lists distinct camera models and lens models visible within `scope` for AI chat router context.
pub fn list_distinct_gear(
    conn: &Connection,
    scope: &RbacScope,
) -> DbResult<(Vec<String>, Vec<String>)> {
    let mut params_c: Vec<Value> = Vec::new();
    let folder_c = rbac_sql::folder_clause("a", scope, &mut params_c);
    let cam_sql = format!(
        "SELECT TRIM(dpm.camera_model) AS cam
         FROM deterministic_photo_meta dpm
         JOIN assets a ON a.id = dpm.asset_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {folder_c}
           AND dpm.camera_model IS NOT NULL
           AND TRIM(dpm.camera_model) != ''
         GROUP BY cam
         ORDER BY COUNT(*) DESC, cam ASC
         LIMIT 25"
    );
    let mut cam_stmt = conn.prepare(&cam_sql)?;
    let cameras = cam_stmt
        .query_map(params_from_iter(params_c), |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut params_l: Vec<Value> = Vec::new();
    let folder_l = rbac_sql::folder_clause("a", scope, &mut params_l);
    let lens_sql = format!(
        "SELECT TRIM(dpm.lens_model) AS lens
         FROM deterministic_photo_meta dpm
         JOIN assets a ON a.id = dpm.asset_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {folder_l}
           AND dpm.lens_model IS NOT NULL
           AND TRIM(dpm.lens_model) != ''
         GROUP BY lens
         ORDER BY COUNT(*) DESC, lens ASC
         LIMIT 30"
    );
    let mut lens_stmt = conn.prepare(&lens_sql)?;
    let lenses = lens_stmt
        .query_map(params_from_iter(params_l), |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok((cameras, lenses))
}

/// RBAC-scoped overview of visible library contents for conversational AI grounding.
#[derive(Debug, Clone, Default)]
pub struct LibraryChatSummary {
    pub total_assets: i64,
    pub date_range: Option<(String, String)>,
    pub folders: Vec<(String, i64)>,
    pub top_tags: Vec<String>,
    pub person_counts: Vec<(String, i64)>,
}

/// Computes an RBAC-scoped summary of the visible library (total count, capture date span,
/// top subfolder/event groups, active tags, and named people with photo counts).
pub fn load_library_chat_summary(
    conn: &Connection,
    scope: &RbacScope,
) -> DbResult<LibraryChatSummary> {
    // 1. Total count + capture date range (strictly active, non-hidden assets).
    let mut params_t: Vec<Value> = Vec::new();
    let folder_t = rbac_sql::folder_clause("v", scope, &mut params_t);
    let totals_sql = format!(
        "SELECT COUNT(*),
                strftime('%Y-%m-%d', MIN(v.effective_capture_at_ms) / 1000, 'unixepoch'),
                strftime('%Y-%m-%d', MAX(v.effective_capture_at_ms) / 1000, 'unixepoch')
         FROM v_assets_effective v
         WHERE v.deleted_at_ms IS NULL
           AND v.is_hidden = 0
           AND {folder_t}"
    );
    let (total_assets, min_date, max_date): (i64, Option<String>, Option<String>) = conn
        .query_row(&totals_sql, params_from_iter(params_t), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let date_range = match (min_date, max_date) {
        (Some(lo), Some(hi)) if !lo.is_empty() && !hi.is_empty() => Some((lo, hi)),
        _ => None,
    };

    // 2. Top subfolder / event collections (handles both `Event/img.jpg` and nested `2026/Event/img.jpg`,
    // falling back to watched folder name when files sit at the root).
    let mut params_f: Vec<Value> = Vec::new();
    let folder_f = rbac_sql::folder_clause("a", scope, &mut params_f);
    let folders_sql = format!(
        "SELECT
             CASE
                 WHEN instr(replace(af.rel_path, '\\', '/'), '/') > 1 THEN
                     CASE
                         WHEN instr(substr(replace(af.rel_path, '\\', '/'), instr(replace(af.rel_path, '\\', '/'), '/') + 1), '/') > 1 THEN
                             substr(
                                 substr(replace(af.rel_path, '\\', '/'), instr(replace(af.rel_path, '\\', '/'), '/') + 1),
                                 1,
                                 instr(substr(replace(af.rel_path, '\\', '/'), instr(replace(af.rel_path, '\\', '/'), '/') + 1), '/') - 1
                             )
                         ELSE
                             substr(replace(af.rel_path, '\\', '/'), 1, instr(replace(af.rel_path, '\\', '/'), '/') - 1)
                     END
                 ELSE COALESCE(NULLIF(TRIM(f.name), ''), 'Library')
             END AS grp,
             COUNT(DISTINCT a.id) AS cnt
         FROM assets a
         JOIN asset_files af ON af.asset_id = a.id
         LEFT JOIN folders f ON f.id = a.folder_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {folder_f}
         GROUP BY grp
         ORDER BY cnt DESC, grp ASC
         LIMIT 20"
    );
    let mut folders_stmt = conn.prepare(&folders_sql)?;
    let folders = folders_stmt
        .query_map(params_from_iter(params_f), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 3. Top active tags.
    let mut params_tag: Vec<Value> = Vec::new();
    let folder_tag = rbac_sql::folder_clause("a", scope, &mut params_tag);
    let tags_sql = format!(
        "SELECT TRIM(mt.tag) AS tag
         FROM media_tags mt
         JOIN assets a ON a.id = mt.asset_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {folder_tag}
           AND mt.state = 'active'
           AND TRIM(mt.tag) != ''
         GROUP BY LOWER(TRIM(mt.tag))
         ORDER BY COUNT(DISTINCT a.id) DESC, tag ASC
         LIMIT 15"
    );
    let mut tags_stmt = conn.prepare(&tags_sql)?;
    let top_tags = tags_stmt
        .query_map(params_from_iter(params_tag), |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    // 4. Visible named people with distinct asset counts (always exclude hidden people).
    let mut params_p: Vec<Value> = Vec::new();
    let folder_p = rbac_sql::folder_clause("a", scope, &mut params_p);
    let people_sql = format!(
        "SELECT TRIM(p.name) AS pname, COUNT(DISTINCT a.id) AS cnt
         FROM people p
         JOIN faces fc ON fc.person_id = p.id
         JOIN assets a ON a.id = fc.asset_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND p.is_hidden = 0
           AND {folder_p}
           AND p.name IS NOT NULL
           AND TRIM(p.name) != ''
         GROUP BY p.id
         ORDER BY cnt DESC, pname ASC
         LIMIT 25"
    );
    let mut people_stmt = conn.prepare(&people_sql)?;
    let person_counts = people_stmt
        .query_map(params_from_iter(params_p), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(LibraryChatSummary {
        total_assets,
        date_range,
        folders,
        top_tags,
        person_counts,
    })
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
            append_free_text_metadata_clauses(trimmed, &mut params, &mut where_clauses);
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

    let order_clause = match req
        .sort_by
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("capture_asc") => "v.effective_capture_at_ms ASC, v.id ASC",
        Some("focal_asc") => {
            "CASE WHEN dpm_s.focal_length_mm IS NULL THEN 1 ELSE 0 END ASC, dpm_s.focal_length_mm ASC, v.effective_capture_at_ms DESC"
        }
        Some("focal_desc") => {
            "CASE WHEN dpm_s.focal_length_mm IS NULL THEN 1 ELSE 0 END ASC, dpm_s.focal_length_mm DESC, v.effective_capture_at_ms DESC"
        }
        Some("iso_desc") => {
            "CASE WHEN dpm_s.iso IS NULL THEN 1 ELSE 0 END ASC, dpm_s.iso DESC, v.effective_capture_at_ms DESC"
        }
        Some("aperture_asc") => {
            "CASE WHEN dpm_s.aperture_f IS NULL THEN 1 ELSE 0 END ASC, dpm_s.aperture_f ASC, v.effective_capture_at_ms DESC"
        }
        Some("shutter_asc") => {
            "CASE WHEN dpm_s.shutter_speed_sec IS NULL THEN 1 ELSE 0 END ASC, dpm_s.shutter_speed_sec ASC, v.effective_capture_at_ms DESC"
        }
        Some("shutter_desc") => {
            "CASE WHEN dpm_s.shutter_speed_sec IS NULL THEN 1 ELSE 0 END ASC, dpm_s.shutter_speed_sec DESC, v.effective_capture_at_ms DESC"
        }
        _ => "v.effective_capture_at_ms DESC, v.id DESC",
    };

    let list_sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM v_assets_effective v
         LEFT JOIN deterministic_photo_meta dpm_s ON dpm_s.asset_id = v.id
         WHERE {where_sql}
         ORDER BY {order_clause}
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

/// Sortable capture & EXIF fields used to honor `sort_by` in Hybrid, Semantic, and OCR search modes.
#[derive(Debug, Clone, Copy, Default)]
pub struct AssetSortKey {
    pub capture_at_ms: i64,
    pub focal_length_mm: Option<f64>,
    pub aperture_f: Option<f64>,
    pub iso: Option<i64>,
    pub shutter_speed_sec: Option<f64>,
}

/// Loads RBAC-scoped sort keys for a set of matched `asset_ids`.
pub fn load_asset_sort_keys(
    conn: &Connection,
    scope: &RbacScope,
    asset_ids: &[String],
) -> DbResult<HashMap<String, AssetSortKey>> {
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
            "SELECT v.id, v.effective_capture_at_ms, dpm.focal_length_mm, dpm.aperture_f, dpm.iso, dpm.shutter_speed_sec
             FROM v_assets_effective v
             LEFT JOIN deterministic_photo_meta dpm ON dpm.asset_id = v.id
             WHERE v.id IN ({})
               AND v.deleted_at_ms IS NULL
               AND v.is_hidden = 0
               AND {rbac_clause}",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                AssetSortKey {
                    capture_at_ms: row.get::<_, i64>(1)?,
                    focal_length_mm: row.get::<_, Option<f64>>(2)?,
                    aperture_f: row.get::<_, Option<f64>>(3)?,
                    iso: row.get::<_, Option<i64>>(4)?,
                    shutter_speed_sec: row.get::<_, Option<f64>>(5)?,
                },
            ))
        })?;
        for row in rows {
            let (id, key) = row?;
            map.insert(id, key);
        }
    }

    Ok(map)
}
