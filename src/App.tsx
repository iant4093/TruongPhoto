import React, { useCallback, useEffect, useRef, useState } from 'react';
import { LogOut, Search, Upload, UploadCloud, X } from 'lucide-react';
import { ContextMenuHost, useContextMenuStore } from './components/ContextMenu';
import { FeedbackHost } from './components/Feedback';
import { InspectorDrawer } from './components/InspectorDrawer';
import { Lightbox } from './components/Lightbox';
import { MobileTabBar } from './components/MobileTabBar';
import { SearchResultsView } from './components/SearchResultsView';
import { Sidebar } from './components/Sidebar';
import { UploadModal } from './components/UploadModal';
import { Badge } from './components/ui';
import { WindowsCaptionButtons, isWindowsPlatform } from './components/WindowControls';
import { AlbumsView } from './components/views/AlbumsView';
import { ChatView } from './components/views/ChatView';
import { FavoritesView } from './components/views/FavoritesView';
import { MapView } from './components/views/MapView';
import { PeopleView } from './components/views/PeopleView';
import { PinGateView } from './components/views/PinGateView';
import { SettingsView } from './components/views/SettingsView';
import { SetupWizardView } from './components/views/SetupWizardView';
import { TimelineView } from './components/views/TimelineView';
import { TrashView } from './components/views/TrashView';
import { isTauri } from './lib/tauri';
import { notifyError, notifySuccess } from './store/feedback';
import { useAppStore } from './store/useAppStore';
import type { NavTab } from './types/api';

const nativeMacTitlebar =
  typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
const windowsCaption = !nativeMacTitlebar && isWindowsPlatform();

const MEDIA_GRID_TABS = new Set<NavTab>(['timeline', 'people', 'albums', 'favorites']);
const UPLOAD_TABS = new Set<NavTab>(['timeline', 'albums', 'favorites']);

const NAV_TITLES: Record<NavTab, string> = {
  timeline: 'Timeline',
  chat: 'Ask AI',
  people: 'People',
  albums: 'Albums',
  favorites: 'Favorites',
  map: 'Map',
  trash: 'Recently Deleted',
  settings: 'Settings',
};

const SearchOmnibar: React.FC<{
  inputRef: React.RefObject<HTMLInputElement | null>;
}> = ({ inputRef }) => {
  const searchQuery = useAppStore((s) => s.searchQuery);
  const searchActive = useAppStore((s) => s.searchActive);
  const setSearchQuery = useAppStore((s) => s.setSearchQuery);
  const runSearch = useAppStore((s) => s.runSearch);
  const clearSearch = useAppStore((s) => s.clearSearch);

  return (
    <form
      className="omnibar"
      onSubmit={(e) => {
        e.preventDefault();
        void runSearch({
          query: searchQuery.trim() || null,
          ocr_query: undefined,
          exclude_person_ids: undefined,
          has_people: undefined,
          min_faces: undefined,
          max_faces: undefined,
          lens_model: undefined,
          focal_length_min_mm: undefined,
          focal_length_max_mm: undefined,
          aperture_min_f: undefined,
          aperture_max_f: undefined,
          iso_min: undefined,
          iso_max: undefined,
          shutter_speed_min_sec: undefined,
          shutter_speed_max_sec: undefined,
          orientation: undefined,
          sort_by: undefined,
          exclude_geo_query: undefined,
          exclude_kind: undefined,
          offset: 0,
        }).catch((err) => notifyError(err, 'Search failed'));
      }}
    >
      <label className="omnibar-field" aria-label="Search library">
        <Search size={14} aria-hidden="true" />
        <input
          ref={inputRef}
          type="search"
          placeholder="Search library…"
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              if (searchActive || searchQuery) clearSearch();
              else inputRef.current?.blur();
            }
          }}
        />
        {(searchQuery || searchActive) && (
          <button
            type="button"
            className="icon-button"
            style={{ width: 22, height: 22 }}
            aria-label="Clear search"
            onClick={clearSearch}
          >
            <X size={13} />
          </button>
        )}
        <span className="omnibar-shortcut" aria-hidden="true">
          /
        </span>
      </label>
    </form>
  );
};

export const App: React.FC = () => {
  const bootstrapping = useAppStore((s) => s.bootstrapping);
  const bootstrapError = useAppStore((s) => s.bootstrapError);
  const initialized = useAppStore((s) => s.initialized);
  const authenticated = useAppStore((s) => s.authenticated);
  const user = useAppStore((s) => s.user);
  const activeNav = useAppStore((s) => s.activeNav);
  const inspectorOpen = useAppStore((s) => s.inspectorOpen);
  const searchActive = useAppStore((s) => s.searchActive);
  const uploadModalOpen = useAppStore((s) => s.uploadModalOpen);
  const lightboxAssetId = useAppStore((s) => s.lightboxAssetId);
  const wsConnected = useAppStore((s) => s.wsConnected);
  const activeJobCount = useAppStore((s) => Object.keys(s.activeJobs).length);

  const bootstrap = useAppStore((s) => s.bootstrap);
  const logout = useAppStore((s) => s.logout);
  const setActiveNav = useAppStore((s) => s.setActiveNav);
  const selectAsset = useAppStore((s) => s.selectAsset);
  const setInspectorOpen = useAppStore((s) => s.setInspectorOpen);
  const toggleInspector = useAppStore((s) => s.toggleInspector);
  const setUploadModalOpen = useAppStore((s) => s.setUploadModalOpen);

  const canUpload = user?.role === 'admin' || user?.role === 'contributor';
  const showSearchOmnibar = searchActive || MEDIA_GRID_TABS.has(activeNav);
  const showUploadControl = canUpload && (searchActive || UPLOAD_TABS.has(activeNav));
  const hideWorkspaceHeader = !searchActive && activeNav === 'chat';

  const [windowDragActive, setWindowDragActive] = useState(false);
  const [droppedFiles, setDroppedFiles] = useState<File[]>([]);
  const [isMobileViewport, setIsMobileViewport] = useState(() =>
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia('(max-width: 767px)').matches
      : false,
  );
  const dragDepthRef = useRef(0);

  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(max-width: 767px)');
    const onChange = (e: MediaQueryListEvent) => setIsMobileViewport(e.matches);
    setIsMobileViewport(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const wasInspectorOpen = useRef(false);
  const prevNavRef = useRef<NavTab>(activeNav);
  const lastNonChatNavRef = useRef<NavTab>('timeline');

  useEffect(() => {
    if (prevNavRef.current !== activeNav) {
      if (prevNavRef.current !== 'chat') {
        lastNonChatNavRef.current = prevNavRef.current;
      }
      prevNavRef.current = activeNav;
    }
  }, [activeNav]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  // Clear local UI state (dropped files, context menus) when signed out or session expires.
  useEffect(() => {
    if (!authenticated) {
      setDroppedFiles([]);
      setWindowDragActive(false);
      dragDepthRef.current = 0;
      useContextMenuStore.getState().close();
    }
  }, [authenticated]);

  // Surface a native toast when a desktop "Download Original" transfer finishes.
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | null = null;
    let active = true;
    void import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen<{ success: boolean; filename?: string | null; path?: string | null }>(
          'desktop-download-finished',
          (event) => {
            if (!active) return;
            const { success, filename } = event.payload ?? { success: false };
            if (success) {
              notifySuccess(
                filename ? `Saved ${filename} to Downloads` : 'Download saved to Downloads',
              );
            } else {
              notifyError('Could not complete file download.');
            }
          },
        ),
      )
      .then((fn) => {
        if (active) unlisten = fn;
        else fn();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // '/' focuses Omnibar when not typing in a control; Cmd/Ctrl+K jumps to Ask AI workspace.
  useEffect(() => {
    if (!authenticated) return;
    const onKeyDown = (event: KeyboardEvent) => {
      const state = useAppStore.getState();
      if (
        state.lightboxAssetId !== null ||
        state.uploadModalOpen ||
        document.querySelector('[aria-modal="true"]') !== null
      ) {
        return;
      }
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        const cur = state.activeNav;
        if (cur === 'chat' && !state.searchActive) {
          setActiveNav(lastNonChatNavRef.current || 'timeline');
        } else {
          setActiveNav('chat');
        }
        return;
      }
      if (event.key !== '/' || event.metaKey || event.ctrlKey || event.altKey) return;
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
      if (!searchInputRef.current) return;
      event.preventDefault();
      searchInputRef.current.focus();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [authenticated, setActiveNav]);

  useEffect(() => {
    if (inspectorOpen && !wasInspectorOpen.current) {
      closeButtonRef.current?.focus();
    }
    wasInspectorOpen.current = inspectorOpen;
  }, [inspectorOpen]);

  const closeInspector = useCallback(() => {
    setInspectorOpen(false);
    selectAsset(null);
    const opener = openerRef.current;
    if (opener?.isConnected) opener.focus();
  }, [selectAsset, setInspectorOpen]);

  const handleCloseUploadModal = useCallback(() => {
    setUploadModalOpen(false);
    setDroppedFiles([]);
  }, [setUploadModalOpen]);

  const macDesktop = isTauri() && nativeMacTitlebar;
  const winDesktop = isTauri() && windowsCaption;

  if (bootstrapping) {
    return (
      <div
        className={`app-window-frame auth-viewport${macDesktop ? ' native-titlebar' : ''}${winDesktop ? ' windows-caption' : ''}`}
        data-tauri-drag-region
      >
        {winDesktop && (
          <div className="windows-auth-chrome" data-tauri-drag-region>
            <WindowsCaptionButtons />
          </div>
        )}
        <div className="auth-shell">
          <div className="auth-header" role="status">
            <img src="/app-icon.png" width={36} height={36} alt="" />
            <h1>TruongPhoto</h1>
            <p>Connecting to library server…</p>
          </div>
        </div>
      </div>
    );
  }

  if (bootstrapError && !authenticated) {
    return (
      <div
        className={`app-window-frame auth-viewport${macDesktop ? ' native-titlebar' : ''}${winDesktop ? ' windows-caption' : ''}`}
        data-tauri-drag-region
      >
        {winDesktop && (
          <div className="windows-auth-chrome" data-tauri-drag-region>
            <WindowsCaptionButtons />
          </div>
        )}
        <div className="auth-shell">
          <div className="auth-header">
            <img src="/app-icon.png" width={36} height={36} alt="" />
            <h1>TruongPhoto</h1>
            <p>Unable to reach the local server.</p>
          </div>
          <div className="card card-body stack">
            <div className="error-banner" role="alert" style={{ marginBottom: 4 }}>
              {bootstrapError}
            </div>
            <button
              type="button"
              className="button button-primary"
              onClick={() => void bootstrap()}
            >
              Retry Connection
            </button>
          </div>
        </div>
      </div>
    );
  }

  if (!initialized) {
    return (
      <>
        <SetupWizardView />
        <FeedbackHost />
      </>
    );
  }

  if (!authenticated) {
    return (
      <>
        <PinGateView />
        <FeedbackHost />
      </>
    );
  }

  const renderActiveView = () => {
    switch (activeNav) {
      case 'timeline':
        return <TimelineView />;
      case 'chat':
        return <ChatView />;
      case 'people':
        return <PeopleView />;
      case 'albums':
        return <AlbumsView />;
      case 'favorites':
        return <FavoritesView />;
      case 'map':
        return <MapView />;
      case 'trash':
        return <TrashView />;
      case 'settings':
        return <SettingsView />;
    }
  };

  const hasFileDrag = (e: React.DragEvent): boolean =>
    Array.from(e.dataTransfer?.types ?? []).includes('Files');

  return (
    <div
      className={`app-window-frame studio-shell${macDesktop ? ' native-titlebar' : ''}${isTauri() && windowsCaption ? ' windows-caption' : ''}`}
      onDragEnter={(e) => {
        if (!canUpload || uploadModalOpen || !hasFileDrag(e)) return;
        dragDepthRef.current += 1;
        setWindowDragActive(true);
      }}
      onDragOver={(e) => {
        if (!canUpload || uploadModalOpen || !hasFileDrag(e)) return;
        e.preventDefault();
      }}
      onDragLeave={(e) => {
        if (!canUpload || uploadModalOpen || !hasFileDrag(e)) return;
        dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
        if (dragDepthRef.current === 0) {
          setWindowDragActive(false);
        }
      }}
      onDrop={(e) => {
        if (!canUpload || uploadModalOpen || !hasFileDrag(e)) return;
        e.preventDefault();
        dragDepthRef.current = 0;
        setWindowDragActive(false);
        const files = Array.from(e.dataTransfer.files ?? []);
        if (files.length > 0) {
          setDroppedFiles(files);
          setUploadModalOpen(true);
        }
      }}
    >
      <FeedbackHost floatingToasts={isMobileViewport || lightboxAssetId !== null} />
      <ContextMenuHost />
      <Lightbox />
      <UploadModal
        open={uploadModalOpen}
        initialFiles={droppedFiles}
        onClose={handleCloseUploadModal}
      />

      {windowDragActive && canUpload && (
        <div className="app-drop-overlay" aria-hidden="true">
          <UploadCloud size={42} strokeWidth={1.3} />
          <div style={{ fontSize: 16, fontWeight: 600 }}>Drop photos or videos to upload</div>
          <div className="view-subtitle" style={{ marginTop: 0 }}>
            Files stream directly into Managed Uploads
          </div>
        </div>
      )}

      <Sidebar showStatusBanner={!isMobileViewport && lightboxAssetId === null} />

      <div className="workspace">
        {!hideWorkspaceHeader && (
          <header className="workspace-header" data-tauri-drag-region>
            {showSearchOmnibar ? (
              <SearchOmnibar inputRef={searchInputRef} />
            ) : (
              <div className="workspace-header-title" data-tauri-drag-region>
                {NAV_TITLES[activeNav]}
              </div>
            )}

            <div className="workspace-controls">
              {user && (
                <div className="row header-secondary-control" style={{ gap: 6 }}>
                  <span className="workspace-label">{user.display_name}</span>
                  <Badge tone={user.role === 'admin' ? 'info' : 'muted'}>{user.role}</Badge>
                </div>
              )}

              {showUploadControl && (
                <button
                  type="button"
                  className="button button-primary"
                  onClick={() => {
                    setDroppedFiles([]);
                    setUploadModalOpen(true);
                  }}
                >
                  <Upload size={14} />
                  <span className="header-btn-label">Upload</span>
                </button>
              )}

              <button
                type="button"
                className="button button-quiet header-secondary-control"
                onClick={() => void logout()}
              >
                <LogOut size={14} />
                <span>Sign Out</span>
              </button>

              <button
                type="button"
                className={`button details-toggle status-pill-btn ${inspectorOpen ? 'is-active' : ''}`}
                aria-pressed={inspectorOpen}
                aria-label="Server Status"
                title="Server Status & Activity"
                onClick={(event) => {
                  openerRef.current = event.currentTarget;
                  if (!inspectorOpen) {
                    selectAsset(null);
                  }
                  toggleInspector();
                }}
              >
                <span
                  className={`status-live-dot${wsConnected ? ' is-online' : ' is-offline'}${activeJobCount > 0 ? ' is-busy' : ''}`}
                  aria-hidden="true"
                />
                <span className="header-btn-label">
                  {activeJobCount > 0 ? 'Indexing…' : 'Status'}
                </span>
              </button>
            </div>
            {winDesktop && <WindowsCaptionButtons />}
          </header>
        )}

        <div className="workspace-body">
          <main className="content-viewport">
            {searchActive ? (
              <div key="search" className="view-content is-full-bleed">
                <SearchResultsView />
              </div>
            ) : (
              <div
                key={activeNav}
                className={
                  activeNav === 'settings'
                    ? 'settings-enter'
                    : activeNav === 'chat'
                      ? 'chat-enter'
                      : 'view-content is-full-bleed'
                }
              >
                {renderActiveView()}
              </div>
            )}
          </main>

          {inspectorOpen && (
            <div
              className="status-drawer-scrim"
              aria-hidden="true"
              onClick={closeInspector}
            />
          )}

          <div
            className={`status-drawer-slot${inspectorOpen ? ' is-open' : ''}`}
            inert={!inspectorOpen}
            aria-hidden={!inspectorOpen}
          >
            <InspectorDrawer
              open={inspectorOpen}
              closeRef={closeButtonRef}
              onClose={closeInspector}
            />
          </div>
        </div>
      </div>

      <MobileTabBar />
    </div>
  );
};

export default App;
