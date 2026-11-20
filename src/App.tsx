import React, { useCallback, useEffect, useRef, useState } from 'react';
import { LogOut, PanelRight, Search, Sparkles, Upload, UploadCloud, X } from 'lucide-react';
import { ChatDrawer } from './components/ChatDrawer';
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
import { FavoritesView } from './components/views/FavoritesView';
import { MapView } from './components/views/MapView';
import { PeopleView } from './components/views/PeopleView';
import { PinGateView } from './components/views/PinGateView';
import { SettingsView } from './components/views/SettingsView';
import { SetupWizardView } from './components/views/SetupWizardView';
import { TimelineView } from './components/views/TimelineView';
import { TrashView } from './components/views/TrashView';
import { isTauri } from './lib/tauri';
import { notifyError } from './store/feedback';
import { useAppStore } from './store/useAppStore';

const nativeMacTitlebar =
  typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
const windowsCaption = !nativeMacTitlebar && isWindowsPlatform();

export const App: React.FC = () => {
  const bootstrapping = useAppStore((s) => s.bootstrapping);
  const bootstrapError = useAppStore((s) => s.bootstrapError);
  const initialized = useAppStore((s) => s.initialized);
  const authenticated = useAppStore((s) => s.authenticated);
  const user = useAppStore((s) => s.user);
  const activeNav = useAppStore((s) => s.activeNav);
  const inspectorOpen = useAppStore((s) => s.inspectorOpen);
  const searchQuery = useAppStore((s) => s.searchQuery);
  const searchActive = useAppStore((s) => s.searchActive);
  const uploadModalOpen = useAppStore((s) => s.uploadModalOpen);
  const chatDrawerOpen = useAppStore((s) => s.chatDrawerOpen);

  const bootstrap = useAppStore((s) => s.bootstrap);
  const logout = useAppStore((s) => s.logout);
  const setInspectorOpen = useAppStore((s) => s.setInspectorOpen);
  const toggleInspector = useAppStore((s) => s.toggleInspector);
  const toggleChatDrawer = useAppStore((s) => s.toggleChatDrawer);
  const setSearchQuery = useAppStore((s) => s.setSearchQuery);
  const runSearch = useAppStore((s) => s.runSearch);
  const clearSearch = useAppStore((s) => s.clearSearch);
  const setUploadModalOpen = useAppStore((s) => s.setUploadModalOpen);

  const canUpload = user?.role === 'admin' || user?.role === 'contributor';

  const [windowDragActive, setWindowDragActive] = useState(false);
  const [droppedFiles, setDroppedFiles] = useState<File[]>([]);
  const dragDepthRef = useRef(0);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const wasInspectorOpen = useRef(false);
  const prevNavRef = useRef(activeNav);

  useEffect(() => {
    if (prevNavRef.current !== activeNav) {
      prevNavRef.current = activeNav;
      if (searchActive) {
        clearSearch();
      }
    }
  }, [activeNav, clearSearch, searchActive]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  // '/' focuses Omnibar when not typing in a control; Cmd/Ctrl+K toggles Ask AI drawer.
  useEffect(() => {
    if (!authenticated) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        toggleChatDrawer();
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
      event.preventDefault();
      searchInputRef.current?.focus();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [authenticated, toggleChatDrawer]);

  useEffect(() => {
    if (inspectorOpen && !wasInspectorOpen.current) {
      closeButtonRef.current?.focus();
    }
    wasInspectorOpen.current = inspectorOpen;
  }, [inspectorOpen]);

  const closeInspector = useCallback(() => {
    setInspectorOpen(false);
    const opener = openerRef.current;
    if (opener?.isConnected) opener.focus();
  }, [setInspectorOpen]);

  const handleCloseUploadModal = useCallback(() => {
    setUploadModalOpen(false);
    setDroppedFiles([]);
  }, [setUploadModalOpen]);

  if (bootstrapping) {
    return (
      <div className="auth-viewport">
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
      <div className="auth-viewport">
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
      className={`app-window-frame studio-shell${isTauri() && nativeMacTitlebar ? ' native-titlebar' : ''}${isTauri() && windowsCaption ? ' windows-caption' : ''}`}
      onDragEnter={(e) => {
        if (!canUpload || !hasFileDrag(e)) return;
        dragDepthRef.current += 1;
        setWindowDragActive(true);
      }}
      onDragOver={(e) => {
        if (!canUpload || !hasFileDrag(e)) return;
        e.preventDefault();
      }}
      onDragLeave={(e) => {
        if (!canUpload || !hasFileDrag(e)) return;
        dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
        if (dragDepthRef.current === 0) {
          setWindowDragActive(false);
        }
      }}
      onDrop={(e) => {
        if (!canUpload || !hasFileDrag(e)) return;
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
      <FeedbackHost />
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

      <Sidebar />

      <div className="workspace">
        <header className="workspace-header" data-tauri-drag-region>
          <form
            className="omnibar"
            onSubmit={(e) => {
              e.preventDefault();
              void runSearch({ query: searchQuery.trim() || null, offset: 0 }).catch((err) =>
                notifyError(err, 'Search failed'),
              );
            }}
          >
            <label className="omnibar-field" aria-label="Search library">
              <Search size={14} aria-hidden="true" />
              <input
                ref={searchInputRef}
                type="search"
                placeholder="Search library…"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Escape') {
                    if (searchActive || searchQuery) clearSearch();
                    else searchInputRef.current?.blur();
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
            <button type="submit" className="button" aria-label="Search">
              Search
            </button>
          </form>

          <button
            type="button"
            className={`button details-toggle ${chatDrawerOpen ? 'is-active' : ''}`}
            aria-pressed={chatDrawerOpen}
            title="Ask AI (Cmd/Ctrl+K)"
            onClick={toggleChatDrawer}
          >
            <Sparkles size={14} />
            <span>Ask AI</span>
          </button>

          <div className="workspace-controls">
            {user && (
              <div className="row" style={{ gap: 6 }}>
                <span className="workspace-label">{user.display_name}</span>
                <Badge tone={user.role === 'admin' ? 'info' : 'muted'}>{user.role}</Badge>
              </div>
            )}

            {canUpload && (
              <button
                type="button"
                className="button button-primary"
                onClick={() => {
                  setDroppedFiles([]);
                  setUploadModalOpen(true);
                }}
              >
                <Upload size={14} />
                <span>Upload</span>
              </button>
            )}

            <button
              type="button"
              className="button button-quiet"
              onClick={() => void logout()}
            >
              <LogOut size={14} />
              <span>Sign Out</span>
            </button>

            <button
              type="button"
              className={`button details-toggle ${inspectorOpen ? 'is-active' : ''}`}
              aria-pressed={inspectorOpen}
              onClick={(event) => {
                openerRef.current = event.currentTarget;
                toggleInspector();
              }}
            >
              <PanelRight size={14} />
              <span>Inspector</span>
            </button>

            {isTauri() && windowsCaption && <WindowsCaptionButtons />}
          </div>
        </header>

        <div className={`workspace-body ${inspectorOpen ? 'with-inspector' : ''}`}>
          <main className="content-viewport">
            {searchActive ? (
              <div key="search" className="view-content is-full-bleed">
                <SearchResultsView />
              </div>
            ) : (
              <div
                key={activeNav}
                className={
                  activeNav === 'settings' ? 'settings-enter' : 'view-content is-full-bleed'
                }
              >
                {renderActiveView()}
              </div>
            )}
          </main>

          {inspectorOpen && (
            <div className="inspector-scrim" aria-hidden="true" onClick={closeInspector} />
          )}

          <div
            className="inspector-slot"
            inert={!inspectorOpen}
            aria-hidden={!inspectorOpen}
          >
            <InspectorDrawer
              open={inspectorOpen}
              closeRef={closeButtonRef}
              onClose={closeInspector}
            />
          </div>

          <ChatDrawer />
        </div>
      </div>

      <MobileTabBar />
    </div>
  );
};

export default App;
