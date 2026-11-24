-- Migration 0006: Universal EXIF-first cross-folder stacking & nested subfolder browsing

ALTER TABLE deterministic_photo_meta ADD COLUMN software TEXT;
ALTER TABLE deterministic_photo_meta ADD COLUMN exif_modified_at_ms INTEGER;
ALTER TABLE deterministic_photo_meta ADD COLUMN taken_at_has_subsec INTEGER NOT NULL DEFAULT 0;

ALTER TABLE assets ADD COLUMN stack_group_id TEXT;
ALTER TABLE assets ADD COLUMN stack_role TEXT CHECK (stack_role IN ('edited', 'jpeg', 'raw', 'other'));
ALTER TABLE assets ADD COLUMN stack_rank INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assets ADD COLUMN stack_manual INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assets ADD COLUMN dhash64 INTEGER;

-- Fast range index for EXIF timestamp matching (UTC and timezone wall-clock lookup).
CREATE INDEX IF NOT EXISTS idx_photo_meta_taken_at
    ON deterministic_photo_meta(taken_at_ms)
    WHERE taken_at_ms IS NOT NULL;

-- Fast lookup for stack collapsing, cover selection, and Lightbox version loading.
CREATE INDEX IF NOT EXISTS idx_assets_stack_group
    ON assets(stack_group_id, stack_rank DESC, updated_at_ms DESC, id DESC)
    WHERE stack_group_id IS NOT NULL AND deleted_at_ms IS NULL AND is_hidden = 0;

-- Fast prefix lookup for nested subfolder filtering inside any watched folder.
CREATE INDEX IF NOT EXISTS idx_asset_files_folder_relpath
    ON asset_files(folder_id, rel_path)
    WHERE is_missing = 0;

-- Recreate authoritative read view to project stack metadata directly.
DROP VIEW IF EXISTS v_assets_effective;
CREATE VIEW v_assets_effective AS
SELECT
    a.id,
    a.folder_id,
    a.kind,
    a.primary_file_id,
    a.stack_key,
    a.stack_group_id,
    a.stack_role,
    a.stack_rank,
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
