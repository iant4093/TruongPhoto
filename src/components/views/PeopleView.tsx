import React, { useCallback, useEffect, useRef, useState } from 'react';
import {
  ArrowLeft,
  Check,
  Eye,
  EyeOff,
  GitMerge,
  Pencil,
  Pin,
  Sparkles,
  User,
  Users,
  X,
} from 'lucide-react';
import {
  faceThumbUrl,
  listPeople,
  listPersonAssets,
  mergePeople,
  triggerFaceClustering,
  updatePerson,
} from '../../lib/api';
import { plural } from '../../lib/format';
import { notifyError, notifySuccess } from '../../store/feedback';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AssetSummary, PersonSummary } from '../../types/api';
import { openContextMenu } from '../ContextMenu';
import { Dropdown } from '../Dropdown';
import { JustifiedTimeline } from '../JustifiedTimeline';
import { Badge, EmptyState, ViewHeader } from '../ui';

const PersonAvatar: React.FC<{
  person: PersonSummary;
  size?: number;
}> = ({ person, size = 84 }) => {
  const [failedFaceId, setFailedFaceId] = useState<string | null>(null);
  const faceId = person.cover_face_id;
  const showImage = Boolean(faceId && failedFaceId !== faceId);

  return (
    <div className="person-avatar" style={{ width: size, height: size }}>
      {showImage ? (
        <img
          src={faceThumbUrl(faceId!)}
          alt={person.name || 'Unnamed'}
          loading="lazy"
          decoding="async"
          onError={() => setFailedFaceId(faceId)}
        />
      ) : (
        <User size={Math.round(size * 0.36)} strokeWidth={1.3} aria-hidden="true" />
      )}
    </div>
  );
};

interface PersonDetailProps {
  person: PersonSummary;
  isAdmin: boolean;
  onBack: () => void;
  onPersonUpdated: (updated: PersonSummary) => void;
}

const PersonDetail: React.FC<PersonDetailProps> = ({
  person,
  isAdmin,
  onBack,
  onPersonUpdated,
}) => {
  const libraryRevision = useAppStore((s) => s.libraryRevision);
  const [assets, setAssets] = useState<AssetSummary[]>([]);
  const [totalCount, setTotalCount] = useState(0);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const seqRef = useRef(0);
  const loadedPersonIdRef = useRef<string | null>(null);

  const [nameInput, setNameInput] = useState(person.name ?? '');
  const [savingMeta, setSavingMeta] = useState(false);

  useEffect(() => {
    setNameInput(person.name ?? '');
  }, [person.id, person.name]);

  const loadAssets = useCallback(async () => {
    const seq = ++seqRef.current;
    if (loadedPersonIdRef.current !== person.id) {
      setLoading(true);
    }
    setError(null);
    setLoadMoreError(false);
    try {
      const assetsRes = await listPersonAssets(person.id, { limit: 120 });
      if (seq !== seqRef.current) return;
      loadedPersonIdRef.current = person.id;
      setAssets(assetsRes.items);
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
  }, [person.id]);

  useEffect(() => {
    void loadAssets();
  }, [loadAssets, libraryRevision]);

  const handleLoadMore = useCallback(async () => {
    if (!nextCursor || loadingMore) return;
    const seq = seqRef.current;
    setLoadingMore(true);
    setLoadMoreError(false);
    try {
      const res = await listPersonAssets(person.id, { cursor: nextCursor, limit: 120 });
      if (seq !== seqRef.current) return;
      setAssets((prev) => {
        const seen = new Set(prev.map((a) => a.id));
        return [...prev, ...res.items.filter((a) => !seen.has(a.id))];
      });
      setNextCursor(res.next_cursor);
      setTotalCount(res.total_count);
    } catch (err) {
      if (seq !== seqRef.current) return;
      setLoadMoreError(true);
      notifyError(err, 'Could not load more photos');
    } finally {
      if (seq === seqRef.current) {
        setLoadingMore(false);
      }
    }
  }, [loadingMore, nextCursor, person.id]);

  const handleSaveName = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!isAdmin) return;
    setSavingMeta(true);
    try {
      const updated = await updatePerson(person.id, { name: nameInput.trim() || null });
      onPersonUpdated(updated);
      notifySuccess('Updated person name');
    } catch (err) {
      notifyError(err, 'Could not rename person');
    } finally {
      setSavingMeta(false);
    }
  };

  const handleToggleFlag = async (field: 'is_pinned' | 'is_hidden', nextVal: boolean) => {
    if (!isAdmin) return;
    setSavingMeta(true);
    try {
      const updated = await updatePerson(person.id, { [field]: nextVal });
      onPersonUpdated(updated);
      notifySuccess(
        field === 'is_pinned'
          ? nextVal
            ? 'Pinned person'
            : 'Unpinned person'
          : nextVal
            ? 'Hidden person'
            : 'Unhidden person',
      );
    } catch (err) {
      notifyError(err, 'Could not update person');
    } finally {
      setSavingMeta(false);
    }
  };

  return (
    <div>
      <div style={{ marginBottom: 14 }}>
        <button type="button" className="button button-quiet" onClick={onBack}>
          <ArrowLeft size={14} />
          <span>Back to People</span>
        </button>
      </div>

      <div className="person-detail-header">
        <div className="row" style={{ gap: 14, flexWrap: 'wrap' }}>
          <PersonAvatar person={person} size={64} />
          <div>
            <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
              <h2 style={{ margin: 0, fontSize: 19 }}>{person.name || 'Unnamed'}</h2>
              {person.is_pinned && <Badge tone="info">Pinned</Badge>}
              {person.is_hidden && <Badge tone="warn">Hidden</Badge>}
            </div>
            <div className="view-subtitle" style={{ marginTop: 4 }}>
              {plural(person.face_count, 'face')} · {plural(totalCount, 'asset')}
            </div>
          </div>
        </div>

        {isAdmin && (
          <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
            <form className="row" style={{ gap: 6 }} onSubmit={(e) => void handleSaveName(e)}>
              <input
                aria-label="Person name"
                className="text-input"
                style={{ width: 170 }}
                type="text"
                placeholder="Enter name…"
                value={nameInput}
                onChange={(e) => setNameInput(e.target.value)}
              />
              <button type="submit" className="button button-primary" disabled={savingMeta}>
                Save
              </button>
            </form>

            <button
              type="button"
              className="button"
              disabled={savingMeta}
              onClick={() => void handleToggleFlag('is_pinned', !person.is_pinned)}
            >
              <Pin size={13} />
              <span>{person.is_pinned ? 'Unpin' : 'Pin'}</span>
            </button>

            <button
              type="button"
              className="button"
              disabled={savingMeta}
              onClick={() => void handleToggleFlag('is_hidden', !person.is_hidden)}
            >
              {person.is_hidden ? <Eye size={13} /> : <EyeOff size={13} />}
              <span>{person.is_hidden ? 'Unhide' : 'Hide'}</span>
            </button>
          </div>
        )}
      </div>

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button type="button" className="button" onClick={() => void loadAssets()}>
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading photos…
        </div>
      ) : assets.length > 0 ? (
        <JustifiedTimeline
          assets={assets}
          hasMore={nextCursor !== null && !loadMoreError}
          loadingMore={loadingMore}
          onLoadMore={handleLoadMore}
          onAssetUpdated={(assetId, patch) =>
            setAssets((prev) => prev.map((a) => (a.id === assetId ? { ...a, ...patch } : a)))
          }
          onAssetsTrashed={(trashedIds) => {
            const removed = new Set(trashedIds);
            setAssets((prev) => prev.filter((a) => !removed.has(a.id)));
            setTotalCount((prev) => Math.max(0, prev - trashedIds.length));
          }}
        />
      ) : (
        <EmptyState
          icon={<Users size={28} strokeWidth={1.3} />}
          title="No visible assets for this person"
          description="Assets containing this person appear here based on your folder access."
        />
      )}
    </div>
  );
};

export const PeopleView: React.FC = () => {
  const user = useAppStore((s) => s.user);
  const searchQuery = useAppStore((s) => s.searchQuery);
  const libraryRevision = useAppStore((s) => s.libraryRevision);
  const isAdmin = user?.role === 'admin';

  const [items, setItems] = useState<PersonSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showHidden, setShowHidden] = useState(false);

  const [selectedPersonId, setSelectedPersonId] = useState<string | null>(null);

  // Admin inline card rename state
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameText, setRenameText] = useState('');
  const [savingRename, setSavingRename] = useState(false);

  // Admin merge mode state
  const [mergeMode, setMergeMode] = useState(false);
  const [mergeSelectedIds, setMergeSelectedIds] = useState<string[]>([]);
  const [mergeTargetId, setMergeTargetId] = useState<string>('');
  const [merging, setMerging] = useState(false);

  // Admin clustering state
  const [clustering, setClustering] = useState(false);
  const listSeqRef = useRef(0);
  const hasLoadedOnceRef = useRef(false);

  const load = useCallback(
    async (includeHidden = showHidden) => {
      const seq = ++listSeqRef.current;
      if (!hasLoadedOnceRef.current) {
        setLoading(true);
      }
      setError(null);
      try {
        const res = await listPeople(isAdmin && includeHidden ? { include_hidden: true } : undefined);
        if (seq !== listSeqRef.current) return;
        hasLoadedOnceRef.current = true;
        setItems(res.items);
      } catch (err) {
        if (seq !== listSeqRef.current) return;
        setError(errorMessage(err));
      } finally {
        if (seq === listSeqRef.current) {
          setLoading(false);
        }
      }
    },
    [isAdmin, showHidden],
  );

  useEffect(() => {
    void load(showHidden);
  }, [load, showHidden, libraryRevision]);

  const selectedPerson = selectedPersonId
    ? items.find((p) => p.id === selectedPersonId) ?? null
    : null;

  const handlePersonUpdated = useCallback((updated: PersonSummary) => {
    setItems((prev) => prev.map((p) => (p.id === updated.id ? updated : p)));
  }, []);

  if (selectedPerson) {
    return (
      <PersonDetail
        person={selectedPerson}
        isAdmin={isAdmin}
        onBack={() => setSelectedPersonId(null)}
        onPersonUpdated={handlePersonUpdated}
      />
    );
  }

  const handleToggleMergeMode = () => {
    if (!isAdmin) return;
    setMergeMode((prev) => !prev);
    setMergeSelectedIds([]);
    setMergeTargetId('');
  };

  const toggleMergeSelect = (personId: string) => {
    const next = mergeSelectedIds.includes(personId)
      ? mergeSelectedIds.filter((id) => id !== personId)
      : [...mergeSelectedIds, personId];
    setMergeSelectedIds(next);
    if (next.length > 0 && !next.includes(mergeTargetId)) {
      setMergeTargetId(next[0]);
    } else if (next.length === 0) {
      setMergeTargetId('');
    }
  };

  const handleMergeSubmit = async () => {
    if (!isAdmin || mergeSelectedIds.length < 2) return;
    const targetId = mergeTargetId && mergeSelectedIds.includes(mergeTargetId)
      ? mergeTargetId
      : mergeSelectedIds[0];
    const sourceIds = mergeSelectedIds.filter((id) => id !== targetId);
    if (sourceIds.length === 0) return;

    setMerging(true);
    try {
      const res = await mergePeople({
        source_person_ids: sourceIds,
        target_person_id: targetId,
      });
      notifySuccess(`Merged ${plural(res.merged_faces, 'face')} into ${res.person.name || 'Unnamed'}`);
      setMergeMode(false);
      setMergeSelectedIds([]);
      setMergeTargetId('');
      await load(showHidden);
    } catch (err) {
      notifyError(err, 'Could not merge people');
    } finally {
      setMerging(false);
    }
  };

  const handleRunClustering = async () => {
    if (!isAdmin) return;
    setClustering(true);
    try {
      const res = await triggerFaceClustering();
      notifySuccess(
        `Clustering complete: ${plural(res.clusters_created, 'cluster')}, ${plural(res.faces_assigned, 'face')} assigned`,
      );
      await load(showHidden);
    } catch (err) {
      notifyError(err, 'Face clustering failed');
    } finally {
      setClustering(false);
    }
  };

  const handleInlineRenameSubmit = async (event: React.FormEvent, personId: string) => {
    event.preventDefault();
    event.stopPropagation();
    if (!isAdmin) return;
    setSavingRename(true);
    try {
      const updated = await updatePerson(personId, { name: renameText.trim() || null });
      handlePersonUpdated(updated);
      setRenamingId(null);
      notifySuccess('Renamed person');
    } catch (err) {
      notifyError(err, 'Could not rename person');
    } finally {
      setSavingRename(false);
    }
  };

  const handleCardToggleFlag = async (
    event: React.MouseEvent,
    person: PersonSummary,
    field: 'is_pinned' | 'is_hidden',
  ) => {
    event.stopPropagation();
    if (!isAdmin) return;
    try {
      const updated = await updatePerson(person.id, { [field]: !person[field] });
      handlePersonUpdated(updated);
      if (field === 'is_hidden' && !showHidden && updated.is_hidden) {
        setItems((prev) => prev.filter((p) => p.id !== updated.id));
      }
    } catch (err) {
      notifyError(err, 'Could not update person');
    }
  };

  const query = searchQuery.trim().toLowerCase();
  const visible = query
    ? items.filter((p) => (p.name ?? 'Unnamed').toLowerCase().includes(query))
    : items;

  return (
    <div>
      <ViewHeader
        title="People"
        subtitle={
          loading && items.length === 0
            ? '\u00A0'
            : `${plural(items.length, 'person', 'people')} visible in your allowed folders`
        }
        actions={
          isAdmin ? (
            <>
              <label className="row" style={{ fontSize: 12, cursor: 'pointer', gap: 6 }}>
                <input
                  type="checkbox"
                  aria-label="Show Hidden"
                  checked={showHidden}
                  onChange={(e) => setShowHidden(e.target.checked)}
                />
                <span>Show Hidden</span>
              </label>

              <button
                type="button"
                className={`button${mergeMode ? ' button-primary' : ''}`}
                onClick={handleToggleMergeMode}
              >
                <GitMerge size={14} />
                <span>{mergeMode ? 'Exit Merge' : 'Merge People'}</span>
              </button>

              <button
                type="button"
                className="button button-primary"
                disabled={clustering}
                onClick={() => void handleRunClustering()}
              >
                <Sparkles size={14} />
                <span>{clustering ? 'Clustering…' : 'Run Face Clustering'}</span>
              </button>
            </>
          ) : undefined
        }
      />

      {isAdmin && mergeMode && (
        <div className="person-merge-bar" role="region" aria-label="Merge people">
          <div className="row" style={{ flexWrap: 'wrap', gap: 10 }}>
            <span style={{ fontSize: 12, fontWeight: 500 }}>
              {mergeSelectedIds.length < 2
                ? `Select at least 2 people to merge (${mergeSelectedIds.length} selected)`
                : `${mergeSelectedIds.length} selected`}
            </span>

            {mergeSelectedIds.length >= 2 && (
              <div className="row" style={{ gap: 6, fontSize: 12 }}>
                <span className="workspace-label">Merge into:</span>
                <Dropdown
                  label="Target person"
                  value={mergeTargetId}
                  options={mergeSelectedIds.map((id) => {
                    const p = items.find((item) => item.id === id);
                    return [
                      id,
                      `${p?.name || 'Unnamed'} (${plural(p?.face_count ?? 0, 'face')})`,
                    ] as const;
                  })}
                  onChange={(val) => setMergeTargetId(val)}
                />
              </div>
            )}
          </div>

          <div className="row" style={{ gap: 8 }}>
            <button
              type="button"
              className="button button-primary"
              disabled={mergeSelectedIds.length < 2 || merging}
              onClick={() => void handleMergeSubmit()}
            >
              {merging ? 'Merging…' : 'Merge Selected'}
            </button>
            <button type="button" className="button" onClick={handleToggleMergeMode}>
              Cancel
            </button>
          </div>
        </div>
      )}

      {error && (
        <div className="error-banner error-banner-dismissible" role="alert">
          <span>{error}</span>
          <button type="button" className="button" onClick={() => void load(showHidden)}>
            Retry
          </button>
        </div>
      )}

      {loading ? (
        <div className="view-loading" role="status">
          Loading people…
        </div>
      ) : visible.length > 0 ? (
        <div className="people-grid">
          {visible.map((person) => {
            const isMergeSelected = mergeMode && mergeSelectedIds.includes(person.id);
            const isRenaming = isAdmin && renamingId === person.id;

            return (
              <div
                key={person.id}
                role="button"
                tabIndex={0}
                className={`person-card${isMergeSelected ? ' is-selected' : ''}`}
                aria-label={person.name || 'Unnamed'}
                aria-pressed={mergeMode ? isMergeSelected : undefined}
                onClick={() => {
                  if (mergeMode) {
                    toggleMergeSelect(person.id);
                  } else if (!isRenaming) {
                    setSelectedPersonId(person.id);
                  }
                }}
                onContextMenu={(e) =>
                  openContextMenu(e, [
                    {
                      id: 'person-open',
                      label: 'View Photos',
                      icon: <Eye size={14} />,
                      onSelect: () => setSelectedPersonId(person.id),
                    },
                    ...(isAdmin
                      ? [
                          {
                            id: 'person-rename',
                            label: 'Rename Person',
                            icon: <Pencil size={14} />,
                            onSelect: () => {
                              setRenamingId(person.id);
                              setRenameText(person.name || '');
                            },
                          },
                          {
                            id: 'person-pin',
                            label: person.is_pinned ? 'Unpin Person' : 'Pin to Top',
                            icon: <Pin size={14} />,
                            onSelect: async () => {
                              try {
                                const updated = await updatePerson(person.id, {
                                  is_pinned: !person.is_pinned,
                                });
                                handlePersonUpdated(updated);
                              } catch (err) {
                                notifyError(err, 'Could not update person');
                              }
                            },
                          },
                          {
                            id: 'person-hide',
                            label: person.is_hidden ? 'Unhide Person' : 'Hide Person',
                            icon: person.is_hidden ? <Eye size={14} /> : <EyeOff size={14} />,
                            onSelect: async () => {
                              try {
                                const updated = await updatePerson(person.id, {
                                  is_hidden: !person.is_hidden,
                                });
                                handlePersonUpdated(updated);
                                if (!showHidden && updated.is_hidden) {
                                  setItems((prev) => prev.filter((p) => p.id !== updated.id));
                                }
                              } catch (err) {
                                notifyError(err, 'Could not update person');
                              }
                            },
                          },
                        ]
                      : []),
                  ])
                }
                onKeyDown={(e) => {
                  if (e.target !== e.currentTarget) return;
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    if (mergeMode) toggleMergeSelect(person.id);
                    else if (!isRenaming) setSelectedPersonId(person.id);
                  }
                }}
              >
                <PersonAvatar person={person} />

                {isRenaming ? (
                  <form
                    className="stack"
                    style={{ width: '100%' }}
                    onClick={(e) => e.stopPropagation()}
                    onSubmit={(e) => void handleInlineRenameSubmit(e, person.id)}
                  >
                    <input
                      aria-label={`Rename ${person.name || 'Unnamed'}`}
                      className="text-input"
                      type="text"
                      value={renameText}
                      autoFocus
                      onChange={(e) => setRenameText(e.target.value)}
                    />
                    <div className="row" style={{ justifyContent: 'center', gap: 6 }}>
                      <button
                        type="submit"
                        className="button button-primary"
                        disabled={savingRename}
                        aria-label="Save name"
                      >
                        <Check size={12} />
                        <span>Save</span>
                      </button>
                      <button
                        type="button"
                        className="button"
                        aria-label="Cancel rename"
                        onClick={(e) => {
                          e.stopPropagation();
                          setRenamingId(null);
                        }}
                      >
                        <X size={12} />
                      </button>
                    </div>
                  </form>
                ) : (
                  <strong style={{ fontSize: 13 }}>{person.name || 'Unnamed'}</strong>
                )}

                <div className="view-subtitle" style={{ marginTop: 0 }}>
                  {plural(person.face_count, 'face')}
                </div>

                <div className="row" style={{ gap: 4, flexWrap: 'wrap', justifyContent: 'center' }}>
                  {person.is_pinned && <Badge tone="info">Pinned</Badge>}
                  {person.is_hidden && <Badge tone="warn">Hidden</Badge>}
                </div>

                {isAdmin && !mergeMode && !isRenaming && (
                  <div
                    className="row"
                    style={{ gap: 4, marginTop: 2, flexWrap: 'wrap', justifyContent: 'center' }}
                  >
                    <button
                      type="button"
                      className="button button-quiet"
                      style={{ minHeight: 26, padding: '2px 7px', fontSize: 11 }}
                      onClick={(e) => {
                        e.stopPropagation();
                        setRenamingId(person.id);
                        setRenameText(person.name ?? '');
                      }}
                    >
                      <Pencil size={11} />
                      <span>Rename</span>
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      style={{ minHeight: 26, padding: '2px 7px', fontSize: 11 }}
                      aria-label={person.is_pinned ? `Unpin ${person.name || 'Unnamed'}` : `Pin ${person.name || 'Unnamed'}`}
                      onClick={(e) => void handleCardToggleFlag(e, person, 'is_pinned')}
                    >
                      <Pin size={11} />
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      style={{ minHeight: 26, padding: '2px 7px', fontSize: 11 }}
                      aria-label={person.is_hidden ? `Unhide ${person.name || 'Unnamed'}` : `Hide ${person.name || 'Unnamed'}`}
                      onClick={(e) => void handleCardToggleFlag(e, person, 'is_hidden')}
                    >
                      {person.is_hidden ? <Eye size={11} /> : <EyeOff size={11} />}
                    </button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      ) : items.length > 0 && searchQuery.trim() ? (
        <EmptyState
          icon={<Users size={28} strokeWidth={1.3} />}
          title={`No people match "${searchQuery.trim()}"`}
          description="Try a different name or clear the search box to see everyone."
        />
      ) : (
        <EmptyState
          icon={<Users size={28} strokeWidth={1.3} />}
          title="No people clustered yet"
          description="Faces detected in your allowed folders are grouped into people automatically."
          actions={
            isAdmin ? (
              <button
                type="button"
                className="button button-primary"
                disabled={clustering}
                onClick={() => void handleRunClustering()}
              >
                <Sparkles size={14} />
                <span>{clustering ? 'Clustering…' : 'Run Face Clustering'}</span>
              </button>
            ) : undefined
          }
        />
      )}
    </div>
  );
};
