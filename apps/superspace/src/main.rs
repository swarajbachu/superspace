//! Superspace command-line and desktop application entry point.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use superspace_core::{LauncherPreferences, builtin_features};
use superspace_productivity::{
    Item, ItemContent, ProductivityStore, expand_snippet, resolve_command, resolve_quicklink,
    search_emoji,
};
use superspace_storage::{BlobStore, ClipboardQuery, ClipboardStore, Retention};
use uuid::Uuid;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => superspace_ui::run(),
        Some("apps") => print_apps()?,
        Some("features") => print_features(),
        Some("launch") => {
            let id = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: superspace launch <app-id>"))?;
            if arguments.next().is_some() {
                bail!("usage: superspace launch <app-id>");
            }
            launch_app(&id)?;
        }
        Some("launcher") => launcher(arguments)?,
        Some("clipboard") => clipboard(arguments)?,
        Some("productivity") => productivity(arguments)?,
        Some("files") => files(arguments)?,
        Some("--version" | "-V") => println!("superspace {}", env!("CARGO_PKG_VERSION")),
        Some(command) => bail!("unknown command: {command}"),
    }
    Ok(())
}

fn clipboard(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments.next().unwrap_or_else(|| "history".into());
    let root = data_root();
    std::fs::create_dir_all(&root)?;
    let history_path = root.join("clipboard.sqlite");
    match action.as_str() {
        "watch" => watch_clipboard(&root, &history_path, arguments)?,
        "history" | "search" => {
            let query = arguments.collect::<Vec<_>>().join(" ");
            for entry in ClipboardStore::open(history_path)?.query(&ClipboardQuery {
                text: &query,
                kind: None,
                include_sensitive: false,
                limit: 100,
            })? {
                let content = entry.text.as_deref().unwrap_or("[binary content]");
                println!(
                    "{}\t{:?}\t{}\t{}",
                    entry.id,
                    entry.kind,
                    entry.pinned_at.is_some(),
                    content.replace(['\r', '\n'], " ")
                );
            }
        }
        "pin" | "unpin" => {
            let id = required(&mut arguments, "clipboard item id")?.parse::<Uuid>()?;
            no_more(arguments)?;
            let pinned_at = (action == "pin").then_some(now_ms());
            if !ClipboardStore::open(history_path)?.set_pinned(id, pinned_at)? {
                bail!("clipboard item not found: {id}");
            }
        }
        "remove" => {
            let id = required(&mut arguments, "clipboard item id")?.parse::<Uuid>()?;
            no_more(arguments)?;
            let store = ClipboardStore::open(history_path)?;
            let before = store.count()?;
            store.remove(id)?;
            if store.count()? == before {
                bail!("clipboard item not found: {id}");
            }
        }
        "prune" => {
            let days = required(&mut arguments, "retention days")?.parse::<u64>()?;
            no_more(arguments)?;
            let duration = days
                .checked_mul(24 * 60 * 60 * 1_000)
                .context("retention duration is too large")?;
            let cutoff = now_ms().saturating_sub(i64::try_from(duration).unwrap_or(i64::MAX));
            let removed = ClipboardStore::open(history_path)?.prune(Retention::Before(cutoff))?;
            println!("removed {removed} clipboard items");
        }
        _ => bail!(
            "usage: superspace clipboard <watch|history|search|pin|unpin|remove|prune> [arguments]"
        ),
    }
    Ok(())
}

fn watch_clipboard(
    root: &Path,
    history_path: &Path,
    mut arguments: impl Iterator<Item = String>,
) -> Result<()> {
    let once = match arguments.next().as_deref() {
        None => false,
        Some("--once") => true,
        Some(_) => bail!("usage: superspace clipboard watch [--once]"),
    };
    no_more(arguments)?;
    let device_id = installation_id(root)?;
    let backend = superspace_platform::NativeClipboard::connect()?;
    let history = ClipboardStore::open(history_path)?;
    let blobs = BlobStore::open(root.join("clipboard-blobs"))?;
    let mut sync =
        superspace_sync::ClipboardSync::new(device_id, now_u64(), backend, history, blobs);
    loop {
        let now = now_u64();
        if let Some(event) = sync.poll_local(now, [], i64::MAX)? {
            println!("captured {} {:?}", event.id, event.format);
        }
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn installation_id(root: &Path) -> Result<Uuid> {
    let path = root.join("device-id");
    match std::fs::read_to_string(&path) {
        Ok(value) => return Ok(value.trim().parse()?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let id = Uuid::new_v4();
    std::fs::write(path, id.to_string())?;
    Ok(id)
}

fn launcher(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments
        .next()
        .context("usage: superspace launcher <alias|favorite|show> <item-id> [alias]")?;
    let id = required(&mut arguments, "item id")?;
    let path = data_root().join("launcher.json");
    let mut preferences = LauncherPreferences::load(&path)?;
    match action.as_str() {
        "alias" => {
            let alias = arguments.collect::<Vec<_>>().join(" ");
            if alias.is_empty() {
                bail!("usage: superspace launcher alias <item-id> <alias|--clear>");
            }
            preferences.set_alias(&id, (alias != "--clear").then_some(alias.as_str()))?;
            preferences.save(path)?;
            println!("updated alias for {id}");
        }
        "favorite" => {
            no_more(arguments)?;
            let favorite = preferences.toggle_favorite(&id)?;
            preferences.save(path)?;
            println!("{}", if favorite { "favorite" } else { "not favorite" });
        }
        "show" => {
            no_more(arguments)?;
            let preference = preferences.get(&id);
            println!("{}", serde_json::to_string_pretty(&preference)?);
        }
        _ => bail!("usage: superspace launcher <alias|favorite|show> <item-id> [alias]"),
    }
    Ok(())
}

fn files(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments
        .next()
        .context("usage: superspace files <index|search|preview|share-path> [arguments]")?;
    match action.as_str() {
        "index" => {
            let root = required(&mut arguments, "scope root")?;
            let scope = superspace_files::SearchScope {
                root: PathBuf::from(root),
                include_hidden: false,
                ignores: arguments.collect(),
            };
            let mut index = open_file_index()?;
            let report = index.refresh(&scope, &AtomicBool::new(false))?;
            println!(
                "scanned={} updated={} removed={} skipped={}",
                report.scanned, report.updated, report.removed, report.skipped
            );
        }
        "search" => {
            let query = arguments.collect::<Vec<_>>().join(" ");
            if query.is_empty() {
                bail!("usage: superspace files search <query>");
            }
            for result in open_file_index()?.search(&query, None, 100)? {
                println!("{}\t{}", result.size, result.path.display());
            }
        }
        "preview" => {
            let path = required(&mut arguments, "file path")?;
            no_more(arguments)?;
            match superspace_files::preview(path)? {
                superspace_files::FilePreview::Text { content, truncated } => {
                    print!("{content}");
                    if truncated {
                        eprintln!("\n[preview truncated]");
                    }
                }
                superspace_files::FilePreview::Binary { size } => {
                    println!("binary file ({size} bytes)");
                }
            }
        }
        "share-path" => {
            let path = required(&mut arguments, "file path")?;
            no_more(arguments)?;
            println!("{}", superspace_files::action_path(path)?.display());
        }
        _ => bail!("usage: superspace files <index|search|preview|share-path> [arguments]"),
    }
    Ok(())
}

fn open_file_index() -> Result<superspace_files::FileIndex> {
    let database = data_root().join("files.sqlite");
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    superspace_files::FileIndex::open(database).map_err(Into::into)
}

fn productivity(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments.next().unwrap_or_else(|| "list".into());
    let database = productivity_database();
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut store = ProductivityStore::open(database)?;
    match action.as_str() {
        "list" | "search" => {
            let query = arguments.collect::<Vec<_>>().join(" ");
            for item in store.search(&query, 100)? {
                println!("{}", serde_json::to_string(&item)?);
            }
        }
        "add-quicklink" => {
            let title = required(&mut arguments, "title")?;
            let template = required(&mut arguments, "URL template")?;
            let keyword = arguments.next();
            no_more(arguments)?;
            let item = item_with_keyword(title, ItemContent::Quicklink(template), keyword);
            save_item(&mut store, &item)?;
        }
        "add-snippet" => {
            let title = required(&mut arguments, "title")?;
            let markdown = required(&mut arguments, "Markdown body")?;
            let keyword = arguments.next();
            no_more(arguments)?;
            let item = item_with_keyword(title, ItemContent::Snippet(markdown), keyword);
            save_item(&mut store, &item)?;
        }
        "add-note" => {
            let title = required(&mut arguments, "title")?;
            let markdown = required(&mut arguments, "Markdown body")?;
            no_more(arguments)?;
            let item = Item::new(title, ItemContent::Note(markdown), now_ms());
            save_item(&mut store, &item)?;
        }
        "add-command" => {
            let title = required(&mut arguments, "title")?;
            let executable = required(&mut arguments, "executable")?;
            let args = arguments.collect();
            let item = Item::new(title, ItemContent::Command { executable, args }, now_ms());
            save_item(&mut store, &item)?;
        }
        "delete" => {
            let id = required(&mut arguments, "item id")?;
            no_more(arguments)?;
            if !store.delete(&id)? {
                bail!("productivity item not found: {id}");
            }
        }
        "resolve" => {
            let id = required(&mut arguments, "item id")?;
            let query = arguments.collect::<Vec<_>>().join(" ");
            resolve_productivity(&find_item(&store, &id)?, &query)?;
        }
        "expand" => {
            let keyword = required(&mut arguments, "keyword")?;
            let input = arguments.collect::<Vec<_>>().join(" ");
            let item = store
                .by_keyword(&keyword)?
                .with_context(|| format!("keyword not found: {keyword}"))?;
            let ItemContent::Snippet(body) = item.content else {
                bail!("keyword does not belong to a snippet");
            };
            println!(
                "{}",
                expand_snippet(&input, &keyword, &body).unwrap_or(input)
            );
        }
        "run" => {
            let id = required(&mut arguments, "item id")?;
            let confirmation = required(&mut arguments, "--execute confirmation")?;
            if confirmation != "--execute" {
                bail!("refusing to run without the explicit `--execute` argument");
            }
            let query = arguments.collect::<Vec<_>>().join(" ");
            let item = find_item(&store, &id)?;
            let invocation = resolve_command(&item, &query)?;
            let status = Command::new(&invocation.executable)
                .args(&invocation.args)
                .status()
                .with_context(|| format!("failed to execute {}", invocation.executable))?;
            if !status.success() {
                bail!("command exited with {status}");
            }
        }
        "emoji" => {
            let query = arguments.collect::<Vec<_>>().join(" ");
            for emoji in search_emoji(&query, 100) {
                println!("{}\t{}", emoji.value, emoji.name);
            }
        }
        _ => bail!(
            "usage: superspace productivity <list|search|add-quicklink|add-snippet|add-note|add-command|delete|resolve|expand|run|emoji> [arguments]"
        ),
    }
    Ok(())
}

fn resolve_productivity(item: &Item, query: &str) -> Result<()> {
    match &item.content {
        ItemContent::Quicklink(template) => {
            println!("{}", resolve_quicklink(template, query));
        }
        ItemContent::Snippet(body) | ItemContent::Note(body) => println!("{body}"),
        ItemContent::Command { .. } => bail!("use `run` to invoke a command"),
    }
    Ok(())
}

fn save_item(store: &mut ProductivityStore, item: &Item) -> Result<()> {
    store.save(item)?;
    println!("{}", item.id);
    Ok(())
}

fn item_with_keyword(title: String, content: ItemContent, keyword: Option<String>) -> Item {
    let mut item = Item::new(title, content, now_ms());
    item.keyword = keyword;
    item
}

fn find_item(store: &ProductivityStore, id: &str) -> Result<Item> {
    store
        .get(id)?
        .with_context(|| format!("productivity item not found: {id}"))
}

fn required(arguments: &mut impl Iterator<Item = String>, label: &str) -> Result<String> {
    arguments.next().with_context(|| format!("missing {label}"))
}

fn no_more(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    if arguments.next().is_some() {
        bail!("too many arguments");
    }
    Ok(())
}

fn productivity_database() -> PathBuf {
    data_root().join("productivity.sqlite")
}

fn data_root() -> PathBuf {
    std::env::var_os("SUPERSPACE_DATA_DIR").map_or_else(default_data_root, PathBuf::from)
}

fn default_data_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map_or_else(
            || PathBuf::from("Superspace"),
            |home| Path::new(&home).join("Library/Application Support/Superspace"),
        )
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(
            || {
                std::env::var_os("HOME").map_or_else(
                    || PathBuf::from(".local/share/superspace"),
                    |home| Path::new(&home).join(".local/share/superspace"),
                )
            },
            |root| Path::new(&root).join("superspace"),
        )
    }
}

fn now_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

fn now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn print_apps() -> Result<()> {
    for application in
        superspace_platform::discover_apps(&superspace_platform::default_app_roots())?
    {
        println!("{}\t{}", application.id, application.name);
    }
    Ok(())
}

fn launch_app(id: &str) -> Result<()> {
    let application =
        superspace_platform::discover_apps(&superspace_platform::default_app_roots())?
            .into_iter()
            .find(|application| application.id == id)
            .ok_or_else(|| anyhow::anyhow!("application not found: {id}"))?;
    let process_id = application.launch()?;
    println!("launched {} ({process_id})", application.name);
    Ok(())
}

fn print_features() {
    for feature in builtin_features() {
        println!("{:?}\t{}\t{}", feature.area, feature.id, feature.title);
    }
}
