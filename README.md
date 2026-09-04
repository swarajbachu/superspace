# Superspace

Superspace is a local-first command launcher and nearby-sharing utility for macOS and Linux. It
combines universal search, clipboard history, encrypted LAN clipboard/file transfer, productivity
tools, and sandboxed WebAssembly extensions in one fast GPUI interface. The optional AI layer is
currently deferred.

The project is under active development. The implementation contract is tracked in
[`docs/features.md`](docs/features.md), and architectural boundaries are described in
[`docs/architecture.md`](docs/architecture.md).

For moving development to another machine, see [`HANDOFF.md`](HANDOFF.md). It records the exact
verified state, remaining work, environment setup, and next implementation sequence.

## Principles

- Local-first and account-free: LAN sharing needs no cloud relay.
- Secure by default: paired identities, encrypted transport, explicit extension capabilities.
- Cross-platform honestly: platform gaps are surfaced instead of silently ignored.
- Keyboard-first, accessible, and reduced-motion-aware.
- No telemetry.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo run -p superspace -- features
cargo run -p superspace --features desktop
```

## Nearby sharing in the desktop app

Open **Nearby Sharing** from the Superspace palette on both computers. Superspace advertises and
browses `_superspace._tcp.local` while the app is open, with a bounded UDP broadcast fallback on
port `43869` for LANs that filter multicast. Computers on the same subnet appear automatically. On
one computer choose **Pair this computer**; on the discovered row on
the other choose **Pair**, then approve the identical six-digit code on both screens.

Paired rows provide clipboard receive/connect, file/folder sending through the native picker,
one-shot file receiving, pause/enable, and forget controls. A typed IP remains available as a
fallback for networks that block multicast DNS. Transfers remain authenticated through the paired
identity and pinned QUIC certificate.

## Diagnostic CLI workflow

Build the CLI on both machines, then inspect each installation ID:

```sh
cargo run -p superspace -- nearby identity
```

Pair once over the local network. On the receiving machine:

```sh
cargo run -p superspace -- nearby pair-listen 0.0.0.0:43870 "My Linux PC"
```

On the other machine, substitute the receiver's LAN address:

```sh
cargo run -p superspace -- nearby pair-connect 192.168.1.20:43870 "My Mac"
```

Both machines show a six-digit code. Check that the codes match and type `yes` on each. Superspace
stores the authenticated peer certificate and Noise public key only after both confirmations. List
the resulting peer IDs with `superspace nearby trusted`.

Start the bidirectional pinned-QUIC clipboard session. On the first machine, use the second
machine's peer ID:

```sh
cargo run -p superspace -- nearby clipboard-listen 0.0.0.0:43871 <SECOND_PEER_ID> "My Linux PC"
```

Then connect from the second machine using the first machine's peer ID and LAN address:

```sh
cargo run -p superspace -- nearby clipboard-connect 192.168.1.20:43871 <FIRST_PEER_ID> "My Mac"
```

Text and images now synchronize in both directions with history persistence, loop suppression,
conflict ordering, resumable large-blob transfer, and acknowledgement after durable application.
Use `nearby revoke`, `nearby enable`, or `nearby forget` to manage trust.

To send a file or recursively send a non-empty folder, start a one-shot receiver on the destination:

```sh
cargo run -p superspace -- nearby file-listen 0.0.0.0:43872 <SENDER_PEER_ID> "My Linux PC"
```

Then send from the other machine using the receiver's address and peer ID:

```sh
cargo run -p superspace -- nearby file-send 192.168.1.20:43872 <RECEIVER_PEER_ID> \
  "/path/to/file-or-folder" "My Mac"
```

Incoming content is integrity-checked and published under the Superspace data directory's
`incoming/` folder without overwriting an existing destination. The receiver also checks that the
manifest origin is the authenticated paired peer; only a verified published destination can satisfy
a matching file clipboard event. Native file-list clipboard adapters, automatic reconnection, and
structured transfer progress remain under active development.

On Ubuntu, the GPUI desktop build requires:

```sh
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-dev libxcb1-dev libx11-xcb-dev libfontconfig1-dev libfreetype-dev \
  libasound2-dev libvulkan-dev pkg-config cmake
```

## References and provenance

Superspace is an original implementation. External projects were used as product, interaction, or
framework references; their code, assets, names, and copy are not included in this repository.

- [Tinycast](https://github.com/abue-ammar/tinycast) — launcher workflow and feature-reference
  baseline. Audited at `f5fa11f990e90c766301b617822afa725d7e9809`; licensed AGPL-3.0. No Tinycast
  source was copied into this MIT project.
- [Comet, now Zeron](https://github.com/zeronsh/comet) — GPUI interaction, motion, and visual-quality
  reference supplied by the project owner. Audited at
  `b3fa51872f70c8f973c241b659cf0c166766f4f5`; licensed MIT. No branding or assets were copied.
- [Raycast](https://www.raycast.com/) — product/workflow reference used to define the compatibility
  feature catalog. Superspace is not affiliated with Raycast.
- [wingleeio/zed](https://github.com/wingleeio/zed) — source of the actual `gpui` and
  `gpui_platform` dependencies, pinned in `Cargo.toml` at
  `e2ddcc6805f8c5088e62a60dfe517abcccd61a9a`. Those crates declare Apache-2.0.

## License

[MIT](LICENSE)
