import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Plus, Star, X } from 'lucide-react';
import {
  addAssetTag,
  getAssetDetail,
  removeAssetTag,
  updateAssetOverrides,
} from '../lib/api';
import { formatBytes, formatDateTime } from '../lib/format';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AssetDetail, MediaTagView } from '../types/api';
import { HistogramChart } from './HistogramChart';
import { Badge } from './ui';

export interface AssetInspectorProps {
  assetId: string;
  initialDetail?: AssetDetail | null;
  onAssetUpdated?: (detail: AssetDetail) => void;
}

function formatShutterSpeed(sec: number | null): string | null {
  if (sec === null || !Number.isFinite(sec) || sec <= 0) return null;
  if (sec >= 1) return `${Number(sec.toFixed(2))}s`;
  const recip = Math.round(1 / sec);
  return `1/${recip}s`;
}

function formatDurationMs(ms: number | null | undefined): string | null {
  if (ms === null || ms === undefined || !Number.isFinite(ms) || ms < 0) return null;
  const totalSec = Math.round(ms / 1000);
  const hours = Math.floor(totalSec / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (hours > 0) {
    return `${hours}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
  }
  return `${mins}:${String(secs).padStart(2, '0')}`;
}

function toDatetimeLocalValue(ms: number | null | undefined): string {
  if (!ms || !Number.isFinite(ms) || ms <= 0) return '';
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return '';
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export const AssetInspector: React.FC<AssetInspectorProps> = ({
  assetId,
  initialDetail = null,
  onAssetUpdated,
}) => {
  const user = useAppStore((s) => s.user);
  const bumpLibraryRevision = useAppStore((s) => s.bumpLibraryRevision);
  const canEdit = user?.role === 'admin' || user?.role === 'contributor';

  const [detail, setDetail] = useState<AssetDetail | null>(
    initialDetail && initialDetail.summary.id === assetId ? initialDetail : null,
  );
  const [loading, setLoading] = useState<boolean>(
    !(initialDetail && initialDetail.summary.id === assetId),
  );
  const [error, setError] = useState<string | null>(null);

  // Override form state
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [captureInput, setCaptureInput] = useState('');
  const [rating, setRating] = useState<number>(0);
  const [geoCity, setGeoCity] = useState('');
  const [geoCountry, setGeoCountry] = useState('');
  const [savingOverrides, setSavingOverrides] = useState(false);

  // Tag state
  const [tagInput, setTagInput] = useState('');
  const [tagBusy, setTagBusy] = useState(false);

  const syncFormFromDetail = useCallback((next: AssetDetail) => {
    setTitle(next.overrides?.title ?? next.summary.title ?? '');
    setDescription(next.overrides?.description ?? '');
    setCaptureInput(toDatetimeLocalValue(next.overrides?.capture_at_ms ?? next.summary.capture_at_ms));
    setRating(next.overrides?.rating ?? next.summary.rating ?? 0);
    setGeoCity(
      next.overrides?.geo_city ??
        next.photo_meta?.geo_city ??
        next.video_meta?.geo_city ??
        next.summary.geo_city ??
        '',
    );
    setGeoCountry(
      next.overrides?.geo_country ??
        next.photo_meta?.geo_country ??
        next.video_meta?.geo_country ??
        next.summary.geo_country ??
        '',
    );
  }, []);

  useEffect(() => {
    if (initialDetail && initialDetail.summary.id === assetId) {
      setDetail(initialDetail);
      setLoading(false);
      setError(null);
      syncFormFromDetail(initialDetail);
      return;
    }

    let active = true;
    setLoading(true);
    setError(null);

    void getAssetDetail(assetId)
      .then((res) => {
        if (!active) return;
        setDetail(res);
        syncFormFromDetail(res);
        setLoading(false);
      })
      .catch((err) => {
        if (!active) return;
        setError(errorMessage(err));
        setLoading(false);
      });

    return () => {
      active = false;
    };
  }, [assetId, initialDetail, syncFormFromDetail]);

  const activeTags = useMemo<MediaTagView[]>(() => {
    if (!detail) return [];
    return detail.tags.filter((t) => t.state === 'active');
  }, [detail]);

  const handleSaveOverrides = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canEdit || !detail) return;

    let captureAtMs: number | null | undefined = undefined;
    if (captureInput.trim()) {
      const parsed = new Date(captureInput).getTime();
      if (Number.isNaN(parsed)) {
        notifyError(new Error('Invalid capture date/time.'));
        return;
      }
      captureAtMs = parsed;
    } else if (detail.overrides?.capture_at_ms !== null) {
      captureAtMs = null;
    }

    setSavingOverrides(true);
    try {
      const updated = await updateAssetOverrides(assetId, {
        title: title.trim() || null,
        description: description.trim() || null,
        capture_at_ms: captureAtMs,
        rating: rating > 0 ? rating : null,
        geo_city: geoCity.trim() || null,
        geo_country: geoCountry.trim() || null,
      });
      setDetail(updated);
      syncFormFromDetail(updated);
      onAssetUpdated?.(updated);
      bumpLibraryRevision();
      notifySuccess('Saved metadata overrides');
    } catch (err) {
      notifyError(err, 'Could not save overrides');
    } finally {
      setSavingOverrides(false);
    }
  };

  const handleAddTag = async (event: React.FormEvent) => {
    event.preventDefault();
    const cleaned = tagInput.trim();
    if (!canEdit || !cleaned || !detail) return;
    setTagBusy(true);
    try {
      const nextTags = await addAssetTag(assetId, cleaned);
      const updated = { ...detail, tags: nextTags };
      setDetail(updated);
      setTagInput('');
      onAssetUpdated?.(updated);
      notifySuccess(`Added tag "${cleaned}"`);
    } catch (err) {
      notifyError(err, 'Could not add tag');
    } finally {
      setTagBusy(false);
    }
  };

  const handleRemoveTag = async (tag: string) => {
    if (!canEdit || !detail) return;
    setTagBusy(true);
    try {
      const nextTags = await removeAssetTag(assetId, tag);
      const updated = { ...detail, tags: nextTags };
      setDetail(updated);
      onAssetUpdated?.(updated);
      notifySuccess(`Removed tag "${tag}"`);
    } catch (err) {
      notifyError(err, 'Could not remove tag');
    } finally {
      setTagBusy(false);
    }
  };

  if (loading) {
    return (
      <div className="asset-inspector">
        <div className="view-loading" role="status">
          Loading asset details…
        </div>
      </div>
    );
  }

  if (error || !detail) {
    return (
      <div className="asset-inspector">
        <div className="error-banner" role="alert">
          {error ?? 'Asset details unavailable.'}
        </div>
      </div>
    );
  }

  const { summary, photo_meta, video_meta, files, histogram } = detail;
  const primaryFile = files.find((f) => f.file_role === 'primary') ?? files[0];
  const cameraLabel =
    [photo_meta?.camera_make, photo_meta?.camera_model ?? summary.camera_model]
      .filter(Boolean)
      .join(' ') || '—';
  const shutterLabel = formatShutterSpeed(photo_meta?.shutter_speed_sec ?? null);
  const locationLabel =
    [
      detail.overrides?.geo_city ?? photo_meta?.geo_city ?? video_meta?.geo_city ?? summary.geo_city,
      detail.overrides?.geo_country ??
        photo_meta?.geo_country ??
        video_meta?.geo_country ??
        summary.geo_country,
    ]
      .filter(Boolean)
      .join(', ') || '—';

  return (
    <div className="asset-inspector">
      <div className="stack" style={{ gap: 6 }}>
        <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
          <strong style={{ fontSize: 14, overflowWrap: 'anywhere' }}>
            {summary.title || primaryFile?.rel_path.split(/[\\/]/).pop() || summary.id}
          </strong>
          <Badge>{summary.kind}</Badge>
        </div>
        <div className="view-subtitle" style={{ marginTop: 0 }}>
          {formatDateTime(summary.capture_at_ms)}
        </div>
      </div>

      {histogram && (
        <div>
          <div className="workspace-label" style={{ marginBottom: 6 }}>
            RGB &amp; Luma Histogram
          </div>
          <HistogramChart histogram={histogram} />
        </div>
      )}

      <div>
        <div className="workspace-label" style={{ marginBottom: 6 }}>
          EXIF &amp; Media Info
        </div>
        <div className="exif-grid">
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Camera</div>
            <div>{cameraLabel}</div>
          </div>
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Lens</div>
            <div>{photo_meta?.lens_model || '—'}</div>
          </div>
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Exposure</div>
            <div>
              {[
                photo_meta?.focal_length_mm ? `${Math.round(photo_meta.focal_length_mm)}mm` : null,
                photo_meta?.aperture_f ? `f/${photo_meta.aperture_f}` : null,
                shutterLabel,
                photo_meta?.iso ? `ISO ${photo_meta.iso}` : null,
              ]
                .filter(Boolean)
                .join(' · ') || '—'}
            </div>
          </div>
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Dimensions</div>
            <div>
              {summary.width && summary.height ? `${summary.width} × ${summary.height}` : '—'}
            </div>
          </div>
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Location</div>
            <div>{locationLabel}</div>
          </div>
          <div>
            <div className="view-subtitle" style={{ marginTop: 0 }}>Primary Size</div>
            <div>{primaryFile ? formatBytes(primaryFile.file_size) : '—'}</div>
          </div>
          {video_meta && (
            <>
              <div>
                <div className="view-subtitle" style={{ marginTop: 0 }}>Video Codec</div>
                <div className="row" style={{ gap: 6 }}>
                  <span>{video_meta.video_codec || '—'}</span>
                  {video_meta.is_hdr && <Badge tone="info">HDR</Badge>}
                </div>
              </div>
              <div>
                <div className="view-subtitle" style={{ marginTop: 0 }}>Duration / FPS</div>
                <div>
                  {[
                    formatDurationMs(video_meta.duration_ms),
                    video_meta.fps ? `${video_meta.fps.toFixed(2)} fps` : null,
                  ]
                    .filter(Boolean)
                    .join(' · ') || '—'}
                </div>
              </div>
            </>
          )}
        </div>
      </div>

      <form className="override-form" onSubmit={(e) => void handleSaveOverrides(e)}>
        <div className="row" style={{ justifyContent: 'space-between' }}>
          <div className="workspace-label">Metadata Overrides</div>
          {!canEdit && <Badge>Read-only</Badge>}
        </div>

        <label className="settings-field" style={{ marginBottom: 0 }}>
          <span className="view-subtitle" style={{ marginTop: 0 }}>Title</span>
          <input
            className="text-input"
            type="text"
            aria-label="Override title"
            placeholder="Optional title"
            value={title}
            disabled={!canEdit || savingOverrides}
            onChange={(e) => setTitle(e.target.value)}
          />
        </label>

        <label className="settings-field" style={{ marginBottom: 0 }}>
          <span className="view-subtitle" style={{ marginTop: 0 }}>Description</span>
          <textarea
            className="text-input"
            rows={2}
            aria-label="Override description"
            placeholder="Optional description"
            value={description}
            disabled={!canEdit || savingOverrides}
            onChange={(e) => setDescription(e.target.value)}
          />
        </label>

        <label className="settings-field" style={{ marginBottom: 0 }}>
          <span className="view-subtitle" style={{ marginTop: 0 }}>Capture Date</span>
          <input
            className="text-input"
            type="datetime-local"
            aria-label="Override capture date"
            value={captureInput}
            disabled={!canEdit || savingOverrides}
            onChange={(e) => setCaptureInput(e.target.value)}
          />
        </label>

        <div className="row" style={{ justifyContent: 'space-between' }}>
          <span className="view-subtitle" style={{ marginTop: 0 }}>Rating</span>
          <div className="star-rating-row" role="group" aria-label="Asset rating">
            {[1, 2, 3, 4, 5].map((starValue) => {
              const active = starValue <= rating;
              return (
                <button
                  key={starValue}
                  type="button"
                  className={active ? 'is-active' : undefined}
                  aria-label={`Rate ${starValue} star${starValue === 1 ? '' : 's'}`}
                  aria-pressed={active}
                  disabled={!canEdit || savingOverrides}
                  onClick={() => setRating((prev) => (prev === starValue ? 0 : starValue))}
                >
                  <Star size={15} fill={active ? 'currentColor' : 'none'} />
                </button>
              );
            })}
          </div>
        </div>

        <div className="row" style={{ gap: 8 }}>
          <input
            className="text-input"
            type="text"
            aria-label="Override city"
            placeholder="City"
            value={geoCity}
            disabled={!canEdit || savingOverrides}
            onChange={(e) => setGeoCity(e.target.value)}
          />
          <input
            className="text-input"
            type="text"
            aria-label="Override country"
            placeholder="Country"
            value={geoCountry}
            disabled={!canEdit || savingOverrides}
            onChange={(e) => setGeoCountry(e.target.value)}
          />
        </div>

        {canEdit && (
          <button
            type="submit"
            className="button button-primary"
            disabled={savingOverrides}
          >
            {savingOverrides ? 'Saving…' : 'Save Overrides'}
          </button>
        )}
      </form>

      <div className="stack" style={{ gap: 8 }}>
        <div className="workspace-label">Tags ({activeTags.length})</div>
        <div className="tag-chip-list">
          {activeTags.length === 0 ? (
            <span className="view-subtitle" style={{ marginTop: 0 }}>
              No tags assigned.
            </span>
          ) : (
            activeTags.map((t) => (
              <span key={t.tag} className="tag-chip">
                <span>{t.tag}</span>
                {t.source === 'ai' && (
                  <span style={{ color: 'var(--text-muted)', fontSize: 10 }}>AI</span>
                )}
                {canEdit && (
                  <button
                    type="button"
                    aria-label={`Remove tag ${t.tag}`}
                    disabled={tagBusy}
                    onClick={() => void handleRemoveTag(t.tag)}
                  >
                    <X size={12} />
                  </button>
                )}
              </span>
            ))
          )}
        </div>

        {canEdit && (
          <form className="row" onSubmit={(e) => void handleAddTag(e)}>
            <input
              className="text-input"
              type="text"
              placeholder="Add tag…"
              aria-label="Add tag"
              value={tagInput}
              disabled={tagBusy}
              onChange={(e) => setTagInput(e.target.value)}
            />
            <button
              type="submit"
              className="button"
              disabled={tagBusy || !tagInput.trim()}
            >
              <Plus size={14} />
              Add
            </button>
          </form>
        )}
      </div>

      <div className="stack" style={{ gap: 6 }}>
        <div className="workspace-label">Stacked Files ({files.length})</div>
        {files.map((file) => (
          <div
            key={file.id}
            className="card"
            style={{ padding: '8px 10px', background: '#222', borderColor: 'var(--border-subtle)' }}
          >
            <div className="row" style={{ justifyContent: 'space-between', gap: 6 }}>
              <Badge>{file.file_role}</Badge>
              <span className="property-value">{formatBytes(file.file_size)}</span>
            </div>
            <div className="view-subtitle" style={{ marginTop: 4, fontSize: 11 }}>
              {file.rel_path}
            </div>
            {file.is_missing && (
              <div style={{ marginTop: 4 }}>
                <Badge tone="warn">Missing on disk</Badge>
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  );
};
