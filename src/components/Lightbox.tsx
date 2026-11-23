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
  triggerAssetDownload,
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

const MAX_DETAIL_CACHE = 80;
const MAX_PRELOAD_IMAGES = 24;
const SLIDE_TRANSITION_MS = 240;

const detailCache = new Map<string, AssetDetail>();
const previewImageCache = new Map<string, HTMLImageElement>();
const decodedUrlSet = new Set<string>();

function getCachedDetail(id: string): AssetDetail | null {
  const hit = detailCache.get(id) ?? null;
  if (hit) {
    detailCache.delete(id);
    detailCache.set(id, hit);
  }
  return hit;
}

function setCachedDetail(id: string, detail: AssetDetail): void {
  if (detailCache.has(id)) detailCache.delete(id);
  detailCache.set(id, detail);
  if (detailCache.size > MAX_DETAIL_CACHE) {
    const oldest = detailCache.keys().next().value;
    if (oldest !== undefined) detailCache.delete(oldest);
  }
}

function preloadPreviewUrl(url: string): void {
  if (typeof Image === 'undefined') return;
  if (previewImageCache.has(url)) {
    const existing = previewImageCache.get(url)!;
    previewImageCache.delete(url);
    previewImageCache.set(url, existing);
    return;
  }
  const img = new Image();
  img.decoding = 'async';
  img.onload = () => {
    decodedUrlSet.add(url);
  };
  img.src = url;
  if (typeof img.decode === 'function') {
    img
      .decode()
      .then(() => {
        decodedUrlSet.add(url);
      })
      .catch(() => {});
  }
  previewImageCache.set(url, img);
  if (previewImageCache.size > MAX_PRELOAD_IMAGES) {
    const oldest = previewImageCache.keys().next().value;
    if (oldest !== undefined) previewImageCache.delete(oldest);
  }
}

export function clearLightboxCaches(): void {
  detailCache.clear();
  previewImageCache.clear();
  decodedUrlSet.clear();
}

function touchDistance(a: React.Touch, b: React.Touch): number {
  const dx = a.clientX - b.clientX;
  const dy = a.clientY - b.clientY;
  return Math.hypot(dx, dy);
}

interface ExitingSlide {
  key: string;
  assetId: string;
  previewUrl: string;
  thumbUrl: string;
  dir: 'next' | 'prev';
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
  const authenticated = useAppStore((s) => s.authenticated);

  const canMutate = user?.role === 'admin' || user?.role === 'contributor';

  const [detail, setDetail] = useState<AssetDetail | null>(() =>
    lightboxAssetId ? getCachedDetail(lightboxAssetId) : null,
  );
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [infoOpen, setInfoOpen] = useState(false);
  const [rawDemosaic, setRawDemosaic] = useState(false);
  const [livePlaying, setLivePlaying] = useState(false);
  const [liveVideoReady, setLiveVideoReady] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const [animateZoom, setAnimateZoom] = useState(false);

  // Stacked zero-flash media readiness state
  const [previewLoadedUrl, setPreviewLoadedUrl] = useState<string | null>(null);
  const [underlayDismissedUrl, setUnderlayDismissedUrl] = useState<string | null>(null);
  const [readyOverlayUrl, setReadyOverlayUrl] = useState<string | null>(null);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const [hiResLoading, setHiResLoading] = useState(false);

  // Smooth dual-slot slide transition state
  const [navDir, setNavDir] = useState<'fade' | 'next' | 'prev'>('fade');
  const [exitingSlide, setExitingSlide] = useState<ExitingSlide | null>(null);

  const overlayRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const exitingTimerRef = useRef<number | null>(null);
  const slideSeqRef = useRef(0);
  const prevAssetRef = useRef<{ id: string; previewUrl: string } | null>(null);
  const wasOpenRef = useRef(false);
  const zoomRef = useRef(1);
  const panRef = useRef({ x: 0, y: 0 });
  const readyOverlayUrlRef = useRef<string | null>(null);
  const infoOpenRef = useRef(false);
  const closeLightboxRef = useRef(closeLightbox);
  const lastTouchDoubleTapAtRef = useRef(0);
  const touchLongPressLiveRef = useRef(false);
  zoomRef.current = zoom;
  panRef.current = pan;
  readyOverlayUrlRef.current = readyOverlayUrl;
  infoOpenRef.current = infoOpen;
  closeLightboxRef.current = closeLightbox;

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

  // Clear caches when user signs out so another profile never sees stale details.
  useEffect(() => {
    if (!authenticated) {
      clearLightboxCaches();
    }
  }, [authenticated]);

  const clearLongPress = useCallback(() => {
    if (gestureRef.current.longPressTimer !== null) {
      window.clearTimeout(gestureRef.current.longPressTimer);
      gestureRef.current.longPressTimer = null;
    }
  }, []);

  useEffect(() => {
    return () => {
      clearLongPress();
      if (exitingTimerRef.current !== null) {
        window.clearTimeout(exitingTimerRef.current);
        exitingTimerRef.current = null;
      }
    };
  }, [clearLongPress]);

  const currentIndex = useMemo(() => {
    if (!lightboxAssetId) return -1;
    return lightboxAssetIds.indexOf(lightboxAssetId);
  }, [lightboxAssetId, lightboxAssetIds]);

  const hasPrev = currentIndex > 0;
  const hasNext = currentIndex >= 0 && currentIndex < lightboxAssetIds.length - 1;

  const navigateWithSlide = useCallback(
    (targetId: string, dir: 'next' | 'prev') => {
      if (lightboxAssetId && lightboxAssetId !== targetId) {
        const curPreview =
          prevAssetRef.current?.id === lightboxAssetId
            ? prevAssetRef.current.previewUrl
            : mediaPreviewUrl(lightboxAssetId);
        slideSeqRef.current += 1;
        setExitingSlide({
          key: `${lightboxAssetId}-${slideSeqRef.current}`,
          assetId: lightboxAssetId,
          previewUrl: curPreview,
          thumbUrl: mediaThumbUrl(lightboxAssetId),
          dir,
        });
        if (exitingTimerRef.current !== null) {
          window.clearTimeout(exitingTimerRef.current);
        }
        exitingTimerRef.current = window.setTimeout(() => {
          setExitingSlide(null);
          exitingTimerRef.current = null;
        }, SLIDE_TRANSITION_MS + 30);
      }
      setNavDir(dir);
      setLightboxAsset(targetId);
    },
    [lightboxAssetId, setLightboxAsset],
  );

  const goPrev = useCallback(() => {
    if (!hasPrev) return;
    navigateWithSlide(lightboxAssetIds[currentIndex - 1], 'prev');
  }, [currentIndex, hasPrev, lightboxAssetIds, navigateWithSlide]);

  const goNext = useCallback(() => {
    if (!hasNext) return;
    navigateWithSlide(lightboxAssetIds[currentIndex + 1], 'next');
  }, [currentIndex, hasNext, lightboxAssetIds, navigateWithSlide]);

  useEffect(() => {
    clearLongPress();
    if (!lightboxAssetId) {
      setDetail(null);
      setDetailLoading(false);
      setInfoOpen(false);
      setNavDir('fade');
      setExitingSlide(null);
      prevAssetRef.current = null;
      if (exitingTimerRef.current !== null) {
        window.clearTimeout(exitingTimerRef.current);
        exitingTimerRef.current = null;
      }
      return;
    }

    const cached = getCachedDetail(lightboxAssetId);
    const nextPreview = cached?.playback.preview_url ?? mediaPreviewUrl(lightboxAssetId);
    prevAssetRef.current = { id: lightboxAssetId, previewUrl: nextPreview };

    let active = true;
    setError(null);
    setRawDemosaic(false);
    setLivePlaying(false);
    setLiveVideoReady(false);
    setAnimateZoom(false);
    setZoom(1);
    setPan({ x: 0, y: 0 });
    setReadyOverlayUrl(null);
    setOverlayVisible(false);
    setHiResLoading(false);
    const alreadyDecoded = decodedUrlSet.has(nextPreview);
    setPreviewLoadedUrl(alreadyDecoded ? nextPreview : null);
    setUnderlayDismissedUrl(alreadyDecoded ? nextPreview : null);

    if (cached) {
      setDetail(cached);
      setDetailLoading(false);
    } else {
      setDetailLoading(true);
    }

    void getAssetDetail(lightboxAssetId)
      .then((res) => {
        if (!active) return;
        setCachedDetail(lightboxAssetId, res);
        setDetail(res);
        prevAssetRef.current = {
          id: lightboxAssetId,
          previewUrl: res.playback.preview_url ?? mediaPreviewUrl(lightboxAssetId),
        };
      })
      .catch((err) => {
        if (!active) return;
        if (!cached) {
          setError(errorMessage(err));
        }
      })
      .finally(() => {
        if (active) setDetailLoading(false);
      });

    return () => {
      active = false;
      clearLongPress();
    };
  }, [clearLongPress, lightboxAssetId]);

  // Preload adjacent ±1 and ±2 preview images AND AssetDetail metadata so photo switching is instant & flash-free.
  useEffect(() => {
    if (!lightboxAssetId || currentIndex < 0) return;
    let cancelled = false;
    const offsets = [1, -1, 2, -2];
    const neighbors = offsets
      .map((delta) => lightboxAssetIds[currentIndex + delta])
      .filter((id): id is string => Boolean(id) && id !== lightboxAssetId);

    for (const id of neighbors) {
      const cached = getCachedDetail(id);
      preloadPreviewUrl(cached?.playback.preview_url ?? mediaPreviewUrl(id));
      if (!cached) {
        void getAssetDetail(id)
          .then((res) => {
            if (cancelled) return;
            setCachedDetail(id, res);
            if (res.playback.preview_url) {
              preloadPreviewUrl(res.playback.preview_url);
            }
          })
          .catch(() => {});
      }
    }

    return () => {
      cancelled = true;
    };
  }, [currentIndex, lightboxAssetId, lightboxAssetIds]);

  const activeDetail = detail?.summary.id === lightboxAssetId ? detail : null;
  // Keep prior detail ONLY to stabilize non-conflicting toolbar layout during cold cache misses.
  const toolbarDetail = activeDetail ?? detail;

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

  // Move focus into Lightbox overlay ONLY when transitioning from closed -> open (prevents focus-ring flicker on next/prev).
  useEffect(() => {
    const isOpen = Boolean(lightboxAssetId);
    if (isOpen && !wasOpenRef.current) {
      overlayRef.current?.focus({ preventScroll: true });
    }
    wasOpenRef.current = isOpen;
  }, [lightboxAssetId]);

  const handleToggleFavorite = useCallback(async () => {
    if (!canMutate || !lightboxAssetId || !activeDetail) return;
    const nextFav = !activeDetail.summary.is_favorite;
    const optimistic: AssetDetail = {
      ...activeDetail,
      summary: { ...activeDetail.summary, is_favorite: nextFav },
    };
    setCachedDetail(lightboxAssetId, optimistic);
    setDetail(optimistic);
    try {
      await setAssetFavorite(lightboxAssetId, nextFav);
      bumpLibraryRevision();
    } catch (err) {
      setCachedDetail(lightboxAssetId, activeDetail);
      setDetail((prev) => (prev && prev.summary.id === lightboxAssetId ? activeDetail : prev));
      notifyError(err, 'Could not update favorite');
    }
  }, [activeDetail, bumpLibraryRevision, canMutate, lightboxAssetId]);

  const handleMoveToTrash = useCallback(async () => {
    if (!canMutate || !lightboxAssetId) return;
    const trashedId = lightboxAssetId;
    try {
      await trashAssets([trashedId]);
      detailCache.delete(trashedId);
      bumpLibraryRevision();
      notifySuccess('Moved item to Trash');
      removeLightboxAsset(trashedId);
    } catch (err) {
      notifyError(err, 'Could not move item to Trash');
    }
  }, [bumpLibraryRevision, canMutate, lightboxAssetId, removeLightboxAsset]);

  const togglePixelPeepZoom = useCallback(() => {
    setAnimateZoom(true);
    setZoom((prev) => {
      if (prev > 1.02) {
        setPan({ x: 0, y: 0 });
        return 1;
      }
      return 2.5;
    });
  }, []);

  // Dedicated Escape layer effect keyed ONLY on open/closed state so detail fetches or Info drawer
  // toggles never re-register the Lightbox layer above open ContextMenus or confirm dialogs.
  const isLightboxOpen = Boolean(lightboxAssetId);
  useEffect(() => {
    if (!isLightboxOpen) return;
    const layer = pushEscapeLayer();
    const onEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isTopEscapeLayer(layer)) {
        event.preventDefault();
        event.stopPropagation();
        if (infoOpenRef.current) {
          setInfoOpen(false);
        } else {
          closeLightboxRef.current();
        }
      }
    };
    document.addEventListener('keydown', onEscape);
    return () => {
      document.removeEventListener('keydown', onEscape);
      popEscapeLayer(layer);
    };
  }, [isLightboxOpen]);

  // Lightbox keyboard shortcuts
  useEffect(() => {
    if (!lightboxAssetId) return;

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        return;
      }

      if (event.metaKey || event.ctrlKey || event.altKey) {
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
            navigateWithSlide(lightboxAssetIds[0], 'prev');
          }
          break;
        case 'End':
          if (lightboxAssetIds.length > 0 && currentIndex !== lightboxAssetIds.length - 1) {
            event.preventDefault();
            navigateWithSlide(lightboxAssetIds[lightboxAssetIds.length - 1], 'next');
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
          setAnimateZoom(true);
          setZoom((prev) => Math.min(5, Number((prev + 0.5).toFixed(2))));
          break;
        case '-':
          event.preventDefault();
          setAnimateZoom(true);
          setZoom((prev) => {
            const next = Math.max(1, Number((prev - 0.5).toFixed(2)));
            if (next <= 1.02) {
              setPan({ x: 0, y: 0 });
              return 1;
            }
            setPan((p) => clampPan(p.x, p.y, next));
            return next;
          });
          break;
        case '0':
          event.preventDefault();
          setAnimateZoom(true);
          setZoom(1);
          setPan({ x: 0, y: 0 });
          break;
        case ' ': {
          const isLivePhoto = Boolean(
            activeDetail?.playback.live_video_url ||
              activeDetail?.summary.kind === 'live_photo' ||
              activeDetail?.summary.kind === 'motion_photo',
          );
          if (isLivePhoto) {
            event.preventDefault();
            setLivePlaying((prev) => {
              if (prev) setLiveVideoReady(false);
              return !prev;
            });
            break;
          }
          const videoEl = stageRef.current?.querySelector('video');
          if (videoEl) {
            event.preventDefault();
            if (videoEl.paused) void videoEl.play().catch(() => {});
            else videoEl.pause();
          }
          break;
        }
      }
    };

    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [
    activeDetail,
    clampPan,
    currentIndex,
    goNext,
    goPrev,
    handleToggleFavorite,
    lightboxAssetId,
    lightboxAssetIds,
    navigateWithSlide,
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

  // Zero-flash stacked hi-res / RAW overlay:
  // Keeps the base preview <img> untouched underneath; mounts the overlay <img> at opacity 0
  // and fades it in smoothly ONLY after load + decode(). Stays mounted on zoom-out so zooming
  // in and out never swaps src or flashes!
  const wantsOverlay = Boolean(
    lightboxAssetId && !isVideo && fullResUrl && (rawDemosaic || zoom > 1.02),
  );

  useEffect(() => {
    if (!lightboxAssetId || isVideo || !fullResUrl) return;
    if (!wantsOverlay) {
      // When toggling RAW 100% back to Preview at 1x, smoothly fade out RAW overlay while preview stays painted.
      if (!rawDemosaic && readyOverlayUrl?.includes('mode=raw_demosaic')) {
        setOverlayVisible(false);
      }
      setHiResLoading(false);
      return;
    }

    if (readyOverlayUrl !== fullResUrl) {
      setReadyOverlayUrl(fullResUrl);
    }
    if (decodedUrlSet.has(fullResUrl)) {
      setOverlayVisible(true);
      setHiResLoading(false);
    } else {
      setOverlayVisible(false);
      setHiResLoading(true);
    }
  }, [fullResUrl, isVideo, lightboxAssetId, rawDemosaic, readyOverlayUrl, wantsOverlay]);

  // Native non-passive wheel zoom listener with pointer focal-point anchoring & zero CSS transition jitter.
  useEffect(() => {
    const el = stageRef.current;
    if (!el || !lightboxAssetId) return;
    const onNativeWheel = (e: WheelEvent) => {
      if (isVideo) return;
      if (Math.abs(e.deltaY) < 1) return;
      e.preventDefault();
      setAnimateZoom(false);

      const curZoom = zoomRef.current;
      const curPan = panRef.current;
      // Smooth continuous delta scaling for both trackpad pinch/scroll and notched mice.
      const clampedDelta = Math.max(-140, Math.min(140, e.deltaY));
      const factor = Math.exp(-clampedDelta * 0.0025);
      const nextZoom = Math.min(5, Math.max(1, Number((curZoom * factor).toFixed(3))));
      if (nextZoom <= 1.02) {
        setZoom(1);
        setPan({ x: 0, y: 0 });
        return;
      }

      const rect = el.getBoundingClientRect();
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const dx = e.clientX - cx;
      const dy = e.clientY - cy;
      const ratio = nextZoom / curZoom;
      const nextPanX = curPan.x * ratio + dx * (1 - ratio);
      const nextPanY = curPan.y * ratio + dy * (1 - ratio);
      setZoom(nextZoom);
      setPan(clampPan(nextPanX, nextPanY, nextZoom));
    };
    el.addEventListener('wheel', onNativeWheel, { passive: false });
    return () => {
      el.removeEventListener('wheel', onNativeWheel);
    };
  }, [clampPan, isVideo, lightboxAssetId]);

  if (!lightboxAssetId) return null;

  const titleDetail = activeDetail ?? toolbarDetail;
  const titlePrimaryFile =
    titleDetail?.files.find((f) => f.file_role === 'primary') ?? titleDetail?.files[0];
  const titleText =
    titleDetail?.overrides?.title ||
    titleDetail?.summary.title ||
    titlePrimaryFile?.rel_path.split(/[\\/]/).pop() ||
    '\u00A0';

  const previewUrl = activeDetail?.playback.preview_url ?? mediaPreviewUrl(lightboxAssetId);
  const thumbUrl = mediaThumbUrl(lightboxAssetId);
  const previewIsReady = previewLoadedUrl === previewUrl || decodedUrlSet.has(previewUrl);
  const showUnderlay = !previewIsReady || underlayDismissedUrl !== previewUrl;
  const usingHiRes = Boolean(readyOverlayUrl && overlayVisible && (rawDemosaic || zoom > 1.02));
  const liveVideoUrl = activeDetail?.playback.live_video_url ?? mediaLiveVideoUrl(lightboxAssetId);

  const handleTouchStart = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (e.touches.length === 2) {
      setAnimateZoom(false);
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
        lastTouchDoubleTapAtRef.current = now;
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
          touchLongPressLiveRef.current = true;
          setLiveVideoReady(false);
          setLivePlaying(true);
        }, 420);
      }
    }
  };

  const handleTouchMove = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (e.touches.length === 2 && gestureRef.current.pinchStartDist) {
      setAnimateZoom(false);
      const dist = touchDistance(e.touches[0], e.touches[1]);
      const scaleFactor = dist / gestureRef.current.pinchStartDist;
      const nextZoom = Math.min(4, Math.max(1, gestureRef.current.pinchStartZoom * scaleFactor));
      setZoom(nextZoom);
      if (nextZoom <= 1.02) {
        setPan({ x: 0, y: 0 });
      } else {
        setPan((p) => clampPan(p.x, p.y, nextZoom));
      }
      return;
    }
    if (e.touches.length === 1 && zoom > 1.02) {
      setAnimateZoom(false);
      const t = e.touches[0];
      const dx = t.clientX - gestureRef.current.startX;
      const dy = t.clientY - gestureRef.current.startY;
      setPan(clampPan(gestureRef.current.startPanX + dx, gestureRef.current.startPanY + dy, zoom));
    }
  };

  const handleTouchEnd = (e: React.TouchEvent<HTMLDivElement>) => {
    clearLongPress();
    if (touchLongPressLiveRef.current) {
      touchLongPressLiveRef.current = false;
      setLivePlaying(false);
      setLiveVideoReady(false);
      return;
    }
    if (gestureRef.current.pinchStartDist !== null || gestureRef.current.wasPinching) {
      gestureRef.current.pinchStartDist = null;
      if (e.touches.length === 0) {
        gestureRef.current.wasPinching = false;
      }
      return;
    }
    if (zoom <= 1.02 && e.changedTouches.length === 1) {
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
    if (touchLongPressLiveRef.current) {
      touchLongPressLiveRef.current = false;
      setLivePlaying(false);
      setLiveVideoReady(false);
    }
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
    if (zoom > 1.02) {
      setAnimateZoom(false);
      gestureRef.current.dragging = true;
      setIsDragging(true);
    }
  };

  const handleMouseMove = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!gestureRef.current.dragging || zoom <= 1.02) return;
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
    if (Date.now() - lastTouchDoubleTapAtRef.current < 380) {
      return;
    }
    if (gestureRef.current.didDrag) {
      gestureRef.current.didDrag = false;
      return;
    }
    setAnimateZoom(true);
    if (zoom > 1.02) {
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
                setAnimateZoom(true);
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
          triggerAssetDownload(lightboxAssetId);
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

  const subtitleText = titleDetail
    ? [
        formatDateTime(titleDetail.summary.capture_at_ms),
        titleDetail.photo_meta?.focal_length_mm != null && titleDetail.photo_meta.focal_length_mm > 0
          ? `${Math.round(titleDetail.photo_meta.focal_length_mm)}mm`
          : null,
        titleDetail.photo_meta?.aperture_f != null && titleDetail.photo_meta.aperture_f > 0
          ? `f/${Number(titleDetail.photo_meta.aperture_f.toFixed(1))}`
          : null,
        titleDetail.photo_meta?.shutter_speed_sec != null &&
        titleDetail.photo_meta.shutter_speed_sec > 0
          ? titleDetail.photo_meta.shutter_speed_sec >= 1
            ? `${Number(titleDetail.photo_meta.shutter_speed_sec.toFixed(1))}s`
            : `1/${Math.max(1, Math.round(1 / titleDetail.photo_meta.shutter_speed_sec))}s`
          : null,
        titleDetail.photo_meta?.iso != null && titleDetail.photo_meta.iso > 0
          ? `ISO ${titleDetail.photo_meta.iso}`
          : null,
        lightboxAssetIds.length > 1 && currentIndex >= 0
          ? `${currentIndex + 1} / ${lightboxAssetIds.length}`
          : null,
      ]
        .filter(Boolean)
        .join(' · ')
    : lightboxAssetIds.length > 1 && currentIndex >= 0
      ? `${currentIndex + 1} / ${lightboxAssetIds.length}`
      : '\u00A0';

  const isFav = Boolean(activeDetail?.summary.is_favorite);
  const hdLabel = hiResLoading ? 'HD…' : usingHiRes ? 'HD' : null;

  return (
    <div
      ref={overlayRef}
      tabIndex={-1}
      className="lightbox-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Media lightbox"
    >
      <header className="lightbox-toolbar" data-tauri-drag-region="deep">
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
          <div style={{ minWidth: 0, minHeight: 31, display: 'flex', flexDirection: 'column', justifyContent: 'center' }}>
            <div
              style={{
                fontSize: 13,
                fontWeight: 600,
                lineHeight: 1.25,
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}
            >
              {titleText}
            </div>
            <div
              className="view-subtitle"
              style={{
                marginTop: 0,
                fontSize: 11,
                lineHeight: 1.3,
                fontVariantNumeric: 'tabular-nums',
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}
            >
              {subtitleText}
            </div>
          </div>
        </div>

        <div className="row" style={{ gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end', flexShrink: 0 }}>
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
              onClick={() => {
                setLivePlaying((prev) => {
                  if (prev) setLiveVideoReady(false);
                  return !prev;
                });
              }}
            >
              <Play size={11} fill="currentColor" />
              <span>{activeDetail?.summary.kind === 'motion_photo' ? 'MOTION' : 'LIVE'}</span>
            </button>
          )}

          {!isVideo && (
            <button
              type="button"
              className={`lightbox-pill-btn lightbox-zoom-btn${zoom > 1 ? ' is-active' : ''}`}
              aria-pressed={zoom > 1}
              title={usingHiRes ? 'Viewing full original resolution' : 'Click photo or button to zoom'}
              onClick={togglePixelPeepZoom}
            >
              <ZoomIn size={13} />
              <span>{zoom > 1 ? `${zoom.toFixed(1)}×` : '100%'}</span>
              {hdLabel && <span className="lightbox-hd-tag">{hdLabel}</span>}
            </button>
          )}

          {canMutate && (
            <button
              type="button"
              className="icon-button"
              disabled={!activeDetail}
              aria-label={isFav ? 'Remove favorite' : 'Favorite'}
              aria-pressed={isFav}
              style={isFav ? { color: '#f2cf6b' } : undefined}
              onClick={() => void handleToggleFavorite()}
            >
              <Star size={16} fill={isFav ? 'currentColor' : 'none'} />
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

          {/* Outgoing slide layer for simultaneous cross-slide photo-to-photo transitions */}
          {exitingSlide && (
            <div
              key={exitingSlide.key}
              className={`lightbox-media-frame is-exiting-${exitingSlide.dir}`}
              aria-hidden="true"
            >
              <div className="lightbox-zoom-surface">
                <img
                  className="lightbox-media lightbox-media-underlay"
                  src={exitingSlide.thumbUrl}
                  alt=""
                  draggable={false}
                />
                <img
                  className="lightbox-media"
                  src={exitingSlide.previewUrl}
                  alt=""
                  draggable={false}
                />
              </div>
            </div>
          )}

          {error ? (
            <div className="error-banner" role="alert">
              {error}
            </div>
          ) : isVideo && activeDetail ? (
            <div key={lightboxAssetId} className={`lightbox-media-frame is-nav-${navDir}`}>
              <VideoPlayer
                playback={activeDetail.playback}
                posterUrl={activeDetail.playback.preview_url ?? mediaThumbUrl(lightboxAssetId)}
                initialTimestampSec={lightboxInitialTimestampSec}
                className="lightbox-media"
              />
            </div>
          ) : (
            <div key={lightboxAssetId} className={`lightbox-media-frame is-nav-${navDir}`}>
              <div
                className="lightbox-zoom-surface"
                style={{
                  transform: `translate3d(${pan.x}px, ${pan.y}px, 0) scale(${zoom})`,
                  transition: animateZoom
                    ? 'transform 220ms cubic-bezier(0.22, 1, 0.36, 1)'
                    : 'none',
                }}
              >
                {/* Instant cached grid thumbnail underlay stays mounted until preview fade finishes */}
                {showUnderlay && (
                  <img
                    className="lightbox-media lightbox-media-underlay"
                    src={thumbUrl}
                    alt=""
                    aria-hidden="true"
                    draggable={false}
                  />
                )}

                {/* Stable base preview image: NEVER swaps src during zoom-in/out or RAW toggles */}
                <img
                  ref={imgRef}
                  className="lightbox-media"
                  src={previewUrl}
                  alt={titleText}
                  draggable={false}
                  decoding="async"
                  onClick={handleImageClick}
                  onLoad={(e) => {
                    const el = e.currentTarget;
                    if (el.naturalWidth <= 0) return;
                    const reveal = () => {
                      decodedUrlSet.add(previewUrl);
                      setPreviewLoadedUrl(previewUrl);
                    };
                    if (typeof el.decode === 'function') {
                      el.decode().then(reveal, reveal);
                    } else {
                      reveal();
                    }
                  }}
                  onTransitionEnd={(e) => {
                    if (e.propertyName === 'opacity' && previewIsReady) {
                      setUnderlayDismissedUrl(previewUrl);
                    }
                  }}
                  style={{
                    cursor: zoom > 1.02 ? (isDragging ? 'grabbing' : 'grab') : 'zoom-in',
                    opacity: previewIsReady ? 1 : 0,
                    transition: 'opacity 140ms ease-out',
                  }}
                />

                {/* Stacked decoded full-resolution / RAW 100% overlay: fades in seamlessly with zero flash */}
                {readyOverlayUrl && (
                  <img
                    data-testid="lightbox-hires-layer"
                    className="lightbox-media lightbox-media-overlay"
                    src={readyOverlayUrl}
                    alt=""
                    aria-hidden="true"
                    draggable={false}
                    decoding="async"
                    onLoad={(e) => {
                      const el = e.currentTarget;
                      const url = readyOverlayUrl;
                      const reveal = () => {
                        decodedUrlSet.add(url);
                        if (readyOverlayUrlRef.current === url) {
                          setOverlayVisible(true);
                          setHiResLoading(false);
                        }
                      };
                      if (typeof el.decode === 'function') {
                        el.decode().then(reveal, reveal);
                      } else {
                        reveal();
                      }
                    }}
                    onError={() => {
                      setHiResLoading(false);
                    }}
                    style={{
                      opacity: overlayVisible ? 1 : 0,
                    }}
                  />
                )}

                {/* Live Photo video fades in ONLY once playback frame is ready (zero black flash) */}
                {livePlaying && hasLiveClip && (
                  <video
                    className="lightbox-media lightbox-media-overlay"
                    src={liveVideoUrl}
                    poster={previewUrl}
                    playsInline
                    muted
                    autoPlay
                    loop
                    onPlaying={() => setLiveVideoReady(true)}
                    style={{
                      opacity: liveVideoReady ? 1 : 0,
                    }}
                  />
                )}
              </div>
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
              onAssetUpdated={(updated) => {
                setCachedDetail(updated.summary.id, updated);
                setDetail(updated);
              }}
            />
          </aside>
        )}
      </div>
    </div>
  );
};
