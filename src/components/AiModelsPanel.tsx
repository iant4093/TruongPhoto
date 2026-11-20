import React, { useCallback, useEffect, useState } from 'react';
import {
  ChevronDown,
  ChevronUp,
  Cpu,
  Download,
  FileText,
  MessageSquare,
  Pause,
  Play,
  RefreshCw,
  Search,
  Sparkles,
  Trash2,
  Users,
  X,
} from 'lucide-react';
import {
  cancelAiModelDownload,
  deleteAiModel,
  downloadAiModel,
  pauseAiIndexing,
  resumeAiIndexing,
  setAiChatTier,
  setAiVisionTier,
  triggerAiReindex,
  updateAiSettings,
} from '../lib/api';
import { formatBytes } from '../lib/format';
import { notifyError, notifySuccess } from '../store/feedback';
import { errorMessage, useAppStore } from '../store/useAppStore';
import type { AiModelStatus, AiModelView, AiTier, ChatTier } from '../types/api';
import { Dropdown } from './Dropdown';
import { Badge, SegmentedControl, ToggleRow } from './ui';

const TIER_OPTIONS: ReadonlyArray<readonly [AiTier, string]> = [
  ['tier1', 'Tier 1 · Base'],
  ['tier2', 'Tier 2 · Large'],
  ['tier3', 'Tier 3 · SO400M'],
];

const SIMPLE_VISION_PRESETS: ReadonlyArray<{
  tier: AiTier;
  modelId: string;
  label: string;
  desc: string;
}> = [
  { tier: 'tier1', modelId: 'siglip2-base', label: 'Fast', desc: '~380 MB · Low memory' },
  { tier: 'tier2', modelId: 'siglip2-large', label: 'Balanced', desc: '~1.2 GB · Recommended' },
  { tier: 'tier3', modelId: 'siglip2-so400m', label: 'Max Accuracy', desc: '~1.7 GB · Best search' },
];

const SIMPLE_CHAT_PRESETS: ReadonlyArray<{
  tier: ChatTier;
  modelId: string | null;
  label: string;
  desc: string;
}> = [
  { tier: 'tier0', modelId: null, label: 'Instant Rules', desc: 'Built-in · No download' },
  { tier: 'tier1', modelId: 'gemma4-e2b', label: 'Compact AI', desc: '~1.6 GB local LLM' },
  { tier: 'tier2', modelId: 'gemma4-e4b', label: 'Standard AI', desc: '~2.8 GB local LLM' },
  { tier: 'tier3', modelId: 'gemma4-26b-a4b', label: 'Studio AI + VLM', desc: '~15 GB · Visual rerank' },
];

const CHAT_TIER_OPTIONS: ReadonlyArray<readonly [ChatTier, string]> = [
  ['tier0', 'Tier 0 Classic'],
  ['tier1', 'Tier 1 Gemma-4-E2B'],
  ['tier2', 'Tier 2 Gemma-4-E4B'],
  ['tier3', 'Tier 3 Gemma-4-26B + VLM'],
];

function formatChatTierLabel(tier: ChatTier): string {
  switch (tier) {
    case 'tier0':
      return 'Tier 0 Classic';
    case 'tier1':
      return 'Tier 1 Gemma-4-E2B';
    case 'tier2':
      return 'Tier 2 Gemma-4-E4B';
    case 'tier3':
      return 'Tier 3 Gemma-4-26B + VLM';
  }
}

const IDLE_TIMEOUT_OPTIONS: ReadonlyArray<readonly [number, string]> = [
  [60, '1m'],
  [300, '5m'],
  [900, '15m'],
  [0, 'Never'],
];

function formatTierLabel(tier: AiTier): string {
  switch (tier) {
    case 'tier1':
      return 'Tier 1';
    case 'tier2':
      return 'Tier 2';
    case 'tier3':
      return 'Tier 3';
  }
}

function visionModelIdForTier(tier: AiTier): string {
  switch (tier) {
    case 'tier1':
      return 'siglip2-base';
    case 'tier2':
      return 'siglip2-large';
    case 'tier3':
      return 'siglip2-so400m';
  }
}

function statusBadgeTone(status: AiModelStatus): 'ok' | 'info' | 'danger' | 'muted' {
  switch (status) {
    case 'installed':
      return 'ok';
    case 'downloading':
      return 'info';
    case 'error':
      return 'danger';
    default:
      return 'muted';
  }
}

function statusBadgeLabel(status: AiModelStatus): string {
  switch (status) {
    case 'installed':
      return 'Ready ✓';
    case 'downloading':
      return 'Downloading…';
    case 'error':
      return 'Error';
    default:
      return 'Not Installed';
  }
}

function aggregateBundleStatus(items: AiModelView[]): {
  status: AiModelStatus;
  bytesTotal: number;
  bytesDownloaded: number;
  pct: number;
} {
  if (items.length === 0) {
    return { status: 'not_installed', bytesTotal: 0, bytesDownloaded: 0, pct: 0 };
  }
  const bytesTotal = items.reduce(
    (acc, m) => acc + (m.bytes_total > 0 ? m.bytes_total : m.approx_bytes),
    0,
  );
  const bytesDownloaded = items.reduce(
    (acc, m) =>
      acc +
      (m.status === 'installed'
        ? m.bytes_total > 0
          ? m.bytes_total
          : m.approx_bytes
        : m.bytes_downloaded),
    0,
  );
  const pct = bytesTotal > 0 ? Math.min(100, Math.round((bytesDownloaded / bytesTotal) * 100)) : 0;

  if (items.every((m) => m.status === 'installed')) {
    return { status: 'installed', bytesTotal, bytesDownloaded: bytesTotal, pct: 100 };
  }
  if (items.some((m) => m.status === 'downloading')) {
    return { status: 'downloading', bytesTotal, bytesDownloaded, pct };
  }
  if (items.some((m) => m.status === 'error')) {
    return { status: 'error', bytesTotal, bytesDownloaded, pct };
  }
  return { status: 'not_installed', bytesTotal, bytesDownloaded, pct };
}

export const AiModelsPanel: React.FC = () => {
  const aiDashboard = useAppStore((s) => s.aiDashboard);
  const refreshAiDashboard = useAppStore((s) => s.refreshAiDashboard);

  const [loading, setLoading] = useState(!aiDashboard);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busyModelIds, setBusyModelIds] = useState<Record<string, boolean>>({});
  const [busyControl, setBusyControl] = useState<string | null>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);

  const loadDashboard = useCallback(async () => {
    setLoading((prev) => (aiDashboard ? prev : true));
    setLoadError(null);
    try {
      await refreshAiDashboard();
    } catch (err) {
      setLoadError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [aiDashboard, refreshAiDashboard]);

  useEffect(() => {
    let active = true;
    void refreshAiDashboard()
      .then(() => {
        if (active) {
          setLoading(false);
          setLoadError(null);
        }
      })
      .catch((err) => {
        if (active) {
          setLoading(false);
          setLoadError(errorMessage(err));
        }
      });
    return () => {
      active = false;
    };
  }, [refreshAiDashboard]);

  // Automatically poll dashboard every 2 seconds while any model is downloading
  useEffect(() => {
    const anyDownloading = aiDashboard?.models.some((m) => m.status === 'downloading') ?? false;
    if (!anyDownloading) return;
    const timer = window.setInterval(() => {
      void refreshAiDashboard().catch(() => {});
    }, 2000);
    return () => window.clearInterval(timer);
  }, [aiDashboard?.models, refreshAiDashboard]);

  const setModelBusy = (id: string, busy: boolean) => {
    setBusyModelIds((prev) => ({ ...prev, [id]: busy }));
  };

  const handleDownloadModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await downloadAiModel(id);
      await refreshAiDashboard();
      notifySuccess(`Downloading ${name}…`);
    } catch (err) {
      notifyError(err, `Could not download ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleDownloadBundle = async (ids: string[], label: string) => {
    setBusyControl(`bundle-${label}`);
    try {
      for (const id of ids) {
        const model = aiDashboard?.models.find((m) => m.id === id);
        if (!model || model.status === 'not_installed' || model.status === 'error') {
          await downloadAiModel(id);
        }
      }
      await refreshAiDashboard();
      notifySuccess(`Started downloading ${label}`);
    } catch (err) {
      notifyError(err, `Could not download ${label}`);
    } finally {
      setBusyControl(null);
    }
  };

  const handleCancelModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await cancelAiModelDownload(id);
      await refreshAiDashboard();
      notifySuccess(`Cancelled download for ${name}`);
    } catch (err) {
      notifyError(err, `Could not cancel ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleDeleteModel = async (id: string, name: string) => {
    setModelBusy(id, true);
    try {
      await deleteAiModel(id);
      await refreshAiDashboard();
      notifySuccess(`Removed ${name}`);
    } catch (err) {
      notifyError(err, `Could not delete ${name}`);
    } finally {
      setModelBusy(id, false);
    }
  };

  const handleSetTier = async (tier: AiTier) => {
    if (!aiDashboard || aiDashboard.state.active_vision_tier === tier) return;
    setBusyControl('tier');
    try {
      await setAiVisionTier(tier);
      await refreshAiDashboard();
      notifySuccess(`Switched Smart Photo Search quality to ${formatTierLabel(tier)}`);
    } catch (err) {
      notifyError(err, 'Could not change search quality tier');
    } finally {
      setBusyControl(null);
    }
  };

  const handleSetChatTier = async (tier: ChatTier, vlmRerankEnabled?: boolean) => {
    if (!aiDashboard) return;
    const nextVlm =
      vlmRerankEnabled !== undefined
        ? vlmRerankEnabled
        : tier === 'tier3'
          ? Boolean(aiDashboard.state.vlm_rerank_enabled)
          : false;
    setBusyControl('chat-tier');
    try {
      await setAiChatTier({ tier, vlm_rerank_enabled: nextVlm });
      await refreshAiDashboard();
      notifySuccess(`Switched Ask AI mode to ${formatChatTierLabel(tier)}`);
    } catch (err) {
      notifyError(err, 'Could not update Ask AI mode');
    } finally {
      setBusyControl(null);
    }
  };

  const handleTogglePause = async () => {
    if (!aiDashboard) return;
    setBusyControl('pause');
    try {
      if (aiDashboard.state.paused) {
        await resumeAiIndexing();
        notifySuccess('Resumed photo analysis');
      } else {
        await pauseAiIndexing();
        notifySuccess('Paused photo analysis');
      }
      await refreshAiDashboard();
    } catch (err) {
      notifyError(err, 'Could not update indexing state');
    } finally {
      setBusyControl(null);
    }
  };

  const handleReindex = async (scope: 'missing' | 'all') => {
    setBusyControl(`reindex-${scope}`);
    try {
      await triggerAiReindex(scope);
      await refreshAiDashboard();
      notifySuccess(
        scope === 'all' ? 'Queued full library re-analysis' : 'Queued analysis for pending photos',
      );
    } catch (err) {
      notifyError(err, 'Could not queue AI analysis');
    } finally {
      setBusyControl(null);
    }
  };

  const handleIdleUnloadChange = async (idleUnloadSecs: number) => {
    setBusyControl('idle');
    try {
      await updateAiSettings({ idle_unload_secs: idleUnloadSecs });
      await refreshAiDashboard();
      notifySuccess('Updated AI memory unload timeout');
    } catch (err) {
      notifyError(err, 'Could not update AI settings');
    } finally {
      setBusyControl(null);
    }
  };

  if (loading && !aiDashboard) {
    return (
      <div className="card card-body view-loading" role="status">
        Loading AI features…
      </div>
    );
  }

  if (loadError && !aiDashboard) {
    return (
      <div className="error-banner error-banner-dismissible" role="alert">
        <span>{loadError}</span>
        <button type="button" className="button" onClick={() => void loadDashboard()}>
          Retry
        </button>
      </div>
    );
  }

  if (!aiDashboard) return null;

  const { ort_available, ort_ep, hardware, state, models, queue } = aiDashboard;
  const activeVisionTier = state.staging_vision_tier ?? state.active_vision_tier;
  const activeVisionModelId = visionModelIdForTier(activeVisionTier);
  const activeChatTier: ChatTier = state.active_chat_tier ?? 'tier0';
  const vlmRerankEnabled = Boolean(state.vlm_rerank_enabled);

  const totalAssets = queue.indexed_assets + queue.pending_assets;
  const indexedPct =
    totalAssets > 0 ? Math.min(100, Math.round((queue.indexed_assets / totalAssets) * 100)) : 100;

  const stagingTotal = Math.max(1, totalAssets);
  const stagingDone = Math.max(0, stagingTotal - queue.staging_remaining);
  const stagingPct = Math.min(100, Math.round((stagingDone / stagingTotal) * 100));

  const visionModels = models.filter((m) => m.id === activeVisionModelId);
  const faceModels = models.filter((m) => m.id === 'yunet' || m.id === 'auraface-v1');
  const ocrModels = models.filter((m) => m.id === 'pp-ocrv6-det' || m.id === 'pp-ocrv6-rec');
  const essentialIds = [activeVisionModelId, 'yunet', 'auraface-v1', 'pp-ocrv6-det', 'pp-ocrv6-rec'];
  const essentialModels = models.filter((m) => essentialIds.includes(m.id));

  const visionStatus = aggregateBundleStatus(visionModels);
  const faceStatus = aggregateBundleStatus(faceModels);
  const ocrStatus = aggregateBundleStatus(ocrModels);
  const essentialStatus = aggregateBundleStatus(essentialModels);

  const selectedChatPreset =
    SIMPLE_CHAT_PRESETS.find((p) => p.tier === activeChatTier) ?? SIMPLE_CHAT_PRESETS[0];
  const activeChatModel = selectedChatPreset.modelId
    ? models.find((m) => m.id === selectedChatPreset.modelId) ?? null
    : null;

  return (
    <div className="ai-dashboard">
      {/* 1. Simple Top Overview & One-Click Setup Banner */}
      <div className="ai-simple-hero">
        <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 12 }}>
          <div>
            <div className="row" style={{ gap: 8 }}>
              <Sparkles size={15} />
              <strong style={{ fontSize: 14 }}>Smart Library Analysis</strong>
              <Badge
                tone={
                  state.paused
                    ? 'warn'
                    : essentialStatus.status === 'installed'
                      ? 'ok'
                      : essentialStatus.status === 'downloading'
                        ? 'info'
                        : 'muted'
                }
              >
                {state.paused
                  ? 'Paused'
                  : essentialStatus.status === 'installed'
                    ? queue.pending_assets === 0
                      ? 'Up to Date ✓'
                      : `Analyzing (${indexedPct}%)`
                    : essentialStatus.status === 'downloading'
                      ? `Downloading Models (${essentialStatus.pct}%)`
                      : 'Setup Needed'}
              </Badge>
            </div>
            <div className="view-subtitle" style={{ marginTop: 4 }}>
              {totalAssets === 0
                ? 'Add photos to begin automatic on-device search, face grouping, and text recognition.'
                : `${queue.indexed_assets.toLocaleString()} of ${totalAssets.toLocaleString()} photos analyzed (${queue.pending_assets.toLocaleString()} remaining)`}
            </div>
          </div>

          <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
            {essentialStatus.status !== 'installed' && (
              <button
                type="button"
                className="button button-primary"
                disabled={
                  busyControl === 'bundle-Recommended AI Models' ||
                  essentialStatus.status === 'downloading'
                }
                onClick={() =>
                  void handleDownloadBundle(essentialIds, 'Recommended AI Models')
                }
              >
                <Download size={14} />
                <span>
                  {essentialStatus.status === 'downloading'
                    ? `Downloading (${essentialStatus.pct}%)…`
                    : 'Download Recommended AI Models'}
                </span>
              </button>
            )}

            <button
              type="button"
              className="button"
              disabled={busyControl === 'pause'}
              onClick={() => void handleTogglePause()}
            >
              {state.paused ? <Play size={13} /> : <Pause size={13} />}
              <span>{state.paused ? 'Resume Indexing' : 'Pause Indexing'}</span>
            </button>
          </div>
        </div>

        {(totalAssets > 0 || essentialStatus.status === 'downloading') && (
          <div
            className="ai-model-progress"
            role="progressbar"
            aria-label="Overall AI progress"
            aria-valuenow={
              essentialStatus.status === 'downloading' ? essentialStatus.pct : indexedPct
            }
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div
              style={{
                width: `${essentialStatus.status === 'downloading' ? essentialStatus.pct : indexedPct}%`,
              }}
            />
          </div>
        )}
      </div>

      {/* 2. Simple 4-Feature Cards for Non-Technical Users */}
      <div className="ai-feature-grid">
        {/* Card A: Smart Photo Search & Auto-Tags */}
        <div className="ai-feature-card">
          <div className="row" style={{ justifyContent: 'space-between', alignItems: 'flex-start', gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <Search size={15} />
              <strong style={{ fontSize: 13 }}>Smart Photo Search &amp; Auto-Tags</strong>
            </div>
            <Badge tone={statusBadgeTone(visionStatus.status)}>
              {statusBadgeLabel(visionStatus.status)}
            </Badge>
          </div>
          <p className="view-subtitle" style={{ margin: 0 }}>
            Search by describing scenes or objects (like &ldquo;sunset&rdquo; or &ldquo;dog&rdquo;)
            and generate automatic tags.
          </p>

          <div className="preset-pill-grid">
            {SIMPLE_VISION_PRESETS.map((preset) => {
              const active = activeVisionTier === preset.tier;
              const isRec = hardware.recommended_vision_tier === preset.tier;
              return (
                <button
                  key={preset.tier}
                  type="button"
                  className={`preset-choice-card ${active ? 'is-selected' : ''}`}
                  onClick={() => void handleSetTier(preset.tier)}
                >
                  <div className="row" style={{ justifyContent: 'space-between', width: '100%' }}>
                    <strong>{preset.label}</strong>
                    {isRec && <Badge tone="info">Suggested</Badge>}
                  </div>
                  <span className="view-subtitle" style={{ marginTop: 2 }}>
                    {preset.desc}
                  </span>
                </button>
              );
            })}
          </div>

          {visionStatus.status === 'downloading' && (
            <div className="ai-model-progress">
              <div style={{ width: `${visionStatus.pct}%` }} />
            </div>
          )}

          {(visionStatus.status === 'not_installed' || visionStatus.status === 'error') && (
            <div className="row" style={{ justifyContent: 'space-between', paddingTop: 4 }}>
              <span className="view-subtitle" style={{ marginTop: 0 }}>
                Download required ({formatBytes(visionStatus.bytesTotal)})
              </span>
              <button
                type="button"
                className="button button-primary"
                disabled={Boolean(busyModelIds[activeVisionModelId])}
                onClick={() =>
                  void handleDownloadModel(activeVisionModelId, 'Smart Photo Search')
                }
              >
                <Download size={13} />
                <span>Install Search Model</span>
              </button>
            </div>
          )}
        </div>

        {/* Card B: People & Face Grouping */}
        <div className="ai-feature-card">
          <div className="row" style={{ justifyContent: 'space-between', alignItems: 'flex-start', gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <Users size={15} />
              <strong style={{ fontSize: 13 }}>People &amp; Face Grouping</strong>
            </div>
            <Badge tone={statusBadgeTone(faceStatus.status)}>
              {statusBadgeLabel(faceStatus.status)}
            </Badge>
          </div>
          <p className="view-subtitle" style={{ margin: 0 }}>
            Automatically detects faces and groups photos of the same person into the People tab.
          </p>

          {faceStatus.status === 'downloading' && (
            <div className="stack" style={{ gap: 4 }}>
              <div className="row" style={{ justifyContent: 'space-between', fontSize: 11 }}>
                <span>
                  {formatBytes(faceStatus.bytesDownloaded)} / {formatBytes(faceStatus.bytesTotal)}
                </span>
                <span>{faceStatus.pct}%</span>
              </div>
              <div className="ai-model-progress">
                <div style={{ width: `${faceStatus.pct}%` }} />
              </div>
            </div>
          )}

          <div className="row" style={{ justifyContent: 'space-between', marginTop: 'auto', paddingTop: 4 }}>
            <span className="view-subtitle" style={{ marginTop: 0 }}>
              {faceStatus.status === 'installed'
                ? 'Face detector & recognizer installed'
                : `Download size: ${formatBytes(faceStatus.bytesTotal)}`}
            </span>
            {(faceStatus.status === 'not_installed' || faceStatus.status === 'error') && (
              <button
                type="button"
                className="button button-primary"
                disabled={busyControl === 'bundle-Face Grouping'}
                onClick={() => void handleDownloadBundle(['yunet', 'auraface-v1'], 'Face Grouping')}
              >
                <Download size={13} />
                <span>Install Face Models</span>
              </button>
            )}
          </div>
        </div>

        {/* Card C: Text in Photos (OCR) */}
        <div className="ai-feature-card">
          <div className="row" style={{ justifyContent: 'space-between', alignItems: 'flex-start', gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <FileText size={15} />
              <strong style={{ fontSize: 13 }}>Text in Photos (OCR)</strong>
            </div>
            <Badge tone={statusBadgeTone(ocrStatus.status)}>
              {statusBadgeLabel(ocrStatus.status)}
            </Badge>
          </div>
          <p className="view-subtitle" style={{ margin: 0 }}>
            Finds words inside receipts, signs, notes, and screenshots so you can search for them.
          </p>

          {ocrStatus.status === 'downloading' && (
            <div className="stack" style={{ gap: 4 }}>
              <div className="row" style={{ justifyContent: 'space-between', fontSize: 11 }}>
                <span>
                  {formatBytes(ocrStatus.bytesDownloaded)} / {formatBytes(ocrStatus.bytesTotal)}
                </span>
                <span>{ocrStatus.pct}%</span>
              </div>
              <div className="ai-model-progress">
                <div style={{ width: `${ocrStatus.pct}%` }} />
              </div>
            </div>
          )}

          <div className="row" style={{ justifyContent: 'space-between', marginTop: 'auto', paddingTop: 4 }}>
            <span className="view-subtitle" style={{ marginTop: 0 }}>
              {ocrStatus.status === 'installed'
                ? 'Text detector & reader installed'
                : `Download size: ${formatBytes(ocrStatus.bytesTotal)}`}
            </span>
            {(ocrStatus.status === 'not_installed' || ocrStatus.status === 'error') && (
              <button
                type="button"
                className="button button-primary"
                disabled={busyControl === 'bundle-Text Recognition'}
                onClick={() =>
                  void handleDownloadBundle(['pp-ocrv6-det', 'pp-ocrv6-rec'], 'Text Recognition')
                }
              >
                <Download size={13} />
                <span>Install Text Models</span>
              </button>
            )}
          </div>
        </div>

        {/* Card D: Ask AI Chat Assistant */}
        <div className="ai-feature-card">
          <div className="row" style={{ justifyContent: 'space-between', alignItems: 'flex-start', gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <MessageSquare size={15} />
              <strong style={{ fontSize: 13 }}>Ask AI Assistant</strong>
            </div>
            <Badge
              tone={
                !activeChatModel
                  ? 'ok'
                  : statusBadgeTone(activeChatModel.status)
              }
            >
              {!activeChatModel ? 'Ready ✓' : statusBadgeLabel(activeChatModel.status)}
            </Badge>
          </div>
          <p className="view-subtitle" style={{ margin: 0 }}>
            Powers conversational search in the Ask AI drawer. Instant Rules work out-of-the-box
            with zero download.
          </p>

          <div className="preset-pill-grid">
            {SIMPLE_CHAT_PRESETS.map((preset) => {
              const active = activeChatTier === preset.tier;
              return (
                <button
                  key={preset.tier}
                  type="button"
                  className={`preset-choice-card ${active ? 'is-selected' : ''}`}
                  onClick={() => {
                    if (preset.tier !== activeChatTier) {
                      void handleSetChatTier(preset.tier);
                    }
                  }}
                >
                  <strong>{preset.label}</strong>
                  <span className="view-subtitle" style={{ marginTop: 2 }}>
                    {preset.desc}
                  </span>
                </button>
              );
            })}
          </div>

          {activeChatModel &&
            (activeChatModel.status === 'not_installed' || activeChatModel.status === 'error') && (
              <div className="row" style={{ justifyContent: 'space-between', paddingTop: 4 }}>
                <span className="view-subtitle" style={{ marginTop: 0 }}>
                  Model download required ({formatBytes(activeChatModel.approx_bytes)})
                </span>
                <button
                  type="button"
                  className="button button-primary"
                  disabled={Boolean(busyModelIds[activeChatModel.id])}
                  onClick={() =>
                    void handleDownloadModel(activeChatModel.id, activeChatModel.name)
                  }
                >
                  <Download size={13} />
                  <span>Install Chat Model</span>
                </button>
              </div>
            )}
        </div>
      </div>

      {/* 3. Collapsible Advanced Technical Details (Hidden by default so non-technical users aren't overwhelmed) */}
      <div className="advanced-accordion">
        <button
          type="button"
          className="advanced-accordion-trigger"
          aria-expanded={showAdvanced}
          onClick={() => setShowAdvanced((v) => !v)}
        >
          <span>
            {showAdvanced
              ? 'Hide Advanced Model & Engine Details'
              : 'Show Advanced Model & Engine Details'}
          </span>
          {showAdvanced ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
        </button>

        <div className={`advanced-accordion-body ${showAdvanced ? 'is-open' : ''}`}>
          <div className="stack" style={{ gap: 12, paddingTop: 12 }}>
            <div className="ai-hardware-banner">
              <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
                <Cpu size={15} />
                <span>
                  Detected {formatBytes(hardware.total_ram_bytes)} RAM ({hardware.cpu_cores} cores)
                  → <strong>{formatTierLabel(hardware.recommended_vision_tier)} recommended</strong>
                </span>
              </div>
              <span
                className={`ai-ep-badge ${ort_available ? 'is-available' : 'is-unavailable'}`}
                data-testid="ai-ep-badge"
              >
                {ort_available ? ort_ep : 'Not Loaded'}
              </span>
            </div>

            <div className="ai-tier-bar">
              <div
                className="row"
                style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}
              >
                <div>
                  <div style={{ fontWeight: 600, fontSize: 13 }}>Vision Embedding Tier</div>
                  <div className="view-subtitle" style={{ marginTop: 2 }}>
                    Active: {formatTierLabel(state.active_vision_tier)} · {queue.indexed_assets}{' '}
                    indexed · {queue.pending_assets} pending
                  </div>
                </div>

                <SegmentedControl
                  value={state.staging_vision_tier ?? state.active_vision_tier}
                  options={TIER_OPTIONS}
                  onChange={(tier) => void handleSetTier(tier)}
                />
              </div>

              {state.staging_vision_tier && (
                <div className="stack" style={{ gap: 6 }}>
                  <div className="row" style={{ justifyContent: 'space-between', fontSize: 12 }}>
                    <span>
                      Staging migration to {formatTierLabel(state.staging_vision_tier)} (live search
                      stays on {formatTierLabel(state.active_vision_tier)})
                    </span>
                    <span className="property-value">
                      {queue.staging_remaining} remaining ({stagingPct}%)
                    </span>
                  </div>
                  <div
                    className="ai-model-progress"
                    role="progressbar"
                    aria-label="Vision tier staging progress"
                    aria-valuenow={stagingPct}
                    aria-valuemin={0}
                    aria-valuemax={100}
                  >
                    <div style={{ width: `${stagingPct}%` }} />
                  </div>
                </div>
              )}

              <div
                className="row"
                style={{
                  justifyContent: 'space-between',
                  flexWrap: 'wrap',
                  gap: 10,
                  paddingTop: 8,
                  borderTop: '1px solid var(--border-subtle)',
                }}
              >
                <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
                  <button
                    type="button"
                    className="button"
                    disabled={busyControl === 'reindex-missing'}
                    onClick={() => void handleReindex('missing')}
                  >
                    <RefreshCw size={13} />
                    <span>Reindex</span>
                  </button>

                  <button
                    type="button"
                    className="button button-quiet"
                    disabled={busyControl === 'reindex-all'}
                    onClick={() => void handleReindex('all')}
                  >
                    <span>Reindex All</span>
                  </button>
                </div>

                <div className="row" style={{ gap: 8, fontSize: 12 }}>
                  <span className="workspace-label">Idle Memory Unload</span>
                  <Dropdown
                    label="Idle Memory Unload"
                    value={state.idle_unload_secs}
                    disabled={busyControl === 'idle'}
                    options={IDLE_TIMEOUT_OPTIONS}
                    onChange={(secs) => void handleIdleUnloadChange(secs)}
                  />
                </div>
              </div>
            </div>

            <div className="chat-tier-section">
              <div
                className="row"
                style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}
              >
                <div>
                  <div style={{ fontWeight: 600, fontSize: 13 }}>Conversational Chat Tier</div>
                  <div className="view-subtitle" style={{ marginTop: 2 }}>
                    Active: {formatChatTierLabel(activeChatTier)} · Recommended:{' '}
                    {formatTierLabel(hardware.recommended_chat_tier)}
                  </div>
                </div>

                <SegmentedControl
                  value={activeChatTier}
                  options={CHAT_TIER_OPTIONS}
                  onChange={(tier) => {
                    if (tier !== activeChatTier) {
                      void handleSetChatTier(tier);
                    }
                  }}
                />
              </div>

              <div style={{ borderTop: '1px solid var(--border-subtle)', paddingTop: 10 }}>
                <ToggleRow
                  label="VLM Thumbnail Reranking"
                  description="Use Tier 3 multimodal Gemma-4-26B to visually rerank top candidate thumbnails (enabled on Tier 3)."
                  checked={vlmRerankEnabled}
                  disabled={activeChatTier !== 'tier3' || busyControl === 'chat-tier'}
                  onChange={(next) => void handleSetChatTier(activeChatTier, next)}
                />
              </div>
            </div>

            <div className="stack">
              {models.map((model) => {
                const isBusy = Boolean(busyModelIds[model.id]);
                const totalBytes = model.bytes_total > 0 ? model.bytes_total : model.approx_bytes;
                const pct =
                  totalBytes > 0
                    ? Math.min(100, Math.round((model.bytes_downloaded / totalBytes) * 100))
                    : 0;

                return (
                  <div key={model.id} className="ai-model-card" data-model-id={model.id}>
                    <div
                      className="row"
                      style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}
                    >
                      <div className="row" style={{ flexWrap: 'wrap', gap: 8 }}>
                        <strong style={{ fontSize: 13 }}>{model.name}</strong>
                        <Badge tone={statusBadgeTone(model.status)}>
                          {statusBadgeLabel(model.status)}
                        </Badge>
                      </div>

                      <div className="row" style={{ gap: 8 }}>
                        <span className="property-value">{formatBytes(model.approx_bytes)}</span>

                        {(model.status === 'not_installed' || model.status === 'error') && (
                          <button
                            type="button"
                            className="button button-primary"
                            disabled={isBusy}
                            onClick={() => void handleDownloadModel(model.id, model.name)}
                          >
                            <Download size={13} />
                            <span>Download</span>
                          </button>
                        )}

                        {model.status === 'downloading' && (
                          <button
                            type="button"
                            className="button"
                            disabled={isBusy}
                            onClick={() => void handleCancelModel(model.id, model.name)}
                          >
                            <X size={13} />
                            <span>Cancel</span>
                          </button>
                        )}

                        {model.status === 'installed' && (
                          <button
                            type="button"
                            className="button button-danger-quiet"
                            disabled={isBusy}
                            onClick={() => void handleDeleteModel(model.id, model.name)}
                          >
                            <Trash2 size={13} />
                            <span>Delete</span>
                          </button>
                        )}
                      </div>
                    </div>

                    {model.status === 'downloading' && (
                      <div className="stack" style={{ gap: 4 }}>
                        <div
                          className="row"
                          style={{ justifyContent: 'space-between', fontSize: 11 }}
                        >
                          <span>
                            {formatBytes(model.bytes_downloaded)} / {formatBytes(totalBytes)}
                          </span>
                          <span className="property-value">{pct}%</span>
                        </div>
                        <div
                          className="ai-model-progress"
                          role="progressbar"
                          aria-label={`${model.name} download progress`}
                          aria-valuenow={pct}
                          aria-valuemin={0}
                          aria-valuemax={100}
                        >
                          <div style={{ width: `${pct}%` }} />
                        </div>
                      </div>
                    )}

                    {model.status === 'error' && model.error_message && (
                      <div className="error-banner" role="alert" style={{ marginBottom: 0 }}>
                        {model.error_message}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
