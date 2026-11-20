-- 0003_phase2_ai.sql — Phase 2 Vision/Face Vector Maps, Model Registry & Zero-Downtime Tier State

-- Maps flat vectors.f16 slot (vector_id) <-> logical asset + optional video timestamp, per vision tier.
-- Supports dual-namespace zero-downtime vision tier upgrades!
CREATE TABLE IF NOT EXISTS vision_embeddings (
    model_tier      TEXT NOT NULL CHECK (model_tier IN ('tier1', 'tier2', 'tier3')),
    vector_id       INTEGER NOT NULL,
    asset_id        TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    timestamp_ms    INTEGER NOT NULL DEFAULT 0,   -- 0 for photos; keyframe timestamp for videos
    created_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (model_tier, vector_id),
    UNIQUE (model_tier, asset_id, timestamp_ms)
);

CREATE INDEX IF NOT EXISTS idx_vision_embeddings_asset ON vision_embeddings(model_tier, asset_id);

-- Maps faces.f16 slot (vector_id) <-> faces.id
CREATE TABLE IF NOT EXISTS face_embeddings (
    vector_id       INTEGER PRIMARY KEY NOT NULL,
    face_id         TEXT NOT NULL UNIQUE REFERENCES faces(id) ON DELETE CASCADE,
    created_at_ms   INTEGER NOT NULL
);

-- Tracks downloaded Apache-2.0/MIT ONNX models & verified SHA-256 checksums
CREATE TABLE IF NOT EXISTS ai_models (
    model_id        TEXT PRIMARY KEY NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('not_installed', 'downloading', 'installed', 'error')),
    local_dir       TEXT,
    bytes_total     INTEGER NOT NULL DEFAULT 0,
    bytes_downloaded INTEGER NOT NULL DEFAULT 0,
    sha256_manifest TEXT,                          -- JSON map { filename: sha256_hex }
    error_message   TEXT,
    updated_at_ms   INTEGER NOT NULL
);

-- Singleton row tracking active vs staging vision tier for zero-downtime cutover + pause flag
CREATE TABLE IF NOT EXISTS ai_index_state (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    active_vision_tier  TEXT NOT NULL DEFAULT 'tier2' CHECK (active_vision_tier IN ('tier1', 'tier2', 'tier3')),
    staging_vision_tier TEXT CHECK (staging_vision_tier IS NULL OR staging_vision_tier IN ('tier1', 'tier2', 'tier3')),
    paused              INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    idle_unload_secs    INTEGER NOT NULL DEFAULT 300,
    updated_at_ms       INTEGER NOT NULL
);

INSERT OR IGNORE INTO ai_index_state (id, active_vision_tier, staging_vision_tier, paused, idle_unload_secs, updated_at_ms)
VALUES (1, 'tier2', NULL, 0, 300, 0);

-- Store people centroids compactly as raw f32 LE blob (512 * 4 = 2048 bytes) for instant Stage 1 face matching
CREATE TABLE IF NOT EXISTS person_centroids (
    person_id       TEXT PRIMARY KEY NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    face_count      INTEGER NOT NULL DEFAULT 0,
    centroid_blob   BLOB NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);
