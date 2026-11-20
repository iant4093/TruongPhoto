-- 0002_phase1_media.sql — Phase 1 RGB Histograms, Bounded LRU Cache Tracking & Ingest/Trash Indexes

-- Compact 256-bin RGB + Rec.709 Luma histograms normalized to u16 [0..65535] (512 bytes each = 2 KB/asset).
-- Stored in a side table to keep `assets` rows lean for 500k timeline scans.
CREATE TABLE IF NOT EXISTS asset_histograms (
    asset_id        TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    r_bins          BLOB NOT NULL,
    g_bins          BLOB NOT NULL,
    b_bins          BLOB NOT NULL,
    luma_bins       BLOB NOT NULL,
    updated_at_ms   INTEGER NOT NULL
);

-- Tracks ONLY evictable artifacts in `cache_dir` (2048px previews, LibRaw renders, HLS proxies, live clips).
-- Persistent 400px grid thumbnails in `thumbnails_dir` are NEVER stored here and NEVER LRU-evicted.
CREATE TABLE IF NOT EXISTS lru_cache_entries (
    cache_key        TEXT PRIMARY KEY NOT NULL,
    asset_id         TEXT REFERENCES assets(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL CHECK (kind IN ('preview_2048', 'raw_full', 'hls_proxy', 'live_proxy', 'motion_video')),
    rel_path         TEXT NOT NULL,
    size_bytes       INTEGER NOT NULL,
    last_accessed_ms INTEGER NOT NULL,
    created_at_ms    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_lru_cache_evict ON lru_cache_entries(last_accessed_ms ASC);
CREATE INDEX IF NOT EXISTS idx_lru_cache_asset ON lru_cache_entries(asset_id);

-- Fast lookups for album asset ordering, 30-day trash auto-purge, and partial BLAKE3 move/rename re-linking
CREATE INDEX IF NOT EXISTS idx_album_assets_order ON album_assets(album_id, position ASC, added_at_ms ASC);
CREATE INDEX IF NOT EXISTS idx_assets_trash_age ON assets(deleted_at_ms ASC) WHERE deleted_at_ms IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_asset_files_missing ON asset_files(folder_id, is_missing) WHERE is_missing = 1;
