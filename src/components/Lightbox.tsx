import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Check,
  ChevronLeft,
  ChevronRight,
  Download,
  Info,
  Layers,
  Play,
  Share2,
  Star,
  Trash2,
  Unlink,
  X,
  ZoomIn,
} from 'lucide-react';
import {
  getAssetDetail,
  mediaLiveVideoUrl,
  mediaOriginalUrl,
  mediaPreviewUrl,
  mediaThumbUrl,
  removeFromStack,
  setAssetFavorite,
  setStackCover,
  trashAssets,
  triggerAssetDownload,
} from '../lib/api';
import { isTopEscapeLayer, popEscapeLayer, pushEscapeLayer } from '../lib/escapeStack';
import { formatDateTime } from '../lib/format';
import { isTauri } from '../lib/tauri';
import { notifyError, notifyInfo, notifySuccess, useFeedback } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AssetDetail, StackMemberView } from '../types/api';
import { AssetInspector } from './AssetInspector';
import { openContextMenu, useContextMenuStore } from './ContextMenu';
import { VideoPlayer } from './VideoPlayer';
import { WindowsCaptionButtons, isWindowsPlatform } from './WindowControls';

const showWindowsCaption = isTauri() && isWindowsPlatform();
const ZERO_PAN = { x: 0, y: 0 };
const INFO_SHEET_EXIT_MS = 220;

function formatStackVersionLabel(member: StackMemberView, members: StackMemberView[]): string {
  const base =
    member.stack_role === 'edited'
      ? 'Edited'
      : member.stack_role === 'raw' || member.is_raw
        ? 'RAW'
        : member.stack_role === 'jpeg'
          ? 'JPEG'
          : member.extension.toUpperCase() || 'Version';
  const sameRole = members.filter((m) => {
    const mb =
      m.stack_role === 'edited'
        ? 'Edited'
        : m.stack_role === 'raw' || m.is_raw
          ? 'RAW'
          : m.stack_role === 'jpeg'
            ? 'JPEG'
            : m.extension.toUpperCase() || 'Version';
    return mb === base;
  });
  if (sameRole.length <= 1) return base;
  const idx = sameRole.findIndex((m) => m.asset_id === member.asset_id);
  return `${base} ${idx + 1}`;
}

function readIsMobileViewport(): boolean {
  if (typeof window === 'undefined') return false;
  if (typeof window.matchMedia === 'function') {
    return window.matchMedia('(max-width: 767px)').matches;
  }
  return window.innerWidth <= 767;
}

function canUseNativeWebShare(): boolean {
  if (isTauri() || typeof navigator === 'undefined') return false;
  return typeof navigator.share === 'function' && typeof navigator.canShare === 'function';
}

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

  // Active version inside a multi-version stack (null = use the timeline slot's `lightboxAssetId`).
  const [activeVersionId, setActiveVersionId] = useState<string | null>(null);
  const displayedAssetId = lightboxAssetId ? (activeVersionId ?? lightboxAssetId) : null;

  useEffect(() => {
    setActiveVersionId(null);
  }, [lightboxAssetId]);

  const [detail, setDetail] = useState<AssetDetail | null>(() =>
    displayedAssetId ? getCachedDetail(displayedAssetId) : null,
  );
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [infoOpen, setInfoOpen] = useState(false);
  const [infoRendered, setInfoRendered] = useState(false);
  const [isMobileViewport, setIsMobileViewport] = useState<boolean>(readIsMobileViewport);
  const [sheetDragY, setSheetDragY] = useState(0);
  const [sheetDragging, setSheetDragging] = useState(false);
  const [sharing, setSharing] = useState(false);
  const [trashing, setTrashing] = useState(false);

  const [rawDemosaic, setRawDemosaic] = useState(false);
  const [livePlaying, setLivePlaying] = useState(false);
  const [liveVideoReady, setLiveVideoReady] = useState(false);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState(ZERO_PAN);
  const [mediaStateAssetId, setMediaStateAssetId] = useState<string | null>(displayedAssetId);
  const [isDragging, setIsDragging] = useState(false);
  const [animateZoom, setAnimateZoom] = useState(false);

  // Stacked zero-flash media readiness state
  const [previewLoadedUrl, setPreviewLoadedUrl] = useState<string | null>(null);
  const [underlayDismissedUrl, setUnderlayDismissedUrl] = useState<string | null>(null);
  const [readyOverlayUrl, setReadyOverlayUrl] = useState<string | null>(null);
  const [readyOverlayAssetId, setReadyOverlayAssetId] = useState<string | null>(null);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const [hiResLoading, setHiResLoading] = useState(false);

  // Smooth dual-slot slide transition state
  const [navDir, setNavDir] = useState<'fade' | 'next' | 'prev'>('fade');
  const [exitingSlide, setExitingSlide] = useState<ExitingSlide | null>(null);

  const overlayRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  const infoDrawerRef = useRef<HTMLElement | null>(null);
  const shareAbortRef = useRef<AbortController | null>(null);
  const activeAssetIdRef = useRef<string | null>(displayedAssetId);
  activeAssetIdRef.current = displayedAssetId;
  const exitingTimerRef = useRef<number | null>(null);
  const slideSeqRef = useRef(0);
  const prevAssetRef = useRef<{ id: string; previewUrl: string } | null>(null);
  const wasOpenRef = useRef(false);
  const zoomRef = useRef(1);
  const panRef = useRef(ZERO_PAN);
  const readyOverlayUrlRef = useRef<string | null>(null);
  const infoOpenRef = useRef(false);
  const closeLightboxRef = useRef(closeLightbox);
  const lastTouchAtRef = useRef(0);
  const touchLongPressLiveRef = useRef(false);
  const sheetDragStartYRef = useRef<number | null>(null);
  const sheetDragAtRef = useRef(0);
  zoomRef.current = zoom;
  panRef.current = pan;
  readyOverlayUrlRef.current = readyOverlayUrl;
  infoOpenRef.current = infoOpen;
  closeLightboxRef.current = closeLightbox;

  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(max-width: 767px)');
    const onChange = () => setIsMobileViewport(mq.matches);
    onChange();
    if (typeof mq.addEventListener === 'function') {
      mq.addEventListener('change', onChange);
      return () => mq.removeEventListener('change', onChange);
    }
    mq.addListener(onChange);
    return () => mq.removeListener(onChange);
  }, []);

  useEffect(() => {
    if (infoOpen) {
      setInfoRendered(true);
      setSheetDragY(0);
      setSheetDragging(false);
      return;
    }
    const t = window.setTimeout(() => {
      setInfoRendered(false);
      setSheetDragY(0);
      setSheetDragging(false);
    }, INFO_SHEET_EXIT_MS);
    return () => window.clearTimeout(t);
  }, [infoOpen]);

  useEffect(() => {
    const el = infoDrawerRef.current as (HTMLElement & { inert?: boolean }) | null;
    if (el) {
      el.inert = !infoOpen;
    }
  }, [infoOpen]);

  // Listen for mobile browser Back button while Info sheet is open so Back closes Info first
  // (matching Escape behavior) before closing Lightbox.
  useEffect(() => {
    if (typeof window === 'undefined') return;
    const onPop = (e: Event) => {
      if (infoOpenRef.current) {
        e.preventDefault();
        setInfoOpen(false);
      }
    };
    window.addEventListener('tp:lightbox-popstate', onPop);
    return () => window.removeEventListener('tp:lightbox-popstate', onPop);
  }, []);

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
    lastTapX: number;
    lastTapY: number;
    ignoreTouch: boolean;
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
    lastTapX: 0,
    lastTapY: 0,
    ignoreTouch: false,
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
      // Reset zoom/pan/overlay immediately on navigation so the next slide never renders a 1-frame flash
      // of the previous photo's zoom/pan or hi-res layer before effects run.
      setAnimateZoom(false);
      setZoom(1);
      setPan(ZERO_PAN);
      setRawDemosaic(false);
      setLivePlaying(false);
      setLiveVideoReady(false);
      setReadyOverlayUrl(null);
      setReadyOverlayAssetId(null);
      setOverlayVisible(false);
      setHiResLoading(false);
      setMediaStateAssetId(targetId);
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
    shareAbortRef.current?.abort();
    shareAbortRef.current = null;
    setSharing(false);
    gestureRef.current.lastTapAt = 0;
    gestureRef.current.ignoreTouch = false;
    if (!displayedAssetId) {
      setDetail(null);
      setDetailLoading(false);
      setInfoOpen(false);
      setInfoRendered(false);
      setMediaStateAssetId(null);
      setReadyOverlayUrl(null);
      setReadyOverlayAssetId(null);
      setOverlayVisible(false);
      setNavDir('fade');
      setExitingSlide(null);
      prevAssetRef.current = null;
      if (exitingTimerRef.current !== null) {
        window.clearTimeout(exitingTimerRef.current);
        exitingTimerRef.current = null;
      }
      return;
    }

    const cached = getCachedDetail(displayedAssetId);
    const nextPreview = cached?.playback.preview_url ?? mediaPreviewUrl(displayedAssetId);
    prevAssetRef.current = { id: displayedAssetId, previewUrl: nextPreview };

    let active = true;
    setError(null);
    setMediaStateAssetId(displayedAssetId);
    setRawDemosaic(false);
    setLivePlaying(false);
    setLiveVideoReady(false);
    setAnimateZoom(false);
    setZoom(1);
    setPan(ZERO_PAN);
    setReadyOverlayUrl(null);
    setReadyOverlayAssetId(null);
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

    void getAssetDetail(displayedAssetId)
      .then((res) => {
        if (!active) return;
        setCachedDetail(displayedAssetId, res);
        setDetail(res);
        prevAssetRef.current = {
          id: displayedAssetId,
          previewUrl: res.playback.preview_url ?? mediaPreviewUrl(displayedAssetId),
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
  }, [clearLongPress, displayedAssetId]);

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

  const activeDetail = detail?.summary.id === displayedAssetId ? detail : null;
  // Keep prior detail ONLY to stabilize non-conflicting toolbar layout during cold cache misses.
  const toolbarDetail = activeDetail ?? detail;
  const stackMembers = useMemo(
    () => activeDetail?.stack_members ?? toolbarDetail?.stack_members ?? [],
    [activeDetail?.stack_members, toolbarDetail?.stack_members],
  );
  const currentStackGroupId =
    activeDetail?.summary.stack_group_id ?? toolbarDetail?.summary.stack_group_id ?? null;

  // Preload sibling versions in the active stack so switching Edited <-> JPEG <-> RAW is instant.
  useEffect(() => {
    if (!displayedAssetId || stackMembers.length <= 1) return;
    let cancelled = false;
    for (const member of stackMembers) {
      if (member.asset_id === displayedAssetId) continue;
      const cached = getCachedDetail(member.asset_id);
      preloadPreviewUrl(cached?.playback.preview_url ?? mediaPreviewUrl(member.asset_id));
      if (!cached) {
        void getAssetDetail(member.asset_id)
          .then((res) => {
            if (cancelled) return;
            setCachedDetail(member.asset_id, res);
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
  }, [displayedAssetId, stackMembers]);

  const switchStackVersion = useCallback(
    (targetAssetId: string) => {
      if (!lightboxAssetId || targetAssetId === displayedAssetId) return;
      // Instant Before/After swap in place (no directional slide animation).
      setNavDir('fade');
      setExitingSlide(null);
      if (exitingTimerRef.current !== null) {
        window.clearTimeout(exitingTimerRef.current);
        exitingTimerRef.current = null;
      }
      setActiveVersionId(targetAssetId === lightboxAssetId ? null : targetAssetId);
    },
    [displayedAssetId, lightboxAssetId],
  );

  const toggleBeforeAfterVersion = useCallback(() => {
    if (stackMembers.length <= 1 || !displayedAssetId) return;
    const coverMember = stackMembers.find((m) => m.is_cover) ?? stackMembers[0];
    const compareMember =
      stackMembers.find((m) => (m.stack_role === 'raw' || m.is_raw) && m.asset_id !== coverMember.asset_id) ??
      stackMembers.find((m) => m.asset_id !== coverMember.asset_id);
    if (!compareMember) return;
    const nextId =
      displayedAssetId === coverMember.asset_id ? compareMember.asset_id : coverMember.asset_id;
    switchStackVersion(nextId);
  }, [displayedAssetId, stackMembers, switchStackVersion]);

  const cycleStackVersion = useCallback(() => {
    if (stackMembers.length <= 1 || !displayedAssetId) return;
    const idx = stackMembers.findIndex((m) => m.asset_id === displayedAssetId);
    const next = stackMembers[(idx + 1) % stackMembers.length];
    if (next) switchStackVersion(next.asset_id);
  }, [displayedAssetId, stackMembers, switchStackVersion]);

  const handleSetAsStackCover = useCallback(async () => {
    if (!canMutate || !currentStackGroupId || !displayedAssetId) return;
    try {
      await setStackCover(currentStackGroupId, displayedAssetId);
      for (const m of stackMembers) {
        detailCache.delete(m.asset_id);
      }
      const refreshed = await getAssetDetail(displayedAssetId);
      setCachedDetail(displayedAssetId, refreshed);
      setDetail(refreshed);
      bumpLibraryRevision();
      notifySuccess('Updated stack cover');
    } catch (err) {
      notifyError(err, 'Could not set stack cover');
    }
  }, [bumpLibraryRevision, canMutate, currentStackGroupId, displayedAssetId, stackMembers]);

  const handleRemoveFromStack = useCallback(async () => {
    if (!canMutate || !currentStackGroupId || !displayedAssetId) return;
    try {
      await removeFromStack(currentStackGroupId, displayedAssetId);
      for (const m of stackMembers) {
        detailCache.delete(m.asset_id);
      }
      const refreshed = await getAssetDetail(displayedAssetId);
      setCachedDetail(displayedAssetId, refreshed);
      setDetail(refreshed);
      bumpLibraryRevision();
      notifySuccess('Removed photo from stack');
    } catch (err) {
      notifyError(err, 'Could not remove from stack');
    }
  }, [bumpLibraryRevision, canMutate, currentStackGroupId, displayedAssetId, stackMembers]);

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
    if (!canMutate || !displayedAssetId || !activeDetail) return;
    const nextFav = !activeDetail.summary.is_favorite;
    const optimistic: AssetDetail = {
      ...activeDetail,
      summary: { ...activeDetail.summary, is_favorite: nextFav },
    };
    setCachedDetail(displayedAssetId, optimistic);
    setDetail(optimistic);
    try {
      await setAssetFavorite(displayedAssetId, nextFav);
      bumpLibraryRevision();
    } catch (err) {
      setCachedDetail(displayedAssetId, activeDetail);
      setDetail((prev) => (prev && prev.summary.id === displayedAssetId ? activeDetail : prev));
      notifyError(err, 'Could not update favorite');
    }
  }, [activeDetail, bumpLibraryRevision, canMutate, displayedAssetId]);

  const handleMoveToTrash = useCallback(async () => {
    if (!canMutate || !displayedAssetId || trashing) return;
    const trashedId = displayedAssetId;
    setTrashing(true);
    try {
      await trashAssets([trashedId]);
      detailCache.delete(trashedId);
      for (const m of stackMembers) {
        detailCache.delete(m.asset_id);
      }
      bumpLibraryRevision();
      notifySuccess('Moved item to Trash');
      // If user trashed one version of a multi-version stack, switch to another surviving version.
      const remainingSibling = stackMembers.find((m) => m.asset_id !== trashedId);
      if (remainingSibling) {
        switchStackVersion(remainingSibling.asset_id);
      } else if (lightboxAssetId) {
        removeLightboxAsset(lightboxAssetId);
      }
    } catch (err) {
      notifyError(err, 'Could not move item to Trash');
    } finally {
      setTrashing(false);
    }
  }, [
    bumpLibraryRevision,
    canMutate,
    displayedAssetId,
    lightboxAssetId,
    removeLightboxAsset,
    stackMembers,
    switchStackVersion,
    trashing,
  ]);

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

      // Never steal shortcuts while a context menu or confirmation modal is open above Lightbox
      if (useContextMenuStore.getState().open || useFeedback.getState().confirm) {
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
        case '\\':
          if (stackMembers.length > 1) {
            event.preventDefault();
            toggleBeforeAfterVersion();
          }
          break;
        case 'v':
        case 'V':
          if (stackMembers.length > 1) {
            event.preventDefault();
            cycleStackVersion();
          }
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
              setPan(ZERO_PAN);
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
          setPan(ZERO_PAN);
          break;
        case ' ': {
          // Let focused buttons, links, and native <video controls> handle Space natively without double-toggling
          if (
            target &&
            (target.tagName === 'BUTTON' || target.tagName === 'A' || target.tagName === 'VIDEO')
          ) {
            break;
          }
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
    cycleStackVersion,
    goNext,
    goPrev,
    handleToggleFavorite,
    lightboxAssetId,
    lightboxAssetIds,
    navigateWithSlide,
    stackMembers.length,
    toggleBeforeAfterVersion,
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
    if (!displayedAssetId || isVideo) return null;
    if (rawDemosaic) return mediaOriginalUrl(displayedAssetId, { mode: 'raw_demosaic' });
    if (browserNativeOriginal) return mediaOriginalUrl(displayedAssetId);
    if (
      hasRaw ||
      primaryExt === 'heic' ||
      primaryExt === 'heif' ||
      primaryExt === 'tif' ||
      primaryExt === 'tiff' ||
      primaryExt === 'jxl'
    ) {
      return mediaOriginalUrl(displayedAssetId, { mode: 'raw_demosaic' });
    }
    return mediaOriginalUrl(displayedAssetId);
  }, [browserNativeOriginal, displayedAssetId, hasRaw, isVideo, primaryExt, rawDemosaic]);

  // Zero-flash stacked hi-res / RAW overlay:
  // Keeps the base preview <img> untouched underneath; mounts the overlay <img> at opacity 0
  // and fades it in smoothly ONLY after load + decode(). Stays mounted on zoom-out so zooming
  // in and out never swaps src or flashes!
  const wantsOverlay = Boolean(
    displayedAssetId && !isVideo && fullResUrl && (rawDemosaic || zoom > 1.02),
  );

  useEffect(() => {
    if (!displayedAssetId || isVideo || !fullResUrl) return;
    if (!wantsOverlay) {
      // Only fade out RAW 100% overlay when explicitly toggling a RAW+JPG stack back to Preview at 1x
      // (keep decoded hi-res overlay mounted for standalone HEIC/TIFF/RAW zoomed back out to 1x).
      if (
        !rawDemosaic &&
        activeDetail?.summary.kind === 'raw_jpg_stack' &&
        readyOverlayUrl?.includes('mode=raw_demosaic')
      ) {
        setOverlayVisible(false);
      }
      setHiResLoading(false);
      return;
    }

    if (readyOverlayUrl !== fullResUrl || readyOverlayAssetId !== displayedAssetId) {
      setReadyOverlayUrl(fullResUrl);
      setReadyOverlayAssetId(displayedAssetId);
    }
    if (decodedUrlSet.has(fullResUrl)) {
      setOverlayVisible(true);
      setHiResLoading(false);
    } else {
      setOverlayVisible(false);
      setHiResLoading(true);
    }
  }, [
    activeDetail?.summary.kind,
    displayedAssetId,
    fullResUrl,
    isVideo,
    rawDemosaic,
    readyOverlayAssetId,
    readyOverlayUrl,
    wantsOverlay,
  ]);

  // Dismiss thumbnail underlay after preview fade completes, even if onTransitionEnd doesn't fire
  // (e.g., prefers-reduced-motion or synchronous HTTP cache hits).
  const activePreviewUrl = displayedAssetId
    ? activeDetail?.playback.preview_url ?? mediaPreviewUrl(displayedAssetId)
    : null;
  const activePreviewReady = Boolean(
    activePreviewUrl &&
      (previewLoadedUrl === activePreviewUrl || decodedUrlSet.has(activePreviewUrl)),
  );
  useEffect(() => {
    if (!activePreviewUrl || !activePreviewReady || underlayDismissedUrl === activePreviewUrl) {
      return;
    }
    const t = window.setTimeout(() => {
      setUnderlayDismissedUrl(activePreviewUrl);
    }, 220);
    return () => window.clearTimeout(t);
  }, [activePreviewReady, activePreviewUrl, underlayDismissedUrl]);

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

  // Re-clamp pan if desktop info drawer toggles open/closed while zoomed
  useEffect(() => {
    if (!lightboxAssetId || isMobileViewport || zoomRef.current <= 1.02) return;
    const t = window.setTimeout(() => {
      setPan((p) => clampPan(p.x, p.y, zoomRef.current));
    }, INFO_SHEET_EXIT_MS + 20);
    return () => window.clearTimeout(t);
  }, [clampPan, infoOpen, isMobileViewport, lightboxAssetId]);

  if (!lightboxAssetId || !displayedAssetId) return null;

  const titleDetail = activeDetail ?? toolbarDetail;
  const titlePrimaryFile =
    titleDetail?.files.find((f) => f.file_role === 'primary') ?? titleDetail?.files[0];
  const titleText =
    titleDetail?.overrides?.title ||
    titleDetail?.summary.title ||
    titlePrimaryFile?.rel_path.split(/[\\/]/).pop() ||
    '\u00A0';

  const previewUrl = activeDetail?.playback.preview_url ?? mediaPreviewUrl(displayedAssetId);
  const thumbUrl = mediaThumbUrl(displayedAssetId);
  const previewIsReady = previewLoadedUrl === previewUrl || decodedUrlSet.has(previewUrl);
  const showUnderlay = !previewIsReady || underlayDismissedUrl !== previewUrl;
  const effectiveZoom = mediaStateAssetId === displayedAssetId ? zoom : 1;
  const effectivePan = mediaStateAssetId === displayedAssetId ? pan : ZERO_PAN;
  const activeOverlayUrl = readyOverlayAssetId === displayedAssetId ? readyOverlayUrl : null;
  const usingHiRes = Boolean(
    activeOverlayUrl && overlayVisible && (rawDemosaic || effectiveZoom > 1.02),
  );
  const liveVideoUrl = activeDetail?.playback.live_video_url ?? mediaLiveVideoUrl(displayedAssetId);
  const currentStackMember = stackMembers.find((m) => m.asset_id === displayedAssetId) ?? null;

  const zoomAtPoint = (clientX: number, clientY: number, targetZoom: number) => {
    setAnimateZoom(true);
    const rect = imgRef.current?.getBoundingClientRect();
    if (rect && rect.width > 0 && rect.height > 0) {
      const cx = rect.left + rect.width / 2;
      const cy = rect.top + rect.height / 2;
      const offsetX = clientX - cx;
      const offsetY = clientY - cy;
      setPan(
        clampPan(
          Math.round(-offsetX * (targetZoom - 1) * 0.75),
          Math.round(-offsetY * (targetZoom - 1) * 0.75),
          targetZoom,
        ),
      );
    } else {
      setPan(ZERO_PAN);
    }
    setZoom(targetZoom);
  };

  const handleTouchStart = (e: React.TouchEvent<HTMLDivElement>) => {
    lastTouchAtRef.current = Date.now();
    clearLongPress();
    gestureRef.current.ignoreTouch = false;

    const targetEl = e.target as HTMLElement | null;
    if (targetEl?.closest('button, a, input, select, textarea')) {
      gestureRef.current.ignoreTouch = true;
      gestureRef.current.lastTapAt = 0;
      return;
    }

    // For videos, allow horizontal swipe-left/right and swipe-down away from the bottom native
    // transport controls, while skipping pinch/double-tap zoom.
    if (isVideo) {
      if (e.touches.length !== 1) {
        gestureRef.current.ignoreTouch = true;
        return;
      }
      const t = e.touches[0];
      const videoEl = targetEl?.closest('video');
      if (videoEl) {
        const rect = videoEl.getBoundingClientRect();
        // Bottom 60px hosts native seek scrubber & play/fullscreen controls
        if (rect.height > 0 && t.clientY > rect.bottom - 60) {
          gestureRef.current.ignoreTouch = true;
          return;
        }
      }
      gestureRef.current.startX = t.clientX;
      gestureRef.current.startY = t.clientY;
      gestureRef.current.pinchStartDist = null;
      gestureRef.current.wasPinching = false;
      gestureRef.current.lastTapAt = 0;
      return;
    }

    if (e.touches.length === 2) {
      setAnimateZoom(false);
      gestureRef.current.lastTapAt = 0;
      gestureRef.current.pinchStartDist = touchDistance(e.touches[0], e.touches[1]);
      gestureRef.current.pinchStartZoom = zoom;
      gestureRef.current.wasPinching = true;
      return;
    }
    if (e.touches.length === 1) {
      const t = e.touches[0];
      const now = Date.now();
      const tapDist = Math.hypot(
        t.clientX - gestureRef.current.lastTapX,
        t.clientY - gestureRef.current.lastTapY,
      );
      if (now - gestureRef.current.lastTapAt < 300 && tapDist < 40) {
        gestureRef.current.lastTapAt = 0;
        if (zoom > 1.02) {
          setAnimateZoom(true);
          setZoom(1);
          setPan(ZERO_PAN);
        } else {
          zoomAtPoint(t.clientX, t.clientY, 2.25);
        }
        return;
      }
      gestureRef.current.lastTapAt = now;
      gestureRef.current.lastTapX = t.clientX;
      gestureRef.current.lastTapY = t.clientY;
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
    if (gestureRef.current.ignoreTouch) return;
    if (e.touches.length === 1) {
      const t = e.touches[0];
      const moved = Math.hypot(
        t.clientX - gestureRef.current.startX,
        t.clientY - gestureRef.current.startY,
      );
      if (moved > 10) {
        clearLongPress();
        gestureRef.current.lastTapAt = 0;
      }
    } else {
      clearLongPress();
      gestureRef.current.lastTapAt = 0;
    }
    if (isVideo) return;
    if (e.touches.length === 2 && gestureRef.current.pinchStartDist) {
      setAnimateZoom(false);
      const dist = touchDistance(e.touches[0], e.touches[1]);
      const scaleFactor = dist / gestureRef.current.pinchStartDist;
      const nextZoom = Math.min(4, Math.max(1, gestureRef.current.pinchStartZoom * scaleFactor));
      setZoom(nextZoom);
      if (nextZoom <= 1.02) {
        setPan(ZERO_PAN);
      } else {
        setPan((p) => clampPan(p.x, p.y, nextZoom));
      }
      return;
    }
    if (e.touches.length === 1 && zoom > 1.02 && !gestureRef.current.wasPinching) {
      setAnimateZoom(false);
      const t = e.touches[0];
      const dx = t.clientX - gestureRef.current.startX;
      const dy = t.clientY - gestureRef.current.startY;
      setPan(clampPan(gestureRef.current.startPanX + dx, gestureRef.current.startPanY + dy, zoom));
    }
  };

  const handleTouchEnd = (e: React.TouchEvent<HTMLDivElement>) => {
    lastTouchAtRef.current = Date.now();
    clearLongPress();
    if (gestureRef.current.ignoreTouch) {
      if (e.touches.length === 0) gestureRef.current.ignoreTouch = false;
      return;
    }
    if (touchLongPressLiveRef.current) {
      touchLongPressLiveRef.current = false;
      setLivePlaying(false);
      setLiveVideoReady(false);
      return;
    }
    if (gestureRef.current.pinchStartDist !== null || gestureRef.current.wasPinching) {
      gestureRef.current.pinchStartDist = null;
      if (e.touches.length === 1) {
        // Re-anchor remaining finger so lifting 1 finger after pinch never jumps pan
        const remaining = e.touches[0];
        gestureRef.current.startX = remaining.clientX;
        gestureRef.current.startY = remaining.clientY;
        gestureRef.current.startPanX = panRef.current.x;
        gestureRef.current.startPanY = panRef.current.y;
      }
      if (e.touches.length === 0) {
        gestureRef.current.wasPinching = false;
      }
      return;
    }
    if ((isVideo || zoom <= 1.02) && e.changedTouches.length === 1) {
      const t = e.changedTouches[0];
      const dx = t.clientX - gestureRef.current.startX;
      const dy = t.clientY - gestureRef.current.startY;
      const minRatio = isVideo ? 1.4 : 1.2;
      if (Math.abs(dx) > 55 && Math.abs(dx) > Math.abs(dy) * minRatio) {
        gestureRef.current.lastTapAt = 0;
        if (dx > 0) goPrev();
        else goNext();
      } else if (!infoOpen && dy > 92 && dy > Math.abs(dx) * 1.4) {
        gestureRef.current.lastTapAt = 0;
        closeLightbox();
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
    // On touch devices, ignore synthetic click events after taps so single-tap never fights double-tap zoom
    if (Date.now() - lastTouchAtRef.current < 550) {
      return;
    }
    if (gestureRef.current.didDrag) {
      gestureRef.current.didDrag = false;
      return;
    }
    setAnimateZoom(true);
    if (zoom > 1.02) {
      setZoom(1);
      setPan(ZERO_PAN);
      return;
    }
    zoomAtPoint(e.clientX, e.clientY, 2.25);
  };

  const handleWebDownloadClick = () => {
    if (!isTauri()) {
      notifyInfo('Starting download…');
    }
  };

  const handleNativeShare = async () => {
    if (!displayedAssetId || sharing) return;
    const targetAssetId = displayedAssetId;
    const knownSize = titlePrimaryFile?.file_size ?? 0;
    // Avoid buffering huge videos or RAW files (>25 MB) into mobile JS memory / expiring user activation
    if (isVideo || knownSize > 25 * 1024 * 1024) {
      triggerAssetDownload(targetAssetId);
      notifyInfo('Starting download…');
      return;
    }
    const controller = new AbortController();
    shareAbortRef.current?.abort();
    shareAbortRef.current = controller;
    setSharing(true);
    try {
      const url = mediaOriginalUrl(targetAssetId);
      const res = await fetch(url, { credentials: 'include', signal: controller.signal });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const blob = await res.blob();
      if (controller.signal.aborted || activeAssetIdRef.current !== targetAssetId) return;
      const rawName =
        titlePrimaryFile?.rel_path.split(/[\\/]/).pop() ||
        `${targetAssetId}.${primaryExt || 'jpg'}`;
      const file = new File([blob], rawName, { type: blob.type || 'image/jpeg' });
      if (navigator.canShare && navigator.canShare({ files: [file] })) {
        await navigator.share({ files: [file], title: rawName });
      } else {
        triggerAssetDownload(targetAssetId);
        notifyInfo('Starting download…');
      }
    } catch (err) {
      if (
        !controller.signal.aborted &&
        activeAssetIdRef.current === targetAssetId &&
        (err as { name?: string })?.name !== 'AbortError'
      ) {
        triggerAssetDownload(targetAssetId);
        notifyInfo('Starting download…');
      }
    } finally {
      if (shareAbortRef.current === controller) {
        shareAbortRef.current = null;
      }
      setSharing(false);
    }
  };

  const handleSheetTouchStart = (e: React.TouchEvent<HTMLDivElement>) => {
    if (!isMobileViewport || e.touches.length !== 1) return;
    if ((e.target as HTMLElement | null)?.closest('button, a, input, select, textarea')) return;
    sheetDragStartYRef.current = e.touches[0].clientY;
    sheetDragAtRef.current = Date.now();
    setSheetDragging(true);
  };

  const handleSheetTouchMove = (e: React.TouchEvent<HTMLDivElement>) => {
    if (sheetDragStartYRef.current === null || e.touches.length !== 1) return;
    const dy = Math.max(0, e.touches[0].clientY - sheetDragStartYRef.current);
    setSheetDragY(dy);
  };

  const handleSheetTouchEnd = () => {
    if (sheetDragStartYRef.current === null) return;
    const elapsed = Math.max(1, Date.now() - sheetDragAtRef.current);
    const velocity = sheetDragY / elapsed;
    const shouldClose = sheetDragY > 72 || (sheetDragY > 28 && velocity > 0.45);
    sheetDragStartYRef.current = null;
    setSheetDragging(false);
    setSheetDragY(0);
    if (shouldClose) {
      setInfoOpen(false);
    }
  };

  const handleMediaContextMenu = (e: React.MouseEvent) => {
    if (!displayedAssetId) return;
    openContextMenu(e, [
      ...(stackMembers.length > 1
        ? [
            {
              id: 'lb-compare',
              label: 'Toggle Before / After Version',
              icon: <Layers size={14} />,
              shortcut: '\\',
              onSelect: () => toggleBeforeAfterVersion(),
            },
            ...(canMutate && currentStackGroupId && currentStackMember && !currentStackMember.is_cover
              ? [
                  {
                    id: 'lb-set-cover',
                    label: 'Set This Version as Cover',
                    icon: <Check size={14} />,
                    onSelect: () => void handleSetAsStackCover(),
                  },
                ]
              : []),
            ...(canMutate && currentStackGroupId
              ? [
                  {
                    id: 'lb-unstack',
                    label: 'Remove Version from Stack',
                    icon: <Unlink size={14} />,
                    onSelect: () => void handleRemoveFromStack(),
                  },
                ]
              : []),
            { type: 'separator' as const, id: 'lb-sep-stack' },
          ]
        : []),
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
          triggerAssetDownload(displayedAssetId);
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
  const nativeShareAvailable = canUseNativeWebShare();

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
          style={{ gap: 10, minWidth: 0, flex: '1 1 0%' }}
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

        <div className="row lightbox-toolbar-actions">
          {stackMembers.length > 1 && !isVideo && (
            <div
              className="lightbox-version-switcher"
              role="group"
              aria-label="Photo stack versions"
            >
              {stackMembers.map((member) => {
                const isSelected = member.asset_id === displayedAssetId;
                const label = formatStackVersionLabel(member, stackMembers);
                const tip = `${label}${member.is_cover ? ' (Cover)' : ''} · ${member.folder_name}/${member.rel_path}${member.software ? ` · ${member.software}` : ''} (\\ to compare)`;
                return (
                  <button
                    key={member.asset_id}
                    type="button"
                    className={`lightbox-version-btn${isSelected ? ' is-active' : ''}`}
                    aria-pressed={isSelected}
                    title={tip}
                    onClick={() => switchStackVersion(member.asset_id)}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}

          {hasRaw && !isVideo && (
            <div className="row" style={{ gap: 4 }} role="group" aria-label="RAW tier toggle">
              <button
                type="button"
                className={`lightbox-pill-btn${!rawDemosaic ? ' is-active' : ''}`}
                aria-pressed={!rawDemosaic}
                onClick={() => setRawDemosaic(false)}
              >
                {isMobileViewport ? 'JPG' : 'Preview'}
              </button>
              <button
                type="button"
                className={`lightbox-pill-btn${rawDemosaic ? ' is-active' : ''}`}
                aria-pressed={rawDemosaic}
                onClick={() => setRawDemosaic(true)}
              >
                {isMobileViewport ? 'RAW' : 'RAW 100%'}
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

          {!isMobileViewport && (
            <>
              {!isVideo && (
                <button
                  type="button"
                  className={`lightbox-pill-btn lightbox-zoom-btn${effectiveZoom > 1.02 ? ' is-active' : ''}`}
                  aria-pressed={effectiveZoom > 1.02}
                  title={usingHiRes ? 'Viewing full original resolution' : 'Click photo or button to zoom'}
                  onClick={togglePixelPeepZoom}
                >
                  <ZoomIn size={13} />
                  <span>{effectiveZoom > 1.02 ? `${effectiveZoom.toFixed(1)}×` : '100%'}</span>
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
                href={mediaOriginalUrl(displayedAssetId, { download: true })}
                download
                aria-label="Download original"
                title="Download original"
                onClick={handleWebDownloadClick}
              >
                <Download size={16} />
              </a>

              {canMutate && (
                <button
                  type="button"
                  className="icon-button"
                  disabled={trashing}
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
            </>
          )}
        </div>
        {showWindowsCaption && <WindowsCaptionButtons />}
      </header>

      <div className="lightbox-body">
        <div
          ref={stageRef}
          className={`lightbox-stage${isVideo ? ' is-video' : ''}`}
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
            <div key={displayedAssetId} className={`lightbox-media-frame is-nav-${navDir}`}>
              <VideoPlayer
                playback={activeDetail.playback}
                posterUrl={activeDetail.playback.preview_url ?? mediaThumbUrl(displayedAssetId)}
                initialTimestampSec={lightboxInitialTimestampSec}
                className="lightbox-media"
              />
            </div>
          ) : (
            <div key={displayedAssetId} className={`lightbox-media-frame is-nav-${navDir}`}>
              <div
                className="lightbox-zoom-surface"
                style={{
                  transform: `translate3d(${effectivePan.x}px, ${effectivePan.y}px, 0) scale(${effectiveZoom})`,
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
                  onError={() => {
                    // Keep the cached thumbnail underlay visible rather than hiding everything on preview error
                    setPreviewLoadedUrl(null);
                  }}
                  onTransitionEnd={(e) => {
                    if (e.propertyName === 'opacity' && previewIsReady) {
                      setUnderlayDismissedUrl(previewUrl);
                    }
                  }}
                  style={{
                    cursor: effectiveZoom > 1.02 ? (isDragging ? 'grabbing' : 'grab') : 'zoom-in',
                    opacity: previewIsReady ? 1 : 0,
                    transition: 'opacity 140ms ease-out',
                  }}
                />

                {/* Stacked decoded full-resolution / RAW 100% overlay: fades in seamlessly with zero flash */}
                {activeOverlayUrl && (
                  <img
                    data-testid="lightbox-hires-layer"
                    className="lightbox-media lightbox-media-overlay"
                    src={activeOverlayUrl}
                    alt=""
                    aria-hidden="true"
                    draggable={false}
                    decoding="async"
                    onLoad={(e) => {
                      const el = e.currentTarget;
                      const url = activeOverlayUrl;
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

        {/* Dimmed backdrop scrim for tablet right-drawer & mobile bottom sheet */}
        <div
          className={`lightbox-info-scrim${infoOpen ? ' is-open' : ''}`}
          aria-hidden="true"
          onClick={() => setInfoOpen(false)}
        />

        <aside
          ref={infoDrawerRef}
          className={`inspector-drawer lightbox-info-drawer${infoOpen ? ' is-open' : ''}${sheetDragging ? ' is-dragging' : ''}`}
          aria-label="Lightbox asset inspector"
          aria-hidden={!infoOpen}
          style={
            isMobileViewport && infoOpen && sheetDragY > 0
              ? { transform: `translate3d(0, ${sheetDragY}px, 0)` }
              : undefined
          }
        >
          <div
            className="lightbox-sheet-handle"
            onTouchStart={handleSheetTouchStart}
            onTouchMove={handleSheetTouchMove}
            onTouchEnd={handleSheetTouchEnd}
            onTouchCancel={handleSheetTouchEnd}
          >
            <span className="lightbox-sheet-grabber" />
          </div>
          <div
            className="inspector-heading"
            onTouchStart={handleSheetTouchStart}
            onTouchMove={handleSheetTouchMove}
            onTouchEnd={handleSheetTouchEnd}
            onTouchCancel={handleSheetTouchEnd}
          >
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
          <div className="lightbox-info-scroll">
            {infoRendered && (
              <>
                {stackMembers.length > 1 && (
                  <div className="lightbox-stack-inspector-card">
                    <div className="row" style={{ justifyContent: 'space-between', marginBottom: 8 }}>
                      <span className="row" style={{ gap: 6, fontSize: 12, fontWeight: 600 }}>
                        <Layers size={13} />
                        <span>Stacked Versions ({stackMembers.length})</span>
                      </span>
                      <span className="view-subtitle" style={{ marginTop: 0, fontSize: 11 }}>
                        Press \ to compare
                      </span>
                    </div>
                    <div className="stack" style={{ gap: 6 }}>
                      {stackMembers.map((member) => {
                        const isSelected = member.asset_id === displayedAssetId;
                        const label = formatStackVersionLabel(member, stackMembers);
                        return (
                          <button
                            key={member.asset_id}
                            type="button"
                            className={`lightbox-stack-member-row${isSelected ? ' is-active' : ''}`}
                            onClick={() => switchStackVersion(member.asset_id)}
                          >
                            <div style={{ minWidth: 0, textAlign: 'left' }}>
                              <div className="row" style={{ gap: 6 }}>
                                <strong style={{ fontSize: 12 }}>{label}</strong>
                                {member.is_cover && (
                                  <span className="lightbox-stack-cover-tag">Cover</span>
                                )}
                              </div>
                              <div
                                className="view-subtitle"
                                style={{
                                  marginTop: 1,
                                  fontSize: 11,
                                  overflow: 'hidden',
                                  textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap',
                                }}
                              >
                                {member.folder_name} / {member.filename}
                              </div>
                            </div>
                            <span className="view-subtitle" style={{ marginTop: 0, fontSize: 11, flexShrink: 0 }}>
                              {member.extension.toUpperCase()}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                    {canMutate && currentStackGroupId && (
                      <div className="row" style={{ gap: 6, marginTop: 8, flexWrap: 'wrap' }}>
                        {currentStackMember && !currentStackMember.is_cover && (
                          <button
                            type="button"
                            className="button"
                            style={{ fontSize: 11, padding: '4px 9px' }}
                            onClick={() => void handleSetAsStackCover()}
                          >
                            <Check size={12} />
                            Set as Cover
                          </button>
                        )}
                        <button
                          type="button"
                          className="button button-quiet"
                          style={{ fontSize: 11, padding: '4px 9px' }}
                          onClick={() => void handleRemoveFromStack()}
                        >
                          <Unlink size={12} />
                          Unstack
                        </button>
                      </div>
                    )}
                  </div>
                )}
                <AssetInspector
                  assetId={displayedAssetId}
                  initialDetail={activeDetail}
                  loadingInitialDetail={detailLoading}
                  onAssetUpdated={(updated) => {
                    setCachedDetail(updated.summary.id, updated);
                    setDetail(updated);
                  }}
                />
              </>
            )}
          </div>
        </aside>
      </div>

      {/* Dedicated mobile bottom action bar with 44x44px touch targets & safe-area inset */}
      {isMobileViewport && (
        <nav className="lightbox-mobile-bar" aria-label="Photo actions">
          {canMutate && (
            <button
              type="button"
              className={`lightbox-mobile-action${isFav ? ' is-fav' : ''}`}
              disabled={!activeDetail}
              aria-label={isFav ? 'Remove favorite' : 'Favorite'}
              aria-pressed={isFav}
              onClick={() => void handleToggleFavorite()}
            >
              <Star size={19} fill={isFav ? 'currentColor' : 'none'} />
              <span>Favorite</span>
            </button>
          )}

          {nativeShareAvailable ? (
            <button
              type="button"
              className="lightbox-mobile-action"
              disabled={sharing}
              aria-label="Share"
              onClick={() => void handleNativeShare()}
            >
              <Share2 size={19} />
              <span>{sharing ? 'Preparing…' : 'Share'}</span>
            </button>
          ) : (
            <a
              className="lightbox-mobile-action"
              href={mediaOriginalUrl(displayedAssetId, { download: true })}
              download
              aria-label="Download original"
              onClick={handleWebDownloadClick}
            >
              <Download size={19} />
              <span>Save</span>
            </a>
          )}

          <button
            type="button"
            className={`lightbox-mobile-action${infoOpen ? ' is-active' : ''}`}
            aria-label="Toggle info"
            aria-pressed={infoOpen}
            onClick={() => setInfoOpen((prev) => !prev)}
          >
            <Info size={19} />
            <span>Info</span>
          </button>

          {canMutate && (
            <button
              type="button"
              className="lightbox-mobile-action is-danger"
              disabled={trashing}
              aria-label="Move to Trash"
              onClick={() => void handleMoveToTrash()}
            >
              <Trash2 size={19} />
              <span>Trash</span>
            </button>
          )}
        </nav>
      )}
    </div>
  );
};
