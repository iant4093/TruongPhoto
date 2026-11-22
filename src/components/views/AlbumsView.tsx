import React, { useCallback, useEffect, useState } from 'react';
import {
  ArrowLeft,
  FolderHeart,
  Pencil,
  Plus,
  Share2,
  Trash2,
  X,
} from 'lucide-react';
import {
  createAlbum,
  deleteAlbum,
  getAlbumDetail,
  listAlbums,
  listShareableUsers,
  mediaThumbUrl,
  removeAlbumAssets,
  replaceAlbumShares,
  updateAlbum,
} from '../../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../../lib/escapeStack';
import { formatRelativeTime, plural } from '../../lib/format';
import { confirmAction, notifyError, notifySuccess } from '../../store/feedback';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type {
  AlbumDetail,
  AlbumShareInput,
  AlbumSharePermission,
  AlbumSortOrder,
  AlbumSummary,
  AssetSummary,
  ShareableUser,
} from '../../types/api';
import { Dropdown } from '../Dropdown';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { openContextMenu } from '../ContextMenu';
import { Badge, EmptyState, ViewHeader } from '../ui';

const SORT_ORDER_OPTIONS: ReadonlyArray<readonly [AlbumSortOrder, string]> = [
  ['capture_desc', 'Newest First'],
  ['capture_asc', 'Oldest First'],
  ['added_desc', 'Recently Added'],
  ['custom', 'Custom Order'],
];

const SHARE_PERM_OPTIONS: ReadonlyArray<readonly [AlbumSharePermission, string]> = [
  ['view', 'Can View'],
  ['contribute', 'Can Contribute'],
];

export const AlbumsView: React.FC = () => {
  const user = useAppStore((s) => s.user);
  const searchQuery = useAppStore((s) => s.searchQuery);
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const canCreate = user?.role === 'admin' || user?.role === 'contributor';

  const [items, setItems] = useState<AlbumSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Selected album detail state
  const [selectedAlbumId, setSelectedAlbumId] = useState<string | null>(null);
  const [detail, setDetail] = useState<AlbumDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  // New album modal
  const [createOpen, setCreateOpen] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newDesc, setNewDesc] = useState('');
  const [creating, setCreating] = useState(false);

  // Edit album modal
  const [editOpen, setEditOpen] = useState(false);
  const [editTitle, setEditTitle] = useState('');
  const [editDesc, setEditDesc] = useState('');
  const [editSort, setEditSort] = useState<AlbumSortOrder>('capture_desc');
  const [savingEdit, setSavingEdit] = useState(false);

  // Share modal
  const [shareOpen, setShareOpen] = useState(false);
  const [shareableUsers, setShareableUsers] = useState<ShareableUser[]>([]);
  const [shareLoading, setShareLoading] = useState(false);
  const [shareMap, setShareMap] = useState<Record<string, AlbumSharePermission | 'none'>>({});
  const [savingShares, setSavingShares] = useState(false);

  const loadList = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listAlbums();
      setItems(res.items);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadDetail = useCallback(async (albumId: string) => {
    setDetailLoading(true);
    setDetailError(null);
    try {
      const res = await getAlbumDetail(albumId);
      setDetail(res);
    } catch (err) {
      setDetailError(errorMessage(err));
    } finally {
      setDetailLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadList();
  }, [loadList, libraryRevision]);

  useEffect(() => {
    if (selectedAlbumId) {
      void loadDetail(selectedAlbumId);
    } else {
      setDetail(null);
    }
  }, [selectedAlbumId, loadDetail, libraryRevision]);

  // Escape key for modals
  useEffect(() => {
    if (!createOpen && !editOpen && !shareOpen) return;
    const layer = pushEscapeLayer();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        if (shareOpen) setShareOpen(false);
        else if (editOpen) setEditOpen(false);
        else if (createOpen) setCreateOpen(false);
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [createOpen, editOpen, shareOpen]);

  const handleCreateAlbum = async (e: React.FormEvent) => {
    e.preventDefault();
    const title = newTitle.trim();
    if (!title) {
      notifyError(new Error('Album title is required.'));
      return;
    }
    setCreating(true);
    try {
      const created = await createAlbum({
        title,
        description: newDesc.trim() || null,
      });
      setItems((prev) => [created, ...prev]);
      setCreateOpen(false);
      setNewTitle('');
      setNewDesc('');
      notifySuccess(`Created album "${created.title}"`);
    } catch (err) {
      notifyError(err, 'Could not create album');
    } finally {
      setCreating(false);
    }
  };

  const openEditModal = () => {
    if (!detail) return;
    setEditTitle(detail.album.title);
    setEditDesc(detail.album.description ?? '');
    setEditSort((detail.album.sort_order as AlbumSortOrder) || 'capture_desc');
    setEditOpen(true);
  };

  const handleSaveEdit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!detail) return;
    const title = editTitle.trim();
    if (!title) {
      notifyError(new Error('Album title is required.'));
      return;
    }
    setSavingEdit(true);
    try {
      const updated = await updateAlbum(detail.album.id, {
        title,
        description: editDesc.trim() || null,
        sort_order: editSort,
      });
      setDetail((prev) => (prev ? { ...prev, album: updated } : prev));
      setItems((prev) => prev.map((a) => (a.id === updated.id ? updated : a)));
      setEditOpen(false);
      notifySuccess(`Updated album "${updated.title}"`);
    } catch (err) {
      notifyError(err, 'Could not update album');
    } finally {
      setSavingEdit(false);
    }
  };

  const handleDeleteAlbum = async () => {
    if (!detail) return;
    const ok = await confirmAction(
      `Delete album "${detail.album.title}"?`,
      'Delete Album',
      'This deletes the album only. Photos and videos inside remain in your library.',
    );
    if (!ok) return;
    try {
      await deleteAlbum(detail.album.id);
      setItems((prev) => prev.filter((a) => a.id !== detail.album.id));
      notifySuccess(`Deleted album "${detail.album.title}"`);
      setSelectedAlbumId(null);
    } catch (err) {
      notifyError(err, 'Could not delete album');
    }
  };

  const handleOpenShareModal = async () => {
    if (!detail) return;
    setShareOpen(true);
    setShareLoading(true);
    try {
      const users = await listShareableUsers();
      const filtered = users.filter((u) => u.id !== detail.album.owner_user_id);
      setShareableUsers(filtered);
      const nextMap: Record<string, AlbumSharePermission | 'none'> = {};
      for (const u of filtered) {
        const existing = detail.shares.find((s) => s.user_id === u.id);
        nextMap[u.id] = existing ? existing.permission : 'none';
      }
      setShareMap(nextMap);
    } catch (err) {
      notifyError(err, 'Could not load shareable users');
    } finally {
      setShareLoading(false);
    }
  };

  const handleSaveShares = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!detail) return;
    setSavingShares(true);
    const payload: AlbumShareInput[] = Object.entries(shareMap)
      .filter((entry): entry is [string, AlbumSharePermission] => entry[1] !== 'none')
      .map(([user_id, permission]) => ({ user_id, permission }));

    try {
      const nextShares = await replaceAlbumShares(detail.album.id, payload);
      setDetail((prev) => (prev ? { ...prev, shares: nextShares } : prev));
      setShareOpen(false);
      notifySuccess('Updated album sharing');
    } catch (err) {
      notifyError(err, 'Could not update album sharing');
    } finally {
      setSavingShares(false);
    }
  };

  const handleRemoveAssetsFromAlbum = async (assetIds: string[], clearSelection: () => void) => {
    if (!detail || assetIds.length === 0) return;
    try {
      const res = await removeAlbumAssets(detail.album.id, assetIds);
      const removedSet = new Set(assetIds);
      setDetail((prev) =>
        prev
          ? {
              ...prev,
              assets: prev.assets.filter((a) => !removedSet.has(a.id)),
              album: {
                ...prev.album,
                asset_count: Math.max(0, prev.album.asset_count - res.removed_count),
              },
            }
          : prev,
      );
      clearSelection();
      notifySuccess(`Removed ${res.removed_count} item${res.removed_count === 1 ? '' : 's'} from album`);
    } catch (err) {
      notifyError(err, 'Could not remove items from album');
    }
  };

  const handleDetailAssetUpdated = useCallback((assetId: string, patch: Partial<AssetSummary>) => {
    setDetail((prev) =>
      prev
        ? {
            ...prev,
            assets: prev.assets.map((a) => (a.id === assetId ? { ...a, ...patch } : a)),
          }
        : prev,
    );
  }, []);

  const handleDetailAssetsTrashed = useCallback((assetIds: string[]) => {
    const removed = new Set(assetIds);
    setDetail((prev) => {
      if (!prev) return prev;
      const nextAssets = prev.assets.filter((a) => !removed.has(a.id));
      const delta = prev.assets.length - nextAssets.length;
      return {
        ...prev,
        album: {
          ...prev.album,
          asset_count: Math.max(0, prev.album.asset_count - delta),
        },
        assets: nextAssets,
      };
    });
  }, []);

  // Render Album Detail view
  if (selectedAlbumId) {
    const canManage = detail?.permission === 'owner' || detail?.permission === 'admin';
    const canContribute =
      canManage || detail?.permission === 'contribute';

    return (
      <div>
        <div style={{ marginBottom: 14 }}>
          <button
            type="button"
            className="button button-quiet"
            onClick={() => setSelectedAlbumId(null)}
          >
            <ArrowLeft size={14} />
            <span>Back to Albums</span>
          </button>
        </div>

        {detailLoading && !detail ? (
          <div className="view-loading" role="status">
            Loading album…
          </div>
        ) : detailError && !detail ? (
          <div className="error-banner error-banner-dismissible" role="alert">
            <span>{detailError}</span>
            <button
              type="button"
              className="button"
              onClick={() => void loadDetail(selectedAlbumId)}
            >
              Retry
            </button>
          </div>
        ) : detail ? (
          <>
            <ViewHeader
              title={detail.album.title}
              subtitle={
                [
                  plural(detail.album.asset_count, 'item'),
                  detail.album.description,
                  detail.shares.length > 0
                    ? `Shared with ${plural(detail.shares.length, 'person', 'people')}`
                    : null,
                ]
                  .filter(Boolean)
                  .join(' · ')
              }
              actions={
                canManage ? (
                  <>
                    <button type="button" className="button" onClick={() => void handleOpenShareModal()}>
                      <Share2 size={14} />
                      <span>Share</span>
                    </button>
                    <button type="button" className="button" onClick={openEditModal}>
                      <Pencil size={14} />
                      <span>Edit Album</span>
                    </button>
                    <button
                      type="button"
                      className="button button-danger-quiet"
                      onClick={() => void handleDeleteAlbum()}
                    >
                      <Trash2 size={14} />
                      <span>Delete Album</span>
                    </button>
                  </>
                ) : (
                  <Badge>{detail.permission}</Badge>
                )
              }
            />

            {detail.assets.length > 0 ? (
              <JustifiedTimeline
                assets={detail.assets}
                onAssetUpdated={handleDetailAssetUpdated}
                onAssetsTrashed={handleDetailAssetsTrashed}
                extraBulkActions={
                  canContribute
                    ? (selectedIds, clearSelection) => (
                        <button
                          type="button"
                          className="button"
                          onClick={() => void handleRemoveAssetsFromAlbum(selectedIds, clearSelection)}
                        >
                          Remove from Album
                        </button>
                      )
                    : undefined
                }
              />
            ) : (
              <EmptyState
                icon={<FolderHeart size={28} strokeWidth={1.3} />}
                title="This album is empty"
                description="Select photos or videos in Timeline and choose Add to Album to populate this collection."
              />
            )}
          </>
        ) : null}

        {editOpen && (
          <div className="modal-backdrop" onClick={() => setEditOpen(false)}>
            <form
              className="panel upload-modal stack"
              role="dialog"
              aria-modal="true"
              aria-label="Edit album"
              onClick={(e) => e.stopPropagation()}
              onSubmit={(e) => void handleSaveEdit(e)}
            >
              <div className="row" style={{ justifyContent: 'space-between' }}>
                <strong style={{ fontSize: 15 }}>Edit Album</strong>
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Close edit album"
                  onClick={() => setEditOpen(false)}
                >
                  <X size={16} />
                </button>
              </div>
              <label className="settings-field">
                <span>Title</span>
                <input
                  className="text-input"
                  type="text"
                  aria-label="Album title"
                  value={editTitle}
                  onChange={(e) => setEditTitle(e.target.value)}
                />
              </label>
              <label className="settings-field">
                <span>Description</span>
                <textarea
                  className="text-input"
                  rows={2}
                  aria-label="Album description"
                  value={editDesc}
                  onChange={(e) => setEditDesc(e.target.value)}
                />
              </label>
              <div className="settings-field">
                <span>Sort Order</span>
                <Dropdown
                  label="Album sort order"
                  value={editSort}
                  options={SORT_ORDER_OPTIONS}
                  onChange={setEditSort}
                />
              </div>
              <div className="confirm-actions">
                <button type="button" className="button" onClick={() => setEditOpen(false)}>
                  Cancel
                </button>
                <button type="submit" className="button button-primary" disabled={savingEdit}>
                  {savingEdit ? 'Saving…' : 'Save Changes'}
                </button>
              </div>
            </form>
          </div>
        )}

        {shareOpen && (
          <div className="modal-backdrop" onClick={() => setShareOpen(false)}>
            <form
              className="panel share-modal stack"
              role="dialog"
              aria-modal="true"
              aria-label="Share album"
              onClick={(e) => e.stopPropagation()}
              onSubmit={(e) => void handleSaveShares(e)}
            >
              <div className="row" style={{ justifyContent: 'space-between' }}>
                <strong style={{ fontSize: 15 }}>Share “{detail?.album.title}”</strong>
                <button
                  type="button"
                  className="icon-button"
                  aria-label="Close share dialog"
                  onClick={() => setShareOpen(false)}
                >
                  <X size={16} />
                </button>
              </div>
              <div className="view-subtitle" style={{ marginTop: 0 }}>
                Shared users only see items inside folders they have permission to access.
              </div>

              {shareLoading ? (
                <div className="view-loading" style={{ padding: '20px 0' }} role="status">
                  Loading users…
                </div>
              ) : shareableUsers.length === 0 ? (
                <div className="view-subtitle">No other users available to share with.</div>
              ) : (
                <div className="stack" style={{ gap: 8 }}>
                  {shareableUsers.map((u) => {
                    const perm = shareMap[u.id] ?? 'none';
                    const isShared = perm !== 'none';
                    return (
                      <div
                        key={u.id}
                        className="row"
                        style={{
                          justifyContent: 'space-between',
                          padding: '8px 10px',
                          border: '1px solid var(--border)',
                          borderRadius: 6,
                          background: '#1e1e1e',
                        }}
                      >
                        <label className="row" style={{ cursor: 'pointer', gap: 8 }}>
                          <input
                            type="checkbox"
                            checked={isShared}
                            aria-label={`Share with ${u.display_name}`}
                            onChange={(e) =>
                              setShareMap((prev) => ({
                                ...prev,
                                [u.id]: e.target.checked ? 'view' : 'none',
                              }))
                            }
                          />
                          <span>{u.display_name}</span>
                        </label>
                        {isShared && (
                          <div style={{ minWidth: 145 }}>
                            <Dropdown
                              label={`Permission for ${u.display_name}`}
                              value={perm}
                              options={SHARE_PERM_OPTIONS}
                              onChange={(next) =>
                                setShareMap((prev) => ({ ...prev, [u.id]: next }))
                              }
                            />
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}

              <div className="confirm-actions">
                <button type="button" className="button" onClick={() => setShareOpen(false)}>
                  Cancel
                </button>
                <button type="submit" className="button button-primary" disabled={savingShares}>
                  {savingShares ? 'Saving…' : 'Save Shares'}
                </button>
              </div>
            </form>
          </div>
        )}
      </div>
    );
  }

  const query = searchQuery.trim().toLowerCase();
  const visible = query
    ? items.filter(
        (a) =>
          a.title.toLowerCase().includes(query) ||
          a.description?.toLowerCase().includes(query),
      )
    : items;

  return (
    <div>
      <ViewHeader
        title="Albums"
        subtitle={`${plural(items.length, 'album')} owned or shared with you`}
        actions={
          canCreate ? (
            <button
              type="button"
              className="button button-primary"
              onClick={() => setCreateOpen(true)}
            >
              <Plus size={14} />
              <span>New Album</span>
            </button>
          ) : undefined
        }
      />

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button type="button" className="button" onClick={() => void loadList()}>
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading albums…
        </div>
      ) : visible.length > 0 ? (
        <div className="album-grid">
          {visible.map((album) => (
            <button
              key={album.id}
              type="button"
              className="album-card"
              onClick={() => setSelectedAlbumId(album.id)}
              onContextMenu={(e) => {
                const canDelete = user?.role === 'admin' || album.owner_user_id === user?.id;
                openContextMenu(e, [
                  {
                    id: 'alb-open',
                    label: 'Open Album',
                    icon: <FolderHeart size={14} />,
                    onSelect: () => setSelectedAlbumId(album.id),
                  },
                  ...(canDelete
                    ? [
                        { type: 'separator' as const, id: 'alb-sep-del' },
                        {
                          id: 'alb-delete',
                          label: 'Delete Album',
                          icon: <Trash2 size={14} />,
                          danger: true,
                          onSelect: async () => {
                            const ok = await confirmAction(
                              `Delete album "${album.title}"?`,
                              'Delete Album',
                              'This deletes the album only. Photos and videos inside remain in your library.',
                            );
                            if (!ok) return;
                            try {
                              await deleteAlbum(album.id);
                              setItems((prev) => prev.filter((a) => a.id !== album.id));
                              notifySuccess(`Deleted album "${album.title}"`);
                            } catch (err) {
                              notifyError(err, 'Could not delete album');
                            }
                          },
                        },
                      ]
                    : []),
                ]);
              }}
            >
              <div className="album-cover">
                {album.cover_asset_id ? (
                  <img
                    src={mediaThumbUrl(album.cover_asset_id)}
                    alt={album.title}
                    loading="lazy"
                    decoding="async"
                  />
                ) : (
                  <FolderHeart size={32} strokeWidth={1.2} />
                )}
              </div>
              <div className="card-body stack" style={{ width: '100%', gap: 6 }}>
                <div className="row" style={{ justifyContent: 'space-between' }}>
                  <strong style={{ fontSize: 13 }}>{album.title}</strong>
                  <Badge>{plural(album.asset_count, 'item')}</Badge>
                </div>
                {album.description && (
                  <div className="view-subtitle" style={{ marginTop: 0 }}>
                    {album.description}
                  </div>
                )}
                <div className="view-subtitle" style={{ marginTop: 0 }}>
                  Updated {formatRelativeTime(album.updated_at_ms)}
                </div>
              </div>
            </button>
          ))}
        </div>
      ) : (
        <EmptyState
          icon={<FolderHeart size={28} strokeWidth={1.3} />}
          title="No albums yet"
          description="Create personal or shared family albums to organize your favorite moments."
          actions={
            canCreate ? (
              <button
                type="button"
                className="button button-primary"
                onClick={() => setCreateOpen(true)}
              >
                <Plus size={14} />
                <span>New Album</span>
              </button>
            ) : undefined
          }
        />
      )}

      {createOpen && (
        <div className="modal-backdrop" onClick={() => setCreateOpen(false)}>
          <form
            className="panel upload-modal stack"
            role="dialog"
            aria-modal="true"
            aria-label="New album"
            onClick={(e) => e.stopPropagation()}
            onSubmit={(e) => void handleCreateAlbum(e)}
          >
            <div className="row" style={{ justifyContent: 'space-between' }}>
              <strong style={{ fontSize: 15 }}>New Album</strong>
              <button
                type="button"
                className="icon-button"
                aria-label="Close new album modal"
                onClick={() => setCreateOpen(false)}
              >
                <X size={16} />
              </button>
            </div>
            <label className="settings-field">
              <span>Title</span>
              <input
                className="text-input"
                type="text"
                placeholder="e.g. Summer Vacation"
                aria-label="New album title"
                value={newTitle}
                onChange={(e) => setNewTitle(e.target.value)}
              />
            </label>
            <label className="settings-field">
              <span>Description</span>
              <textarea
                className="text-input"
                rows={2}
                placeholder="Optional description"
                aria-label="New album description"
                value={newDesc}
                onChange={(e) => setNewDesc(e.target.value)}
              />
            </label>
            <div className="confirm-actions">
              <button type="button" className="button" onClick={() => setCreateOpen(false)}>
                Cancel
              </button>
              <button type="submit" className="button button-primary" disabled={creating}>
                {creating ? 'Creating…' : 'Create Album'}
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
};
