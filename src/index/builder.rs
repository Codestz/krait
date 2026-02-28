use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use anyhow::Context;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::hasher::hash_files_parallel;
use super::store::{CachedSymbol, IndexStore};
use crate::commands::find::symbol_kind_name;
use crate::detect::Language;
use crate::lsp::client::{path_to_uri, LspClient};
use crate::lsp::files::FileTracker;
use crate::lsp::install;

/// Statistics from an index build.
#[derive(Debug, Default)]
pub struct IndexStats {
    pub files_total: usize,
    pub files_indexed: usize,
    pub files_cached: usize,
    pub symbols_total: usize,
}

/// A file entry to be indexed: absolute path, relative path, and BLAKE3 hash.
pub struct FileEntry {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub hash: String,
}

/// Determine which files need (re)indexing by comparing BLAKE3 hashes.
///
/// Returns `(files_to_index, cached_count)`.
///
/// # Errors
/// Returns an error if walking the source tree fails.
pub fn plan_index(
    store: &IndexStore,
    project_root: &Path,
    extensions: &[&str],
) -> anyhow::Result<(Vec<FileEntry>, usize)> {
    let source_files = walk_source_files(project_root, extensions)?;
    info!("index: found {} source files", source_files.len());

    // Hash all files in parallel (rayon)
    let hashes = hash_files_parallel(&source_files);

    // Build rel_path → abs_path + hash map
    let path_hashes: Vec<(String, PathBuf, String)> = hashes
        .into_iter()
        .map(|(abs_path, hash)| {
            let rel_path = abs_path
                .strip_prefix(project_root)
                .unwrap_or(&abs_path)
                .to_string_lossy()
                .to_string();
            (rel_path, abs_path, hash)
        })
        .collect();

    // Batch SELECT all stored hashes in one query
    let rel_paths: Vec<&str> = path_hashes.iter().map(|(r, _, _)| r.as_str()).collect();
    let stored = store.get_file_hashes_batch(&rel_paths).unwrap_or_default();

    let mut to_index = Vec::new();
    let mut cached = 0usize;

    for (rel_path, abs_path, hash) in path_hashes {
        if stored.get(&rel_path).is_some_and(|h| *h == hash) {
            cached += 1;
        } else {
            to_index.push(FileEntry {
                abs_path,
                rel_path,
                hash,
            });
        }
    }

    Ok((to_index, cached))
}

/// Detect optimal batch size for pipelined LSP requests based on system resources.
#[must_use]
pub fn detect_batch_size() -> usize {
    let cpus = thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(4);
    // I/O-bound work: use 4x CPU cores, clamped to [8, 64]
    (cpus * 4).clamp(8, 64)
}

/// Detect the number of parallel init workers based on system resources.
#[must_use]
pub fn detect_worker_count() -> usize {
    let cpus = thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(4);
    // Each LSP process is single-threaded, so more workers = more parallelism.
    // Aggressive: use 2/3 of available cores, cap at 10.
    let workers = (cpus * 2) / 3;
    workers.clamp(1, 10)
}

/// Index files using N parallel LSP workers for a single language.
///
/// Spawns temporary LSP server processes, splits files across them,
/// indexes in parallel, then shuts them all down. Works for all languages.
///
/// # Errors
/// Returns an error if no workers can be started.
pub async fn collect_symbols_parallel(
    files: Vec<FileEntry>,
    lang: Language,
    workspace_root: &Path,
    num_workers: usize,
) -> anyhow::Result<Vec<(String, String, Vec<CachedSymbol>)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let num_workers = num_workers.min(files.len()).max(1);
    let batch_size = detect_batch_size();

    if num_workers <= 1 {
        return collect_with_single_worker(files, lang, workspace_root, batch_size).await;
    }

    info!(
        "init: spawning {num_workers} parallel workers for {lang} ({} files)",
        files.len()
    );

    // Boot N temporary LSP servers in parallel
    let boot_start = std::time::Instant::now();
    let (binary_path, entry) = install::ensure_server(lang).await?;
    let mut boot_handles = Vec::new();
    for i in 0..num_workers {
        let bp = binary_path.clone();
        let args: Vec<String> = entry.args.iter().map(|s| (*s).to_string()).collect();
        let wr = workspace_root.to_path_buf();
        boot_handles.push(tokio::spawn(async move {
            let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            (i, boot_temp_worker(&bp, &args_refs, lang, &wr).await)
        }));
    }

    let mut workers = Vec::new();
    for handle in boot_handles {
        if let Ok((i, result)) = handle.await {
            match result {
                Ok((client, tracker)) => workers.push((client, tracker)),
                Err(e) => warn!("init: worker {i} failed to start: {e}"),
            }
        }
    }

    if workers.is_empty() {
        anyhow::bail!("no init workers could be started for {lang}");
    }

    let actual_workers = workers.len();
    info!(
        "init: {actual_workers} workers booted in {:?}",
        boot_start.elapsed()
    );

    // Split files round-robin across workers
    let files = Arc::new(files);
    let mut handles = Vec::new();

    for (worker_idx, (mut client, mut tracker)) in workers.into_iter().enumerate() {
        let files_ref = Arc::clone(&files);
        let worker_indices: Vec<usize> = (worker_idx..files_ref.len())
            .step_by(actual_workers)
            .collect();

        handles.push(tokio::spawn(async move {
            let worker_files: Vec<&FileEntry> =
                worker_indices.iter().map(|&i| &files_ref[i]).collect();
            info!(
                "init: worker {worker_idx} processing {} files",
                worker_files.len()
            );
            let results =
                collect_symbols(&worker_files, &mut client, &mut tracker, batch_size).await;

            // Shut down temp worker
            let _ = tracker.close_all(client.transport_mut()).await;
            if let Err(e) = client.shutdown().await {
                debug!("init: worker {worker_idx} shutdown error: {e}");
            }

            results
        }));
    }

    // Collect all results
    let mut all_results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(results) => all_results.extend(results),
            Err(e) => debug!("init: worker task panicked: {e}"),
        }
    }

    Ok(all_results)
}

/// Fallback: single worker (same as before).
async fn collect_with_single_worker(
    files: Vec<FileEntry>,
    lang: Language,
    workspace_root: &Path,
    batch_size: usize,
) -> anyhow::Result<Vec<(String, String, Vec<CachedSymbol>)>> {
    let (binary_path, entry) = install::ensure_server(lang).await?;
    let (mut client, mut tracker) =
        boot_temp_worker(&binary_path, entry.args, lang, workspace_root).await?;

    let file_refs: Vec<&FileEntry> = files.iter().collect();
    let results = collect_symbols(&file_refs, &mut client, &mut tracker, batch_size).await;

    let _ = tracker.close_all(client.transport_mut()).await;
    let _ = client.shutdown().await;

    Ok(results)
}

/// Boot a temporary LSP server for indexing (not part of the pool).
async fn boot_temp_worker(
    binary_path: &Path,
    args: &[&str],
    lang: Language,
    workspace_root: &Path,
) -> anyhow::Result<(LspClient, FileTracker)> {
    let mut client = LspClient::start_with_binary(binary_path, args, lang, workspace_root)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    client
        .initialize(workspace_root)
        .await
        .context("LSP initialize failed")?;
    let tracker = FileTracker::new(lang);
    Ok((client, tracker))
}

/// Pre-read file content from disk (for parallel pre-fetching).
struct PreReadFile<'a> {
    entry: &'a FileEntry,
    content: String,
    uri: String,
}

/// Pre-read a batch of files from disk using a blocking thread pool.
///
/// Returns files with their content already loaded, ready for `didOpen`.
async fn prefetch_files<'a>(files: &[&'a FileEntry]) -> Vec<PreReadFile<'a>> {
    let paths: Vec<(usize, PathBuf)> = files
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.abs_path.clone()))
        .collect();

    let read_results = tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .filter_map(|(i, path)| {
                let canonical = std::fs::canonicalize(&path).ok()?;
                let content = std::fs::read_to_string(&canonical).ok()?;
                let uri = path_to_uri(&canonical).ok()?.to_string();
                Some((i, content, uri))
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    read_results
        .into_iter()
        .map(|(i, content, uri)| PreReadFile {
            entry: files[i],
            content,
            uri,
        })
        .collect()
}

/// Query LSP for document symbols using pipelined batches with parallel disk I/O.
///
/// For each batch: pre-reads files from disk in parallel, opens them in the LSP,
/// fires all `documentSymbol` requests, collects all responses, then closes files.
/// The next batch's disk I/O overlaps with the current batch's LSP processing.
///
/// Returns `(rel_path, hash, symbols)` for each successfully indexed file.
/// This function is `Send` — it does NOT touch `IndexStore`.
pub async fn collect_symbols(
    files: &[&FileEntry],
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    batch_size: usize,
) -> Vec<(String, String, Vec<CachedSymbol>)> {
    let mut results = Vec::new();
    let total = files.len();
    let chunks: Vec<&[&FileEntry]> = files.chunks(batch_size).collect();

    // Pre-read first batch from disk
    let mut prefetched = if chunks.is_empty() {
        Vec::new()
    } else {
        prefetch_files(chunks[0]).await
    };

    for (batch_idx, batch) in chunks.iter().enumerate() {
        let batch_start = batch_idx * batch_size;
        debug!(
            "index: batch {}-{}/{total} ({} files)",
            batch_start + 1,
            (batch_start + batch.len()).min(total),
            batch.len()
        );

        // Kick off prefetch for NEXT batch while we process this one
        let next_prefetch = if batch_idx + 1 < chunks.len() {
            let next_batch = chunks[batch_idx + 1];
            let paths: Vec<(usize, PathBuf)> = next_batch
                .iter()
                .enumerate()
                .map(|(i, e)| (i, e.abs_path.clone()))
                .collect();
            Some(tokio::task::spawn_blocking(move || {
                paths
                    .into_iter()
                    .filter_map(|(i, path)| {
                        let canonical = std::fs::canonicalize(&path).ok()?;
                        let content = std::fs::read_to_string(&canonical).ok()?;
                        let uri = path_to_uri(&canonical).ok()?.to_string();
                        Some((i, content, uri))
                    })
                    .collect::<Vec<_>>()
            }))
        } else {
            None
        };

        // Process batch: open → query → collect → close
        process_batch(&prefetched, batch, client, file_tracker, &mut results).await;

        // Collect next batch's prefetch results
        if let Some(handle) = next_prefetch {
            let next_batch = chunks[batch_idx + 1];
            prefetched = handle
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(i, content, uri)| PreReadFile {
                    entry: next_batch[i],
                    content,
                    uri,
                })
                .collect();
        }
    }

    info!(
        "index: collected symbols from {}/{total} files (batch_size={batch_size})",
        results.len()
    );
    results
}

/// Process one batch: open pre-read files, send requests, collect responses, close files.
async fn process_batch(
    prefetched: &[PreReadFile<'_>],
    batch: &[&FileEntry],
    client: &mut LspClient,
    file_tracker: &mut FileTracker,
    results: &mut Vec<(String, String, Vec<CachedSymbol>)>,
) {
    // Phase 1: Open pre-read files and send all documentSymbol requests
    let mut pending: Vec<(&FileEntry, i64)> = Vec::new();
    for file in prefetched {
        if let Err(e) = file_tracker
            .open_with_content(
                &file.entry.abs_path,
                &file.uri,
                &file.content,
                client.transport_mut(),
            )
            .await
        {
            debug!("index: failed to open {}: {e}", file.entry.rel_path);
            continue;
        }

        let params = json!({ "textDocument": { "uri": file.uri } });
        match client
            .transport_mut()
            .send_request("textDocument/documentSymbol", params)
            .await
        {
            Ok(id) => pending.push((file.entry, id)),
            Err(e) => debug!(
                "index: failed to send request for {}: {e}",
                file.entry.rel_path
            ),
        }
    }

    // Phase 2: Collect all responses
    for (entry, request_id) in &pending {
        match client.wait_for_response_public(*request_id).await {
            Ok(response) => {
                let symbols = flatten_document_symbols(&response, None);
                results.push((entry.rel_path.clone(), entry.hash.clone(), symbols));
            }
            Err(e) => {
                debug!("index: failed to index {}: {e}", entry.rel_path);
            }
        }
    }

    // Phase 3: Close batch files to keep LSP memory bounded
    for entry in batch {
        if let Err(e) = file_tracker
            .close(&entry.abs_path, client.transport_mut())
            .await
        {
            debug!("index: failed to close {}: {e}", entry.rel_path);
        }
    }
}

/// Write collected symbols to the index store in a single transaction.
///
/// # Errors
/// Returns an error if upserting files or inserting symbols into the store fails.
pub fn commit_index(
    store: &IndexStore,
    results: &[(String, String, Vec<CachedSymbol>)],
) -> anyhow::Result<usize> {
    Ok(store.batch_commit(results)?)
}

/// Flatten a hierarchical `documentSymbol` response into a flat list.
#[allow(clippy::cast_possible_truncation)]
fn flatten_document_symbols(value: &Value, parent: Option<&str>) -> Vec<CachedSymbol> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    let mut result = Vec::new();
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let kind =
            symbol_kind_name(item.get("kind").and_then(Value::as_u64).unwrap_or(0)).to_string();

        let start_line = item
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let start_col = item
            .pointer("/range/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let end_line = item
            .pointer("/range/end/line")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let end_col = item
            .pointer("/range/end/character")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        result.push(CachedSymbol {
            name: name.clone(),
            kind,
            path: String::new(),
            range_start_line: start_line,
            range_start_col: start_col,
            range_end_line: end_line,
            range_end_col: end_col,
            parent_name: parent.map(String::from),
        });

        if let Some(children) = item.get("children") {
            result.extend(flatten_document_symbols(children, Some(&name)));
        }
    }
    result
}

/// Walk project source files, respecting .gitignore.
fn walk_source_files(project_root: &Path, extensions: &[&str]) -> anyhow::Result<Vec<PathBuf>> {
    let mut builder = ignore::WalkBuilder::new(project_root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true);

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(&ext) {
                files.push(path.to_path_buf());
            }
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn flatten_empty_response() {
        let result = flatten_document_symbols(&json!(null), None);
        assert!(result.is_empty());
    }

    #[test]
    fn flatten_nested_symbols() {
        let response = json!([
            {
                "name": "Config",
                "kind": 5,
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 20, "character": 1 }
                },
                "children": [
                    {
                        "name": "new",
                        "kind": 6,
                        "range": {
                            "start": { "line": 5, "character": 2 },
                            "end": { "line": 10, "character": 3 }
                        }
                    }
                ]
            },
            {
                "name": "greet",
                "kind": 12,
                "range": {
                    "start": { "line": 22, "character": 0 },
                    "end": { "line": 25, "character": 1 }
                }
            }
        ]);

        let symbols = flatten_document_symbols(&response, None);
        assert_eq!(symbols.len(), 3);
        assert_eq!(symbols[0].name, "Config");
        assert!(symbols[0].parent_name.is_none());
        assert_eq!(symbols[1].name, "new");
        assert_eq!(symbols[1].parent_name, Some("Config".to_string()));
        assert_eq!(symbols[2].name, "greet");
        assert!(symbols[2].parent_name.is_none());
    }

    #[test]
    fn walk_source_files_filters_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "notes").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let files = walk_source_files(dir.path(), &["rs"]).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.extension().unwrap() == "rs"));
    }

    #[test]
    fn walk_source_files_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/output.rs"), "// generated").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let files = walk_source_files(dir.path(), &["rs"]).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("main.rs"));
    }
}
