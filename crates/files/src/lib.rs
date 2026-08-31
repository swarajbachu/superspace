//! Scoped, incremental, ignore-aware file indexing for launcher search and nearby sharing.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use ignore::WalkBuilder;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_RESULTS: usize = 500;
const MAX_PREVIEW_BYTES: u64 = 64 * 1024;
const MAX_PREVIEW_LENGTH: usize = 64 * 1024;

/// One user-approved indexing root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchScope {
    /// Canonical directory to index.
    pub root: PathBuf,
    /// Include dotfiles unless excluded by ignore rules.
    #[serde(default)]
    pub include_hidden: bool,
    /// Additional gitignore-style patterns applied beneath this root.
    #[serde(default)]
    pub ignores: Vec<String>,
}

/// Search result suitable for previewing, opening, or passing to Nearby Share.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMatch {
    /// Canonical absolute file path.
    pub path: PathBuf,
    /// Filename shown as the primary label.
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time in Unix milliseconds, when available.
    pub modified_at_ms: Option<i64>,
}

/// Safe bounded file preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilePreview {
    /// UTF-8 content, truncated at the preview limit when necessary.
    Text {
        /// Decoded UTF-8 prefix.
        content: String,
        /// Whether content continues past the preview limit.
        truncated: bool,
    },
    /// Binary metadata without embedding arbitrary bytes.
    Binary {
        /// Complete file size in bytes.
        size: u64,
    },
}

/// Statistics from an incremental scope refresh.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexReport {
    /// Files observed during this generation.
    pub scanned: usize,
    /// New or metadata-changed records written.
    pub updated: usize,
    /// Records removed because files disappeared or became ignored.
    pub removed: usize,
    /// Entries skipped after recoverable filesystem errors.
    pub skipped: usize,
    /// Whether the caller requested cancellation.
    pub cancelled: bool,
}

/// Persistent file index with FTS-backed basename and path search.
pub struct FileIndex {
    connection: Connection,
}

impl FileIndex {
    /// Open or create an index database and migrate its schema.
    ///
    /// # Errors
    /// Returns database initialization failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, FileSearchError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS indexed_files (
                 path TEXT PRIMARY KEY,
                 scope TEXT NOT NULL,
                 name TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 modified_at_ms INTEGER,
                 generation INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS indexed_files_scope ON indexed_files(scope);
             CREATE VIRTUAL TABLE IF NOT EXISTS file_search USING fts5(
                 path UNINDEXED, name, searchable_path
             );
             CREATE TABLE IF NOT EXISTS index_state (
                 scope TEXT PRIMARY KEY,
                 generation INTEGER NOT NULL
             );",
        )?;
        Ok(Self { connection })
    }

    /// Incrementally refresh one scope while respecting git, global, and custom ignores.
    ///
    /// Existing unchanged records remain untouched. A cancelled scan retains the prior complete
    /// generation and does not delete records it did not reach.
    ///
    /// # Errors
    /// Returns errors for invalid scope roots, ignore patterns, or database writes.
    #[allow(
        clippy::too_many_lines,
        reason = "scan and generation commit form one auditable transaction"
    )]
    pub fn refresh(
        &mut self,
        scope: &SearchScope,
        cancel: &AtomicBool,
    ) -> Result<IndexReport, FileSearchError> {
        let root = scope.root.canonicalize()?;
        if !root.is_dir() {
            return Err(FileSearchError::InvalidScope);
        }
        let root_text = path_text(&root)?;
        let generation = self
            .connection
            .query_row(
                "SELECT generation FROM index_state WHERE scope = ?1",
                [&root_text],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(FileSearchError::GenerationOverflow)?;
        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(!scope.include_hidden)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true);
        if !scope.ignores.is_empty() {
            let mut overrides = ignore::overrides::OverrideBuilder::new(&root);
            for pattern in &scope.ignores {
                overrides.add(&format!("!{pattern}"))?;
            }
            builder.overrides(overrides.build()?);
        }

        let transaction = self.connection.transaction()?;
        let mut report = IndexReport::default();
        for result in builder.build() {
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            let Ok(entry) = result else {
                report.skipped += 1;
                continue;
            };
            let Some(file_type) = entry.file_type() else {
                report.skipped += 1;
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let path = match entry.path().canonicalize() {
                Ok(path) if path.starts_with(&root) => path,
                _ => {
                    report.skipped += 1;
                    continue;
                }
            };
            let Ok(metadata) = fs::metadata(&path) else {
                report.skipped += 1;
                continue;
            };
            let path_value = path_text(&path)?;
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or(FileSearchError::NonUtf8Path)?;
            let size = i64::try_from(metadata.len()).map_err(|_| FileSearchError::FileTooLarge)?;
            let modified_at_ms = modified_ms(&metadata);
            let unchanged = transaction
                .query_row(
                    "SELECT size = ?2 AND modified_at_ms IS ?3 FROM indexed_files WHERE path = ?1",
                    params![path_value, size, modified_at_ms],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            transaction.execute(
                "INSERT INTO indexed_files(path, scope, name, size, modified_at_ms, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET scope=excluded.scope, name=excluded.name,
                 size=excluded.size, modified_at_ms=excluded.modified_at_ms,
                 generation=excluded.generation",
                params![
                    path_value,
                    root_text,
                    name,
                    size,
                    modified_at_ms,
                    generation
                ],
            )?;
            if !unchanged {
                transaction.execute("DELETE FROM file_search WHERE path = ?1", [&path_value])?;
                let relative = path.strip_prefix(&root).unwrap_or(&path);
                transaction.execute(
                    "INSERT INTO file_search(path, name, searchable_path) VALUES (?1, ?2, ?3)",
                    params![path_value, name, relative.to_string_lossy()],
                )?;
                report.updated += 1;
            }
            report.scanned += 1;
        }
        if !report.cancelled {
            let mut stale = transaction
                .prepare("SELECT path FROM indexed_files WHERE scope = ?1 AND generation <> ?2")?;
            let stale_paths = stale
                .query_map(params![root_text, generation], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stale);
            for path in &stale_paths {
                transaction.execute("DELETE FROM file_search WHERE path = ?1", [path])?;
            }
            report.removed = transaction.execute(
                "DELETE FROM indexed_files WHERE scope = ?1 AND generation <> ?2",
                params![root_text, generation],
            )?;
            transaction.execute(
                "INSERT INTO index_state(scope, generation) VALUES (?1, ?2)
                 ON CONFLICT(scope) DO UPDATE SET generation=excluded.generation",
                params![root_text, generation],
            )?;
        }
        transaction.commit()?;
        Ok(report)
    }

    /// Search indexed filenames and relative paths within an optional scope.
    ///
    /// # Errors
    /// Returns database failures or malformed stored paths.
    pub fn search(
        &self,
        query: &str,
        scope: Option<&Path>,
        limit: usize,
    ) -> Result<Vec<FileMatch>, FileSearchError> {
        let query = fts_query(query);
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let scope = scope.map(path_text).transpose()?;
        let limit =
            i64::try_from(limit.min(MAX_RESULTS)).map_err(|_| FileSearchError::InvalidLimit)?;
        let mut statement = self.connection.prepare(
            "SELECT i.path, i.name, i.size, i.modified_at_ms
             FROM file_search s JOIN indexed_files i ON i.path = s.path
             WHERE file_search MATCH ?1 AND (?2 IS NULL OR i.scope = ?2)
             ORDER BY bm25(file_search), i.modified_at_ms DESC LIMIT ?3",
        )?;
        statement
            .query_map(params![query, scope, limit], |row| {
                let size: i64 = row.get(2)?;
                Ok(FileMatch {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    name: row.get(1)?,
                    size: u64::try_from(size)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, size))?,
                    modified_at_ms: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Read a bounded preview without following a changed path outside its canonical location.
///
/// # Errors
/// Returns filesystem failures or invalid UTF-8 paths.
pub fn preview(path: impl AsRef<Path>) -> Result<FilePreview, FileSearchError> {
    let path = path.as_ref().canonicalize()?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(FileSearchError::NotAFile);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > MAX_PREVIEW_LENGTH;
    bytes.truncate(MAX_PREVIEW_LENGTH);
    match String::from_utf8(bytes) {
        Ok(content) if !content.contains('\0') => Ok(FilePreview::Text { content, truncated }),
        _ => Ok(FilePreview::Binary {
            size: metadata.len(),
        }),
    }
}

/// Canonicalize a search result for an explicit open or Nearby Share action.
///
/// # Errors
/// Returns an error unless the target still exists as a regular file.
pub fn action_path(path: impl AsRef<Path>) -> Result<PathBuf, FileSearchError> {
    let path = path.as_ref().canonicalize()?;
    if path.is_file() {
        Ok(path)
    } else {
        Err(FileSearchError::NotAFile)
    }
}

/// File indexing and preview failures.
#[derive(Debug, Error)]
pub enum FileSearchError {
    /// Scope is not a directory.
    #[error("file search scope is invalid")]
    InvalidScope,
    /// A platform path cannot be represented in the portable index.
    #[error("file search path is not valid UTF-8")]
    NonUtf8Path,
    /// File size cannot be represented by SQLite.
    #[error("file is too large to index")]
    FileTooLarge,
    /// Index generation counter overflowed.
    #[error("file index generation overflowed")]
    GenerationOverflow,
    /// Search result limit is invalid.
    #[error("file search result limit is invalid")]
    InvalidLimit,
    /// Requested target is not a regular file.
    #[error("file search target is not a regular file")]
    NotAFile,
    /// Custom ignore pattern is invalid.
    #[error("file search ignore pattern is invalid")]
    Ignore(#[from] ignore::Error),
    /// Filesystem operation failed.
    #[error("file search filesystem operation failed")]
    Io(#[from] std::io::Error),
    /// Database operation failed.
    #[error("file search database operation failed")]
    Database(#[from] rusqlite::Error),
}

fn path_text(path: &Path) -> Result<String, FileSearchError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(FileSearchError::NonUtf8Path)
}

fn modified_ms(metadata: &fs::Metadata) -> Option<i64> {
    let milliseconds = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(milliseconds).ok()
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn incrementally_indexes_ignores_updates_and_removes() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("report.md"), "first").expect("report");
        fs::write(directory.path().join("secret.tmp"), "ignored").expect("secret");
        fs::create_dir(directory.path().join(".hidden")).expect("hidden directory");
        fs::write(directory.path().join(".hidden/note.txt"), "hidden").expect("hidden file");
        let mut index = FileIndex::open(":memory:").expect("index");
        let scope = SearchScope {
            root: directory.path().to_owned(),
            include_hidden: false,
            ignores: vec!["*.tmp".into()],
        };
        let cancel = AtomicBool::new(false);
        assert_eq!(index.refresh(&scope, &cancel).expect("initial").updated, 1);
        assert_eq!(index.search("rep", None, 10).expect("search").len(), 1);
        assert!(
            index
                .search("secret", None, 10)
                .expect("ignored")
                .is_empty()
        );

        fs::write(directory.path().join("report.md"), "second version").expect("update");
        let update = index.refresh(&scope, &cancel).expect("update index");
        assert_eq!(update.updated, 1);
        fs::remove_file(directory.path().join("report.md")).expect("remove");
        assert_eq!(
            index
                .refresh(&scope, &cancel)
                .expect("remove index")
                .removed,
            1
        );
        assert!(index.search("report", None, 10).expect("gone").is_empty());
    }

    #[test]
    fn cancellation_never_prunes_and_previews_are_bounded() {
        let directory = tempfile::tempdir().expect("tempdir");
        let file = directory.path().join("hello.txt");
        fs::write(&file, "hello").expect("text");
        let mut index = FileIndex::open(":memory:").expect("index");
        let scope = SearchScope {
            root: directory.path().to_owned(),
            include_hidden: true,
            ignores: Vec::new(),
        };
        index
            .refresh(&scope, &AtomicBool::new(false))
            .expect("initial");
        fs::remove_file(&file).expect("remove");
        let report = index
            .refresh(&scope, &AtomicBool::new(true))
            .expect("cancelled");
        assert!(report.cancelled);
        assert_eq!(index.search("hello", None, 10).expect("retained").len(), 1);

        let text = directory.path().join("preview.txt");
        fs::write(&text, "hello preview").expect("preview");
        assert_eq!(
            preview(&text).expect("text preview"),
            FilePreview::Text {
                content: "hello preview".into(),
                truncated: false
            }
        );
        let binary = directory.path().join("binary.dat");
        fs::write(&binary, [0, 1, 2]).expect("binary");
        assert_eq!(
            preview(&binary).expect("binary preview"),
            FilePreview::Binary { size: 3 }
        );
        assert_eq!(
            action_path(&text).expect("action path"),
            text.canonicalize().expect("canonical")
        );
    }
}
