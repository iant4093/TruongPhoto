use crate::ingest::scanner::ingest_single_file;
use crate::state::AppState;
use notify::{
    event::{EventKind, ModifyKind},
    Event, RecommendedWatcher, RecursiveMode, Watcher,
};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use truongphoto_core::RbacScope;
use truongphoto_db::repo;
use truongphoto_media::classify::{classify_path, is_ignored_entry};

const WATCHER_DEBOUNCE: Duration = Duration::from_millis(500);

struct FolderWatchEntry {
    cancel: CancellationToken,
    _watcher: RecommendedWatcher,
}

/// Manages OS-native filesystem watchers (`notify`) per watched folder with 500ms debouncing
/// and automatic fallback to incremental folder scans on `Flag::Rescan` or buffer overflows.
pub struct FolderWatcherManager {
    watchers: Mutex<HashMap<String, FolderWatchEntry>>,
}

impl Default for FolderWatcherManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderWatcherManager {
    pub fn new() -> Self {
        Self {
            watchers: Mutex::new(HashMap::new()),
        }
    }

    /// Registers watchers for all existing watched folders in SQLite on server startup.
    pub async fn start_existing_folders(&self, state: &Arc<AppState>) {
        let folders = match state
            .db
            .read(|conn| repo::folders::list_folders(conn, &RbacScope::AdminAll))
            .await
        {
            Ok(list) => list,
            Err(err) => {
                tracing::warn!(error = %err, "failed to load watched folders for FS watcher startup");
                return;
            }
        };

        for folder in folders {
            self.watch_folder(state, folder.id, PathBuf::from(folder.root_path));
        }
    }

    /// Starts or replaces the filesystem watcher for `folder_id` at `root_path`.
    pub fn watch_folder(&self, state: &Arc<AppState>, folder_id: String, root_path: PathBuf) {
        self.unwatch_folder(&folder_id);

        if !root_path.is_dir() {
            tracing::debug!(folder_id = %folder_id, path = %root_path.display(), "skipping FS watcher for offline/missing folder");
            return;
        }

        let (tx, rx) = mpsc::unbounded_channel::<notify::Result<Event>>();
        let mut watcher = match RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(err) => {
                tracing::warn!(folder_id = %folder_id, error = %err, "failed to create FS watcher");
                return;
            }
        };

        if let Err(err) = watcher.watch(&root_path, RecursiveMode::Recursive) {
            tracing::warn!(
                folder_id = %folder_id,
                path = %root_path.display(),
                error = %err,
                "failed to watch folder directory"
            );
            return;
        }

        let cancel = state.shutdown_token.child_token();
        let task_cancel = cancel.clone();
        let task_state = state.clone();
        let fid = folder_id.clone();
        let root_clone = root_path.clone();

        tokio::spawn(async move {
            run_folder_debounce_loop(task_state, fid, root_clone, rx, task_cancel).await;
        });

        self.watchers.lock().insert(
            folder_id,
            FolderWatchEntry {
                cancel,
                _watcher: watcher,
            },
        );
    }

    /// Stops and removes the filesystem watcher for `folder_id`.
    pub fn unwatch_folder(&self, folder_id: &str) {
        if let Some(entry) = self.watchers.lock().remove(folder_id) {
            entry.cancel.cancel();
        }
    }

    /// Stops and removes all active filesystem watchers.
    pub fn stop_all(&self) {
        let mut guard = self.watchers.lock();
        for (_, entry) in guard.drain() {
            entry.cancel.cancel();
        }
    }
}

async fn run_folder_debounce_loop(
    state: Arc<AppState>,
    folder_id: String,
    root_path: PathBuf,
    mut rx: mpsc::UnboundedReceiver<notify::Result<Event>>,
    cancel: CancellationToken,
) {
    let mut pending_paths: HashSet<PathBuf> = HashSet::new();
    let mut needs_full_scan = false;
    let mut debounce_active = false;
    let sleep = tokio::time::sleep(WATCHER_DEBOUNCE);
    tokio::pin!(sleep);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe_evt = rx.recv() => {
                let Some(res) = maybe_evt else {
                    break;
                };
                match res {
                    Ok(event) => {
                        if event.need_rescan()
                            || matches!(
                                event.kind,
                                EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
                            )
                        {
                            needs_full_scan = true;
                            debounce_active = true;
                            sleep.as_mut().reset(tokio::time::Instant::now() + WATCHER_DEBOUNCE);
                        } else if matches!(
                            event.kind,
                            EventKind::Create(_) | EventKind::Modify(_)
                        ) {
                            let mut added = false;
                            for p in event.paths {
                                if !should_ignore_watch_path(&root_path, &p) {
                                    pending_paths.insert(p);
                                    added = true;
                                }
                            }
                            if added {
                                debounce_active = true;
                                sleep.as_mut().reset(tokio::time::Instant::now() + WATCHER_DEBOUNCE);
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            folder_id = %folder_id,
                            error = %err,
                            "FS watcher overflow/error; triggering incremental rescan"
                        );
                        needs_full_scan = true;
                        debounce_active = true;
                        sleep.as_mut().reset(tokio::time::Instant::now() + WATCHER_DEBOUNCE);
                    }
                }
            }
            () = &mut sleep, if debounce_active => {
                debounce_active = false;
                let do_full = needs_full_scan || pending_paths.len() > 32;
                let batch: Vec<PathBuf> = pending_paths.drain().collect();
                needs_full_scan = false;

                if do_full {
                    if let Err(err) = state.scanner.enqueue_folder_scan(&state, folder_id.clone()).await {
                        tracing::debug!(folder_id = %folder_id, error = ?err, "watcher scan enqueue skipped");
                    }
                } else if !batch.is_empty() {
                    process_debounced_paths(&state, &folder_id, batch).await;
                }
            }
        }
    }
}

async fn process_debounced_paths(state: &Arc<AppState>, folder_id: &str, paths: Vec<PathBuf>) {
    let fid = folder_id.to_string();
    let folder = match state
        .db
        .read(move |conn| repo::folders::get_folder_by_id(conn, &fid))
        .await
    {
        Ok(Some(f)) => f,
        _ => return,
    };

    let mut trigger_rescan = false;
    for path in paths {
        if path.is_dir() || !path.exists() {
            trigger_rescan = true;
            continue;
        }
        if classify_path(&path).is_none() {
            continue;
        }
        if let Err(err) = ingest_single_file(state, &folder, &path).await {
            tracing::warn!(
                folder_id = %folder.id,
                path = %path.display(),
                error = %err,
                "targeted watcher ingest failed"
            );
        }
    }

    if trigger_rescan {
        let _ = state.scanner.enqueue_folder_scan(state, folder.id).await;
    }
}

fn should_ignore_watch_path(root: &Path, path: &Path) -> bool {
    let rel_owned: std::path::PathBuf;
    let rel = if let Ok(r) = path.strip_prefix(root) {
        r
    } else if let (Ok(cr), Ok(cp)) = (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        if let Ok(r) = cp.strip_prefix(&cr) {
            rel_owned = r.to_path_buf();
            rel_owned.as_path()
        } else {
            return false;
        }
    } else {
        // When `path` was just deleted, `canonicalize` fails; inspect ONLY the basename so
        // hidden ancestor directories outside `root` (e.g. `~/.config/...`) never suppress rescans.
        return path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_ignored_entry);
    };
    for comp in rel.components() {
        if let std::path::Component::Normal(s) = comp {
            if let Some(name) = s.to_str() {
                if is_ignored_entry(name) {
                    return true;
                }
            }
        }
    }
    false
}
