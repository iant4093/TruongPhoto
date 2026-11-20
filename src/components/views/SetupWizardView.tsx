import React, { useEffect, useState } from 'react';
import { ChevronDown, ChevronUp, FolderOpen, Sparkles } from 'lucide-react';
import { downloadAiModel, scanFolder, setAiVisionTier } from '../../lib/api';
import { formatBytes } from '../../lib/format';
import { isTauri, pickFolderDialog } from '../../lib/tauri';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import type { AiTier } from '../../types/api';
import { Badge } from '../ui';

const DIGIT_4 = /^\d{4}$/;
const DIGIT_6 = /^\d{6}$/;

function basename(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '');
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || 'Photos';
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

function presetSummaryLabel(tier: AiTier): string {
  switch (tier) {
    case 'tier1':
      return 'Fast & Lightweight (~660 MB)';
    case 'tier2':
      return 'Balanced Quality (~1.5 GB)';
    case 'tier3':
      return 'Maximum Accuracy (~2.0 GB)';
  }
}

export const SetupWizardView: React.FC = () => {
  const setupStatus = useAppStore((s) => s.setupStatus);
  const completeSetup = useAppStore((s) => s.completeSetup);
  const bootstrap = useAppStore((s) => s.bootstrap);

  const [step, setStep] = useState<1 | 2 | 3>(1);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // Step 1: Smart AI Setup options
  const [autoDownloadAi, setAutoDownloadAi] = useState(true);
  const [selectedVisionTier, setSelectedVisionTier] = useState<AiTier>('tier2');
  const [includeChatModel, setIncludeChatModel] = useState(false);
  const [showHardwareDetails, setShowHardwareDetails] = useState(false);

  // Step 2: Admin Profile & PINs
  const [adminUsername, setAdminUsername] = useState('');
  const [usernameTouched, setUsernameTouched] = useState(false);
  const [adminDisplayName, setAdminDisplayName] = useState('');
  const [profilePin, setProfilePin] = useState('');
  const [confirmProfilePin, setConfirmProfilePin] = useState('');
  const [serverPin, setServerPin] = useState('');
  const [confirmServerPin, setConfirmServerPin] = useState('');

  // Step 3: Photo Folder & Advanced Storage Paths
  const [managedUploadsDir, setManagedUploadsDir] = useState(
    () => setupStatus?.default_paths.managed_uploads_dir ?? '',
  );
  const [thumbnailsDir, setThumbnailsDir] = useState(
    () => setupStatus?.default_paths.thumbnails_dir ?? '',
  );
  const [cacheDir, setCacheDir] = useState(() => setupStatus?.default_paths.cache_dir ?? '');
  const [backupDir, setBackupDir] = useState(() => setupStatus?.default_paths.backup_dir ?? '');
  const [initialFolderPath, setInitialFolderPath] = useState('');
  const [initialFolderName, setInitialFolderName] = useState('');
  const [showAdvancedStorage, setShowAdvancedStorage] = useState(false);

  useEffect(() => {
    if (!setupStatus) return;
    setManagedUploadsDir((prev) => prev || setupStatus.default_paths.managed_uploads_dir);
    setThumbnailsDir((prev) => prev || setupStatus.default_paths.thumbnails_dir);
    setCacheDir((prev) => prev || setupStatus.default_paths.cache_dir);
    setBackupDir((prev) => prev || setupStatus.default_paths.backup_dir);
    setSelectedVisionTier(setupStatus.hardware.recommended_vision_tier);
  }, [setupStatus]);

  if (!setupStatus) {
    return (
      <div className="auth-viewport" data-tauri-drag-region>
        <div className="auth-shell">
          <div className="card card-body stack">
            <div className="view-loading" role="status">
              Loading setup status…
            </div>
            <button type="button" className="button" onClick={() => void bootstrap()}>
              Retry
            </button>
          </div>
        </div>
      </div>
    );
  }

  const handleStep2Next = (event: React.FormEvent) => {
    event.preventDefault();
    setError(null);
    const displayName = adminDisplayName.trim();
    const derivedUser = displayName
      .toLowerCase()
      .replace(/[^a-z0-9._-]+/g, '')
      .slice(0, 32);
    const username = adminUsername.trim() || derivedUser;

    if (!username) {
      setError('Admin username is required.');
      return;
    }
    if (!/^[a-zA-Z0-9._-]+$/.test(username)) {
      setError('Username may only contain letters, numbers, dots, underscores, or hyphens.');
      return;
    }
    if (!displayName) {
      setError('Display name is required.');
      return;
    }
    if (!DIGIT_4.test(profilePin)) {
      setError('Profile PIN must be exactly 4 digits.');
      return;
    }
    if (profilePin !== confirmProfilePin) {
      setError('Profile PIN confirmation does not match.');
      return;
    }
    if (!DIGIT_6.test(serverPin)) {
      setError('Server PIN must be exactly 6 digits.');
      return;
    }
    if (serverPin !== confirmServerPin) {
      setError('Server PIN confirmation does not match.');
      return;
    }

    if (!adminUsername.trim()) {
      setAdminUsername(username);
    }
    setStep(3);
  };

  const handlePickInitialFolder = async () => {
    try {
      const picked = await pickFolderDialog('Select your photo folder');
      if (picked) {
        setInitialFolderPath(picked);
        if (!initialFolderName.trim()) {
          setInitialFolderName(basename(picked));
        }
      }
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const startInitialTasksInBackground = () => {
    const firstFolder = useAppStore.getState().folders[0];
    if (firstFolder) {
      void scanFolder(firstFolder.id).catch(() => {});
    }

    if (!autoDownloadAi) return;

    const visionModelId = visionModelIdForTier(selectedVisionTier);
    const modelsToDownload = [
      visionModelId,
      'yunet',
      'auraface-v1',
      'pp-ocrv6-det',
      'pp-ocrv6-rec',
    ];
    if (includeChatModel) {
      modelsToDownload.push(
        setupStatus.hardware.recommended_chat_tier === 'tier1' ? 'gemma4-e2b' : 'gemma4-e4b',
      );
    }

    void (async () => {
      try {
        await setAiVisionTier(selectedVisionTier);
      } catch {
        // Ignore tier switch failure during bootstrap
      }
      for (const modelId of modelsToDownload) {
        try {
          await downloadAiModel(modelId);
        } catch {
          // Continue queuing remaining models
        }
      }
      void useAppStore
        .getState()
        .refreshAiDashboard()
        .catch(() => {});
    })();
  };

  const handleComplete = async (event: React.FormEvent) => {
    event.preventDefault();
    setError(null);

    const uploads = managedUploadsDir.trim();
    const thumbs = thumbnailsDir.trim();
    const cache = cacheDir.trim();
    const backup = backupDir.trim();

    if (!uploads || !thumbs || !cache || !backup) {
      setError('All four storage paths are required.');
      return;
    }

    const folderPath = initialFolderPath.trim();
    const folderName = initialFolderName.trim() || (folderPath ? basename(folderPath) : '');

    setSubmitting(true);
    try {
      await completeSetup({
        admin_username: adminUsername.trim(),
        admin_display_name: adminDisplayName.trim(),
        admin_profile_pin: profilePin,
        server_pin: serverPin,
        managed_uploads_dir: uploads,
        thumbnails_dir: thumbs,
        cache_dir: cache,
        backup_dir: backup,
        initial_folder_path: folderPath ? folderPath : null,
        initial_folder_name: folderPath ? folderName : null,
      });
      startInitialTasksInBackground();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="auth-viewport" data-tauri-drag-region>
      <div className="auth-shell is-wide">
        <div className="auth-header">
          <img src="/app-icon.png" width={36} height={36} alt="" />
          <h1>Welcome to TruongPhoto</h1>
          <p>Quick Setup · Step {step} of 3</p>
        </div>

        {error && (
          <div className="error-banner" role="alert">
            {error}
          </div>
        )}

        {step === 1 && (
          <div className="card card-body stack">
            <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}>
              <div>
                <h2 style={{ margin: 0, fontSize: 16 }}>Smart Features &amp; Hardware Tier Review</h2>
                <p className="view-subtitle" style={{ marginTop: 4 }}>
                  All photo search, face recognition, and text scanning run 100% privately on your
                  computer.
                </p>
              </div>
              <Badge tone="ok">100% Offline</Badge>
            </div>

            <div className="setup-ai-box stack" style={{ gap: 12 }}>
              <label
                className="row"
                style={{
                  justifyContent: 'space-between',
                  alignItems: 'flex-start',
                  gap: 12,
                  cursor: 'pointer',
                }}
              >
                <div className="row" style={{ alignItems: 'flex-start', gap: 10 }}>
                  <input
                    type="checkbox"
                    checked={autoDownloadAi}
                    onChange={(e) => setAutoDownloadAi(e.target.checked)}
                    style={{ marginTop: 3 }}
                  />
                  <div>
                    <div className="row" style={{ gap: 6 }}>
                      <Sparkles size={14} />
                      <strong style={{ fontSize: 13 }}>
                        Download Recommended AI Models automatically
                      </strong>
                    </div>
                    <div className="view-subtitle" style={{ marginTop: 3 }}>
                      Starts downloading Smart Photo Search, Face Grouping, and Text Recognition in
                      the background as soon as setup finishes.
                    </div>
                  </div>
                </div>
                <Badge tone={autoDownloadAi ? 'ok' : 'muted'}>
                  {autoDownloadAi ? 'Enabled' : 'Skip for now'}
                </Badge>
              </label>

              {autoDownloadAi && (
                <div className="stack" style={{ gap: 8, paddingTop: 6, borderTop: '1px solid var(--border-subtle)' }}>
                  <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}>
                    <span style={{ fontSize: 12, fontWeight: 500 }}>AI Quality Preset</span>
                    <span className="view-subtitle" style={{ marginTop: 0 }}>
                      {presetSummaryLabel(selectedVisionTier)}
                    </span>
                  </div>
                  <div className="preset-pill-grid">
                    {(
                      [
                        ['tier1', 'Fast', 'Smallest download (~660 MB)'],
                        ['tier2', 'Balanced', 'Recommended (~1.5 GB)'],
                        ['tier3', 'Max Accuracy', 'Best for large libraries (~2.0 GB)'],
                      ] as const
                    ).map(([tier, title, desc]) => {
                      const active = selectedVisionTier === tier;
                      const isRec = setupStatus.hardware.recommended_vision_tier === tier;
                      return (
                        <button
                          key={tier}
                          type="button"
                          className={`preset-choice-card ${active ? 'is-selected' : ''}`}
                          onClick={() => setSelectedVisionTier(tier)}
                        >
                          <div className="row" style={{ justifyContent: 'space-between', width: '100%' }}>
                            <strong>{title}</strong>
                            {isRec && <Badge tone="info">Suggested</Badge>}
                          </div>
                          <span className="view-subtitle" style={{ marginTop: 2 }}>
                            {desc}
                          </span>
                        </button>
                      );
                    })}
                  </div>

                  <label className="row" style={{ gap: 8, fontSize: 12, cursor: 'pointer', paddingTop: 2 }}>
                    <input
                      type="checkbox"
                      checked={includeChatModel}
                      onChange={(e) => setIncludeChatModel(e.target.checked)}
                    />
                    <span>
                      Also download Local AI Chat Assistant (~1.6–2.8 GB) — optional, built-in smart
                      search rules already work without this download.
                    </span>
                  </label>
                </div>
              )}
            </div>

            <div className="advanced-accordion">
              <button
                type="button"
                className="advanced-accordion-trigger"
                aria-expanded={showHardwareDetails}
                onClick={() => setShowHardwareDetails((v) => !v)}
              >
                <span>Detected System Hardware ({formatBytes(setupStatus.hardware.total_ram_bytes)} RAM · {setupStatus.hardware.cpu_cores} cores)</span>
                {showHardwareDetails ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
              </button>
              <div className={`advanced-accordion-body ${showHardwareDetails ? 'is-open' : ''}`}>
                <div className="property-field" style={{ marginTop: 8 }}>
                  <div className="property-row">
                    <span>System Memory</span>
                    <span className="property-value">
                      {formatBytes(setupStatus.hardware.total_ram_bytes)}
                    </span>
                  </div>
                  <div className="property-row">
                    <span>CPU Cores</span>
                    <span className="property-value">{setupStatus.hardware.cpu_cores}</span>
                  </div>
                  <div className="property-row">
                    <span>Recommended Vision Tier</span>
                    <Badge tone="info">{setupStatus.hardware.recommended_vision_tier}</Badge>
                  </div>
                  <div className="property-row">
                    <span>Recommended Chat Tier</span>
                    <Badge tone="info">{setupStatus.hardware.recommended_chat_tier}</Badge>
                  </div>
                </div>
              </div>
            </div>

            {!setupStatus.is_direct_loopback && (
              <div className="offline-notice" role="status" style={{ marginBottom: 0 }}>
                Initial setup must be completed from the host machine (direct loopback).
              </div>
            )}

            <div className="confirm-actions">
              <button
                type="button"
                className="button button-primary"
                onClick={() => {
                  setError(null);
                  setStep(2);
                }}
              >
                Continue
              </button>
            </div>
          </div>
        )}

        {step === 2 && (
          <form className="card card-body stack" onSubmit={handleStep2Next} noValidate>
            <h2 style={{ margin: 0, fontSize: 16 }}>Admin Profile &amp; PINs</h2>
            <p className="view-subtitle" style={{ marginTop: 0 }}>
              Set your name, a 4-digit personal PIN, and a 6-digit Server PIN used when connecting
              from your phone or another computer on Wi-Fi.
            </p>

            <div className="row" style={{ alignItems: 'stretch', gap: 12, flexWrap: 'wrap' }}>
              <label className="settings-field" style={{ flex: 1, minWidth: 180 }}>
                <span>Display Name</span>
                <input
                  className="text-input"
                  type="text"
                  autoComplete="name"
                  placeholder="Ian"
                  value={adminDisplayName}
                  onChange={(e) => {
                    const nextName = e.target.value;
                    setAdminDisplayName(nextName);
                    if (!usernameTouched) {
                      const slug = nextName
                        .trim()
                        .toLowerCase()
                        .replace(/[^a-z0-9._-]+/g, '')
                        .slice(0, 32);
                      setAdminUsername(slug);
                    }
                  }}
                />
              </label>

              <label className="settings-field" style={{ flex: 1, minWidth: 180 }}>
                <span>Admin Username</span>
                <input
                  className="text-input"
                  type="text"
                  autoComplete="username"
                  placeholder="ian"
                  value={adminUsername}
                  onChange={(e) => {
                    setUsernameTouched(true);
                    setAdminUsername(e.target.value);
                  }}
                />
              </label>
            </div>

            <div className="row" style={{ alignItems: 'stretch', gap: 12, flexWrap: 'wrap' }}>
              <label className="settings-field" style={{ flex: 1, minWidth: 160 }}>
                <span>4-Digit Profile PIN</span>
                <input
                  className="text-input"
                  type="password"
                  inputMode="numeric"
                  maxLength={4}
                  placeholder="••••"
                  value={profilePin}
                  onChange={(e) => setProfilePin(e.target.value)}
                />
              </label>
              <label className="settings-field" style={{ flex: 1, minWidth: 160 }}>
                <span>Confirm Profile PIN</span>
                <input
                  className="text-input"
                  type="password"
                  inputMode="numeric"
                  maxLength={4}
                  placeholder="••••"
                  value={confirmProfilePin}
                  onChange={(e) => setConfirmProfilePin(e.target.value)}
                />
              </label>
            </div>

            <div className="row" style={{ alignItems: 'stretch', gap: 12, flexWrap: 'wrap' }}>
              <label className="settings-field" style={{ flex: 1, minWidth: 160 }}>
                <span>6-Digit Server PIN</span>
                <input
                  className="text-input"
                  type="password"
                  inputMode="numeric"
                  maxLength={6}
                  placeholder="••••••"
                  value={serverPin}
                  onChange={(e) => setServerPin(e.target.value)}
                />
              </label>
              <label className="settings-field" style={{ flex: 1, minWidth: 160 }}>
                <span>Confirm Server PIN</span>
                <input
                  className="text-input"
                  type="password"
                  inputMode="numeric"
                  maxLength={6}
                  placeholder="••••••"
                  value={confirmServerPin}
                  onChange={(e) => setConfirmServerPin(e.target.value)}
                />
              </label>
            </div>

            <div className="confirm-actions">
              <button
                type="button"
                className="button"
                onClick={() => {
                  setError(null);
                  setStep(1);
                }}
              >
                Back
              </button>
              <button type="submit" className="button button-primary">
                Continue
              </button>
            </div>
          </form>
        )}

        {step === 3 && (
          <form className="card card-body stack" onSubmit={(e) => void handleComplete(e)} noValidate>
            <h2 style={{ margin: 0, fontSize: 16 }}>Storage Paths &amp; Initial Folder</h2>
            <p className="view-subtitle" style={{ marginTop: 0 }}>
              Choose a photo folder to scan right away. You can add more folders or upload directly
              at any time.
            </p>

            <div className="settings-field">
              <span>Photo Folder (Optional)</span>
              <div className="row">
                <input
                  className="text-input"
                  type="text"
                  placeholder="/Users/ian/Pictures"
                  value={initialFolderPath}
                  onChange={(e) => {
                    const nextPath = e.target.value;
                    setInitialFolderPath(nextPath);
                    if (!initialFolderName.trim() && nextPath.trim()) {
                      setInitialFolderName(basename(nextPath.trim()));
                    }
                  }}
                />
                {isTauri() && (
                  <button
                    type="button"
                    className="button button-primary"
                    onClick={() => void handlePickInitialFolder()}
                  >
                    <FolderOpen size={14} />
                    Browse…
                  </button>
                )}
              </div>
            </div>

            {initialFolderPath.trim() && (
              <label className="settings-field">
                <span>Folder Nickname</span>
                <input
                  className="text-input"
                  type="text"
                  placeholder="Pictures"
                  value={initialFolderName}
                  onChange={(e) => setInitialFolderName(e.target.value)}
                />
              </label>
            )}

            <label
              className="row setup-ai-box"
              style={{
                justifyContent: 'space-between',
                gap: 10,
                cursor: 'pointer',
              }}
            >
              <div className="row" style={{ gap: 8 }}>
                <input
                  type="checkbox"
                  checked={autoDownloadAi}
                  onChange={(e) => setAutoDownloadAi(e.target.checked)}
                />
                <span style={{ fontSize: 12 }}>
                  Download recommended AI models in the background ({presetSummaryLabel(selectedVisionTier)})
                </span>
              </div>
              <Badge tone={autoDownloadAi ? 'ok' : 'muted'}>
                {autoDownloadAi ? 'Auto-Download' : 'Manual'}
              </Badge>
            </label>

            <div className="advanced-accordion">
              <button
                type="button"
                className="advanced-accordion-trigger"
                aria-expanded={showAdvancedStorage}
                onClick={() => setShowAdvancedStorage((v) => !v)}
              >
                <span>Advanced Internal Storage Locations (Configured Automatically)</span>
                {showAdvancedStorage ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
              </button>
              <div className={`advanced-accordion-body ${showAdvancedStorage ? 'is-open' : ''}`}>
                <div className="stack" style={{ gap: 10, paddingTop: 10 }}>
                  <label className="settings-field">
                    <span>Managed Uploads Directory</span>
                    <input
                      className="text-input"
                      type="text"
                      value={managedUploadsDir}
                      onChange={(e) => setManagedUploadsDir(e.target.value)}
                    />
                  </label>

                  <label className="settings-field">
                    <span>Thumbnails Directory</span>
                    <input
                      className="text-input"
                      type="text"
                      value={thumbnailsDir}
                      onChange={(e) => setThumbnailsDir(e.target.value)}
                    />
                  </label>

                  <label className="settings-field">
                    <span>Cache Directory</span>
                    <input
                      className="text-input"
                      type="text"
                      value={cacheDir}
                      onChange={(e) => setCacheDir(e.target.value)}
                    />
                  </label>

                  <label className="settings-field">
                    <span>Backup Directory</span>
                    <input
                      className="text-input"
                      type="text"
                      value={backupDir}
                      onChange={(e) => setBackupDir(e.target.value)}
                    />
                  </label>
                </div>
              </div>
            </div>

            <div className="confirm-actions">
              <button
                type="button"
                className="button"
                disabled={submitting}
                onClick={() => {
                  setError(null);
                  setStep(2);
                }}
              >
                Back
              </button>
              <button type="submit" className="button button-primary" disabled={submitting}>
                {submitting ? 'Setting Up…' : 'Complete Setup'}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
};
