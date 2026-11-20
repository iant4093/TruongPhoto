import { create } from 'zustand';
import {
  ApiClientError,
  WsClient,
  createChatSession,
  deleteChatSession,
  desktopBootstrap,
  getAdminSettings,
  getAiModelsDashboard,
  getAuthStatus,
  getChatSession,
  getMe,
  getSetupStatus,
  getSystemInfo,
  initializeSetup,
  listAdminUsers,
  listChatSessions,
  listFolders,
  loginWithProfilePin,
  logout as apiLogout,
  searchLibrary,
  sendChatMessage,
  setUnauthorizedHandler,
  verifyServerPin,
} from '../lib/api';
import { getDesktopBootstrap, isTauri } from '../lib/tauri';
import type {
  ActiveJobInfo,
  AdminUserView,
  AiModelsDashboardResponse,
  AppSettings,
  ChatMessageView,
  ChatSessionSummary,
  FolderView,
  NavTab,
  PowerState,
  ProfileSummary,
  RouterPlan,
  SearchRequest,
  SearchResponse,
  SendChatMessageResponse,
  SetupInitializeRequest,
  SetupStatusResponse,
  SystemInfoResponse,
  TailscaleStatus,
  ThrottleStatus,
  UserPublic,
  WsServerEvent,
} from '../types/api';

export interface LiveMetrics {
  uptime_secs: number;
  process_ram_bytes: number;
  db_size_bytes: number;
  active_jobs: number;
  updated_at_ms: number;
}

export interface AppState {
  activeNav: NavTab;
  inspectorOpen: boolean;
  searchQuery: string;

  bootstrapping: boolean;
  bootstrapError: string | null;
  initialized: boolean;
  authenticated: boolean;
  serverGatePassed: boolean;
  desktopPinLock: boolean;
  profiles: ProfileSummary[] | null;
  setupStatus: SetupStatusResponse | null;

  user: UserPublic | null;
  allowedFolderIds: string[] | null;
  systemInfo: SystemInfoResponse | null;
  folders: FolderView[];
  adminUsers: AdminUserView[];
  adminSettings: AppSettings | null;

  wsConnected: boolean;
  liveMetrics: LiveMetrics | null;
  powerState: PowerState | null;
  throttleStatus: ThrottleStatus | null;
  tailscaleStatus: TailscaleStatus | null;

  // Phase 1 UI & library sync state
  selectedAssetId: string | null;
  lightboxAssetId: string | null;
  lightboxAssetIds: string[];
  lightboxInitialTimestampSec: number | null;
  uploadModalOpen: boolean;
  activeJobs: Record<number, ActiveJobInfo>;
  libraryRevision: number;

  // Phase 2 Search & AI state
  searchActive: boolean;
  searchRequest: SearchRequest;
  searchResponse: SearchResponse | null;
  searchLoading: boolean;
  aiDashboard: AiModelsDashboardResponse | null;

  // Phase 3 Conversational Search state
  chatDrawerOpen: boolean;
  chatSessions: ChatSessionSummary[];
  activeChatSessionId: string | null;
  chatMessages: ChatMessageView[];
  chatActiveFilters: RouterPlan | null;
  chatSending: boolean;
  chatLastReranked: boolean;

  setActiveNav: (nav: NavTab) => void;
  toggleInspector: () => void;
  setInspectorOpen: (open: boolean) => void;
  setSearchQuery: (query: string) => void;

  selectAsset: (id: string | null) => void;
  openLightbox: (assetId: string, assetIds: string[], initialTimestampSec?: number | null) => void;
  closeLightbox: () => void;
  setLightboxAsset: (assetId: string) => void;
  setUploadModalOpen: (open: boolean) => void;
  bumpLibraryRevision: () => void;

  runSearch: (partial?: Partial<SearchRequest>) => Promise<void>;
  setSearchFilters: (partial: Partial<SearchRequest>) => void;
  clearSearch: () => void;
  refreshAiDashboard: () => Promise<void>;

  setChatDrawerOpen: (open: boolean) => void;
  toggleChatDrawer: () => void;
  loadChatSessions: () => Promise<void>;
  createNewChatSession: (title?: string) => Promise<ChatSessionSummary>;
  selectChatSession: (sessionId: string) => Promise<void>;
  removeChatSession: (sessionId: string) => Promise<void>;
  sendChatPrompt: (content: string, limit?: number) => Promise<SendChatMessageResponse>;

  bootstrap: () => Promise<void>;
  submitServerPin: (pin: string) => Promise<ProfileSummary[]>;
  submitProfileLogin: (
    userId: string,
    profilePin: string,
    serverPin?: string | null,
  ) => Promise<void>;
  completeSetup: (req: SetupInitializeRequest) => Promise<void>;
  logout: () => Promise<void>;

  refreshSystemInfo: () => Promise<void>;
  refreshFolders: () => Promise<void>;
  refreshAdminData: () => Promise<void>;

  connectWs: () => void;
  disconnectWs: () => void;
  applyWsEvent: (event: WsServerEvent) => void;
  handleUnauthorized: () => void;
  resetForTest: () => void;
}

const defaultSearchRequest = (): SearchRequest => ({
  mode: 'hybrid',
  limit: 60,
  offset: 0,
});

const initialDataState = {
  activeNav: 'timeline' as NavTab,
  inspectorOpen: false,
  searchQuery: '',
  bootstrapping: true,
  bootstrapError: null as string | null,
  initialized: false,
  authenticated: false,
  serverGatePassed: false,
  desktopPinLock: false,
  profiles: null as ProfileSummary[] | null,
  setupStatus: null as SetupStatusResponse | null,
  user: null as UserPublic | null,
  allowedFolderIds: null as string[] | null,
  systemInfo: null as SystemInfoResponse | null,
  folders: [] as FolderView[],
  adminUsers: [] as AdminUserView[],
  adminSettings: null as AppSettings | null,
  wsConnected: false,
  liveMetrics: null as LiveMetrics | null,
  powerState: null as PowerState | null,
  throttleStatus: null as ThrottleStatus | null,
  tailscaleStatus: null as TailscaleStatus | null,
  selectedAssetId: null as string | null,
  lightboxAssetId: null as string | null,
  lightboxAssetIds: [] as string[],
  lightboxInitialTimestampSec: null as number | null,
  uploadModalOpen: false,
  activeJobs: {} as Record<number, ActiveJobInfo>,
  libraryRevision: 0,
  searchActive: false,
  searchRequest: defaultSearchRequest(),
  searchResponse: null as SearchResponse | null,
  searchLoading: false,
  aiDashboard: null as AiModelsDashboardResponse | null,
  chatDrawerOpen: false,
  chatSessions: [] as ChatSessionSummary[],
  activeChatSessionId: null as string | null,
  chatMessages: [] as ChatMessageView[],
  chatActiveFilters: null as RouterPlan | null,
  chatSending: false,
  chatLastReranked: false,
};

let wsClient: WsClient | null = null;
let searchSeq = 0;
let chatSeq = 0;

export function errorMessage(err: unknown): string {
  if (err instanceof ApiClientError) return err.message;
  if (err instanceof Error) return err.message;
  return String(err);
}

export const useAppStore = create<AppState>((set, get) => {
  const loadAuthenticatedSession = async (): Promise<void> => {
    const [me, sysInfo, folders] = await Promise.all([
      getMe(),
      getSystemInfo(),
      listFolders(),
    ]);

    let adminUsers: AdminUserView[] = [];
    let adminSettings: AppSettings | null = null;
    if (me.user.role === 'admin') {
      [adminUsers, adminSettings] = await Promise.all([
        listAdminUsers(),
        getAdminSettings(),
      ]);
    }

    set({
      initialized: true,
      authenticated: true,
      serverGatePassed: true,
      profiles: null,
      setupStatus: null,
      desktopPinLock: sysInfo.desktop_pin_lock,
      user: me.user,
      allowedFolderIds: me.allowed_folder_ids,
      systemInfo: sysInfo,
      powerState: sysInfo.power,
      throttleStatus: sysInfo.throttle ?? null,
      folders,
      adminUsers,
      adminSettings,
      bootstrapError: null,
    });

    get().connectWs();
  };

  return {
    ...initialDataState,

    setActiveNav: (nav) => set({ activeNav: nav }),
    toggleInspector: () =>
      set((s) => ({
        inspectorOpen: !s.inspectorOpen,
        ...(!s.inspectorOpen ? { chatDrawerOpen: false } : {}),
      })),
    setInspectorOpen: (open) =>
      set((s) => ({
        inspectorOpen: open,
        ...(open ? { chatDrawerOpen: false } : { chatDrawerOpen: s.chatDrawerOpen }),
      })),
    setSearchQuery: (query) => set({ searchQuery: query }),

    selectAsset: (id) => set({ selectedAssetId: id }),
    openLightbox: (assetId, assetIds, initialTimestampSec = null) =>
      set({
        lightboxAssetId: assetId,
        lightboxAssetIds: assetIds,
        lightboxInitialTimestampSec: initialTimestampSec ?? null,
        selectedAssetId: assetId,
      }),
    closeLightbox: () =>
      set({
        lightboxAssetId: null,
        lightboxAssetIds: [],
        lightboxInitialTimestampSec: null,
      }),
    setLightboxAsset: (assetId) =>
      set({
        lightboxAssetId: assetId,
        lightboxInitialTimestampSec: null,
        selectedAssetId: assetId,
      }),
    setUploadModalOpen: (open) => set({ uploadModalOpen: open }),
    bumpLibraryRevision: () => set((s) => ({ libraryRevision: s.libraryRevision + 1 })),

    runSearch: async (partial) => {
      const prev = get().searchRequest;
      const merged: SearchRequest = {
        ...prev,
        ...partial,
      };
      if (merged.query === undefined) {
        const trimmed = get().searchQuery.trim();
        if (trimmed) {
          merged.query = trimmed;
        }
      }
      const nextQuery = merged.query ?? '';
      const seq = ++searchSeq;
      set({
        searchActive: true,
        searchLoading: true,
        searchRequest: merged,
        searchQuery: nextQuery,
      });

      try {
        const res = await searchLibrary(merged);
        if (seq === searchSeq) {
          set({ searchResponse: res, searchLoading: false });
        }
      } catch (err) {
        if (seq === searchSeq) {
          set({ searchLoading: false });
        }
        throw err;
      }
    },

    setSearchFilters: (partial) =>
      set((state) => ({
        searchRequest: {
          ...state.searchRequest,
          ...partial,
        },
        ...(partial && 'query' in partial ? { searchQuery: partial.query ?? '' } : {}),
      })),

    clearSearch: () => {
      searchSeq += 1;
      set({
        searchActive: false,
        searchQuery: '',
        searchRequest: defaultSearchRequest(),
        searchResponse: null,
        searchLoading: false,
      });
    },

    refreshAiDashboard: async () => {
      const aiDashboard = await getAiModelsDashboard();
      set({ aiDashboard });
    },

    setChatDrawerOpen: (open) =>
      set((s) => ({
        chatDrawerOpen: open,
        ...(open ? { inspectorOpen: false } : { inspectorOpen: s.inspectorOpen }),
      })),
    toggleChatDrawer: () =>
      set((s) => ({
        chatDrawerOpen: !s.chatDrawerOpen,
        ...(!s.chatDrawerOpen ? { inspectorOpen: false } : {}),
      })),

    loadChatSessions: async () => {
      const seq = chatSeq;
      const chatSessions = await listChatSessions();
      if (seq === chatSeq) {
        set({ chatSessions });
      }
    },

    createNewChatSession: async (title) => {
      const seq = ++chatSeq;
      const session = await createChatSession(title);
      if (seq === chatSeq) {
        set((state) => ({
          chatSessions: [session, ...state.chatSessions.filter((s) => s.id !== session.id)],
          activeChatSessionId: session.id,
          chatMessages: [],
          chatActiveFilters: null,
          chatLastReranked: false,
        }));
      }
      return session;
    },

    selectChatSession: async (sessionId) => {
      const seq = ++chatSeq;
      const detail = await getChatSession(sessionId);
      if (seq !== chatSeq) return;
      const lastAssistant = [...detail.messages]
        .reverse()
        .find((m) => m.role === 'assistant');
      set({
        activeChatSessionId: detail.session.id,
        chatMessages: detail.messages,
        chatActiveFilters: detail.session.active_filters,
        chatLastReranked: Boolean(lastAssistant?.reranked),
      });
    },

    removeChatSession: async (sessionId) => {
      await deleteChatSession(sessionId);
      set((state) => {
        const remaining = state.chatSessions.filter((s) => s.id !== sessionId);
        const isActive = state.activeChatSessionId === sessionId;
        return {
          chatSessions: remaining,
          ...(isActive
            ? {
                activeChatSessionId: null,
                chatMessages: [],
                chatActiveFilters: null,
                chatLastReranked: false,
              }
            : {}),
        };
      });
    },

    sendChatPrompt: async (content, limit) => {
      const seq = chatSeq;
      set({ chatSending: true });
      try {
        let sessionId = get().activeChatSessionId;
        if (!sessionId) {
          const derivedTitle = content.trim().slice(0, 48) || undefined;
          const created = await createChatSession(derivedTitle);
          if (seq !== chatSeq) {
            throw new Error('Chat session reset while sending message');
          }
          sessionId = created.id;
          set((state) => ({
            chatSessions: [created, ...state.chatSessions.filter((s) => s.id !== created.id)],
            activeChatSessionId: created.id,
            chatMessages: [],
            chatActiveFilters: null,
            chatLastReranked: false,
          }));
        }

        const res = await sendChatMessage(sessionId, content, limit);
        if (seq === chatSeq) {
          set((state) => ({
            chatMessages: [...state.chatMessages, res.user_message, res.assistant_message],
            chatActiveFilters: res.active_filters,
            chatLastReranked: res.reranked,
            chatSending: false,
            chatSessions: state.chatSessions.map((s) =>
              s.id === sessionId
                ? { ...s, updated_at_ms: res.assistant_message.created_at_ms }
                : s,
            ),
          }));
        }
        return res;
      } catch (err) {
        if (seq === chatSeq) {
          set({ chatSending: false });
        }
        throw err;
      }
    },

    bootstrap: async () => {
      setUnauthorizedHandler(() => {
        get().handleUnauthorized();
      });
      set({ bootstrapping: true, bootstrapError: null });

      try {
        const status = await getAuthStatus();
        if (!status.initialized) {
          const setupStatus = await getSetupStatus();
          set({
            initialized: false,
            authenticated: false,
            serverGatePassed: false,
            desktopPinLock: status.desktop_pin_lock,
            profiles: null,
            user: null,
            allowedFolderIds: null,
            setupStatus,
            bootstrapping: false,
          });
          return;
        }

        if (!status.authenticated) {
          if (isTauri() && !status.desktop_pin_lock) {
            try {
              const info = await getDesktopBootstrap();
              await desktopBootstrap(info.desktop_bootstrap_token);
              await loadAuthenticatedSession();
              set({ bootstrapping: false });
              return;
            } catch {
              // Fall through to PinGateView if desktop bootstrap fails or is locked
            }
          }

          set({
            initialized: true,
            authenticated: false,
            serverGatePassed: status.server_gate_passed,
            desktopPinLock: status.desktop_pin_lock,
            profiles: status.profiles,
            user: null,
            allowedFolderIds: null,
            setupStatus: null,
            bootstrapping: false,
          });
          return;
        }

        await loadAuthenticatedSession();
        set({ bootstrapping: false });
      } catch (err) {
        set({
          bootstrapping: false,
          bootstrapError: errorMessage(err),
        });
      }
    },

    submitServerPin: async (pin) => {
      const res = await verifyServerPin(pin);
      set({
        serverGatePassed: true,
        profiles: res.profiles,
      });
      return res.profiles;
    },

    submitProfileLogin: async (userId, profilePin, serverPin = null) => {
      await loginWithProfilePin({
        user_id: userId,
        profile_pin: profilePin,
        server_pin: serverPin,
      });
      await loadAuthenticatedSession();
    },

    completeSetup: async (req) => {
      await initializeSetup(req);
      await loadAuthenticatedSession();
    },

    logout: async () => {
      try {
        await apiLogout();
      } catch {
        // Ignore network errors during sign-out cleanup
      }
      get().disconnectWs();
      searchSeq += 1;
      chatSeq += 1;
      set({
        authenticated: false,
        serverGatePassed: false,
        profiles: null,
        user: null,
        allowedFolderIds: null,
        folders: [],
        adminUsers: [],
        adminSettings: null,
        wsConnected: false,
        liveMetrics: null,
        throttleStatus: null,
        tailscaleStatus: null,
        inspectorOpen: false,
        activeNav: 'timeline',
        searchQuery: '',
        selectedAssetId: null,
        lightboxAssetId: null,
        lightboxAssetIds: [],
        lightboxInitialTimestampSec: null,
        uploadModalOpen: false,
        activeJobs: {},
        libraryRevision: 0,
        searchActive: false,
        searchRequest: defaultSearchRequest(),
        searchResponse: null,
        searchLoading: false,
        aiDashboard: null,
        chatDrawerOpen: false,
        chatSessions: [],
        activeChatSessionId: null,
        chatMessages: [],
        chatActiveFilters: null,
        chatSending: false,
        chatLastReranked: false,
      });
    },

    refreshSystemInfo: async () => {
      const sysInfo = await getSystemInfo();
      set({
        systemInfo: sysInfo,
        desktopPinLock: sysInfo.desktop_pin_lock,
        powerState: sysInfo.power,
        throttleStatus: sysInfo.throttle ?? null,
      });
    },

    refreshFolders: async () => {
      const folders = await listFolders();
      set({ folders });
    },

    refreshAdminData: async () => {
      if (get().user?.role !== 'admin') return;
      const [adminUsers, adminSettings] = await Promise.all([
        listAdminUsers(),
        getAdminSettings(),
      ]);
      set({ adminUsers, adminSettings });
    },

    connectWs: () => {
      if (wsClient) {
        wsClient.connect();
        return;
      }
      wsClient = new WsClient({
        onEvent: (event) => {
          get().applyWsEvent(event);
        },
        onConnectionChange: (connected) => {
          set({ wsConnected: connected });
        },
      });
      wsClient.connect();
    },

    disconnectWs: () => {
      if (wsClient) {
        wsClient.disconnect();
        wsClient = null;
      }
      set({ wsConnected: false });
    },

    applyWsEvent: (event) => {
      switch (event.type) {
        case 'connected':
          set({ wsConnected: true });
          break;
        case 'heartbeat':
          set((state) => ({
            liveMetrics: {
              uptime_secs: event.uptime_secs,
              process_ram_bytes: event.process_ram_bytes,
              db_size_bytes: event.db_size_bytes,
              active_jobs: event.active_jobs,
              updated_at_ms: Date.now(),
            },
            systemInfo: state.systemInfo
              ? {
                  ...state.systemInfo,
                  stats: {
                    ...state.systemInfo.stats,
                    db_size_bytes: event.db_size_bytes,
                  },
                }
              : null,
          }));
          break;
        case 'power_state_changed':
          set((state) => ({
            powerState: event.power,
            systemInfo: state.systemInfo
              ? {
                  ...state.systemInfo,
                  power: event.power,
                }
              : null,
          }));
          break;
        case 'throttle_state_changed':
          set((state) => ({
            throttleStatus: event.throttle,
            powerState: state.powerState
              ? { ...state.powerState, on_battery: event.throttle.on_battery }
              : {
                  sleeping: false,
                  display_asleep: false,
                  session_locked: false,
                  on_battery: event.throttle.on_battery,
                },
            systemInfo: state.systemInfo
              ? {
                  ...state.systemInfo,
                  throttle: event.throttle,
                  power: {
                    ...state.systemInfo.power,
                    on_battery: event.throttle.on_battery,
                  },
                }
              : null,
          }));
          break;
        case 'tailscale_status_changed':
          set((state) => ({
            tailscaleStatus: event.status,
            systemInfo: state.systemInfo
              ? {
                  ...state.systemInfo,
                  tailscale_url: event.status.serve_active ? event.status.https_url : null,
                }
              : null,
          }));
          break;
        case 'settings_changed':
          set((state) => ({
            adminSettings: event.settings,
            desktopPinLock: event.settings.desktop_pin_lock,
            systemInfo: state.systemInfo
              ? {
                  ...state.systemInfo,
                  mdns_enabled: event.settings.mdns_enabled,
                  desktop_pin_lock: event.settings.desktop_pin_lock,
                }
              : null,
          }));
          break;
        case 'ai_state_changed':
          set((state) => ({
            aiDashboard: state.aiDashboard
              ? {
                  ...state.aiDashboard,
                  state: {
                    ...state.aiDashboard.state,
                    active_vision_tier: event.active_vision_tier,
                    staging_vision_tier: event.staging_vision_tier,
                    paused: event.paused,
                    updated_at_ms: Date.now(),
                  },
                }
              : null,
          }));
          break;
        case 'folder_changed':
          set((state) => ({ libraryRevision: state.libraryRevision + 1 }));
          void get().refreshFolders().catch(() => {});
          void get().refreshSystemInfo().catch(() => {});
          break;
        case 'job_progress':
          set((state) => {
            const nextJobs = { ...state.activeJobs };
            if (
              event.status === 'completed' ||
              event.status === 'failed' ||
              event.status === 'cancelled'
            ) {
              delete nextJobs[event.job_id];
            } else {
              nextJobs[event.job_id] = {
                job_id: event.job_id,
                kind: event.kind,
                status: event.status,
                processed: event.processed,
                total: event.total,
                message: event.message,
                updated_at_ms: Date.now(),
              };
            }
            return { activeJobs: nextJobs };
          });
          break;
        case 'pong':
          break;
      }
    },

    handleUnauthorized: () => {
      get().disconnectWs();
      searchSeq += 1;
      chatSeq += 1;
      set({
        authenticated: false,
        serverGatePassed: false,
        profiles: null,
        user: null,
        allowedFolderIds: null,
        folders: [],
        adminUsers: [],
        adminSettings: null,
        wsConnected: false,
        liveMetrics: null,
        throttleStatus: null,
        tailscaleStatus: null,
        inspectorOpen: false,
        searchQuery: '',
        selectedAssetId: null,
        lightboxAssetId: null,
        lightboxAssetIds: [],
        lightboxInitialTimestampSec: null,
        uploadModalOpen: false,
        activeJobs: {},
        libraryRevision: 0,
        searchActive: false,
        searchRequest: defaultSearchRequest(),
        searchResponse: null,
        searchLoading: false,
        aiDashboard: null,
        chatDrawerOpen: false,
        chatSessions: [],
        activeChatSessionId: null,
        chatMessages: [],
        chatActiveFilters: null,
        chatSending: false,
        chatLastReranked: false,
      });
    },

    resetForTest: () => {
      setUnauthorizedHandler(null);
      searchSeq += 1;
      chatSeq += 1;
      if (wsClient) {
        wsClient.disconnect();
        wsClient = null;
      }
      set({
        ...initialDataState,
        searchRequest: defaultSearchRequest(),
      });
    },
  };
});
