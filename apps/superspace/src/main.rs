//! Superspace command-line and desktop application entry point.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{future::Future, net::IpAddr};

use anyhow::{Context as _, Result, bail};
use fs2::FileExt as _;
use superspace_core::{LauncherPreferences, builtin_features};
use superspace_emoji::search as search_emoji;
use superspace_productivity::{
    Item, ItemContent, ProductivityStore, expand_snippet, resolve_command, resolve_quicklink,
};
use superspace_storage::{
    BlobStore, ClipboardQuery, ClipboardStore, Retention, TrustedDevice, TrustedDeviceStore,
};
use uuid::Uuid;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => {
            ensure_clipboard_watcher()?;
            superspace_ui::run();
        }
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
        Some("nearby") => nearby(arguments)?,
        Some("productivity") => productivity(arguments)?,
        Some("files") => files(arguments)?,
        Some("--version" | "-V") => println!("superspace {}", env!("CARGO_PKG_VERSION")),
        Some(command) => bail!("unknown command: {command}"),
    }
    Ok(())
}

fn nearby(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments.next().unwrap_or_else(|| "trusted".into());
    let root = data_root();
    std::fs::create_dir_all(&root)?;
    match action.as_str() {
        "identity" => {
            no_more(arguments)?;
            let identity = superspace_network::LocalIdentity::load_or_create(
                root.join("local-identity.cbor"),
            )?;
            println!("device={}", identity.device_id);
            println!("certificate={}", identity.transport.fingerprint());
        }
        "trusted" => {
            no_more(arguments)?;
            for device in
                superspace_storage::TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?
                    .list()?
            {
                println!(
                    "{}\t{}\t{}\t{}",
                    device.id,
                    if device.enabled { "enabled" } else { "revoked" },
                    device
                        .last_seen_at
                        .map_or_else(|| "never".into(), |value| value.to_string()),
                    device.name
                );
            }
        }
        "enable" | "revoke" => {
            let id = required(&mut arguments, "device id")?.parse::<Uuid>()?;
            no_more(arguments)?;
            let enabled = action == "enable";
            if !superspace_storage::TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?
                .set_enabled(id, enabled)?
            {
                bail!("trusted device not found: {id}");
            }
        }
        "forget" => {
            let id = required(&mut arguments, "device id")?.parse::<Uuid>()?;
            no_more(arguments)?;
            if !superspace_storage::TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?
                .remove(id)?
            {
                bail!("trusted device not found: {id}");
            }
        }
        "pair-listen" | "pair-connect" => {
            let address = required(&mut arguments, "socket address")?.parse::<SocketAddr>()?;
            let name = arguments.collect::<Vec<_>>().join(" ");
            if name.trim().is_empty() {
                bail!("usage: superspace nearby {action} <ip:port> <device-name>");
            }
            pair_device(&root, action == "pair-listen", address, name)?;
        }
        "clipboard-listen" | "clipboard-connect" => {
            let address = required(&mut arguments, "socket address")?.parse::<SocketAddr>()?;
            let peer_id = required(&mut arguments, "trusted device id")?.parse::<Uuid>()?;
            let name = arguments.collect::<Vec<_>>().join(" ");
            if name.trim().is_empty() {
                bail!("usage: superspace nearby {action} <ip:port> <peer-id> <device-name>");
            }
            run_peer_clipboard(&root, action == "clipboard-listen", address, peer_id, name)?;
        }
        "file-listen" => {
            let address = required(&mut arguments, "socket address")?.parse::<SocketAddr>()?;
            let peer_id = required(&mut arguments, "trusted device id")?.parse::<Uuid>()?;
            let name = arguments.collect::<Vec<_>>().join(" ");
            if name.trim().is_empty() {
                bail!("usage: superspace nearby file-listen <ip:port> <peer-id> <device-name>");
            }
            run_peer_file(&root, true, address, peer_id, name, None)?;
        }
        "file-send" => {
            let address = required(&mut arguments, "socket address")?.parse::<SocketAddr>()?;
            let peer_id = required(&mut arguments, "trusted device id")?.parse::<Uuid>()?;
            let path = PathBuf::from(required(&mut arguments, "file or folder path")?);
            let name = arguments.collect::<Vec<_>>().join(" ");
            if name.trim().is_empty() {
                bail!(
                    "usage: superspace nearby file-send <ip:port> <peer-id> <path> <device-name>"
                );
            }
            run_peer_file(&root, false, address, peer_id, name, Some(&path))?;
        }
        _ => bail!(
            "usage: superspace nearby <identity|trusted|enable|revoke|forget|pair-listen|pair-connect|clipboard-listen|clipboard-connect|file-listen|file-send> [arguments]"
        ),
    }
    Ok(())
}

fn run_peer_file(
    root: &Path,
    listen: bool,
    address: SocketAddr,
    peer_id: Uuid,
    name: String,
    source: Option<&Path>,
) -> Result<()> {
    let identity =
        superspace_network::LocalIdentity::load_or_create(root.join("local-identity.cbor"))?;
    let trust = TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?;
    let peer = trust
        .get(peer_id)?
        .filter(|peer| peer.enabled)
        .with_context(|| format!("trusted device is missing or revoked: {peer_id}"))?;
    let peer_certificate =
        superspace_network::PeerCertificate::from_der(peer.certificate_der.clone())?;
    let prepared = source
        .map(|path| superspace_network::prepare_transfer(path, identity.device_id))
        .transpose()?;
    let bind_ip = if listen {
        address.ip()
    } else if address.is_ipv6() {
        IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    };
    tokio::runtime::Runtime::new()?.block_on(async {
        let endpoint = superspace_network::QuicEndpoint::bind(
            SocketAddr::new(bind_ip, if listen { address.port() } else { 0 }),
            identity.transport.clone(),
            std::slice::from_ref(&peer_certificate),
        )?;
        let local_info = superspace_protocol::DeviceInfo {
            id: identity.device_id,
            name,
            platform: std::env::consts::OS.into(),
            protocol_versions: vec![superspace_protocol::PROTOCOL_VERSION],
        };
        let connection = if listen {
            println!(
                "waiting for a file from {} on {}",
                peer.name,
                endpoint.local_addr()?
            );
            let connection = endpoint.accept().await?;
            superspace_network::exchange_hello_incoming(&connection, &local_info, peer_id).await?;
            connection
        } else {
            let connection = endpoint.connect(address, &peer_certificate).await?;
            superspace_network::exchange_hello_outgoing(&connection, &local_info, peer_id).await?;
            connection
        };
        let cancellation = superspace_network::TransferCancellation::new();
        if let Some(prepared) = prepared {
            let display_name = prepared.manifest().name.clone();
            superspace_network::send_transfer_with_progress(
                &connection,
                prepared.source_root(),
                prepared.manifest(),
                &cancellation,
                |progress| print_transfer_progress("sending", &display_name, &progress),
            )
            .await?;
            println!("sent {display_name}");
        } else {
            let destination = superspace_network::receive_transfer_with_progress(
                &connection,
                peer_id,
                root.join("incoming"),
                &cancellation,
                |progress| print_transfer_progress("receiving", "transfer", &progress),
            )
            .await?;
            println!("received {}", destination.destination().display());
        }
        Result::<()>::Ok(())
    })
}

fn print_transfer_progress(
    operation: &str,
    name: &str,
    progress: &superspace_network::TransferProgress,
) {
    eprintln!(
        "{operation} {name}: {}/{} bytes ({}/{})",
        progress.completed_bytes,
        progress.total_bytes,
        progress.completed_files,
        progress.total_files
    );
}

fn run_peer_clipboard(
    root: &Path,
    listen: bool,
    address: SocketAddr,
    peer_id: Uuid,
    name: String,
) -> Result<()> {
    let identity =
        superspace_network::LocalIdentity::load_or_create(root.join("local-identity.cbor"))?;
    let trust = TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?;
    let peer = trust
        .get(peer_id)?
        .filter(|peer| peer.enabled)
        .with_context(|| format!("trusted device is missing or revoked: {peer_id}"))?;
    let peer_certificate =
        superspace_network::PeerCertificate::from_der(peer.certificate_der.clone())?;
    let bind_ip = if listen {
        address.ip()
    } else if address.is_ipv6() {
        IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    };
    let endpoint = superspace_network::QuicEndpoint::bind(
        SocketAddr::new(bind_ip, if listen { address.port() } else { 0 }),
        identity.transport.clone(),
        std::slice::from_ref(&peer_certificate),
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let local_info = superspace_protocol::DeviceInfo {
            id: identity.device_id,
            name,
            platform: std::env::consts::OS.into(),
            protocol_versions: vec![superspace_protocol::PROTOCOL_VERSION],
        };
        let connection = if listen {
            println!("waiting for {} on {}", peer.name, endpoint.local_addr()?);
            let connection = endpoint.accept().await?;
            superspace_network::exchange_hello_incoming(&connection, &local_info, peer_id).await?;
            connection
        } else {
            let connection = endpoint.connect(address, &peer_certificate).await?;
            superspace_network::exchange_hello_outgoing(&connection, &local_info, peer_id).await?;
            connection
        };
        println!("encrypted clipboard session active with {}", peer.name);
        clipboard_connection_loop(&connection, root, identity.device_id, peer_id).await
    })
}

type ClipboardOfferFuture =
    Pin<Box<dyn Future<Output = (Uuid, Result<(), superspace_network::PeerSessionError>)>>>;

async fn clipboard_connection_loop(
    connection: &quinn::Connection,
    root: &Path,
    local_id: Uuid,
    peer_id: Uuid,
) -> Result<()> {
    let backend = superspace_platform::NativeClipboard::connect()?;
    let history = ClipboardStore::open(root.join("clipboard.sqlite"))?;
    let blobs = BlobStore::open(root.join("clipboard-blobs"))?;
    let mut sync =
        superspace_sync::ClipboardSync::new(local_id, now_u64(), backend, history, blobs);
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut outgoing: Option<ClipboardOfferFuture> = None;
    loop {
        if let Some(active) = outgoing.as_mut() {
            tokio::select! {
                (event_id, result) = active => {
                    result?;
                    if !sync.acknowledge(peer_id, event_id) {
                        bail!("clipboard acknowledgement did not match the pending peer event");
                    }
                    outgoing = None;
                }
                request = superspace_network::receive_peer_request(connection) => {
                    handle_peer_request(&mut sync, connection, root, peer_id, request?, now_u64()).await?;
                }
                _ = interval.tick() => {}
            }
        } else {
            tokio::select! {
                request = superspace_network::receive_peer_request(connection) => {
                    handle_peer_request(&mut sync, connection, root, peer_id, request?, now_u64()).await?;
                }
                _ = interval.tick() => {
                    let now = now_u64();
                    let expires = i64::try_from(now)
                        .unwrap_or(i64::MAX)
                        .saturating_add(7 * 24 * 60 * 60 * 1_000);
                    if let Some(event) = sync.poll_local(now, [peer_id], expires)? {
                        let connection = connection.clone();
                        let event_id = event.id;
                        outgoing = Some(Box::pin(async move {
                            let result = superspace_network::offer_clipboard(&connection, &event).await;
                            (event_id, result)
                        }));
                    }
                }
            }
        }
    }
}

async fn handle_peer_request(
    sync: &mut superspace_sync::ClipboardSync<superspace_platform::NativeClipboard>,
    connection: &quinn::Connection,
    root: &Path,
    peer_id: Uuid,
    request: superspace_network::IncomingPeerRequest,
    now: u64,
) -> Result<()> {
    match request {
        superspace_network::IncomingPeerRequest::Blob(request) => {
            request.serve(root.join("clipboard-blobs")).await?;
        }
        superspace_network::IncomingPeerRequest::Transfer(request) => {
            let name = request.manifest().name.clone();
            let cancellation = superspace_network::TransferCancellation::new();
            let destination = request
                .receive_with_progress(peer_id, root.join("incoming"), &cancellation, |progress| {
                    eprintln!(
                        "receiving {name}: {}/{} bytes ({}/{})",
                        progress.completed_bytes,
                        progress.total_bytes,
                        progress.completed_files,
                        progress.total_files
                    );
                })
                .await?;
            println!("received {}", destination.destination().display());
        }
        superspace_network::IncomingPeerRequest::Clipboard(offer) => {
            let outcome = sync.receive(&offer.event, now)?;
            let outcome = match outcome {
                superspace_sync::SyncOutcome::NeedsBlob { hash, size } => {
                    let receiver = superspace_network::BlobReceiver::begin(
                        root.join("clipboard-blobs"),
                        hash,
                        size,
                    )?;
                    let path = superspace_network::request_blob(connection, receiver).await?;
                    let bytes = std::fs::read(path)?;
                    sync.receive_blob(&offer.event, &bytes, now)?
                }
                superspace_sync::SyncOutcome::NeedsTransfer { .. } => {
                    bail!("peer offered files; transfer dispatch is not active yet");
                }
                ready => ready,
            };
            if !outcome.should_acknowledge() {
                bail!("clipboard event did not reach a durable terminal state");
            }
            offer.acknowledge().await?;
        }
    }
    Ok(())
}

fn pair_device(root: &Path, listen: bool, address: SocketAddr, name: String) -> Result<()> {
    let identity =
        superspace_network::LocalIdentity::load_or_create(root.join("local-identity.cbor"))?;
    let info = superspace_network::PairingPublicInfo::for_local(&identity, name);
    let runtime = tokio::runtime::Runtime::new()?;
    let peer = runtime.block_on(async {
        let pairing = async {
            if listen {
                let listener = tokio::net::TcpListener::bind(address).await?;
                println!("waiting for a peer on {}", listener.local_addr()?);
                let (mut stream, remote) = listener.accept().await?;
                println!("pairing request from {remote}");
                superspace_network::pair_incoming(
                    &mut stream,
                    &identity,
                    &info,
                    |code| async move { confirm_pairing(code) },
                )
                .await
                .map_err(anyhow::Error::from)
            } else {
                let mut stream = tokio::net::TcpStream::connect(address).await?;
                superspace_network::pair_outgoing(
                    &mut stream,
                    &identity,
                    &info,
                    |code| async move { confirm_pairing(code) },
                )
                .await
                .map_err(anyhow::Error::from)
            }
        };
        tokio::time::timeout(Duration::from_secs(5 * 60), pairing)
            .await
            .context("pairing timed out")?
    })?;
    TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))?.upsert(&TrustedDevice {
        id: peer.info.device_id,
        name: peer.info.name.clone(),
        noise_public_key: peer.noise_public_key,
        certificate_der: peer.info.certificate_der,
        paired_at: now_ms(),
        last_seen_at: None,
        enabled: true,
    })?;
    println!("paired with {} ({})", peer.info.name, peer.info.device_id);
    Ok(())
}

fn confirm_pairing(code: superspace_network::PairingCode) -> bool {
    print!("Confirm that both devices show {code}. Type `yes`: ");
    if std::io::stdout().flush().is_err() {
        return false;
    }
    let mut response = String::new();
    std::io::stdin().read_line(&mut response).is_ok() && response.trim() == "yes"
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
    let (once, quiet) = match arguments.next().as_deref() {
        None => (false, false),
        Some("--once") => (true, false),
        Some("--service") => (false, true),
        Some(_) => bail!("usage: superspace clipboard watch [--once]"),
    };
    no_more(arguments)?;
    let Some(_watcher_lock) = clipboard_watcher_lock(root)? else {
        return Ok(());
    };
    let identity =
        superspace_network::LocalIdentity::load_or_create(root.join("local-identity.cbor"))?;
    let backend = superspace_platform::NativeClipboard::connect()?;
    let history = ClipboardStore::open(history_path)?;
    let blobs = BlobStore::open(root.join("clipboard-blobs"))?;
    let mut sync =
        superspace_sync::ClipboardSync::new(identity.device_id, now_u64(), backend, history, blobs);
    loop {
        let now = now_u64();
        if let Some(event) = sync.poll_local(now, [], i64::MAX)?
            && !quiet
        {
            println!("captured {} {:?}", event.id, event.format);
        }
        if once {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn ensure_clipboard_watcher() -> Result<()> {
    let root = data_root();
    std::fs::create_dir_all(&root)?;
    if clipboard_watcher_lock(&root)?.is_none() {
        return Ok(());
    }
    Command::new(std::env::current_exe()?)
        .args(["clipboard", "watch", "--service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start clipboard history service")?;
    Ok(())
}

fn clipboard_watcher_lock(root: &Path) -> Result<Option<File>> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("clipboard-watch.lock"))?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
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
            for emoji in search_emoji(&query) {
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

#[cfg(test)]
mod tests {
    use super::clipboard_watcher_lock;

    #[test]
    fn clipboard_watcher_lock_allows_only_one_service() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = clipboard_watcher_lock(directory.path())
            .expect("first lock")
            .expect("lock available");
        assert!(
            clipboard_watcher_lock(directory.path())
                .expect("second lock attempt")
                .is_none()
        );
        drop(first);
        assert!(
            clipboard_watcher_lock(directory.path())
                .expect("released lock")
                .is_some()
        );
    }
}
