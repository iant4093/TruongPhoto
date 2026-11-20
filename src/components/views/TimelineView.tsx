import React, { useCallback, useEffect, useState } from 'react';
import { FolderPlus, Images, Upload, X } from 'lucide-react';
import { listAssetBuckets, listAssets } from '../../lib/api';
import { plural } from '../../lib/format';
import { endOfMonthMs } from '../../lib/justifiedLayout';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary, TimelineBucket } from '../../types/api';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { TimelineScrubber } from '../TimelineScrubber';
import { Badge, EmptyState, ViewHeader } from '../ui';

const MONTH_NAMES = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
] as const;

export const TimelineView: React.FC = () => {
  const folders = useAppStore((s) => s.folders);
  const user = useAppStore((s) => s.user);
  const setActiveNav = useAppStore((s) => s.setActiveNav);
  const setUploadModalOpen = useAppStore((s) => s.setUploadModalOpen);
  const searchQuery = useAppStore((s) => s.searchQuery);
  const libraryRevision = useAppStore((s) => s.libraryRevision);
  const inspectorOpen = useAppStore((s) => s.inspectorOpen);
  const chatDrawerOpen = useAppStore((s) => s.chatDrawerOpen);

  const canUpload = user?.role === 'admin' || user?.role === 'contributor';

  const [items, setItems] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [buckets, setBuckets] = useState<TimelineBucket[]>([]);
  const [jumpTarget, setJumpTarget] = useState<{
    year: number;
    month: number;
    beforeMs: number;
  } | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadInitial = useCallback(async (beforeMs?: number) => {
    setLoading(true);
    setError(null);
    try {
      const [assetsRes, bucketsRes] = await Promise.all([
        listAssets({ limit: 120, before_ms: beforeMs }),
        listAssetBuckets(),
      ]);
      setItems(assetsRes.items);
      setNextCursor(assetsRes.next_cursor);
      setTotalCount(assetsRes.total_count);
      setBuckets(bucketsRes.buckets);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadInitial(jumpTarget?.beforeMs);
  }, [loadInitial, jumpTarget?.beforeMs, libraryRevision]);

  const handleLoadMore = useCallback(async () => {
    if (!nextCursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const res = await listAssets({
        limit: 120,
        cursor: nextCursor,
        before_ms: jumpTarget?.beforeMs,
      });
      setItems((prev) => {
        const seen = new Set(prev.map((a) => a.id));
        const appended = res.items.filter((a) => !seen.has(a.id));
        return [...prev, ...appended];
      });
      setNextCursor(res.next_cursor);
      setTotalCount(res.total_count);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoadingMore(false);
    }
  }, [jumpTarget?.beforeMs, loadingMore, nextCursor]);

  const handleJumpToMonth = useCallback((year: number, month: number) => {
    const beforeMs = endOfMonthMs(year, month);
    setJumpTarget({ year, month, beforeMs });
    if (typeof document !== 'undefined') {
      const viewport = document.querySelector('.content-viewport');
      if (viewport) viewport.scrollTop = 0;
    }
  }, []);

  const handleAssetUpdated = useCallback((assetId: string, patch: Partial<AssetSummary>) => {
    setItems((prev) => prev.map((a) => (a.id === assetId ? { ...a, ...patch } : a)));
  }, []);

  const handleAssetsTrashed = useCallback((assetIds: string[]) => {
    const removed = new Set(assetIds);
    setItems((prev) => prev.filter((a) => !removed.has(a.id)));
    setTotalCount((prev) => Math.max(0, prev - assetIds.length));
  }, []);

  const query = searchQuery.trim().toLowerCase();
  const visibleItems = query
    ? items.filter(
        (item) =>
          item.title?.toLowerCase().includes(query) ||
          item.camera_model?.toLowerCase().includes(query) ||
          item.geo_city?.toLowerCase().includes(query) ||
          item.geo_country?.toLowerCase().includes(query),
      )
    : items;

  return (
    <div>
      <ViewHeader
        title="Photos"
        subtitle={`${plural(totalCount, 'photo')} in ${plural(folders.length, 'folder')}`}
        actions={
          jumpTarget ? (
            <button
              type="button"
              className="button"
              onClick={() => setJumpTarget(null)}
            >
              <Badge tone="info">
                Before {MONTH_NAMES[jumpTarget.month - 1]} {jumpTarget.year}
              </Badge>
              <X size={13} />
              <span>Reset</span>
            </button>
          ) : undefined
        }
      />

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button
            type="button"
            className="button"
            onClick={() => void loadInitial(jumpTarget?.beforeMs)}
          >
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading timeline…
        </div>
      ) : visibleItems.length > 0 ? (
        <>
          <JustifiedTimeline
            assets={visibleItems}
            hasMore={!query && nextCursor !== null}
            loadingMore={loadingMore}
            onLoadMore={() => void handleLoadMore()}
            onAssetUpdated={handleAssetUpdated}
            onAssetsTrashed={handleAssetsTrashed}
          />
          {buckets.length > 1 && !inspectorOpen && !chatDrawerOpen && (
            <TimelineScrubber
              buckets={buckets}
              activeYear={jumpTarget?.year ?? null}
              onJumpToMonth={handleJumpToMonth}
            />
          )}
        </>
      ) : query ? (
        <EmptyState
          icon={<Images size={28} strokeWidth={1.3} />}
          title="No matching items"
          description="No loaded timeline items matched your filter. Semantic and OCR search arrive in Phases 2–3."
        />
      ) : jumpTarget ? (
        <EmptyState
          icon={<Images size={28} strokeWidth={1.3} />}
          title="No items before that date"
          description="Reset the chronological filter to return to your latest photos and videos."
          actions={
            <button
              type="button"
              className="button button-primary"
              onClick={() => setJumpTarget(null)}
            >
              Return to Latest
            </button>
          }
        />
      ) : folders.length === 0 ? (
        <EmptyState
          icon={<Images size={28} strokeWidth={1.3} />}
          title="No watched folders yet"
          description="Add a watched folder in Settings or upload photos directly to start building your library."
          actions={
            <>
              {canUpload && (
                <button
                  type="button"
                  className="button button-primary"
                  onClick={() => setUploadModalOpen(true)}
                >
                  <Upload size={14} />
                  Upload Media
                </button>
              )}
              <button
                type="button"
                className="button"
                onClick={() => setActiveNav('settings')}
              >
                <FolderPlus size={14} />
                Add a watched folder in Settings
              </button>
            </>
          }
        />
      ) : (
        <EmptyState
          icon={<Images size={28} strokeWidth={1.3} />}
          title="No media indexed yet"
          description="Upload photos or trigger a folder scan in Settings to populate your timeline."
          actions={
            canUpload ? (
              <button
                type="button"
                className="button button-primary"
                onClick={() => setUploadModalOpen(true)}
              >
                <Upload size={14} />
                Upload Media
              </button>
            ) : undefined
          }
        />
      )}
    </div>
  );
};
