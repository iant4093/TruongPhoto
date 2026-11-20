import React, { useEffect, useState } from 'react';
import { FolderOpen } from 'lucide-react';
import { formatBytes } from '../../lib/format';
import { isTauri, pickFolderDialog } from '../../lib/tauri';
import { errorMessage, useAppStore } from '../../store/useAppStore';
import { Badge } from '../ui';

const DIGIT_4 = /^\d{4}$/;
const DIGIT_6 = /^\d{6}$/;

function basename(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '');
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || 'Photos';
}

export const SetupWizardView: React.FC = () => {
  const setupStatus = useAppStore((s) => s.setupStatus);
  const completeSetup = useAppStore((s) => s.completeSetup);
  const bootstrap = useAppStore((s) => s.bootstrap);

  const [step, setStep] = useState<1 | 2 | 3>(1);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // Step 2: Admin Profile & PINs
  const [adminUsername, setAdminUsername] = useState('');
  const [adminDisplayName, setAdminDisplayName] = useState('');
  const [profilePin, setProfilePin] = useState('');
  const [confirmProfilePin, setConfirmProfilePin] = useState('');
  const [serverPin, setServerPin] = useState('');
  const [confirmServerPin, setConfirmServerPin] = useState('');

  // Step 3: Storage Paths & Optional Initial Folder
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

  useEffect(() => {
    if (!setupStatus) return;
    setManagedUploadsDir((prev) => prev || setupStatus.default_paths.managed_uploads_dir);
    setThumbnailsDir((prev) => prev || setupStatus.default_paths.thumbnails_dir);
    setCacheDir((prev) => prev || setupStatus.default_paths.cache_dir);
    setBackupDir((prev) => prev || setupStatus.default_paths.backup_dir);
  }, [setupStatus]);

  if (!setupStatus) {
    return (
      <div className="auth-viewport">
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
    const username = adminUsername.trim();
    const displayName = adminDisplayName.trim();

    if (!username) {
      setError('Admin username is required.');
      return;
    }
    if (!/^[a-zA-Z0-9._-]+$/.test(username)) {
      setError('Username may only contain letters, numbers, dots, underscores, or hyphens.');
      return;
    }
    if (!displayName) {
      setError('Admin display name is required.');
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

    setStep(3);
  };

  const handlePickInitialFolder = async () => {
    try {
      const picked = await pickFolderDialog('Select initial photo folder');
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
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="auth-viewport">
      <div className="auth-shell is-wide">
        <div className="auth-header">
          <img src="/app-icon.png" width={36} height={36} alt="" />
          <h1>TruongPhoto First-Run Setup</h1>
          <p>Step {step} of 3</p>
        </div>

        {error && (
          <div className="error-banner" role="alert">
            {error}
          </div>
        )}

        {step === 1 && (
          <div className="card card-body stack">
            <h2 style={{ margin: 0, fontSize: 16 }}>Hardware Tier Review</h2>
            <p className="view-subtitle" style={{ marginTop: 0 }}>
              TruongPhoto detected your host hardware and selected local AI recommendations for
              upcoming Phases 2–3.
            </p>

            <div className="property-field" style={{ marginBottom: 4 }}>
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
              <div className="property-row">
                <span>Connection</span>
                <Badge tone={setupStatus.is_direct_loopback ? 'ok' : 'warn'}>
                  {setupStatus.is_direct_loopback ? 'Direct loopback' : 'Remote'}
                </Badge>
              </div>
            </div>

            <div
              className="stack"
              style={{
                gap: 6,
                padding: '10px 12px',
                borderRadius: 6,
                border: '1px solid var(--border)',
                background: '#1c1c1c',
              }}
            >
              <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
                <strong style={{ fontSize: 12 }}>Offline AI Model Lineup (100% Apache-2.0 / MIT)</strong>
                <Badge tone="ok">On-Device</Badge>
              </div>
              <div className="property-row">
                <span style={{ fontSize: 12 }}>SigLIP2 (Vision &amp; Zero-Shot Tags)</span>
                <Badge>Apache-2.0</Badge>
              </div>
              <div className="property-row">
                <span style={{ fontSize: 12 }}>YuNet + AuraFace-v1 (Faces &amp; Clustering)</span>
                <Badge>MIT / Apache-2.0</Badge>
              </div>
              <div className="property-row">
                <span style={{ fontSize: 12 }}>PP-OCRv6 (Document &amp; Scene Text)</span>
                <Badge>Apache-2.0</Badge>
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
              Create your primary Administrator account, 4-digit Profile PIN, and shared 6-digit
              Server PIN for LAN/Tailscale access.
            </p>

            <label className="settings-field">
              <span>Admin Username</span>
              <input
                className="text-input"
                type="text"
                autoComplete="username"
                placeholder="ian"
                value={adminUsername}
                onChange={(e) => setAdminUsername(e.target.value)}
              />
            </label>

            <label className="settings-field">
              <span>Display Name</span>
              <input
                className="text-input"
                type="text"
                autoComplete="name"
                placeholder="Ian"
                value={adminDisplayName}
                onChange={(e) => setAdminDisplayName(e.target.value)}
              />
            </label>

            <div className="row" style={{ alignItems: 'stretch', gap: 12 }}>
              <label className="settings-field" style={{ flex: 1 }}>
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
              <label className="settings-field" style={{ flex: 1 }}>
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

            <div className="row" style={{ alignItems: 'stretch', gap: 12 }}>
              <label className="settings-field" style={{ flex: 1 }}>
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
              <label className="settings-field" style={{ flex: 1 }}>
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
              Confirm where TruongPhoto stores uploads, thumbnails, cache, and SQLite backups.
            </p>

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

            <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
              <div className="settings-field">
                <span>First Watched Folder (Optional)</span>
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
                      className="button"
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
                  <span>Folder Display Name</span>
                  <input
                    className="text-input"
                    type="text"
                    placeholder="Pictures"
                    value={initialFolderName}
                    onChange={(e) => setInitialFolderName(e.target.value)}
                  />
                </label>
              )}
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
                {submitting ? 'Initializing…' : 'Complete Setup'}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
};
