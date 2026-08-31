use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 2;

/// Clipboard representation stored in history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    /// UTF-8 text; links and addresses are derived during presentation.
    Text,
    /// UTF-8 HTML accompanied by searchable plain text.
    Html,
    /// Rich Text Format accompanied by searchable plain text.
    Rtf,
    /// PNG data held in the blob store.
    Image,
    /// JSON file-transfer metadata held in the blob store.
    Files,
}

impl ClipboardKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Html => "html",
            Self::Rtf => "rtf",
            Self::Image => "image",
            Self::Files => "files",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "html" => Some(Self::Html),
            "rtf" => Some(Self::Rtf),
            "image" => Some(Self::Image),
            "files" => Some(Self::Files),
            _ => None,
        }
    }
}

/// Where a clipboard event originated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardSource {
    /// Local application identifier, when available.
    pub application_id: Option<String>,
    /// Paired device identifier, when remote.
    pub device_id: Option<Uuid>,
}

/// Durable clipboard entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    /// Stable event identity used for deduplication.
    pub id: Uuid,
    /// Clipboard representation.
    pub kind: ClipboardKind,
    /// Searchable or directly pasteable text.
    pub text: Option<String>,
    /// Content-addressed binary payload.
    pub blob_hash: Option<String>,
    /// Source attribution.
    pub source: ClipboardSource,
    /// Capture time in Unix milliseconds.
    pub created_at: i64,
    /// Pin time in Unix milliseconds.
    pub pinned_at: Option<i64>,
    /// Whether content should be concealed and excluded from automatic sync.
    pub sensitive: bool,
}

/// History query with optional kind restriction.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardQuery<'a> {
    /// Case-insensitive text query.
    pub text: &'a str,
    /// Restrict results to one representation.
    pub kind: Option<ClipboardKind>,
    /// Reveal entries marked sensitive. Defaults to false.
    pub include_sensitive: bool,
    /// Maximum rows returned.
    pub limit: usize,
}

/// Age-based history policy. Pinned entries are always retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Delete unpinned entries older than the given Unix millisecond.
    Before(i64),
    /// Keep all history.
    Forever,
}

/// Storage failures surfaced without leaking SQL statements or user content.
#[derive(Debug, Error)]
pub enum StorageError {
    /// SQLite operation failed.
    #[error("clipboard database operation failed")]
    Database(#[from] rusqlite::Error),
    /// A database row contained an unknown kind.
    #[error("clipboard database contains an unsupported item kind")]
    UnsupportedKind,
    /// A caller attempted to store an invalid entry shape.
    #[error("clipboard entry is missing its required content")]
    InvalidEntry,
}

/// SQLite-backed clipboard history and search index.
pub struct ClipboardStore {
    connection: Connection,
}

impl ClipboardStore {
    /// Open or create a store and apply migrations.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot open or migrate the database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let connection = Connection::open(path)?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    /// Create an in-memory store for tests and ephemeral sessions.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot initialize the schema.
    pub fn memory() -> Result<Self, StorageError> {
        let connection = Connection::open_in_memory()?;
        Self::configure(&connection)?;
        Self::migrate(&connection)?;
        Ok(Self { connection })
    }

    fn configure(connection: &Connection) -> Result<(), rusqlite::Error> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        Ok(())
    }

    fn migrate(connection: &Connection) -> Result<(), rusqlite::Error> {
        let current: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current < 1 {
            connection.execute_batch(
                "BEGIN;
                 CREATE TABLE clipboard_items (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    id TEXT NOT NULL UNIQUE,
                    kind TEXT NOT NULL,
                    text_content TEXT,
                    blob_hash TEXT,
                    source_application TEXT,
                    source_device TEXT,
                    created_at INTEGER NOT NULL,
                    pinned_at INTEGER,
                    CHECK(text_content IS NOT NULL OR blob_hash IS NOT NULL)
                 );
                 CREATE INDEX clipboard_items_created ON clipboard_items(created_at DESC);
                 CREATE INDEX clipboard_items_pinned ON clipboard_items(pinned_at)
                    WHERE pinned_at IS NOT NULL;
                 CREATE VIRTUAL TABLE clipboard_search USING fts5(
                    text_content, content='clipboard_items', content_rowid='sequence',
                    tokenize='trigram case_sensitive 0'
                 );
                 CREATE TRIGGER clipboard_insert AFTER INSERT ON clipboard_items BEGIN
                    INSERT INTO clipboard_search(rowid, text_content)
                    VALUES (new.sequence, coalesce(new.text_content, ''));
                 END;
                 CREATE TRIGGER clipboard_delete AFTER DELETE ON clipboard_items BEGIN
                    INSERT INTO clipboard_search(clipboard_search, rowid, text_content)
                    VALUES ('delete', old.sequence, coalesce(old.text_content, ''));
                 END;
                 CREATE TRIGGER clipboard_update AFTER UPDATE OF text_content ON clipboard_items BEGIN
                    INSERT INTO clipboard_search(clipboard_search, rowid, text_content)
                    VALUES ('delete', old.sequence, coalesce(old.text_content, ''));
                    INSERT INTO clipboard_search(rowid, text_content)
                    VALUES (new.sequence, coalesce(new.text_content, ''));
                 END;
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        if current < 2 {
            connection.execute_batch(
                "BEGIN;
                 ALTER TABLE clipboard_items ADD COLUMN sensitive INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        debug_assert_eq!(SCHEMA_VERSION, 2);
        Ok(())
    }

    /// Insert a new event once. Replayed IDs are harmless no-ops.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidEntry`] for empty content or a database error.
    pub fn insert(&self, entry: &ClipboardEntry) -> Result<bool, StorageError> {
        if entry.text.as_deref().is_none_or(str::is_empty) && entry.blob_hash.is_none() {
            return Err(StorageError::InvalidEntry);
        }
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO clipboard_items
             (id, kind, text_content, blob_hash, source_application, source_device, created_at,
              pinned_at, sensitive)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                entry.id.to_string(),
                entry.kind.as_str(),
                entry.text,
                entry.blob_hash,
                entry.source.application_id,
                entry.source.device_id.map(|id| id.to_string()),
                entry.created_at,
                entry.pinned_at,
                entry.sensitive,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Query history with pins first and recency within each section.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for SQL or row-decoding failures.
    pub fn query(&self, query: &ClipboardQuery<'_>) -> Result<Vec<ClipboardEntry>, StorageError> {
        let limit = i64::try_from(query.limit.clamp(1, 1_000)).unwrap_or(1_000);
        let normalized = query.text.trim();
        if normalized.chars().count() >= 3 {
            self.query_fts(normalized, query.kind, query.include_sensitive, limit)
        } else {
            self.query_recent(normalized, query.kind, query.include_sensitive, limit)
        }
    }

    fn query_fts(
        &self,
        text: &str,
        kind: Option<ClipboardKind>,
        include_sensitive: bool,
        limit: i64,
    ) -> Result<Vec<ClipboardEntry>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT i.id, i.kind, i.text_content, i.blob_hash, i.source_application,
                    i.source_device, i.created_at, i.pinned_at, i.sensitive
             FROM clipboard_search s JOIN clipboard_items i ON i.sequence = s.rowid
             WHERE clipboard_search MATCH ?1 AND (?2 IS NULL OR i.kind = ?2)
               AND (?3 OR NOT i.sensitive)
             ORDER BY i.pinned_at IS NULL, i.pinned_at ASC, i.created_at DESC LIMIT ?4",
        )?;
        Self::collect(statement.query_map(
            params![
                format!("\"{}\"", text.replace('"', "\"\"")),
                kind.map(ClipboardKind::as_str),
                include_sensitive,
                limit
            ],
            Self::row,
        )?)
    }

    fn query_recent(
        &self,
        text: &str,
        kind: Option<ClipboardKind>,
        include_sensitive: bool,
        limit: i64,
    ) -> Result<Vec<ClipboardEntry>, StorageError> {
        let pattern = format!("%{}%", text.replace('%', "\\%").replace('_', "\\_"));
        let mut statement = self.connection.prepare(
            "SELECT id, kind, text_content, blob_hash, source_application, source_device,
                    created_at, pinned_at, sensitive
             FROM clipboard_items
             WHERE (?1 = '%%' OR text_content LIKE ?1 ESCAPE '\\')
               AND (?2 IS NULL OR kind = ?2)
               AND (?3 OR NOT sensitive)
             ORDER BY pinned_at IS NULL, pinned_at ASC, created_at DESC LIMIT ?4",
        )?;
        Self::collect(statement.query_map(
            params![
                pattern,
                kind.map(ClipboardKind::as_str),
                include_sensitive,
                limit
            ],
            Self::row,
        )?)
    }

    fn collect(
        rows: rusqlite::MappedRows<
            '_,
            impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<ClipboardEntry>,
        >,
    ) -> Result<Vec<ClipboardEntry>, StorageError> {
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ClipboardEntry> {
        let kind_value: String = row.get(1)?;
        let kind = ClipboardKind::parse(&kind_value).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(StorageError::UnsupportedKind),
            )
        })?;
        let source_device: Option<String> = row.get(5)?;
        Ok(ClipboardEntry {
            id: parse_uuid(&row.get::<_, String>(0)?, 0)?,
            kind,
            text: row.get(2)?,
            blob_hash: row.get(3)?,
            source: ClipboardSource {
                application_id: row.get(4)?,
                device_id: source_device
                    .as_deref()
                    .map(|value| parse_uuid(value, 5))
                    .transpose()?,
            },
            created_at: row.get(6)?,
            pinned_at: row.get(7)?,
            sensitive: row.get(8)?,
        })
    }

    /// Set or clear a pin timestamp.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update cannot be persisted.
    pub fn set_pinned(&self, id: Uuid, pinned_at: Option<i64>) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE clipboard_items SET pinned_at = ?2 WHERE id = ?1",
            params![id.to_string(), pinned_at],
        )? == 1)
    }

    /// Remove one history entry and return its blob identity for later garbage collection.
    ///
    /// # Errors
    ///
    /// Returns a database error when the lookup or deletion fails.
    pub fn remove(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        let blob = self
            .connection
            .query_row(
                "SELECT blob_hash FROM clipboard_items WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        self.connection.execute(
            "DELETE FROM clipboard_items WHERE id = ?1",
            [id.to_string()],
        )?;
        Ok(blob.flatten())
    }

    /// Apply the retention policy and return the number of deleted unpinned entries.
    ///
    /// # Errors
    ///
    /// Returns a database error when pruning fails.
    pub fn prune(&self, retention: Retention) -> Result<usize, StorageError> {
        match retention {
            Retention::Forever => Ok(0),
            Retention::Before(cutoff) => Ok(self.connection.execute(
                "DELETE FROM clipboard_items WHERE pinned_at IS NULL AND created_at < ?1",
                [cutoff],
            )?),
        }
    }

    /// Count all stored rows, including entries outside a query window.
    ///
    /// # Errors
    ///
    /// Returns a database error when counting fails.
    pub fn count(&self) -> Result<u64, StorageError> {
        self.connection
            .query_row("SELECT count(*) FROM clipboard_items", [], |row| row.get(0))
            .map_err(StorageError::from)
    }
}

fn parse_uuid(value: &str, column: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str, created_at: i64) -> ClipboardEntry {
        ClipboardEntry {
            id: Uuid::new_v4(),
            kind: ClipboardKind::Text,
            text: Some(value.into()),
            blob_hash: None,
            source: ClipboardSource::default(),
            created_at,
            pinned_at: None,
            sensitive: false,
        }
    }

    #[test]
    fn insert_deduplicates_and_searches() {
        let store = ClipboardStore::memory().expect("open store");
        let entry = text("Superspace crosses devices", 1);
        assert!(store.insert(&entry).expect("first insert"));
        assert!(!store.insert(&entry).expect("replay insert"));
        let found = store
            .query(&ClipboardQuery {
                text: "cross",
                kind: None,
                include_sensitive: false,
                limit: 20,
            })
            .expect("query");
        assert_eq!(found, [entry]);
    }

    #[test]
    fn pins_lead_and_survive_pruning() {
        let store = ClipboardStore::memory().expect("open store");
        let old = text("old", 1);
        let recent = text("recent", 100);
        store.insert(&old).expect("insert old");
        store.insert(&recent).expect("insert recent");
        store.set_pinned(old.id, Some(5)).expect("pin old");
        assert_eq!(store.prune(Retention::Before(50)).expect("prune"), 0);
        let found = store
            .query(&ClipboardQuery {
                text: "",
                kind: None,
                include_sensitive: false,
                limit: 20,
            })
            .expect("query");
        assert_eq!(
            found.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [old.id, recent.id]
        );
    }

    #[test]
    fn short_queries_escape_like_wildcards() {
        let store = ClipboardStore::memory().expect("open store");
        store.insert(&text("100%", 1)).expect("insert percent");
        store.insert(&text("100x", 2)).expect("insert other");
        let found = store
            .query(&ClipboardQuery {
                text: "%",
                kind: None,
                include_sensitive: false,
                limit: 20,
            })
            .expect("query");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text.as_deref(), Some("100%"));
    }

    #[test]
    fn invalid_empty_entries_are_rejected() {
        let store = ClipboardStore::memory().expect("open store");
        let mut entry = text("", 1);
        entry.text = None;
        assert!(matches!(
            store.insert(&entry),
            Err(StorageError::InvalidEntry)
        ));
    }

    #[test]
    fn sensitive_entries_require_explicit_reveal() {
        let store = ClipboardStore::memory().expect("open store");
        let mut secret = text("correct horse battery staple", 1);
        secret.sensitive = true;
        store.insert(&secret).expect("insert secret");
        assert!(
            store
                .query(&ClipboardQuery {
                    text: "horse",
                    kind: None,
                    include_sensitive: false,
                    limit: 20,
                })
                .expect("concealed query")
                .is_empty()
        );
        assert_eq!(
            store
                .query(&ClipboardQuery {
                    text: "horse",
                    kind: None,
                    include_sensitive: true,
                    limit: 20,
                })
                .expect("revealed query"),
            [secret]
        );
    }
}
