import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ChevronLeft,
  ChevronRight,
  Download,
  Info,
  Play,
  Star,
  Trash2,
  X,
  ZoomIn,
} from 'lucide-react';
import {
  getAssetDetail,
  mediaLiveVideoUrl,
  mediaOriginalUrl,
  mediaPreviewUrl,
  mediaThumbUrl,
  setAssetFavorite,
  trashAssets,
} from '../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { formatDateTime } from '../lib/format';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AssetDetail } from '../types/api';
import { AssetInspector } from './AssetInspector';
import { openContextMenu } from './ContextMenu';
import { VideoPlayer } from './VideoPlayer';

function touchDistance(a: React.Touch, b: React.Touch): number {
  const dx = a.clientX - b.clientX;
  const dy = a.clientY - b.clientY;
  return Math.hypot(dx, dy);
}

export const Lightbox: React.FC = () => {
  const lightboxAssetId = useAppStore((s) => s.lightboxAssetId);
  const lightboxAssetIds = useAppStore((s) => s.lightboxAssetIds);
  const lightboxInitialTimestampSec = useAppStore((s) => s.lightboxInitialTimestampSec);
  const closeLightbox = useAppStore((s) => s.closeLightbox);
  const setLightboxAsset = useAppStore((s) => s.setLightboxAsset);
  const removeLightboxAsset = useAppStore((s) => s.removeLightboxAsset);
  const bumpLibraryRevision = useAppStore((s) => s.bumpLibraryRevision);
  const user = useAppStore((s) => s.user);

  const canMutate = user?.role === 'admin' || user?.role === 'contributor';

  const [detail, setDetail] = useState<AssetDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [infoOpen, setInfoOpen] = useState(false);
  const [rawDemosaic, setRawDemosaic] = useState(false);
  const [livePlaying, setLivePlaying] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const [hiResLoadedUrl, setHiResLoadedUrl] = useState<string | null>(null);
  const [hiResLoading, setHiResLoading] = useState(false);

  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);

  // Touch & pointer gesture tracking
  const gestureRef = useRef<{
    startX: number;
    startY: number;
    startPanX: number;
    startPanY: number;
    pinchStartDist: number | null;
    pinchStartZoom: number;
    lastTapAt: number;
    longPressTimer: number | null;
    dragging: boolean;
    didDrag: boolean;
  }>({
    startX: 0,
    startY: 0,
    startPanX: 0,
    startPanY: 0,
    pinchStartDist: null,
    pinchStartZoom: 1,
    lastTapAt: 0,
    longPressTimer: null,
    dragging: false,
    didDrag: false,
  });

  const currentIndex = useMemo(() => {
    if (!lightboxAssetId) return -1;
    return lightboxAssetIds.indexOf(lightboxAssetId);
  }, [lightboxAssetId, lightboxAssetIds]);

  const hasPrev = currentIndex > 0;
  const hasNext = currentIndex >= 0 && currentIndex < lightboxAssetIds.length - 1;

  const goPrev = useCallback(() => {
    if (!hasPrev) return;
    setLightboxAsset(lightboxAssetIds[currentIndex - 1]);
  }, [currentIndex, hasPrev, lightboxAssetIds, setLightboxAsset]);

  const goNext = useCallback(() => {
    if (!hasNext) return;
    setLightboxAsset(lightboxAssetIds[currentIndex + 1]);
  }, [currentIndex, hasNext, lightboxAssetIds, setLightboxAsset]);

  useEffect(() => {
    if (!lightboxAssetId) {
      setDetail(null);
      setInfoOpen(false);
      return;
    }

    let active = true;
    setLoading(true);
    setError(null);
    setRawDemosaic(false);
    setLivePlaying(false);
    setZoom(1);
    setPan({ x: 0, y: 0 });
    setHiResLoadedUrl(null);
    setHiResLoading(false);

    void getAssetDetail(lightboxAssetId)
      .then((res) => {
        if (!active) return;
        setDetail(res);
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
  }, [lightboxAssetId]);

  // Preload adjacent prev/next photo previews in the background for instantaneous arrow navigation.
  useEffect(() => {
    if (!lightboxAssetId || currentIndex < 0) return;
    const neighbors = [
      lightboxAssetIds[currentIndex + 1],
      lightboxAssetIds[currentIndex - 1],
    ].filter((id): id is string => Boolean(id));
    for (const id of neighbors) {
      const img = new Image();
      img.decoding = 'async';
      img.src = mediaPreviewUrl(id);
    }
  }, [currentIndex, lightboxAssetId, lightboxAssetIds]);

  const handleToggleFavorite = useCallback(async () => {
    if (!canMutate || !lightboxAssetId || !detail) return;
    const nextFav = !detail.summary.is_favorite;
    setDetail((prev) =>
      prev ? { ...prev, summary: { ...prev.summary, is_favorite: nextFav } } : prev,
    );
    try {
      await setAssetFavorite(lightboxAssetId, nextFav);
      bumpLibraryRevision();
    } catch (err) {
      setDetail((prev) =>
        prev ? { ...prev, summary: { ...prev.summary, is_favorite: !nextFav } } : prev,
      );
      notifyError(err, 'Could not update favorite');
    }
  }, [bumpLibraryRevision, canMutate, detail, lightboxAssetId]);

  const handleMoveToTrash = useCallback(async () => {
    if (!canMutate || !lightboxAssetId) return;
    const trashedId = lightboxAssetId;
    try {
      await trashAssets([trashedId]);
      bumpLibraryRevision();
      notifySuccess('Moved item to Trash');
      removeLightboxAsset(trashedId);
    } catch (err) {
      notifyError(err, 'Could not move item to Trash');
    }
  }, [bumpLibraryRevision, canMutate, lightboxAssetId, removeLightboxAsset]);

  const togglePixelPeepZoom = useCallback(() => {
    setZoom((prev) => {
      if (prev > 1.05) {
        setPan({ x: 0, y: 0 });
        return 1;
      }
      return 2.5;
    });
  }, []);

  // Escape layer + Lightbox keyboard shortcuts
  useEffect(() => {
    if (!lightboxAssetId) return;
    const layer = pushEscapeLayer();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        if (infoOpen) {
          setInfoOpen(false);
        } else {
          closeLightbox();
        }
        return;
      }

      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === 'INPUT' ||
          target.tagName === 'TEXTAREA' ||
          target.tagName === 'SELECT' ||
          target.isContentEditable)
      ) {
        return;
      }

      switch (event.key) {
        case 'ArrowLeft':
          event.preventDefault();
          goPrev();
          break;
        case 'ArrowRight':
          event.preventDefault();
          goNext();
          break;
        case 'f':
        case 'F':
          event.preventDefault();
          void handleToggleFavorite();
          break;
        case 'i':
        case 'I':
          event.preventDefault();
          setInfoOpen((prev) => !prev);
          break;
        case '+':
        case '=':
          event.preventDefault();
          setZoom((prev) => Math.min(5, Number((prev + 0.5).toFixed(2))));
          break;
        case '-':
          event.preventDefault();
          setZoom((prev) => {
            const next = Math.max(1, Number((prev - 0.5).toFixed(2)));
            if (next === 1) setPan({ x: 0, y: 0 });
            return next;
          });
          break;
        case '0':
          event.preventDefault();
          setZoom(1);
          setPan({ x: 0, y: 0 });
          break;
        case ' ': {
          const videoEl = stageRef.current?.querySelector('video');
          if (videoEl) {
            event.preventDefault();
            if (videoEl.paused) void videoEl.play().catch(() => {});
            else videoEl.pause();
          } else if (
            detail?.playback.live_video_url ||
            detail?.summary.kind === 'live_photo' ||
            detail?.summary.kind === 'motion_photo'
          ) {
            event.preventDefault();
            setLivePlaying((prev) => !prev);
          }
          break;
        }
      }
    };

    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      popEscapeLayer(layer);
    };
  }, [
    closeLightbox,
    detail,
    goNext,
    goPrev,
    handleToggleFavorite,
    infoOpen,
    lightboxAssetId,
  ]);

  const isVideo =
    detail?.summary.kind === 'video' ||
    detail?.playback.mode === 'direct' ||
    detail?.playback.mode === 'hls';

  const hasRaw = Boolean(
    detail?.playback.has_raw ||
      detail?.summary.kind === 'raw_jpg_stack' ||
      detail?.files.some((f) => f.file_role === 'raw'),
  );

  const hasLiveClip = Boolean(
    detail?.playback.live_video_url ||
      detail?.summary.kind === 'live_photo' ||
      detail?.summary.kind === 'motion_photo',
  );

  const primaryFile = detail?.files.find((f) => f.file_role === 'primary') ?? detail?.files[0];
  const primaryExt = primaryFile?.rel_path.split('.').pop()?.toLowerCase() ?? '';
  const browserNativeOriginal = ['jpg', 'jpeg', 'png', 'webp', 'avif', 'gif'].includes(primaryExt);

  const fullResUrl = useMemo(() => {
    if (!lightboxAssetId || isVideo) return null;
    if (rawDemosaic) return mediaOriginalUrl(lightboxAssetId, { mode: 'raw_demosaic' });
    if (browserNativeOriginal) return mediaOriginalUrl(lightboxAssetId);
    if (
      hasRaw ||
      primaryExt === 'heic' ||
      primaryExt === 'heif' ||
      primaryExt === 'tif' ||
      primaryExt === 'tiff' ||
      primaryExt === 'jxl'
    ) {
      return mediaOriginalUrl(lightboxAssetId, { mode: 'raw_demosaic' });
    }
    return mediaOriginalUrl(lightboxAssetId);
  }, [browserNativeOriginal, hasRaw, isVideo, lightboxAssetId, primaryExt, rawDemosaic]);

  // Progressive full-resolution pixel-peeping: automatically preload & decode full original when zoomed in.
  useEffect(() => {
    if (!lightboxAssetId || !fullResUrl || isVideo || rawDemosaic) return;
    if (zoom <= 1.05) return;
    if (hiResLoadedUrl === fullResUrl) return;

    let cancelled = false;
    setHiResLoading(true);
    const loader = new Image();
    loader.decoding = 'async';
    loader.src = fullResUrl;

    const finalize = () => {
      if (cancelled) return;
      setHiResLoadedUrl(fullResUrl);
      setHiResLoading(false);
    };
    const fail = () => {
      if (cancelled) return;
      setHiResLoading(false);
    };

    if (typeof loader.decode === 'function') {
      loader
        .decode()
        .then(finalize)
        .catch(() => {
          loader.onload = finalize;
          loader.onerror = fail;
        });
    } else {
      loader.onload = finalize;
      loader.onerror = fail;
    }

    return () => {
      cancelled = true;
    };
  }, [fullResUrl, hiResLoadedUrl, isVideo, lightboxAssetId, rawDemosaic, zoom]);

  if (!lightboxAssetId) return null;

  const titleText =
    detail?.overrides?.title ||
    detail?.summary.title ||
    primaryFile?.rel_path.split(/[\\/]/).pop() ||
    'Media';

  const previewUrl = detail?.playback.preview_url ?? mediaPreviewUrl(lightboxAssetId);
  const usingHiRes = Boolean(
    rawDemosaic || (fullResUrl && hiResLoadedUrl === fullResUrl && zoom > 1.05),
  );
  const stillUrl = rawDemosaic
    ? mediaOriginalUrl(lightboxAssetId, { mode: 'raw_demosaic' })
    : usingHiRes
      ? fullResUrl!
      : previewUrl;

  const liveVideoUrl = detail?.playback.live_video_url ?? mediaLiveVideoUrl(lightboxAssetId);

  const clearLongPress = () => {
    if (gestureRef.current.longPressTimer !== null) {
      window.clearTimeout(gestureRef.current.longPressTimer);
      gestureRef.current.longPressTimer = null;
    }
  };

  const handleTouchStart = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (e.touches.length === 2) {
      gestureRef.current.pinchStartDist = touchDistance(e.touches[0], e.touches[1]);
      gestureRef.current.pinchStartZoom = zoom;
      return;
    }
    if (e.touches.length === 1) {
      const t = e.touches[0];
      const now = Date.now();
      if (now - gestureRef.current.lastTapAt < 300) {
        gestureRef.current.lastTapAt = 0;
        togglePixelPeepZoom();
        return;
      }
      gestureRef.current.lastTapAt = now;
      gestureRef.current.startX = t.clientX;
      gestureRef.current.startY = t.clientY;
      gestureRef.current.startPanX = pan.x;
      gestureRef.current.startPanY = pan.y;
      gestureRef.current.pinchStartDist = null;
      if (hasLiveClip && !isVideo) {
        gestureRef.current.longPressTimer = window.setTimeout(() => {
          setLivePlaying(true);
        }, 420);
      }
    }
  };

  const handleTouchMove = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (e.touches.length === 2 && gestureRef.current.pinchStartDist) {
      const dist = touchDistance(e.touches[0], e.touches[1]);
      const scaleFactor = dist / gestureRef.current.pinchStartDist;
      const nextZoom = Math.min(4, Math.max(1, gestureRef.current.pinchStartZoom * scaleFactor));
      setZoom(nextZoom);
      if (nextZoom <= 1.02) setPan({ x: 0, y: 0 });
      return;
    }
    if (e.touches.length === 1 && zoom > 1) {
      const t = e.touches[0];
      const dx = t.clientX - gestureRef.current.startX;
      const dy = t.clientY - gestureRef.current.startY;
      setPan({
        x: gestureRef.current.startPanX + dx,
        y: gestureRef.current.startPanY + dy,
      });
    }
  };

  const handleTouchEnd = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (gestureRef.current.pinchStartDist !== null) {
      gestureRef.current.pinchStartDist = null;
      return;
    }
    if (zoom === 1 && e.changedTouches.length === 1) {
      const t = e.changedTouches[0];
      const dx = t.clientX - gestureRef.current.startX;
      const dy = t.clientY - gestureRef.current.startY;
      if (Math.abs(dx) > 55 && Math.abs(dx) > Math.abs(dy) * 1.2) {
        if (dx > 0) goPrev();
        else goNext();
      }
    }
  };

  const handleMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0 || isVideo) return;
    gestureRef.current.didDrag = false;
    gestureRef.current.startX = e.clientX;
    gestureRef.current.startY = e.clientY;
    gestureRef.current.startPanX = pan.x;
    gestureRef.current.startPanY = pan.y;
    if (zoom > 1.05) {
      gestureRef.current.dragging = true;
      setIsDragging(true);
    }
  };

  const handleMouseMove = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!gestureRef.current.dragging || zoom <= 1.05) return;
    const dx = e.clientX - gestureRef.current.startX;
    const dy = e.clientY - gestureRef.current.startY;
    if (Math.hypot(dx, dy) > 4) {
      gestureRef.current.didDrag = true;
    }
    setPan({
      x: gestureRef.current.startPanX + dx,
      y: gestureRef.current.startPanY + dy,
    });
  };

  const handleMouseUp = () => {
    gestureRef.current.dragging = false;
    setIsDragging(false);
  };

  const handleImageClick = (e: React.MouseEvent<HTMLImageElement>) => {
    if (isVideo) return;
    if (gestureRef.current.didDrag) {
      gestureRef.current.didDrag = false;
      return;
    }
    if (zoom > 1.05) {
      setZoom(1);
      setPan({ x: 0, y: 0 });
      return;
    }
    const targetZoom = 2.25;
    const rect = imgRef.current?.getBoundingClientRect();
    if (rect && rect.width > 0 && rect.height > 0) {
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const offsetX = e.clientX - cx;
      const offsetY = e.clientY - cy;
      setPan({
        x: Math.round(-offsetX * (targetZoom - 1) * 0.75),
        y: Math.round(-offsetY * (targetZoom - 1) * 0.75),
      });
    } else {
      setPan({ x: 0, y: 0 });
    }
    setZoom(targetZoom);
  };

  const handleWheel = (e: React.WheelEvent<HTMLDivElement>) => {
    if (isVideo) return;
    if (Math.abs(e.deltaY) < 1) return;
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
    setZoom((prev) => {
      const next = Math.min(5, Math.max(1, Number((prev * factor).toFixed(2))));
      if (next <= 1.02) {
        setPan({ x: 0, y: 0 });
        return 1;
      }
      return next;
    });
  };

  const handleMediaContextMenu = (e: React.MouseEvent) => {
    if (!lightboxAssetId) return;
    openContextMenu(e, [
      ...(!isVideo
        ? [
            {
              id: 'lb-zoom',
              label: zoom > 1.05 ? 'Reset Zoom (100%)' : 'Zoom In (225%)',
              icon: <ZoomIn size={14} />,
              onSelect: () => {
                if (zoom > 1.05) {
                  setZoom(1);
                  setPan({ x: 0, y: 0 });
                } else {
                  setZoom(2.25);
                  setPan({ x: 0, y: 0 });
                }
              },
            },
          ]
        : []),
      ...(canMutate && detail
        ? [
            {
              id: 'lb-fav',
              label: detail.summary.is_favorite ? 'Remove from Favorites' : 'Add to Favorites',
              icon: <Star size={14} />,
              shortcut: 'F',
              onSelect: () => handleToggleFavorite(),
            },
          ]
        : []),
      {
        id: 'lb-info',
        label: infoOpen ? 'Hide Details' : 'Inspect Details',
        icon: <Info size={14} />,
        shortcut: 'I',
        onSelect: () => setInfoOpen((prev) => !prev),
      },
      {
        id: 'lb-download',
        label: 'Download Original',
        icon: <Download size={14} />,
        onSelect: () => {
          window.open(mediaOriginalUrl(lightboxAssetId, { download: true }), '_blank', 'noopener');
        },
      },
      ...(canMutate
        ? [
            { type: 'separator' as const, id: 'lb-sep-del' },
            {
              id: 'lb-trash',
              label: 'Move to Trash',
              icon: <Trash2 size={14} />,
              danger: true,
              onSelect: () => handleMoveToTrash(),
            },
          ]
        : []),
    ]);
  };

  return (
    <div
      className="lightbox-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Media lightbox"
    >
      <header className="lightbox-toolbar" data-tauri-drag-region>
        <div className="row lightbox-toolbar-left" style={{ gap: 10, minWidth: 0 }}>
          <button
            type="button"
            className="icon-button lightbox-close-btn"
            aria-label="Close lightbox"
            title="Close (Esc)"
            onClick={closeLightbox}
          >
            <X size={16} />
          </button>
          <div style={{ minWidth: 0 }}>
            <div
              style={{
                fontSize: 13,
                fontWeight: 600,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}
            >
              {titleText}
            </div>
            {detail && (
              <div
                className="view-subtitle"
                style={{
                  marginTop: 0,
                  fontSize: 11,
                  overflow: 'hidden',
                  textOverflow: 'ellipsis',
                  whiteSpace: 'nowrap',
                }}
              >
                {[
                  formatDateTime(detail.summary.capture_at_ms),
                  detail.photo_meta?.focal_length_mm != null
                    ? `${Math.round(detail.photo_meta.focal_length_mm)}mm`
                    : null,
                  detail.photo_meta?.aperture_f != null
                    ? `f/${Number(detail.photo_meta.aperture_f.toFixed(1))}`
                    : null,
                  detail.photo_meta?.shutter_speed_sec != null &&
                  detail.photo_meta.shutter_speed_sec > 0
                    ? detail.photo_meta.shutter_speed_sec >= 1
                      ? `${Number(detail.photo_meta.shutter_speed_sec.toFixed(1))}s`
                      : `1/${Math.max(1, Math.round(1 / detail.photo_meta.shutter_speed_sec))}s`
                    : null,
                  detail.photo_meta?.iso != null ? `ISO ${detail.photo_meta.iso}` : null,
                  lightboxAssetIds.length > 1 && currentIndex >= 0
                    ? `${currentIndex + 1} / ${lightboxAssetIds.length}`
                    : null,
                ]
                  .filter(Boolean)
                  .join(' · ')}
              </div>
            )}
          </div>
        </div>

        <div className="row" style={{ gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
          {hasRaw && !isVideo && (
            <div className="row" style={{ gap: 4 }} role="group" aria-label="RAW tier toggle">
              <button
                type="button"
                className={`lightbox-pill-btn${!rawDemosaic ? ' is-active' : ''}`}
                aria-pressed={!rawDemosaic}
                onClick={() => setRawDemosaic(false)}
              >
                Preview
              </button>
              <button
                type="button"
                className={`lightbox-pill-btn${rawDemosaic ? ' is-active' : ''}`}
                aria-pressed={rawDemosaic}
                onClick={() => setRawDemosaic(true)}
              >
                RAW 100%
              </button>
            </div>
          )}

          {hasLiveClip && !isVideo && (
            <button
              type="button"
              className={`lightbox-live-badge${livePlaying ? ' is-playing' : ''}`}
              aria-pressed={livePlaying}
              onClick={() => setLivePlaying((prev) => !prev)}
            >
              <Play size={11} fill="currentColor" />
              <span>{detail?.summary.kind === 'motion_photo' ? 'MOTION' : 'LIVE'}</span>
            </button>
          )}

          {!isVideo && (
            <button
              type="button"
              className={`lightbox-pill-btn${zoom > 1 ? ' is-active' : ''}`}
              aria-pressed={zoom > 1}
              title={usingHiRes ? 'Viewing full original resolution' : 'Click photo or button to zoom'}
              onClick={togglePixelPeepZoom}
            >
              <ZoomIn size={13} />
              <span>{zoom > 1 ? `${zoom.toFixed(1)}×` : '100%'}</span>
              {zoom > 1 && (
                <span className="lightbox-hd-tag">{hiResLoading ? 'HD…' : usingHiRes ? 'HD' : ''}</span>
              )}
            </button>
          )}

          {canMutate && detail && (
            <button
              type="button"
              className="icon-button"
              aria-label={detail.summary.is_favorite ? 'Remove favorite' : 'Favorite'}
              aria-pressed={detail.summary.is_favorite}
              style={detail.summary.is_favorite ? { color: '#f2cf6b' } : undefined}
              onClick={() => void handleToggleFavorite()}
            >
              <Star size={16} fill={detail.summary.is_favorite ? 'currentColor' : 'none'} />
            </button>
          )}

          <a
            className="icon-button"
            href={mediaOriginalUrl(lightboxAssetId, { download: true })}
            download
            aria-label="Download original"
            title="Download original"
          >
            <Download size={16} />
          </a>

          {canMutate && (
            <button
              type="button"
              className="icon-button"
              aria-label="Move to Trash"
              title="Move to Trash"
              onClick={() => void handleMoveToTrash()}
            >
              <Trash2 size={16} />
            </button>
          )}

          <button
            type="button"
            className={`lightbox-pill-btn${infoOpen ? ' is-active' : ''}`}
            aria-label="Toggle info"
            aria-pressed={infoOpen}
            onClick={() => setInfoOpen((prev) => !prev)}
          >
            <Info size={14} />
            <span>Info</span>
          </button>
        </div>
      </header>

      <div className="lightbox-body">
        <div
          ref={stageRef}
          className="lightbox-stage"
          onTouchStart={handleTouchStart}
          onTouchMove={handleTouchMove}
          onTouchEnd={handleTouchEnd}
          onMouseDown={handleMouseDown}
          onMouseMove={handleMouseMove}
          onMouseUp={handleMouseUp}
          onMouseLeave={handleMouseUp}
          onWheel={handleWheel}
          onContextMenu={handleMediaContextMenu}
        >
          {hasPrev && (
            <button
              type="button"
              className="lightbox-nav-btn is-prev"
              aria-label="Previous item"
              onClick={goPrev}
            >
              <ChevronLeft size={20} />
            </button>
          )}

          {loading ? (
            <div className="view-loading" role="status">
              Loading media…
            </div>
          ) : error ? (
            <div className="error-banner" role="alert">
              {error}
            </div>
          ) : isVideo && detail ? (
            <VideoPlayer
              playback={detail.playback}
              posterUrl={mediaThumbUrl(lightboxAssetId)}
              initialTimestampSec={lightboxInitialTimestampSec}
              className="lightbox-media"
            />
          ) : (
            <div
              style={{
                position: 'relative',
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'center',
                width: '100%',
                height: '100%',
              }}
            >
              <img
                ref={imgRef}
                className="lightbox-media"
                src={stillUrl}
                alt={titleText}
                draggable={false}
                onClick={handleImageClick}
                style={{
                  transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
                  cursor: zoom > 1.05 ? (isDragging ? 'grabbing' : 'grab') : 'zoom-in',
                  opacity: livePlaying ? 0 : 1,
                  transition: isDragging
                    ? 'opacity 180ms ease-out'
                    : 'transform 200ms cubic-bezier(0.22, 1, 0.36, 1), opacity 180ms ease-out',
                }}
              />

              {livePlaying && hasLiveClip && (
                <video
                  className="lightbox-media"
                  src={liveVideoUrl}
                  playsInline
                  muted
                  autoPlay
                  loop
                  style={{
                    position: 'absolute',
                    inset: 0,
                    margin: 'auto',
                  }}
                />
              )}
            </div>
          )}

          {hasNext && (
            <button
              type="button"
              className="lightbox-nav-btn is-next"
              aria-label="Next item"
              onClick={goNext}
            >
              <ChevronRight size={20} />
            </button>
          )}
        </div>

        {infoOpen && (
          <aside
            className="inspector-drawer lightbox-info-drawer"
            aria-label="Lightbox asset inspector"
          >
            <div className="inspector-heading">
              <span>Details</span>
              <button
                type="button"
                className="icon-button"
                aria-label="Close info drawer"
                onClick={() => setInfoOpen(false)}
              >
                <X size={15} />
              </button>
            </div>
            <AssetInspector
              assetId={lightboxAssetId}
              initialDetail={detail}
              onAssetUpdated={(updated) => setDetail(updated)}
            />
          </aside>
        )}
      </div>
    </div>
  );
};
