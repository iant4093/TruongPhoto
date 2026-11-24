use crate::error::{DbError, DbResult};
use crate::rbac_sql;
use crate::repo::{now_ms, with_savepoint};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::{BTreeMap, HashMap, HashSet};
use truongphoto_core::models::{AssetKind, AssetSummary, FolderSubfolderView, StackMemberView};
use truongphoto_core::rbac::RbacScope;

/// Raw camera file extensions (matches `truongphoto_media::classify::is_raw_extension`).
const RAW_EXTS: &[&str] = &[
    "cr3", "cr2", "nef", "nrw", "arw", "srf", "dng", "raf", "orf", "rw2", "pef", "srw", "iiq",
    "3fr",
];

fn is_raw_ext(ext: &str) -> bool {
    let norm = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    RAW_EXTS.contains(&norm.as_str())
}

/// Returns `true` when an EXIF `Software` string identifies an image editing application
/// rather than in-camera firmware.
pub fn is_editing_software(software: &str) -> bool {
    let low = software.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    const EDITOR_KEYWORDS: &[&str] = &[
        "lightroom",
        "photoshop",
        "adobe",
        "capture one",
        "darktable",
        "rawtherapee",
        "dxo",
        "luminar",
        "affinity",
        "pixelmator",
        "photomator",
        "on1",
        "acorn",
        "gimp",
        "snapseed",
        "vsco",
        "polarr",
        "skylum",
        "topaz",
        "nik collection",
        "silkypix",
        "digital photo professional",
        "imaging edge",
        "nx studio",
        "x raw studio",
        "om workspace",
        "aftershot",
        "acdsee",
        "paintshop",
        "canva",
        "figma",
        "imagemagick",
        "exiftool",
        "apple photos",
        "photos ",
        "preview",
    ];
    EDITOR_KEYWORDS.iter().any(|kw| low.contains(kw))
}

#[inline]
const fn dhash_distance(a: i64, b: i64) -> u32 {
    ((a as u64) ^ (b as u64)).count_ones()
}

/// Candidate record loaded for EXIF-first cross-folder stack reconciliation.
#[derive(Debug, Clone)]
struct StackCandidate {
    asset_id: String,
    kind: AssetKind,
    extension: String,
    rel_path: String,
    width: Option<u32>,
    height: Option<u32>,
    file_mtime_ms: i64,
    taken_at_ms: i64,
    taken_at_has_subsec: bool,
    tz_offset_m: Option<i32>,
    camera_model: Option<String>,
    focal_length_mm: Option<f64>,
    aperture_f: Option<f64>,
    shutter_speed_sec: Option<f64>,
    iso: Option<u32>,
    software: Option<String>,
    exif_modified_at_ms: Option<i64>,
    dhash64: Option<i64>,
    existing_group_id: Option<String>,
    stack_manual: bool,
    existing_rank: i64,
}

impl StackCandidate {
    fn is_raw_asset(&self) -> bool {
        self.kind != AssetKind::RawJpgStack && is_raw_ext(&self.extension)
    }

    fn base_role_and_rank(&self) -> (&'static str, i64) {
        if self.is_raw_asset() {
            return ("raw", 100);
        }
        if self.kind == AssetKind::RawJpgStack {
            return ("jpeg", 200);
        }
        if self.software.as_deref().is_some_and(is_editing_software) {
            return ("edited", 300);
        }
        if let Some(mod_ms) = self.exif_modified_at_ms {
            if mod_ms > self.taken_at_ms.saturating_add(60_000) {
                return ("edited", 290);
            }
        }
        ("jpeg", 200)
    }

    fn lower_stem(&self) -> String {
        let filename = self.rel_path.rsplit('/').next().unwrap_or(&self.rel_path);
        filename
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(filename)
            .to_ascii_lowercase()
    }

    /// Wall-clock local-time milliseconds when timezone offset is known, else raw `taken_at_ms`.
    fn wall_clock_ms(&self) -> i64 {
        match self.tz_offset_m {
            Some(off) => self.taken_at_ms.saturating_add(i64::from(off) * 60_000),
            None => self.taken_at_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimestampMatch {
    ExactSubsec,
    SameSecond,
}

fn check_ts_pair(a_ms: i64, b_ms: i64, a_sub: bool, b_sub: bool) -> Option<TimestampMatch> {
    if a_sub && b_sub {
        if (a_ms - b_ms).abs() <= 20 {
            Some(TimestampMatch::ExactSubsec)
        } else {
            None
        }
    } else if a_ms.div_euclid(1000) == b_ms.div_euclid(1000) {
        Some(TimestampMatch::SameSecond)
    } else {
        None
    }
}

fn timestamps_match(a: &StackCandidate, b: &StackCandidate) -> Option<TimestampMatch> {
    if let Some(m) = check_ts_pair(
        a.taken_at_ms,
        b.taken_at_ms,
        a.taken_at_has_subsec,
        b.taken_at_has_subsec,
    ) {
        return Some(m);
    }
    // Timezone-naive fallback: handle editors that omit OffsetTimeOriginal while the camera wrote it.
    if a.tz_offset_m.is_some() != b.tz_offset_m.is_some() {
        return check_ts_pair(
            a.wall_clock_ms(),
            b.wall_clock_ms(),
            a.taken_at_has_subsec,
            b.taken_at_has_subsec,
        );
    }
    None
}

/// Checks whether two photo candidates share compatible camera & exposure EXIF metadata.
///
/// Rejects any conflicting EXIF field and requires at least 2 matching identity/exposure fields
/// (or 1 matching field + exact sub-second timestamp) so unrelated files never falsely stack.
fn exposure_compatible(a: &StackCandidate, b: &StackCandidate, ts_match: TimestampMatch) -> bool {
    let mut matches = 0u32;

    if let (Some(ca), Some(cb)) = (a.camera_model.as_deref(), b.camera_model.as_deref()) {
        if ca.trim().eq_ignore_ascii_case(cb.trim()) {
            matches += 1;
        } else {
            return false;
        }
    }

    if let (Some(fa), Some(fb)) = (a.focal_length_mm, b.focal_length_mm) {
        if (fa - fb).abs() <= 0.5 {
            matches += 1;
        } else {
            return false;
        }
    }

    if let (Some(aa), Some(ab)) = (a.aperture_f, b.aperture_f) {
        if (aa - ab).abs() <= 0.15 {
            matches += 1;
        } else {
            return false;
        }
    }

    if let (Some(sa), Some(sb)) = (a.shutter_speed_sec, b.shutter_speed_sec) {
        let denom = sa.abs().max(sb.abs()).max(1e-9);
        if (sa - sb).abs() <= 0.0002 || ((sa - sb).abs() / denom) <= 0.05 {
            matches += 1;
        } else {
            return false;
        }
    }

    if let (Some(ia), Some(ib)) = (a.iso, b.iso) {
        if ia.abs_diff(ib) <= 1 {
            matches += 1;
        } else {
            return false;
        }
    }

    matches >= 2 || (matches >= 1 && ts_match == TimestampMatch::ExactSubsec)
}

/// Weak tie-breaker ONLY used when multiple burst anchors share the exact same second without
/// sub-second timestamps and have nearly identical visual hashes (e.g., static tripod burst).
fn stem_related(a: &StackCandidate, b: &StackCandidate) -> bool {
    let sa = a.lower_stem();
    let sb = b.lower_stem();
    if sa.len() < 4 || sb.len() < 4 {
        return false;
    }
    sa == sb || sa.contains(&sb) || sb.contains(&sa)
}

/// Scores how well a non-RAW derivative matches a candidate anchor (higher is better).
/// Returns `None` if incompatible.
fn match_score(derivative: &StackCandidate, anchor: &StackCandidate) -> Option<i64> {
    let ts_match = timestamps_match(derivative, anchor)?;
    if !exposure_compatible(derivative, anchor, ts_match) {
        return None;
    }

    let mut score: i64 = match ts_match {
        TimestampMatch::ExactSubsec => 10_000,
        TimestampMatch::SameSecond => 1_000,
    };

    if let (Some(da), Some(db)) = (derivative.dhash64, anchor.dhash64) {
        let dist = dhash_distance(da, db);
        // For same-second matches without sub-second precision, reject visually unrelated frames
        // unless their stems explicitly link them (e.g., heavy crop of same frame).
        if ts_match == TimestampMatch::SameSecond && dist > 26 && !stem_related(derivative, anchor)
        {
            return None;
        }
        score += (64 - i64::from(dist)) * 10;
    }

    if stem_related(derivative, anchor) {
        score += 15;
    }

    Some(score)
}

/// Partitions a bucket of photo candidates into automatic stack clusters using universal EXIF rules:
/// - Never auto-stacks two RAW files together (each RAW is a distinct shutter actuation).
/// - Never auto-stacks two unedited in-camera JPEGs together without a shared RAW.
/// - Pairs Edited exports and JPEGs to their best-matching RAW anchor (or unedited JPEG anchor),
///   supporting multiple edits of the same shot and burst disambiguation via SubSec + dHash.
fn cluster_bucket(candidates: &[StackCandidate]) -> Vec<Vec<usize>> {
    let active_indices: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.stack_manual)
        .map(|(i, _)| i)
        .collect();
    if active_indices.len() < 2 {
        return Vec::new();
    }

    let raw_anchors: Vec<usize> = active_indices
        .iter()
        .copied()
        .filter(|&i| candidates[i].is_raw_asset())
        .collect();

    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut assigned = vec![false; candidates.len()];

    if !raw_anchors.is_empty() {
        let mut raw_groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for &r in &raw_anchors {
            raw_groups.insert(r, vec![r]);
            assigned[r] = true;
        }

        // Attach every compatible non-RAW candidate to its best-matching RAW anchor.
        for &idx in &active_indices {
            if assigned[idx] {
                continue;
            }
            let cand = &candidates[idx];
            let best_raw = raw_anchors
                .iter()
                .filter_map(|&r| match_score(cand, &candidates[r]).map(|s| (r, s)))
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));

            if let Some((winner_raw, _)) = best_raw {
                raw_groups.entry(winner_raw).or_default().push(idx);
                assigned[idx] = true;
            }
        }

        for r in raw_anchors {
            if let Some(group) = raw_groups.remove(&r) {
                if group.len() >= 2 {
                    clusters.push(group);
                }
            }
        }
    }

    // Handle remaining unassigned non-RAW photos (e.g., JPEG-only shoot + Edited exports, or
    // multiple Edited exports of the same shot when RAW isn't in the library).
    let remaining: Vec<usize> = active_indices
        .iter()
        .copied()
        .filter(|&i| !assigned[i])
        .collect();
    if remaining.len() >= 2 {
        let jpeg_anchors: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| candidates[i].base_role_and_rank().0 == "jpeg")
            .collect();

        if !jpeg_anchors.is_empty() {
            let mut jpeg_groups: HashMap<usize, Vec<usize>> = HashMap::new();
            for &j in &jpeg_anchors {
                jpeg_groups.insert(j, vec![j]);
                assigned[j] = true;
            }
            for &idx in &remaining {
                if assigned[idx] {
                    continue;
                }
                let cand = &candidates[idx];
                let best_jpeg = jpeg_anchors
                    .iter()
                    .filter_map(|&j| match_score(cand, &candidates[j]).map(|s| (j, s)))
                    .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)));
                if let Some((winner_j, _)) = best_jpeg {
                    jpeg_groups.entry(winner_j).or_default().push(idx);
                    assigned[idx] = true;
                }
            }
            for j in jpeg_anchors {
                if let Some(group) = jpeg_groups.remove(&j) {
                    if group.len() >= 2 {
                        clusters.push(group);
                    }
                }
            }
        }

        // Finally, cluster multiple Edited exports of the same shot (when neither RAW nor JPEG exists).
        let edited_left: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| !assigned[i] && candidates[i].base_role_and_rank().0 == "edited")
            .collect();
        let mut ed_visited = HashSet::new();
        for &seed in &edited_left {
            if !ed_visited.insert(seed) {
                continue;
            }
            let mut group = vec![seed];
            for &other in &edited_left {
                if ed_visited.contains(&other) {
                    continue;
                }
                if match_score(&candidates[other], &candidates[seed]).is_some() {
                    ed_visited.insert(other);
                    group.push(other);
                }
            }
            if group.len() >= 2 {
                clusters.push(group);
            }
        }
    }

    clusters
}

/// Computes refined `(stack_role, stack_rank)` for every member of a cluster.
///
/// Promotes a non-RAW member to `edited` (rank 260) even when EXIF `Software` was stripped if
/// its dimensions differ from the RAW in the stack or it was modified significantly later.
fn compute_group_roles_and_ranks(
    candidates: &[StackCandidate],
    member_indices: &[usize],
) -> Vec<(usize, &'static str, i64)> {
    let raw_member = member_indices
        .iter()
        .copied()
        .find(|&i| candidates[i].is_raw_asset());
    let raw_dims = raw_member.and_then(|i| candidates[i].width.zip(candidates[i].height));
    let raw_mtime = raw_member.map(|i| candidates[i].file_mtime_ms);

    member_indices
        .iter()
        .copied()
        .map(|idx| {
            let c = &candidates[idx];
            let (mut role, mut rank) = c.base_role_and_rank();
            if role == "jpeg" && !c.is_raw_asset() {
                let dims_differ = match (c.width.zip(c.height), raw_dims) {
                    (Some((w1, h1)), Some((w2, h2))) => {
                        !((w1 == w2 && h1 == h2) || (w1 == h2 && h1 == w2))
                    }
                    _ => false,
                };
                let much_newer_than_raw =
                    raw_mtime.is_some_and(|rm| c.file_mtime_ms > rm.saturating_add(5 * 60_000));
                if dims_differ || much_newer_than_raw {
                    role = "edited";
                    rank = 260;
                }
            }
            (idx, role, rank)
        })
        .collect()
}

fn load_candidates_sql_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StackCandidate> {
    let kind_str: String = row.get(1)?;
    let kind = kind_str.parse::<AssetKind>().unwrap_or(AssetKind::Photo);
    let width: Option<i64> = row.get(4)?;
    let height: Option<i64> = row.get(5)?;
    let tz: Option<i64> = row.get(9)?;
    let iso: Option<i64> = row.get(14)?;

    Ok(StackCandidate {
        asset_id: row.get(0)?,
        kind,
        extension: row.get(2)?,
        rel_path: row.get(3)?,
        width: width.and_then(|v| u32::try_from(v).ok()),
        height: height.and_then(|v| u32::try_from(v).ok()),
        file_mtime_ms: row.get(6)?,
        taken_at_ms: row.get(7)?,
        taken_at_has_subsec: row.get::<_, i64>(8)? != 0,
        tz_offset_m: tz.and_then(|v| i32::try_from(v).ok()),
        camera_model: row.get(10)?,
        focal_length_mm: row.get(11)?,
        aperture_f: row.get(12)?,
        shutter_speed_sec: row.get(13)?,
        iso: iso.and_then(|v| u32::try_from(v).ok()),
        software: row.get(15)?,
        exif_modified_at_ms: row.get(16)?,
        dhash64: row.get(17)?,
        existing_group_id: row.get(18)?,
        stack_manual: row.get::<_, i64>(19)? != 0,
        existing_rank: row.get(20)?,
    })
}

const CANDIDATE_SELECT_PREFIX: &str = "
    SELECT
        a.id,
        a.kind,
        af.extension,
        af.rel_path,
        a.width,
        a.height,
        af.mtime_ms,
        p.taken_at_ms,
        p.taken_at_has_subsec,
        a.capture_tz_offset_m,
        p.camera_model,
        p.focal_length_mm,
        p.aperture_f,
        p.shutter_speed_sec,
        p.iso,
        p.software,
        p.exif_modified_at_ms,
        a.dhash64,
        a.stack_group_id,
        a.stack_manual,
        a.stack_rank
    FROM assets a
    JOIN asset_files af ON af.id = a.primary_file_id
    JOIN deterministic_photo_meta p ON p.asset_id = a.id
    WHERE a.deleted_at_ms IS NULL
      AND a.is_hidden = 0
      AND af.is_missing = 0
      AND a.kind IN ('photo', 'raw_jpg_stack')
      AND p.taken_at_ms IS NOT NULL
";

fn apply_reconciliation_for_bucket(
    conn: &Connection,
    candidates: &[StackCandidate],
) -> DbResult<usize> {
    let clusters = cluster_bucket(candidates);
    let mut clustered_indices = HashSet::new();
    let mut used_group_ids = HashSet::new();
    let mut stacked_assets = 0usize;

    for cluster in &clusters {
        // Reuse an existing group ID from any member if not already claimed by another cluster.
        let group_id = cluster
            .iter()
            .filter_map(|&i| candidates[i].existing_group_id.as_deref())
            .find(|gid| !used_group_ids.contains(*gid))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        used_group_ids.insert(group_id.clone());

        let roles = compute_group_roles_and_ranks(candidates, cluster);
        for (idx, role, rank) in roles {
            clustered_indices.insert(idx);
            stacked_assets += 1;
            let c = &candidates[idx];
            if c.existing_group_id.as_deref() != Some(group_id.as_str()) || c.existing_rank != rank
            {
                conn.execute(
                    "UPDATE assets
                     SET stack_group_id = ?1,
                         stack_role = ?2,
                         stack_rank = ?3
                     WHERE id = ?4 AND stack_manual = 0",
                    params![group_id, role, rank, c.asset_id],
                )?;
            }
        }
    }

    // Clear stale automatic stack_group_id on unclustered non-manual candidates in this bucket.
    for (idx, c) in candidates.iter().enumerate() {
        if c.stack_manual || clustered_indices.contains(&idx) {
            continue;
        }
        let (role, rank) = c.base_role_and_rank();
        if c.existing_group_id.is_some() || c.existing_rank != rank {
            conn.execute(
                "UPDATE assets
                 SET stack_group_id = NULL,
                     stack_role = ?1,
                     stack_rank = ?2
                 WHERE id = ?3 AND stack_manual = 0",
                params![role, rank, c.asset_id],
            )?;
        }
    }

    Ok(stacked_assets)
}

/// Summary report returned by `reconcile_all_stacks`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StackReconcileReport {
    pub scanned_photos: usize,
    pub stacked_assets: usize,
    pub stack_groups: usize,
}

/// Reconciles automatic EXIF-first cross-folder stacks across the entire active photo library.
///
/// Preserves all manually edited stacks (`stack_manual = 1`).
pub fn reconcile_all_stacks(conn: &Connection) -> DbResult<StackReconcileReport> {
    with_savepoint(conn, "reconcile_all_stacks", || {
        let mut stmt = conn.prepare(CANDIDATE_SELECT_PREFIX)?;
        let all_candidates: Vec<StackCandidate> = stmt
            .query_map([], load_candidates_sql_row)?
            .collect::<Result<Vec<_>, _>>()?;

        // Group candidates by normalized capture second using union-find over both UTC second
        // and local wall-clock second so timezone-naive exports still meet timezone-aware RAWs.
        let mut buckets: HashMap<i64, Vec<usize>> = HashMap::new();
        for (idx, c) in all_candidates.iter().enumerate() {
            let utc_sec = c.taken_at_ms.div_euclid(1000);
            buckets.entry(utc_sec).or_default().push(idx);
            if c.tz_offset_m.is_some() {
                let wall_sec = c.wall_clock_ms().div_euclid(1000);
                if wall_sec != utc_sec {
                    buckets.entry(wall_sec).or_default().push(idx);
                }
            }
        }

        // Deduplicate connected components of candidates sharing any bucket key.
        let mut parent: Vec<usize> = (0..all_candidates.len()).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for indices in buckets.values() {
            if indices.len() < 2 {
                continue;
            }
            let root = find(&mut parent, indices[0]);
            for &other in &indices[1..] {
                let r2 = find(&mut parent, other);
                if r2 != root {
                    parent[r2] = root;
                }
            }
        }

        let mut components: HashMap<usize, Vec<StackCandidate>> = HashMap::new();
        for (idx, c) in all_candidates.iter().cloned().enumerate() {
            let root = find(&mut parent, idx);
            components.entry(root).or_default().push(c);
        }

        let mut total_stacked = 0usize;
        for comp in components.values() {
            total_stacked += apply_reconciliation_for_bucket(conn, comp)?;
        }

        // Ensure standalone photos without EXIF taken_at_ms still get baseline stack_role/stack_rank.
        conn.execute(
            "UPDATE assets
             SET stack_role = CASE
                    WHEN kind = 'raw_jpg_stack' THEN 'jpeg'
                    WHEN EXISTS (
                        SELECT 1 FROM asset_files af
                        WHERE af.id = assets.primary_file_id
                          AND lower(af.extension) IN ('cr3','cr2','nef','nrw','arw','srf','dng','raf','orf','rw2','pef','srw','iiq','3fr')
                    ) THEN 'raw'
                    ELSE 'jpeg'
                 END,
                 stack_rank = CASE
                    WHEN kind = 'raw_jpg_stack' THEN 200
                    WHEN EXISTS (
                        SELECT 1 FROM asset_files af
                        WHERE af.id = assets.primary_file_id
                          AND lower(af.extension) IN ('cr3','cr2','nef','nrw','arw','srf','dng','raf','orf','rw2','pef','srw','iiq','3fr')
                    ) THEN 100
                    ELSE 200
                 END
             WHERE deleted_at_ms IS NULL
               AND is_hidden = 0
               AND stack_manual = 0
               AND stack_role IS NULL
               AND kind IN ('photo', 'raw_jpg_stack')",
            [],
        )?;

        let stack_groups: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT stack_group_id)
             FROM assets
             WHERE stack_group_id IS NOT NULL
               AND deleted_at_ms IS NULL
               AND is_hidden = 0",
            [],
            |row| row.get(0),
        )?;

        Ok(StackReconcileReport {
            scanned_photos: all_candidates.len(),
            stacked_assets: total_stacked,
            stack_groups: usize::try_from(stack_groups).unwrap_or(0),
        })
    })
}

/// Incrementally reconciles stacks for a single newly ingested or updated asset in O(1) via index.
pub fn reconcile_stacks_for_asset(conn: &Connection, asset_id: &str) -> DbResult<()> {
    let target_sql = format!("{CANDIDATE_SELECT_PREFIX} AND a.id = ?1 LIMIT 1");
    let Some(target) = conn
        .query_row(&target_sql, params![asset_id], load_candidates_sql_row)
        .optional()?
    else {
        return Ok(());
    };
    if target.stack_manual {
        return Ok(());
    }

    // Collect candidates in the same UTC second AND (if timezone-aware) same wall-clock second.
    let utc_sec = target.taken_at_ms.div_euclid(1000);
    let wall_sec = target.wall_clock_ms().div_euclid(1000);

    let mut ranges = vec![(utc_sec * 1000, utc_sec * 1000 + 999)];
    if wall_sec != utc_sec {
        ranges.push((wall_sec * 1000, wall_sec * 1000 + 999));
    }
    // Also check if target is timezone-naive while a peer has a timezone offset in [-12h, +14h]:
    // We check exact second match on `(taken_at_ms % 60_000)` only for the rare naive-vs-tz case
    // when no direct UTC peer matched.
    let mut bucket_map: HashMap<String, StackCandidate> = HashMap::new();
    let range_sql = format!("{CANDIDATE_SELECT_PREFIX} AND p.taken_at_ms BETWEEN ?1 AND ?2");
    let mut stmt = conn.prepare(&range_sql)?;
    for (lo, hi) in ranges {
        let rows = stmt.query_map(params![lo, hi], load_candidates_sql_row)?;
        for row in rows {
            let c = row?;
            bucket_map.insert(c.asset_id.clone(), c);
        }
    }

    // If target had no timezone offset and no peer found yet, check timezone-shifted UTC window (±14h).
    if target.tz_offset_m.is_none() && bucket_map.len() <= 1 {
        let tz_sql = format!(
            "{CANDIDATE_SELECT_PREFIX}
             AND a.capture_tz_offset_m IS NOT NULL
             AND p.taken_at_ms BETWEEN ?1 AND ?2"
        );
        let mut tz_stmt = conn.prepare(&tz_sql)?;
        let rows = tz_stmt.query_map(
            params![
                target.taken_at_ms.saturating_sub(14 * 3_600_000),
                target.taken_at_ms.saturating_add(14 * 3_600_000)
            ],
            load_candidates_sql_row,
        )?;
        for row in rows {
            let c = row?;
            if timestamps_match(&target, &c).is_some() {
                bucket_map.insert(c.asset_id.clone(), c);
            }
        }
    }

    let bucket: Vec<StackCandidate> = bucket_map.into_values().collect();
    if bucket.is_empty() {
        return Ok(());
    }
    with_savepoint(conn, "reconcile_asset_stack", || {
        let _ = apply_reconciliation_for_bucket(conn, &bucket)?;
        Ok(())
    })
}

/// Hydrates `stack_count`, `stack_has_edited`, and `stack_has_raw` on a page of `AssetSummary`
/// rows using a single RBAC-filtered aggregate query.
pub fn hydrate_stack_summaries(
    conn: &Connection,
    scope: &RbacScope,
    items: &mut [AssetSummary],
) -> DbResult<()> {
    let group_ids: Vec<String> = items
        .iter()
        .filter_map(|it| it.stack_group_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if group_ids.is_empty() {
        return Ok(());
    }

    let mut params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
    let mut placeholders = Vec::with_capacity(group_ids.len());
    for gid in &group_ids {
        let idx = params.len() + 1;
        params.push(Value::Text(gid.clone()));
        placeholders.push(format!("?{idx}"));
    }

    let sql = format!(
        "SELECT
            a.stack_group_id,
            COUNT(*),
            MAX(CASE WHEN a.stack_role = 'edited' THEN 1 ELSE 0 END),
            MAX(CASE WHEN a.stack_role = 'raw' OR a.kind = 'raw_jpg_stack' THEN 1 ELSE 0 END)
         FROM assets a
         WHERE a.stack_group_id IN ({})
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND {rbac_clause}
         GROUP BY a.stack_group_id",
        placeholders.join(", ")
    );

    let mut stats: HashMap<String, (u32, bool, bool)> = HashMap::new();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, i64>(3)? != 0,
        ))
    })?;
    for row in rows {
        let (gid, cnt, has_edited, has_raw) = row?;
        stats.insert(
            gid,
            (u32::try_from(cnt).unwrap_or(1).max(1), has_edited, has_raw),
        );
    }

    for item in items.iter_mut() {
        if let Some(gid) = item.stack_group_id.as_deref() {
            if let Some(&(cnt, has_edited, has_raw)) = stats.get(gid) {
                item.stack_count = cnt;
                item.stack_has_edited = has_edited;
                item.stack_has_raw = has_raw;
                if cnt <= 1 {
                    // Only 1 RBAC-visible asset in this group: don't expose hidden stack membership.
                    item.stack_group_id = None;
                }
            }
        }
    }

    Ok(())
}

/// Loads all RBAC-accessible versions in the same stack group, ordered highest-priority cover first.
///
/// Returns an empty list if the asset is not stacked or only 1 version is visible to `scope`.
pub fn load_stack_members(
    conn: &Connection,
    scope: &RbacScope,
    stack_group_id: Option<&str>,
) -> DbResult<Vec<StackMemberView>> {
    let Some(gid) = stack_group_id.filter(|s| !s.trim().is_empty()) else {
        return Ok(Vec::new());
    };

    let mut params: Vec<Value> = vec![Value::Text(gid.to_string())];
    let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);

    let sql = format!(
        "SELECT
            a.id,
            a.folder_id,
            f.name,
            af.rel_path,
            af.extension,
            a.kind,
            COALESCE(a.stack_role, 'other'),
            a.width,
            a.height,
            af.file_size,
            a.thumb_ready,
            a.capture_at_ms,
            p.software
         FROM assets a
         JOIN folders f ON f.id = a.folder_id
         JOIN asset_files af ON af.id = a.primary_file_id
         LEFT JOIN deterministic_photo_meta p ON p.asset_id = a.id
         WHERE a.stack_group_id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0
           AND {rbac_clause}
         ORDER BY a.stack_rank DESC, COALESCE(p.exif_modified_at_ms, af.mtime_ms) DESC, a.id DESC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut members: Vec<StackMemberView> = stmt
        .query_map(params_from_iter(params), |row| {
            let rel_path: String = row.get(3)?;
            let filename = rel_path.rsplit('/').next().unwrap_or(&rel_path).to_string();
            let extension: String = row.get(4)?;
            let kind_str: String = row.get(5)?;
            let kind = kind_str.parse::<AssetKind>().unwrap_or(AssetKind::Photo);
            let width: Option<i64> = row.get(7)?;
            let height: Option<i64> = row.get(8)?;
            let file_size: i64 = row.get(9)?;
            let is_raw = kind == AssetKind::RawJpgStack || is_raw_ext(&extension);

            Ok(StackMemberView {
                asset_id: row.get(0)?,
                folder_id: row.get(1)?,
                folder_name: row.get(2)?,
                rel_path,
                filename,
                extension,
                kind,
                stack_role: row.get(6)?,
                is_cover: false,
                is_raw,
                width: width.and_then(|v| u32::try_from(v).ok()),
                height: height.and_then(|v| u32::try_from(v).ok()),
                file_size: u64::try_from(file_size).unwrap_or(0),
                thumb_ready: row.get::<_, i64>(10)? != 0,
                capture_at_ms: row.get(11)?,
                software: row.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if members.len() <= 1 {
        return Ok(Vec::new());
    }
    if let Some(first) = members.first_mut() {
        first.is_cover = true;
    }
    Ok(members)
}

/// Manually groups 2+ RBAC-accessible assets into a stack (`stack_manual = 1`).
pub fn create_manual_stack(
    conn: &Connection,
    scope: &RbacScope,
    asset_ids: &[String],
    cover_asset_id: Option<&str>,
) -> DbResult<String> {
    let unique_ids: Vec<String> = asset_ids
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if unique_ids.len() < 2 {
        return Err(DbError::InvalidArgument(
            "Select at least 2 photos to create a stack.".to_string(),
        ));
    }

    with_savepoint(conn, "create_manual_stack", || {
        // Verify every asset exists, is active, and is within caller's RBAC scope.
        let mut params: Vec<Value> = Vec::new();
        let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
        let mut ph = Vec::with_capacity(unique_ids.len());
        for id in &unique_ids {
            let idx = params.len() + 1;
            params.push(Value::Text(id.clone()));
            ph.push(format!("?{idx}"));
        }

        let check_sql = format!(
            "SELECT a.id, COALESCE(a.stack_role, 'other'), a.stack_rank
             FROM assets a
             WHERE a.id IN ({})
               AND a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
               AND {rbac_clause}",
            ph.join(", ")
        );
        let mut stmt = conn.prepare(&check_sql)?;
        let found: Vec<(String, String, i64)> = stmt
            .query_map(params_from_iter(params), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        if found.len() != unique_ids.len() {
            return Err(DbError::NotFound(
                "One or more selected assets were not found or are inaccessible.".to_string(),
            ));
        }

        let group_id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        for (id, role, existing_rank) in &found {
            let is_cover = cover_asset_id == Some(id.as_str());
            let next_rank = if is_cover {
                1000
            } else if *existing_rank >= 1000 {
                match role.as_str() {
                    "edited" => 300,
                    "jpeg" => 200,
                    "raw" => 100,
                    _ => 150,
                }
            } else {
                *existing_rank
            };
            conn.execute(
                "UPDATE assets
                 SET stack_group_id = ?1,
                     stack_rank = ?2,
                     stack_manual = 1,
                     updated_at_ms = ?3
                 WHERE id = ?4",
                params![group_id, next_rank, now, id],
            )?;
        }

        Ok(group_id)
    })
}

/// Promotes a specific RBAC-accessible asset to be the cover of its stack (`stack_rank = 1000`).
pub fn set_stack_cover(
    conn: &Connection,
    scope: &RbacScope,
    group_id: &str,
    asset_id: &str,
) -> DbResult<()> {
    with_savepoint(conn, "set_stack_cover", || {
        let mut params: Vec<Value> = vec![
            Value::Text(group_id.to_string()),
            Value::Text(asset_id.to_string()),
        ];
        let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
        let check_sql = format!(
            "SELECT 1 FROM assets a
             WHERE a.stack_group_id = ?1
               AND a.id = ?2
               AND a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
               AND {rbac_clause}
             LIMIT 1"
        );
        let exists = conn
            .query_row(&check_sql, params_from_iter(params), |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Err(DbError::NotFound("Stack member not found.".to_string()));
        }

        let now = now_ms();
        // Reset any previous manual cover in the same group back to its natural role rank.
        conn.execute(
            "UPDATE assets
             SET stack_rank = CASE stack_role
                    WHEN 'edited' THEN 300
                    WHEN 'jpeg' THEN 200
                    WHEN 'raw' THEN 100
                    ELSE 150
                 END
             WHERE stack_group_id = ?1 AND stack_rank >= 1000",
            params![group_id],
        )?;
        conn.execute(
            "UPDATE assets
             SET stack_rank = 1000,
                 stack_manual = 1,
                 updated_at_ms = ?1
             WHERE id = ?2 AND stack_group_id = ?3",
            params![now, asset_id, group_id],
        )?;
        Ok(())
    })
}

/// Removes one asset from a stack group (marking `stack_manual = 1` so auto-reconciliation won't
/// re-merge it). Dissolves the group automatically if only 1 member remains.
pub fn remove_from_stack(
    conn: &Connection,
    scope: &RbacScope,
    group_id: &str,
    asset_id: &str,
) -> DbResult<()> {
    with_savepoint(conn, "remove_from_stack", || {
        let mut params: Vec<Value> = vec![
            Value::Text(group_id.to_string()),
            Value::Text(asset_id.to_string()),
        ];
        let rbac_clause = rbac_sql::folder_clause("a", scope, &mut params);
        let check_sql = format!(
            "SELECT 1 FROM assets a
             WHERE a.stack_group_id = ?1
               AND a.id = ?2
               AND a.deleted_at_ms IS NULL
               AND a.is_hidden = 0
               AND {rbac_clause}
             LIMIT 1"
        );
        let exists = conn
            .query_row(&check_sql, params_from_iter(params), |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Err(DbError::NotFound("Stack member not found.".to_string()));
        }

        let now = now_ms();
        conn.execute(
            "UPDATE assets
             SET stack_group_id = NULL,
                 stack_manual = 1,
                 stack_rank = CASE stack_role
                    WHEN 'edited' THEN 300
                    WHEN 'jpeg' THEN 200
                    WHEN 'raw' THEN 100
                    ELSE 150
                 END,
                 updated_at_ms = ?1
             WHERE id = ?2",
            params![now, asset_id],
        )?;

        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM assets
             WHERE stack_group_id = ?1 AND deleted_at_ms IS NULL AND is_hidden = 0",
            params![group_id],
            |row| row.get(0),
        )?;
        if remaining <= 1 {
            conn.execute(
                "UPDATE assets SET stack_group_id = NULL WHERE stack_group_id = ?1",
                params![group_id],
            )?;
        }
        Ok(())
    })
}

/// Normalizes a user-supplied subfolder path prefix (forward slashes, no leading/trailing slashes,
/// rejects path traversal segments).
pub fn normalize_subfolder_prefix(raw: &str) -> DbResult<Option<String>> {
    let cleaned = raw.trim().replace('\\', "/");
    let trimmed = cleaned.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::new();
    for seg in trimmed.split('/') {
        let s = seg.trim();
        if s.is_empty() || s == "." {
            continue;
        }
        if s == ".." {
            return Err(DbError::InvalidArgument(
                "Subfolder path cannot contain '..' segments.".to_string(),
            ));
        }
        parts.push(s);
    }
    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join("/")))
    }
}

/// Lists all nested subfolders containing active media inside a watched folder,
/// with recursive asset counts for arbitrary directory nesting depth.
pub fn list_folder_subfolders(
    conn: &Connection,
    scope: &RbacScope,
    folder_id: &str,
) -> DbResult<Vec<FolderSubfolderView>> {
    if !scope.allows_folder(folder_id) {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT af.rel_path
         FROM assets a
         JOIN asset_files af ON af.id = a.primary_file_id
         WHERE a.folder_id = ?1
           AND a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0",
    )?;
    let rel_paths: Vec<String> = stmt
        .query_map(params![folder_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for rel in rel_paths {
        let normalized = rel.replace('\\', "/");
        let Some((dir_part, _file)) = normalized.rsplit_once('/') else {
            continue;
        };
        let mut accum = String::new();
        for seg in dir_part.split('/').filter(|s| !s.is_empty()) {
            if !accum.is_empty() {
                accum.push('/');
            }
            accum.push_str(seg);
            *counts.entry(accum.clone()).or_insert(0) += 1;
        }
    }

    let mut out: Vec<FolderSubfolderView> = counts
        .into_iter()
        .map(|(path, asset_count)| {
            let (parent_path, name) = match path.rsplit_once('/') {
                Some((parent, leaf)) => (Some(parent.to_string()), leaf.to_string()),
                None => (None, path.clone()),
            };
            let depth = path.matches('/').count() as u32;
            FolderSubfolderView {
                path,
                name,
                parent_path,
                depth,
                asset_count,
            }
        })
        .collect();

    // Sort case-insensitively by path segments so trees display in natural hierarchical order.
    out.sort_by(|a, b| {
        a.path
            .to_ascii_lowercase()
            .cmp(&b.path.to_ascii_lowercase())
    });
    Ok(out)
}

/// Active photo candidate awaiting one-time EXIF/dHash backfill.
#[derive(Debug, Clone)]
pub struct BackfillCandidateRow {
    pub asset_id: String,
    pub root_path: String,
    pub rel_path: String,
    pub thumb_rel_path: Option<String>,
    pub needs_dhash: bool,
}

/// Extracted EXIF & perceptual hash update to persist during backfill.
#[derive(Debug, Clone)]
pub struct BackfillUpdateRow {
    pub asset_id: String,
    pub taken_at_ms: Option<i64>,
    pub taken_at_has_subsec: bool,
    pub tz_offset_m: Option<i32>,
    pub software: Option<String>,
    pub exif_modified_at_ms: Option<i64>,
    pub dhash64: Option<i64>,
}

/// Lists active photo assets needing EXIF sub-second/software/dHash backfill.
pub fn list_backfill_candidates(
    conn: &Connection,
    force: bool,
) -> DbResult<Vec<BackfillCandidateRow>> {
    let sql = if force {
        "SELECT a.id, f.root_path, af.rel_path, a.thumb_rel_path, a.dhash64
         FROM assets a
         JOIN folders f ON f.id = a.folder_id
         JOIN asset_files af ON af.id = a.primary_file_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0
           AND a.kind IN ('photo', 'raw_jpg_stack')"
    } else {
        "SELECT a.id, f.root_path, af.rel_path, a.thumb_rel_path, a.dhash64
         FROM assets a
         JOIN folders f ON f.id = a.folder_id
         JOIN asset_files af ON af.id = a.primary_file_id
         WHERE a.deleted_at_ms IS NULL
           AND a.is_hidden = 0
           AND af.is_missing = 0
           AND a.kind IN ('photo', 'raw_jpg_stack')
           AND (a.stack_role IS NULL OR a.dhash64 IS NULL)"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let dhash: Option<i64> = row.get(4)?;
        Ok(BackfillCandidateRow {
            asset_id: row.get(0)?,
            root_path: row.get(1)?,
            rel_path: row.get(2)?,
            thumb_rel_path: row.get(3)?,
            needs_dhash: force || dhash.is_none(),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Persists backfilled EXIF/dHash updates and runs full universal stack reconciliation atomically.
pub fn apply_backfill_and_reconcile(
    conn: &Connection,
    updates: &[BackfillUpdateRow],
) -> DbResult<StackReconcileReport> {
    with_savepoint(conn, "apply_backfill_and_reconcile", || {
        for u in updates {
            if let Some(dhash) = u.dhash64 {
                conn.execute(
                    "UPDATE assets SET dhash64 = ?1 WHERE id = ?2",
                    params![dhash, u.asset_id],
                )?;
            }
            if u.taken_at_ms.is_some()
                || u.software.is_some()
                || u.exif_modified_at_ms.is_some()
                || u.taken_at_has_subsec
            {
                conn.execute(
                    "UPDATE deterministic_photo_meta
                     SET taken_at_ms = COALESCE(?1, taken_at_ms),
                         taken_at_has_subsec = CASE WHEN ?2 = 1 THEN 1 ELSE taken_at_has_subsec END,
                         software = COALESCE(?3, software),
                         exif_modified_at_ms = COALESCE(?4, exif_modified_at_ms)
                     WHERE asset_id = ?5",
                    params![
                        u.taken_at_ms,
                        i64::from(u.taken_at_has_subsec),
                        u.software,
                        u.exif_modified_at_ms,
                        u.asset_id
                    ],
                )?;
                if let Some(ts) = u.taken_at_ms {
                    conn.execute(
                        "UPDATE assets
                         SET capture_at_ms = COALESCE(
                                 (SELECT o.capture_at_ms FROM user_overrides o WHERE o.asset_id = assets.id),
                                 ?1
                             ),
                             capture_tz_offset_m = COALESCE(?2, capture_tz_offset_m)
                         WHERE id = ?3",
                        params![ts, u.tz_offset_m.map(i64::from), u.asset_id],
                    )?;
                }
            }
        }
        reconcile_all_stacks(conn)
    })
}
