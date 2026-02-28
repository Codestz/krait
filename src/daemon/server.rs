use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Mutex};
use tracing::{debug, error, info};

use crate::commands::{check, edit, find, fix, format as fmt, hover, list, read, rename};
use crate::lsp::client::path_to_uri;
use crate::index::builder;
use crate::index::cache_query;
use crate::index::store::IndexStore;
use crate::index::watcher::{self, DirtyFiles};
use crate::config::{self, ConfigSource};
use crate::detect::{self, Language, language_for_file};
use crate::lsp::diagnostics::DiagnosticStore;
use crate::lsp::pool::LspMultiplexer;
use crate::lsp::registry::{find_server, get_entry};
use crate::lsp::router;
use crate::protocol::{Request, Response};

/// Grace period after daemon start during which "still indexing" errors are expected.
const INDEXING_GRACE_PERIOD_SECS: u64 = 60;

/// Shared daemon state accessible from all connection handlers.
pub struct DaemonState {
    pub start_time: Instant,
    pub last_activity: Arc<Mutex<Instant>>,
    pub project_root: PathBuf,
    pub languages: Vec<Language>,
    pub pool: Arc<LspMultiplexer>,
    pub config_source: ConfigSource,
    /// All discovered workspace roots (for workspace registry).
    pub package_roots: Vec<(Language, PathBuf)>,
    /// Persistent symbol index for cache-first queries.
    pub index: std::sync::Mutex<Option<IndexStore>>,
    /// In-memory set of files changed since last index (fed by file watcher).
    pub dirty_files: DirtyFiles,
    /// Whether the file watcher is running (determines BLAKE3 fallback behavior).
    pub watcher_active: bool,
    /// File watcher handle — kept alive by ownership; dropped on shutdown.
    _watcher: Option<notify_debouncer_full::Debouncer<notify_debouncer_full::notify::RecommendedWatcher, notify_debouncer_full::FileIdMap>>,
    shutdown_tx: watch::Sender<bool>,
    /// LSP diagnostic store — fed by `textDocument/publishDiagnostics` notifications.
    pub diagnostic_store: Arc<DiagnosticStore>,
}

impl DaemonState {
    fn new(
        shutdown_tx: watch::Sender<bool>,
        project_root: PathBuf,
        languages: Vec<Language>,
        package_roots: Vec<(Language, PathBuf)>,
        config_source: ConfigSource,
    ) -> Self {
        let now = Instant::now();
        let index = Self::open_index(&project_root);
        let dirty_files = DirtyFiles::new();

        let diagnostic_store = Arc::new(DiagnosticStore::new());
        let pool = Arc::new(LspMultiplexer::new(project_root.clone(), package_roots.clone()));
        pool.set_diagnostic_store(Arc::clone(&diagnostic_store));

        // Start file watcher — clears stale diagnostics when files change on disk
        let extensions = language_extensions(&languages);
        let watcher_result = watcher::start_watcher(
            &project_root,
            &extensions,
            dirty_files.clone(),
            Some(Arc::clone(&diagnostic_store)),
        );
        let (watcher_handle, watcher_active) = match watcher_result {
            Ok(w) => (Some(w), true),
            Err(e) => {
                debug!("file watcher unavailable, using BLAKE3 fallback: {e}");
                (None, false)
            }
        };

        Self {
            start_time: now,
            last_activity: Arc::new(Mutex::new(now)),
            pool,
            project_root,
            languages,
            config_source,
            package_roots,
            index: std::sync::Mutex::new(index),
            dirty_files,
            watcher_active,
            _watcher: watcher_handle,
            shutdown_tx,
            diagnostic_store,
        }
    }

    /// Try to open the index DB if it exists. Auto-deletes corrupted databases.
    fn open_index(project_root: &Path) -> Option<IndexStore> {
        let db_path = project_root.join(".krait/index.db");
        if !db_path.exists() {
            return None;
        }
        match IndexStore::open(&db_path) {
            Ok(store) => Some(store),
            Err(e) => {
                info!("index DB corrupted ({e}), deleting for fresh rebuild");
                let _ = std::fs::remove_file(&db_path);
                None
            }
        }
    }

    /// Re-open the index store (called after `handle_init` completes).
    fn refresh_index(&self) {
        if let Ok(mut guard) = self.index.lock() {
            *guard = Self::open_index(&self.project_root);
        }
    }

    /// Get the dirty files reference for cache queries.
    ///
    /// Returns `Some` only if the file watcher is active. When `None`,
    /// cache queries fall back to per-file BLAKE3 hashing.
    fn dirty_files_ref(&self) -> Option<&DirtyFiles> {
        if self.watcher_active {
            Some(&self.dirty_files)
        } else {
            None
        }
    }

    async fn touch(&self) {
        *self.last_activity.lock().await = Instant::now();
    }

    fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Run the daemon's accept loop until shutdown is requested or idle timeout fires.
///
/// # Errors
/// Returns an error if the UDS listener fails to bind.
pub async fn run_server(
    socket_path: &Path,
    idle_timeout: std::time::Duration,
    project_root: &Path,
) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    info!("daemon listening on {}", socket_path.display());

    let languages = detect::detect_languages(project_root);

    // Load config: krait.toml → .krait/config.toml → auto-detection
    let loaded = config::load(project_root);
    let config_source = loaded.source.clone();
    let package_roots = if let Some(ref cfg) = loaded.config {
        let roots = config::config_to_package_roots(cfg, project_root);
        info!(
            "config: {} ({} workspaces)",
            loaded.source.label(),
            roots.len()
        );
        roots
    } else {
        detect::find_package_roots(project_root)
    };

    if package_roots.len() > 1 {
        if loaded.config.is_none() {
            info!(
                "monorepo detected: {} workspace roots",
                package_roots.len()
            );
        }
        for (lang, root) in &package_roots {
            debug!("  workspace: {lang}:{}", root.display());
        }
    } else if !package_roots.is_empty() {
        info!(
            "project: {} workspace root(s)",
            package_roots.len()
        );
    }

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let state = DaemonState::new(
        shutdown_tx,
        project_root.to_path_buf(),
        languages,
        package_roots,
        config_source,
    );
    apply_pool_config(&state, &loaded, project_root);
    let state = Arc::new(state);

    // When config exists, boot one server per language in background.
    if matches!(state.config_source, ConfigSource::KraitToml | ConfigSource::LegacyKraitToml) {
        spawn_background_boot(Arc::clone(&state));
    }

    // Install SIGTERM handler for graceful shutdown
    let state_for_sigterm = Arc::clone(&state);
    tokio::spawn(async move {
        if let Ok(mut sigterm) = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            sigterm.recv().await;
            info!("received SIGTERM, shutting down gracefully");
            state_for_sigterm.pool.shutdown_all().await;
            state_for_sigterm.request_shutdown();
        }
    });

    loop {
        let idle_deadline = {
            let last = *state.last_activity.lock().await;
            last + idle_timeout
        };
        let sleep_dur = idle_deadline.saturating_duration_since(Instant::now());

        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, &state).await {
                                error!("connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept error: {e}");
                    }
                }
            }
            () = tokio::time::sleep(sleep_dur) => {
                let last = *state.last_activity.lock().await;
                if last.elapsed() >= idle_timeout {
                    info!("idle timeout reached, shutting down");
                    state.pool.shutdown_all().await;
                    break;
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("shutdown requested");
                    state.pool.shutdown_all().await;
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Apply config-driven pool settings (priority workspaces, max sessions).
fn apply_pool_config(
    state: &DaemonState,
    loaded: &config::LoadedConfig,
    project_root: &Path,
) {
    if let Some(ref cfg) = loaded.config {
        if let Some(max) = cfg.max_active_sessions {
            state.pool.set_max_lru_sessions(max);
            info!("config: max_active_sessions={max}");
        }
        if let Some(max) = cfg.max_language_servers {
            state.pool.set_max_language_servers(max);
            info!("config: max_language_servers={max}");
        }
        if !cfg.primary_workspaces.is_empty() {
            let priority: HashSet<PathBuf> = cfg
                .primary_workspaces
                .iter()
                .map(|p| project_root.join(p))
                .collect();
            info!("config: {} primary workspaces", priority.len());
            state.pool.set_priority_roots(priority);
        }
    }
}

/// Boot one server per language in the background so first query doesn't wait.
///
/// With per-language mutexes, each language boots truly concurrently —
/// TypeScript and Go no longer block each other.
#[allow(clippy::needless_pass_by_value)]
fn spawn_background_boot(state: Arc<DaemonState>) {
    let langs = state.pool.unique_languages();
    let priority = state.pool.priority_roots();
    info!("background boot: starting {} language servers", langs.len());

    // Boot each language concurrently
    for lang in langs {
        let pool = Arc::clone(&state.pool);
        tokio::spawn(async move {
            match pool.get_or_start(lang).await {
                Ok(mut guard) => {
                    if let Err(e) = pool.attach_all_workspaces_with_guard(lang, &mut guard).await {
                        debug!("background boot: attach failed for {lang}: {e}");
                    }
                    info!("booted {lang}");
                }
                Err(e) => debug!("boot failed {lang}: {e}"),
            }
        });
    }

    // Pre-warm LRU priority roots
    if !priority.is_empty() {
        let pool = Arc::clone(&state.pool);
        tokio::spawn(async move {
            if let Err(e) = pool.warm_priority_roots().await {
                debug!("priority warmup failed: {e}");
            }
        });
    }
}

async fn handle_connection(mut stream: UnixStream, state: &DaemonState) -> anyhow::Result<()> {
    state.touch().await;

    let len = stream.read_u32().await?;
    if len > crate::protocol::MAX_FRAME_SIZE {
        anyhow::bail!("oversized frame: {len} bytes");
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;

    let request: Request = serde_json::from_slice(&buf)?;
    debug!("received request: {request:?}");

    let response = dispatch(&request, state).await;

    let response_bytes = serde_json::to_vec(&response)?;
    let response_len = u32::try_from(response_bytes.len())?;
    stream.write_u32(response_len).await?;
    stream.write_all(&response_bytes).await?;
    stream.flush().await?;

    Ok(())
}

async fn dispatch(request: &Request, state: &DaemonState) -> Response {
    match request {
        Request::Status => build_status_response(state),
        Request::DaemonStop => {
            state.pool.shutdown_all().await;
            state.request_shutdown();
            Response::ok(json!({"message": "shutting down"}))
        }
        Request::Check { path, errors_only } => {
            handle_check(path.as_deref(), *errors_only, state).await
        }
        Request::Init => handle_init(state).await,
        Request::FindSymbol { name, path_filter, src_only, include_body } => {
            handle_find_symbol(name, path_filter.as_deref(), *src_only, *include_body, state).await
        }
        Request::FindRefs { name, with_symbol } => {
            handle_find_refs(name, *with_symbol, state).await
        }
        Request::FindImpl { name } => {
            handle_find_impl(name, state).await
        }
        Request::ListSymbols { path, depth } => handle_list_symbols(path, *depth, state).await,
        Request::ReadFile {
            path,
            from,
            to,
            max_lines,
        } => handle_read_file(path, *from, *to, *max_lines, state),
        Request::ReadSymbol {
            name,
            signature_only,
            max_lines,
            path_filter,
            has_body,
        } => handle_read_symbol(name, *signature_only, *max_lines, path_filter.as_deref(), *has_body, state).await,
        Request::EditReplace { symbol, code } => {
            handle_edit(symbol, code, EditKind::Replace, state).await
        }
        Request::EditInsertAfter { symbol, code } => {
            handle_edit(symbol, code, EditKind::InsertAfter, state).await
        }
        Request::EditInsertBefore { symbol, code } => {
            handle_edit(symbol, code, EditKind::InsertBefore, state).await
        }
        Request::Hover { name } => handle_hover_cmd(name, state).await,
        Request::Format { path } => handle_format_cmd(path, state).await,
        Request::Rename { name, new_name } => handle_rename_cmd(name, new_name, state).await,
        Request::Fix { path } => handle_fix_cmd(path.as_deref(), state).await,
        Request::ServerStatus => handle_server_status(state),
        Request::ServerRestart { language } => handle_server_restart(language, state).await,
    }
}

fn handle_server_status(state: &DaemonState) -> Response {
    let statuses = state.pool.status();
    let items: Vec<serde_json::Value> = statuses
        .iter()
        .map(|s| {
            json!({
                "language": s.language,
                "server": s.server_name,
                "status": s.status,
                "uptime_secs": s.uptime_secs,
                "open_files": s.open_files,
                "attached_folders": s.attached_folders,
                "total_folders": s.total_folders,
            })
        })
        .collect();
    Response::ok(json!({"servers": items, "count": items.len()}))
}

async fn handle_server_restart(language: &str, state: &DaemonState) -> Response {
    let Some(lang) = crate::config::parse_language(language) else {
        return Response::err("unknown_language", format!("unknown language: {language}"));
    };

    match state.pool.restart_language(lang).await {
        Ok(()) => {
            let server_name = crate::lsp::registry::get_entry(lang).map_or_else(|| "unknown".to_string(), |e| e.binary_name.to_string());
            Response::ok(json!({
                "restarted": language,
                "server_name": server_name,
            }))
        }
        Err(e) => Response::err("restart_failed", e.to_string()),
    }
}

/// Find symbol across all languages — runs each language concurrently.
async fn handle_find_symbol(name: &str, path_filter: Option<&str>, src_only: bool, include_body: bool, state: &DaemonState) -> Response {
    // Cache-first: try the index before touching LSP
    if let Ok(guard) = state.index.lock() {
        if let Some(ref store) = *guard {
            if let Some(results) = cache_query::cached_find_symbol(store, name, &state.project_root, state.dirty_files_ref())
            {
                debug!("find_symbol cache hit for '{name}' ({} results)", results.len());
                let filtered = filter_by_path(results, path_filter);
                let filtered = if src_only { filter_src_only(filtered, &state.project_root) } else { filtered };
                let filtered = if include_body { populate_bodies(filtered, &state.project_root) } else { filtered };
                return Response::ok(serde_json::to_value(filtered).unwrap_or_default());
            }
        }
    }

    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    // Query each language concurrently — per-language locks allow true parallelism
    let handles: Vec<_> = languages
        .iter()
        .map(|lang| {
            let pool = Arc::clone(&state.pool);
            let name = name.to_string();
            let project_root = state.project_root.clone();
            let lang = *lang;
            tokio::spawn(async move {
                let mut guard = pool.get_or_start(lang).await?;
                pool.attach_all_workspaces_with_guard(lang, &mut guard).await?;
                let session = guard.session_mut()
                    .ok_or_else(|| anyhow::anyhow!("no session for {lang}"))?;
                find::find_symbol(&name, &mut session.client, &project_root).await
            })
        })
        .collect();

    let mut all_results = Vec::new();
    for (lang, handle) in languages.iter().zip(handles) {
        match handle.await {
            Ok(Ok(results)) => all_results.push(results),
            Ok(Err(e)) => debug!("find_symbol failed for {lang}: {e}"),
            Err(e) => debug!("find_symbol task panicked for {lang}: {e}"),
        }
    }

    let merged = router::merge_symbol_results(all_results);
    if merged.is_empty() {
        let readiness = state.pool.readiness();
        let uptime = state.start_time.elapsed().as_secs();
        if uptime < INDEXING_GRACE_PERIOD_SECS && readiness.total > 0 && !readiness.is_all_ready() {
            return Response::err_with_advice(
                "indexing",
                format!(
                    "LSP servers still indexing ({}/{} ready, daemon uptime {}s)",
                    readiness.ready, readiness.total, uptime
                ),
                "Wait a few seconds and try again, or run `krait daemon status`",
            );
        }

        // LSP came up empty — fall back to text search for const exports and other
        // identifiers that workspace/symbol doesn't index.
        let name_owned = name.to_string();
        let root = state.project_root.clone();
        let fallback = tokio::task::spawn_blocking(move || {
            find::text_search_find_symbol(&name_owned, &root)
        })
        .await
        .unwrap_or_default();

        let filtered = filter_by_path(fallback, path_filter);
        let filtered = if src_only { filter_src_only(filtered, &state.project_root) } else { filtered };
        if filtered.is_empty() {
            return Response::ok(json!([]));
        }
        let filtered = if include_body { populate_bodies(filtered, &state.project_root) } else { filtered };
        Response::ok(serde_json::to_value(filtered).unwrap_or_default())
    } else {
        let filtered = filter_by_path(merged, path_filter);
        let filtered = if src_only { filter_src_only(filtered, &state.project_root) } else { filtered };
        let filtered = if include_body { populate_bodies(filtered, &state.project_root) } else { filtered };
        Response::ok(serde_json::to_value(filtered).unwrap_or_default())
    }
}

/// Find refs across all languages — runs each language concurrently.
async fn handle_find_refs(name: &str, with_symbol: bool, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    let handles: Vec<_> = languages
        .iter()
        .map(|lang| {
            let pool = Arc::clone(&state.pool);
            let name = name.to_string();
            let project_root = state.project_root.clone();
            let lang = *lang;
            tokio::spawn(async move {
                let mut guard = pool.get_or_start(lang).await?;
                pool.attach_all_workspaces_with_guard(lang, &mut guard).await?;
                let session = guard.session_mut()
                    .ok_or_else(|| anyhow::anyhow!("no session for {lang}"))?;
                find::find_refs(&name, &mut session.client, &mut session.file_tracker, &project_root).await
            })
        })
        .collect();

    let mut all_results = Vec::new();
    for (lang, handle) in languages.iter().zip(handles) {
        match handle.await {
            Ok(Ok(results)) => all_results.push(results),
            Ok(Err(e)) => {
                let msg = e.to_string();
                if !msg.contains("not found") {
                    debug!("find_refs failed for {lang}: {e}");
                }
            }
            Err(e) => debug!("find_refs task panicked for {lang}: {e}"),
        }
    }

    let merged = router::merge_reference_results(all_results);

    // Check whether LSP returned any real call sites (non-definition references).
    // Interface methods like `computeActions` often yield only type-level definitions
    // (interface declaration, implementing method signatures) with zero runtime callers.
    // In that case the text-search fallback is always worth running to find call sites
    // that the LSP missed due to workspace boundaries or interface indirection.
    let has_call_sites = merged.iter().any(|r| !r.is_definition);

    if merged.is_empty() || !has_call_sites {
        let readiness = state.pool.readiness();
        let uptime = state.start_time.elapsed().as_secs();
        if merged.is_empty() && uptime < INDEXING_GRACE_PERIOD_SECS && readiness.total > 0 && !readiness.is_all_ready() {
            return Response::err_with_advice(
                "indexing",
                format!(
                    "LSP servers still indexing ({}/{} ready, daemon uptime {}s)",
                    readiness.ready, readiness.total, uptime
                ),
                "Wait a few seconds and try again, or run `krait daemon status`",
            );
        }

        // LSP found no call sites — run text search to find runtime callers.
        // Merge with any LSP definition results already found.
        let name_owned = name.to_string();
        let root = state.project_root.clone();
        let text_results = tokio::task::spawn_blocking(move || {
            find::text_search_find_refs(&name_owned, &root)
        })
        .await
        .unwrap_or_default();

        // Merge: keep LSP definition results + text search results, dedup by path:line
        let mut combined = merged;
        for r in text_results {
            let already_present = combined.iter().any(|m| m.path == r.path && m.line == r.line);
            if !already_present {
                combined.push(r);
            }
        }

        if combined.is_empty() {
            Response::err_with_advice(
                "symbol_not_found",
                format!("symbol '{name}' not found"),
                "Check the symbol name and try again",
            )
        } else {
            if with_symbol {
                enrich_with_symbols(&mut combined, state).await;
            }
            Response::ok(serde_json::to_value(combined).unwrap_or_default())
        }
    } else {
        let mut refs = merged;
        if with_symbol {
            enrich_with_symbols(&mut refs, state).await;
        }
        Response::ok(serde_json::to_value(refs).unwrap_or_default())
    }
}

/// Filter a `Vec<SymbolMatch>` to entries whose path contains `substr`.
/// Returns the original vec unchanged when `substr` is `None`.
fn filter_by_path(symbols: Vec<find::SymbolMatch>, substr: Option<&str>) -> Vec<find::SymbolMatch> {
    match substr {
        None => symbols,
        Some(s) => symbols.into_iter().filter(|m| m.path.contains(s)).collect(),
    }
}

/// Populate the `body` field for each symbol by reading its source file.
///
/// Pure file I/O — no LSP needed. Works for all symbol kinds including
/// `const` variables that `documentSymbol` doesn't surface.
fn populate_bodies(symbols: Vec<find::SymbolMatch>, project_root: &Path) -> Vec<find::SymbolMatch> {
    symbols
        .into_iter()
        .map(|mut m| {
            let abs = project_root.join(&m.path);
            m.body = find::extract_symbol_body(&abs, m.line);
            m
        })
        .collect()
}

/// Filter out gitignored paths from symbol results.
///
/// Loads `.gitignore` (and `.git/info/exclude`) from `project_root` using the same
/// rules as ripgrep / git. Paths that would be ignored by git are excluded.
/// Falls back to keeping all results if no ignore file is found.
fn filter_src_only(symbols: Vec<find::SymbolMatch>, project_root: &Path) -> Vec<find::SymbolMatch> {
    use ignore::gitignore::GitignoreBuilder;

    let mut builder = GitignoreBuilder::new(project_root);

    // Root .gitignore
    let root_ignore = project_root.join(".gitignore");
    if root_ignore.exists() {
        let _ = builder.add(&root_ignore);
    }

    // .git/info/exclude (per-repo excludes that aren't committed)
    let git_exclude = project_root.join(".git/info/exclude");
    if git_exclude.exists() {
        let _ = builder.add(&git_exclude);
    }

    let Ok(gitignore) = builder.build() else {
        return symbols;
    };

    symbols
        .into_iter()
        .filter(|m| {
            let abs = project_root.join(&m.path);
            !gitignore
                .matched_path_or_any_parents(&abs, /* is_dir */ false)
                .is_ignore()
        })
        .collect()
}

/// Enrich a list of references with the containing symbol (function/class) for each site.
///
/// For every unique file in `refs`, fetches the document symbol tree via LSP
/// and resolves which symbol's range contains each reference line.
async fn enrich_with_symbols(refs: &mut [find::ReferenceMatch], state: &DaemonState) {
    use std::collections::HashMap;
    use crate::commands::list;

    // Collect unique non-definition file paths
    let files: std::collections::HashSet<String> = refs
        .iter()
        .filter(|r| !r.is_definition)
        .map(|r| r.path.clone())
        .collect();

    let mut file_symbols: HashMap<String, Vec<list::SymbolEntry>> = HashMap::new();

    for file_path in files {
        let abs_path = state.project_root.join(&file_path);
        let Ok(mut guard) = state.pool.route_for_file(&abs_path).await else { continue };
        let Some(session) = guard.session_mut() else { continue };
        if let Ok(symbols) = list::list_symbols(
            &abs_path,
            3,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
        )
        .await
        {
            file_symbols.insert(file_path, symbols);
        }
    }

    for r in refs.iter_mut() {
        if r.is_definition {
            continue;
        }
        if let Some(symbols) = file_symbols.get(&r.path) {
            r.containing_symbol = find::find_innermost_containing(symbols, r.line);
        }
    }
}

/// Route `list_symbols` to the correct LSP based on file extension.
async fn handle_list_symbols(path: &Path, depth: u8, state: &DaemonState) -> Response {
    // Directory mode: walk source files and aggregate symbols per file
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        state.project_root.join(path)
    };
    if abs_path.is_dir() {
        return handle_list_symbols_dir(&abs_path, depth, state).await;
    }

    // Cache-first: check index for this file
    let rel_path = path.to_string_lossy();
    if let Ok(guard) = state.index.lock() {
        if let Some(ref store) = *guard {
            if let Some(symbols) =
                cache_query::cached_list_symbols(store, &rel_path, depth, &state.project_root, state.dirty_files_ref())
            {
                debug!("list_symbols cache hit for '{rel_path}' ({} symbols)", symbols.len());
                return Response::ok(serde_json::to_value(symbols).unwrap_or_default());
            }
        }
    }

    let lang = language_for_file(path)
        .or_else(|| state.languages.first().copied());

    let Some(lang) = lang else {
        return Response::err("no_language", "No language detected in project");
    };

    // Route for file: acquires only the relevant language's lock
    let mut guard = match state.pool.route_for_file(path).await {
        Ok(g) => g,
        Err(e) => {
            // Fallback: try get_or_start with detected language
            match state.pool.get_or_start(lang).await {
                Ok(g) => g,
                Err(e2) => return Response::err("lsp_not_available", format!("{e}; {e2}")),
            }
        }
    };

    let Some(session) = guard.session_mut() else {
        return Response::err("lsp_not_available", "No active session");
    };

    match list::list_symbols(
        path,
        depth,
        &mut session.client,
        &mut session.file_tracker,
        &state.project_root,
    )
    .await
    {
        Ok(symbols) => Response::ok(serde_json::to_value(symbols).unwrap_or_default()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Response::err_with_advice("file_not_found", &msg, "Check the file path exists")
            } else {
                debug!("list_symbols error: {e:?}");
                Response::err("list_symbols_failed", &msg)
            }
        }
    }
}

/// Walk a directory and return symbols per file, depth=1 (top-level only).
async fn handle_list_symbols_dir(dir: &Path, depth: u8, state: &DaemonState) -> Response {
    use ignore::WalkBuilder;

    let valid_exts = language_extensions(&state.languages);

    // Collect source files in the directory (respects .gitignore)
    let mut files: Vec<PathBuf> = WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|p| {
            p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| valid_exts.iter().any(|v| v == ext))
        })
        .collect();
    files.sort();

    if files.is_empty() {
        return Response::ok(json!({"dir": true, "files": []}));
    }

    let effective_depth = if depth == 0 { 1 } else { depth };
    let mut file_entries = Vec::new();

    for file_path in &files {
        // Route to the appropriate LSP session for this file
        let Ok(mut guard) = state.pool.route_for_file(file_path).await else {
            continue;
        };
        let Some(session) = guard.session_mut() else {
            continue;
        };

        let rel = file_path
            .strip_prefix(&state.project_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .into_owned();

        match list::list_symbols(
            file_path,
            effective_depth,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
        )
        .await
        {
            Ok(symbols) if !symbols.is_empty() => {
                file_entries.push(json!({
                    "file": rel,
                    "symbols": serde_json::to_value(symbols).unwrap_or_default(),
                }));
            }
            _ => {} // skip files with no symbols or LSP errors
        }
    }

    Response::ok(json!({"dir": true, "files": file_entries}))
}

/// Handle `init` — build the symbol index for the project.
#[allow(clippy::similar_names)]
async fn handle_init(state: &DaemonState) -> Response {
    let krait_dir = state.project_root.join(".krait");
    if let Err(e) = std::fs::create_dir_all(&krait_dir) {
        return Response::err("init_failed", format!("failed to create .krait/: {e}"));
    }

    let db_path = krait_dir.join("index.db");
    let extensions = language_extensions(&state.languages);
    let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let init_start = Instant::now();

    // Phase 1: plan which files need indexing
    let (files_to_index, files_cached) = match plan_index_phase(&db_path, state, &exts) {
        Ok(result) => result,
        Err(resp) => return resp,
    };
    let files_total = files_to_index.len() + files_cached;
    info!(
        "init: phase 1 (plan) {:?} — {} to index, {} cached",
        init_start.elapsed(),
        files_to_index.len(),
        files_cached
    );

    // Store discovered workspaces in SQLite
    if let Err(e) = store_workspaces(&db_path, state) {
        debug!("init: failed to store workspaces: {e}");
    }

    // Phase 2: query LSP for symbols — one server per language
    let phase2_start = Instant::now();
    let all_results = match collect_symbols_parallel(state, files_to_index).await {
        Ok(results) => results,
        Err(resp) => return resp,
    };
    let phase2_dur = phase2_start.elapsed();
    info!("init: phase 2 (LSP) {phase2_dur:?} — {} results", all_results.len());

    // Phase 3: write results to DB
    let phase3_start = Instant::now();
    let (files_indexed, symbols_total) = match commit_index_phase(&db_path, &all_results) {
        Ok(result) => result,
        Err(resp) => return resp,
    };
    let phase3_dur = phase3_start.elapsed();
    info!("init: phase 3 (commit) {phase3_dur:?}");

    // When all files were cached, symbols_total counts only newly indexed symbols (0).
    // Report the DB total instead so the user sees the real symbol count.
    #[allow(clippy::cast_possible_truncation)]
    let symbols_total = if symbols_total == 0 && files_cached > 0 {
        IndexStore::open(&db_path)
            .and_then(|s| s.count_all_symbols())
            .unwrap_or(0) as usize
    } else {
        symbols_total
    };

    let total_ms = init_start.elapsed().as_millis();
    let batch_size = builder::detect_batch_size();
    info!(
        "init: total {:?} — {files_indexed} files, {symbols_total} symbols",
        init_start.elapsed()
    );

    // Optimize the DB after a full index build
    if let Ok(store) = crate::index::store::IndexStore::open(&db_path) {
        if let Err(e) = store.optimize() {
            debug!("init: optimize failed (non-fatal): {e}");
        }
    }

    // Refresh the cache-first index so subsequent queries use the new data
    state.refresh_index();
    // Clear the dirty set — everything is freshly indexed
    state.dirty_files.clear();

    let num_workers = builder::detect_worker_count();
    Response::ok(json!({
        "message": "index built",
        "db_path": db_path.display().to_string(),
        "files_total": files_total,
        "files_indexed": files_indexed,
        "files_cached": files_cached,
        "symbols_total": symbols_total,
        "elapsed_ms": total_ms,
        "batch_size": batch_size,
        "workers": num_workers,
        "phase2_lsp_ms": phase2_dur.as_millis(),
        "phase3_commit_ms": phase3_dur.as_millis(),
    }))
}

/// Phase 1: plan which files need indexing (sync, no LSP).
fn plan_index_phase(
    db_path: &Path,
    state: &DaemonState,
    exts: &[&str],
) -> Result<(Vec<builder::FileEntry>, usize), Response> {
    let store = IndexStore::open(db_path).or_else(|e| {
        // Corrupted DB — delete and retry with a fresh one
        info!("index DB corrupted in plan phase ({e}), deleting");
        let _ = std::fs::remove_file(db_path);
        IndexStore::open(db_path)
    }).map_err(|e| Response::err("init_failed", format!("failed to open index DB: {e}")))?;
    builder::plan_index(&store, &state.project_root, exts)
        .map_err(|e| Response::err("init_failed", format!("failed to plan index: {e}")))
}

/// Phase 2: collect symbols using parallel temporary workers.
///
/// Spawns N temporary LSP servers per language (not from the pool) and splits
/// files across them for true parallel indexing. Works for all languages.
async fn collect_symbols_parallel(
    state: &DaemonState,
    files_to_index: Vec<builder::FileEntry>,
) -> Result<Vec<(String, String, Vec<crate::index::store::CachedSymbol>)>, Response> {
    let num_workers = builder::detect_worker_count();
    info!("init: workers={num_workers} (based on system resources)");

    // Group files by language — use detected languages, falling back to pool
    let languages = {
        let detected = state.languages.clone();
        if detected.is_empty() {
            state.pool.unique_languages()
        } else {
            detected
        }
    };
    if languages.is_empty() {
        return Err(Response::err(
            "no_language",
            "No language detected in project",
        ));
    }

    let mut handles = Vec::new();
    for lang in &languages {
        let lang_files: Vec<builder::FileEntry> = files_to_index
            .iter()
            .filter(|f| language_for_file(&f.abs_path) == Some(*lang))
            .map(|f| builder::FileEntry {
                abs_path: f.abs_path.clone(),
                rel_path: f.rel_path.clone(),
                hash: f.hash.clone(),
            })
            .collect();

        if lang_files.is_empty() {
            continue;
        }

        info!("init: {lang} — {} files, {num_workers} workers", lang_files.len());
        let lang = *lang;
        let root = state.project_root.clone();
        handles.push(tokio::spawn(async move {
            builder::collect_symbols_parallel(lang_files, lang, &root, num_workers).await
        }));
    }

    let mut all_results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(results)) => all_results.extend(results),
            Ok(Err(e)) => debug!("init: worker error: {e}"),
            Err(e) => debug!("init: task panicked: {e}"),
        }
    }

    Ok(all_results)
}

/// Phase 3: commit results to the index store (sync, no LSP).
fn commit_index_phase(
    db_path: &Path,
    results: &[(String, String, Vec<crate::index::store::CachedSymbol>)],
) -> Result<(usize, usize), Response> {
    let store = IndexStore::open(db_path)
        .map_err(|e| Response::err("init_failed", format!("failed to open index DB: {e}")))?;
    let symbols = builder::commit_index(&store, results)
        .map_err(|e| Response::err("init_failed", format!("failed to write index: {e}")))?;
    Ok((results.len(), symbols))
}

/// Store all discovered workspaces in the index database.
fn store_workspaces(db_path: &Path, state: &DaemonState) -> anyhow::Result<()> {
    let store = IndexStore::open(db_path)?;
    store.clear_workspaces()?;
    for (lang, root) in &state.package_roots {
        let rel = root
            .strip_prefix(&state.project_root)
            .unwrap_or(root)
            .to_string_lossy();
        let path = if rel.is_empty() { "." } else { &rel };
        store.upsert_workspace(path, lang.name())?;
    }
    info!(
        "init: stored {} workspaces in index",
        state.package_roots.len()
    );
    Ok(())
}

/// Get file extensions to watch/index for the detected languages.
/// TypeScript and JavaScript always include each other's extensions since they interoperate.
fn language_extensions(languages: &[Language]) -> Vec<String> {
    let mut exts = Vec::new();
    for &lang in languages {
        match lang {
            Language::TypeScript | Language::JavaScript => {
                // TS/JS share file types — watch both when either is detected
                for &e in Language::TypeScript.extensions().iter().chain(Language::JavaScript.extensions()) {
                    exts.push(e.to_string());
                }
            }
            _ => {
                for &e in lang.extensions() {
                    exts.push(e.to_string());
                }
            }
        }
    }
    exts.sort();
    exts.dedup();
    exts
}

/// Handle `read file` — pure file I/O, no LSP needed.
fn handle_read_file(
    path: &Path,
    from: Option<u32>,
    to: Option<u32>,
    max_lines: Option<u32>,
    state: &DaemonState,
) -> Response {
    match read::handle_read_file(path, from, to, max_lines, &state.project_root) {
        Ok(data) => Response::ok(data),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                Response::err_with_advice("file_not_found", &msg, "Check the file path exists")
            } else if msg.contains("binary file") {
                Response::err("binary_file", &msg)
            } else {
                Response::err("read_failed", &msg)
            }
        }
    }
}

/// Handle `read symbol` — find symbol via LSP then extract lines.
#[allow(clippy::too_many_lines)]
async fn handle_read_symbol(
    name: &str,
    signature_only: bool,
    max_lines: Option<u32>,
    path_filter: Option<&str>,
    has_body: bool,
    state: &DaemonState,
) -> Response {
    // When path_filter is set, use cached_find_symbol to get ALL candidates, filter
    // by path, then read the matching symbol body from disk via the LSP session.
    // This avoids the "cache only returns first match" problem.
    if let Some(filter) = path_filter {
        // Try to get all candidates from the index
        let filtered = if let Ok(guard) = state.index.lock() {
            if let Some(ref store) = *guard {
                cache_query::cached_find_symbol(store, name, &state.project_root, state.dirty_files_ref())
                    .map(|all| filter_by_path(all, Some(filter)))
            } else {
                None
            }
        } else {
            None
        };

        if let Some(candidates) = filtered {
            if !candidates.is_empty() {
                // Have filtered candidates from cache — use LSP session to read body
                let languages = state.pool.unique_languages();
                for lang in &languages {
                    let Ok(mut guard) = state.pool.get_or_start(*lang).await else { continue };
                    let Some(session) = guard.session_mut() else { continue };
                    match read::handle_read_symbol(
                        name, &candidates, signature_only, max_lines, has_body,
                        &mut session.client, &mut session.file_tracker, &state.project_root,
                    ).await {
                        Ok(data) => return Response::ok(data),
                        Err(e) => return Response::err("read_symbol_failed", e.to_string()),
                    }
                }
            }
        }

        // Cache miss with path_filter: fall through to LSP path below (path_filter
        // will be applied to candidates returned by workspace/symbol)
    }

    // Cache-first: read symbol body from index + disk (no path filter, no has_body).
    if path_filter.is_none() && !has_body {
        if let Ok(guard) = state.index.lock() {
            if let Some(ref store) = *guard {
                if let Some(data) = cache_query::cached_read_symbol(
                    store,
                    name,
                    signature_only,
                    max_lines,
                    &state.project_root,
                    state.dirty_files_ref(),
                ) {
                    debug!("read_symbol cache hit for '{name}'");
                    return Response::ok(data);
                }
            }
        }
    }

    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    // For dotted names like "Config.new", search for the parent symbol
    let search_name = name.split('.').next().unwrap_or(name);

    // Search each language server sequentially (first hit wins)
    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => { debug!("skipping {lang}: {e}"); continue; }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("skipping {lang}: {e}");
            continue;
        }

        let Some(session) = guard.session_mut() else { continue };

        // Find symbol candidates in this language, optionally filtered by path
        let raw_candidates =
            match find::find_symbol(search_name, &mut session.client, &state.project_root).await {
                Ok(s) if !s.is_empty() => s,
                _ => continue,
            };
        let candidates = filter_by_path(raw_candidates, path_filter);
        if candidates.is_empty() {
            continue;
        }

        // Try to resolve the symbol body from the candidates
        match read::handle_read_symbol(
            name,
            &candidates,
            signature_only,
            max_lines,
            has_body,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
        )
        .await
        {
            Ok(data) => return Response::ok(data),
            Err(e) => {
                let msg = e.to_string();
                debug!("read_symbol failed for {name}: {msg}");
                return Response::err("read_symbol_failed", &msg);
            }
        }
    }

    // Symbol not found in any language
    let readiness = state.pool.readiness();
    let uptime = state.start_time.elapsed().as_secs();
    if uptime < INDEXING_GRACE_PERIOD_SECS && readiness.total > 0 && !readiness.is_all_ready() {
        Response::err_with_advice(
            "indexing",
            format!(
                "LSP servers still indexing ({}/{} ready, daemon uptime {}s)",
                readiness.ready, readiness.total, uptime
            ),
            "Wait a few seconds and try again, or run `krait daemon status`",
        )
    } else {
        Response::err_with_advice(
            "symbol_not_found",
            format!("symbol '{name}' not found"),
            "Check the symbol name and try again",
        )
    }
}

/// Find concrete implementations of an interface method using `textDocument/implementation`.
async fn handle_find_impl(name: &str, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    let mut all_results = Vec::new();
    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => { debug!("find_impl skipping {lang}: {e}"); continue; }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("find_impl skipping {lang}: {e}");
            continue;
        }
        let Some(session) = guard.session_mut() else { continue };
        match find::find_impl(name, &mut session.client, &mut session.file_tracker, &state.project_root).await {
            Ok(results) => all_results.extend(results),
            Err(e) => debug!("find_impl failed for {lang}: {e}"),
        }
    }

    // Deduplicate by path:line
    all_results.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    all_results.dedup_by(|a, b| a.path == b.path && a.line == b.line);

    if all_results.is_empty() {
        Response::err_with_advice(
            "impl_not_found",
            format!("no implementations found for '{name}'"),
            "Try krait find refs <name> to see where it's called, or krait find symbol <name> for definitions",
        )
    } else {
        Response::ok(serde_json::to_value(all_results).unwrap_or_default())
    }
}

// ── Semantic editing ──────────────────────────────────────────────────────────

enum EditKind {
    Replace,
    InsertAfter,
    InsertBefore,
}

/// Shared dispatcher for all three edit commands.
async fn handle_edit(name: &str, code: &str, kind: EditKind, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => { debug!("skipping {lang}: {e}"); continue; }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("skipping {lang} attach: {e}");
            continue;
        }

        let Some(session) = guard.session_mut() else { continue };

        let result = match kind {
            EditKind::Replace => edit::handle_edit_replace(
                name,
                code,
                &mut session.client,
                &mut session.file_tracker,
                &state.project_root,
                &state.dirty_files,
            )
            .await,
            EditKind::InsertAfter => edit::handle_edit_insert_after(
                name,
                code,
                &mut session.client,
                &mut session.file_tracker,
                &state.project_root,
                &state.dirty_files,
            )
            .await,
            EditKind::InsertBefore => edit::handle_edit_insert_before(
                name,
                code,
                &mut session.client,
                &mut session.file_tracker,
                &state.project_root,
                &state.dirty_files,
            )
            .await,
        };

        match result {
            Ok(data) => return Response::ok(data),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not found") {
                    continue; // try next language
                }
                return Response::err("edit_failed", msg);
            }
        }
    }

    let readiness = state.pool.readiness();
    let uptime = state.start_time.elapsed().as_secs();
    if uptime < INDEXING_GRACE_PERIOD_SECS && readiness.total > 0 && !readiness.is_all_ready() {
        Response::err_with_advice(
            "indexing",
            format!(
                "LSP servers still indexing ({}/{} ready)",
                readiness.ready, readiness.total
            ),
            "Wait a few seconds and try again",
        )
    } else {
        Response::err_with_advice(
            "symbol_not_found",
            format!("symbol '{name}' not found"),
            "Check the symbol name and try again",
        )
    }
}

/// Handle `krait check [path]`.
///
/// If `path` is given, actively reopens the file so the LSP analyses its current on-disk
/// content (important after `krait edit`), then waits for `publishDiagnostics` to arrive.
/// If no path, returns diagnostics accumulated passively from prior queries.
async fn handle_check(path: Option<&std::path::Path>, errors_only: bool, state: &DaemonState) -> Response {
    const DIAG_WAIT_MS: u64 = 3_000;

    if let Some(file_path) = path {
        let Some(lang) = language_for_file(file_path) else {
            // Unknown language — return any passively-stored diagnostics
            let data = check::handle_check(Some(file_path), &state.diagnostic_store, &state.project_root, errors_only);
            return Response::ok(data);
        };

        // Ensure the LSP is running for this language
        let Ok(mut guard) = state.pool.get_or_start(lang).await else {
            // Can't reach LSP — return passive diagnostics
            let data = check::handle_check(path, &state.diagnostic_store, &state.project_root, errors_only);
            return Response::ok(data);
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(lang, &mut guard).await {
            return Response::err("lsp_error", e.to_string());
        }

        let Some(session) = guard.session_mut() else {
            let data = check::handle_check(path, &state.diagnostic_store, &state.project_root, errors_only);
            return Response::ok(data);
        };

        // Force-reopen so the LSP sees the latest on-disk content (post-edit)
        let _ = session
            .file_tracker
            .reopen(file_path, session.client.transport_mut())
            .await;

        // Send a documentSymbol probe — during wait, publishDiagnostics notifications are captured
        let abs = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            state.project_root.join(file_path)
        };
        if let Ok(uri) = path_to_uri(&abs) {
            let params = serde_json::json!({
                "textDocument": { "uri": uri.as_str() }
            });
            if let Ok(req_id) = session.client.transport_mut()
                .send_request("textDocument/documentSymbol", params)
                .await
            {
                let timeout = std::time::Duration::from_millis(DIAG_WAIT_MS);
                let _ = tokio::time::timeout(
                    timeout,
                    session.client.wait_for_response_public(req_id),
                )
                .await;
            }
        }
    }

    let data = check::handle_check(path, &state.diagnostic_store, &state.project_root, errors_only);
    Response::ok(data)
}

// ── Phase 10 handlers ─────────────────────────────────────────────────────────

/// Handle `krait hover <name>` — type info at symbol definition.
async fn handle_hover_cmd(name: &str, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => {
                debug!("skipping {lang}: {e}");
                continue;
            }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("skipping {lang} attach: {e}");
            continue;
        }
        let Some(session) = guard.session_mut() else { continue };
        match hover::handle_hover(
            name,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
        )
        .await
        {
            Ok(data) => return Response::ok(data),
            Err(e) => {
                if !e.to_string().contains("not found") {
                    return Response::err("hover_failed", e.to_string());
                }
            }
        }
    }

    Response::err_with_advice(
        "symbol_not_found",
        format!("symbol '{name}' not found"),
        "Check the symbol name and try again",
    )
}

/// Handle `krait format <path>` — run LSP formatter on a file.
async fn handle_format_cmd(path: &std::path::Path, state: &DaemonState) -> Response {
    let lang = language_for_file(path).or_else(|| state.languages.first().copied());
    let Some(lang) = lang else {
        return Response::err("no_language", "Cannot detect language for file");
    };

    let mut guard = match state.pool.get_or_start(lang).await {
        Ok(g) => g,
        Err(e) => return Response::err("lsp_not_available", e.to_string()),
    };
    if let Err(e) = state.pool.attach_all_workspaces_with_guard(lang, &mut guard).await {
        debug!("format attach warning: {e}");
    }
    let Some(session) = guard.session_mut() else {
        return Response::err("lsp_not_available", "No active session");
    };

    match fmt::handle_format(
        path,
        &mut session.client,
        &mut session.file_tracker,
        &state.project_root,
    )
    .await
    {
        Ok(data) => Response::ok(data),
        Err(e) => Response::err("format_failed", e.to_string()),
    }
}

/// Handle `krait rename <symbol> <new_name>` — cross-file rename.
async fn handle_rename_cmd(name: &str, new_name: &str, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => {
                debug!("skipping {lang}: {e}");
                continue;
            }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("skipping {lang} attach: {e}");
            continue;
        }
        let Some(session) = guard.session_mut() else { continue };
        match rename::handle_rename(
            name,
            new_name,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
        )
        .await
        {
            Ok(data) => return Response::ok(data),
            Err(e) => {
                if !e.to_string().contains("not found") {
                    return Response::err("rename_failed", e.to_string());
                }
            }
        }
    }

    Response::err_with_advice(
        "symbol_not_found",
        format!("symbol '{name}' not found"),
        "Check the symbol name and try again",
    )
}

/// Handle `krait fix [path]` — apply LSP quick-fix code actions.
async fn handle_fix_cmd(path: Option<&std::path::Path>, state: &DaemonState) -> Response {
    let languages = state.pool.unique_languages();
    if languages.is_empty() {
        return Response::err("no_language", "No language detected in project");
    }

    // Pick the first available language session — diagnostics are file-agnostic
    for lang in &languages {
        let mut guard = match state.pool.get_or_start(*lang).await {
            Ok(g) => g,
            Err(e) => {
                debug!("skipping {lang}: {e}");
                continue;
            }
        };
        if let Err(e) = state.pool.attach_all_workspaces_with_guard(*lang, &mut guard).await {
            debug!("skipping {lang} attach: {e}");
            continue;
        }
        let Some(session) = guard.session_mut() else { continue };
        match fix::handle_fix(
            path,
            &mut session.client,
            &mut session.file_tracker,
            &state.project_root,
            &state.diagnostic_store,
        )
        .await
        {
            Ok(data) => return Response::ok(data),
            Err(e) => return Response::err("fix_failed", e.to_string()),
        }
    }

    Response::err("lsp_not_available", "No LSP server available for fix")
}

fn build_status_response(state: &DaemonState) -> Response {
    let uptime = state.start_time.elapsed().as_secs();
    let language_names: Vec<&str> = state.languages.iter().map(|l| l.name()).collect();

    let (lsp_info, workspace_count) = {
        let readiness = state.pool.readiness();
        let statuses = state.pool.status();
        let workspace_count = state.pool.workspace_roots().len();

        let lsp = if readiness.total == 0 {
            // No workspaces configured
            match state.languages.first() {
                Some(lang) => {
                    let entry = get_entry(*lang);
                    let available = entry
                        .as_ref()
                        .is_some_and(|e| find_server(e).is_some());
                    let server = entry.as_ref().map_or("unknown", |e| e.binary_name);
                    let advice = entry.as_ref().map(|e| e.install_advice);
                    json!({
                        "language": lang.name(),
                        "status": if available { "available" } else { "not_installed" },
                        "server": server,
                        "advice": advice,
                    })
                }
                None => json!(null),
            }
        } else {
            let status_label = if readiness.is_all_ready() {
                "ready"
            } else {
                "pending"
            };
            json!({
                "status": status_label,
                "sessions": readiness.total,
                "ready": readiness.ready,
                "progress": format!("{}/{}", readiness.ready, readiness.total),
                "servers": statuses.iter().map(|s| json!({
                    "language": s.language,
                    "server": s.server_name,
                    "status": s.status,
                    "uptime_secs": s.uptime_secs,
                    "open_files": s.open_files,
                    "attached_folders": s.attached_folders,
                    "total_folders": s.total_folders,
                })).collect::<Vec<_>>(),
            })
        };
        (lsp, workspace_count)
    };

    // Try to read workspace counts from the index DB
    let (discovered, attached) = {
        let db_path = state.project_root.join(".krait/index.db");
        IndexStore::open(&db_path)
            .ok()
            .and_then(|store| store.workspace_counts().ok())
            .unwrap_or((workspace_count, 0))
    };

    let config_label = state.config_source.label();

    Response::ok(json!({
        "daemon": {
            "pid": std::process::id(),
            "uptime_secs": uptime,
        },
        "config": config_label,
        "lsp": lsp_info,
        "project": {
            "root": state.project_root.display().to_string(),
            "languages": language_names,
            "workspaces": workspace_count,
            "workspaces_discovered": discovered,
            "workspaces_attached": attached,
        },
        "index": {
            "dirty_files": state.dirty_files.len(),
            "watcher_active": state.watcher_active,
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    use super::*;

    async fn send_request(socket_path: &Path, req: &Request) -> Response {
        let mut stream = UnixStream::connect(socket_path).await.unwrap();

        let json = serde_json::to_vec(req).unwrap();
        let len = u32::try_from(json.len()).unwrap();
        stream.write_u32(len).await.unwrap();
        stream.write_all(&json).await.unwrap();
        stream.flush().await.unwrap();

        let resp_len = stream.read_u32().await.unwrap();
        let mut resp_buf = vec![0u8; resp_len as usize];
        stream.read_exact(&mut resp_buf).await.unwrap();
        serde_json::from_slice(&resp_buf).unwrap()
    }

    fn start_server(
        sock: &Path,
        project_root: &Path,
        timeout_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        let sock = sock.to_path_buf();
        let root = project_root.to_path_buf();
        tokio::spawn(async move {
            run_server(&sock, Duration::from_secs(timeout_secs), &root)
                .await
                .unwrap();
        })
    }

    #[tokio::test]
    async fn daemon_starts_and_listens() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 2);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(sock.exists());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn daemon_handles_status_request() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Status).await;
        assert!(resp.success);
        assert!(resp.data.is_some());

        let data = resp.data.unwrap();
        assert!(data["daemon"]["pid"].is_u64());
        assert!(data["daemon"]["uptime_secs"].is_u64());
        assert!(data["project"]["root"].is_string());
        assert!(data["project"]["languages"].is_array());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn status_shows_lsp_null_when_no_languages() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Status).await;
        let data = resp.data.unwrap();
        assert!(data["lsp"].is_null());
        assert!(data["project"]["languages"].as_array().unwrap().is_empty());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn status_shows_language_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

        let sock = dir.path().join("test.sock");
        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Status).await;
        let data = resp.data.unwrap();
        assert_eq!(data["project"]["languages"][0], "rust");

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn status_shows_workspace_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();

        let sock = dir.path().join("test.sock");
        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Status).await;
        let data = resp.data.unwrap();
        assert!(data["project"]["workspaces"].is_u64());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn dispatch_unknown_returns_not_implemented() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Init).await;
        assert!(!resp.success);
        assert!(resp.error.is_some());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn daemon_cleans_up_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::DaemonStop).await;
        assert!(resp.success);

        let _ = handle.await;
    }

    #[tokio::test]
    async fn daemon_idle_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let sock_clone = sock.clone();
        let root = dir.path().to_path_buf();
        let handle = tokio::spawn(async move {
            run_server(&sock_clone, Duration::from_millis(200), &root)
                .await
                .unwrap();
        });

        let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(result.is_ok(), "daemon should have exited on idle timeout");
    }

    #[tokio::test]
    async fn daemon_handles_concurrent_connections() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut tasks = Vec::new();
        for _ in 0..3 {
            let s = sock.clone();
            tasks.push(tokio::spawn(
                async move { send_request(&s, &Request::Status).await },
            ));
        }

        for task in tasks {
            let resp = task.await.unwrap();
            assert!(resp.success);
        }

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn dispatch_status_returns_success() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        let resp = send_request(&sock, &Request::Status).await;
        assert!(resp.success);
        assert!(resp.data.is_some());

        // Verify key fields are present
        let data = resp.data.unwrap();
        assert!(data["daemon"]["pid"].as_u64().is_some());
        assert!(data["daemon"]["uptime_secs"].as_u64().is_some());
        assert!(data["project"]["root"].is_string());
        assert!(data["index"]["watcher_active"].is_boolean());

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }

    #[tokio::test]
    async fn handle_connection_rejects_oversized_frame() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handle = start_server(&sock, dir.path(), 5);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send an oversized frame (20 MB > 10 MB limit)
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let oversized_len: u32 = 20 * 1024 * 1024;
        stream.write_u32(oversized_len).await.unwrap();
        stream.flush().await.unwrap();

        // The connection should be closed by the server
        let result = stream.read_u32().await;
        assert!(result.is_err(), "server should close connection on oversized frame");

        // Daemon should still be running after rejecting the bad frame
        let resp = send_request(&sock, &Request::Status).await;
        assert!(resp.success, "daemon should still accept valid requests");

        send_request(&sock, &Request::DaemonStop).await;
        let _ = handle.await;
    }
}
