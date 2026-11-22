import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { Check, Download, Eye, FolderPlus, Heart, Info, Star, Trash2, X } from 'lucide-react';
import {
  addAlbumAssets,
  listAlbums,
  mediaOriginalUrl,
  mediaThumbUrl,
  setAssetFavorite,
  trashAssets,
} from '../lib/api';
import { computeJustifiedRows, type JustifiedRow } from '../lib/justifiedLayout';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { notifyError, notifySuccess } from '../store/feedback';
import { useAppStore } from '../store/useAppStore';
import type { AlbumSummary, AssetSummary } from '../types/api';
import { openContextMenu } from './ContextMenu';

const HEADER_ROW_HEIGHT = 42;
const ROW_GAP = 4;

function formatVideoDuration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return 'VIDEO';
  const totalSec = Math.max(0, Math.round(ms / 1000));
  const hours = Math.floor(totalSec / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (hours > 0) {
    return `${hours}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
  }
  return `${mins}:${String(secs).padStart(2, '0')}`;
}

function getTilePill(asset: AssetSummary): string | null {
  switch (asset.kind) {
    case 'raw_jpg_stack':
      return 'RAW+JPG';
    case 'live_photo':
      return 'LIVE';
    case 'motion_photo':
      return 'MOTION';
    case 'video':
      return formatVideoDuration(asset.duration_ms);
    default:
      return null;
  }
}

export interface JustifiedTimelineProps {
  assets: AssetSummary[];
  hasMore?: boolean;
  loadingMore?: boolean;
  onLoadMore?: () => void;
  onAssetUpdated?: (assetId: string, patch: Partial<AssetSummary>) => void;
  onAssetsTrashed?: (assetIds: string[]) => void;
  extraBulkActions?: (selectedIds: string[], clearSelection: () => void) => React.ReactNode;
}

export const JustifiedTimeline: React.FC<JustifiedTimelineProps> = ({
  assets,
  hasMore = false,
  loadingMore = false,
  onLoadMore,
  onAssetUpdated,
  onAssetsTrashed,
  extraBulkActions,
}) => {
  const user = useAppStore((s) => s.user);
  const openLightbox = useAppStore((s) => s.openLightbox);
  const selectAsset = useAppStore((s) => s.selectAsset);
  const setInspectorOpen = useAppStore((s) => s.setInspectorOpen);
  const bumpLibraryRevision = useAppStore((s) => s.bumpLibraryRevision);

  const canMutate = user?.role === 'admin' || user?.role === 'contributor';

  const containerRef = useRef<HTMLDivElement | null>(null);
  const [containerWidth, setContainerWidth] = useState<number>(960);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const lastSelectedIdxRef = useRef<number | null>(null);

  const [albumPickerOpen, setAlbumPickerOpen] = useState(false);
  const [albums, setAlbums] = useState<AlbumSummary[]>([]);
  const [loadingAlbums, setLoadingAlbums] = useState(false);
  const [bulkBusy, setBulkBusy] = useState(false);

  // Measure container width with ResizeObserver, keeping safe 960px fallback in jsdom/SSR.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    const updateWidth = (rawWidth: number) => {
      if (Number.isFinite(rawWidth) && rawWidth > 0) {
        setContainerWidth(Math.round(rawWidth));
      }
    };

    updateWidth(el.clientWidth);

    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      const measured = entry ? entry.contentRect.width : el.clientWidth;
      updateWidth(measured);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Prune selections that are no longer present in the loaded assets list.
  useEffect(() => {
    const assetSet = new Set(assets.map((a) => a.id));
    setSelectedIds((prev) => {
      if (prev.size === 0) return prev;
      let changed = false;
      const next = new Set<string>();
      for (const id of prev) {
        if (assetSet.has(id)) next.add(id);
        else changed = true;
      }
      return changed ? next : prev;
    });
  }, [assets]);

  const targetRowHeight = containerWidth < 768 ? 130 : 210;
  // ResizeObserver contentRect.width already excludes .justified-timeline's right padding.
  const effectiveGridWidth = Math.max(240, containerWidth);

  const rows: JustifiedRow[] = useMemo(
    () => computeJustifiedRows(assets, effectiveGridWidth, targetRowHeight, ROW_GAP),
    [assets, effectiveGridWidth, targetRowHeight],
  );

  const allAssetIds = useMemo(() => assets.map((a) => a.id), [assets]);

  const assetIndexById = useMemo(() => {
    const map = new Map<string, number>();
    assets.forEach((asset, idx) => map.set(asset.id, idx));
    return map;
  }, [assets]);

  const getScrollElement = useCallback((): HTMLElement | null => {
    const el = containerRef.current;
    if (!el) return null;
    const viewport = el.closest('.content-viewport');
    return (viewport as HTMLElement | null) ?? el;
  }, []);

  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement,
    estimateSize: (index) => {
      const row = rows[index];
      if (!row) return targetRowHeight + ROW_GAP;
      return row.type === 'header' ? HEADER_ROW_HEIGHT : row.height + ROW_GAP;
    },
    overscan: 5,
    initialRect: { width: 960, height: 800 },
    getItemKey: (index) => rows[index]?.key ?? index,
  });

  // Re-measure virtual rows whenever row layout, container width, or row height changes
  // so @tanstack/react-virtual never uses stale cached offsets that cause vertical row overlap.
  useEffect(() => {
    rowVirtualizer.measure();
  }, [rowVirtualizer, rows, containerWidth, targetRowHeight]);

  const virtualItems = rowVirtualizer.getVirtualItems();

  // Trigger cursor pagination when approaching the bottom 25% of loaded rows.
  useEffect(() => {
    if (!hasMore || loadingMore || !onLoadMore || rows.length === 0) return;
    const lastVirtual = virtualItems[virtualItems.length - 1];
    if (!lastVirtual) return;
    const thresholdIndex = Math.floor(rows.length * 0.75);
    if (lastVirtual.index >= thresholdIndex) {
      onLoadMore();
    }
  }, [hasMore, loadingMore, onLoadMore, rows.length, virtualItems]);

  const clearSelection = useCallback(() => {
    setSelectedIds(new Set());
    lastSelectedIdxRef.current = null;
    setAlbumPickerOpen(false);
  }, []);

  // Pressing Escape clears active multi-selection cleanly when no modal/lightbox/menu is on top.
  useEffect(() => {
    if (selectedIds.size === 0) return;
    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || !isTopEscapeLayer(layer)) return;
      if (useAppStore.getState().lightboxAssetId) return;
      event.preventDefault();
      event.stopPropagation();
      clearSelection();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [clearSelection, selectedIds.size]);

  const toggleSelect = useCallback(
    (assetId: string, shiftKey: boolean) => {
      const idx = assetIndexById.get(assetId) ?? -1;
      setSelectedIds((prev) => {
        const next = new Set(prev);
        if (shiftKey && lastSelectedIdxRef.current !== null && idx >= 0) {
          const start = Math.min(lastSelectedIdxRef.current, idx);
          const end = Math.max(lastSelectedIdxRef.current, idx);
          for (let i = start; i <= end; i += 1) {
            const target = assets[i];
            if (target) next.add(target.id);
          }
        } else {
          if (next.has(assetId)) next.delete(assetId);
          else next.add(assetId);
        }
        return next;
      });
      if (idx >= 0) {
        lastSelectedIdxRef.current = idx;
      }
    },
    [assetIndexById, assets],
  );

  const handleToggleFavorite = async (asset: AssetSummary) => {
    if (!canMutate) return;
    const nextFav = !asset.is_favorite;
    onAssetUpdated?.(asset.id, { is_favorite: nextFav });
    try {
      await setAssetFavorite(asset.id, nextFav);
    } catch (err) {
      onAssetUpdated?.(asset.id, { is_favorite: asset.is_favorite });
      notifyError(err, 'Could not update favorite');
    }
  };

  const handleBulkFavorite = async () => {
    if (!canMutate || selectedIds.size === 0) return;
    setBulkBusy(true);
    const ids = Array.from(selectedIds);
    try {
      await Promise.all(ids.map((id) => setAssetFavorite(id, true)));
      for (const id of ids) {
        onAssetUpdated?.(id, { is_favorite: true });
      }
      notifySuccess(`Favorited ${ids.length} item${ids.length === 1 ? '' : 's'}`);
      clearSelection();
    } catch (err) {
      notifyError(err, 'Failed to favorite selected items');
    } finally {
      setBulkBusy(false);
    }
  };

  const handleBulkTrash = async () => {
    if (!canMutate || selectedIds.size === 0) return;
    setBulkBusy(true);
    const ids = Array.from(selectedIds);
    try {
      const res = await trashAssets(ids);
      onAssetsTrashed?.(ids);
      bumpLibraryRevision();
      notifySuccess(`Moved ${res.trashed_count} item${res.trashed_count === 1 ? '' : 's'} to Trash`);
      clearSelection();
    } catch (err) {
      notifyError(err, 'Failed to move items to Trash');
    } finally {
      setBulkBusy(false);
    }
  };

  const handleOpenAlbumPicker = async () => {
    if (!canMutate) return;
    if (albumPickerOpen) {
      setAlbumPickerOpen(false);
      return;
    }
    setAlbumPickerOpen(true);
    setLoadingAlbums(true);
    try {
      const res = await listAlbums();
      setAlbums(res.items);
    } catch (err) {
      notifyError(err, 'Could not load albums');
    } finally {
      setLoadingAlbums(false);
    }
  };

  const handleAddSelectedToAlbum = async (album: AlbumSummary) => {
    if (!canMutate || selectedIds.size === 0) return;
    setBulkBusy(true);
    const ids = Array.from(selectedIds);
    try {
      const res = await addAlbumAssets(album.id, ids);
      notifySuccess(`Added ${res.added_count} item${res.added_count === 1 ? '' : 's'} to "${album.title}"`);
      clearSelection();
    } catch (err) {
      notifyError(err, 'Could not add items to album');
    } finally {
      setBulkBusy(false);
    }
  };

  const renderRow = (row: JustifiedRow) => {
    if (row.type === 'header') {
      return (
        <div className="timeline-date-header">
          <span>{row.label}</span>
          <span>{row.count}</span>
        </div>
      );
    }

    return (
      <div className="timeline-row" style={{ height: row.height }}>
        {row.items.map(({ asset, width, height }) => {
          const isSelected = selectedIds.has(asset.id);
          const pill = getTilePill(asset);
          return (
            <div
              key={asset.id}
              role="button"
              tabIndex={0}
              className={`timeline-tile${isSelected ? ' is-selected' : ''}`}
              style={{ width, height }}
              aria-label={asset.title || `${asset.kind} item`}
              onClick={(e) => {
                if (canMutate && (e.metaKey || e.ctrlKey || e.shiftKey)) {
                  e.preventDefault();
                  toggleSelect(asset.id, e.shiftKey);
                  return;
                }
                selectAsset(asset.id);
                openLightbox(asset.id, allAssetIds);
              }}
              onContextMenu={(e) => {
                openContextMenu(e, [
                  {
                    id: 'tile-open',
                    label: 'Open in Viewer',
                    icon: <Eye size={14} />,
                    onSelect: () => {
                      selectAsset(asset.id);
                      openLightbox(asset.id, allAssetIds);
                    },
                  },
                  {
                    id: 'tile-inspect',
                    label: 'Inspect Details',
                    icon: <Info size={14} />,
                    onSelect: () => {
                      selectAsset(asset.id);
                      setInspectorOpen(true);
                    },
                  },
                  ...(canMutate
                    ? [
                        {
                          id: 'tile-fav',
                          label: asset.is_favorite ? 'Remove from Favorites' : 'Add to Favorites',
                          icon: <Star size={14} />,
                          onSelect: () => handleToggleFavorite(asset),
                        },
                        {
                          id: 'tile-select',
                          label: isSelected ? 'Deselect Item' : 'Select Item',
                          icon: <Check size={14} />,
                          onSelect: () => toggleSelect(asset.id, false),
                        },
                      ]
                    : []),
                  {
                    id: 'tile-download',
                    label: 'Download Original',
                    icon: <Download size={14} />,
                    onSelect: () => {
                      window.open(mediaOriginalUrl(asset.id, { download: true }), '_blank', 'noopener');
                    },
                  },
                  ...(canMutate
                    ? [
                        { type: 'separator' as const, id: 'tile-sep-trash' },
                        {
                          id: 'tile-trash',
                          label: 'Move to Trash',
                          icon: <Trash2 size={14} />,
                          danger: true,
                          onSelect: async () => {
                            try {
                              await trashAssets([asset.id]);
                              onAssetsTrashed?.([asset.id]);
                              bumpLibraryRevision();
                              notifySuccess('Moved item to Trash');
                            } catch (err) {
                              notifyError(err, 'Could not move item to Trash');
                            }
                          },
                        },
                      ]
                    : []),
                ]);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  selectAsset(asset.id);
                  openLightbox(asset.id, allAssetIds);
                }
              }}
            >
              <img
                src={mediaThumbUrl(asset.id)}
                alt={asset.title ?? `${asset.kind} thumbnail`}
                loading="lazy"
                decoding="async"
                draggable={false}
              />

              <button
                type="button"
                className={`tile-select-btn${isSelected ? ' is-selected' : ''}`}
                aria-label={isSelected ? 'Deselect item' : 'Select item'}
                aria-pressed={isSelected}
                onClick={(e) => {
                  e.stopPropagation();
                  toggleSelect(asset.id, e.shiftKey);
                }}
              >
                {isSelected && <Check size={12} strokeWidth={2.5} />}
              </button>

              {(canMutate || asset.is_favorite) && (
                <button
                  type="button"
                  className={`tile-fav-btn${asset.is_favorite ? ' is-favorite' : ''}`}
                  aria-label={asset.is_favorite ? 'Unfavorite' : 'Favorite'}
                  aria-pressed={asset.is_favorite}
                  disabled={!canMutate}
                  onClick={(e) => {
                    e.stopPropagation();
                    void handleToggleFavorite(asset);
                  }}
                >
                  <Star size={13} fill={asset.is_favorite ? 'currentColor' : 'none'} />
                </button>
              )}

              {pill && <span className="tile-pill">{pill}</span>}
            </div>
          );
        })}
      </div>
    );
  };

  // Fallback for jsdom environments where scroll container clientHeight === 0
  const useFallbackRows = virtualItems.length === 0 && rows.length > 0;

  return (
    <div
      ref={containerRef}
      className={`justified-timeline${selectedIds.size > 0 ? ' has-selection' : ''}`}
    >
      {useFallbackRows ? (
        <div className="stack" style={{ gap: ROW_GAP }}>
          {rows.slice(0, 40).map((row) => (
            <div key={row.key}>{renderRow(row)}</div>
          ))}
        </div>
      ) : (
        <div
          style={{
            position: 'relative',
            width: '100%',
            height: rowVirtualizer.getTotalSize(),
          }}
        >
          {virtualItems.map((virtualRow) => {
            const row = rows[virtualRow.index];
            if (!row) return null;
            return (
              <div
                key={row.key}
                data-index={virtualRow.index}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  height: `${virtualRow.size}px`,
                  boxSizing: 'border-box',
                  overflow: 'hidden',
                  transform: `translateY(${virtualRow.start}px)`,
                }}
              >
                {renderRow(row)}
              </div>
            );
          })}
        </div>
      )}

      {loadingMore && (
        <div className="view-loading" style={{ padding: '16px 0' }} role="status">
          Loading more…
        </div>
      )}

      {selectedIds.size > 0 && (
        <div className="bulk-action-bar" role="region" aria-label="Bulk selection actions">
          <span style={{ fontSize: 12, fontWeight: 500, padding: '0 6px' }}>
            {selectedIds.size} selected
          </span>

          {canMutate && (
            <>
              <button
                type="button"
                className="button"
                disabled={bulkBusy}
                onClick={() => void handleBulkFavorite()}
              >
                <Heart size={13} />
                Favorite
              </button>

              <div style={{ position: 'relative' }}>
                <button
                  type="button"
                  className="button"
                  disabled={bulkBusy}
                  onClick={() => void handleOpenAlbumPicker()}
                >
                  <FolderPlus size={13} />
                  Add to Album
                </button>

                {albumPickerOpen && (
                  <div
                    className="panel stack"
                    role="dialog"
                    aria-label="Choose album"
                    style={{
                      position: 'absolute',
                      bottom: 'calc(100% + 8px)',
                      left: 0,
                      width: 240,
                      maxHeight: 240,
                      overflowY: 'auto',
                      padding: 8,
                      zIndex: 1000,
                      boxShadow: '0 10px 28px #000b',
                    }}
                  >
                    <div className="row" style={{ justifyContent: 'space-between' }}>
                      <strong style={{ fontSize: 12 }}>Select Album</strong>
                      <button
                        type="button"
                        className="icon-button"
                        aria-label="Close album picker"
                        onClick={() => setAlbumPickerOpen(false)}
                      >
                        <X size={13} />
                      </button>
                    </div>
                    {loadingAlbums ? (
                      <div className="view-subtitle">Loading albums…</div>
                    ) : albums.length === 0 ? (
                      <div className="view-subtitle">No albums created yet.</div>
                    ) : (
                      albums.map((album) => (
                        <button
                          key={album.id}
                          type="button"
                          className="button button-quiet"
                          style={{ justifyContent: 'space-between', width: '100%' }}
                          onClick={() => void handleAddSelectedToAlbum(album)}
                        >
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                            {album.title}
                          </span>
                          <span className="view-subtitle" style={{ marginTop: 0 }}>
                            {album.asset_count}
                          </span>
                        </button>
                      ))
                    )}
                  </div>
                )}
              </div>

              {extraBulkActions?.(Array.from(selectedIds), clearSelection)}

              <button
                type="button"
                className="button button-danger-quiet"
                disabled={bulkBusy}
                onClick={() => void handleBulkTrash()}
              >
                <Trash2 size={13} />
                Move to Trash
              </button>
            </>
          )}

          <button type="button" className="button button-quiet" onClick={clearSelection}>
            Clear
          </button>
        </div>
      )}
    </div>
  );
};
