import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Check, RotateCcw, Trash2 } from 'lucide-react';
import {
  listTrash,
  mediaThumbUrl,
  purgeTrashAssets,
  restoreTrashAssets,
} from '../../lib/api';
import { formatDateTime, plural } from '../../lib/format';
import { confirmAction, notifyError, notifySuccess } from '../../store/feedback';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary } from '../../types/api';
import { EmptyState, Thumb, ViewHeader } from '../ui';

const MS_PER_DAY = 86_400_000;

function computeDaysLeft(deletedAtMs: number | null): number {
  if (!deletedAtMs || !Number.isFinite(deletedAtMs)) return 30;
  const daysElapsed = Math.max(0, Math.floor((Date.now() - deletedAtMs) / MS_PER_DAY));
  return Math.max(0, 30 - daysElapsed);
}

export const TrashView: React.FC = () => {
  const user = useAppStore((s) => s.user);
  const folders = useAppStore((s) => s.folders);
  const bumpLibraryRevision = useAppStore((s) => s.bumpLibraryRevision);
  const libraryRevision = useAppStore((s) => s.libraryRevision);

  const canRestore = user?.role === 'admin' || user?.role === 'contributor';
  const isAdmin = user?.role === 'admin';

  const [items, setItems] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listTrash({ limit: 100 });
      setItems(res.items);
      setTotalCount(res.total_count);
      setSelectedIds(new Set());
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, libraryRevision]);

  const folderById = useMemo(() => {
    const map = new Map<string, (typeof folders)[number]>();
    for (const f of folders) map.set(f.id, f);
    return map;
  }, [folders]);

  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleSelectAll = () => {
    if (selectedIds.size === items.length) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(items.map((a) => a.id)));
    }
  };

  const describePurgeScope = (targetAssets: AssetSummary[]): string => {
    const targetFolderIds = new Set(targetAssets.map((a) => a.folder_id));
    const diskDeleteFolders: string[] = [];
    const dbOnlyFolders: string[] = [];

    for (const fid of targetFolderIds) {
      const f = folderById.get(fid);
      if (!f) continue;
      if (f.allow_disk_delete) diskDeleteFolders.push(f.name);
      else dbOnlyFolders.push(f.name);
    }

    const parts: string[] = [];
    if (diskDeleteFolders.length > 0) {
      parts.push(
        `Allow Disk Deletion is ON for ${diskDeleteFolders.join(', ')}: original files will be moved to OS Trash / deleted from disk.`,
      );
    }
    if (dbOnlyFolders.length > 0) {
      parts.push(
        `Allow Disk Deletion is OFF (DB-only archive) for ${dbOnlyFolders.join(', ')}: database records will be purged while original files on disk remain untouched.`,
      );
    }
    if (parts.length === 0) {
      parts.push(
        'Folders with Allow Disk Deletion enabled delete original files; folders with it disabled only remove database records (DB-only archive).',
      );
    }
    return parts.join(' ');
  };

  const handleRestoreSelected = async () => {
    if (!canRestore || selectedIds.size === 0) return;
    setBusy(true);
    const ids = Array.from(selectedIds);
    try {
      const res = await restoreTrashAssets(ids);
      notifySuccess(
        `Restored ${res.restored_count} item${res.restored_count === 1 ? '' : 's'}`,
      );
      bumpLibraryRevision();
      await load();
    } catch (err) {
      notifyError(err, 'Could not restore items');
    } finally {
      setBusy(false);
    }
  };

  const handlePurgeSelected = async () => {
    if (!isAdmin || selectedIds.size === 0) return;
    const ids = Array.from(selectedIds);
    const targetAssets = items.filter((a) => selectedIds.has(a.id));
    const ok = await confirmAction(
      `Permanently purge ${plural(ids.length, 'item')}?`,
      'Permanently Purge',
      describePurgeScope(targetAssets),
    );
    if (!ok) return;

    setBusy(true);
    try {
      const report = await purgeTrashAssets({ asset_ids: ids });
      notifySuccess(
        `Purged ${report.purged_count} item(s) (${report.disk_deleted_files} disk file(s), ${report.db_only_assets} DB-only)`,
      );
      bumpLibraryRevision();
      await load();
    } catch (err) {
      notifyError(err, 'Could not purge selected items');
    } finally {
      setBusy(false);
    }
  };

  const handleEmptyTrash = async () => {
    if (!isAdmin || items.length === 0) return;
    const ok = await confirmAction(
      `Empty Trash (${plural(totalCount, 'item')})?`,
      'Empty Trash',
      describePurgeScope(items),
    );
    if (!ok) return;

    setBusy(true);
    try {
      const report = await purgeTrashAssets({ all: true });
      notifySuccess(
        `Emptied Trash: ${report.purged_count} purged (${report.disk_deleted_files} disk file(s), ${report.db_only_assets} DB-only)`,
      );
      bumpLibraryRevision();
      await load();
    } catch (err) {
      notifyError(err, 'Could not empty Trash');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <ViewHeader
        title="Trash"
        subtitle={`${plural(totalCount, 'trashed item')} in your allowed folders · Auto-purged after 30 days`}
        actions={
          items.length > 0 ? (
            <>
              {canRestore && (
                <>
                  <button
                    type="button"
                    className="button"
                    disabled={busy}
                    onClick={toggleSelectAll}
                  >
                    {selectedIds.size === items.length ? 'Deselect All' : 'Select All'}
                  </button>
                  <button
                    type="button"
                    className="button button-primary"
                    disabled={busy || selectedIds.size === 0}
                    onClick={() => void handleRestoreSelected()}
                  >
                    <RotateCcw size={14} />
                    <span>Restore ({selectedIds.size})</span>
                  </button>
                </>
              )}
              {isAdmin && (
                <>
                  <button
                    type="button"
                    className="button button-danger"
                    disabled={busy || selectedIds.size === 0}
                    onClick={() => void handlePurgeSelected()}
                  >
                    <Trash2 size={14} />
                    <span>Permanently Purge</span>
                  </button>
                  <button
                    type="button"
                    className="button button-danger-quiet"
                    disabled={busy}
                    onClick={() => void handleEmptyTrash()}
                  >
                    <span>Empty Trash</span>
                  </button>
                </>
              )}
            </>
          ) : undefined
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
          Loading trash…
        </div>
      ) : items.length > 0 ? (
        <div className="library-grid-shell">
          {items.map((asset) => {
            const isSelected = selectedIds.has(asset.id);
            const daysLeft = computeDaysLeft(asset.deleted_at_ms);
            const folder = folderById.get(asset.folder_id);

            return (
              <div
                key={asset.id}
                className="card"
                style={{
                  position: 'relative',
                  outline: isSelected ? '2px solid #e5e5e5' : undefined,
                  outlineOffset: isSelected ? -2 : undefined,
                }}
              >
                <div style={{ position: 'relative' }}>
                  <Thumb
                    src={mediaThumbUrl(asset.id)}
                    alt={asset.title ?? 'Trashed asset'}
                    height={160}
                    lazy
                  />

                  {canRestore && (
                    <button
                      type="button"
                      className={`tile-select-btn${isSelected ? ' is-selected' : ''}`}
                      style={{ opacity: 1 }}
                      aria-label={isSelected ? 'Deselect trashed item' : 'Select trashed item'}
                      aria-pressed={isSelected}
                      onClick={() => toggleSelect(asset.id)}
                    >
                      {isSelected && <Check size={12} strokeWidth={2.5} />}
                    </button>
                  )}

                  <span
                    className="trash-countdown-badge"
                    style={{ position: 'absolute', left: 8, bottom: 8 }}
                  >
                    {daysLeft} days left
                  </span>
                </div>

                <div className="card-body" style={{ padding: 10 }}>
                  <div className="row" style={{ justifyContent: 'space-between', gap: 6 }}>
                    <span
                      style={{
                        fontSize: 12,
                        fontWeight: 500,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                      }}
                    >
                      {asset.title || formatDateTime(asset.capture_at_ms)}
                    </span>
                  </div>
                  <div className="view-subtitle" style={{ marginTop: 4 }}>
                    {folder ? `${folder.name} · ` : ''}
                    {asset.deleted_at_ms ? `Deleted ${formatDateTime(asset.deleted_at_ms)}` : ''}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      ) : (
        <EmptyState
          icon={<Trash2 size={28} strokeWidth={1.3} />}
          title="Trash is empty"
          description="Soft-deleted items remain recoverable here for 30 days until an Administrator permanently purges them."
        />
      )}
    </div>
  );
};
