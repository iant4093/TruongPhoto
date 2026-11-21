import React, { useCallback, useEffect, useState } from 'react';
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
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listAssets({ favorite: true, limit: 120 });
      setItems(res.items);
      setNextCursor(res.next_cursor);
      setTotalCount(res.total_count);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, libraryRevision]);

  const handleLoadMore = useCallback(async () => {
    if (!nextCursor || loadingMore) return;
    setLoadingMore(true);
    try {
      const res = await listAssets({ favorite: true, limit: 120, cursor: nextCursor });
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
        subtitle={`${plural(totalCount, 'favorite asset')} in your allowed folders`}
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
          hasMore={nextCursor !== null}
          loadingMore={loadingMore}
          onLoadMore={() => void handleLoadMore()}
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
