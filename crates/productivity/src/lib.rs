//! Persistent built-in quicklinks, snippets, commands, notes, and emoji search.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The kind and payload of a productivity item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ItemContent {
    /// URL template. `{query}` is replaced with percent-encoded input.
    Quicklink(String),
    /// Markdown snippet body.
    Snippet(String),
    /// Executable plus an argument vector; never interpreted by a shell.
    Command {
        /// Executable path or name passed directly to process spawning.
        executable: String,
        /// Literal argument vector; `{query}` placeholders are substituted in place.
        args: Vec<String>,
    },
    /// Markdown note body.
    Note(String),
}

/// A user-defined productivity item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// Stable identifier.
    pub id: String,
    /// User-facing title.
    pub title: String,
    /// Optional expansion keyword, without surrounding whitespace.
    pub keyword: Option<String>,
    /// Searchable tags.
    pub tags: Vec<String>,
    /// Kind-specific content.
    pub content: ItemContent,
    /// Whether the item leads search results.
    pub favorite: bool,
    /// Monotonic client timestamp in milliseconds.
    pub updated_at_ms: i64,
}

impl Item {
    /// Construct an item with a random stable identifier.
    #[must_use]
    pub fn new(title: impl Into<String>, content: ItemContent, updated_at_ms: i64) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            keyword: None,
            tags: Vec::new(),
            content,
            favorite: false,
            updated_at_ms,
        }
    }
}

/// A safe command invocation resolved without shell parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInvocation {
    /// Executable path or name.
    pub executable: String,
    /// Literal argument vector after placeholder expansion.
    pub args: Vec<String>,
}

/// `SQLite`-backed productivity collection.
pub struct ProductivityStore {
    connection: Connection,
}

impl ProductivityStore {
    /// Open or create a collection and migrate its schema.
    ///
    /// # Errors
    /// Returns database initialization failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProductivityError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS productivity_items (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 keyword TEXT UNIQUE,
                 tags TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 favorite INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS productivity_search USING fts5(
                 id UNINDEXED, title, keyword, tags, payload
             );",
        )?;
        Ok(Self { connection })
    }

    /// Insert or replace a fully validated item and its search index entry.
    ///
    /// # Errors
    /// Returns validation, serialization, or database failures.
    pub fn save(&mut self, item: &Item) -> Result<(), ProductivityError> {
        validate(item)?;
        let (kind, payload) = encode_content(&item.content);
        let tags = item.tags.join("\n");
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO productivity_items
             (id, title, keyword, tags, kind, payload, favorite, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, keyword=excluded.keyword,
             tags=excluded.tags, kind=excluded.kind, payload=excluded.payload,
             favorite=excluded.favorite, updated_at_ms=excluded.updated_at_ms",
            params![
                item.id,
                item.title,
                item.keyword,
                tags,
                kind,
                payload,
                item.favorite,
                item.updated_at_ms
            ],
        )?;
        transaction.execute("DELETE FROM productivity_search WHERE id = ?1", [&item.id])?;
        transaction.execute(
            "INSERT INTO productivity_search (id, title, keyword, tags, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![item.id, item.title, item.keyword, tags, payload],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Remove an item and its search entry.
    ///
    /// # Errors
    /// Returns database failures.
    pub fn delete(&mut self, id: &str) -> Result<bool, ProductivityError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM productivity_search WHERE id = ?1", [id])?;
        let changed = transaction.execute("DELETE FROM productivity_items WHERE id = ?1", [id])?;
        transaction.commit()?;
        Ok(changed != 0)
    }

    /// Search title, keyword, tags, and payload, with favorites first.
    ///
    /// # Errors
    /// Returns database failures or malformed stored data.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>, ProductivityError> {
        let limit = i64::try_from(limit.min(500)).map_err(|_| ProductivityError::InvalidItem)?;
        let query = query.trim();
        let sql = if query.is_empty() {
            "SELECT i.id, i.title, i.keyword, i.tags, i.kind, i.payload, i.favorite,
                    i.updated_at_ms
             FROM productivity_items i
             ORDER BY i.favorite DESC, i.updated_at_ms DESC LIMIT ?1"
        } else {
            "SELECT i.id, i.title, i.keyword, i.tags, i.kind, i.payload, i.favorite,
                    i.updated_at_ms
             FROM productivity_search s JOIN productivity_items i ON i.id = s.id
             WHERE productivity_search MATCH ?1
             ORDER BY i.favorite DESC, bm25(productivity_search), i.updated_at_ms DESC LIMIT ?2"
        };
        let mut statement = self.connection.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| decode_row(row);
        let rows = if query.is_empty() {
            statement.query_map([limit], map_row)?
        } else {
            let match_query = fts_prefix_query(query);
            statement.query_map(params![match_query, limit], map_row)?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Find an item by exact expansion keyword.
    ///
    /// # Errors
    /// Returns database failures or malformed stored data.
    pub fn by_keyword(&self, keyword: &str) -> Result<Option<Item>, ProductivityError> {
        self.connection
            .query_row(
                "SELECT id, title, keyword, tags, kind, payload, favorite, updated_at_ms
                 FROM productivity_items WHERE keyword = ?1",
                [keyword],
                decode_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Find an item by stable identifier.
    ///
    /// # Errors
    /// Returns database failures or malformed stored data.
    pub fn get(&self, id: &str) -> Result<Option<Item>, ProductivityError> {
        self.connection
            .query_row(
                "SELECT id, title, keyword, tags, kind, payload, favorite, updated_at_ms
                 FROM productivity_items WHERE id = ?1",
                [id],
                decode_row,
            )
            .optional()
            .map_err(Into::into)
    }
}

/// Expand an exact snippet keyword only when it is the final standalone token.
#[must_use]
pub fn expand_snippet(input: &str, keyword: &str, body: &str) -> Option<String> {
    let prefix = input.strip_suffix(keyword)?;
    if prefix.chars().next_back().is_some_and(|character| {
        character.is_alphanumeric() || character == '_' || character == '-'
    }) {
        return None;
    }
    Some(format!("{prefix}{body}"))
}

/// Resolve a quicklink template using RFC 3986 percent encoding.
#[must_use]
pub fn resolve_quicklink(template: &str, query: &str) -> String {
    template.replace("{query}", &percent_encode(query.as_bytes()))
}

/// Resolve a stored command by replacing literal `{query}` arguments.
///
/// # Errors
/// Returns an error unless the item is a safe, non-empty command definition.
pub fn resolve_command(item: &Item, query: &str) -> Result<CommandInvocation, ProductivityError> {
    let ItemContent::Command { executable, args } = &item.content else {
        return Err(ProductivityError::WrongKind);
    };
    if executable.trim().is_empty() || executable.contains('\0') {
        return Err(ProductivityError::InvalidItem);
    }
    Ok(CommandInvocation {
        executable: executable.clone(),
        args: args
            .iter()
            .map(|argument| argument.replace("{query}", query))
            .collect(),
    })
}

/// A built-in emoji search result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Emoji {
    /// Unicode grapheme.
    pub value: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Search aliases.
    pub keywords: Vec<&'static str>,
}

/// A searchable typographic, mathematical, currency, or keyboard symbol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// Unicode grapheme.
    pub value: &'static str,
    /// Human-readable name.
    pub name: &'static str,
    /// Search aliases.
    pub keywords: &'static [&'static str],
}

/// Search the built-in emoji catalog.
#[must_use]
pub fn search_emoji(query: &str, limit: usize) -> Vec<Emoji> {
    let query = query.trim().to_ascii_lowercase();
    let mut matches = emojis::iter()
        .filter_map(|emoji| emoji_match_score(emoji, &query).map(|score| (score, emoji)))
        .collect::<Vec<_>>();
    matches.sort_by_key(|(score, _)| *score);
    matches
        .into_iter()
        .take(limit)
        .map(|(_, emoji)| emoji)
        .map(|emoji| Emoji {
            value: emoji.as_str(),
            name: emoji.name(),
            keywords: emoji
                .shortcodes()
                .chain(extra_emoji_keywords(emoji.as_str()).iter().copied())
                .collect(),
        })
        .collect()
}

/// Search the built-in symbol catalog by name or common alias.
#[must_use]
pub fn search_symbols(query: &str, limit: usize) -> Vec<Symbol> {
    let query = query.trim().to_ascii_lowercase();
    SYMBOLS
        .iter()
        .copied()
        .filter(|symbol| {
            query.is_empty()
                || symbol.value == query
                || symbol.name.contains(&query)
                || symbol
                    .keywords
                    .iter()
                    .any(|keyword| keyword.contains(&query))
        })
        .take(limit)
        .collect()
}

fn emoji_match_score(emoji: &emojis::Emoji, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let aliases = extra_emoji_keywords(emoji.as_str());
    if aliases.contains(&query) {
        Some(0)
    } else if emoji.shortcodes().any(|shortcode| shortcode == query) {
        Some(1)
    } else if emoji.name() == query {
        Some(2)
    } else if aliases.iter().any(|keyword| keyword.contains(query)) {
        Some(3)
    } else if emoji
        .shortcodes()
        .any(|shortcode| shortcode.contains(query))
    {
        Some(4)
    } else if emoji.name().contains(query) {
        Some(5)
    } else {
        None
    }
}

fn extra_emoji_keywords(value: &str) -> &'static [&'static str] {
    match value {
        "😀" => &["smile", "happy"],
        "😂" => &["laugh", "lol"],
        "❤️" => &["love", "like"],
        "👍" => &["yes", "approve", "like"],
        "🎉" => &["celebrate", "tada"],
        "🚀" => &["launch", "ship"],
        "✅" => &["done", "success"],
        "🔥" => &["hot", "lit"],
        "👀" => &["look", "watch"],
        "🙏" => &["thanks", "please"],
        _ => &[],
    }
}

/// Productivity persistence and validation failures.
#[derive(Debug, Error)]
pub enum ProductivityError {
    /// Item fields violate limits or invariants.
    #[error("productivity item is invalid")]
    InvalidItem,
    /// An operation was used with the wrong item kind.
    #[error("productivity item has the wrong kind")]
    WrongKind,
    /// Stored content is malformed.
    #[error("stored productivity item is corrupt")]
    Corrupt,
    /// Database operation failed.
    #[error("productivity database operation failed")]
    Database(#[from] rusqlite::Error),
}

fn validate(item: &Item) -> Result<(), ProductivityError> {
    if item.id.is_empty()
        || item.id.len() > 128
        || item.title.trim().is_empty()
        || item.title.len() > 512
        || item.tags.len() > 64
        || item.tags.iter().any(|tag| tag.len() > 128)
        || item.keyword.as_ref().is_some_and(|keyword| {
            keyword.is_empty() || keyword.len() > 128 || keyword.contains(char::is_whitespace)
        })
    {
        return Err(ProductivityError::InvalidItem);
    }
    let (_, payload) = encode_content(&item.content);
    if payload.len() > 4 * 1024 * 1024
        || matches!(
            &item.content,
            ItemContent::Command { executable, args }
                if executable.trim().is_empty()
                    || executable.contains('\0')
                    || args.len() > 256
                    || args.iter().any(|argument| argument.contains('\0'))
        )
    {
        return Err(ProductivityError::InvalidItem);
    }
    Ok(())
}

fn encode_content(content: &ItemContent) -> (&'static str, String) {
    match content {
        ItemContent::Quicklink(value) => ("quicklink", value.clone()),
        ItemContent::Snippet(value) => ("snippet", value.clone()),
        ItemContent::Note(value) => ("note", value.clone()),
        ItemContent::Command { executable, args } => {
            let mut fields = Vec::with_capacity(args.len() + 1);
            fields.push(executable.as_str());
            fields.extend(args.iter().map(String::as_str));
            ("command", fields.join("\0"))
        }
    }
}

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let kind: String = row.get(4)?;
    let payload: String = row.get(5)?;
    let content = match kind.as_str() {
        "quicklink" => ItemContent::Quicklink(payload),
        "snippet" => ItemContent::Snippet(payload),
        "note" => ItemContent::Note(payload),
        "command" => {
            let mut fields = payload.split('\0');
            let executable = fields.next().unwrap_or_default().to_owned();
            ItemContent::Command {
                executable,
                args: fields.map(str::to_owned).collect(),
            }
        }
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let tags: String = row.get(3)?;
    Ok(Item {
        id: row.get(0)?,
        title: row.get(1)?,
        keyword: row.get(2)?,
        tags: tags
            .split('\n')
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect(),
        content,
        favorite: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn fts_prefix_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"*", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    output
}

macro_rules! symbol {
    ($value:literal, $name:literal, [$($keyword:literal),* $(,)?]) => {
        Symbol { value: $value, name: $name, keywords: &[$($keyword),*] }
    };
}

const SYMBOLS: &[Symbol] = &[
    symbol!("$", "dollar sign", ["currency", "usd", "money"]),
    symbol!("¢", "cent sign", ["currency", "money"]),
    symbol!("£", "pound sign", ["currency", "gbp", "money"]),
    symbol!("€", "euro sign", ["currency", "eur", "money"]),
    symbol!("¥", "yen sign", ["currency", "jpy", "money"]),
    symbol!("₹", "rupee sign", ["currency", "inr", "money"]),
    symbol!("₽", "ruble sign", ["currency", "rub", "money"]),
    symbol!("₩", "won sign", ["currency", "krw", "money"]),
    symbol!("₿", "bitcoin sign", ["currency", "btc", "crypto"]),
    symbol!("±", "plus minus", ["math", "add", "subtract"]),
    symbol!("×", "multiplication sign", ["math", "times", "multiply"]),
    symbol!("÷", "division sign", ["math", "divide"]),
    symbol!("≠", "not equal", ["math", "inequality"]),
    symbol!("≈", "approximately equal", ["math", "approx"]),
    symbol!("≤", "less than or equal", ["math", "inequality"]),
    symbol!("≥", "greater than or equal", ["math", "inequality"]),
    symbol!("∞", "infinity", ["math", "forever"]),
    symbol!("√", "square root", ["math", "radical"]),
    symbol!("∑", "summation", ["math", "sum", "sigma"]),
    symbol!("∏", "product", ["math", "pi"]),
    symbol!("∫", "integral", ["math", "calculus"]),
    symbol!("∂", "partial differential", ["math", "calculus"]),
    symbol!("∆", "delta", ["math", "change", "triangle"]),
    symbol!("π", "pi", ["math", "greek"]),
    symbol!("°", "degree sign", ["temperature", "angle"]),
    symbol!("‰", "per mille", ["percent", "thousand"]),
    symbol!("←", "left arrow", ["arrow", "back"]),
    symbol!("↑", "up arrow", ["arrow"]),
    symbol!("→", "right arrow", ["arrow", "forward"]),
    symbol!("↓", "down arrow", ["arrow"]),
    symbol!("↔", "left right arrow", ["arrow", "horizontal"]),
    symbol!("↕", "up down arrow", ["arrow", "vertical"]),
    symbol!("↖", "up left arrow", ["arrow", "diagonal"]),
    symbol!("↗", "up right arrow", ["arrow", "diagonal"]),
    symbol!("↘", "down right arrow", ["arrow", "diagonal"]),
    symbol!("↙", "down left arrow", ["arrow", "diagonal"]),
    symbol!("⇒", "double right arrow", ["arrow", "implies"]),
    symbol!("⇔", "double left right arrow", ["arrow", "equivalent"]),
    symbol!("©", "copyright", ["legal"]),
    symbol!("®", "registered trademark", ["legal", "trademark"]),
    symbol!("™", "trademark", ["legal", "tm"]),
    symbol!("§", "section sign", ["legal", "paragraph"]),
    symbol!("¶", "paragraph sign", ["pilcrow", "writing"]),
    symbol!("†", "dagger", ["footnote", "cross"]),
    symbol!("‡", "double dagger", ["footnote", "cross"]),
    symbol!("•", "bullet", ["dot", "list"]),
    symbol!("…", "ellipsis", ["dots", "more"]),
    symbol!("–", "en dash", ["dash", "typography"]),
    symbol!("—", "em dash", ["dash", "typography"]),
    symbol!("“", "left double quote", ["quote", "typography"]),
    symbol!("”", "right double quote", ["quote", "typography"]),
    symbol!("‘", "left single quote", ["quote", "typography"]),
    symbol!("’", "right single quote", ["apostrophe", "typography"]),
    symbol!("★", "filled star", ["shape", "favorite"]),
    symbol!("☆", "outline star", ["shape", "favorite"]),
    symbol!("●", "filled circle", ["shape", "dot"]),
    symbol!("○", "outline circle", ["shape"]),
    symbol!("■", "filled square", ["shape"]),
    symbol!("□", "outline square", ["shape", "checkbox"]),
    symbol!("▲", "filled up triangle", ["shape", "arrow"]),
    symbol!("△", "outline up triangle", ["shape"]),
    symbol!("▼", "filled down triangle", ["shape", "arrow"]),
    symbol!("▽", "outline down triangle", ["shape"]),
    symbol!("◆", "filled diamond", ["shape"]),
    symbol!("◇", "outline diamond", ["shape"]),
    symbol!("✓", "check mark", ["done", "success", "tick"]),
    symbol!("✔", "heavy check mark", ["done", "success", "tick"]),
    symbol!("✕", "multiplication x", ["close", "delete"]),
    symbol!("✖", "heavy multiplication x", ["close", "delete"]),
    symbol!("⌘", "command key", ["mac", "keyboard", "cmd"]),
    symbol!("⌥", "option key", ["mac", "keyboard", "alt"]),
    symbol!("⇧", "shift key", ["mac", "keyboard"]),
    symbol!("⌃", "control key", ["mac", "keyboard", "ctrl"]),
    symbol!("⎋", "escape key", ["mac", "keyboard", "esc"]),
    symbol!("↩", "return key", ["mac", "keyboard", "enter"]),
    symbol!("⌫", "delete left key", ["mac", "keyboard", "backspace"]),
    symbol!("⌦", "delete right key", ["mac", "keyboard"]),
    symbol!("␣", "space key", ["keyboard", "spacebar"]),
    symbol!("α", "alpha", ["greek"]),
    symbol!("β", "beta", ["greek"]),
    symbol!("γ", "gamma", ["greek"]),
    symbol!("δ", "delta", ["greek"]),
    symbol!("λ", "lambda", ["greek", "programming"]),
    symbol!("μ", "mu", ["greek", "micro"]),
    symbol!("Ω", "omega", ["greek", "ohm"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_searches_updates_and_deletes_all_item_kinds() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut store =
            ProductivityStore::open(directory.path().join("items.sqlite")).expect("open");
        let mut snippet = Item::new("Standup", ItemContent::Snippet("## Today".into()), 1);
        snippet.keyword = Some("!standup".into());
        snippet.tags = vec!["work".into(), "markdown".into()];
        snippet.favorite = true;
        store.save(&snippet).expect("save snippet");
        let note = Item::new(
            "Launch notes",
            ItemContent::Note("Ship Superspace".into()),
            2,
        );
        store.save(&note).expect("save note");

        assert_eq!(
            store.by_keyword("!standup").expect("keyword"),
            Some(snippet.clone())
        );
        assert_eq!(
            store.search("mark", 10).expect("search"),
            vec![snippet.clone()]
        );
        assert_eq!(store.search("", 10).expect("all")[0], snippet);
        assert!(store.delete(&note.id).expect("delete"));
        assert!(store.search("launch", 10).expect("removed").is_empty());
    }

    #[test]
    fn expansion_and_resolvers_preserve_boundaries_and_avoid_shell_parsing() {
        assert_eq!(
            expand_snippet("say !hi", "!hi", "hello"),
            Some("say hello".into())
        );
        assert_eq!(expand_snippet("say x!hi", "!hi", "hello"), None);
        assert_eq!(
            resolve_quicklink("https://example.test/?q={query}", "rust & gpui"),
            "https://example.test/?q=rust%20%26%20gpui"
        );
        let command = Item::new(
            "Echo",
            ItemContent::Command {
                executable: "printf".into(),
                args: vec!["%s".into(), "{query}".into()],
            },
            1,
        );
        assert_eq!(
            resolve_command(&command, "$(touch nope)").expect("resolve"),
            CommandInvocation {
                executable: "printf".into(),
                args: vec!["%s".into(), "$(touch nope)".into()]
            }
        );
    }

    #[test]
    fn emoji_search_uses_names_and_aliases() {
        assert_eq!(search_emoji("", 256).len(), 256);
        assert_eq!(search_emoji("ship", 5)[0].value, "🚀");
        assert_eq!(search_emoji("like", 1)[0].value, "❤️");
        assert_eq!(search_symbols("command", 1)[0].value, "⌘");
        assert!(search_symbols("math", 50).len() > 10);
    }

    #[test]
    fn duplicate_keywords_and_invalid_items_are_rejected() {
        let mut store = ProductivityStore::open(":memory:").expect("open");
        let mut first = Item::new("First", ItemContent::Snippet("one".into()), 1);
        first.keyword = Some("!same".into());
        store.save(&first).expect("first");
        let mut second = Item::new("Second", ItemContent::Snippet("two".into()), 2);
        second.keyword = Some("!same".into());
        assert!(store.save(&second).is_err());
        second.keyword = Some("has whitespace".into());
        assert!(matches!(
            store.save(&second),
            Err(ProductivityError::InvalidItem)
        ));
    }
}
