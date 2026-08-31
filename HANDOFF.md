# Superspace development handoff

Last verified: 2026-08-31 on branch `main`.

This document is the restart point for continuing Superspace on another machine. The authoritative
definition of product completion remains [`docs/features.md`](docs/features.md); unchecked items must
not be described as complete merely because supporting types or partial implementations exist.

## Repository

- Public repository: <https://github.com/swarajbachu/superspace>
- License: MIT
- Primary executable: `apps/superspace`
- Extension developer executable: `apps/superspace-extension`
- Language/toolchain: Rust 1.98 via `rust-toolchain.toml`
- UI: GPUI/GPUI Platform pinned to `e2ddcc6805f8c5088e62a60dfe517abcccd61a9a`

Clone and verify the portable, headless workspace:

```sh
git clone https://github.com/swarajbachu/superspace.git
cd superspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

On Linux, install the native packages listed in [`README.md`](README.md), then verify GPUI:

```sh
RUST_FONTCONFIG_DLOPEN=1 cargo check -p superspace --features desktop
RUST_FONTCONFIG_DLOPEN=1 cargo clippy -p superspace --all-targets --features desktop -- -D warnings
```

On macOS, install Xcode Command Line Tools and run the same two commands without
`RUST_FONTCONFIG_DLOPEN`. A real macOS build and physical Mac/Linux integration test are still
required before release.

## Honest completion estimate

Approximately 35–45% of the full production objective is complete. The low-level foundation is much
further along than the user-facing product: protocol, persistence, calculator, extension sandbox,
search models, and the first GPUI shell exist, while lifecycle integration, automatic nearby
operation, platform actions, packaging, and release proof remain substantial.

Roughly 55–65% remains if AI continues to be deferred. Against the original objective including the
full AI feature set, roughly 65–75% remains. These are engineering estimates, not checklist claims;
the feature contract currently marks only the two fully verified calculator lines complete.

## Implemented and verified foundation

- MIT Rust workspace with strict Clippy/pedantic gates and a focused, incremental commit history.
- GPUI floating palette with keyboard and pointer navigation, previews, action menus, three themes,
  centralized motion, native app launch, and indexed file results.
- Deterministic fuzzy ranking plus persistent aliases, favorites, and invocation frequency.
- Incremental SQLite FTS file indexing with scopes, ignores, bounded previews, and cancellation.
- Searchable SQLite clipboard history for text, HTML/RTF metadata, PNG images, file metadata, pins,
  retention, source attribution, sensitive markers, excluded-app policy, and loop suppression.
- Stable owner-only local device identity, Noise XX pairing with a mutually verified six-digit code,
  trusted-device enable/revoke/forget lifecycle, and pinned mutual TLS 1.3 over QUIC.
- Manual bidirectional paired clipboard runtime for text and images, including deterministic conflict
  ordering, acknowledgement, offline ledger behavior, content-addressed blobs, resume, and integrity
  verification.
- Transfer protocol with safe relative paths, staging, disk preflight, collision handling, BLAKE3
  verification, resume, progress, and cooperative cancellation. Deterministic outbound manifests
  recursively hash regular files while rejecting symlinks, empty folders, and non-portable paths.
  One-shot `file-listen`/`file-send` commands route files and folders through paired, certificate-
  pinned sessions and publish single files without an unnecessary wrapper directory. Published
  destinations carry a private, integrity-verified transfer proof; file clipboard events can only
  consume a proof with the same transfer ID and origin, and session receivers reject manifests
  whose origin differs from the authenticated peer.
- Full calculator engine for arithmetic, scientific functions, bases, percentages, ratios, lists,
  units, dates, workdays, timespans, time zones, fiat, and crypto conversion.
- Persistent quicklinks, Markdown snippets, keyword expansion, notes, explicit custom commands, and
  emoji search foundations plus CLI workflows; GPUI integration remains incomplete.
- Versioned WIT contract, `.superspace-extension` packaging, Wasmtime resource limits and deny-by-
  default capabilities, declarative view validation, developer CLI workflow, and signed registry
  foundations. In-app extension browsing/rendering and full host grants remain incomplete.
- AI provider models and streaming decoders exist as an early foundation, but the user explicitly
  deferred all AI-layer work. Do not resume it until requested.

## Current manual cross-device workflow

The exact pairing and clipboard commands are in [`README.md`](README.md). In short:

1. Run `superspace nearby identity` on both devices.
2. Run `pair-listen` on one and `pair-connect` on the other.
3. Confirm the identical six-digit code on both devices.
4. Obtain peer IDs with `superspace nearby trusted`.
5. Run `clipboard-listen` and `clipboard-connect` using the paired IDs, or use `file-listen` on the
   destination followed by `file-send` on the source. Exact commands are in the README.

The file workflow was integration-tested on 2026-08-31 between two isolated paired data roots over
loopback: a nested two-file folder transferred, acknowledged, and matched the source contents. A
physical Mac/Linux LAN run is still required.

This is deliberately a manual diagnostic workflow. Production behavior must start discovery and sync
inside the desktop application, reconnect automatically, restore pending events after process
restart, and expose status/errors in GPUI.

## Best next implementation sequence

1. Replace manual listener/connector roles with one mDNS-driven service. Resolve trusted device IDs,
   deduplicate simultaneous connections deterministically, reconnect with bounded backoff, touch
   `last_seen_at`, and preserve the offline queue across process restarts.
2. Move the long-running nearby and clipboard services into the desktop composition root. GPUI must
   receive immutable status/progress events; it must not perform blocking SQLite/network work.
3. Build clipboard-history and Nearby GPUI surfaces, including drag/drop, device picker, progress,
   cancellation, retry, trust management, privacy controls, and transfer notifications.
4. Add native macOS and Linux file-list clipboard adapters, then retain and pair incoming clipboard
   offers with their matching published transfer proof in the session dispatcher.
5. Complete launcher lifecycle: configurable global/per-command/per-app shortcuts, tray/menu-bar,
   hide/show behavior, launch at login, onboarding, permissions diagnostics, and updater.
6. Complete built-in platform actions and productivity UI, then backup/restore and Raycast import.
7. Finish extension host capabilities and in-app registry/browser.
8. Add CI, dependency/license/security audits, threat model, release signing, macOS universal app/DMG
   and Homebrew cask, plus AppImage/deb/rpm/Arch packaging.
9. Run physical-device macOS/Linux pairing, clipboard, image, sleep/reconnect, Wi-Fi change, large
    file, cancellation, resume, disk-full, and revocation suites. Only then update broad checklist
    lines in `docs/features.md`.

AI remains intentionally outside this sequence until the user re-enables it.

## Known gaps and cautions

- There is no automatic daemon/tray lifecycle yet; nearby clipboard sessions are foreground CLI
  processes with manually supplied addresses.
- File/folder transfer is currently a one-shot foreground CLI workflow. There is no GPUI send flow,
  native file-list clipboard application, pending offer/transfer rendezvous, background queue, or
  automatic retry yet. The sync boundary is ready for verified destinations, but the native backend
  deliberately returns unsupported rather than degrading file lists to text.
- Clipboard replication queues are memory-resident; history/trust are durable, but queued per-peer
  acknowledgements must be persisted for real restart reconciliation.
- Native clipboard support currently reads/writes text and images through `arboard`; rich native
  formats and file-list clipboard semantics require dedicated macOS/X11/Wayland adapters.
- The GPUI shell performs some discovery/search work synchronously during construction. Move this to
  background services before claiming the architecture's non-blocking-UI invariant.
- Linux desktop compilation works with the packages in the README. This container cannot perform the
  final native link without host `xkbcommon`, `xkbcommon-x11`, and XCB libraries.
- macOS has not been physically verified. Cross-target compilation alone is not release evidence.
- No CI, installers, signing/notarization, update feed, visual regression suite, property tests,
  dependency audit policy, secret scanner, or release automation exists yet.
- `docs/architecture.md` describes the intended credential-store boundary. The current peer identity
  is an owner-only `0600` file; moving private keys into Keychain/Secret Service remains release work.

## Data locations

`SUPERSPACE_DATA_DIR` overrides all data paths and is useful for tests. Defaults are:

- macOS: `~/Library/Application Support/Superspace`
- Linux: `$XDG_DATA_HOME/superspace` or `~/.local/share/superspace`

Important files include `local-identity.cbor` (required mode `0600`), `trusted-devices.sqlite`,
`clipboard.sqlite`, `clipboard-blobs/`, `files.sqlite`, `productivity.sqlite`, and `launcher.json`.
Never print private identity bytes, commit a data directory, or silently rotate a corrupt identity.

## External references and provenance

- Tinycast: <https://github.com/abue-ammar/tinycast>, audited at
  `f5fa11f990e90c766301b617822afa725d7e9809`, AGPL-3.0. Product/feature reference only; no source or
  assets copied.
- Comet/Zeron: <https://github.com/zeronsh/comet>, audited at
  `b3fa51872f70c8f973c241b659cf0c166766f4f5`, MIT. Interaction/motion reference only.
- Raycast: <https://www.raycast.com/>, product compatibility reference; no affiliation.
- GPUI source: <https://github.com/wingleeio/zed>, pinned at
  `e2ddcc6805f8c5088e62a60dfe517abcccd61a9a`; `gpui` and `gpui_platform` declare Apache-2.0.

Local reusable reference checkouts on the original machine were kept outside the project at
`~/.zuse/reference-repos/abue-ammar__tinycast` and
`~/.zuse/reference-repos/zeronsh__comet`. They are not required to build Superspace.
