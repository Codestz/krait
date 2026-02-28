use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracing::{debug, info, warn};

use super::client::LspClient;
use super::diagnostics::DiagnosticStore;
use super::files::FileTracker;
use super::install;
use crate::detect::{Language, language_for_file};

/// Maximum crashes before permanent failure (`MultiRoot` only).
const MAX_CRASHES: u32 = 5;

/// Backoff delays (seconds) indexed by crash count (0-based). Last value is the cap.
const BACKOFF_DELAYS_SECS: &[u64] = &[2, 4, 8, 16, 32, 60];

/// After this many seconds of uptime, reset the crash counter.
const STABILITY_RESET_SECS: u64 = 300; // 5 minutes

/// Default max concurrent sessions for LRU fallback.
/// Supports up to 15 language servers + spare capacity for LRU overflow.
const DEFAULT_MAX_LRU_SESSIONS: usize = 20;

/// LSP client and file tracker bundled together.
pub struct LspSession {
    pub client: LspClient,
    pub file_tracker: FileTracker,
}

/// Per-server slot with lifecycle state.
struct ServerSlot {
    session: LspSession,
    started_at: Instant,
    last_used_at: Instant,
    server_name: String,
}

/// Strategy for managing a language's LSP server(s).
enum ServerStrategy {
    /// One server, multiple workspace folders dynamically attached.
    MultiRoot(Box<ServerSlot>),
    /// Multiple servers (one per workspace root), LRU-evicted at cap.
    LruPerRoot(HashMap<PathBuf, ServerSlot>),
}

/// Per-language mutable state — one per language, guarded by its own Mutex.
///
/// The outer `LspMultiplexer` holds one `Arc<Mutex<LanguageState>>` per language.
/// Multiple tasks can hold different language locks concurrently.
pub struct LanguageState {
    /// `None` = not started yet (lazy init pending).
    strategy: Option<ServerStrategy>,
    crash_count: u32,
    /// Set when the language has permanently failed (too many crashes).
    pub failed: Option<String>,
}

impl LanguageState {
    fn new() -> Self {
        Self { strategy: None, crash_count: 0, failed: None }
    }

    /// Get a mutable reference to any active session for this language.
    pub fn session_mut(&mut self) -> Option<&mut LspSession> {
        match &mut self.strategy {
            Some(ServerStrategy::MultiRoot(slot)) => Some(&mut slot.session),
            Some(ServerStrategy::LruPerRoot(slots)) => {
                let (_, slot) = slots
                    .iter_mut()
                    .max_by_key(|(_, s)| s.last_used_at)?;
                slot.last_used_at = Instant::now();
                Some(&mut slot.session)
            }
            None => None,
        }
    }

    /// Whether a live session exists.
    #[must_use] 
    pub fn is_ready(&self) -> bool {
        match &self.strategy {
            Some(ServerStrategy::MultiRoot(_)) => true,
            Some(ServerStrategy::LruPerRoot(slots)) => !slots.is_empty(),
            None => false,
        }
    }
}

/// Why a server is not ready for queries.
#[derive(Debug, Clone)]
pub enum NotReadyReason {
    /// Server process has not been started yet (lazy init pending).
    NotStarted,
    /// Server crashed and exceeded max retries.
    Failed(String),
    /// No server configured for this language/scope.
    NotFound,
}

impl std::fmt::Display for NotReadyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted => write!(f, "server not yet started"),
            Self::Failed(reason) => write!(f, "server failed: {reason}"),
            Self::NotFound => write!(f, "no server configured for this scope"),
        }
    }
}

/// Status info for a language server.
#[derive(Debug, serde::Serialize)]
pub struct ServerStatus {
    pub server_name: String,
    pub language: String,
    pub status: &'static str,
    pub uptime_secs: u64,
    pub open_files: usize,
    pub attached_folders: usize,
    pub total_folders: usize,
}

/// Readiness summary for the multiplexer.
#[derive(Debug)]
pub struct Readiness {
    pub ready: usize,
    pub total: usize,
}

impl Readiness {
    #[must_use]
    pub fn is_all_ready(&self) -> bool {
        self.total > 0 && self.ready >= self.total
    }
}

/// Config values that can be set after construction (written once, read many).
struct PoolConfig {
    max_lru_sessions: usize,
    priority_roots: HashSet<PathBuf>,
    /// Max concurrent language server processes across all languages.
    /// `None` = unlimited (default).
    max_language_servers: Option<usize>,
}

/// Manages LSP server instances — one per language (multi-root) or per-root (LRU fallback).
///
/// **Concurrency model**: each language has its own `Arc<Mutex<LanguageState>>`.
/// Callers acquire a per-language `OwnedMutexGuard` for the duration of LSP I/O.
/// Different languages can be queried concurrently — no global lock is held during I/O.
///
/// **Lazy startup**: servers are started on first query, not at daemon boot.
/// **Crash recovery**: exponential backoff for multi-root, silent removal for LRU.
pub struct LspMultiplexer {
    /// Per-language locks. Map is immutable after construction — only inner state changes.
    languages: HashMap<Language, Arc<Mutex<LanguageState>>>,
    /// Read-only after daemon start. Never mutated after `new()` is called.
    pub project_root: PathBuf,
    pub workspace_roots: Vec<(Language, PathBuf)>,
    /// Config settable via `set_max_lru_sessions` / `set_priority_roots` (written once).
    config: std::sync::RwLock<PoolConfig>,
    /// Optional diagnostic store — set once after construction via `set_diagnostic_store()`.
    diagnostic_store: std::sync::OnceLock<Arc<DiagnosticStore>>,
    /// Tracks last-used timestamp per language for global LRU eviction.
    last_used: std::sync::RwLock<HashMap<Language, Instant>>,
}

/// Backward-compatible type alias.
pub type LspPool = LspMultiplexer;

impl LspMultiplexer {
    /// Create a new multiplexer for a project.
    ///
    /// Pre-populates per-language slots for all known languages so the
    /// `languages` map never needs to grow after construction.
    #[must_use]
    pub fn new(project_root: PathBuf, workspace_roots: Vec<(Language, PathBuf)>) -> Self {
        // Pre-populate one slot per unique language
        let unique_langs: HashSet<Language> = workspace_roots.iter().map(|(l, _)| *l).collect();
        let languages = unique_langs
            .into_iter()
            .map(|l| (l, Arc::new(Mutex::new(LanguageState::new()))))
            .collect();

        Self {
            languages,
            project_root,
            workspace_roots,
            config: std::sync::RwLock::new(PoolConfig {
                max_lru_sessions: DEFAULT_MAX_LRU_SESSIONS,
                priority_roots: HashSet::new(),
                max_language_servers: None,
            }),
            diagnostic_store: std::sync::OnceLock::new(),
            last_used: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Attach a diagnostic store so all new LSP clients collect diagnostics.
    ///
    /// Must be called before the first query. Subsequent calls are no-ops.
    pub fn set_diagnostic_store(&self, store: Arc<DiagnosticStore>) {
        let _ = self.diagnostic_store.set(store);
    }

    /// Set the maximum number of concurrent LRU sessions (from config).
    pub fn set_max_lru_sessions(&self, max: usize) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.max_lru_sessions = max;
        }
    }

    /// Set the maximum number of concurrent language server processes (from config).
    pub fn set_max_language_servers(&self, max: usize) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.max_language_servers = Some(max);
        }
    }

    /// Set priority workspace roots that are exempt from LRU eviction.
    pub fn set_priority_roots(&self, roots: HashSet<PathBuf>) {
        if let Ok(mut cfg) = self.config.write() {
            cfg.priority_roots = roots;
        }
    }

    /// Get the priority workspace roots.
    #[must_use]
    pub fn priority_roots(&self) -> HashSet<PathBuf> {
        self.config
            .read()
            .map_or_else(|_| HashSet::new(), |cfg| cfg.priority_roots.clone())
    }

    /// Acquire a per-language guard for the given language, booting the server if needed.
    ///
    /// The returned guard holds the per-language mutex for the duration of LSP I/O.
    /// Other languages remain accessible concurrently.
    ///
    /// # Errors
    /// Returns an error if the server cannot be started or has permanently failed.
    pub async fn get_or_start(
        &self,
        lang: Language,
    ) -> anyhow::Result<OwnedMutexGuard<LanguageState>> {
        let lock = self.language_lock(lang)?;
        let mut guard = lock.lock_owned().await;

        self.ensure_running(&mut guard, lang).await?;

        // For empty LRU pool, boot the initial root
        if Self::is_lru_empty(&guard) {
            let root = self.initial_root(lang);
            self.boot_lru_session(&mut guard, lang, &root).await?;
        }

        // Touch last-used for global LRU tracking
        self.touch_language(lang);

        Ok(guard)
    }

    /// Route a file to its language server, attaching the workspace folder if needed.
    ///
    /// Returns a per-language guard that the caller holds during LSP I/O.
    ///
    /// # Errors
    /// Returns an error if language cannot be detected or server fails to start.
    pub async fn route_for_file(
        &self,
        file_path: &Path,
    ) -> anyhow::Result<OwnedMutexGuard<LanguageState>> {
        let lang = language_for_file(file_path)
            .ok_or_else(|| anyhow::anyhow!("unknown language for {}", file_path.display()))?;

        let root = self
            .find_nearest_workspace(file_path, lang)
            .unwrap_or_else(|| self.project_root.clone());

        let lock = self.language_lock(lang)?;
        let mut guard = lock.lock_owned().await;

        self.ensure_running(&mut guard, lang).await?;
        self.route_with_root(&mut guard, lang, &root).await?;

        // Touch last-used for global LRU tracking
        self.touch_language(lang);

        Ok(guard)
    }

    /// Attach all discovered workspace folders for a language.
    ///
    /// Acquires the per-language lock internally.
    ///
    /// # Errors
    /// Returns an error if the server is not running or attachment fails.
    pub async fn attach_all_workspaces(&self, lang: Language) -> anyhow::Result<()> {
        let lock = self.language_lock(lang)?;
        let mut guard = lock.lock_owned().await;
        self.attach_all_workspaces_inner(&mut guard, lang).await
    }

    /// Attach all workspaces using an already-held language guard.
    ///
    /// Use this variant when you already hold the per-language lock.
    ///
    /// # Errors
    /// Returns an error if attachment fails.
    pub async fn attach_all_workspaces_with_guard(
        &self,
        lang: Language,
        guard: &mut OwnedMutexGuard<LanguageState>,
    ) -> anyhow::Result<()> {
        self.attach_all_workspaces_inner(guard, lang).await
    }

    /// Find the nearest discovered workspace root for a file.
    #[must_use]
    pub fn find_nearest_workspace(&self, file_path: &Path, lang: Language) -> Option<PathBuf> {
        self.workspace_roots
            .iter()
            .filter(|(l, _)| *l == lang)
            .filter(|(_, root)| file_path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(_, root)| root.clone())
    }

    /// Get all unique languages detected in the project.
    #[must_use]
    pub fn unique_languages(&self) -> Vec<Language> {
        let mut langs: Vec<Language> = self
            .workspace_roots
            .iter()
            .map(|(l, _)| *l)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        langs.sort_by_key(|l| l.name());
        langs
    }

    /// Pre-warm LRU sessions for priority workspace roots.
    ///
    /// # Errors
    /// Returns an error if a session fails to boot (non-fatal, logged by caller).
    pub async fn warm_priority_roots(&self) -> anyhow::Result<()> {
        let roots_to_warm: Vec<(Language, PathBuf)> = {
            let cfg = self.config.read().map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
            self.workspace_roots
                .iter()
                .filter(|(_, root)| cfg.priority_roots.contains(root))
                .cloned()
                .collect()
        };

        for (lang, root) in &roots_to_warm {
            // Only meaningful for LRU strategy
            let Ok(lock) = self.language_lock(*lang) else { continue };
            let mut guard = lock.lock_owned().await;

            // Skip if not LRU or already warm
            let is_lru = matches!(guard.strategy, Some(ServerStrategy::LruPerRoot(_)));
            if !is_lru {
                continue;
            }
            let already_warm = match &guard.strategy {
                Some(ServerStrategy::LruPerRoot(slots)) => slots.contains_key(root),
                _ => true,
            };
            if already_warm {
                continue;
            }

            info!("pre-warming priority workspace: {lang}:{}", root.display());
            self.boot_lru_session(&mut guard, *lang, root).await?;
        }
        Ok(())
    }

    /// Get all active (running) languages.
    #[must_use]
    pub fn active_languages(&self) -> Vec<Language> {
        let mut langs: Vec<Language> = self
            .languages
            .iter()
            .filter(|(_, lock)| {
                lock.try_lock()
                    .map_or(true, |g| g.is_ready()) // treat "in use" as active
            })
            .map(|(l, _)| *l)
            .collect();
        langs.sort_by_key(|l| l.name());
        langs
    }

    /// Get status info for all known languages. Uses `try_lock` — busy languages show as "ready".
    #[must_use]
    pub fn status(&self) -> Vec<ServerStatus> {
        let mut statuses = Vec::new();
        let mut seen = HashSet::new();

        for (lang, _) in &self.workspace_roots {
            if !seen.insert(lang) {
                continue;
            }

            let total_folders = self
                .workspace_roots
                .iter()
                .filter(|(l, _)| l == lang)
                .count();

            let Some(lock) = self.languages.get(lang) else {
                statuses.push(pending_status(*lang, total_folders));
                continue;
            };

            match lock.try_lock() {
                Ok(guard) => {
                    statuses.push(slot_status(*lang, &guard, total_folders));
                }
                Err(_) => {
                    // Lock is held — server is busy (actively processing a query)
                    statuses.push(ServerStatus {
                        server_name: default_server_name(*lang),
                        language: lang.name().to_string(),
                        status: "ready",
                        uptime_secs: 0,
                        open_files: 0,
                        attached_folders: 0,
                        total_folders,
                    });
                }
            }
        }

        statuses
    }

    /// Readiness summary: how many languages have running servers.
    #[must_use]
    pub fn readiness(&self) -> Readiness {
        let unique_langs: HashSet<Language> =
            self.workspace_roots.iter().map(|(l, _)| *l).collect();
        let ready = self
            .languages
            .iter()
            .filter(|(_, lock)| {
                lock.try_lock().map_or(true, |g| g.is_ready())
            })
            .count();
        Readiness {
            ready,
            total: unique_langs.len(),
        }
    }

    /// Check if a language has a running session (best-effort, non-blocking).
    #[must_use]
    pub fn is_ready(&self, lang: Language) -> bool {
        self.languages
            .get(&lang)
            .and_then(|l| l.try_lock().ok())
            .is_some_and(|g| g.is_ready())
    }

    /// Get all known workspace roots.
    #[must_use]
    pub fn workspace_roots(&self) -> &[(Language, PathBuf)] {
        &self.workspace_roots
    }

    /// Get the project root.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Shut down all active LSP sessions.
    pub async fn shutdown_all(&self) {
        for (lang, lock) in &self.languages {
            let mut guard = lock.lock().await;
            match guard.strategy.take() {
                Some(ServerStrategy::MultiRoot(mut slot)) => {
                    shutdown_slot(*lang, None, &mut slot).await;
                }
                Some(ServerStrategy::LruPerRoot(slots)) => {
                    for (root, mut slot) in slots {
                        shutdown_slot(*lang, Some(&root), &mut slot).await;
                    }
                }
                None => {}
            }
        }
    }

    // ─── Internal helpers ──────────────────────────────────────────────

    /// Record that a language was just used (for global LRU tracking).
    fn touch_language(&self, lang: Language) {
        if let Ok(mut map) = self.last_used.write() {
            map.insert(lang, Instant::now());
        }
    }

    /// Count how many languages currently have running servers.
    fn active_language_count(&self) -> usize {
        self.languages
            .iter()
            .filter(|(_, lock)| lock.try_lock().map_or(true, |g| g.is_ready()))
            .count()
    }

    /// If the global language-server cap is reached, evict the least-recently-used language.
    ///
    /// This shuts down the entire language server (all sessions) for the evicted language.
    /// The evicted language will be re-booted on next query (lazy init).
    async fn evict_global_lru_if_needed(&self, current_lang: Language) {
        let max = match self.config.read() {
            Ok(cfg) => cfg.max_language_servers,
            Err(_) => return,
        };
        let Some(max) = max else { return };

        let active = self.active_language_count();
        if active < max {
            return;
        }

        // Find the LRU language (not current, not in-use)
        let victim = {
            let Ok(last_used) = self.last_used.read() else { return };
            self.languages
                .keys()
                .filter(|&&l| l != current_lang)
                .filter(|l| {
                    self.languages
                        .get(l)
                        .and_then(|lock| lock.try_lock().ok())
                        .is_some_and(|g| g.is_ready())
                })
                .min_by_key(|l| last_used.get(l).copied().unwrap_or(Instant::now()))
                .copied()
        };

        if let Some(victim_lang) = victim {
            info!(
                "global LRU: evicting {victim_lang} (cap={max}, active={active}) to make room for {current_lang}"
            );
            if let Err(e) = self.restart_language(victim_lang).await {
                warn!("global LRU eviction of {victim_lang} failed: {e}");
            }
            // Remove from last_used tracking
            if let Ok(mut map) = self.last_used.write() {
                map.remove(&victim_lang);
            }
        }
    }

    /// Shut down and clear the server for a language so it will be re-booted on next query.
    ///
    /// # Errors
    /// Returns `Err` if the language is not registered in this multiplexer.
    pub async fn restart_language(&self, lang: Language) -> anyhow::Result<()> {
        let lock = self.language_lock(lang)?;
        let mut guard = lock.lock().await;
        match guard.strategy.take() {
            Some(ServerStrategy::MultiRoot(mut slot)) => {
                shutdown_slot(lang, None, &mut slot).await;
            }
            Some(ServerStrategy::LruPerRoot(slots)) => {
                for (root, mut slot) in slots {
                    shutdown_slot(lang, Some(&root), &mut slot).await;
                }
            }
            None => {}
        }
        guard.failed = None;
        guard.crash_count = 0;
        Ok(())
    }

    /// Get the `Arc<Mutex<LanguageState>>` for a language.
    ///
    /// # Errors
    /// Returns `Err` if the language has no registered slot in this multiplexer.
    pub fn language_lock(&self, lang: Language) -> anyhow::Result<Arc<Mutex<LanguageState>>> {
        self.languages
            .get(&lang)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no language slot for {lang}"))
    }

    /// Ensure a server for the given language is healthy or boot one.
    /// Caller must already hold the per-language guard.
    async fn ensure_running(
        &self,
        state: &mut LanguageState,
        lang: Language,
    ) -> anyhow::Result<()> {
        if let Some(ref reason) = state.failed {
            bail!("LSP {lang} permanently failed: {reason}");
        }

        // No strategy yet → first boot
        if state.strategy.is_none() {
            // Check global language-server cap before booting
            self.evict_global_lru_if_needed(lang).await;
            let root = self.initial_root(lang);
            return self.boot_first_server(state, lang, &root).await;
        }

        // Health check
        let action = check_health(state, lang);
        match action {
            HealthAction::Healthy | HealthAction::LruCleaned => Ok(()),
            HealthAction::MultiRootCrashed { crash_count } => {
                if crash_count >= MAX_CRASHES {
                    let reason = format!("crashed {crash_count} times, giving up");
                    state.failed = Some(reason.clone());
                    tracing::error!("LSP {lang} permanently failed: {reason}");
                    bail!("LSP {lang} permanently failed: {reason}");
                }
                state.crash_count = crash_count;
                state.strategy = None;
                let delay_secs = BACKOFF_DELAYS_SECS
                    .get(crash_count.saturating_sub(1) as usize)
                    .copied()
                    .unwrap_or(60);
                warn!("LSP {lang} crashed ({crash_count}×), restarting in {delay_secs}s");
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                let root = self.initial_root(lang);
                self.boot_first_server(state, lang, &root).await
            }
        }
    }

    /// Boot the first server for a language and determine the strategy.
    async fn boot_first_server(
        &self,
        state: &mut LanguageState,
        lang: Language,
        workspace_root: &Path,
    ) -> anyhow::Result<()> {
        let slot = self.boot_slot(lang, workspace_root).await?;

        let is_multi_root = slot.session.client.supports_workspace_folders();
        let strategy_name = if is_multi_root { "multi-root" } else { "LRU" };
        info!(
            "multiplexer: {lang} ({}) using {strategy_name} strategy",
            slot.server_name
        );

        if is_multi_root {
            state.strategy = Some(ServerStrategy::MultiRoot(Box::new(slot)));
        } else {
            let mut slots = HashMap::new();
            slots.insert(workspace_root.to_path_buf(), slot);
            state.strategy = Some(ServerStrategy::LruPerRoot(slots));
        }
        Ok(())
    }

    /// Boot a new LSP session for an LRU slot.
    async fn boot_lru_session(
        &self,
        state: &mut LanguageState,
        lang: Language,
        workspace_root: &Path,
    ) -> anyhow::Result<()> {
        // Evict if at cap
        let max_sessions = self
            .config
            .read()
            .map(|c| c.max_lru_sessions)
            .unwrap_or(DEFAULT_MAX_LRU_SESSIONS);

        let needs_evict = match &state.strategy {
            Some(ServerStrategy::LruPerRoot(slots)) => slots.len() >= max_sessions,
            _ => false,
        };
        if needs_evict {
            self.evict_lru(state, lang).await?;
        }

        let slot = self.boot_slot(lang, workspace_root).await?;
        info!(
            "multiplexer: LRU {lang} ({}) @ {} ready",
            slot.server_name,
            workspace_root.display()
        );

        match &mut state.strategy {
            Some(ServerStrategy::LruPerRoot(slots)) => {
                slots.insert(workspace_root.to_path_buf(), slot);
            }
            _ => bail!("expected LRU strategy for {lang}"),
        }
        Ok(())
    }

    /// Boot a single LSP server slot (shared by both strategies).
    async fn boot_slot(
        &self,
        lang: Language,
        workspace_root: &Path,
    ) -> anyhow::Result<ServerSlot> {
        let (binary_path, entry) = install::ensure_server(lang).await?;

        let mut client =
            LspClient::start_with_binary(&binary_path, entry.args, lang, workspace_root)
                .map_err(|e| anyhow::anyhow!("{e}"))?;

        if let Some(store) = self.diagnostic_store.get() {
            client.set_diagnostic_store(Arc::clone(store));
        }

        client
            .initialize(workspace_root)
            .await
            .with_context(|| format!("LSP initialize failed for {lang}"))?;

        let server_name = client.server_name().to_string();


        let mut file_tracker = FileTracker::new(lang);
        if let Some(warmup_file) = find_warmup_file(workspace_root, lang) {
            if let Err(e) = file_tracker
                .ensure_open(&warmup_file, client.transport_mut())
                .await
            {
                debug!("warmup file open failed (non-fatal): {e}");
            } else {
                debug!("warmup: opened {}", warmup_file.display());
                probe_until_ready(&mut client, &warmup_file).await;
            }
        }

        let now = Instant::now();
        Ok(ServerSlot {
            session: LspSession {
                client,
                file_tracker,
            },
            started_at: now,
            last_used_at: now,
            server_name,
        })
    }

    /// Evict the least-recently-used LRU session for a language.
    async fn evict_lru(
        &self,
        state: &mut LanguageState,
        lang: Language,
    ) -> anyhow::Result<()> {
        let priority_roots = self
            .config
            .read()
            .map(|c| c.priority_roots.clone())
            .unwrap_or_default();

        let oldest_root = match &state.strategy {
            Some(ServerStrategy::LruPerRoot(slots)) => slots
                .iter()
                .filter(|(root, _)| !priority_roots.contains(*root))
                .min_by_key(|(_, s)| s.last_used_at)
                .map(|(root, _)| root.clone()),
            _ => None,
        };

        if oldest_root.is_none() {
            if let Some(ServerStrategy::LruPerRoot(slots)) = &state.strategy {
                if !slots.is_empty() {
                    warn!(
                        "all {} LRU sessions for {lang} are priority — exceeding cap",
                        slots.len()
                    );
                }
            }
            return Ok(());
        }

        if let Some(root) = oldest_root {
            if let Some(ServerStrategy::LruPerRoot(slots)) = &mut state.strategy {
                if let Some(mut slot) = slots.remove(&root) {
                    info!("evicting LRU session for {lang}:{}", root.display());
                    shutdown_slot(lang, Some(&root), &mut slot).await;
                }
            }
        }
        Ok(())
    }

    /// Route to the correct session for a language + workspace root.
    async fn route_with_root(
        &self,
        state: &mut LanguageState,
        lang: Language,
        root: &Path,
    ) -> anyhow::Result<()> {
        match &state.strategy {
            Some(ServerStrategy::MultiRoot(_)) => {
                // Attach folder if not already attached
                let Some(ServerStrategy::MultiRoot(slot)) = &mut state.strategy else {
                    anyhow::bail!("unexpected server strategy for {lang}")
                };
                if !slot.session.client.is_folder_attached(root) {
                    slot.session.client.attach_folder(root).await?;
                }
                Ok(())
            }
            Some(ServerStrategy::LruPerRoot(_)) => {
                let needs_boot = match &state.strategy {
                    Some(ServerStrategy::LruPerRoot(slots)) => !slots.contains_key(root),
                    _ => false,
                };
                if needs_boot {
                    self.boot_lru_session(state, lang, root).await?;
                }
                // Touch last_used_at
                if let Some(ServerStrategy::LruPerRoot(slots)) = &mut state.strategy {
                    if let Some(slot) = slots.get_mut(root) {
                        slot.last_used_at = Instant::now();
                    }
                }
                Ok(())
            }
            None => bail!("no server for {lang}"),
        }
    }

    /// Attach all workspace folders to a multi-root server.
    async fn attach_all_workspaces_inner(
        &self,
        state: &mut LanguageState,
        lang: Language,
    ) -> anyhow::Result<()> {
        // Only meaningful for MultiRoot
        if !matches!(state.strategy, Some(ServerStrategy::MultiRoot(_))) {
            return Ok(());
        }

        let roots: Vec<PathBuf> = self
            .workspace_roots
            .iter()
            .filter(|(l, _)| *l == lang)
            .map(|(_, r)| r.clone())
            .collect();

        if let Some(ServerStrategy::MultiRoot(slot)) = &mut state.strategy {
            for root in &roots {
                if !slot.session.client.is_folder_attached(root) {
                    slot.session.client.attach_folder(root).await?;
                }
            }
        }
        Ok(())
    }

    /// Check if a language uses LRU strategy with an empty pool.
    fn is_lru_empty(state: &LanguageState) -> bool {
        matches!(
            &state.strategy,
            Some(ServerStrategy::LruPerRoot(slots)) if slots.is_empty()
        )
    }

    /// Get the first workspace root for a language, or fall back to project root.
    fn initial_root(&self, lang: Language) -> PathBuf {
        self.workspace_roots
            .iter()
            .find(|(l, _)| *l == lang)
            .map_or_else(|| self.project_root.clone(), |(_, r)| r.clone())
    }
}

/// Result of checking strategy health.
enum HealthAction {
    Healthy,
    MultiRootCrashed { crash_count: u32 },
    LruCleaned,
}

/// Check the health of a language state (does not hold outer pool lock).
fn check_health(state: &mut LanguageState, lang: Language) -> HealthAction {
    match &mut state.strategy {
        Some(ServerStrategy::MultiRoot(slot)) => {
            if slot.session.client.transport_mut().is_alive() {
                if slot.started_at.elapsed().as_secs() >= STABILITY_RESET_SECS {
                    state.crash_count = 0;
                }
                HealthAction::Healthy
            } else {
                state.crash_count += 1;
                let crash_count = state.crash_count;
                warn!("LSP {lang} crashed (count: {crash_count})");
                HealthAction::MultiRootCrashed { crash_count }
            }
        }
        Some(ServerStrategy::LruPerRoot(slots)) => {
            let mut dead = Vec::new();
            for (root, slot) in slots.iter_mut() {
                if !slot.session.client.transport_mut().is_alive() {
                    dead.push(root.clone());
                }
            }
            for r in &dead {
                warn!("LRU session for {lang}:{} crashed, removed", r.display());
                slots.remove(r);
            }
            HealthAction::LruCleaned
        }
        None => HealthAction::Healthy,
    }
}

/// Build a pending `ServerStatus` for a language.
fn pending_status(lang: Language, total_folders: usize) -> ServerStatus {
    ServerStatus {
        server_name: default_server_name(lang),
        language: lang.name().to_string(),
        status: "pending",
        uptime_secs: 0,
        open_files: 0,
        attached_folders: 0,
        total_folders,
    }
}

/// Build a `ServerStatus` from a `LanguageState` guard.
fn slot_status(lang: Language, state: &LanguageState, total_folders: usize) -> ServerStatus {
    match &state.strategy {
        Some(ServerStrategy::MultiRoot(slot)) => ServerStatus {
            server_name: slot.server_name.clone(),
            language: lang.name().to_string(),
            status: "ready",
            uptime_secs: slot.started_at.elapsed().as_secs(),
            open_files: slot.session.file_tracker.open_count(),
            attached_folders: slot.session.client.attached_folders().len(),
            total_folders,
        },
        Some(ServerStrategy::LruPerRoot(slots)) if !slots.is_empty() => {
            let total_files: usize =
                slots.values().map(|s| s.session.file_tracker.open_count()).sum();
            let oldest = slots
                .values()
                .map(|s| s.started_at)
                .min()
                .unwrap_or_else(Instant::now);
            let name = slots
                .values()
                .next()
                .map_or_else(|| default_server_name(lang), |s| s.server_name.clone());
            ServerStatus {
                server_name: name,
                language: lang.name().to_string(),
                status: "ready",
                uptime_secs: oldest.elapsed().as_secs(),
                open_files: total_files,
                attached_folders: slots.len(),
                total_folders,
            }
        }
        _ => {
            let status = if state.failed.is_some() { "failed" } else { "pending" };
            ServerStatus {
                server_name: default_server_name(lang),
                language: lang.name().to_string(),
                status,
                uptime_secs: 0,
                open_files: 0,
                attached_folders: 0,
                total_folders,
            }
        }
    }
}

/// Gracefully shut down a server slot.
async fn shutdown_slot(lang: Language, root: Option<&Path>, slot: &mut ServerSlot) {
    let _ = slot
        .session
        .file_tracker
        .close_all(slot.session.client.transport_mut())
        .await;
    let label = root.map_or_else(String::new, |r| format!(":{}", r.display()));
    if let Err(e) = slot.session.client.shutdown().await {
        warn!("LSP shutdown error for {lang}{label}: {e}");
    }
}

/// Get the default server binary name for a language.
fn default_server_name(lang: Language) -> String {
    use super::registry::get_entry;
    get_entry(lang).map_or_else(|| lang.name().to_string(), |e| e.binary_name.to_string())
}


/// Probe the LSP server with documentSymbol until it responds or max attempts reached.
///
/// Uses `wait_for_response_with_timeout` so every response is consumed internally —
/// no orphaned responses in the transport pipe. Budget: 5 × (2s + 500ms) ≈ 12.5s max.
async fn probe_until_ready(client: &mut LspClient, warmup_file: &std::path::Path) {
    use super::client::path_to_uri;

    const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
    const RETRY_DELAY: Duration = Duration::from_millis(500);
    const MAX_ATTEMPTS: u8 = 5;

    let uri = match path_to_uri(warmup_file) {
        Ok(u) => u,
        Err(e) => {
            debug!("probe_until_ready: could not get URI: {e}");
            return;
        }
    };

    for attempt in 0..MAX_ATTEMPTS {
        let probe = client
            .transport_mut()
            .send_request(
                "textDocument/documentSymbol",
                serde_json::json!({ "textDocument": { "uri": uri.as_str() } }),
            )
            .await;
        if let Ok(req_id) = probe {
            match client.wait_for_response_with_timeout(req_id, PROBE_TIMEOUT).await {
                Ok(resp) if resp != serde_json::Value::Null => {
                    debug!("probe_until_ready: ready after {} attempts", attempt + 1);
                    return;
                }
                Ok(_) => {
                    debug!("probe_until_ready: null response on attempt {}", attempt + 1);
                }
                Err(e) => {
                    debug!("probe_until_ready: attempt {} failed: {e}", attempt + 1);
                }
            }
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
    debug!("probe_until_ready: giving up after {MAX_ATTEMPTS} attempts");
}

/// Find a single representative source file to open for warmup.
fn find_warmup_file(workspace_root: &Path, lang: Language) -> Option<PathBuf> {
    let extensions = lang.extensions();
    let search_dirs = [
        workspace_root.join("src"),
        workspace_root.join("lib"),
        workspace_root.to_path_buf(),
    ];

    for dir in &search_dirs {
        if let Some(f) = find_first_source_file(dir, extensions) {
            return Some(f);
        }
    }
    None
}

fn find_first_source_file(dir: &Path, extensions: &[&str]) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplexer_starts_empty() {
        let mux = LspMultiplexer::new(PathBuf::from("/tmp"), vec![]);
        assert!(mux.active_languages().is_empty());
        assert!(mux.status().is_empty());
    }

    #[test]
    fn not_ready_reason_display() {
        assert_eq!(
            NotReadyReason::NotStarted.to_string(),
            "server not yet started"
        );
        assert_eq!(
            NotReadyReason::Failed("crashed".to_string()).to_string(),
            "server failed: crashed"
        );
        assert_eq!(
            NotReadyReason::NotFound.to_string(),
            "no server configured for this scope"
        );
    }

    #[test]
    fn readiness_tracks_unique_languages() {
        let roots = vec![
            (Language::TypeScript, PathBuf::from("/project/packages/api")),
            (Language::TypeScript, PathBuf::from("/project/packages/web")),
            (Language::Rust, PathBuf::from("/project")),
        ];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        let r = mux.readiness();
        assert_eq!(r.ready, 0);
        assert_eq!(r.total, 2);
        assert!(!r.is_all_ready());
    }

    #[test]
    fn unique_languages_deduplicates() {
        let roots = vec![
            (Language::TypeScript, PathBuf::from("/project/packages/api")),
            (Language::TypeScript, PathBuf::from("/project/packages/web")),
            (Language::Rust, PathBuf::from("/project")),
        ];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        let langs = mux.unique_languages();
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn status_shows_pending_with_folder_counts() {
        let roots = vec![
            (Language::TypeScript, PathBuf::from("/project/packages/api")),
            (Language::TypeScript, PathBuf::from("/project/packages/web")),
        ];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        let statuses = mux.status();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].status, "pending");
        assert_eq!(statuses[0].total_folders, 2);
        assert_eq!(statuses[0].attached_folders, 0);
    }

    #[test]
    fn find_nearest_workspace_picks_deepest() {
        let roots = vec![
            (Language::TypeScript, PathBuf::from("/project")),
            (
                Language::TypeScript,
                PathBuf::from("/project/packages/api"),
            ),
        ];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        let result = mux.find_nearest_workspace(
            Path::new("/project/packages/api/src/main.ts"),
            Language::TypeScript,
        );
        assert_eq!(result, Some(PathBuf::from("/project/packages/api")));
    }

    #[test]
    fn find_nearest_workspace_returns_none_for_wrong_lang() {
        let roots = vec![(Language::Rust, PathBuf::from("/project"))];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        let result = mux.find_nearest_workspace(
            Path::new("/project/src/index.ts"),
            Language::TypeScript,
        );
        assert!(result.is_none());
    }

    #[test]
    fn initial_root_picks_first_for_language() {
        let roots = vec![
            (Language::TypeScript, PathBuf::from("/project/packages/api")),
            (Language::TypeScript, PathBuf::from("/project/packages/web")),
        ];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        assert_eq!(
            mux.initial_root(Language::TypeScript),
            PathBuf::from("/project/packages/api")
        );
    }

    #[test]
    fn initial_root_falls_back_to_project_root() {
        let roots = vec![(Language::Rust, PathBuf::from("/project"))];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        assert_eq!(mux.initial_root(Language::Go), PathBuf::from("/project"));
    }

    #[test]
    fn is_ready_for_unbooted() {
        let roots = vec![(Language::Rust, PathBuf::from("/project"))];
        let mux = LspMultiplexer::new(PathBuf::from("/project"), roots);
        assert!(!mux.is_ready(Language::Rust));
    }

    #[test]
    fn find_warmup_file_prefers_src() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("build.rs"), "fn main() {}").unwrap();

        let result = find_warmup_file(dir.path(), Language::Rust);
        assert!(result.is_some());
        assert!(result.unwrap().starts_with(&src));
    }

    #[test]
    fn find_warmup_file_finds_ts() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("index.ts"), "export {}").unwrap();

        let result = find_warmup_file(dir.path(), Language::TypeScript);
        assert!(result.is_some());
        assert_eq!(result.unwrap().extension().unwrap(), "ts");
    }

    #[test]
    fn find_warmup_file_returns_none_for_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_warmup_file(dir.path(), Language::Go);
        assert!(result.is_none());
    }

    #[test]
    fn set_max_lru_sessions() {
        let mux = LspMultiplexer::new(PathBuf::from("/project"), vec![]);
        mux.set_max_lru_sessions(5);
        assert_eq!(
            mux.config.read().unwrap().max_lru_sessions,
            5
        );
    }

    #[test]
    fn set_and_get_priority_roots() {
        let mux = LspMultiplexer::new(PathBuf::from("/project"), vec![]);
        assert!(mux.priority_roots().is_empty());

        let roots: HashSet<PathBuf> = [
            PathBuf::from("/project/packages/core"),
            PathBuf::from("/project/packages/api"),
        ]
        .into();
        mux.set_priority_roots(roots);
        assert_eq!(mux.priority_roots().len(), 2);
        assert!(mux.priority_roots().contains(&PathBuf::from("/project/packages/core")));
    }

    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn multiplexer_starts_lsp_on_demand() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust-hello");
        let roots = vec![(Language::Rust, fixture.clone())];
        let mux = LspMultiplexer::new(fixture.clone(), roots);

        let guard = mux.get_or_start(Language::Rust).await;
        assert!(guard.is_ok());
        assert_eq!(mux.active_languages(), vec![Language::Rust]);

        mux.shutdown_all().await;
    }

    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn multiplexer_reuses_existing_client() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust-hello");
        let roots = vec![(Language::Rust, fixture.clone())];
        let mux = LspMultiplexer::new(fixture.clone(), roots);

        mux.get_or_start(Language::Rust).await.unwrap();
        assert_eq!(mux.active_languages().len(), 1);

        mux.get_or_start(Language::Rust).await.unwrap();
        assert_eq!(mux.active_languages().len(), 1);

        mux.shutdown_all().await;
    }

    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn multiplexer_shutdown_all() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust-hello");
        let roots = vec![(Language::Rust, fixture.clone())];
        let mux = LspMultiplexer::new(fixture.clone(), roots);

        mux.get_or_start(Language::Rust).await.unwrap();
        assert_eq!(mux.active_languages().len(), 1);

        mux.shutdown_all().await;
        assert!(mux.active_languages().is_empty());
    }

    #[tokio::test]
    #[ignore = "requires rust-analyzer installed"]
    async fn multiplexer_status_shows_info() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust-hello");
        let roots = vec![(Language::Rust, fixture.clone())];
        let mux = LspMultiplexer::new(fixture.clone(), roots);

        mux.get_or_start(Language::Rust).await.unwrap();

        let statuses = mux.status();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].language, "rust");
        assert_eq!(statuses[0].server_name, "rust-analyzer");
        assert_eq!(statuses[0].status, "ready");
        assert_eq!(statuses[0].total_folders, 1);

        mux.shutdown_all().await;
    }
}
