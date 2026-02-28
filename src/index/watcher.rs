//! File watcher for proactive cache invalidation.
//!
//! Watches the project directory for file changes and maintains an in-memory
//! set of dirty paths, eliminating per-query BLAKE3 hashing.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use notify_debouncer_full::{Debouncer, FileIdMap, new_debouncer};
use notify_debouncer_full::notify::{EventKind, RecommendedWatcher, Watcher};
use tracing::{debug, info, warn};

use crate::lsp::diagnostics::DiagnosticStore;

/// Thread-safe set of file paths known to be dirty (changed since last index).
#[derive(Clone)]
pub struct DirtyFiles {
    inner: Arc<RwLock<HashSet<String>>>,
    /// When true, all files are considered dirty (watcher overflow recovery).
    poisoned: Arc<AtomicBool>,
}

impl Default for DirtyFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl DirtyFiles {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashSet::new())),
            poisoned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark a relative path as dirty.
    pub fn mark_dirty(&self, rel_path: String) {
        match self.inner.write() {
            Ok(mut set) => {
                set.insert(rel_path);
            }
            Err(_) => {
                // Lock poisoned — poison the dirty set so all files are considered stale
                self.poison();
            }
        }
    }

    /// Check if a relative path is dirty.
    ///
    /// Returns `true` if the file is known to have changed, or if the watcher
    /// is poisoned (overflow occurred).
    #[must_use]
    pub fn is_dirty(&self, rel_path: &str) -> bool {
        if self.poisoned.load(Ordering::Relaxed) {
            return true;
        }
        self.inner
            .read()
            .is_ok_and(|set| set.contains(rel_path))
    }

    /// Clear all dirty entries and reset poison flag.
    ///
    /// Called after re-indexing (`krait init`).
    pub fn clear(&self) {
        if let Ok(mut set) = self.inner.write() {
            set.clear();
        }
        self.poisoned.store(false, Ordering::Relaxed);
    }

    /// Number of dirty files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().map_or(0, |set| set.len())
    }

    /// Whether the dirty set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the watcher is poisoned (overflow occurred).
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Relaxed)
    }

    /// Mark the watcher as poisoned — all files considered dirty.
    pub(crate) fn poison(&self) {
        self.poisoned.store(true, Ordering::Relaxed);
    }
}

/// Debounce window for file change events.
const DEBOUNCE_MS: u64 = 500;

/// Start watching the project root for file changes.
///
/// Returns the debouncer handle (must be kept alive). Dropping it stops the watcher.
///
/// # Errors
/// Returns an error if the watcher can't be created or the project root can't be watched.
pub fn start_watcher(
    project_root: &Path,
    extensions: &[String],
    dirty_files: DirtyFiles,
    diagnostic_store: Option<Arc<DiagnosticStore>>,
) -> anyhow::Result<Debouncer<RecommendedWatcher, FileIdMap>> {
    // Canonicalize to match FSEvents paths on macOS
    let canonical_root =
        project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let ext_set: HashSet<String> = extensions.iter().cloned().collect();
    let df = dirty_files;

    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        None,
        move |result: notify_debouncer_full::DebounceEventResult| match result {
            Ok(events) => {
                for event in events {
                    match event.kind {
                        // For renames, mark both old and new paths dirty
                        EventKind::Modify(notify_debouncer_full::notify::event::ModifyKind::Name(_)) => {
                            for path in &event.paths {
                                if let Some(rel) =
                                    to_relative(path, &canonical_root, &ext_set)
                                {
                                    debug!("file renamed: {rel}");
                                    df.mark_dirty(rel);
                                    if let Some(store) = &diagnostic_store {
                                        store.clear(path);
                                    }
                                }
                            }
                        }
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                            for path in &event.paths {
                                if let Some(rel) =
                                    to_relative(path, &canonical_root, &ext_set)
                                {
                                    debug!("file changed: {rel}");
                                    df.mark_dirty(rel);
                                    if let Some(store) = &diagnostic_store {
                                        store.clear(path);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(errors) => {
                warn!("watcher errors: {:?} — entering full re-check mode", errors);
                df.poison();
                // Schedule recovery: clear poison after 30s so BLAKE3 fallback
                // eventually stops considering every file dirty.
                let inner_clone = Arc::clone(&df.inner);
                let poisoned_clone = Arc::clone(&df.poisoned);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(30));
                    poisoned_clone.store(false, Ordering::Relaxed);
                    if let Ok(mut set) = inner_clone.write() {
                        set.clear();
                    }
                    info!("watcher: poison cleared after 30s recovery window");
                });
            }
        },
    )?;

    debouncer
        .watcher()
        .watch(project_root, notify_debouncer_full::notify::RecursiveMode::Recursive)?;

    info!("file watcher started on {}", project_root.display());
    Ok(debouncer)
}

/// Convert an absolute path to a relative path if it has an indexed extension.
fn to_relative(path: &Path, canonical_root: &Path, extensions: &HashSet<String>) -> Option<String> {
    // Check extension first (cheapest filter)
    let ext = path.extension()?.to_str()?;
    if !extensions.contains(ext) {
        return None;
    }

    // Try canonical root first
    if let Ok(r) = path.strip_prefix(canonical_root) {
        return Some(r.to_string_lossy().to_string());
    }

    // Fallback: canonicalize the event path (resolves symlinks) and try again
    if let Ok(canonical_path) = path.canonicalize() {
        if let Ok(r) = canonical_path.strip_prefix(canonical_root) {
            return Some(r.to_string_lossy().to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_files_basic() {
        let df = DirtyFiles::new();
        assert!(!df.is_dirty("src/lib.rs"));
        assert_eq!(df.len(), 0);

        df.mark_dirty("src/lib.rs".to_string());
        assert!(df.is_dirty("src/lib.rs"));
        assert!(!df.is_dirty("src/main.rs"));
        assert_eq!(df.len(), 1);
    }

    #[test]
    fn dirty_files_clear() {
        let df = DirtyFiles::new();
        df.mark_dirty("a.rs".to_string());
        df.mark_dirty("b.rs".to_string());
        assert_eq!(df.len(), 2);

        df.clear();
        assert_eq!(df.len(), 0);
        assert!(!df.is_dirty("a.rs"));
    }

    #[test]
    fn dirty_files_poison() {
        let df = DirtyFiles::new();
        assert!(!df.is_poisoned());
        assert!(!df.is_dirty("any_file.rs"));

        df.poison();
        assert!(df.is_poisoned());
        assert!(df.is_dirty("any_file.rs"));
        assert!(df.is_dirty("literally_anything"));
    }

    #[test]
    fn dirty_files_clear_resets_poison() {
        let df = DirtyFiles::new();
        df.poison();
        assert!(df.is_poisoned());

        df.clear();
        assert!(!df.is_poisoned());
        assert!(!df.is_dirty("test.rs"));
    }

    #[test]
    fn dirty_files_clone_shares_state() {
        let df1 = DirtyFiles::new();
        let df2 = df1.clone();

        df1.mark_dirty("shared.rs".to_string());
        assert!(df2.is_dirty("shared.rs"));
    }

    #[test]
    fn to_relative_filters_extension() {
        let root = Path::new("/project");
        let exts: HashSet<String> = ["rs", "ts"].iter().map(|s| (*s).to_string()).collect();

        assert!(to_relative(Path::new("/project/src/lib.rs"), root, &exts).is_some());
        assert!(to_relative(Path::new("/project/src/app.ts"), root, &exts).is_some());
        assert!(to_relative(Path::new("/project/README.md"), root, &exts).is_none());
        assert!(to_relative(Path::new("/project/Cargo.toml"), root, &exts).is_none());
    }

    #[test]
    fn to_relative_strips_prefix() {
        let root = Path::new("/project");
        let exts: HashSet<String> = ["rs"].iter().map(|s| (*s).to_string()).collect();

        let rel = to_relative(Path::new("/project/src/lib.rs"), root, &exts);
        assert_eq!(rel, Some("src/lib.rs".to_string()));
    }

    #[test]
    fn to_relative_outside_root_returns_none() {
        let root = Path::new("/project");
        let exts: HashSet<String> = ["rs"].iter().map(|s| (*s).to_string()).collect();

        assert!(to_relative(Path::new("/other/src/lib.rs"), root, &exts).is_none());
    }
}
