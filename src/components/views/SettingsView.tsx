import React, { useEffect, useState } from 'react';
import {
  Check,
  ChevronDown,
  ChevronUp,
  Copy,
  DatabaseBackup,
  Download,
  ExternalLink,
  FolderOpen,
  FolderPlus,
  LogOut,
  MapPin,
  RefreshCw,
  Trash2,
  UserPlus,
} from 'lucide-react';
import {
  checkForUpdates,
  createAdminUser,
  createFolder,
  deleteAdminUser,
  deleteFolder,
  disableTailscaleServe,
  downloadAiModel,
  enableTailscaleServe,
  getMapConfig,
  getTailscaleStatus,
  listBackups,
  scanFolder,
  systemQrUrl,
  triggerBackup,
  updateAdminSettings,
  updateAdminUser,
  updateFolder,
  updateServerPin,
} from '../../lib/api';
import { formatBytes, formatDateTime, plural } from '../../lib/format';
import {
  checkUpdateFeed,
  getAutostartEnabled,
  isTauri,
  openExternalUrl,
  pickFolderDialog,
  setAutostartEnabled,
} from '../../lib/tauri';
import { confirmAction, notifyError, notifyInfo, notifySuccess } from '../../store/feedback';
import { useAppStore } from '../../store/useAppStore';
import type {
  AdminUserView,
  BackupResult,
  BackupSnapshotView,
  FolderView,
  MapConfigResponse,
  MapTileMode,
  Role,
  TailscaleStatus,
  UpdateCheckResponse,
} from '../../types/api';
import { AiModelsPanel } from '../AiModelsPanel';
import { Dropdown } from '../Dropdown';
import { Badge, SectionTitle, SegmentedControl, ToggleRow, ViewHeader } from '../ui';

const DIGIT_4 = /^\d{4}$/;
const DIGIT_6 = /^\d{6}$/;

const ROLE_OPTIONS: ReadonlyArray<readonly [Role, string]> = [
  ['viewer', 'Viewer'],
  ['contributor', 'Contributor'],
  ['admin', 'Admin'],
];

const MAP_TILE_MODE_OPTIONS: ReadonlyArray<readonly [MapTileMode, string]> = [
  ['offline_pmtiles', 'Offline (PMTiles)'],
  ['online_openfreemap', 'Online (OpenFreeMap)'],
];

function basename(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '');
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || 'Folder';
}

export const SettingsView: React.FC = () => {
  const user = useAppStore((s) => s.user);
  const allowedFolderIds = useAppStore((s) => s.allowedFolderIds);
  const folders = useAppStore((s) => s.folders);
  const systemInfo = useAppStore((s) => s.systemInfo);
  const adminUsers = useAppStore((s) => s.adminUsers);
  const adminSettings = useAppStore((s) => s.adminSettings);
  const activeJobs = useAppStore((s) => s.activeJobs);
  const storeTailscaleStatus = useAppStore((s) => s.tailscaleStatus);
  const logout = useAppStore((s) => s.logout);
  const refreshFolders = useAppStore((s) => s.refreshFolders);
  const refreshAdminData = useAppStore((s) => s.refreshAdminData);
  const refreshSystemInfo = useAppStore((s) => s.refreshSystemInfo);
  const refreshAiDashboard = useAppStore((s) => s.refreshAiDashboard);

  const isAdmin = user?.role === 'admin';
  const [showAdvancedSettings, setShowAdvancedSettings] = useState(false);
  const [scanningFolderIds, setScanningFolderIds] = useState<Record<string, boolean>>({});
  const [jobFolderMap, setJobFolderMap] = useState<Record<number, string>>({});

  // Add Folder state
  const [folderName, setFolderName] = useState('');
  const [folderPath, setFolderPath] = useState('');
  const [folderDiskDelete, setFolderDiskDelete] = useState(false);
  const [addingFolder, setAddingFolder] = useState(false);

  // Create User state
  const [newUsername, setNewUsername] = useState('');
  const [newDisplayName, setNewDisplayName] = useState('');
  const [newRole, setNewRole] = useState<Role>('viewer');
  const [newProfilePin, setNewProfilePin] = useState('');
  const [newAllowedFolders, setNewAllowedFolders] = useState<string[]>([]);
  const [creatingUser, setCreatingUser] = useState(false);

  // Edit User state
  const [editingUserId, setEditingUserId] = useState<string | null>(null);
  const [editDisplayName, setEditDisplayName] = useState('');
  const [editRole, setEditRole] = useState<Role>('viewer');
  const [editPin, setEditPin] = useState('');
  const [editAllowedFolders, setEditAllowedFolders] = useState<string[]>([]);
  const [savingUser, setSavingUser] = useState(false);

  // Server PIN state
  const [currentServerPin, setCurrentServerPin] = useState('');
  const [nextServerPin, setNextServerPin] = useState('');
  const [confirmNextServerPin, setConfirmNextServerPin] = useState('');
  const [savingServerPin, setSavingServerPin] = useState(false);

  // Desktop / Network / Tailscale state
  const [autostartEnabled, setAutostartState] = useState(false);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [updateResult, setUpdateResult] = useState<UpdateCheckResponse | null>(null);
  const [copiedUrl, setCopiedUrl] = useState<string | null>(null);
  const [tailscaleStatus, setTailscaleStatus] = useState<TailscaleStatus | null>(
    storeTailscaleStatus,
  );
  const [tailscaleBusy, setTailscaleBusy] = useState(false);

  // Storage & Backup state
  const [uploadsDir, setUploadsDir] = useState(adminSettings?.managed_uploads_dir ?? '');
  const [thumbsDir, setThumbsDir] = useState(adminSettings?.thumbnails_dir ?? '');
  const [cacheDir, setCacheDir] = useState(adminSettings?.cache_dir ?? '');
  const [cacheLimitGb, setCacheLimitGb] = useState(
    String(adminSettings?.cache_limit_gb ?? 10),
  );
  const [backupDir, setBackupDir] = useState(adminSettings?.backup_dir ?? '');
  const [backupKeepCount, setBackupKeepCount] = useState(
    String(adminSettings?.backup_keep_count ?? 7),
  );
  const [backupIntervalHours, setBackupIntervalHours] = useState(
    String(adminSettings?.backup_interval_hours ?? 24),
  );
  const [backupHistory, setBackupHistory] = useState<BackupSnapshotView[]>([]);
  const [savingStorage, setSavingStorage] = useState(false);
  const [backingUp, setBackingUp] = useState(false);
  const [lastBackup, setLastBackup] = useState<BackupResult | null>(null);

  // Power & Thermal Throttling state
  const [cpuThrottlePercent, setCpuThrottlePercent] = useState(
    String(adminSettings?.cpu_throttle_percent ?? 80),
  );
  const [savingThrottling, setSavingThrottling] = useState(false);

  // Map Basemap state (Admin-only)
  const [mapConfig, setMapConfig] = useState<MapConfigResponse | null>(null);
  const [savingMapMode, setSavingMapMode] = useState(false);
  const [downloadingBasemap, setDownloadingBasemap] = useState(false);

  const srvUploadsDir = adminSettings?.managed_uploads_dir;
  const srvThumbsDir = adminSettings?.thumbnails_dir;
  const srvCacheDir = adminSettings?.cache_dir;
  const srvCacheLimitGb = adminSettings?.cache_limit_gb;
  const srvBackupDir = adminSettings?.backup_dir;
  const srvBackupKeepCount = adminSettings?.backup_keep_count;
  const srvBackupIntervalHours = adminSettings?.backup_interval_hours;
  const srvCpuThrottlePercent = adminSettings?.cpu_throttle_percent;

  useEffect(() => {
    if (srvUploadsDir !== undefined) setUploadsDir(srvUploadsDir);
  }, [srvUploadsDir]);
  useEffect(() => {
    if (srvThumbsDir !== undefined) setThumbsDir(srvThumbsDir);
  }, [srvThumbsDir]);
  useEffect(() => {
    if (srvCacheDir !== undefined) setCacheDir(srvCacheDir);
  }, [srvCacheDir]);
  useEffect(() => {
    if (srvCacheLimitGb !== undefined) setCacheLimitGb(String(srvCacheLimitGb));
  }, [srvCacheLimitGb]);
  useEffect(() => {
    if (srvBackupDir !== undefined) setBackupDir(srvBackupDir);
  }, [srvBackupDir]);
  useEffect(() => {
    if (srvBackupKeepCount !== undefined) setBackupKeepCount(String(srvBackupKeepCount));
  }, [srvBackupKeepCount]);
  useEffect(() => {
    if (srvBackupIntervalHours !== undefined) {
      setBackupIntervalHours(String(srvBackupIntervalHours ?? 24));
    }
  }, [srvBackupIntervalHours]);
  useEffect(() => {
    if (srvCpuThrottlePercent !== undefined) {
      setCpuThrottlePercent(String(srvCpuThrottlePercent ?? 80));
    }
  }, [srvCpuThrottlePercent]);

  useEffect(() => {
    if (storeTailscaleStatus) {
      setTailscaleStatus(storeTailscaleStatus);
    }
  }, [storeTailscaleStatus]);

  useEffect(() => {
    if (!isAdmin || !isTauri()) return;
    let alive = true;
    void getAutostartEnabled()
      .then((enabled) => {
        if (alive) setAutostartState(enabled);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [isAdmin]);

  useEffect(() => {
    if (!isAdmin) return;
    let alive = true;
    void getMapConfig()
      .then((cfg) => {
        if (alive) setMapConfig(cfg);
      })
      .catch(() => {});
    void getTailscaleStatus()
      .then((status) => {
        if (alive) setTailscaleStatus(status);
      })
      .catch(() => {});
    void listBackups()
      .then((res) => {
        if (alive) setBackupHistory(res.items);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [isAdmin]);

  const allowedFolderNames =
    user?.role === 'admin' || allowedFolderIds === null
      ? null
      : folders
          .filter((f) => allowedFolderIds.includes(f.id))
          .map((f) => f.name);

  const toggleAclFolder = (
    list: string[],
    setList: React.Dispatch<React.SetStateAction<string[]>>,
    folderId: string,
  ) => {
    setList(
      list.includes(folderId) ? list.filter((id) => id !== folderId) : [...list, folderId],
    );
  };

  const handleBrowseFolder = async () => {
    try {
      const picked = await pickFolderDialog('Select watched folder');
      if (picked) {
        setFolderPath(picked);
        if (!folderName.trim()) {
          setFolderName(basename(picked));
        }
      }
    } catch (err) {
      notifyError(err, 'Folder picker failed');
    }
  };

  const handleAddFolder = async (event: React.FormEvent) => {
    event.preventDefault();
    const rootPath = folderPath.trim();
    const name = folderName.trim() || (rootPath ? basename(rootPath) : '');
    if (!rootPath || !name) {
      notifyError(new Error('Folder name and path are required.'));
      return;
    }

    setAddingFolder(true);
    try {
      await createFolder({
        name,
        root_path: rootPath,
        allow_disk_delete: folderDiskDelete,
      });
      await Promise.all([refreshFolders(), refreshSystemInfo()]);
      setFolderName('');
      setFolderPath('');
      setFolderDiskDelete(false);
      notifySuccess(`Added folder "${name}"`);
    } catch (err) {
      notifyError(err, 'Could not add folder');
    } finally {
      setAddingFolder(false);
    }
  };

  const handleToggleDiskDelete = async (folder: FolderView, next: boolean) => {
    try {
      await updateFolder(folder.id, { allow_disk_delete: next });
      await refreshFolders();
      notifySuccess(
        next
          ? `Disk deletion enabled for "${folder.name}"`
          : `Disk deletion disabled for "${folder.name}"`,
      );
    } catch (err) {
      notifyError(err, 'Could not update folder');
    }
  };

  const handleRemoveFolder = async (folder: FolderView) => {
    const ok = await confirmAction(
      `Remove "${folder.name}" from TruongPhoto?`,
      'Remove Folder',
      'This removes the folder from the library database without deleting any files on disk.',
    );
    if (!ok) return;
    try {
      await deleteFolder(folder.id);
      await Promise.all([refreshFolders(), refreshSystemInfo()]);
      notifySuccess(`Removed folder "${folder.name}"`);
    } catch (err) {
      notifyError(err, 'Could not remove folder');
    }
  };

  const handleScanFolder = async (folder: FolderView) => {
    setScanningFolderIds((prev) => ({ ...prev, [folder.id]: true }));
    try {
      const res = await scanFolder(folder.id);
      setJobFolderMap((prev) => ({ ...prev, [res.job_id]: folder.id }));
      notifySuccess(`Started scan for "${folder.name}"`);
    } catch (err) {
      notifyError(err, `Could not scan "${folder.name}"`);
    } finally {
      setScanningFolderIds((prev) => ({ ...prev, [folder.id]: false }));
    }
  };

  const handleCreateUser = async (event: React.FormEvent) => {
    event.preventDefault();
    const username = newUsername.trim();
    const displayName = newDisplayName.trim();
    if (!username || !displayName) {
      notifyError(new Error('Username and display name are required.'));
      return;
    }
    if (!DIGIT_4.test(newProfilePin)) {
      notifyError(new Error('Profile PIN must be exactly 4 digits.'));
      return;
    }

    setCreatingUser(true);
    try {
      await createAdminUser({
        username,
        display_name: displayName,
        role: newRole,
        profile_pin: newProfilePin,
        allowed_folder_ids: newRole === 'admin' ? [] : newAllowedFolders,
      });
      await refreshAdminData();
      setNewUsername('');
      setNewDisplayName('');
      setNewRole('viewer');
      setNewProfilePin('');
      setNewAllowedFolders([]);
      notifySuccess(`Created user "${displayName}"`);
    } catch (err) {
      notifyError(err, 'Could not create user');
    } finally {
      setCreatingUser(false);
    }
  };

  const startEditUser = (target: AdminUserView) => {
    setEditingUserId(target.id);
    setEditDisplayName(target.display_name);
    setEditRole(target.role);
    setEditPin('');
    setEditAllowedFolders(target.allowed_folder_ids);
  };

  const handleSaveUserEdit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!editingUserId) return;
    if (!editDisplayName.trim()) {
      notifyError(new Error('Display name is required.'));
      return;
    }
    if (editPin && !DIGIT_4.test(editPin)) {
      notifyError(new Error('New Profile PIN must be exactly 4 digits.'));
      return;
    }

    setSavingUser(true);
    try {
      await updateAdminUser(editingUserId, {
        display_name: editDisplayName.trim(),
        role: editRole,
        ...(editPin ? { profile_pin: editPin } : {}),
        allowed_folder_ids: editRole === 'admin' ? [] : editAllowedFolders,
      });
      await refreshAdminData();
      setEditingUserId(null);
      setEditPin('');
      notifySuccess('Updated user permissions');
    } catch (err) {
      notifyError(err, 'Could not update user');
    } finally {
      setSavingUser(false);
    }
  };

  const handleToggleUserDisabled = async (target: AdminUserView) => {
    const nextDisabled = target.disabled_at_ms === null;
    try {
      await updateAdminUser(target.id, { disabled: nextDisabled });
      await refreshAdminData();
      notifySuccess(
        nextDisabled
          ? `Disabled "${target.display_name}"`
          : `Re-enabled "${target.display_name}"`,
      );
    } catch (err) {
      notifyError(err, 'Could not update user status');
    }
  };

  const handleDeleteUser = async (target: AdminUserView) => {
    const ok = await confirmAction(
      `Delete user "${target.display_name}"?`,
      'Delete User',
      `@${target.username} will immediately lose access to TruongPhoto.`,
    );
    if (!ok) return;
    try {
      await deleteAdminUser(target.id);
      await refreshAdminData();
      if (editingUserId === target.id) setEditingUserId(null);
      notifySuccess(`Deleted user "${target.display_name}"`);
    } catch (err) {
      notifyError(err, 'Could not delete user');
    }
  };

  const handleUpdateServerPin = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!DIGIT_6.test(currentServerPin) || !DIGIT_6.test(nextServerPin)) {
      notifyError(new Error('Both current and new Server PINs must be 6 digits.'));
      return;
    }
    if (nextServerPin !== confirmNextServerPin) {
      notifyError(new Error('New Server PIN confirmation does not match.'));
      return;
    }

    setSavingServerPin(true);
    try {
      await updateServerPin({
        current_server_pin: currentServerPin,
        new_server_pin: nextServerPin,
      });
      setCurrentServerPin('');
      setNextServerPin('');
      setConfirmNextServerPin('');
      notifySuccess('Updated 6-digit Server PIN');
    } catch (err) {
      notifyError(err, 'Could not update Server PIN');
    } finally {
      setSavingServerPin(false);
    }
  };

  const handleToggleSetting = async (
    key: 'mdns_enabled' | 'desktop_pin_lock' | 'pause_on_battery',
    next: boolean,
  ) => {
    try {
      await updateAdminSettings({ [key]: next });
      await Promise.all([refreshAdminData(), refreshSystemInfo()]);
      notifySuccess('Updated settings');
    } catch (err) {
      notifyError(err, 'Could not update settings');
    }
  };

  const handleToggleTailscale = async () => {
    if (tailscaleBusy) return;
    setTailscaleBusy(true);
    try {
      const next = tailscaleStatus?.serve_active
        ? await disableTailscaleServe()
        : await enableTailscaleServe();
      setTailscaleStatus(next);
      useAppStore.setState({ tailscaleStatus: next });
      void refreshSystemInfo().catch(() => {});
      notifySuccess(
        next.serve_active ? 'Enabled Tailscale Remote HTTPS' : 'Disabled Tailscale Remote HTTPS',
      );
    } catch (err) {
      notifyError(err, 'Could not update Tailscale Remote HTTPS');
    } finally {
      setTailscaleBusy(false);
    }
  };

  const handleBackupIntervalChange = async (value: string) => {
    setBackupIntervalHours(value);
    const hours = Number.parseInt(value, 10);
    if (!Number.isFinite(hours) || hours < 0) return;
    try {
      await updateAdminSettings({ backup_interval_hours: hours });
      await refreshAdminData();
      notifySuccess('Updated backup schedule');
    } catch (err) {
      notifyError(err, 'Could not update backup schedule');
    }
  };

  const handleSaveCpuThreshold = async (event: React.FormEvent) => {
    event.preventDefault();
    const parsed = Number.parseInt(cpuThrottlePercent, 10);
    if (!Number.isFinite(parsed)) {
      notifyError(new Error('CPU throttle threshold must be between 30% and 100%.'));
      return;
    }
    const clamped = Math.min(100, Math.max(30, parsed));
    setCpuThrottlePercent(String(clamped));
    setSavingThrottling(true);
    try {
      await updateAdminSettings({ cpu_throttle_percent: clamped });
      await refreshAdminData();
      notifySuccess(`Updated CPU throttle threshold to ${clamped}%`);
    } catch (err) {
      notifyError(err, 'Could not update CPU throttle threshold');
    } finally {
      setSavingThrottling(false);
    }
  };

  const handleToggleAutostart = async (next: boolean) => {
    try {
      await setAutostartEnabled(next);
      setAutostartState(next);
      notifySuccess(next ? 'Launch at login enabled' : 'Launch at login disabled');
    } catch (err) {
      notifyError(err, 'Could not update Launch at Login');
    }
  };

  const handleCheckUpdates = async () => {
    setCheckingUpdates(true);
    try {
      const ghResult = await checkForUpdates();
      setUpdateResult(ghResult);
      if (ghResult.update_available && ghResult.latest_version) {
        notifySuccess(`Update v${ghResult.latest_version} is available on GitHub Releases.`);
      } else if (ghResult.status === 'up_to_date') {
        notifyInfo(`TruongPhoto v${ghResult.current_version} is up to date.`);
      } else if (ghResult.message) {
        notifyInfo(ghResult.message);
      }
      // Also ping Tauri's native updater feed silently if available.
      if (isTauri()) {
        void checkUpdateFeed().catch(() => {});
      }
    } catch (err) {
      notifyError(err, 'Update check failed');
    } finally {
      setCheckingUpdates(false);
    }
  };

  const handleCopyUrl = async (url: string) => {
    try {
      await navigator.clipboard?.writeText(url);
      setCopiedUrl(url);
      window.setTimeout(() => setCopiedUrl((cur) => (cur === url ? null : cur)), 2000);
    } catch {
      notifyInfo(url);
    }
  };

  const handleSaveStorage = async (event: React.FormEvent) => {
    event.preventDefault();
    const limitGb = Number.parseInt(cacheLimitGb, 10);
    const keepCount = Number.parseInt(backupKeepCount, 10);
    if (!Number.isFinite(limitGb) || limitGb < 1 || !Number.isFinite(keepCount) || keepCount < 1) {
      notifyError(new Error('Cache limit and backup retention must be positive integers.'));
      return;
    }

    setSavingStorage(true);
    try {
      const intervalHours = Number.parseInt(backupIntervalHours, 10);
      await updateAdminSettings({
        managed_uploads_dir: uploadsDir.trim(),
        thumbnails_dir: thumbsDir.trim(),
        cache_dir: cacheDir.trim(),
        cache_limit_gb: limitGb,
        backup_dir: backupDir.trim(),
        backup_keep_count: keepCount,
        ...(Number.isFinite(intervalHours) && intervalHours >= 0
          ? { backup_interval_hours: intervalHours }
          : {}),
      });
      await refreshAdminData();
      notifySuccess('Saved storage configuration');
    } catch (err) {
      notifyError(err, 'Could not save storage settings');
    } finally {
      setSavingStorage(false);
    }
  };

  const handleBackupNow = async () => {
    setBackingUp(true);
    try {
      const result = await triggerBackup();
      setLastBackup(result);
      void listBackups()
        .then((res) => setBackupHistory(res.items))
        .catch(() => {});
      notifySuccess(`Library backup created (${formatBytes(result.size_bytes)})`);
    } catch (err) {
      notifyError(err, 'Backup failed');
    } finally {
      setBackingUp(false);
    }
  };

  const qrTargetUrl =
    (tailscaleStatus?.serve_active ? tailscaleStatus.https_url : null) ??
    systemInfo?.tailscale_url ??
    systemInfo?.mdns_url ??
    systemInfo?.lan_urls[0] ??
    null;

  const effectiveMapTileMode: MapTileMode =
    adminSettings?.map_tile_mode ?? mapConfig?.tile_mode ?? 'offline_pmtiles';

  const handleSetMapTileMode = async (mode: MapTileMode) => {
    if (mode === effectiveMapTileMode || savingMapMode) return;
    setSavingMapMode(true);
    try {
      await updateAdminSettings({ map_tile_mode: mode });
      await Promise.all([
        refreshAdminData(),
        getMapConfig()
          .then((cfg) => setMapConfig(cfg))
          .catch(() => {}),
      ]);
      notifySuccess(
        mode === 'online_openfreemap'
          ? 'Switched map basemap to Online (OpenFreeMap)'
          : 'Switched map basemap to Offline (PMTiles)',
      );
    } catch (err) {
      notifyError(err, 'Could not update map basemap mode');
    } finally {
      setSavingMapMode(false);
    }
  };

  const handleDownloadBasemap = async () => {
    if (downloadingBasemap) return;
    setDownloadingBasemap(true);
    try {
      await downloadAiModel('world-basemap-pmtiles');
      void refreshAiDashboard().catch(() => {});
      notifySuccess('Started download for Offline World Basemap (PMTiles)');
    } catch (err) {
      notifyError(err, 'Could not download world-basemap-pmtiles');
    } finally {
      setDownloadingBasemap(false);
    }
  };

  return (
    <div className="settings-view">
      <ViewHeader
        title="Settings"
        subtitle={
          isAdmin
            ? 'Manage watched folders, user permissions, network discovery, and backups.'
            : 'View your active session and assigned folder access.'
        }
      />

      {/* Account & Session — visible to ALL roles */}
      <SectionTitle>Account &amp; Session</SectionTitle>
      <div className="card card-body stack">
        {user && (
          <>
            <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
              <div className="row" style={{ gap: 10 }}>
                <div
                  className="profile-avatar"
                  style={{ background: user.avatar_color || '#4a4a4a' }}
                >
                  {user.display_name.slice(0, 1).toUpperCase()}
                </div>
                <div>
                  <div style={{ fontWeight: 600 }}>{user.display_name}</div>
                  <div className="view-subtitle" style={{ marginTop: 2 }}>
                    @{user.username}
                  </div>
                </div>
              </div>
              <div className="row">
                <Badge tone={isAdmin ? 'info' : 'muted'}>{user.role}</Badge>
                <button type="button" className="button" onClick={() => void logout()}>
                  <LogOut size={14} />
                  Sign Out
                </button>
              </div>
            </div>

            <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
              <div className="workspace-label" style={{ marginBottom: 6 }}>
                Allowed Folders
              </div>
              {allowedFolderNames === null ? (
                <Badge tone="ok">All folders (Admin)</Badge>
              ) : allowedFolderNames.length === 0 ? (
                <span className="view-subtitle">No folders assigned</span>
              ) : (
                <div className="row" style={{ flexWrap: 'wrap' }}>
                  {allowedFolderNames.map((name) => (
                    <Badge key={name}>{name}</Badge>
                  ))}
                </div>
              )}
            </div>
          </>
        )}
      </div>

      {/* Admin-only sections — strictly omitted for viewer and contributor */}
      {isAdmin && (
        <>
          <SectionTitle>Local AI &amp; Models</SectionTitle>
          <AiModelsPanel />

          <SectionTitle>Watched Folders</SectionTitle>
          <div className="card">
            {Object.values(activeJobs).length > 0 && (
              <div
                className="row"
                style={{
                  padding: '10px 16px',
                  borderBottom: '1px solid var(--border)',
                  flexWrap: 'wrap',
                  gap: 8,
                }}
                role="status"
              >
                {Object.values(activeJobs).map((job) => (
                  <Badge key={job.job_id} tone="info">
                    {job.kind}: {job.processed}
                    {job.total !== null ? ` / ${job.total}` : ''}
                    {job.message ? ` · ${job.message}` : ''}
                  </Badge>
                ))}
              </div>
            )}
            {folders.length === 0 ? (
              <div className="card-body view-subtitle" style={{ marginTop: 0 }}>
                No watched folders added yet.
              </div>
            ) : (
              folders.map((folder) => {
                const folderJob = Object.values(activeJobs).find(
                  (j) => jobFolderMap[j.job_id] === folder.id,
                );
                const isScanning = Boolean(scanningFolderIds[folder.id] || folderJob);
                return (
                  <div key={folder.id} className="folder-row">
                    <div style={{ minWidth: 0, flex: 1 }}>
                      <div className="row" style={{ flexWrap: 'wrap' }}>
                        <strong>{folder.name}</strong>
                        {folder.is_offline ? (
                          <Badge tone="warn">Offline</Badge>
                        ) : (
                          <Badge tone="ok">Online</Badge>
                        )}
                        {folder.is_managed_uploads && <Badge tone="info">Managed Uploads</Badge>}
                        {folder.volume_uuid && (
                          <Badge>Vol: {folder.volume_label || folder.volume_uuid}</Badge>
                        )}
                        <Badge>{plural(folder.asset_count, 'asset')}</Badge>
                        {folderJob && (
                          <Badge tone="info">
                            Scanning {folderJob.processed}
                            {folderJob.total !== null ? ` / ${folderJob.total}` : ''}
                          </Badge>
                        )}
                      </div>
                      <div className="view-subtitle" style={{ marginTop: 4 }}>
                        {folder.root_path}
                      </div>
                    </div>
                    <div className="row" style={{ gap: 12 }}>
                      <button
                        type="button"
                        className="button"
                        disabled={isScanning || folder.is_offline}
                        onClick={() => void handleScanFolder(folder)}
                      >
                        <RefreshCw size={14} />
                        <span>{isScanning ? 'Scanning…' : 'Scan Now'}</span>
                      </button>
                      <label className="row" style={{ fontSize: 12, cursor: 'pointer' }}>
                        <span>Allow Disk Deletion</span>
                        <input
                          className="toggle-switch"
                          type="checkbox"
                          checked={folder.allow_disk_delete}
                          aria-label={`Allow Disk Deletion for ${folder.name}`}
                          onChange={(e) => void handleToggleDiskDelete(folder, e.target.checked)}
                        />
                      </label>
                      <button
                        type="button"
                        className="button button-danger-quiet"
                        onClick={() => void handleRemoveFolder(folder)}
                      >
                        <Trash2 size={14} />
                        Remove
                      </button>
                    </div>
                  </div>
                );
              })
            )}

            <form
              className="card-body stack"
              style={{ borderTop: '1px solid var(--border)' }}
              onSubmit={(e) => void handleAddFolder(e)}
            >
              <strong style={{ fontSize: 13 }}>Add Watched Folder</strong>
              <div className="row" style={{ flexWrap: 'wrap' }}>
                <input
                  className="text-input"
                  style={{ flex: '1 1 180px' }}
                  type="text"
                  placeholder="Folder name (e.g. Family Archive)"
                  aria-label="Folder name"
                  value={folderName}
                  onChange={(e) => setFolderName(e.target.value)}
                />
                <div className="row" style={{ flex: '2 1 260px' }}>
                  <input
                    className="text-input"
                    type="text"
                    placeholder={
                      typeof navigator !== 'undefined' && /Win/i.test(navigator.platform || navigator.userAgent)
                        ? 'D:\\Photos'
                        : '/Volumes/Photos'
                    }
                    aria-label="Folder root path"
                    value={folderPath}
                    onChange={(e) => setFolderPath(e.target.value)}
                  />
                  {isTauri() && (
                    <button
                      type="button"
                      className="button"
                      onClick={() => void handleBrowseFolder()}
                    >
                      <FolderOpen size={14} />
                      Browse…
                    </button>
                  )}
                </div>
              </div>
              <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
                <label className="row" style={{ fontSize: 12, cursor: 'pointer' }}>
                  <input
                    type="checkbox"
                    checked={folderDiskDelete}
                    onChange={(e) => setFolderDiskDelete(e.target.checked)}
                  />
                  <span>Allow Disk Deletion when purging from Trash</span>
                </label>
                <button type="submit" className="button button-primary" disabled={addingFolder}>
                  <FolderPlus size={14} />
                  {addingFolder ? 'Adding…' : 'Add Folder'}
                </button>
              </div>
            </form>
          </div>

          <SectionTitle>Users &amp; Permissions</SectionTitle>
          <div className="card">
            {adminUsers.map((account) => {
              const isSelf = account.id === user?.id;
              const isDisabled = account.disabled_at_ms !== null;
              const accountFolders = folders
                .filter((f) => account.allowed_folder_ids.includes(f.id))
                .map((f) => f.name);

              return (
                <div key={account.id} className="user-row">
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <div className="row" style={{ flexWrap: 'wrap' }}>
                      <strong>{account.display_name}</strong>
                      <span className="view-subtitle" style={{ marginTop: 0 }}>
                        @{account.username}
                      </span>
                      <Badge tone={account.role === 'admin' ? 'info' : 'muted'}>
                        {account.role}
                      </Badge>
                      {isDisabled && <Badge tone="danger">Disabled</Badge>}
                    </div>
                    <div className="row" style={{ flexWrap: 'wrap', marginTop: 6 }}>
                      {account.role === 'admin' ? (
                        <Badge tone="ok">All folders</Badge>
                      ) : accountFolders.length === 0 ? (
                        <Badge tone="warn">No folders assigned</Badge>
                      ) : (
                        accountFolders.map((fname) => <Badge key={fname}>{fname}</Badge>)
                      )}
                    </div>
                  </div>
                  <div className="row">
                    <button
                      type="button"
                      className="button"
                      onClick={() =>
                        editingUserId === account.id
                          ? setEditingUserId(null)
                          : startEditUser(account)
                      }
                    >
                      {editingUserId === account.id ? 'Close' : 'Edit'}
                    </button>
                    {!isSelf && (
                      <>
                        <button
                          type="button"
                          className="button"
                          onClick={() => void handleToggleUserDisabled(account)}
                        >
                          {isDisabled ? 'Enable' : 'Disable'}
                        </button>
                        <button
                          type="button"
                          className="button button-danger-quiet"
                          onClick={() => void handleDeleteUser(account)}
                        >
                          Delete
                        </button>
                      </>
                    )}
                  </div>
                </div>
              );
            })}

            {editingUserId && (
              <form
                className="card-body stack"
                style={{ borderTop: '1px solid var(--border)', background: '#1e1e1e' }}
                onSubmit={(e) => void handleSaveUserEdit(e)}
              >
                <strong style={{ fontSize: 13 }}>Edit User</strong>
                <div className="row" style={{ flexWrap: 'wrap' }}>
                  <input
                    className="text-input"
                    style={{ flex: '1 1 180px' }}
                    type="text"
                    placeholder="Display name"
                    aria-label="Edit display name"
                    value={editDisplayName}
                    onChange={(e) => setEditDisplayName(e.target.value)}
                  />
                  <div style={{ minWidth: 150 }}>
                    <Dropdown
                      label="Edit user role"
                      value={editRole}
                      options={ROLE_OPTIONS}
                      onChange={setEditRole}
                    />
                  </div>
                  <input
                    className="text-input"
                    style={{ width: 140 }}
                    type="password"
                    inputMode="numeric"
                    maxLength={4}
                    placeholder="New 4-digit PIN"
                    aria-label="New 4-digit Profile PIN"
                    value={editPin}
                    onChange={(e) => setEditPin(e.target.value)}
                  />
                </div>

                {editRole !== 'admin' && folders.length > 0 && (
                  <div>
                    <div className="workspace-label">Allowed Folders</div>
                    <div className="acl-chip-grid">
                      {folders.map((folder) => {
                        const checked = editAllowedFolders.includes(folder.id);
                        return (
                          <label
                            key={folder.id}
                            className={`acl-chip ${checked ? 'is-selected' : ''}`}
                          >
                            <input
                              type="checkbox"
                              checked={checked}
                              onChange={() =>
                                toggleAclFolder(editAllowedFolders, setEditAllowedFolders, folder.id)
                              }
                            />
                            <span>{folder.name}</span>
                          </label>
                        );
                      })}
                    </div>
                  </div>
                )}

                <div className="confirm-actions">
                  <button
                    type="button"
                    className="button"
                    onClick={() => setEditingUserId(null)}
                  >
                    Cancel
                  </button>
                  <button type="submit" className="button button-primary" disabled={savingUser}>
                    {savingUser ? 'Saving…' : 'Save User'}
                  </button>
                </div>
              </form>
            )}

            <form
              className="card-body stack"
              style={{ borderTop: '1px solid var(--border)' }}
              onSubmit={(e) => void handleCreateUser(e)}
            >
              <strong style={{ fontSize: 13 }}>Create User</strong>
              <div className="row" style={{ flexWrap: 'wrap' }}>
                <input
                  className="text-input"
                  style={{ flex: '1 1 130px' }}
                  type="text"
                  placeholder="Username"
                  aria-label="New username"
                  value={newUsername}
                  onChange={(e) => setNewUsername(e.target.value)}
                />
                <input
                  className="text-input"
                  style={{ flex: '1 1 150px' }}
                  type="text"
                  placeholder="Display name"
                  aria-label="New user display name"
                  value={newDisplayName}
                  onChange={(e) => setNewDisplayName(e.target.value)}
                />
                <div style={{ minWidth: 140 }}>
                  <Dropdown
                    label="New user role"
                    value={newRole}
                    options={ROLE_OPTIONS}
                    onChange={setNewRole}
                  />
                </div>
                <input
                  className="text-input"
                  style={{ width: 120 }}
                  type="password"
                  inputMode="numeric"
                  maxLength={4}
                  placeholder="4-digit PIN"
                  aria-label="New user 4-digit PIN"
                  value={newProfilePin}
                  onChange={(e) => setNewProfilePin(e.target.value)}
                />
              </div>

              {newRole !== 'admin' && folders.length > 0 && (
                <div>
                  <div className="workspace-label">Assign Folder Access</div>
                  <div className="acl-chip-grid">
                    {folders.map((folder) => {
                      const checked = newAllowedFolders.includes(folder.id);
                      return (
                        <label
                          key={folder.id}
                          className={`acl-chip ${checked ? 'is-selected' : ''}`}
                        >
                          <input
                            type="checkbox"
                            checked={checked}
                            onChange={() =>
                              toggleAclFolder(newAllowedFolders, setNewAllowedFolders, folder.id)
                            }
                          />
                          <span>{folder.name}</span>
                        </label>
                      );
                    })}
                  </div>
                </div>
              )}

              <div className="confirm-actions">
                <button type="submit" className="button button-primary" disabled={creatingUser}>
                  <UserPlus size={14} />
                  {creatingUser ? 'Creating…' : 'Create User'}
                </button>
              </div>
            </form>

            <form
              className="card-body stack"
              style={{ borderTop: '1px solid var(--border)' }}
              onSubmit={(e) => void handleUpdateServerPin(e)}
            >
              <strong style={{ fontSize: 13 }}>Change 6-Digit Server PIN</strong>
              <div className="row" style={{ flexWrap: 'wrap' }}>
                <input
                  className="text-input"
                  style={{ flex: '1 1 140px' }}
                  type="password"
                  inputMode="numeric"
                  maxLength={6}
                  placeholder="Current Server PIN"
                  aria-label="Current Server PIN"
                  value={currentServerPin}
                  onChange={(e) => setCurrentServerPin(e.target.value)}
                />
                <input
                  className="text-input"
                  style={{ flex: '1 1 140px' }}
                  type="password"
                  inputMode="numeric"
                  maxLength={6}
                  placeholder="New 6-digit PIN"
                  aria-label="New Server PIN"
                  value={nextServerPin}
                  onChange={(e) => setNextServerPin(e.target.value)}
                />
                <input
                  className="text-input"
                  style={{ flex: '1 1 140px' }}
                  type="password"
                  inputMode="numeric"
                  maxLength={6}
                  placeholder="Confirm New PIN"
                  aria-label="Confirm New Server PIN"
                  value={confirmNextServerPin}
                  onChange={(e) => setConfirmNextServerPin(e.target.value)}
                />
                <button type="submit" className="button" disabled={savingServerPin}>
                  {savingServerPin ? 'Updating…' : 'Update Server PIN'}
                </button>
              </div>
            </form>
          </div>

          <SectionTitle>App Updates</SectionTitle>
          <div className="card card-body stack">
            <div
              className="row"
              style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}
            >
              <div>
                <div className="row" style={{ gap: 8 }}>
                  <strong style={{ fontSize: 13 }}>GitHub Releases</strong>
                  <Badge tone="muted">
                    v{updateResult?.current_version ?? systemInfo?.version ?? '0.1.0'}
                  </Badge>
                  {updateResult?.status === 'update_available' && updateResult.latest_version && (
                    <Badge tone="ok">v{updateResult.latest_version} available</Badge>
                  )}
                  {updateResult?.status === 'up_to_date' && <Badge tone="ok">Up to date</Badge>}
                  {updateResult?.status === 'no_releases' && (
                    <Badge tone="muted">No releases yet</Badge>
                  )}
                  {updateResult?.status === 'offline' && <Badge tone="warn">Offline</Badge>}
                </div>
                <div className="view-subtitle" style={{ marginTop: 2 }}>
                  Pulls the latest macOS &amp; Windows installers directly from{' '}
                  <code>iant4093/TruongPhoto</code> on GitHub.
                </div>
              </div>

              <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
                <button
                  type="button"
                  className="button button-primary"
                  disabled={checkingUpdates}
                  onClick={() => void handleCheckUpdates()}
                >
                  <RefreshCw size={14} className={checkingUpdates ? 'spin-icon' : undefined} />
                  <span>{checkingUpdates ? 'Checking…' : 'Check for Updates'}</span>
                </button>
                <button
                  type="button"
                  className="button"
                  onClick={() =>
                    void openExternalUrl(
                      updateResult?.release_url ??
                        'https://github.com/iant4093/TruongPhoto/releases',
                    )
                  }
                >
                  <ExternalLink size={14} />
                  <span>Releases</span>
                </button>
              </div>
            </div>

            {updateResult && (
              <div className="update-result-box" role="status">
                {updateResult.update_available ? (
                  <div className="stack" style={{ gap: 8 }}>
                    <div
                      className="row"
                      style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 8 }}
                    >
                      <strong style={{ fontSize: 13 }}>
                        {updateResult.release_name || `TruongPhoto v${updateResult.latest_version}`}
                      </strong>
                      {updateResult.download_url && (
                        <button
                          type="button"
                          className="button button-primary"
                          onClick={() => void openExternalUrl(updateResult.download_url!)}
                        >
                          <Download size={14} />
                          <span>
                            Download {updateResult.asset_name ? updateResult.asset_name : 'Update'}
                          </span>
                        </button>
                      )}
                    </div>
                    {updateResult.release_notes && (
                      <pre className="update-release-notes">{updateResult.release_notes}</pre>
                    )}
                  </div>
                ) : (
                  <div className="view-subtitle" style={{ marginTop: 0 }}>
                    {updateResult.message || 'You are running the latest release.'}
                  </div>
                )}
              </div>
            )}
          </div>

          <SectionTitle>Network &amp; Desktop</SectionTitle>
          <div className="card card-body stack">
            <div>
              <div className="row" style={{ justifyContent: 'space-between', marginBottom: 6 }}>
                <span style={{ fontWeight: 500 }}>Local &amp; LAN Access URLs</span>
                <Badge tone={systemInfo?.mdns_active ? 'ok' : 'muted'}>
                  mDNS {systemInfo?.mdns_active ? 'Active' : 'Inactive'}
                </Badge>
              </div>
              <div className="stack">
                {systemInfo?.mdns_url && (
                  <div className="row" style={{ justifyContent: 'space-between' }}>
                    <button
                      type="button"
                      className="button button-quiet"
                      onClick={() => void openExternalUrl(systemInfo.mdns_url!)}
                    >
                      {systemInfo.mdns_url}
                    </button>
                    <button
                      type="button"
                      className="button"
                      onClick={() => void handleCopyUrl(systemInfo.mdns_url!)}
                    >
                      {copiedUrl === systemInfo.mdns_url ? <Check size={13} /> : <Copy size={13} />}
                      {copiedUrl === systemInfo.mdns_url ? 'Copied' : 'Copy'}
                    </button>
                  </div>
                )}
                {systemInfo?.lan_urls.map((url) => (
                  <div key={url} className="row" style={{ justifyContent: 'space-between' }}>
                    <button
                      type="button"
                      className="button button-quiet"
                      onClick={() => void openExternalUrl(url)}
                    >
                      {url}
                    </button>
                    <button
                      type="button"
                      className="button"
                      onClick={() => void handleCopyUrl(url)}
                    >
                      {copiedUrl === url ? <Check size={13} /> : <Copy size={13} />}
                      {copiedUrl === url ? 'Copied' : 'Copy'}
                    </button>
                  </div>
                ))}
              </div>
            </div>

            <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
              <ToggleRow
                label="mDNS Discovery (truongphoto.local)"
                description="Advertise TruongPhoto over Bonjour / mDNS on your home network."
                checked={adminSettings?.mdns_enabled ?? false}
                disabled={!adminSettings}
                onChange={(next) => void handleToggleSetting('mdns_enabled', next)}
              />
              <ToggleRow
                label="Desktop PIN Lock"
                description="Require the 6+4 PIN flow even when opening the desktop app locally."
                checked={adminSettings?.desktop_pin_lock ?? false}
                disabled={!adminSettings}
                onChange={(next) => void handleToggleSetting('desktop_pin_lock', next)}
              />

              {isTauri() && (
                <ToggleRow
                  label="Launch at Login"
                  description="Start TruongPhoto automatically in the menu bar / system tray."
                  checked={autostartEnabled}
                  onChange={(next) => void handleToggleAutostart(next)}
                />
              )}
            </div>

            <div className="tailscale-card">
              <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
                <div>
                  <strong style={{ fontSize: 13 }}>Tailscale Remote HTTPS</strong>
                  <div className="view-subtitle" style={{ marginTop: 2 }}>
                    Encrypted zero-port-forwarding access via Tailscale Serve.
                  </div>
                </div>
                <div className="row">
                  <Badge tone={tailscaleStatus?.serve_active ? 'ok' : 'muted'}>
                    {tailscaleStatus?.serve_active ? 'Serve Active' : 'Serve Inactive'}
                  </Badge>
                  <button
                    type="button"
                    className={`button ${tailscaleStatus?.serve_active ? '' : 'button-primary'}`}
                    disabled={
                      tailscaleBusy ||
                      (!tailscaleStatus?.serve_active &&
                        (!tailscaleStatus?.installed ||
                          !tailscaleStatus?.running ||
                          !tailscaleStatus?.logged_in ||
                          !tailscaleStatus?.https_certs_enabled))
                    }
                    onClick={() => void handleToggleTailscale()}
                  >
                    {tailscaleBusy
                      ? 'Updating…'
                      : tailscaleStatus?.serve_active
                        ? 'Disable Tailscale HTTPS'
                        : 'Enable Tailscale HTTPS'}
                  </button>
                </div>
              </div>

              {tailscaleStatus && (
                <div className="tailscale-pills">
                  <Badge tone={tailscaleStatus.installed ? 'ok' : 'warn'}>Installed</Badge>
                  <Badge tone={tailscaleStatus.logged_in ? 'ok' : 'warn'}>Logged In</Badge>
                  <Badge tone={tailscaleStatus.magic_dns_name ? 'ok' : 'muted'}>MagicDNS</Badge>
                  <Badge tone={tailscaleStatus.https_certs_enabled ? 'ok' : 'muted'}>
                    HTTPS Certs
                  </Badge>
                </div>
              )}

              {tailscaleStatus?.serve_active && tailscaleStatus.https_url && (
                <div className="row" style={{ justifyContent: 'space-between' }}>
                  <button
                    type="button"
                    className="button button-quiet"
                    onClick={() => void openExternalUrl(tailscaleStatus.https_url!)}
                  >
                    {tailscaleStatus.https_url}
                  </button>
                  <button
                    type="button"
                    className="button"
                    onClick={() => void handleCopyUrl(tailscaleStatus.https_url!)}
                  >
                    {copiedUrl === tailscaleStatus.https_url ? (
                      <Check size={13} />
                    ) : (
                      <Copy size={13} />
                    )}
                    {copiedUrl === tailscaleStatus.https_url ? 'Copied' : 'Copy'}
                  </button>
                </div>
              )}

              {(tailscaleStatus?.remediation || tailscaleStatus?.last_error) && (
                <div className="remediation-box" role="status">
                  {tailscaleStatus.remediation || tailscaleStatus.last_error}
                </div>
              )}

              {qrTargetUrl && (
                <div className="qr-panel">
                  <img
                    className="qr-image"
                    src={systemQrUrl(qrTargetUrl)}
                    alt="Remote access QR code"
                  />
                  <div className="stack" style={{ gap: 4, minWidth: 0 }}>
                    <strong style={{ fontSize: 12 }}>Mobile QR Access</strong>
                    <span className="view-subtitle" style={{ marginTop: 0 }}>
                      Scan with your phone camera to open TruongPhoto
                      {tailscaleStatus?.serve_active
                        ? ' over Tailscale HTTPS.'
                        : ' on your local network.'}
                    </span>
                    <code
                      style={{
                        fontSize: 11,
                        color: 'var(--text-secondary)',
                        overflowWrap: 'anywhere',
                      }}
                    >
                      {qrTargetUrl}
                    </code>
                  </div>
                </div>
              )}
            </div>
          </div>

          <div className="advanced-accordion" style={{ marginTop: 18 }}>
            <button
              type="button"
              className="advanced-accordion-trigger"
              aria-expanded={showAdvancedSettings}
              onClick={() => setShowAdvancedSettings((v) => !v)}
            >
              <span>
                {showAdvancedSettings
                  ? 'Hide Advanced Storage, Backups & Performance Settings'
                  : 'Show Advanced Storage, Backups & Performance Settings'}
              </span>
              {showAdvancedSettings ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
            </button>
            <div className={`advanced-accordion-body ${showAdvancedSettings ? 'is-open' : ''}`}>
              <SectionTitle>Storage &amp; Backups</SectionTitle>
          <form className="card card-body stack" onSubmit={(e) => void handleSaveStorage(e)}>
            <label className="settings-field">
              <span>Managed Uploads Path</span>
              <input
                className="text-input"
                type="text"
                value={uploadsDir}
                onChange={(e) => setUploadsDir(e.target.value)}
              />
            </label>
            <label className="settings-field">
              <span>Thumbnails Path</span>
              <input
                className="text-input"
                type="text"
                value={thumbsDir}
                onChange={(e) => setThumbsDir(e.target.value)}
              />
            </label>
            <div className="row" style={{ flexWrap: 'wrap', gap: 12 }}>
              <label className="settings-field" style={{ flex: '2 1 260px' }}>
                <span>Cache Path</span>
                <input
                  className="text-input"
                  type="text"
                  value={cacheDir}
                  onChange={(e) => setCacheDir(e.target.value)}
                />
              </label>
              <label className="settings-field" style={{ flex: '0 0 130px' }}>
                <span>Cache Limit (GB)</span>
                <input
                  className="text-input"
                  type="number"
                  min={1}
                  value={cacheLimitGb}
                  onChange={(e) => setCacheLimitGb(e.target.value)}
                />
              </label>
            </div>
            <div className="row" style={{ flexWrap: 'wrap', gap: 12 }}>
              <label className="settings-field" style={{ flex: '2 1 220px' }}>
                <span>Backup Directory</span>
                <input
                  className="text-input"
                  type="text"
                  value={backupDir}
                  onChange={(e) => setBackupDir(e.target.value)}
                />
              </label>
              <label className="settings-field" style={{ flex: '0 0 120px' }}>
                <span>Keep Backups</span>
                <input
                  className="text-input"
                  type="number"
                  min={1}
                  value={backupKeepCount}
                  onChange={(e) => setBackupKeepCount(e.target.value)}
                />
              </label>
              <div className="settings-field" style={{ flex: '0 0 155px' }}>
                <span>Backup Interval</span>
                <Dropdown
                  label="Backup Interval"
                  value={backupIntervalHours}
                  options={[
                    ['0', 'Manual (0h)'],
                    ['6', '6h'],
                    ['24', '24h'],
                    ['168', 'Weekly (168h)'],
                  ]}
                  onChange={(val) => void handleBackupIntervalChange(val)}
                />
              </div>
            </div>

            <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap' }}>
              <button
                type="button"
                className="button"
                disabled={backingUp}
                onClick={() => void handleBackupNow()}
              >
                <DatabaseBackup size={14} />
                {backingUp ? 'Backing Up…' : 'Back Up Library Now'}
              </button>
              <button type="submit" className="button button-primary" disabled={savingStorage}>
                {savingStorage ? 'Saving…' : 'Save Storage Settings'}
              </button>
            </div>

            {lastBackup && (
              <div className="backup-summary" role="status">
                Backup saved to <strong>{lastBackup.path}</strong> (
                {formatBytes(lastBackup.size_bytes)}) at{' '}
                {formatDateTime(lastBackup.created_at_ms)}. Pruned {lastBackup.pruned_files} older{' '}
                {lastBackup.pruned_files === 1 ? 'file' : 'files'}.
              </div>
            )}

            <div style={{ borderTop: '1px solid var(--border)', paddingTop: 12 }}>
              <div className="workspace-label" style={{ marginBottom: 6 }}>
                Backup Snapshot History
              </div>
              {backupHistory.length === 0 ? (
                <div className="view-subtitle" style={{ marginTop: 0 }}>
                  No backup snapshots found yet.
                </div>
              ) : (
                <div style={{ overflowX: 'auto' }}>
                  <table className="backup-table">
                    <thead>
                      <tr>
                        <th>Filename</th>
                        <th>Size</th>
                        <th>Created</th>
                      </tr>
                    </thead>
                    <tbody>
                      {backupHistory.map((snap) => (
                        <tr key={snap.filename}>
                          <td>{snap.filename}</td>
                          <td>{formatBytes(snap.size_bytes)}</td>
                          <td>{formatDateTime(snap.created_at_ms)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          </form>

          <SectionTitle>Power &amp; Thermal Throttling</SectionTitle>
          <form
            className="card card-body stack"
            onSubmit={(e) => void handleSaveCpuThreshold(e)}
          >
            <ToggleRow
              label="Pause Indexing on Battery"
              description="Automatically pause background media scanning and AI indexing when running on battery power."
              checked={adminSettings?.pause_on_battery ?? true}
              disabled={!adminSettings}
              onChange={(next) => void handleToggleSetting('pause_on_battery', next)}
            />
            <div
              className="row"
              style={{
                justifyContent: 'space-between',
                flexWrap: 'wrap',
                borderTop: '1px solid var(--border)',
                paddingTop: 12,
              }}
            >
              <label className="settings-field" style={{ marginBottom: 0, flex: '1 1 220px' }}>
                <span>CPU Throttle Threshold (%)</span>
                <div className="row" style={{ marginTop: 4 }}>
                  <input
                    className="text-input"
                    type="number"
                    min={30}
                    max={100}
                    aria-label="CPU Throttle Threshold (%)"
                    value={cpuThrottlePercent}
                    onChange={(e) => setCpuThrottlePercent(e.target.value)}
                  />
                  <button
                    type="submit"
                    className="button"
                    disabled={!adminSettings || savingThrottling}
                  >
                    {savingThrottling ? 'Saving…' : 'Apply Threshold'}
                  </button>
                </div>
              </label>
            </div>
          </form>

          <SectionTitle>Map Basemap</SectionTitle>
          <div className="card card-body stack">
            <div className="row" style={{ justifyContent: 'space-between', flexWrap: 'wrap', gap: 10 }}>
              <div>
                <div className="row" style={{ gap: 6 }}>
                  <MapPin size={14} aria-hidden="true" />
                  <strong style={{ fontSize: 13 }}>Tile Source Mode</strong>
                </div>
                <div className="view-subtitle" style={{ marginTop: 2 }}>
                  Default offline mode makes zero external network requests.
                </div>
              </div>

              <SegmentedControl
                value={effectiveMapTileMode}
                options={MAP_TILE_MODE_OPTIONS}
                onChange={(mode) => void handleSetMapTileMode(mode)}
              />
            </div>

            <div
              className="property-field"
              style={{ borderTop: '1px solid var(--border)', paddingTop: 12, marginBottom: 0 }}
            >
              <div className="property-row">
                <span>PMTiles Directory</span>
                <span className="property-value">
                  {adminSettings?.map_pmtiles_dir || 'Default (<data_dir>/maps)'}
                </span>
              </div>
              <div className="property-row">
                <span>Detected .pmtiles Files</span>
                <span className="property-value">
                  {mapConfig && mapConfig.available_pmtiles.length > 0
                    ? mapConfig.available_pmtiles.join(', ')
                    : 'None (built-in offline world coastline active)'}
                </span>
              </div>
            </div>

            <div className="row" style={{ justifyContent: 'flex-end' }}>
              <button
                type="button"
                className="button"
                disabled={downloadingBasemap}
                onClick={() => void handleDownloadBasemap()}
              >
                <Download size={14} />
                <span>
                  {downloadingBasemap
                    ? 'Starting Download…'
                    : 'Download Offline World Basemap (world-basemap-pmtiles)'}
                </span>
              </button>
            </div>
          </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
};
