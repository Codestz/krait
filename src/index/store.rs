#![allow(clippy::missing_errors_doc)]
use std::path::Path;

use rusqlite::{params, Connection};

/// A discovered workspace entry from the index.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    /// Path relative to project root (e.g., "packages/api").
    pub path: String,
    /// Language name (e.g., "typescript").
    pub language: String,
    /// Status: "discovered" or "attached".
    pub status: String,
    /// Last time this workspace was used (unix timestamp), for LRU eviction.
    pub last_used_at: Option<i64>,
}

/// A cached symbol entry from the index.
#[derive(Debug, Clone)]
pub struct CachedSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub range_start_line: u32,
    pub range_start_col: u32,
    pub range_end_line: u32,
    pub range_end_col: u32,
    pub parent_name: Option<String>,
}

/// SQLite-backed symbol index store.
pub struct IndexStore {
    conn: Connection,
}

impl IndexStore {
    /// Open (or create) the index database at the given path.
    ///
    /// # Errors
    /// Returns an error if the database can't be opened or schema migration fails.
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = normal;
             PRAGMA temp_store = memory;
             PRAGMA cache_size = -32000;
             PRAGMA mmap_size = 30000000000;
             PRAGMA foreign_keys = ON;",
        )?;
        let store = Self { conn };
        store.create_tables()?;
        Ok(store)
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let store = Self { conn };
        store.create_tables()?;
        Ok(store)
    }

    fn create_tables(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                blake3_hash TEXT NOT NULL,
                indexed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                path TEXT NOT NULL,
                range_start_line INTEGER NOT NULL,
                range_start_col INTEGER NOT NULL,
                range_end_line INTEGER NOT NULL,
                range_end_col INTEGER NOT NULL,
                parent_name TEXT,
                FOREIGN KEY (path) REFERENCES files(path) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_path ON symbols(path);

            CREATE TABLE IF NOT EXISTS lsp_cache (
                request_hash TEXT PRIMARY KEY,
                response_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workspaces (
                path TEXT PRIMARY KEY,
                language TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'discovered',
                last_used_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS server_capabilities (
                server_name TEXT NOT NULL PRIMARY KEY,
                workspace_folders_supported INTEGER NOT NULL DEFAULT 0,
                work_done_progress INTEGER NOT NULL DEFAULT 0
            );",
        )
    }

    /// Insert or update a file entry.
    pub fn upsert_file(&self, path: &str, hash: &str) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.conn.execute(
            "INSERT INTO files (path, blake3_hash, indexed_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET blake3_hash=?2, indexed_at=?3",
            params![path, hash, now.cast_signed()],
        )?;
        Ok(())
    }

    /// Insert symbols for a file, replacing any existing ones.
    pub fn insert_symbols(&self, path: &str, symbols: &[CachedSymbol]) -> rusqlite::Result<()> {
        // Delete existing symbols for this file
        self.conn
            .execute("DELETE FROM symbols WHERE path = ?1", params![path])?;

        let mut stmt = self.conn.prepare(
            "INSERT INTO symbols (name, kind, path, range_start_line, range_start_col, range_end_line, range_end_col, parent_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;

        for sym in symbols {
            stmt.execute(params![
                sym.name,
                sym.kind,
                path,
                sym.range_start_line,
                sym.range_start_col,
                sym.range_end_line,
                sym.range_end_col,
                sym.parent_name,
            ])?;
        }
        Ok(())
    }

    /// Write all index results in a single transaction for performance.
    ///
    /// This is ~100x faster than individual inserts because it avoids
    /// per-statement autocommit overhead.
    pub fn batch_commit(
        &self,
        results: &[(String, String, Vec<CachedSymbol>)],
    ) -> rusqlite::Result<usize> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .cast_signed();

        self.conn.execute_batch("BEGIN")?;

        let upsert_result = (|| {
            let mut upsert_stmt = self.conn.prepare(
                "INSERT INTO files (path, blake3_hash, indexed_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET blake3_hash=?2, indexed_at=?3",
            )?;
            let mut delete_stmt = self.conn.prepare("DELETE FROM symbols WHERE path = ?1")?;
            let mut insert_stmt = self.conn.prepare(
                "INSERT INTO symbols (name, kind, path, range_start_line, range_start_col, range_end_line, range_end_col, parent_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;

            let mut symbols_total = 0;
            for (rel_path, hash, symbols) in results {
                upsert_stmt.execute(params![rel_path, hash, now])?;
                delete_stmt.execute(params![rel_path])?;
                for sym in symbols {
                    insert_stmt.execute(params![
                        sym.name,
                        sym.kind,
                        rel_path,
                        sym.range_start_line,
                        sym.range_start_col,
                        sym.range_end_line,
                        sym.range_end_col,
                        sym.parent_name,
                    ])?;
                }
                symbols_total += symbols.len();
            }
            Ok(symbols_total)
        })();

        match upsert_result {
            Ok(total) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(total)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Find symbols by name (exact match).
    pub fn find_symbols_by_name(&self, name: &str) -> rusqlite::Result<Vec<CachedSymbol>> {
        // Also match Go receiver-qualified names: "(*ReceiverType).MethodName"
        // The pattern `%).{name}` catches e.g. "(*knowledgeService).CreateKnowledgeFromFile".
        let qualified_pattern = format!("%).\"{name}\"");
        let go_pattern = format!("%).{name}");
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, path, range_start_line, range_start_col, range_end_line, range_end_col, parent_name
             FROM symbols WHERE name = ?1 OR name LIKE ?2 OR name LIKE ?3",
        )?;

        let rows = stmt.query_map(params![name, go_pattern, qualified_pattern], |row| {
            Ok(CachedSymbol {
                name: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                range_start_line: row.get(3)?,
                range_start_col: row.get(4)?,
                range_end_line: row.get(5)?,
                range_end_col: row.get(6)?,
                parent_name: row.get(7)?,
            })
        })?;

        rows.collect()
    }

    /// Find symbols by file path.
    pub fn find_symbols_by_path(&self, path: &str) -> rusqlite::Result<Vec<CachedSymbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, kind, path, range_start_line, range_start_col, range_end_line, range_end_col, parent_name
             FROM symbols WHERE path = ?1
             ORDER BY range_start_line",
        )?;

        let rows = stmt.query_map(params![path], |row| {
            Ok(CachedSymbol {
                name: row.get(0)?,
                kind: row.get(1)?,
                path: row.get(2)?,
                range_start_line: row.get(3)?,
                range_start_col: row.get(4)?,
                range_end_line: row.get(5)?,
                range_end_col: row.get(6)?,
                parent_name: row.get(7)?,
            })
        })?;

        rows.collect()
    }

    /// Get the stored BLAKE3 hash for a file.
    pub fn get_file_hash(&self, path: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT blake3_hash FROM files WHERE path = ?1")?;

        let mut rows = stmt.query(params![path])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Get a cached LSP response by request hash.
    pub fn cache_get(&self, request_hash: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT response_json FROM lsp_cache WHERE request_hash = ?1")?;

        let mut rows = stmt.query(params![request_hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Store a cached LSP response.
    pub fn cache_put(&self, request_hash: &str, response_json: &str) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.conn.execute(
            "INSERT OR REPLACE INTO lsp_cache (request_hash, response_json, created_at)
             VALUES (?1, ?2, ?3)",
            params![request_hash, response_json, now.cast_signed()],
        )?;
        Ok(())
    }

    // ── Workspace registry ──────────────────────────────────────────

    /// Insert or update a workspace entry.
    pub fn upsert_workspace(&self, path: &str, language: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO workspaces (path, language)
             VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET language=?2",
            params![path, language],
        )?;
        Ok(())
    }

    /// Mark a workspace as attached (LSP folder added to server).
    pub fn set_workspace_attached(&self, path: &str) -> rusqlite::Result<()> {
        let now = now_unix();
        self.conn.execute(
            "UPDATE workspaces SET status='attached', last_used_at=?2 WHERE path=?1",
            params![path, now],
        )?;
        Ok(())
    }

    /// Mark a workspace as detached (LSP folder removed from server).
    pub fn set_workspace_detached(&self, path: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE workspaces SET status='discovered' WHERE path=?1",
            params![path],
        )?;
        Ok(())
    }

    /// Update `last_used_at` for a workspace (called on every query).
    pub fn touch_workspace(&self, path: &str) -> rusqlite::Result<()> {
        let now = now_unix();
        self.conn.execute(
            "UPDATE workspaces SET last_used_at=?2 WHERE path=?1",
            params![path, now],
        )?;
        Ok(())
    }

    /// List all workspaces.
    pub fn list_workspaces(&self) -> rusqlite::Result<Vec<WorkspaceInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, language, status, last_used_at FROM workspaces ORDER BY path")?;
        let rows = stmt.query_map([], |row| {
            Ok(WorkspaceInfo {
                path: row.get(0)?,
                language: row.get(1)?,
                status: row.get(2)?,
                last_used_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Get the oldest attached workspace for a language (for LRU eviction).
    pub fn get_lru_attached(&self, language: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM workspaces
             WHERE language=?1 AND status='attached'
             ORDER BY last_used_at ASC NULLS FIRST
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![language])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Count workspaces by status.
    pub fn workspace_counts(&self) -> rusqlite::Result<(usize, usize)> {
        let total: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))?;
        let attached: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM workspaces WHERE status='attached'",
            [],
            |r| r.get(0),
        )?;
        Ok((total, attached))
    }

    /// Remove all workspaces (used before re-populating on init).
    pub fn clear_workspaces(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM workspaces", [])?;
        Ok(())
    }

    /// Run post-index optimization pragmas (call after krait init completes).
    /// Count the total number of symbols in the database.
    pub fn count_all_symbols(&self) -> rusqlite::Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| {
                row.get::<_, u64>(0)
            })
    }

    pub fn optimize(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "PRAGMA analysis_limit = 400;
             PRAGMA optimize;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
    }

    /// Get file hashes for multiple paths in a single query.
    pub fn get_file_hashes_batch(
        &self,
        paths: &[&str],
    ) -> rusqlite::Result<std::collections::HashMap<String, String>> {
        if paths.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders: Vec<String> = (1..=paths.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT path, blake3_hash FROM files WHERE path IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            paths.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (path, hash) = row?;
            map.insert(path, hash);
        }
        Ok(map)
    }

    // ── Server capabilities cache ─────────────────────────────────

    /// Persist server capability flags (survives daemon restarts).
    pub fn upsert_server_capabilities(
        &self,
        server_name: &str,
        workspace_folders_supported: bool,
        work_done_progress: bool,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO server_capabilities (server_name, workspace_folders_supported, work_done_progress)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(server_name) DO UPDATE
             SET workspace_folders_supported=?2, work_done_progress=?3",
            params![
                server_name,
                i32::from(workspace_folders_supported),
                i32::from(work_done_progress)
            ],
        )?;
        Ok(())
    }

    /// Read persisted capability flags for a server. Returns `None` if not found.
    pub fn get_server_capabilities(
        &self,
        server_name: &str,
    ) -> rusqlite::Result<Option<(bool, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_folders_supported, work_done_progress
             FROM server_capabilities WHERE server_name=?1",
        )?;
        let mut rows = stmt.query(params![server_name])?;
        match rows.next()? {
            Some(row) => {
                let wf: i32 = row.get(0)?;
                let wdp: i32 = row.get(1)?;
                Ok(Some((wf != 0, wdp != 0)))
            }
            None => Ok(None),
        }
    }

    // ── File management ───────────────────────────────────────────

    /// Delete a file and all its symbols (via CASCADE).
    pub fn delete_file(&self, path: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .cast_signed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_creates_tables() {
        let store = IndexStore::open_in_memory().unwrap();
        // Verify tables exist by querying them
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM lsp_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn upsert_file_and_retrieve() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_file("src/lib.rs", "abc123").unwrap();

        let hash = store.get_file_hash("src/lib.rs").unwrap();
        assert_eq!(hash, Some("abc123".to_string()));

        // Update hash
        store.upsert_file("src/lib.rs", "def456").unwrap();
        let hash = store.get_file_hash("src/lib.rs").unwrap();
        assert_eq!(hash, Some("def456".to_string()));
    }

    #[test]
    fn insert_and_find_symbols_by_name() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_file("src/lib.rs", "abc").unwrap();

        let symbols = vec![
            CachedSymbol {
                name: "Config".into(),
                kind: "struct".into(),
                path: "src/lib.rs".into(),
                range_start_line: 5,
                range_start_col: 0,
                range_end_line: 10,
                range_end_col: 1,
                parent_name: None,
            },
            CachedSymbol {
                name: "new".into(),
                kind: "method".into(),
                path: "src/lib.rs".into(),
                range_start_line: 6,
                range_start_col: 4,
                range_end_line: 8,
                range_end_col: 5,
                parent_name: Some("Config".into()),
            },
        ];
        store.insert_symbols("src/lib.rs", &symbols).unwrap();

        let found = store.find_symbols_by_name("Config").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, "struct");
        assert_eq!(found[0].range_start_line, 5);

        let found = store.find_symbols_by_name("new").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].parent_name, Some("Config".to_string()));
    }

    #[test]
    fn insert_and_find_symbols_by_path() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_file("src/lib.rs", "abc").unwrap();

        let symbols = vec![CachedSymbol {
            name: "greet".into(),
            kind: "function".into(),
            path: "src/lib.rs".into(),
            range_start_line: 1,
            range_start_col: 0,
            range_end_line: 3,
            range_end_col: 1,
            parent_name: None,
        }];
        store.insert_symbols("src/lib.rs", &symbols).unwrap();

        let found = store.find_symbols_by_path("src/lib.rs").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "greet");
    }

    #[test]
    fn delete_file_cascades_to_symbols() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_file("src/lib.rs", "abc").unwrap();

        let symbols = vec![CachedSymbol {
            name: "Config".into(),
            kind: "struct".into(),
            path: "src/lib.rs".into(),
            range_start_line: 1,
            range_start_col: 0,
            range_end_line: 5,
            range_end_col: 1,
            parent_name: None,
        }];
        store.insert_symbols("src/lib.rs", &symbols).unwrap();
        assert_eq!(store.find_symbols_by_name("Config").unwrap().len(), 1);

        store.delete_file("src/lib.rs").unwrap();
        assert_eq!(store.find_symbols_by_name("Config").unwrap().len(), 0);
        assert!(store.get_file_hash("src/lib.rs").unwrap().is_none());
    }

    #[test]
    fn cache_put_and_get() {
        let store = IndexStore::open_in_memory().unwrap();
        store.cache_put("hash123", r#"{"result": "ok"}"#).unwrap();

        let cached = store.cache_get("hash123").unwrap();
        assert_eq!(cached, Some(r#"{"result": "ok"}"#.to_string()));
    }

    #[test]
    fn cache_miss_returns_none() {
        let store = IndexStore::open_in_memory().unwrap();
        let cached = store.cache_get("nonexistent").unwrap();
        assert!(cached.is_none());
    }

    #[test]
    fn open_existing_db_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");

        // Create and populate
        {
            let store = IndexStore::open(&db_path).unwrap();
            store.upsert_file("src/lib.rs", "abc").unwrap();
            store
                .insert_symbols(
                    "src/lib.rs",
                    &[CachedSymbol {
                        name: "Config".into(),
                        kind: "struct".into(),
                        path: "src/lib.rs".into(),
                        range_start_line: 1,
                        range_start_col: 0,
                        range_end_line: 5,
                        range_end_col: 1,
                        parent_name: None,
                    }],
                )
                .unwrap();
        }

        // Reopen and verify
        let store = IndexStore::open(&db_path).unwrap();
        let hash = store.get_file_hash("src/lib.rs").unwrap();
        assert_eq!(hash, Some("abc".to_string()));

        let found = store.find_symbols_by_name("Config").unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn workspace_upsert_and_list() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();
        store
            .upsert_workspace("packages/web", "typescript")
            .unwrap();
        store.upsert_workspace(".", "go").unwrap();

        let workspaces = store.list_workspaces().unwrap();
        assert_eq!(workspaces.len(), 3);
        assert_eq!(workspaces[0].path, ".");
        assert_eq!(workspaces[0].status, "discovered");
        assert_eq!(workspaces[1].path, "packages/api");
        assert_eq!(workspaces[2].path, "packages/web");
    }

    #[test]
    fn workspace_status_transitions() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();

        let ws = &store.list_workspaces().unwrap()[0];
        assert_eq!(ws.status, "discovered");
        assert!(ws.last_used_at.is_none());

        store.set_workspace_attached("packages/api").unwrap();
        let ws = &store.list_workspaces().unwrap()[0];
        assert_eq!(ws.status, "attached");
        assert!(ws.last_used_at.is_some());

        store.set_workspace_detached("packages/api").unwrap();
        let ws = &store.list_workspaces().unwrap()[0];
        assert_eq!(ws.status, "discovered");
    }

    #[test]
    fn workspace_touch_updates_timestamp() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();
        store.set_workspace_attached("packages/api").unwrap();

        let t1 = store.list_workspaces().unwrap()[0].last_used_at.unwrap();
        // Touch again (same second, but verifies no error)
        store.touch_workspace("packages/api").unwrap();
        let t2 = store.list_workspaces().unwrap()[0].last_used_at.unwrap();
        assert!(t2 >= t1);
    }

    #[test]
    fn workspace_counts() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();
        store
            .upsert_workspace("packages/web", "typescript")
            .unwrap();
        store.upsert_workspace(".", "go").unwrap();

        let (total, attached) = store.workspace_counts().unwrap();
        assert_eq!(total, 3);
        assert_eq!(attached, 0);

        store.set_workspace_attached("packages/api").unwrap();
        let (total, attached) = store.workspace_counts().unwrap();
        assert_eq!(total, 3);
        assert_eq!(attached, 1);
    }

    #[test]
    fn workspace_lru_returns_oldest() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();
        store
            .upsert_workspace("packages/web", "typescript")
            .unwrap();
        store.upsert_workspace(".", "go").unwrap();

        // Nothing attached → no LRU
        assert!(store.get_lru_attached("typescript").unwrap().is_none());

        // Attach both — api first, then web
        store.set_workspace_attached("packages/api").unwrap();
        store.set_workspace_attached("packages/web").unwrap();
        // Touch web so api is older
        store.touch_workspace("packages/web").unwrap();

        let lru = store.get_lru_attached("typescript").unwrap();
        assert_eq!(lru, Some("packages/api".to_string()));

        // Go workspaces should be independent
        assert!(store.get_lru_attached("go").unwrap().is_none());
    }

    #[test]
    fn workspace_clear() {
        let store = IndexStore::open_in_memory().unwrap();
        store
            .upsert_workspace("packages/api", "typescript")
            .unwrap();
        store
            .upsert_workspace("packages/web", "typescript")
            .unwrap();

        store.clear_workspaces().unwrap();
        let workspaces = store.list_workspaces().unwrap();
        assert!(workspaces.is_empty());
    }

    #[test]
    fn workspace_upsert_updates_language() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_workspace("frontend", "javascript").unwrap();
        store.upsert_workspace("frontend", "typescript").unwrap();

        let ws = &store.list_workspaces().unwrap()[0];
        assert_eq!(ws.language, "typescript");
    }

    #[test]
    fn get_file_hashes_batch_empty() {
        let store = IndexStore::open_in_memory().unwrap();
        let result = store.get_file_hashes_batch(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn get_file_hashes_batch_returns_stored_hashes() {
        let store = IndexStore::open_in_memory().unwrap();
        store.upsert_file("src/a.rs", "hash_a").unwrap();
        store.upsert_file("src/b.rs", "hash_b").unwrap();
        store.upsert_file("src/c.rs", "hash_c").unwrap();

        let result = store
            .get_file_hashes_batch(&["src/a.rs", "src/b.rs", "src/missing.rs"])
            .unwrap();

        assert_eq!(result.get("src/a.rs").map(String::as_str), Some("hash_a"));
        assert_eq!(result.get("src/b.rs").map(String::as_str), Some("hash_b"));
        assert!(!result.contains_key("src/missing.rs"));
    }

    #[test]
    fn server_capabilities_roundtrip() {
        let store = IndexStore::open_in_memory().unwrap();

        // Not found initially
        let caps = store.get_server_capabilities("vtsls").unwrap();
        assert!(caps.is_none());

        // Upsert and retrieve
        store
            .upsert_server_capabilities("vtsls", true, false)
            .unwrap();
        let caps = store.get_server_capabilities("vtsls").unwrap().unwrap();
        assert_eq!(caps, (true, false));

        // Update
        store
            .upsert_server_capabilities("vtsls", true, true)
            .unwrap();
        let caps = store.get_server_capabilities("vtsls").unwrap().unwrap();
        assert_eq!(caps, (true, true));
    }
}
