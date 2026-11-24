-- Round 5 performance indexes for people face lookup, AI Stage-2 queue checks,
-- move/rename fingerprint relinking, favorites timeline, and lens queries.

CREATE INDEX IF NOT EXISTS idx_faces_person_score
    ON faces(person_id, det_score DESC, created_at_ms ASC, id ASC);

CREATE INDEX IF NOT EXISTS idx_asset_files_asset_missing
    ON asset_files(asset_id, is_missing);

CREATE INDEX IF NOT EXISTS idx_asset_files_missing_fp
    ON asset_files(folder_id, partial_blake3, file_size)
    WHERE is_missing = 1;

CREATE INDEX IF NOT EXISTS idx_assets_favorites_capture
    ON assets(capture_at_ms DESC, id DESC)
    WHERE deleted_at_ms IS NULL AND is_hidden = 0 AND is_favorite = 1;

CREATE INDEX IF NOT EXISTS idx_photo_meta_lens_model
    ON deterministic_photo_meta(lens_model);
