import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderOpen,
  FolderPlus,
  Images,
  Layers,
  Upload,
  X,
} from 'lucide-react';
import { listAssets, listFolderSubfolders } from '../../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../../lib/escapeStack';
import { plural } from '../../lib/format';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary, FolderSubfolderView } from '../../types/api';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { EmptyState, ViewHeader } from '../ui';

export const TimelineView: React.FC = () => {
  const folders = useAppStore((s) => s.folders);
  const user = useAppStore((s) => s.user);
  const setActiveNav = useAppStore((s) => s.setActiveNav);
  const setUploadModalOpen = useAppStore((s) => s.setUploadModalOpen);
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const canUpload = user?.role === 'admin' || user?.role === 'contributor';

  // Folder / nested subfolder / stack-collapse filter state
  const [selectedFolderId, setSelectedFolderId] = useState<string | null>(null);
  const [selectedPathPrefix, setSelectedPathPrefix] = useState<string | null>(null);
  const [collapseStacks, setCollapseStacks] = useState<boolean>(true);
  const [folderPickerOpen, setFolderPickerOpen] = useState(false);
  const [subfoldersByFolder, setSubfoldersByFolder] = useState<
    Record<string, FolderSubfolderView[]>
  >({});
  const [expandedFolders, setExpandedFolders] = useState<Record<string, boolean>>({});
  const [expandedSubpaths, setExpandedSubpaths] = useState<Record<string, boolean>>({});
  const [loadingSubfoldersFor, setLoadingSubfoldersFor] = useState<string | null>(null);

  const [items, setItems] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const seqRef = useRef(0);
  const hasLoadedOnceRef = useRef(false);
  const pickerRef = useRef<HTMLDivElement | null>(null);

  const selectedFolder = useMemo(
    () => (selectedFolderId ? folders.find((f) => f.id === selectedFolderId) ?? null : null),
    [folders, selectedFolderId],
  );

  // If a previously selected folder was deleted in Settings, reset cleanly to All Folders.
  useEffect(() => {
    if (selectedFolderId && folders.length > 0 && !folders.some((f) => f.id === selectedFolderId)) {
      setSelectedFolderId(null);
      setSelectedPathPrefix(null);
    }
  }, [folders, selectedFolderId]);

  // Clear cached subfolder trees whenever library content changes so new directories appear.
  useEffect(() => {
    setSubfoldersByFolder({});
  }, [libraryRevision]);

  // Close folder picker on outside click or Escape.
  useEffect(() => {
    if (!folderPickerOpen) return;
    const layer = pushEscapeLayer();
    const onMouseDown = (event: MouseEvent) => {
      if (pickerRef.current && !pickerRef.current.contains(event.target as Node)) {
        setFolderPickerOpen(false);
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        setFolderPickerOpen(false);
      }
    };
    document.addEventListener('mousedown', onMouseDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('mousedown', onMouseDown);
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [folderPickerOpen]);

  const ensureSubfoldersLoaded = useCallback(
    async (folderId: string) => {
      if (subfoldersByFolder[folderId] !== undefined) return;
      setLoadingSubfoldersFor(folderId);
      try {
        const res = await listFolderSubfolders(folderId);
        setSubfoldersByFolder((prev) => ({ ...prev, [folderId]: res.items }));
      } catch {
        setSubfoldersByFolder((prev) => ({ ...prev, [folderId]: [] }));
      } finally {
        setLoadingSubfoldersFor((cur) => (cur === folderId ? null : cur));
      }
    },
    [subfoldersByFolder],
  );

  const toggleExpandFolder = (folderId: string) => {
    const next = !expandedFolders[folderId];
    setExpandedFolders((prev) => ({ ...prev, [folderId]: next }));
    if (next) {
      void ensureSubfoldersLoaded(folderId);
    }
  };

  const selectAllFolders = () => {
    setSelectedFolderId(null);
    setSelectedPathPrefix(null);
    setFolderPickerOpen(false);
  };

  const selectFolder = (folderId: string, pathPrefix: string | null = null) => {
    setSelectedFolderId(folderId);
    setSelectedPathPrefix(pathPrefix);
    setFolderPickerOpen(false);
  };

  const loadInitial = useCallback(async () => {
    const seq = ++seqRef.current;
    if (!hasLoadedOnceRef.current) {
      setLoading(true);
    }
    setLoadingMore(false);
    setError(null);
    setLoadMoreError(false);
    try {
      const assetsRes = await listAssets({
        limit: 120,
        folder_id: selectedFolderId ?? undefined,
        path_prefix: selectedFolderId && selectedPathPrefix ? selectedPathPrefix : undefined,
        collapse_stacks: collapseStacks,
      });
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
  }, [collapseStacks, selectedFolderId, selectedPathPrefix]);

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
        folder_id: selectedFolderId ?? undefined,
        path_prefix: selectedFolderId && selectedPathPrefix ? selectedPathPrefix : undefined,
        collapse_stacks: collapseStacks,
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
  }, [collapseStacks, loadingMore, nextCursor, selectedFolderId, selectedPathPrefix]);

  const handleAssetUpdated = useCallback((assetId: string, patch: Partial<AssetSummary>) => {
    setItems((prev) => prev.map((a) => (a.id === assetId ? { ...a, ...patch } : a)));
  }, []);

  const handleAssetsTrashed = useCallback((assetIds: string[]) => {
    const removed = new Set(assetIds);
    setItems((prev) => prev.filter((a) => !removed.has(a.id)));
    setTotalCount((prev) => Math.max(0, prev - assetIds.length));
  }, []);

  const activeScopeLabel = useMemo(() => {
    if (!selectedFolder) return 'All Folders';
    if (!selectedPathPrefix) return selectedFolder.name;
    const leaf = selectedPathPrefix.split('/').pop() ?? selectedPathPrefix;
    return `${selectedFolder.name} / ${leaf}`;
  }, [selectedFolder, selectedPathPrefix]);

  const breadcrumbSegments = useMemo(() => {
    if (!selectedFolder) return [];
    const segs: Array<{ label: string; pathPrefix: string | null }> = [
      { label: selectedFolder.name, pathPrefix: null },
    ];
    if (selectedPathPrefix) {
      let accum = '';
      for (const part of selectedPathPrefix.split('/').filter(Boolean)) {
        accum = accum ? `${accum}/${part}` : part;
        segs.push({ label: part, pathPrefix: accum });
      }
    }
    return segs;
  }, [selectedFolder, selectedPathPrefix]);

  const subtitle = useMemo(() => {
    if (loading && !hasLoadedOnceRef.current) return '\u00A0';
    const base = selectedFolder
      ? selectedPathPrefix
        ? `${plural(totalCount, 'photo')} in ${selectedFolder.name} / ${selectedPathPrefix}`
        : `${plural(totalCount, 'photo')} in ${selectedFolder.name}`
      : `${plural(totalCount, 'photo')} in ${plural(folders.length, 'folder')}`;
    return collapseStacks ? base : `${base} · unstacked`;
  }, [collapseStacks, folders.length, loading, selectedFolder, selectedPathPrefix, totalCount]);

  // Helper to render nested subfolder tree rows for a given watched folder
  const renderSubfolderTree = (folderId: string, subs: FolderSubfolderView[]) => {
    if (subs.length === 0) {
      return <div className="folder-picker-empty">No subfolders</div>;
    }
    const childrenByParent = new Map<string | null, FolderSubfolderView[]>();
    for (const sub of subs) {
      const list = childrenByParent.get(sub.parent_path) ?? [];
      list.push(sub);
      childrenByParent.set(sub.parent_path, list);
    }

    const renderLevel = (parentPath: string | null): React.ReactNode => {
      const nodes = childrenByParent.get(parentPath);
      if (!nodes || nodes.length === 0) return null;
      return nodes.map((node) => {
        const subKey = `${folderId}:${node.path}`;
        const hasChildren = (childrenByParent.get(node.path)?.length ?? 0) > 0;
        const isExpanded = Boolean(expandedSubpaths[subKey]);
        const isSelected = selectedFolderId === folderId && selectedPathPrefix === node.path;
        return (
          <React.Fragment key={subKey}>
            <div
              className={`folder-picker-row${isSelected ? ' is-active' : ''}`}
              style={{ paddingLeft: 26 + node.depth * 14 }}
            >
              {hasChildren ? (
                <button
                  type="button"
                  className="folder-picker-chevron"
                  aria-label={isExpanded ? `Collapse ${node.name}` : `Expand ${node.name}`}
                  aria-expanded={isExpanded}
                  onClick={(e) => {
                    e.stopPropagation();
                    setExpandedSubpaths((prev) => ({ ...prev, [subKey]: !prev[subKey] }));
                  }}
                >
                  {isExpanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                </button>
              ) : (
                <span className="folder-picker-chevron-spacer" aria-hidden="true" />
              )}
              <button
                type="button"
                className="folder-picker-item-btn"
                onClick={() => selectFolder(folderId, node.path)}
              >
                <Folder size={13} className="folder-picker-icon" />
                <span className="folder-picker-name">{node.name}</span>
                <span className="folder-picker-count">{node.asset_count}</span>
                {isSelected && <Check size={13} className="folder-picker-check" />}
              </button>
            </div>
            {hasChildren && isExpanded && renderLevel(node.path)}
          </React.Fragment>
        );
      });
    };

    return renderLevel(null);
  };

  return (
    <div>
      <ViewHeader
        title="Photos"
        subtitle={subtitle}
        actions={
          folders.length > 0 ? (
            <div className="folder-picker-wrap" ref={pickerRef}>
              <button
                type="button"
                className={`button folder-picker-trigger${selectedFolderId ? ' is-filtered' : ''}`}
                aria-haspopup="dialog"
                aria-expanded={folderPickerOpen}
                aria-label={`Folder filter: ${activeScopeLabel}`}
                onClick={() => setFolderPickerOpen((open) => !open)}
              >
                {selectedFolderId ? <FolderOpen size={14} /> : <Folder size={14} />}
                <span className="folder-picker-trigger-label">{activeScopeLabel}</span>
                <ChevronDown size={13} />
              </button>

              {folderPickerOpen && (
                <div
                  className="folder-picker-popover"
                  role="dialog"
                  aria-label="Filter timeline by folder"
                >
                  <div className="folder-picker-section-label">Library Scope</div>
                  <div className={`folder-picker-row${selectedFolderId === null ? ' is-active' : ''}`}>
                    <span className="folder-picker-chevron-spacer" aria-hidden="true" />
                    <button
                      type="button"
                      className="folder-picker-item-btn"
                      onClick={selectAllFolders}
                    >
                      <Images size={14} className="folder-picker-icon" />
                      <span className="folder-picker-name">All Folders</span>
                      {selectedFolderId === null && <Check size={13} className="folder-picker-check" />}
                    </button>
                  </div>

                  <div className="folder-picker-divider" />
                  <div className="folder-picker-section-label">Watched Folders</div>
                  <div className="folder-picker-list">
                    {folders.map((folder) => {
                      const isExpanded = Boolean(expandedFolders[folder.id]);
                      const isFolderSelected =
                        selectedFolderId === folder.id && selectedPathPrefix === null;
                      const subs = subfoldersByFolder[folder.id];
                      const isLoadingSubs = loadingSubfoldersFor === folder.id;
                      return (
                        <React.Fragment key={folder.id}>
                          <div
                            className={`folder-picker-row${isFolderSelected ? ' is-active' : ''}`}
                          >
                            <button
                              type="button"
                              className="folder-picker-chevron"
                              aria-label={
                                isExpanded
                                  ? `Hide subfolders of ${folder.name}`
                                  : `Browse subfolders of ${folder.name}`
                              }
                              aria-expanded={isExpanded}
                              onClick={(e) => {
                                e.stopPropagation();
                                toggleExpandFolder(folder.id);
                              }}
                            >
                              {isExpanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                            </button>
                            <button
                              type="button"
                              className="folder-picker-item-btn"
                              onClick={() => selectFolder(folder.id, null)}
                            >
                              <Folder size={14} className="folder-picker-icon" />
                              <span className="folder-picker-name">{folder.name}</span>
                              <span className="folder-picker-count">{folder.asset_count}</span>
                              {isFolderSelected && (
                                <Check size={13} className="folder-picker-check" />
                              )}
                            </button>
                          </div>
                          {isExpanded &&
                            (isLoadingSubs ? (
                              <div className="folder-picker-empty">Loading subfolders…</div>
                            ) : subs ? (
                              renderSubfolderTree(folder.id, subs)
                            ) : null)}
                        </React.Fragment>
                      );
                    })}
                  </div>

                  <div className="folder-picker-divider" />
                  <label className="folder-picker-stack-toggle">
                    <span className="row" style={{ gap: 7 }}>
                      <Layers size={13} />
                      <span>Stack edited &amp; RAW versions</span>
                    </span>
                    <input
                      type="checkbox"
                      checked={collapseStacks}
                      onChange={(e) => setCollapseStacks(e.target.checked)}
                    />
                  </label>
                </div>
              )}
            </div>
          ) : undefined
        }
      />

      {selectedFolder && (
        <div className="folder-breadcrumbs" aria-label="Active folder filter">
          <button
            type="button"
            className="folder-breadcrumb-btn"
            onClick={selectAllFolders}
          >
            All Folders
          </button>
          {breadcrumbSegments.map((seg, idx) => {
            const isLast = idx === breadcrumbSegments.length - 1;
            return (
              <React.Fragment key={seg.pathPrefix ?? 'root'}>
                <ChevronRight size={12} className="folder-breadcrumb-sep" aria-hidden="true" />
                <button
                  type="button"
                  className={`folder-breadcrumb-btn${isLast ? ' is-current' : ''}`}
                  disabled={isLast}
                  onClick={() => selectFolder(selectedFolder.id, seg.pathPrefix)}
                >
                  {seg.label}
                </button>
              </React.Fragment>
            );
          })}
          <button
            type="button"
            className="folder-breadcrumb-clear"
            aria-label="Clear folder filter"
            title="Show all folders"
            onClick={selectAllFolders}
          >
            <X size={12} />
          </button>
        </div>
      )}

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
          loadMoreError={loadMoreError}
          onLoadMore={handleLoadMore}
          onAssetUpdated={handleAssetUpdated}
          onAssetsTrashed={handleAssetsTrashed}
        />
      ) : selectedFolder ? (
        <EmptyState
          icon={<FolderOpen size={28} strokeWidth={1.3} />}
          title="No photos in this folder"
          description={`No media matched ${activeScopeLabel}.`}
          actions={
            <button type="button" className="button button-primary" onClick={selectAllFolders}>
              Show All Folders
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
