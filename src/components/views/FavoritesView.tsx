import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Star } from 'lucide-react';
import { listAssets } from '../../lib/api';
import { plural } from '../../lib/format';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary } from '../../types/api';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { EmptyState, ViewHeader } from '../ui';

export const FavoritesView: React.FC = () => {
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const [items, setItems] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const seqRef = useRef(0);
  const hasLoadedOnceRef = useRef(false);

  const load = useCallback(async () => {
    const seq = ++seqRef.current;
    if (!hasLoadedOnceRef.current) {
      setLoading(true);
    }
    setError(null);
    setLoadMoreError(false);
    try {
      const res = await listAssets({ favorite: true, limit: 120 });
      if (seq !== seqRef.current) return;
      hasLoadedOnceRef.current = true;
      setItems(res.items);
      setNextCursor(res.next_cursor);
      setTotalCount(res.total_count);
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
    void load();
  }, [load, libraryRevision]);

  const handleLoadMore = useCallback(async () => {
    if (!nextCursor || loadingMore) return;
    const seq = seqRef.current;
    setLoadingMore(true);
    setLoadMoreError(false);
    try {
      const res = await listAssets({ favorite: true, limit: 120, cursor: nextCursor });
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
    if (patch.is_favorite === false) {
      setItems((prev) => prev.filter((a) => a.id !== assetId));
      setTotalCount((prev) => Math.max(0, prev - 1));
      return;
    }
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
        title="Favorites"
        subtitle={
          loading && !hasLoadedOnceRef.current
            ? '\u00A0'
            : `${plural(totalCount, 'favorite asset')} in your allowed folders`
        }
      />

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button type="button" className="button" onClick={() => void load()}>
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading favorites…
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
      ) : (
        <EmptyState
          icon={<Star size={28} strokeWidth={1.3} />}
          title="No favorite items yet"
          description="Starred photos and videos from your allowed folders will appear here."
        />
      )}
    </div>
  );
};
