-- 0004_phase3_chat_map.sql — Phase 3 Multi-Turn Chat Persistence, Chat Tier State & Spatial Map Index

-- ---------------------------------------------------------------------------
-- 1. Conversational Search Sessions & Messages (Strictly Scoped to user_id)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS chat_sessions (
    id                  TEXT PRIMARY KEY NOT NULL,
    user_id             TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title               TEXT NOT NULL,
    active_filters_json TEXT NOT NULL DEFAULT '{}', -- Carried-forward RouterFilters JSON across turns
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chat_sessions_user ON chat_sessions(user_id, updated_at_ms DESC);

CREATE TABLE IF NOT EXISTS chat_messages (
    id                  TEXT PRIMARY KEY NOT NULL,
    session_id          TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    role                TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    content             TEXT NOT NULL,
    plan_json           TEXT,                       -- Structured RouterPlan JSON (transparent filter chips)
    result_asset_ids    TEXT,                       -- JSON array of matched {asset_id, score, timestamp_sec}
    reranked            INTEGER NOT NULL DEFAULT 0 CHECK (reranked IN (0, 1)),
    planner_mode        TEXT,
    elapsed_ms          INTEGER,
    created_at_ms       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id, created_at_ms ASC, id ASC);

-- ---------------------------------------------------------------------------
-- 2. Extend ai_index_state with Chat Tier (Tier 0..3) & VLM Rerank Toggle
-- ---------------------------------------------------------------------------
ALTER TABLE ai_index_state ADD COLUMN active_chat_tier TEXT NOT NULL DEFAULT 'tier0'
    CHECK (active_chat_tier IN ('tier0', 'tier1', 'tier2', 'tier3'));
ALTER TABLE ai_index_state ADD COLUMN vlm_rerank_enabled INTEGER NOT NULL DEFAULT 0
    CHECK (vlm_rerank_enabled IN (0, 1));

-- ---------------------------------------------------------------------------
-- 3. Covering Spatial Bounding-Box Index for 500k+ Geotagged Map Clustering
-- ---------------------------------------------------------------------------
-- Materializes effective (COALESCE override -> photo -> video) lat/lon for active non-hidden assets
-- so viewport bounding-box SQL clustering hits a single covering B-tree index in milliseconds.
CREATE TABLE IF NOT EXISTS asset_geo_index (
    asset_id        TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    folder_id       TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    lat             REAL NOT NULL,
    lon             REAL NOT NULL,
    capture_at_ms   INTEGER NOT NULL,
    geo_city        TEXT,
    geo_country     TEXT
);

CREATE INDEX IF NOT EXISTS idx_asset_geo_bbox
    ON asset_geo_index(lat, lon, folder_id, capture_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_asset_geo_folder
    ON asset_geo_index(folder_id, lat, lon);

-- Backfill existing Phase 1/2 geotagged active assets cleanly on migration
INSERT OR REPLACE INTO asset_geo_index (asset_id, folder_id, lat, lon, capture_at_ms, geo_city, geo_country)
SELECT
    v.id,
    v.folder_id,
    v.effective_gps_lat,
    v.effective_gps_lon,
    v.effective_capture_at_ms,
    v.effective_geo_city,
    v.effective_geo_country
FROM v_assets_effective v
WHERE v.deleted_at_ms IS NULL
  AND v.is_hidden = 0
  AND v.effective_gps_lat IS NOT NULL
  AND v.effective_gps_lon IS NOT NULL;
