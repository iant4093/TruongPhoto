import React, { useCallback, useEffect, useRef, useState } from 'react';
import { FolderPlus, Images, Upload } from 'lucide-react';
import { listAssets } from '../../lib/api';
import { plural } from '../../lib/format';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary } from '../../types/api';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { EmptyState, ViewHeader } from '../ui';

export const TimelineView: React.FC = () => {
  const folders = useAppStore((s) => s.folders);
  const user = useAppStore((s) => s.user);
  const setActiveNav = useAppStore((s) => s.setActiveNav);
  const setUploadModalOpen = useAppStore((s) => s.setUploadModalOpen);
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const canUpload = user?.role === 'admin' || user?.role === 'contributor';

  const [items, setItems] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const seqRef = useRef(0);
  const hasLoadedOnceRef = useRef(false);

  const loadInitial = useCallback(async () => {
    const seq = ++seqRef.current;
    if (!hasLoadedOnceRef.current) {
      setLoading(true);
    }
    setError(null);
    setLoadMoreError(false);
    try {
      const assetsRes = await listAssets({ limit: 120 });
      if (seq !== seqRef.current) return;
      hasLoadedOnceRef.current = true;
      setItems(assetsRes.items);
      setNextCursor(assetsRes.next_cursor);
      setTotalCount(assetsRes.total_count);
    } catch (err) {
      if (seq !== seqRef.current) return;
      setError(errorMessage(err));
    } finally {
      if (seq === seqRef.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void loadInitial();
  }, [loadInitial, libraryRevision]);

  const handleLoadMore = useCallback(async () => {
    if (!nextCursor || loadingMore) return;
    const seq = seqRef.current;
    setLoadingMore(true);
    setLoadMoreError(false);
    try {
      const res = await listAssets({
        limit: 120,
        cursor: nextCursor,
      });
      if (seq !== seqRef.current) return;
      setItems((prev) => {
        const seen = new Set(prev.map((a) => a.id));
        const appended = res.items.filter((a) => !seen.has(a.id));
        return [...prev, ...appended];
      });
      setNextCursor(res.next_cursor);
      setTotalCount(res.total_count);
    } catch (err) {
      if (seq !== seqRef.current) return;
      setLoadMoreError(true);
      setError(errorMessage(err));
    } finally {
      if (seq === seqRef.current) {
        setLoadingMore(false);
      }
    }
  }, [loadingMore, nextCursor]);

  const handleAssetUpdated = useCallback((assetId: string, patch: Partial<AssetSummary>) => {
    setItems((prev) => prev.map((a) => (a.id === assetId ? { ...a, ...patch } : a)));
  }, []);

  const handleAssetsTrashed = useCallback((assetIds: string[]) => {
    const removed = new Set(assetIds);
    setItems((prev) => prev.filter((a) => !removed.has(a.id)));
    setTotalCount((prev) => Math.max(0, prev - assetIds.length));
  }, []);

  return (
    <div>
      <ViewHeader
        title="Photos"
        subtitle={`${plural(totalCount, 'photo')} in ${plural(folders.length, 'folder')}`}
      />

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button
            type="button"
            className="button"
            onClick={() => void loadInitial()}
          >
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading timeline…
        </div>
      ) : items.length > 0 ? (
        <JustifiedTimeline
          assets={items}
          hasMore={nextCursor !== null && !loadMoreError}
          loadingMore={loadingMore}
          onLoadMore={handleLoadMore}
          onAssetUpdated={handleAssetUpdated}
          onAssetsTrashed={handleAssetsTrashed}
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
