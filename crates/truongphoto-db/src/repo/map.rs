use crate::error::DbResult;
use crate::rbac_sql;
use crate::repo::assets::{row_to_asset_summary, ASSET_SUMMARY_COLUMNS};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use truongphoto_core::models::{MapCluster, MapClustersResponse, MapPointsResponse};
use truongphoto_core::rbac::RbacScope;

const DEFAULT_MAP_POINTS_LIMIT: usize = 100;
const MAX_MAP_POINTS_LIMIT: usize = 500;

struct EffectiveGeoRow {
    folder_id: String,
    lat: f64,
    lon: f64,
    capture_at_ms: i64,
    geo_city: Option<String>,
    geo_country: Option<String>,
}

/// Synchronizes `asset_geo_index` with `v_assets_effective` for a single asset.
///
/// Inserts or updates the row when the asset is active (`deleted_at_ms IS NULL AND is_hidden = 0`)
/// and has effective GPS coordinates; otherwise deletes any stale entry from `asset_geo_index`.
pub fn sync_asset_geo(conn: &Connection, asset_id: &str) -> DbResult<()> {
    let row: Option<EffectiveGeoRow> = conn
        .query_row(
            "SELECT
                v.folder_id,
                v.effective_gps_lat,
                v.effective_gps_lon,
                v.effective_capture_at_ms,
                v.effective_geo_city,
                v.effective_geo_country
             FROM v_assets_effective v
             WHERE v.id = ?1
               AND v.deleted_at_ms IS NULL
               AND v.is_hidden = 0
               AND v.effective_gps_lat IS NOT NULL
               AND v.effective_gps_lon IS NOT NULL
             LIMIT 1",
            params![asset_id],
            |r| {
                Ok(EffectiveGeoRow {
                    folder_id: r.get(0)?,
                    lat: r.get(1)?,
                    lon: r.get(2)?,
                    capture_at_ms: r.get(3)?,
                    geo_city: r.get(4)?,
                    geo_country: r.get(5)?,
                })
            },
        )
        .optional()?;

    if let Some(geo) = row {
        conn.execute(
            "INSERT OR REPLACE INTO asset_geo_index (
                asset_id, folder_id, lat, lon, capture_at_ms, geo_city, geo_country
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                asset_id,
                geo.folder_id,
                geo.lat,
                geo.lon,
                geo.capture_at_ms,
                geo.geo_city,
                geo.geo_country
            ],
        )?;
    } else {
        conn.execute(
            "DELETE FROM asset_geo_index WHERE asset_id = ?1",
            params![asset_id],
        )?;
    }

    Ok(())
}

/// Synchronizes `asset_geo_index` for each unique asset ID in `asset_ids`.
pub(crate) fn sync_assets_geo(conn: &Connection, asset_ids: &[String]) -> DbResult<()> {
    let mut seen = HashSet::with_capacity(asset_ids.len());
    for id in asset_ids {
        if seen.insert(id.as_str()) {
            sync_asset_geo(conn, id)?;
        }
    }
    Ok(())
}

/// Counts active, geotagged assets visible under `scope`.
pub fn count_geotagged_assets(conn: &Connection, scope: &RbacScope) -> DbResult<u64> {
    let mut params: Vec<Value> = Vec::new();
    let rbac_clause = rbac_sql::folder_clause("g", scope, &mut params);
    let sql = format!("SELECT COUNT(*) FROM asset_geo_index g WHERE {rbac_clause}");
    let count: i64 = conn.query_row(&sql, params_from_iter(params), |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn append_bbox_clause(
    alias: &str,
    scope: &RbacScope,
    min_lat: f64,
    max_lat: f64,
    min_lng: f64,
    max_lng: f64,
    params: &mut Vec<Value>,
) -> Option<String> {
    if !min_lat.is_finite()
        || !max_lat.is_finite()
        || !min_lng.is_finite()
        || !max_lng.is_finite()
        || min_lat > max_lat
    {
        return None;
    }

    let rbac_clause = rbac_sql::folder_clause(alias, scope, params);
    let min_lat_idx = params.len() + 1;
    let max_lat_idx = params.len() + 2;
    params.push(Value::Real(min_lat));
    params.push(Value::Real(max_lat));

    let min_lng_idx = params.len() + 1;
    let max_lng_idx = params.len() + 2;
    params.push(Value::Real(min_lng));
    params.push(Value::Real(max_lng));

    let lon_clause = if min_lng <= max_lng {
        format!("{alias}.lon BETWEEN ?{min_lng_idx} AND ?{max_lng_idx}")
    } else {
        // Anti-meridian wraparound (e.g. min_lng = 170.0, max_lng = -170.0)
        format!("({alias}.lon >= ?{min_lng_idx} OR {alias}.lon <= ?{max_lng_idx})")
    };

    Some(format!(
        "{rbac_clause} AND {alias}.lat BETWEEN ?{min_lat_idx} AND ?{max_lat_idx} AND {lon_clause}"
    ))
}

struct ClusterAccumulator {
    id: String,
    sum_lat: f64,
    sum_lon: f64,
    count: u64,
    cover_asset_id: String,
    newest_capture_at_ms: i64,
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
    city: Option<String>,
    country: Option<String>,
}

#[inline]
fn extract_clean_opt_str(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<String>> {
    match row.get_ref(idx)?.as_str().ok() {
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

/// Computes RBAC-scoped spatial clusters within the requested bounding box for `zoom` (`0..=20`).
///
/// Supports anti-meridian wraparound when `min_lng > max_lng`.
pub fn list_map_clusters(
    conn: &Connection,
    scope: &RbacScope,
    min_lat: f64,
    max_lat: f64,
    min_lng: f64,
    max_lng: f64,
    zoom: u8,
) -> DbResult<MapClustersResponse> {
    let mut params: Vec<Value> = Vec::new();
    let Some(where_clause) =
        append_bbox_clause("g", scope, min_lat, max_lat, min_lng, max_lng, &mut params)
    else {
        return Ok(MapClustersResponse {
            clusters: Vec::new(),
            total_in_view: 0,
        });
    };

    let sql = format!(
        "SELECT g.asset_id, g.lat, g.lon, g.capture_at_ms, g.geo_city, g.geo_country
         FROM asset_geo_index g
         WHERE {where_clause}"
    );

    let clamped_zoom = zoom.min(20);
    let cell = 360.0 / (2_f64.powi(i32::from(clamped_zoom)) * 6.0);

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(params))?;
    let mut buckets: HashMap<(i64, i64), ClusterAccumulator> = HashMap::new();
    let mut total_in_view: u64 = 0;

    while let Some(row) = rows.next()? {
        let lat: f64 = row.get(1)?;
        let lon: f64 = row.get(2)?;
        let capture_at_ms: i64 = row.get(3)?;

        total_in_view += 1;

        let gy = ((lat + 90.0) / cell).floor() as i64;
        let gx = ((lon + 180.0) / cell).floor() as i64;

        if let Some(acc) = buckets.get_mut(&(gy, gx)) {
            acc.sum_lat += lat;
            acc.sum_lon += lon;
            acc.count += 1;
            acc.min_lat = acc.min_lat.min(lat);
            acc.max_lat = acc.max_lat.max(lat);
            acc.min_lon = acc.min_lon.min(lon);
            acc.max_lon = acc.max_lon.max(lon);

            if capture_at_ms > acc.newest_capture_at_ms {
                let aid = row.get_ref(0)?.as_str().unwrap_or("");
                acc.newest_capture_at_ms = capture_at_ms;
                acc.cover_asset_id.clear();
                acc.cover_asset_id.push_str(aid);
            } else if capture_at_ms == acc.newest_capture_at_ms {
                let aid = row.get_ref(0)?.as_str().unwrap_or("");
                if aid > acc.cover_asset_id.as_str() {
                    acc.cover_asset_id.clear();
                    acc.cover_asset_id.push_str(aid);
                }
            }

            if acc.city.is_none() {
                acc.city = extract_clean_opt_str(row, 4)?;
            }
            if acc.country.is_none() {
                acc.country = extract_clean_opt_str(row, 5)?;
            }
        } else {
            let asset_id: String = row.get(0)?;
            let city = extract_clean_opt_str(row, 4)?;
            let country = extract_clean_opt_str(row, 5)?;
            buckets.insert(
                (gy, gx),
                ClusterAccumulator {
                    id: format!("z{clamped_zoom}_{gy}_{gx}"),
                    sum_lat: lat,
                    sum_lon: lon,
                    count: 1,
                    cover_asset_id: asset_id,
                    newest_capture_at_ms: capture_at_ms,
                    min_lat: lat,
                    max_lat: lat,
                    min_lon: lon,
                    max_lon: lon,
                    city,
                    country,
                },
            );
        }
    }

    let mut accs: Vec<ClusterAccumulator> = buckets.into_values().collect();
    accs.sort_by(|a, b| {
        b.newest_capture_at_ms
            .cmp(&a.newest_capture_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });

    let clusters = accs
        .into_iter()
        .map(|acc| {
            let count_f = acc.count as f64;
            MapCluster {
                id: acc.id,
                lat: acc.sum_lat / count_f,
                lon: acc.sum_lon / count_f,
                count: acc.count,
                cover_asset_id: acc.cover_asset_id,
                min_lat: acc.min_lat,
                max_lat: acc.max_lat,
                min_lon: acc.min_lon,
                max_lon: acc.max_lon,
                label: acc.city.or(acc.country),
            }
        })
        .collect();

    Ok(MapClustersResponse {
        clusters,
        total_in_view,
    })
}

/// Lists RBAC-scoped geotagged `AssetSummary` points inside the requested bounding box.
///
/// Ordered newest-first by `capture_at_ms DESC`. Supports anti-meridian wraparound when `min_lng > max_lng`.
pub fn list_map_points(
    conn: &Connection,
    scope: &RbacScope,
    min_lat: f64,
    max_lat: f64,
    min_lng: f64,
    max_lng: f64,
    limit: usize,
) -> DbResult<MapPointsResponse> {
    let effective_limit = if limit == 0 {
        DEFAULT_MAP_POINTS_LIMIT
    } else {
        limit.min(MAX_MAP_POINTS_LIMIT)
    };

    let mut params: Vec<Value> = Vec::new();
    let Some(where_clause) =
        append_bbox_clause("g", scope, min_lat, max_lat, min_lng, max_lng, &mut params)
    else {
        return Ok(MapPointsResponse {
            items: Vec::new(),
            total_count: 0,
        });
    };

    let count_sql = format!("SELECT COUNT(*) FROM asset_geo_index g WHERE {where_clause}");
    let total_count_i64: i64 =
        conn.query_row(&count_sql, params_from_iter(params.clone()), |row| {
            row.get(0)
        })?;

    let limit_idx = params.len() + 1;
    params.push(Value::Integer(effective_limit as i64));

    let list_sql = format!(
        "SELECT {ASSET_SUMMARY_COLUMNS}
         FROM asset_geo_index g
         JOIN v_assets_effective v ON v.id = g.asset_id
         WHERE {where_clause}
         ORDER BY g.capture_at_ms DESC, g.asset_id DESC
         LIMIT ?{limit_idx}"
    );

    let mut stmt = conn.prepare(&list_sql)?;
    let items = stmt
        .query_map(params_from_iter(params), row_to_asset_summary)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(MapPointsResponse {
        items,
        total_count: u64::try_from(total_count_i64).unwrap_or(0),
    })
}
