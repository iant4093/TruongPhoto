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
import { isTauri } from '../lib/tauri';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AssetDetail } from '../types/api';
import { AssetInspector } from './AssetInspector';
import { openContextMenu } from './ContextMenu';
import { VideoPlayer } from './VideoPlayer';
import { WindowsCaptionButtons, isWindowsPlatform } from './WindowControls';

const showWindowsCaption = isTauri() && isWindowsPlatform();

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
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [infoOpen, setInfoOpen] = useState(false);
  const [rawDemosaic, setRawDemosaic] = useState(false);
  const [livePlaying, setLivePlaying] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const [isWheeling, setIsWheeling] = useState(false);
  const [hiResLoadedUrl, setHiResLoadedUrl] = useState<string | null>(null);
  const [hiResLoading, setHiResLoading] = useState(false);
  const [navDir, setNavDir] = useState<'fade' | 'next' | 'prev'>('fade');

  const overlayRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const wheelTimerRef = useRef<number | null>(null);

  // Touch & pointer gesture tracking
  const gestureRef = useRef<{
    startX: number;
    startY: number;
    startPanX: number;
    startPanY: number;
    pinchStartDist: number | null;
    pinchStartZoom: number;
    wasPinching: boolean;
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
    wasPinching: false,
    lastTapAt: 0,
    longPressTimer: null,
    dragging: false,
    didDrag: false,
  });

  const clearLongPress = useCallback(() => {
    if (gestureRef.current.longPressTimer !== null) {
      window.clearTimeout(gestureRef.current.longPressTimer);
      gestureRef.current.longPressTimer = null;
    }
  }, []);

  useEffect(() => () => clearLongPress(), [clearLongPress]);

  const currentIndex = useMemo(() => {
    if (!lightboxAssetId) return -1;
    return lightboxAssetIds.indexOf(lightboxAssetId);
  }, [lightboxAssetId, lightboxAssetIds]);

  const hasPrev = currentIndex > 0;
  const hasNext = currentIndex >= 0 && currentIndex < lightboxAssetIds.length - 1;

  const goPrev = useCallback(() => {
    if (!hasPrev) return;
    setNavDir('prev');
    setLightboxAsset(lightboxAssetIds[currentIndex - 1]);
  }, [currentIndex, hasPrev, lightboxAssetIds, setLightboxAsset]);

  const goNext = useCallback(() => {
    if (!hasNext) return;
    setNavDir('next');
    setLightboxAsset(lightboxAssetIds[currentIndex + 1]);
  }, [currentIndex, hasNext, lightboxAssetIds, setLightboxAsset]);

  useEffect(() => {
    clearLongPress();
    if (!lightboxAssetId) {
      setDetail(null);
      setDetailLoading(false);
      setInfoOpen(false);
      setNavDir('fade');
      return;
    }

    let active = true;
    setDetailLoading(true);
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
      })
      .catch((err) => {
        if (!active) return;
        setError(errorMessage(err));
      })
      .finally(() => {
        if (active) setDetailLoading(false);
      });

    return () => {
      active = false;
      clearLongPress();
    };
  }, [clearLongPress, lightboxAssetId]);

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

  const activeDetail = detail?.summary.id === lightboxAssetId ? detail : null;

  const clampPan = useCallback((x: number, y: number, targetZoom: number) => {
    if (targetZoom <= 1.02) return { x: 0, y: 0 };
    const stage = stageRef.current;
    const img = imgRef.current;
    if (!stage || !img) return { x, y };
    const maxX = Math.max(0, (img.offsetWidth * targetZoom - stage.clientWidth) / 2 + 48);
    const maxY = Math.max(0, (img.offsetHeight * targetZoom - stage.clientHeight) / 2 + 48);
    return {
      x: Math.max(-maxX, Math.min(maxX, x)),
      y: Math.max(-maxY, Math.min(maxY, y)),
    };
  }, []);

  // Move focus into Lightbox overlay when opened so keyboard navigation stays scoped.
  useEffect(() => {
    if (lightboxAssetId) {
      overlayRef.current?.focus({ preventScroll: true });
    }
  }, [lightboxAssetId]);

  const handleToggleFavorite = useCallback(async () => {
    if (!canMutate || !lightboxAssetId || !activeDetail) return;
    const nextFav = !activeDetail.summary.is_favorite;
    setDetail((prev) =>
      prev && prev.summary.id === lightboxAssetId
        ? { ...prev, summary: { ...prev.summary, is_favorite: nextFav } }
        : prev,
    );
    try {
      await setAssetFavorite(lightboxAssetId, nextFav);
      bumpLibraryRevision();
    } catch (err) {
      setDetail((prev) =>
        prev && prev.summary.id === lightboxAssetId
          ? { ...prev, summary: { ...prev.summary, is_favorite: !nextFav } }
          : prev,
      );
      notifyError(err, 'Could not update favorite');
    }
  }, [activeDetail, bumpLibraryRevision, canMutate, lightboxAssetId]);

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
        case 'Home':
          if (lightboxAssetIds.length > 0 && currentIndex !== 0) {
            event.preventDefault();
            setNavDir('prev');
            setLightboxAsset(lightboxAssetIds[0]);
          }
          break;
        case 'End':
          if (lightboxAssetIds.length > 0 && currentIndex !== lightboxAssetIds.length - 1) {
            event.preventDefault();
            setNavDir('next');
            setLightboxAsset(lightboxAssetIds[lightboxAssetIds.length - 1]);
          }
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
            activeDetail?.playback.live_video_url ||
            activeDetail?.summary.kind === 'live_photo' ||
            activeDetail?.summary.kind === 'motion_photo'
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
    activeDetail,
    closeLightbox,
    goNext,
    goPrev,
    handleToggleFavorite,
    infoOpen,
    lightboxAssetId,
  ]);

  const isVideo =
    activeDetail?.summary.kind === 'video' ||
    activeDetail?.playback.mode === 'direct' ||
    activeDetail?.playback.mode === 'hls';

  const hasRaw = Boolean(
    activeDetail?.playback.has_raw ||
      activeDetail?.summary.kind === 'raw_jpg_stack' ||
      activeDetail?.files.some((f) => f.file_role === 'raw'),
  );

  const hasLiveClip = Boolean(
    activeDetail?.playback.live_video_url ||
      activeDetail?.summary.kind === 'live_photo' ||
      activeDetail?.summary.kind === 'motion_photo',
  );

  const primaryFile =
    activeDetail?.files.find((f) => f.file_role === 'primary') ?? activeDetail?.files[0];
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

    const finalize = () => {
      if (cancelled) return;
      setHiResLoadedUrl(fullResUrl);
      setHiResLoading(false);
    };
    const fail = () => {
      if (cancelled) return;
      setHiResLoading(false);
    };

    loader.onload = finalize;
    loader.onerror = fail;
    loader.src = fullResUrl;

    if (typeof loader.decode === 'function') {
      loader
        .decode()
        .then(finalize)
        .catch(() => {
          if (loader.complete && loader.naturalWidth === 0) {
            fail();
          }
        });
    }

    return () => {
      cancelled = true;
      loader.onload = null;
      loader.onerror = null;
    };
  }, [fullResUrl, hiResLoadedUrl, isVideo, lightboxAssetId, rawDemosaic, zoom]);

  // Native non-passive wheel zoom listener so preventDefault works cleanly without console warnings
  // and wheel ticks zoom smoothly without CSS transition stutter.
  useEffect(() => {
    const el = stageRef.current;
    if (!el || !lightboxAssetId) return;
    const onNativeWheel = (e: WheelEvent) => {
      if (isVideo) return;
      if (Math.abs(e.deltaY) < 1) return;
      e.preventDefault();
      setIsWheeling(true);
      if (wheelTimerRef.current !== null) {
        window.clearTimeout(wheelTimerRef.current);
      }
      wheelTimerRef.current = window.setTimeout(() => {
        setIsWheeling(false);
        wheelTimerRef.current = null;
      }, 140);

      const factor = e.deltaY < 0 ? 1.14 : 1 / 1.14;
      setZoom((prev) => {
        const next = Math.min(5, Math.max(1, Number((prev * factor).toFixed(2))));
        if (next <= 1.02) {
          setPan({ x: 0, y: 0 });
          return 1;
        }
        setPan((cur) => clampPan(cur.x, cur.y, next));
        return next;
      });
    };
    el.addEventListener('wheel', onNativeWheel, { passive: false });
    return () => {
      el.removeEventListener('wheel', onNativeWheel);
      if (wheelTimerRef.current !== null) {
        window.clearTimeout(wheelTimerRef.current);
        wheelTimerRef.current = null;
      }
    };
  }, [clampPan, isVideo, lightboxAssetId]);

  if (!lightboxAssetId) return null;

  const titleText =
    activeDetail?.overrides?.title ||
    activeDetail?.summary.title ||
    primaryFile?.rel_path.split(/[\\/]/).pop() ||
    'Media';

  const previewUrl = activeDetail?.playback.preview_url ?? mediaPreviewUrl(lightboxAssetId);
  const usingHiRes = Boolean(
    rawDemosaic || (fullResUrl && hiResLoadedUrl === fullResUrl && zoom > 1.05),
  );
  const stillUrl = rawDemosaic
    ? mediaOriginalUrl(lightboxAssetId, { mode: 'raw_demosaic' })
    : usingHiRes
      ? fullResUrl!
      : previewUrl;

  const liveVideoUrl = activeDetail?.playback.live_video_url ?? mediaLiveVideoUrl(lightboxAssetId);

  const handleTouchStart = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (e.touches.length === 2) {
      gestureRef.current.pinchStartDist = touchDistance(e.touches[0], e.touches[1]);
      gestureRef.current.pinchStartZoom = zoom;
      gestureRef.current.wasPinching = true;
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
      setPan(clampPan(gestureRef.current.startPanX + dx, gestureRef.current.startPanY + dy, zoom));
    }
  };

  const handleTouchEnd = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (gestureRef.current.pinchStartDist !== null || gestureRef.current.wasPinching) {
      gestureRef.current.pinchStartDist = null;
      if (e.touches.length === 0) {
        gestureRef.current.wasPinching = false;
      }
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

  const handleTouchCancel = () => {
    clearLongPress();
    gestureRef.current.pinchStartDist = null;
    gestureRef.current.wasPinching = false;
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
    setPan(clampPan(gestureRef.current.startPanX + dx, gestureRef.current.startPanY + dy, zoom));
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
      setPan(
        clampPan(
          Math.round(-offsetX * (targetZoom - 1) * 0.75),
          Math.round(-offsetY * (targetZoom - 1) * 0.75),
          targetZoom,
        ),
      );
    } else {
      setPan({ x: 0, y: 0 });
    }
    setZoom(targetZoom);
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
      ...(canMutate && activeDetail
        ? [
            {
              id: 'lb-fav',
              label: activeDetail.summary.is_favorite ? 'Remove from Favorites' : 'Add to Favorites',
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
      ref={overlayRef}
      tabIndex={-1}
      className="lightbox-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Media lightbox"
    >
      <header className="lightbox-toolbar" data-tauri-drag-region>
        <div
          className="row lightbox-toolbar-left"
          style={{ gap: 10, minWidth: 0, flex: '1 1 180px' }}
        >
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
            {activeDetail && (
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
                  formatDateTime(activeDetail.summary.capture_at_ms),
                  activeDetail.photo_meta?.focal_length_mm != null
                    ? `${Math.round(activeDetail.photo_meta.focal_length_mm)}mm`
                    : null,
                  activeDetail.photo_meta?.aperture_f != null
                    ? `f/${Number(activeDetail.photo_meta.aperture_f.toFixed(1))}`
                    : null,
                  activeDetail.photo_meta?.shutter_speed_sec != null &&
                  activeDetail.photo_meta.shutter_speed_sec > 0
                    ? activeDetail.photo_meta.shutter_speed_sec >= 1
                      ? `${Number(activeDetail.photo_meta.shutter_speed_sec.toFixed(1))}s`
                      : `1/${Math.max(1, Math.round(1 / activeDetail.photo_meta.shutter_speed_sec))}s`
                    : null,
                  activeDetail.photo_meta?.iso != null ? `ISO ${activeDetail.photo_meta.iso}` : null,
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
              <span>{activeDetail?.summary.kind === 'motion_photo' ? 'MOTION' : 'LIVE'}</span>
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

          {canMutate && activeDetail && (
            <button
              type="button"
              className="icon-button"
              aria-label={activeDetail.summary.is_favorite ? 'Remove favorite' : 'Favorite'}
              aria-pressed={activeDetail.summary.is_favorite}
              style={activeDetail.summary.is_favorite ? { color: '#f2cf6b' } : undefined}
              onClick={() => void handleToggleFavorite()}
            >
              <Star size={16} fill={activeDetail.summary.is_favorite ? 'currentColor' : 'none'} />
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
        {showWindowsCaption && <WindowsCaptionButtons />}
      </header>

      <div className="lightbox-body">
        <div
          ref={stageRef}
          className="lightbox-stage"
          onTouchStart={handleTouchStart}
          onTouchMove={handleTouchMove}
          onTouchEnd={handleTouchEnd}
          onTouchCancel={handleTouchCancel}
          onMouseDown={handleMouseDown}
          onMouseMove={handleMouseMove}
          onMouseUp={handleMouseUp}
          onMouseLeave={handleMouseUp}
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

          {error ? (
            <div className="error-banner" role="alert">
              {error}
            </div>
          ) : isVideo && activeDetail ? (
            <div
              key={lightboxAssetId}
              className={`lightbox-media-frame is-nav-${navDir}`}
            >
              <VideoPlayer
                playback={activeDetail.playback}
                posterUrl={mediaThumbUrl(lightboxAssetId)}
                initialTimestampSec={lightboxInitialTimestampSec}
                className="lightbox-media"
              />
            </div>
          ) : (
            <div
              key={lightboxAssetId}
              className={`lightbox-media-frame is-nav-${navDir}`}
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
                  transition:
                    isDragging || isWheeling
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
              initialDetail={activeDetail}
              loadingInitialDetail={detailLoading}
              onAssetUpdated={(updated) => setDetail(updated)}
            />
          </aside>
        )}
      </div>
    </div>
  );
};
