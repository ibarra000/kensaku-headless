use crate::config::KensakuConfig;
use crate::indexer::burst;
use crate::indexer::walker::FileEntry;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// SQLite metadata store; the connection is behind a `Mutex` because it is not `Sync`.
pub struct MetadataDb {
    conn: Mutex<Connection>,
}

impl MetadataDb {
    pub fn begin_transaction(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("BEGIN", [])
            .map_err(|e| format!("Failed to begin transaction: {}", e))?;
        Ok(())
    }

    pub fn commit_transaction(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("COMMIT", [])
            .map_err(|e| format!("Failed to commit transaction: {}", e))?;
        Ok(())
    }

    /// Commits the batch so far so a killed crawl keeps its progress.
    pub fn commit_and_reopen(&self) -> Result<(), String> {
        self.commit_transaction()?;
        // PASSIVE checkpoint bounds WAL growth without blocking.
        self.checkpoint_passive();
        self.begin_transaction()
    }

    pub fn open() -> Result<Self, String> {
        let db_dir = KensakuConfig::config_dir();
        std::fs::create_dir_all(&db_dir)
            .map_err(|e| format!("Failed to create metadata directory: {}", e))?;

        Self::open_at(&db_dir.join("metadata.db"))
    }

    /// Opens (and initializes) `<dir>/metadata.db`.
    pub fn open_in_dir(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create metadata directory: {}", e))?;
        Self::open_at(&dir.join("metadata.db"))
    }

    fn open_at(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path)
            .map_err(|e| format!("Failed to open metadata database: {}", e))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA mmap_size=268435456;
             PRAGMA journal_size_limit=67108864;
             PRAGMA optimize;
             CREATE TABLE IF NOT EXISTS nodes (
               id INTEGER PRIMARY KEY,
               path TEXT NOT NULL UNIQUE,
               name TEXT NOT NULL,
               extension TEXT,
               size INTEGER NOT NULL,
               modified INTEGER NOT NULL,
               is_dir INTEGER NOT NULL,
               created_gen INTEGER NOT NULL DEFAULT 0,
               dev INTEGER NOT NULL DEFAULT 0,
               ino INTEGER NOT NULL DEFAULT 0
             );
             DROP INDEX IF EXISTS nodes_path; -- UNIQUE(path) already indexes it; a second copy doubles every write
             CREATE TABLE IF NOT EXISTS usage (
               path TEXT PRIMARY KEY,
               opens INTEGER NOT NULL DEFAULT 0,
               last_open INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS units (
               id INTEGER PRIMARY KEY,
               node_id INTEGER NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
               byte_start INTEGER NOT NULL,
               byte_end INTEGER NOT NULL,
               model_id INTEGER NOT NULL,
               vec BLOB
             );
             CREATE INDEX IF NOT EXISTS units_node ON units(node_id);
             CREATE TABLE IF NOT EXISTS meta (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             INSERT OR IGNORE INTO meta (key, value) VALUES ('model_id', '1');
             INSERT OR IGNORE INTO meta (key, value) VALUES ('embedding_dim', '256');",
        )
        .map_err(|e| format!("Failed to initialize metadata database: {}", e))?;

        // Adding an existing column fails with "duplicate column name"; safe to ignore.
        for ddl in [
            "ALTER TABLE nodes ADD COLUMN dev INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN ino INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN indexed_gen INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN content_hash TEXT",
            "ALTER TABLE nodes ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE nodes ADD COLUMN width INTEGER",
            "ALTER TABLE nodes ADD COLUMN height INTEGER",
            "ALTER TABLE nodes ADD COLUMN phash INTEGER",
            "ALTER TABLE nodes ADD COLUMN burst INTEGER",
            "ALTER TABLE nodes ADD COLUMN look INTEGER",
            "ALTER TABLE nodes ADD COLUMN text_sig BLOB",
            "ALTER TABLE nodes ADD COLUMN content_pending INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE units ADD COLUMN vec BLOB",
        ] {
            if let Err(e) = conn.execute(ddl, []) {
                let message = e.to_string();
                if !message.contains("duplicate column name") {
                    eprintln!("[Kensaku] Metadata migration warning: {}", message);
                }
            }
        }

        let _ = conn.execute("CREATE INDEX IF NOT EXISTS nodes_burst ON nodes(burst)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS nodes_phash ON nodes(phash)", []);

        // Bumping this invalidates stored hashes so `/dupes` never mixes sources.
        const IMAGE_HASH_VERSION: &str = "2";
        let _ = conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('image_hash_version', ?1)",
            [IMAGE_HASH_VERSION],
        );
        let stored_version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'image_hash_version'",
                [],
                |row| row.get(0),
            )
            .ok();
        if stored_version.as_deref() != Some(IMAGE_HASH_VERSION) {
            let _ = conn.execute("UPDATE nodes SET phash = NULL", []);
            let _ = conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('image_hash_version', ?1)",
                [IMAGE_HASH_VERSION],
            );
        }

        // Must run after the `dev`/`ino` columns exist.
        if let Err(e) = conn.execute(
            "CREATE INDEX IF NOT EXISTS nodes_dev_ino ON nodes(dev, ino)",
            [],
        ) {
            eprintln!("[Kensaku] Failed to create dev/ino index: {}", e);
        }

        if let Err(e) = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_unembedded ON nodes(indexed_gen) WHERE indexed_gen = 0",
            [],
        ) {
            eprintln!("[Kensaku] Failed to create unembedded index: {}", e);
        }

        // Covers the embedding queue's ORDER BY `modified` to avoid a temp b-tree.
        if let Err(e) = conn.execute(
            "CREATE INDEX IF NOT EXISTS nodes_unembedded_modified
             ON nodes(modified DESC, size) WHERE indexed_gen = 0",
            [],
        ) {
            eprintln!("[Kensaku] Failed to create unembedded order index: {}", e);
        }

        // Avoids a full scan per image during burst grouping.
        if let Err(e) = conn.execute(
            "CREATE INDEX IF NOT EXISTS nodes_burst_candidates
             ON nodes(is_dir, modified, width, height)",
            [],
        ) {
            eprintln!("[Kensaku] Failed to create burst candidate index: {}", e);
        }

        if let Err(e) = conn.execute(
            "CREATE TABLE IF NOT EXISTS visual_units (
                node_id INTEGER PRIMARY KEY,
                model_id INTEGER NOT NULL,
                dhash INTEGER NOT NULL,
                vec BLOB NOT NULL
            )",
            [],
        ) {
            eprintln!("[Kensaku] Failed to create visual_units table: {}", e);
        }

        let db = Self {
            conn: Mutex::new(conn),
        };

        // Best-effort: a busy or read-only DB must not block opening.
        db.compact_if_bloated();

        Ok(db)
    }

    /// VACUUMs when the free list is large (see [`should_vacuum`]); errors are logged only.
    pub fn compact_if_bloated(&self) {
        let conn = self.conn.lock();

        let Ok(page_count) = conn.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
        else {
            return;
        };
        let Ok(freelist_count) =
            conn.query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
        else {
            return;
        };
        let page_count = page_count.max(0) as u64;
        let freelist_count = freelist_count.max(0) as u64;
        if !should_vacuum(page_count, freelist_count) {
            return;
        }

        let page_size = conn
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
            .unwrap_or(4096)
            .max(1) as u64;
        let before_bytes = page_count.saturating_mul(page_size);

        if let Err(e) = conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);") {
            eprintln!(
                "[Kensaku] Metadata DB compaction skipped ({} free pages): {}",
                freelist_count, e
            );
            return;
        }

        let after_bytes = conn
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
            .map(|pages| (pages.max(0) as u64).saturating_mul(page_size))
            .unwrap_or(before_bytes);
        eprintln!(
            "[Kensaku] Metadata DB compacted: {} bytes -> {} bytes ({} free pages reclaimed)",
            before_bytes, after_bytes, freelist_count
        );
    }

    /// Non-blocking WAL checkpoint.
    pub fn checkpoint_passive(&self) {
        let conn = self.conn.lock();
        if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)") {
            eprintln!("[Kensaku] Metadata WAL checkpoint failed: {}", e);
        }
    }

    pub fn load_all(&self) -> Result<Vec<FileEntry>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, path, name, extension, size, modified, is_dir, dev, ino, content_pending FROM nodes",
            )
            .map_err(|e| format!("Failed to prepare metadata query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let path: String = row.get(1)?;
                let name: String = row.get(2)?;
                let extension: Option<String> = row.get(3)?;
                let size: i64 = row.get(4)?;
                let modified: i64 = row.get(5)?;
                let is_dir: i64 = row.get(6)?;
                let dev: i64 = row.get(7)?;
                let ino: i64 = row.get(8)?;
                let content_pending: i64 = row.get(9)?;

                let char_bloom = crate::search::fuzzy::compute_char_mask(&name);
                Ok(FileEntry {
                    id: id as u64,
                    path: PathBuf::from(path),
                    name,
                    extension: crate::indexer::walker::intern_extension(extension.as_deref()),
                    size: u64::try_from(size).unwrap_or(0),
                    modified: u64::try_from(modified).unwrap_or(0),
                    is_dir: is_dir != 0,
                    dev: u64::try_from(dev).unwrap_or(0),
                    ino: u64::try_from(ino).unwrap_or(0),
                    char_bloom,
                    content_pending: content_pending != 0,
                })
            })
            .map_err(|e| format!("Failed to read metadata rows: {}", e))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("Failed to decode metadata row: {}", e))?);
        }

        Ok(entries)
    }

    /// Inserts or updates a node and returns its row id.
    pub fn upsert(&self, entry: &FileEntry) -> Result<u64, String> {
        let conn = self.conn.lock();
        conn.query_row(
            "INSERT INTO nodes (path, name, extension, size, modified, is_dir, dev, ino, content_pending)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO UPDATE SET
               name = excluded.name,
               extension = excluded.extension,
               size = excluded.size,
               modified = excluded.modified,
               is_dir = excluded.is_dir,
               dev = excluded.dev,
               ino = excluded.ino,
               width = CASE WHEN nodes.modified != excluded.modified OR nodes.size != excluded.size THEN NULL ELSE nodes.width END,
               height = CASE WHEN nodes.modified != excluded.modified OR nodes.size != excluded.size THEN NULL ELSE nodes.height END,
               phash = CASE WHEN nodes.modified != excluded.modified OR nodes.size != excluded.size THEN NULL ELSE nodes.phash END,
               indexed_gen = CASE WHEN nodes.modified != excluded.modified OR nodes.size != excluded.size THEN 0 ELSE nodes.indexed_gen END,
               retry_count = CASE WHEN nodes.modified != excluded.modified OR nodes.size != excluded.size THEN 0 ELSE nodes.retry_count END
             RETURNING id",
            params![
                entry.path.to_string_lossy().to_string(),
                entry.name,
                entry.extension,
                entry.size as i64,
                entry.modified as i64,
                entry.is_dir as i64,
                entry.dev as i64,
                entry.ino as i64,
                entry.content_pending as i64,
            ],
            |row| row.get::<_, i64>(0),
        )
        .map(|id| id as u64)
        .map_err(|e| format!("Failed to upsert metadata entry: {}", e))
    }

    pub fn remove(&self, path: &Path) -> Result<(), String> {
        let conn = self.conn.lock();
        let path_str = path.to_string_lossy().to_string();
        let _ = conn.execute(
            "DELETE FROM units WHERE node_id IN (SELECT id FROM nodes WHERE path = ?1)",
            params![path_str],
        );
        conn.execute("DELETE FROM nodes WHERE path = ?1", params![path_str])
            .map_err(|e| format!("Failed to delete metadata entry: {}", e))?;

        Ok(())
    }

    /// Moves `old` and its descendants under `new` in one transaction, replacing rows already there.
    pub fn rename_prefix(&self, old: &Path, new: &Path) -> Result<usize, String> {
        let old_str = old.to_string_lossy().into_owned();
        let new_str = new.to_string_lossy().into_owned();
        if old_str.is_empty() || old_str == new_str {
            return Ok(0);
        }
        // Trailing separator keeps the match component-aligned.
        let old_prefix = format!("{}{}", old_str, std::path::MAIN_SEPARATOR);
        let new_prefix = format!("{}{}", new_str, std::path::MAIN_SEPARATOR);

        let conn = self.conn.lock();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to start rename transaction: {}", e))?;

        // Compare in Rust: `_` and `%` are legal in file names.
        let all_paths: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT path FROM nodes")
                .map_err(|e| format!("Failed to prepare rename query: {}", e))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| format!("Failed to read paths for rename: {}", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to decode paths for rename: {}", e))?
        };

        for path in &all_paths {
            if path == &new_str || path.starts_with(&new_prefix) {
                tx.execute("DELETE FROM nodes WHERE path = ?1", params![path])
                    .map_err(|e| format!("Failed to clear destination path: {}", e))?;
            }
        }

        let mut moved = 0usize;
        for path in &all_paths {
            let new_path = if path == &old_str {
                new_str.clone()
            } else if let Some(suffix) = path.strip_prefix(&old_prefix) {
                format!("{}{}{}", new_str, std::path::MAIN_SEPARATOR, suffix)
            } else {
                continue;
            };

            moved += tx
                .execute(
                    "UPDATE nodes SET path = ?1 WHERE path = ?2",
                    params![new_path, path],
                )
                .map_err(|e| format!("Failed to rename node path: {}", e))?;
        }

        if moved > 0
            && let Some(name) = new.file_name()
        {
            tx.execute(
                "UPDATE nodes SET name = ?1 WHERE path = ?2",
                params![name.to_string_lossy().to_string(), new_str],
            )
            .map_err(|e| format!("Failed to rename node: {}", e))?;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit rename: {}", e))?;
        Ok(moved)
    }

    pub fn clear(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        let _ = conn.execute("DELETE FROM units", []);
        let _ = conn.execute("DELETE FROM usage", []);
        conn.execute("DELETE FROM nodes", [])
            .map_err(|e| format!("Failed to clear metadata: {}", e))?;

        Ok(())
    }

    pub fn count(&self) -> Result<usize, String> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count metadata entries: {}", e))?;

        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Total bytes of stored embedding vectors inside `metadata.db`.
    pub fn vector_bytes(&self) -> Result<u64, String> {
        let conn = self.conn.lock();
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(vec)), 0) FROM units",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to sum vector bytes: {}", e))?;
        Ok(u64::try_from(total).unwrap_or(0))
    }

    pub fn count_unembedded(&self) -> Result<usize, String> {
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE is_dir = 0 AND indexed_gen = 0",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count unembedded nodes: {}", e))?;

        Ok(usize::try_from(count).unwrap_or(0))
    }

    pub fn record_open(&self, path: &Path) -> Result<(), String> {
        let conn = self.conn.lock();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO usage (path, opens, last_open) VALUES (?1, 1, ?2)
             ON CONFLICT(path) DO UPDATE SET opens = opens + 1, last_open = excluded.last_open",
            params![path.to_string_lossy().to_string(), now as i64],
        )
        .map_err(|e| format!("Failed to record file open: {}", e))?;
        Ok(())
    }

    pub fn load_usage(&self) -> Result<HashMap<PathBuf, (u32, u64)>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT path, opens, last_open FROM usage")
            .map_err(|e| format!("Failed to prepare usage query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let path: String = row.get(0)?;
                let opens: i64 = row.get(1)?;
                let last_open: i64 = row.get(2)?;
                Ok((
                    PathBuf::from(path),
                    (opens.max(0) as u32, last_open.max(0) as u64),
                ))
            })
            .map_err(|e| format!("Failed to load usage rows: {}", e))?;

        let mut usage = HashMap::new();
        for (p, u) in rows.flatten() {
            usage.insert(p, u);
        }
        Ok(usage)
    }

    pub fn insert_unit(
        &self,
        node_id: u64,
        byte_start: usize,
        byte_end: usize,
        model_id: u32,
        vector: &[i8],
    ) -> Result<u64, String> {
        let conn = self.conn.lock();
        let u8_vec: Vec<u8> = vector.iter().map(|&b| b as u8).collect();
        conn.execute(
            "INSERT INTO units (node_id, byte_start, byte_end, model_id, vec) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node_id as i64,
                byte_start as i64,
                byte_end as i64,
                model_id as i64,
                u8_vec,
            ],
        )
        .map_err(|e| format!("Failed to insert unit: {}", e))?;
        Ok(conn.last_insert_rowid() as u64)
    }

    /// Streams persisted units into `store` without a second in-memory copy.
    pub fn load_units_into(&self, store: &mut crate::embed::UnitStore) -> Result<usize, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id, node_id, byte_start, byte_end, model_id, coalesce(vec, zeroblob(256)) FROM units")
            .map_err(|e| format!("Failed to prepare units query: {}", e))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| format!("Failed to query units: {}", e))?;

        let mut count = 0usize;
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read unit row: {}", e))?
        {
            let unit = crate::embed::Unit {
                id: row.get::<_, i64>(0).map_err(|e| e.to_string())? as u64,
                node_id: row.get::<_, i64>(1).map_err(|e| e.to_string())? as u64,
                byte_start: row.get::<_, i64>(2).map_err(|e| e.to_string())? as usize,
                byte_end: row.get::<_, i64>(3).map_err(|e| e.to_string())? as usize,
                model_id: row.get::<_, i64>(4).map_err(|e| e.to_string())? as u32,
            };
            let blob: Vec<u8> = row.get(5).map_err(|e| e.to_string())?;
            let vector: Vec<i8> = blob.into_iter().map(|b| b as i8).collect();
            store.add_unit(unit, &vector);
            count += 1;
        }

        Ok(count)
    }

    pub fn load_all_units(&self) -> Result<Vec<(crate::embed::Unit, Vec<i8>)>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id, node_id, byte_start, byte_end, model_id, coalesce(vec, zeroblob(256)) FROM units")
            .map_err(|e| format!("Failed to prepare units query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let node_id: i64 = row.get(1)?;
                let byte_start: i64 = row.get(2)?;
                let byte_end: i64 = row.get(3)?;
                let model_id: i64 = row.get(4)?;
                let blob: Vec<u8> = row.get(5)?;
                let vec_i8: Vec<i8> = blob.into_iter().map(|b| b as i8).collect();
                Ok((
                    crate::embed::Unit {
                        id: id as u64,
                        node_id: node_id as u64,
                        byte_start: byte_start as usize,
                        byte_end: byte_end as usize,
                        model_id: model_id as u32,
                    },
                    vec_i8,
                ))
            })
            .map_err(|e| format!("Failed to load units: {}", e))?;

        let mut units = Vec::new();
        for u in rows.flatten() {
            units.push(u);
        }
        Ok(units)
    }

    pub fn delete_units_for_node(&self, node_id: u64) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM units WHERE node_id = ?1",
            params![node_id as i64],
        )
        .map_err(|e| format!("Failed to delete units for node: {}", e))?;
        Ok(())
    }

    /// Deletes every unit for `node_ids` in one transaction.
    pub fn delete_units_for_nodes(&self, node_ids: &[u64]) -> Result<(), String> {
        if node_ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock();
        for chunk in node_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("DELETE FROM units WHERE node_id IN ({})", placeholders);
            let mut stmt = conn
                .prepare_cached(&sql)
                .map_err(|e| format!("Failed to prepare unit delete: {}", e))?;
            let params: Vec<i64> = chunk.iter().map(|id| *id as i64).collect();
            stmt.execute(rusqlite::params_from_iter(params))
                .map_err(|e| format!("Failed to delete units for nodes: {}", e))?;
        }
        Ok(())
    }

    pub fn save_visual_unit(
        &self,
        node_id: u64,
        model_id: u32,
        dhash: u64,
        vec: &[i8],
    ) -> Result<(), String> {
        let conn = self.conn.lock();
        let bytes: Vec<u8> = vec.iter().map(|&b| b as u8).collect();
        conn.execute(
            "INSERT OR REPLACE INTO visual_units (node_id, model_id, dhash, vec) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![node_id as i64, model_id as i64, dhash as i64, bytes],
        )
        .map_err(|e| format!("Failed to save visual unit: {}", e))?;
        Ok(())
    }

    pub fn load_visual_units_into(
        &self,
        store: &mut crate::embed::visual::VisualStore,
    ) -> Result<usize, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT node_id, model_id, dhash, vec FROM visual_units")
            .map_err(|e| format!("Failed to prepare visual units query: {}", e))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| format!("Failed to query visual units: {}", e))?;

        let mut count = 0usize;
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read visual unit row: {}", e))?
        {
            let node_id = row.get::<_, i64>(0).map_err(|e| e.to_string())? as u64;
            let dhash = row.get::<_, i64>(2).map_err(|e| e.to_string())? as u64;
            let blob: Vec<u8> = row.get(3).map_err(|e| e.to_string())?;
            let vector: Vec<i8> = blob.into_iter().map(|b| b as i8).collect();
            store.add_or_update(node_id, dhash, &vector);
            count += 1;
        }

        Ok(count)
    }

    pub fn delete_visual_units_for_nodes(&self, ids: &[u64]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock();
        for chunk in ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "DELETE FROM visual_units WHERE node_id IN ({})",
                placeholders
            );
            let mut stmt = conn
                .prepare_cached(&sql)
                .map_err(|e| format!("Failed to prepare visual unit delete: {}", e))?;
            let params: Vec<i64> = chunk.iter().map(|id| *id as i64).collect();
            stmt.execute(rusqlite::params_from_iter(params))
                .map_err(|e| format!("Failed to delete visual units for nodes: {}", e))?;
        }
        Ok(())
    }

    pub fn reset_visual_index(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM visual_units", [])
            .map_err(|e| format!("Failed to reset visual units: {}", e))?;
        Ok(())
    }

    pub fn units_model_id(&self) -> Result<Option<u32>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT DISTINCT model_id FROM units LIMIT 1")
            .map_err(|e| format!("Failed to prepare units model query: {}", e))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("Failed to query units model id: {}", e))?;
        match rows
            .next()
            .map_err(|e| format!("Failed to read units model id: {}", e))?
        {
            Some(row) => {
                let model_id: i64 = row
                    .get(0)
                    .map_err(|e| format!("Failed to decode units model id: {}", e))?;
                Ok(Some(model_id as u32))
            }
            None => Ok(None),
        }
    }

    /// Drops all semantic units and re-queues files; vectors from different models are incomparable.
    pub fn reset_semantic_index(&self) -> Result<(), String> {
        let conn = self.conn.lock();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Failed to start semantic reset: {}", e))?;
        tx.execute("DELETE FROM units", [])
            .map_err(|e| format!("Failed to clear semantic units: {}", e))?;
        tx.execute("UPDATE nodes SET indexed_gen = 0 WHERE is_dir = 0", [])
            .map_err(|e| format!("Failed to re-queue nodes for embedding: {}", e))?;
        tx.commit()
            .map_err(|e| format!("Failed to commit semantic reset: {}", e))?;
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    pub fn get_nodes_for_embedding(
        &self,
        limit: usize,
    ) -> Result<Vec<(u64, PathBuf, u64, u64, Option<String>)>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, path, size, modified, content_hash FROM nodes
                 WHERE is_dir = 0 AND indexed_gen = 0
                 ORDER BY modified DESC, size ASC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare embedding query: {}", e))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                let id: i64 = row.get(0)?;
                let path: String = row.get(1)?;
                let size: i64 = row.get(2)?;
                let modified: i64 = row.get(3)?;
                let content_hash: Option<String> = row.get(4)?;
                Ok((
                    id as u64,
                    PathBuf::from(path),
                    size as u64,
                    modified as u64,
                    content_hash,
                ))
            })
            .map_err(|e| format!("Failed to query nodes for embedding: {}", e))?;

        let mut nodes = Vec::new();
        for r in rows.flatten() {
            nodes.push(r);
        }
        Ok(nodes)
    }

    pub fn update_node_indexed_gen(
        &self,
        node_id: u64,
        indexed_gen: i64,
        content_hash: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "UPDATE nodes SET indexed_gen = ?1, content_hash = coalesce(?2, content_hash) WHERE id = ?3",
            )
            .map_err(|e| format!("Failed to prepare indexed_gen update: {}", e))?;
        stmt.execute(params![indexed_gen, content_hash, node_id as i64])
            .map_err(|e| format!("Failed to update node indexed_gen: {}", e))?;
        Ok(())
    }

    pub fn get_node_id_by_path(&self, path: &Path) -> Result<Option<u64>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM nodes WHERE path = ?1")
            .map_err(|e| e.to_string())?;

        let mut rows = stmt
            .query(params![path.to_string_lossy().to_string()])
            .map_err(|e| e.to_string())?;

        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let id: i64 = row.get(0).map_err(|e| e.to_string())?;
            Ok(Some(id as u64))
        } else {
            Ok(None)
        }
    }

    /// Removes a row from its burst; if it led the burst, re-homes members to their lowest id.
    fn detach_burst(conn: &Connection, path: &str) -> Result<Vec<(PathBuf, u64)>, String> {
        let row = conn
            .query_row(
                "SELECT id, burst FROM nodes WHERE path = ?1",
                params![path],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .map_err(|e| format!("Failed to read old burst: {}", e))?;
        let Some((id, Some(burst))) = row else {
            return Ok(Vec::new());
        };
        conn.execute("UPDATE nodes SET burst = NULL WHERE id = ?1", params![id])
            .map_err(|e| format!("Failed to detach burst row: {}", e))?;
        if burst != id {
            return Ok(Vec::new());
        }
        let next: Option<i64> = conn
            .query_row(
                "SELECT MIN(id) FROM nodes WHERE burst = ?1 AND id <> ?2",
                params![burst, id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to find next burst leader: {}", e))?;
        let Some(next) = next else {
            return Ok(Vec::new());
        };
        conn.execute(
            "UPDATE nodes SET burst = ?1 WHERE burst = ?2 AND id <> ?3",
            params![next, burst, id],
        )
        .map_err(|e| format!("Failed to re-home burst members: {}", e))?;
        let mut statement = conn
            .prepare("SELECT path, burst FROM nodes WHERE burst = ?1 ORDER BY id")
            .map_err(|e| format!("Failed to read re-homed burst rows: {}", e))?;
        let rows = statement
            .query_map(params![next], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, i64>(1)? as u64,
                ))
            })
            .map_err(|e| format!("Failed to query re-homed burst rows: {}", e))?;
        Ok(rows.flatten().collect())
    }

    /// Stores image dimensions and perceptual hash (as bit-compatible `i64`).
    /// Returns re-homed `(path, burst)` rows when this image led a burst.
    pub fn set_image_meta(
        &self,
        path: &Path,
        width: u32,
        height: u32,
        phash: u64,
    ) -> Result<Vec<(PathBuf, u64)>, String> {
        let conn = self.conn.lock();
        let path = path.to_string_lossy().to_string();
        let updates = Self::detach_burst(&conn, &path)?;
        conn.execute(
            "UPDATE nodes SET width = ?1, height = ?2, phash = ?3, look = ?3,
                burst = NULL, text_sig = NULL
             WHERE path = ?4",
            params![i64::from(width), i64::from(height), phash as i64, path],
        )
        .map_err(|e| format!("Failed to store image metadata: {}", e))?;
        Ok(updates)
    }

    /// Stores only image dimensions; the hash is backfilled later so it never gates indexing.
    pub fn set_image_dimensions(&self, path: &Path, width: u32, height: u32) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET width = ?1, height = ?2 WHERE path = ?3",
            params![
                i64::from(width),
                i64::from(height),
                path.to_string_lossy().to_string(),
            ],
        )
        .map_err(|e| format!("Failed to store image dimensions: {}", e))?;
        Ok(())
    }

    /// Stores the perceptual hash; `u64::MAX` means "could not decode".
    pub fn set_image_phash(&self, path: &Path, phash: u64) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET phash = ?1, look = ?1 WHERE path = ?2",
            params![phash as i64, path.to_string_lossy().to_string()],
        )
        .map_err(|e| format!("Failed to store image hash: {}", e))?;
        Ok(())
    }

    /// Drops a stale hash so new pixels are not grouped against the old burst.
    pub fn clear_image_hash(&self, path: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
        let conn = self.conn.lock();
        let path = path.to_string_lossy().to_string();
        let updates = Self::detach_burst(&conn, &path)?;
        conn.execute(
            "UPDATE nodes SET phash = NULL, look = NULL, burst = NULL WHERE path = ?1",
            params![path],
        )
        .map_err(|e| format!("Failed to clear image hash: {}", e))?;
        Ok(updates)
    }

    /// Stores the line signature for burst grouping; returns re-homed rows when it led a burst.
    pub fn set_image_text_sig(
        &self,
        path: &Path,
        signature: &[u8],
    ) -> Result<Vec<(PathBuf, u64)>, String> {
        let conn = self.conn.lock();
        let path = path.to_string_lossy().to_string();
        let updates = Self::detach_burst(&conn, &path)?;
        conn.execute(
            "UPDATE nodes SET text_sig = ?1, burst = NULL WHERE path = ?2",
            params![signature, path],
        )
        .map_err(|e| format!("Failed to store image text signature: {}", e))?;
        Ok(updates)
    }

    pub fn image_paths_missing_burst(&self, limit: usize) -> Result<Vec<PathBuf>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT path FROM nodes
                 WHERE is_dir = 0 AND phash IS NOT NULL AND phash != -1 AND burst IS NULL
                 ORDER BY modified ASC, id ASC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare burst backfill query: {}", e))?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query burst backfill rows: {}", e))?;
        Ok(rows.flatten().map(PathBuf::from).collect())
    }

    /// Assigns an image to the lowest-id compatible burst (same dir, window and dimensions), merging bridged bursts.
    pub fn assign_burst_for_path(&self, path: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
        let conn = self.conn.lock();
        let current = conn
            .query_row(
                "SELECT id, path, width, height, modified, phash, text_sig, burst
                 FROM nodes WHERE path = ?1 AND is_dir = 0",
                params![path.to_string_lossy().to_string()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("Failed to read burst candidate '{}': {}", path.display(), e))?;
        let Some((
            current_id,
            current_path,
            width,
            height,
            modified,
            Some(raw_phash),
            current_sig,
            current_burst,
        )) = current
        else {
            return Ok(Vec::new());
        };
        if raw_phash < 0 {
            return Ok(Vec::new());
        }
        let phash = raw_phash as u64;
        let current_sig = current_sig.unwrap_or_default();
        let parent = Path::new(&current_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let low = modified.saturating_sub(burst::BURST_WINDOW_SECS as i64);
        let high = modified.saturating_add(burst::BURST_WINDOW_SECS as i64);

        let sql = "SELECT id, path, phash, text_sig, coalesce(burst, id)
             FROM nodes
             WHERE is_dir = 0 AND phash IS NOT NULL AND phash != -1
               AND path <> ?1 AND modified BETWEEN ?2 AND ?3
               AND width = ?4 AND height = ?5
             ORDER BY ABS(modified - ?6) ASC, id ASC LIMIT 128";

        // Without equal dimensions a pair cannot be verified, so the image keeps its own burst.
        let candidates = if let (Some(width), Some(height)) = (width, height) {
            let mut statement = conn
                .prepare(sql)
                .map_err(|e| format!("Failed to prepare burst candidates: {}", e))?;
            let rows = statement
                .query_map(
                    params![current_path, low, high, width, height, modified],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<Vec<u8>>>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .map_err(|e| format!("Failed to query burst candidates: {}", e))?;
            rows.flatten().collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let mut matched_ids = Vec::new();
        let mut matched_bursts = Vec::new();
        for (candidate_id, candidate_path, candidate_phash, candidate_sig, candidate_burst) in
            candidates
        {
            if candidate_phash < 0 || Path::new(&candidate_path).parent() != Some(parent.as_path())
            {
                continue;
            }
            let distance = (phash ^ candidate_phash as u64).count_ones();
            let overlap =
                burst::text_overlap(&current_sig, candidate_sig.as_deref().unwrap_or_default());
            if burst::is_same_burst(distance, overlap) {
                matched_ids.push(candidate_id);
                matched_bursts.push(candidate_burst);
            }
        }

        let new_burst = matched_bursts
            .iter()
            .copied()
            .chain(current_burst)
            .chain(std::iter::once(current_id))
            .min()
            .unwrap_or(current_id);
        matched_ids.push(current_id);
        matched_ids.sort_unstable();
        matched_ids.dedup();
        matched_bursts.extend(current_burst);
        matched_bursts.push(current_id);
        matched_bursts.push(new_burst);
        matched_bursts.sort_unstable();
        matched_bursts.dedup();

        let id_placeholders = std::iter::repeat_n("?", matched_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let burst_placeholders = std::iter::repeat_n("?", matched_bursts.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE nodes SET burst = ?1, look = COALESCE(look, phash)
             WHERE id IN ({}) OR burst IN ({})",
            id_placeholders, burst_placeholders
        );
        let mut values: Vec<i64> = vec![new_burst];
        values.extend(matched_ids);
        values.extend(matched_bursts);
        conn.execute(&sql, params_from_iter(values))
            .map_err(|e| format!("Failed to assign burst: {}", e))?;

        let mut statement = conn
            .prepare("SELECT path, burst FROM nodes WHERE burst = ?1 ORDER BY id")
            .map_err(|e| format!("Failed to prepare burst update query: {}", e))?;
        let rows = statement
            .query_map(params![new_burst], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, i64>(1)? as u64,
                ))
            })
            .map_err(|e| format!("Failed to read burst updates: {}", e))?;
        Ok(rows.flatten().collect())
    }

    /// Burst groups for the duplicates view, keyed by burst id.
    pub fn burst_duplicate_groups(&self, limit: usize) -> Result<Vec<(u64, Vec<String>)>, String> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(
                "SELECT burst, path, look FROM nodes
                 WHERE is_dir = 0 AND burst IS NOT NULL
                 ORDER BY burst, path",
            )
            .map_err(|e| format!("Failed to prepare burst duplicate query: {}", e))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|e| format!("Failed to query burst duplicates: {}", e))?;

        let mut groups: HashMap<u64, (Option<u64>, Vec<String>)> = HashMap::new();
        for (burst, path, look) in rows.flatten() {
            let group = groups.entry(burst).or_insert((None, Vec::new()));
            if group.0.is_none() {
                group.0 = look.map(|hash| hash as u64);
            }
            group.1.push(path);
        }
        let mut groups: Vec<(u64, Vec<String>)> = groups
            .into_iter()
            .filter(|(_, (_, paths))| paths.len() > 1)
            .map(|(burst, (look, paths))| (look.unwrap_or(burst), paths))
            .collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        groups.truncate(limit);
        Ok(groups)
    }

    pub fn load_bursts(&self) -> Result<Vec<(u64, u64)>, String> {
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare("SELECT id, burst FROM nodes WHERE burst IS NOT NULL")
            .map_err(|e| format!("Failed to prepare burst load query: {}", e))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|e| format!("Failed to load bursts: {}", e))?;
        Ok(rows.flatten().collect())
    }

    /// Raster image paths with dimensions but no perceptual hash yet.
    pub fn image_paths_missing_phash(&self, limit: usize) -> Result<Vec<PathBuf>, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT path FROM nodes
                 WHERE is_dir = 0 AND width IS NOT NULL AND phash IS NULL
                 ORDER BY modified DESC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare missing phash query: {}", e))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query missing image hashes: {}", e))?;

        let mut paths = Vec::new();
        for row in rows.flatten() {
            paths.push(PathBuf::from(row));
        }
        Ok(paths)
    }

    /// Marks every present raster image for a content re-read.
    pub fn mark_images_pending(&self) -> Result<usize, String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 1
             WHERE is_dir = 0
               AND lower(extension) IN ('png','jpg','jpeg','gif','webp','bmp','tiff','tif')",
            [],
        )
        .map_err(|e| format!("Failed to mark images pending: {}", e))
    }

    /// Marks files with any of `extensions` (normalized, no dot) for a content re-read.
    pub fn mark_extensions_pending(&self, extensions: &[String]) -> Result<usize, String> {
        if extensions.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let mut statement = conn
            .prepare(
                "UPDATE nodes SET content_pending = 1
                 WHERE is_dir = 0 AND lower(extension) = ?1",
            )
            .map_err(|e| format!("Failed to prepare extension re-read: {}", e))?;
        let mut marked = 0;
        for extension in extensions {
            marked += statement
                .execute([extension])
                .map_err(|e| format!("Failed to mark extension '{}' pending: {}", extension, e))?;
        }
        Ok(marked)
    }

    pub fn mark_dir_content_pending(&self, dir: &Path) -> Result<usize, String> {
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 1, retry_count = 0
             WHERE is_dir = 0 AND substr(path, 1, length(?1)) = ?1",
            params![prefix],
        )
        .map_err(|e| format!("Failed to mark '{}' pending: {}", dir.display(), e))
    }

    pub fn mark_all_content_pending(&self) -> Result<usize, String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 1, retry_count = 0 WHERE is_dir = 0",
            [],
        )
            .map_err(|e| format!("Failed to mark all content pending: {}", e))
    }

    pub fn set_content_pending(&self, path: &Path) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 1 WHERE path = ?1",
            params![path.to_string_lossy().to_string()],
        )
        .map_err(|e| format!("Failed to mark '{}' pending: {}", path.display(), e))?;
        Ok(())
    }

    /// Flags these files for re-read and re-embedding (resets `indexed_gen` too).
    pub fn mark_paths_content_pending(&self, paths: &[PathBuf]) -> Result<usize, String> {
        if paths.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "UPDATE nodes SET content_pending = 1, retry_count = 0, indexed_gen = 0
                 WHERE path = ?1",
            )
            .map_err(|e| format!("Failed to prepare pending-path update: {e}"))?;
        let mut total = 0;
        for path in paths {
            total += stmt
                .execute(params![path.to_string_lossy().to_string()])
                .map_err(|e| format!("Failed to mark '{}' pending: {}", path.display(), e))?;
        }
        Ok(total)
    }

    /// Records a pending retry; returns whether another attempt is worthwhile (caps endless re-reads).
    pub fn record_retry(&self, path: &Path, max_attempts: u32) -> Result<bool, String> {
        let path_str = path.to_string_lossy().to_string();
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET retry_count = retry_count + 1 WHERE path = ?1",
            params![&path_str],
        )
        .map_err(|e| format!("Failed to record a retry for '{}': {}", path.display(), e))?;
        let attempts: i64 = conn
            .query_row(
                "SELECT retry_count FROM nodes WHERE path = ?1",
                params![&path_str],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if attempts >= i64::from(max_attempts) {
            // Clear `content_pending` so later crawls skip it until the file changes.
            conn.execute(
                "UPDATE nodes SET content_pending = 0 WHERE path = ?1",
                params![&path_str],
            )
            .map_err(|e| format!("Failed to stop retrying '{}': {}", path.display(), e))?;
            return Ok(false);
        }
        Ok(true)
    }

    /// Marks every image and PDF for a content re-read.
    pub fn mark_ocr_targets_pending(&self) -> Result<usize, String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 1, retry_count = 0
             WHERE is_dir = 0
               AND lower(extension) IN ('png','jpg','jpeg','gif','webp','bmp','tiff','tif','pdf')",
            [],
        )
        .map_err(|e| format!("Failed to mark OCR targets pending: {}", e))
    }

    pub fn clear_content_pending(&self, path: &Path) -> Result<(), String> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE nodes SET content_pending = 0 WHERE path = ?1",
            params![path.to_string_lossy().to_string()],
        )
        .map_err(|e| {
            format!(
                "Failed to clear pending content for '{}': {}",
                path.display(),
                e
            )
        })?;
        Ok(())
    }

    /// Groups nodes with identical `content_hash`, largest first; hashless rows ignored.
    pub fn exact_duplicate_groups(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        const MAX_ROWS: usize = 200_000;
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT content_hash, path FROM nodes
                 WHERE is_dir = 0 AND content_hash IS NOT NULL AND content_hash <> ''
                 ORDER BY content_hash, path
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare duplicate query: {}", e))?;

        let rows = stmt
            .query_map(params![MAX_ROWS as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query duplicate rows: {}", e))?;

        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (hash, path) = row.map_err(|e| format!("Failed to decode duplicate row: {}", e))?;
            groups.entry(hash).or_default().push(path);
        }

        Ok(finish_groups(groups, limit))
    }

    /// Groups images within `max_distance` Hamming bits, using banded lookup (max 50k rows).
    pub fn near_duplicate_groups(
        &self,
        max_distance: u32,
        limit: usize,
    ) -> Result<Vec<(u64, Vec<String>)>, String> {
        const MAX_IMAGE_ROWS: usize = 50_000;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let max_distance = max_distance.min(64);

        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT path, phash FROM nodes
                 WHERE is_dir = 0 AND phash IS NOT NULL AND phash != -1
                 ORDER BY path
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare near-duplicate query: {}", e))?;

        let rows = stmt
            .query_map(params![MAX_IMAGE_ROWS as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to query near-duplicate rows: {}", e))?;

        // With `max_distance + 1` bands, any true neighbour shares at least one band (pigeonhole).
        let bands = usize::try_from(max_distance + 1).unwrap_or(64).clamp(1, 64);
        let mut band_index: Vec<HashMap<u64, Vec<usize>>> = vec![HashMap::new(); bands];
        let mut representatives: Vec<u64> = Vec::new();
        let mut groups: Vec<Vec<String>> = Vec::new();

        for row in rows {
            let (path, raw_hash) =
                row.map_err(|e| format!("Failed to decode near-duplicate row: {}", e))?;
            let phash = raw_hash as u64;

            let mut candidates: Vec<usize> = Vec::new();
            for (band, index) in band_index.iter().enumerate() {
                let key = band_key(phash, band, bands);
                if let Some(matches) = index.get(&key) {
                    candidates.extend(matches.iter().copied());
                }
            }
            candidates.sort_unstable();
            candidates.dedup();

            let existing = candidates
                .into_iter()
                .find(|&idx| (representatives[idx] ^ phash).count_ones() <= max_distance);

            let group_idx = match existing {
                Some(idx) => idx,
                None => {
                    let idx = representatives.len();
                    representatives.push(phash);
                    groups.push(Vec::new());
                    for (band, index) in band_index.iter_mut().enumerate() {
                        index
                            .entry(band_key(phash, band, bands))
                            .or_default()
                            .push(idx);
                    }
                    idx
                }
            };
            groups[group_idx].push(path);
        }

        let mut out: Vec<(u64, Vec<String>)> = groups
            .into_iter()
            .zip(representatives)
            .filter(|(paths, _)| paths.len() >= 2)
            .map(|(paths, hash)| (hash, paths))
            .collect();
        out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        out.truncate(limit);
        Ok(out)
    }
}

/// True when free pages exceed 1000 and 20% of the file.
fn should_vacuum(page_count: u64, freelist_count: u64) -> bool {
    page_count > 0 && freelist_count > 1000 && (freelist_count as f64 / page_count as f64) > 0.20
}

fn band_key(phash: u64, band: usize, bands: usize) -> u64 {
    let start = (band * 64 / bands) as u32;
    let end = ((band + 1) * 64 / bands) as u32;
    let width = end - start;
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    (phash >> start) & mask
}

fn finish_groups(groups: HashMap<String, Vec<String>>, limit: usize) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = groups
        .into_iter()
        .filter(|(_, paths)| paths.len() >= 2)
        .collect();
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        db: Option<MetadataDb>,
        dir: PathBuf,
    }

    impl TempDb {
        fn new(tag: &str) -> Self {
            let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "kensaku-metadata-test-{}-{}-{}",
                tag,
                std::process::id(),
                seq
            ));
            std::fs::create_dir_all(&dir).expect("create temp db dir");
            let db = MetadataDb::open_at(&dir.join("metadata.db")).expect("open temp db");
            Self { db: Some(db), dir }
        }

        fn db(&self) -> &MetadataDb {
            self.db.as_ref().expect("db open")
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            // Close before deleting the directory (Windows).
            self.db.take();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn entry(path: &Path, is_dir: bool) -> FileEntry {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        FileEntry {
            id: 0,
            path: path.to_path_buf(),
            name,
            extension: if is_dir {
                None
            } else {
                crate::indexer::walker::intern_extension(
                    path.extension().and_then(|ext| ext.to_str()),
                )
            },
            size: 1,
            modified: 1,
            is_dir,
            dev: 0,
            ino: 0,
            char_bloom: 0,
            content_pending: false,
        }
    }

    fn paths(db: &MetadataDb) -> Vec<PathBuf> {
        db.load_all()
            .expect("load nodes")
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    #[test]
    fn commit_and_reopen_makes_rows_durable_for_a_killed_crawl() {
        let temp = TempDb::new("commit-reopen");
        let db = temp.db();
        db.begin_transaction().unwrap();
        db.upsert(&entry(Path::new("/a/one.txt"), false)).unwrap();
        db.commit_and_reopen().unwrap();
        db.upsert(&entry(Path::new("/a/two.txt"), false)).unwrap();

        // A second connection sees only committed rows.
        let other = rusqlite::Connection::open_with_flags(
            temp.dir.join("metadata.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let seen: Vec<String> = other
            .prepare("SELECT path FROM nodes")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(seen.len(), 1, "only the committed row is visible: {seen:?}");
        assert!(seen[0].ends_with("one.txt"));
    }

    #[test]
    fn vacuum_requires_both_a_large_free_list_and_a_large_ratio() {
        assert!(!should_vacuum(100_000, 500));
        assert!(!should_vacuum(100_000, 1_001));
        assert!(!should_vacuum(4_000, 1_000));
        assert!(should_vacuum(10_000, 3_000));
        assert!(should_vacuum(1_001, 1_001));
        assert!(!should_vacuum(0, 0));
    }

    #[test]
    fn rename_prefix_rewrites_subtree_and_preserves_siblings() {
        let temp = TempDb::new("rename-prefix");
        let db = temp.db();
        let old_root = temp.dir.join("old");
        let new_root = temp.dir.join("new");

        db.upsert(&entry(&old_root, true)).unwrap();
        db.upsert(&entry(&old_root.join("a.txt"), false)).unwrap();
        db.upsert(&entry(&old_root.join("nested"), true)).unwrap();
        db.upsert(&entry(&old_root.join("nested").join("b.rs"), false))
            .unwrap();
        let sibling = temp.dir.join("oldish").join("c.txt");
        db.upsert(&entry(&sibling, false)).unwrap();

        let moved = db
            .rename_prefix(&old_root, &new_root)
            .expect("rename prefix");
        assert_eq!(moved, 4);

        let after = paths(db);
        assert!(after.contains(&new_root));
        assert!(after.contains(&new_root.join("a.txt")));
        assert!(after.contains(&new_root.join("nested")));
        assert!(after.contains(&new_root.join("nested").join("b.rs")));
        assert!(after.contains(&sibling));
        assert!(!after.iter().any(|path| path.starts_with(&old_root)));

        let renamed_root = db
            .load_all()
            .expect("load nodes")
            .into_iter()
            .find(|entry| entry.path == new_root)
            .expect("renamed root row");
        assert_eq!(renamed_root.name, "new");
        assert!(renamed_root.is_dir);
    }

    #[test]
    fn near_duplicate_groups_cluster_similar_phashes() {
        let temp = TempDb::new("near-duplicates");
        let db = temp.db();

        // High bit set exercises signed i64 storage.
        let base = 0xF000_0000_0000_0000u64;
        let similar_one = base ^ 0b1;
        let similar_two = base ^ 0b111;
        let unrelated = 0x0F00_0000_0000_0000u64;

        for (name, phash) in [
            ("a.png", base),
            ("b.png", similar_one),
            ("c.png", similar_two),
            ("d.png", unrelated),
        ] {
            let path = temp.dir.join(name);
            db.upsert(&entry(&path, false)).expect("insert image row");
            db.set_image_meta(&path, 64, 48, phash)
                .expect("store image metadata");
        }

        let groups = db.near_duplicate_groups(4, 10).expect("group images");
        assert_eq!(groups.len(), 1, "unrelated hash must not join: {groups:?}");
        let (representative, paths) = &groups[0];
        assert_eq!(*representative, base);
        assert_eq!(
            paths,
            &vec![
                temp.dir.join("a.png").to_string_lossy().to_string(),
                temp.dir.join("b.png").to_string_lossy().to_string(),
                temp.dir.join("c.png").to_string_lossy().to_string(),
            ]
        );

        assert!(
            db.near_duplicate_groups(0, 10)
                .expect("exact phash groups")
                .is_empty(),
            "distance 0 must not group distinct hashes"
        );
    }

    #[test]
    fn content_pending_round_trips_and_survives_upsert() {
        let temp = TempDb::new("content-pending");
        let db = temp.db();
        let image = temp.dir.join("photo.png");
        let doc = temp.dir.join("notes.txt");

        let mut pending = entry(&image, false);
        pending.content_pending = true;
        db.upsert(&pending).expect("insert pending image");
        db.upsert(&entry(&doc, false)).expect("insert document");

        let loaded = db.load_all().expect("load nodes");
        let loaded_image = loaded
            .iter()
            .find(|entry| entry.path == image)
            .expect("image row");
        assert!(loaded_image.content_pending);
        let loaded_doc = loaded
            .iter()
            .find(|entry| entry.path == doc)
            .expect("document row");
        assert!(!loaded_doc.content_pending);

        // A later upsert must not reset the flag before the re-read.
        let mut cleared = entry(&image, false);
        cleared.content_pending = false;
        db.upsert(&cleared).expect("upsert same image");
        let reloaded = db
            .load_all()
            .expect("reload nodes")
            .into_iter()
            .find(|entry| entry.path == image)
            .expect("image row");
        assert!(
            reloaded.content_pending,
            "upsert must not reset a pending flag"
        );
    }

    #[test]
    fn mark_extensions_pending_targets_only_matching_files() {
        let temp = TempDb::new("mark-extensions");
        let db = temp.db();
        let stl = temp.dir.join("part.STL");
        let obj = temp.dir.join("scene.obj");
        let stl_lower = temp.dir.join("other.stl");
        let doc = temp.dir.join("notes.txt");
        let dir = temp.dir.join("folder.stl");

        for path in [&stl, &obj, &stl_lower, &doc, &dir] {
            db.upsert(&entry(path, path == &dir)).expect("insert row");
        }

        let marked = db
            .mark_extensions_pending(&["stl".to_string(), "obj".to_string()])
            .expect("mark extensions");
        assert_eq!(marked, 3);

        let loaded = db.load_all().expect("load nodes");
        for path in [&stl, &obj, &stl_lower] {
            assert!(
                loaded
                    .iter()
                    .find(|e| e.path == *path)
                    .unwrap()
                    .content_pending,
                "{} should be pending",
                path.display()
            );
        }
        for path in [&doc, &dir] {
            assert!(
                !loaded
                    .iter()
                    .find(|e| e.path == *path)
                    .unwrap()
                    .content_pending,
                "{} should not be pending",
                path.display()
            );
        }
    }

    #[test]
    fn mark_images_pending_targets_only_image_rows() {
        let temp = TempDb::new("mark-images");
        let db = temp.db();
        let image = temp.dir.join("photo.JPG");
        let other_image = temp.dir.join("scan.tiff");
        let doc = temp.dir.join("notes.txt");
        let dir = temp.dir.join("folder.png");

        for path in [&image, &other_image, &doc, &dir] {
            db.upsert(&entry(path, path == &dir)).expect("insert row");
        }

        assert_eq!(db.mark_images_pending().expect("mark images"), 2);

        let loaded = db.load_all().expect("load nodes");
        let pending: Vec<&str> = loaded
            .iter()
            .filter(|entry| entry.content_pending)
            .map(|entry| entry.path.to_str().expect("utf-8 path"))
            .collect();
        assert_eq!(pending.len(), 2);
        assert!(
            loaded
                .iter()
                .find(|e| e.path == image)
                .unwrap()
                .content_pending
        );
        assert!(
            loaded
                .iter()
                .find(|e| e.path == other_image)
                .unwrap()
                .content_pending
        );
        assert!(
            !loaded
                .iter()
                .find(|e| e.path == doc)
                .unwrap()
                .content_pending
        );
        assert!(
            !loaded
                .iter()
                .find(|e| e.path == dir)
                .unwrap()
                .content_pending
        );

        db.clear_content_pending(&image).expect("clear pending");
        let reloaded = db.load_all().expect("reload nodes");
        assert!(
            !reloaded
                .iter()
                .find(|e| e.path == image)
                .unwrap()
                .content_pending
        );
        assert!(
            reloaded
                .iter()
                .find(|e| e.path == other_image)
                .unwrap()
                .content_pending
        );
    }

    #[test]
    fn burst_assignment_groups_scrolls_and_keeps_distinct_pages_apart() {
        let temp = TempDb::new("burst-grouping");
        let db = temp.db();

        let add = |name: &str, modified: u64, phash: u64, text: &str| {
            let path = temp.dir.join(name);
            let mut item = entry(&path, false);
            item.modified = modified;
            let id = db.upsert(&item).expect("insert image row");
            db.set_image_meta(&path, 800, 600, phash)
                .expect("store image metadata");
            db.set_image_text_sig(&path, &crate::indexer::burst::line_signature(text))
                .expect("store text signature");
            (path, id)
        };

        let (a, a_id) = add(
            "a.png",
            1_000,
            0,
            "Header\nalpha one\nbeta two\ngamma three\ndelta four\nepsilon five\nzeta six\neta seven\ntheta eight",
        );
        let updates = db.assign_burst_for_path(&a).expect("assign first");
        assert_eq!(
            updates,
            vec![(a.clone(), a_id)],
            "first image starts its own burst"
        );

        let (b, b_id) = add(
            "b.png",
            1_010,
            0x00FF_FFFF,
            "gamma three\ndelta four\nepsilon five\nzeta six\neta seven\ntheta eight\niota nine\nkappa ten",
        );
        let updates = db.assign_burst_for_path(&b).expect("assign scroll");
        assert_eq!(
            updates,
            vec![(a.clone(), a_id), (b.clone(), a_id)],
            "scrolled page joins the lower id"
        );
        assert_ne!(b_id, a_id);

        let (c, c_id) = add(
            "c.png",
            1_020,
            0xFFF,
            "Home\nSearch\nSettings\nProfile\nHelp\nAbout\nPrivacy\nTerms\nWeather today\nRain expected\nWindy afternoon\nUmbrella needed",
        );
        let updates = db.assign_burst_for_path(&c).expect("assign distinct");
        assert_eq!(
            updates,
            vec![(c.clone(), c_id)],
            "a distinct page with the same chrome stays separate"
        );
        assert!(
            db.image_paths_missing_burst(50)
                .expect("burst backfill rows")
                .is_empty()
        );

        let groups = db
            .burst_duplicate_groups(10)
            .expect("burst duplicate groups");
        assert_eq!(groups.len(), 1, "only the scrolled pair is a group");
        assert_eq!(groups[0].0, 0, "the display hash comes from look");
        assert_eq!(
            groups[0].1,
            vec![
                a.to_string_lossy().to_string(),
                b.to_string_lossy().to_string()
            ]
        );

        let loaded = db.load_bursts().expect("load bursts");
        assert!(loaded.contains(&(a_id, a_id)));
        assert!(loaded.contains(&(b_id, a_id)));
        assert!(loaded.contains(&(c_id, c_id)));
    }

    #[test]
    fn burst_assignment_chains_two_groups_to_the_lowest_id() {
        let temp = TempDb::new("burst-chain");
        let db = temp.db();

        let add = |name: &str, modified: u64, phash: u64, text: &str| {
            let path = temp.dir.join(name);
            let mut item = entry(&path, false);
            item.modified = modified;
            let id = db.upsert(&item).expect("insert image row");
            db.set_image_meta(&path, 320, 200, phash)
                .expect("store image metadata");
            db.set_image_text_sig(&path, &crate::indexer::burst::line_signature(text))
                .expect("store text signature");
            (path, id)
        };

        // A~C by text, B~C visually: C bridges both groups.
        let text = "alpha one\nbeta two\ngamma three\ndelta four";
        let (a, a_id) = add("a.png", 2_000, 0, text);
        db.assign_burst_for_path(&a).expect("assign a");
        let (b, b_id) = add("b.png", 2_010, 0xFF, "");
        db.assign_burst_for_path(&b).expect("assign b");
        let (c, c_id) = add("c.png", 2_020, 0xFF, text);
        let updates = db.assign_burst_for_path(&c).expect("assign bridge");
        assert_eq!(
            updates,
            vec![(a.clone(), a_id), (b.clone(), a_id), (c.clone(), a_id)],
            "bridge merges both groups into the lowest id"
        );
        assert!(b_id > a_id && c_id > b_id);
    }

    #[test]
    fn changed_burst_leader_detaches_and_rehomes_members() {
        let temp = TempDb::new("burst-detach");
        let db = temp.db();

        let add = |name: &str, modified: u64, text: &str| {
            let path = temp.dir.join(name);
            let mut item = entry(&path, false);
            item.modified = modified;
            let id = db.upsert(&item).expect("insert image row");
            db.set_image_meta(&path, 200, 100, 0)
                .expect("store image metadata");
            db.set_image_text_sig(&path, &crate::indexer::burst::line_signature(text))
                .expect("store text signature");
            (path, id)
        };

        let text = "alpha one\nbeta two\ngamma three\ndelta four";
        let (a, a_id) = add("a.png", 3_000, text);
        let (b, b_id) = add("b.png", 3_010, text);
        db.assign_burst_for_path(&a).expect("assign a");
        db.assign_burst_for_path(&b).expect("assign b");
        assert_eq!(
            db.load_bursts().expect("load bursts"),
            vec![(a_id, a_id), (b_id, a_id)]
        );

        // Rewriting leader A must re-home B under its own id.
        db.set_image_text_sig(
            &a,
            &crate::indexer::burst::line_signature(
                "weather today\nrain expected\nwindy afternoon\numbrella needed",
            ),
        )
        .expect("rewrite a signature");
        let updates = db.assign_burst_for_path(&a).expect("reassign a");
        assert_eq!(updates, vec![(a.clone(), a_id)]);
        assert_eq!(
            db.load_bursts().expect("load bursts after detach"),
            vec![(a_id, a_id), (b_id, b_id)]
        );
    }

    #[test]
    fn set_image_meta_is_scoped_to_the_node_path() {
        let temp = TempDb::new("image-meta");
        let db = temp.db();
        let image = temp.dir.join("photo.jpg");
        let other = temp.dir.join("other.jpg");
        db.upsert(&entry(&image, false)).expect("insert image row");
        db.upsert(&entry(&other, false)).expect("insert other row");

        db.set_image_meta(&image, 10, 7, 0xDEAD_BEEF)
            .expect("store image metadata");

        let conn = db.conn.lock();
        let (width, height, phash): (i64, i64, i64) = conn
            .query_row(
                "SELECT width, height, phash FROM nodes WHERE path = ?1",
                params![image.to_string_lossy().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read image metadata");
        assert_eq!((width, height), (10, 7));
        assert_eq!(phash as u64, 0xDEAD_BEEF);

        let other_phash: Option<i64> = conn
            .query_row(
                "SELECT phash FROM nodes WHERE path = ?1",
                params![other.to_string_lossy().to_string()],
                |row| row.get(0),
            )
            .expect("read other metadata");
        assert_eq!(other_phash, None);
    }

    #[test]
    fn deferred_image_hash_is_tracked_and_sentinel_is_excluded() {
        let temp = TempDb::new("deferred-phash");
        let db = temp.db();
        let image = temp.dir.join("photo.jpg");
        db.upsert(&entry(&image, false)).expect("insert image row");

        db.set_image_dimensions(&image, 64, 48)
            .expect("store dimensions");
        assert_eq!(
            db.image_paths_missing_phash(10).expect("missing hashes"),
            vec![image.clone()]
        );

        db.set_image_phash(&image, 0xDEAD_BEEF).expect("store hash");
        assert!(
            db.image_paths_missing_phash(10)
                .expect("missing")
                .is_empty()
        );
        assert_eq!(
            db.near_duplicate_groups(0, 10)
                .expect("duplicate groups")
                .len(),
            0,
            "a single image cannot form a group"
        );

        db.set_image_phash(&image, u64::MAX)
            .expect("store failure sentinel");
        assert!(
            db.image_paths_missing_phash(10)
                .expect("missing")
                .is_empty()
        );
        assert!(
            db.near_duplicate_groups(64, 10)
                .expect("duplicate groups")
                .is_empty()
        );
    }

    #[test]
    fn record_open_increments_count_and_updates_recency() {
        let temp = TempDb::new("record-open");
        let db = temp.db();
        let path = Path::new("/path/to/doc.pdf");

        db.record_open(path).expect("first record_open");
        let usage = db.load_usage().expect("load_usage");
        let &(count, last_opened) = usage.get(path).expect("path in usage");
        assert_eq!(count, 1);
        assert!(last_opened > 0);

        db.record_open(path).expect("second record_open");
        let usage2 = db.load_usage().expect("load_usage");
        let &(count2, last_opened2) = usage2.get(path).expect("path in usage");
        assert_eq!(count2, 2);
        assert!(last_opened2 >= last_opened);
    }

    #[test]
    fn upsert_remove_count_and_clear_cycle() {
        let temp = TempDb::new("crud-cycle");
        let db = temp.db();

        assert_eq!(db.count().unwrap(), 0);

        let p1 = Path::new("/data/a.txt");
        let p2 = Path::new("/data/b.txt");
        let id1 = db.upsert(&entry(p1, false)).expect("insert a");
        let id2 = db.upsert(&entry(p2, false)).expect("insert b");
        assert_eq!(db.count().unwrap(), 2);

        assert_eq!(db.get_node_id_by_path(p1).unwrap(), Some(id1));
        assert_eq!(db.get_node_id_by_path(p2).unwrap(), Some(id2));
        assert_eq!(
            db.get_node_id_by_path(Path::new("/not/found")).unwrap(),
            None
        );

        db.remove(p1).expect("remove p1");
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.get_node_id_by_path(p1).unwrap(), None);
        assert_eq!(db.get_node_id_by_path(p2).unwrap(), Some(id2));

        db.clear().expect("clear db");
        assert_eq!(db.count().unwrap(), 0);
        assert_eq!(db.get_node_id_by_path(p2).unwrap(), None);
    }

    #[test]
    fn unit_vectors_crud_and_reset_semantic_index() {
        let temp = TempDb::new("units-crud");
        let db = temp.db();

        let node_id = db.upsert(&entry(Path::new("/file.txt"), false)).unwrap();
        let vector: [i8; 4] = [10, -20, 30, -40];

        let unit_id = db
            .insert_unit(node_id, 0, 100, 1, &vector)
            .expect("insert unit");
        assert!(unit_id > 0);

        let all_units = db.load_all_units().expect("load units");
        assert_eq!(all_units.len(), 1);
        assert_eq!(all_units[0].0.node_id, node_id);
        assert_eq!(all_units[0].0.byte_start, 0);
        assert_eq!(all_units[0].0.byte_end, 100);
        assert_eq!(all_units[0].0.model_id, 1);
        assert_eq!(all_units[0].1, vector);

        db.delete_units_for_nodes(&[node_id]).expect("delete units");
        let after_del = db.load_all_units().expect("load units after del");
        assert!(after_del.is_empty());

        db.insert_unit(node_id, 0, 50, 1, &vector).unwrap();
        assert_eq!(db.load_all_units().unwrap().len(), 1);
        db.reset_semantic_index().expect("reset semantic index");
        assert!(db.load_all_units().unwrap().is_empty());
    }
}
