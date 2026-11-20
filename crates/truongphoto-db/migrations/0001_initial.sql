-- 0001_initial.sql — TruongPhoto v1 Core Schema (SQLite WAL + FTS5)
-- Executed inside a single transaction by truongphoto-db::migrations.

-- ---------------------------------------------------------------------------
-- 1. Users, Roles & Sessions
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS users (
    id              TEXT PRIMARY KEY NOT NULL,
    username        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name    TEXT NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('admin', 'contributor', 'viewer')),
    pin_hash        TEXT NOT NULL,                 -- Argon2id PHC string of 4-digit Profile PIN
    avatar_color    TEXT NOT NULL DEFAULT '#4a4a4a',
    disabled_at_ms  INTEGER,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY NOT NULL,
    token_hash      TEXT NOT NULL UNIQUE,          -- SHA-256 hex of 32-byte random cookie token
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    client_kind     TEXT NOT NULL CHECK (client_kind IN ('web', 'tauri_desktop', 'tailscale')),
    user_agent      TEXT,
    ip_addr         TEXT,
    created_at_ms   INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL,
    expires_at_ms   INTEGER NOT NULL,
    revoked_at_ms   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at_ms) WHERE revoked_at_ms IS NULL;

-- ---------------------------------------------------------------------------
-- 2. Watched Folders, Volume Identity & Folder ACLs
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS folders (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT NOT NULL,
    root_path           TEXT NOT NULL UNIQUE,
    volume_uuid         TEXT,                      -- macOS DiskArbitration UUID or Windows \\?\Volume{GUID}\
    volume_label        TEXT,
    is_managed_uploads  INTEGER NOT NULL DEFAULT 0 CHECK (is_managed_uploads IN (0, 1)),
    allow_disk_delete   INTEGER NOT NULL DEFAULT 0 CHECK (allow_disk_delete IN (0, 1)),
    is_offline          INTEGER NOT NULL DEFAULT 0 CHECK (is_offline IN (0, 1)),
    last_scanned_at_ms  INTEGER,
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

-- Admins implicitly have access to all folders; Contributors/Viewers access ONLY rows here.
CREATE TABLE IF NOT EXISTS user_folder_acls (
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    folder_id       TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    granted_at_ms   INTEGER NOT NULL,
    PRIMARY KEY (user_id, folder_id)
);

CREATE INDEX IF NOT EXISTS idx_user_folder_acls_folder ON user_folder_acls(folder_id);

-- ---------------------------------------------------------------------------
-- 3. Logical Assets & Physical Asset Files (Stacks, Live Photos, Motion Photos)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS assets (
    id                  TEXT PRIMARY KEY NOT NULL,
    folder_id           TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL CHECK (kind IN ('photo', 'video', 'live_photo', 'motion_photo', 'raw_jpg_stack')),
    primary_file_id     TEXT,                      -- References asset_files.id (resolved in app logic)
    stack_key           TEXT,                      -- ContentIdentifier or normalized (folder_id, stem) reconciliation key
    capture_at_ms       INTEGER NOT NULL,          -- Best-known capture timestamp (EXIF -> mtime fallback)
    capture_tz_offset_m INTEGER,                   -- Optional EXIF timezone offset in minutes
    width               INTEGER,
    height              INTEGER,
    duration_ms         INTEGER,                   -- Non-null for videos & live/motion clips
    aspect_ratio        REAL,                      -- width / height for justified timeline layout
    is_favorite         INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    is_hidden           INTEGER NOT NULL DEFAULT 0 CHECK (is_hidden IN (0, 1)),
    deleted_at_ms       INTEGER,                   -- Non-null = in Trash
    deleted_by          TEXT REFERENCES users(id) ON DELETE SET NULL,
    thumb_rel_path      TEXT,                      -- Relative path inside thumbnails_dir (400px WebP)
    thumb_ready         INTEGER NOT NULL DEFAULT 0 CHECK (thumb_ready IN (0, 1)),
    vision_vector_id    INTEGER UNIQUE,            -- Row index into vectors.f16 vision namespace (photos/stills)
    ai_stage            INTEGER NOT NULL DEFAULT 0 CHECK (ai_stage IN (0, 1, 2)),
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_assets_folder_capture
    ON assets(folder_id, capture_at_ms DESC, id DESC)
    WHERE deleted_at_ms IS NULL AND is_hidden = 0;

CREATE INDEX IF NOT EXISTS idx_assets_capture_active
    ON assets(capture_at_ms DESC, id DESC)
    WHERE deleted_at_ms IS NULL AND is_hidden = 0;

CREATE INDEX IF NOT EXISTS idx_assets_favorite
    ON assets(folder_id, capture_at_ms DESC)
    WHERE deleted_at_ms IS NULL AND is_favorite = 1;

CREATE INDEX IF NOT EXISTS idx_assets_trash
    ON assets(deleted_at_ms DESC, folder_id)
    WHERE deleted_at_ms IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_assets_stack_key
    ON assets(folder_id, stack_key)
    WHERE stack_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_assets_ai_stage
    ON assets(ai_stage)
    WHERE deleted_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS asset_files (
    id                      TEXT PRIMARY KEY NOT NULL,
    asset_id                TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    folder_id               TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    rel_path                TEXT NOT NULL,         -- Forward-slash relative path inside folder root_path
    file_role               TEXT NOT NULL CHECK (file_role IN ('primary', 'raw', 'jpeg_companion', 'live_video', 'motion_embedded')),
    mime_type               TEXT NOT NULL,
    extension               TEXT NOT NULL,         -- Lowercase without dot (e.g. 'heic', 'cr3', 'mov', 'jpg')
    file_size               INTEGER NOT NULL,
    mtime_ms                INTEGER NOT NULL,
    partial_blake3          TEXT NOT NULL,         -- Hex BLAKE3(first 64KB || last 64KB || le_u64(file_size))
    content_identifier      TEXT,                  -- Apple MakerNotes 0x0011 / QuickTime content.identifier
    embedded_video_offset   INTEGER,               -- Android Motion Photo byte offset of appended MP4
    embedded_video_length   INTEGER,               -- Android Motion Photo byte length of appended MP4
    is_missing              INTEGER NOT NULL DEFAULT 0 CHECK (is_missing IN (0, 1)),
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL,
    UNIQUE (folder_id, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_asset_files_asset ON asset_files(asset_id);
CREATE INDEX IF NOT EXISTS idx_asset_files_partial_blake3 ON asset_files(partial_blake3, file_size);
CREATE INDEX IF NOT EXISTS idx_asset_files_content_id ON asset_files(folder_id, content_identifier) WHERE content_identifier IS NOT NULL;

-- Video keyframe vectors (max 30/video) linked back to logical asset for k*4 collapse
CREATE TABLE IF NOT EXISTS video_keyframes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id        TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    frame_index     INTEGER NOT NULL,
    timestamp_ms    INTEGER NOT NULL,
    vector_id       INTEGER NOT NULL UNIQUE,
    UNIQUE (asset_id, frame_index)
);

CREATE INDEX IF NOT EXISTS idx_video_keyframes_asset ON video_keyframes(asset_id);

-- ---------------------------------------------------------------------------
-- 4. Deterministic Metadata & Non-Destructive User Overrides + COALESCE View
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS deterministic_photo_meta (
    asset_id            TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    camera_make         TEXT,
    camera_model        TEXT,
    lens_model          TEXT,
    focal_length_mm     REAL,
    focal_length_35mm   INTEGER,
    aperture_f          REAL,
    shutter_speed_sec   REAL,
    iso                 INTEGER,
    orientation         INTEGER,
    color_space         TEXT,
    gps_lat             REAL,
    gps_lon             REAL,
    gps_alt_m           REAL,
    geo_city            TEXT,
    geo_admin1          TEXT,
    geo_country         TEXT,
    geo_country_code    TEXT,
    taken_at_ms         INTEGER,
    raw_exif_json       TEXT
);

CREATE INDEX IF NOT EXISTS idx_photo_meta_camera ON deterministic_photo_meta(camera_model);
CREATE INDEX IF NOT EXISTS idx_photo_meta_geo ON deterministic_photo_meta(geo_country_code, geo_city);
CREATE INDEX IF NOT EXISTS idx_photo_meta_gps ON deterministic_photo_meta(gps_lat, gps_lon) WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL;

CREATE TABLE IF NOT EXISTS deterministic_video_meta (
    asset_id            TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    duration_ms         INTEGER NOT NULL,
    video_codec         TEXT,
    audio_codec         TEXT,
    fps                 REAL,
    bitrate_bps         INTEGER,
    bit_depth           INTEGER,
    color_transfer      TEXT,
    color_primaries     TEXT,
    is_hdr              INTEGER NOT NULL DEFAULT 0 CHECK (is_hdr IN (0, 1)),
    rotation            INTEGER DEFAULT 0,
    gps_lat             REAL,
    gps_lon             REAL,
    geo_city            TEXT,
    geo_admin1          TEXT,
    geo_country         TEXT,
    geo_country_code    TEXT,
    taken_at_ms         INTEGER,
    raw_probe_json      TEXT
);

CREATE INDEX IF NOT EXISTS idx_video_meta_geo ON deterministic_video_meta(geo_country_code, geo_city);
CREATE INDEX IF NOT EXISTS idx_video_meta_gps ON deterministic_video_meta(gps_lat, gps_lon) WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL;

CREATE TABLE IF NOT EXISTS user_overrides (
    asset_id            TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    title               TEXT,
    description         TEXT,
    capture_at_ms       INTEGER,
    gps_lat             REAL,
    gps_lon             REAL,
    geo_city            TEXT,
    geo_admin1          TEXT,
    geo_country         TEXT,
    geo_country_code    TEXT,
    rating              INTEGER CHECK (rating IS NULL OR (rating >= 0 AND rating <= 5)),
    updated_by          TEXT REFERENCES users(id) ON DELETE SET NULL,
    updated_at_ms       INTEGER NOT NULL
);

-- Authoritative read view unifying deterministic EXIF/video metadata with user overrides.
CREATE VIEW IF NOT EXISTS v_assets_effective AS
SELECT
    a.id,
    a.folder_id,
    a.kind,
    a.primary_file_id,
    a.stack_key,
    COALESCE(o.capture_at_ms, p.taken_at_ms, v.taken_at_ms, a.capture_at_ms) AS effective_capture_at_ms,
    a.capture_tz_offset_m,
    a.width,
    a.height,
    COALESCE(v.duration_ms, a.duration_ms) AS effective_duration_ms,
    a.aspect_ratio,
    a.is_favorite,
    a.is_hidden,
    a.deleted_at_ms,
    a.deleted_by,
    a.thumb_rel_path,
    a.thumb_ready,
    a.vision_vector_id,
    a.ai_stage,
    a.created_at_ms,
    a.updated_at_ms,
    o.title AS effective_title,
    o.description AS effective_description,
    o.rating AS effective_rating,
    COALESCE(o.gps_lat, p.gps_lat, v.gps_lat) AS effective_gps_lat,
    COALESCE(o.gps_lon, p.gps_lon, v.gps_lon) AS effective_gps_lon,
    COALESCE(o.geo_city, p.geo_city, v.geo_city) AS effective_geo_city,
    COALESCE(o.geo_admin1, p.geo_admin1, v.geo_admin1) AS effective_geo_admin1,
    COALESCE(o.geo_country, p.geo_country, v.geo_country) AS effective_geo_country,
    COALESCE(o.geo_country_code, p.geo_country_code, v.geo_country_code) AS effective_geo_country_code,
    p.camera_make,
    p.camera_model,
    p.lens_model,
    p.focal_length_mm,
    p.aperture_f,
    p.shutter_speed_sec,
    p.iso,
    v.video_codec,
    v.is_hdr
FROM assets a
LEFT JOIN deterministic_photo_meta p ON p.asset_id = a.id
LEFT JOIN deterministic_video_meta v ON v.asset_id = a.id
LEFT JOIN user_overrides o ON o.asset_id = a.id;

-- ---------------------------------------------------------------------------
-- 5. Media Tags (AI Zero-Shot + User Tags, Active vs Rejected)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS media_tags (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id        TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    tag             TEXT NOT NULL COLLATE NOCASE,
    source          TEXT NOT NULL CHECK (source IN ('ai', 'user')),
    state           TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'rejected')),
    confidence      REAL,
    model_id        TEXT,
    created_by      TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL,
    UNIQUE (asset_id, tag)
);

CREATE INDEX IF NOT EXISTS idx_media_tags_tag_active ON media_tags(tag, asset_id) WHERE state = 'active';
CREATE INDEX IF NOT EXISTS idx_media_tags_asset ON media_tags(asset_id);

-- ---------------------------------------------------------------------------
-- 6. Albums & Internal User Sharing (v1: per-user albums + internal shares)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS albums (
    id              TEXT PRIMARY KEY NOT NULL,
    owner_user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    description     TEXT,
    cover_asset_id  TEXT REFERENCES assets(id) ON DELETE SET NULL,
    sort_order      TEXT NOT NULL DEFAULT 'capture_desc'
                    CHECK (sort_order IN ('capture_desc', 'capture_asc', 'added_desc', 'custom')),
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_albums_owner ON albums(owner_user_id, updated_at_ms DESC);

CREATE TABLE IF NOT EXISTS album_assets (
    album_id        TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    asset_id        TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL DEFAULT 0,
    added_by        TEXT REFERENCES users(id) ON DELETE SET NULL,
    added_at_ms     INTEGER NOT NULL,
    PRIMARY KEY (album_id, asset_id)
);

CREATE INDEX IF NOT EXISTS idx_album_assets_asset ON album_assets(asset_id);

CREATE TABLE IF NOT EXISTS album_shares (
    album_id            TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    shared_with_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    permission          TEXT NOT NULL CHECK (permission IN ('view', 'contribute')),
    shared_by           TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (album_id, shared_with_user_id)
);

CREATE INDEX IF NOT EXISTS idx_album_shares_user ON album_shares(shared_with_user_id);

-- ---------------------------------------------------------------------------
-- 7. People & Faces (Admin-Curated Family-Wide with Strict RBAC Query Filtering)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS people (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT,                      -- NULL = unnamed cluster
    is_hidden           INTEGER NOT NULL DEFAULT 0 CHECK (is_hidden IN (0, 1)),
    is_pinned           INTEGER NOT NULL DEFAULT 0 CHECK (is_pinned IN (0, 1)),
    cover_face_id       TEXT,                      -- Admin-preferred face_id; RBAC queries fall back to an allowed-folder face!
    centroid_vector_id  INTEGER,
    created_at_ms       INTEGER NOT NULL,
    updated_at_ms       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS faces (
    id              TEXT PRIMARY KEY NOT NULL,
    asset_id        TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    person_id       TEXT REFERENCES people(id) ON DELETE SET NULL,
    bbox_x          REAL NOT NULL,                 -- Normalized [0.0, 1.0]
    bbox_y          REAL NOT NULL,
    bbox_w          REAL NOT NULL,
    bbox_h          REAL NOT NULL,
    det_score       REAL NOT NULL,
    vector_id       INTEGER UNIQUE,                -- Row index into vectors.f16 faces namespace
    assigned_by     TEXT NOT NULL DEFAULT 'unassigned' CHECK (assigned_by IN ('auto', 'manual', 'unassigned')),
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_faces_person_asset ON faces(person_id, asset_id) WHERE person_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_faces_asset ON faces(asset_id);

-- ---------------------------------------------------------------------------
-- 8. OCR Storage & FTS5 Full-Text Index (Synchronized via Triggers)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ocr_documents (
    rowid           INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id        TEXT NOT NULL UNIQUE REFERENCES assets(id) ON DELETE CASCADE,
    full_text       TEXT NOT NULL,
    lang_hint       TEXT,
    model_id        TEXT,
    updated_at_ms   INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS ocr_fts USING fts5(
    asset_id UNINDEXED,
    full_text,
    content='ocr_documents',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS trg_ocr_documents_ai AFTER INSERT ON ocr_documents BEGIN
    INSERT INTO ocr_fts(rowid, asset_id, full_text) VALUES (new.rowid, new.asset_id, new.full_text);
END;

CREATE TRIGGER IF NOT EXISTS trg_ocr_documents_ad AFTER DELETE ON ocr_documents BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, asset_id, full_text) VALUES ('delete', old.rowid, old.asset_id, old.full_text);
END;

CREATE TRIGGER IF NOT EXISTS trg_ocr_documents_au AFTER UPDATE ON ocr_documents BEGIN
    INSERT INTO ocr_fts(ocr_fts, rowid, asset_id, full_text) VALUES ('delete', old.rowid, old.asset_id, old.full_text);
    INSERT INTO ocr_fts(rowid, asset_id, full_text) VALUES (new.rowid, new.asset_id, new.full_text);
END;

-- ---------------------------------------------------------------------------
-- 9. Background Jobs, Settings & Audit Log
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS jobs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    kind            TEXT NOT NULL,                 -- e.g. 'folder_scan', 'stage1_ingest', 'stage2_ai', 'backup', 'vacuum'
    payload_json    TEXT NOT NULL DEFAULT '{}',
    status          TEXT NOT NULL DEFAULT 'queued'
                    CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    priority        INTEGER NOT NULL DEFAULT 0,    -- Higher executes first
    attempts        INTEGER NOT NULL DEFAULT 0,
    max_attempts    INTEGER NOT NULL DEFAULT 3,
    error_message   TEXT,
    run_after_ms    INTEGER NOT NULL DEFAULT 0,
    started_at_ms   INTEGER,
    finished_at_ms  INTEGER,
    created_at_ms   INTEGER NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_jobs_runnable
    ON jobs(status, priority DESC, run_after_ms ASC, id ASC)
    WHERE status IN ('queued', 'running');

CREATE TABLE IF NOT EXISTS settings (
    key             TEXT PRIMARY KEY NOT NULL,
    value_json      TEXT NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_user_id   TEXT REFERENCES users(id) ON DELETE SET NULL,
    action          TEXT NOT NULL,
    target_type     TEXT,
    target_id       TEXT,
    detail_json     TEXT NOT NULL DEFAULT '{}',
    ip_addr         TEXT,
    created_at_ms   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_audit_log_created ON audit_log(created_at_ms DESC);
