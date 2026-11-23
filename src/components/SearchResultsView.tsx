import React, { useEffect, useMemo, useState } from 'react';
import { Image as ImageIcon, Play, Search } from 'lucide-react';
import { mediaThumbUrl } from '../lib/api';
import { formatDateTime } from '../lib/format';
import { notifyError } from '../store/feedback';
import { useAppStore } from '../store/useAppStore';
import type { PlannerMode, SearchHit } from '../types/api';
import { SearchFilterBar } from './SearchFilterBar';
import { EmptyState, ViewHeader } from './ui';

function formatPlannerMode(mode: PlannerMode): string {
  switch (mode) {
    case 'exact_simd':
      return 'Exact SIMD';
    case 'hnsw':
      return 'HNSW';
    case 'sql_only':
      return 'SQL Only';
  }
}

function formatTimestamp(seconds: number): string {
  const totalSec = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(totalSec / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (hours > 0) {
    return `${hours}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
  }
  return `${mins}:${String(secs).padStart(2, '0')}`;
}

const SearchHitCard: React.FC<{
  hit: SearchHit;
  onOpen: (hit: SearchHit) => void;
}> = ({ hit, onOpen }) => {
  const [imgFailed, setImgFailed] = useState(false);
  useEffect(() => {
    setImgFailed(false);
  }, [hit.asset.id]);
  const title = hit.asset.title || hit.asset.camera_model || 'Media item';
  const showTimestamp = hit.timestamp_sec != null && hit.timestamp_sec > 0;

  return (
    <button
      type="button"
      className="search-hit-card"
      onClick={() => onOpen(hit)}
      aria-label={`Open ${title}`}
    >
      <div className="search-hit-thumb">
        {!imgFailed ? (
          <img
            src={mediaThumbUrl(hit.asset.id)}
            alt={title}
            loading="lazy"
            onError={() => setImgFailed(true)}
          />
        ) : (
          <div className="thumbnail-placeholder" role="img" aria-label={title}>
            <ImageIcon size={24} strokeWidth={1.2} />
          </div>
        )}

        {showTimestamp && (
          <span className="search-ts-badge">
            <Play size={10} fill="currentColor" />
            <span>{formatTimestamp(hit.timestamp_sec!)}</span>
          </span>
        )}
      </div>

      <div style={{ padding: '8px 10px 4px' }}>
        <div
          style={{
            fontSize: 12,
            fontWeight: 600,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
        >
          {hit.asset.title || formatDateTime(hit.asset.capture_at_ms)}
        </div>
        {hit.asset.title && (
          <div className="view-subtitle" style={{ marginTop: 2, fontSize: 11 }}>
            {formatDateTime(hit.asset.capture_at_ms)}
          </div>
        )}
      </div>

      {hit.match_reasons.length > 0 && (
        <div className="search-reason-badges">
          {hit.match_reasons.map((reason, idx) => (
            <span key={`${reason}-${idx}`} className="search-reason-badge">
              {reason}
            </span>
          ))}
        </div>
      )}

      {hit.ocr_snippet && <div className="search-ocr-snippet">{hit.ocr_snippet}</div>}
    </button>
  );
};

export const SearchResultsView: React.FC = () => {
  const searchQuery = useAppStore((s) => s.searchQuery);
  const searchResponse = useAppStore((s) => s.searchResponse);
  const searchLoading = useAppStore((s) => s.searchLoading);
  const loadMoreSearch = useAppStore((s) => s.loadMoreSearch);
  const clearSearch = useAppStore((s) => s.clearSearch);
  const openLightbox = useAppStore((s) => s.openLightbox);

  const hits = searchResponse?.items ?? [];
  const totalCount = searchResponse?.total_count ?? 0;
  const hasMore = hits.length < totalCount;
  const allHitIds = useMemo(
    () => Array.from(new Set(hits.map((h) => h.asset.id))),
    [hits],
  );

  const telemetryText = searchResponse
    ? `${searchResponse.total_count} results · ${formatPlannerMode(searchResponse.planner_mode)} · ${searchResponse.elapsed_ms} ms`
    : null;

  const handleOpenHit = (hit: SearchHit) => {
    openLightbox(hit.asset.id, allHitIds, hit.timestamp_sec ?? null);
  };

  return (
    <div>
      <ViewHeader
        title="Search"
        subtitle={
          searchQuery.trim()
            ? `Results for "${searchQuery.trim()}"`
            : 'Filtered hybrid library search'
        }
        actions={
          telemetryText ? (
            <span className="search-telemetry-pill" data-testid="search-telemetry-pill">
              {telemetryText}
            </span>
          ) : undefined
        }
      />

      <SearchFilterBar />

      {searchLoading && !searchResponse ? (
        <div className="view-loading" role="status">
          Searching library…
        </div>
      ) : hits.length > 0 ? (
        <>
          <div
            className="search-hit-grid"
            style={searchLoading ? { opacity: 0.6, transition: 'opacity 140ms ease' } : undefined}
          >
            {hits.map((hit) => (
              <SearchHitCard
                key={`${hit.asset.id}-${hit.timestamp_sec ?? 'photo'}`}
                hit={hit}
                onOpen={handleOpenHit}
              />
            ))}
          </div>
          {hasMore && (
            <div style={{ display: 'flex', justifyContent: 'center', marginTop: 18 }}>
              <button
                type="button"
                className="button"
                disabled={searchLoading}
                onClick={() =>
                  void loadMoreSearch().catch((err) =>
                    notifyError(err, 'Could not load more search results'),
                  )
                }
              >
                {searchLoading
                  ? 'Loading more…'
                  : `Load More (${hits.length} of ${totalCount.toLocaleString()})`}
              </button>
            </div>
          )}
        </>
      ) : (
        <EmptyState
          icon={<Search size={28} strokeWidth={1.3} />}
          title="No matching photos or videos"
          description="Try broadening your search terms, switching to Hybrid mode, or clearing active filters."
          actions={
            <button type="button" className="button" onClick={clearSearch}>
              Clear Search
            </button>
          }
        />
      )}
    </div>
  );
};
