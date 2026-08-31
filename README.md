# Superspace

Superspace is a local-first command launcher and nearby-sharing utility for macOS and Linux. It
combines universal search, clipboard history, encrypted LAN clipboard/file transfer, productivity
tools, and sandboxed WebAssembly extensions in one fast GPUI interface. The optional AI layer is
currently deferred.

The project is under active development. The implementation contract is tracked in
[`docs/features.md`](docs/features.md), and architectural boundaries are described in
[`docs/architecture.md`](docs/architecture.md).

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

## Try encrypted cross-device clipboard

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
Use `nearby revoke`, `nearby enable`, or `nearby forget` to manage trust. Automatic discovery and
file-transfer dispatch are still under active development; the feature checklist deliberately
leaves those broader product requirements unchecked until they are integrated and verified.

On Ubuntu, the GPUI desktop build requires:

```sh
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-dev libxcb1-dev libx11-xcb-dev libfontconfig1-dev libfreetype-dev \
  libasound2-dev libvulkan-dev pkg-config cmake
```

## Inspiration

Superspace is an original implementation inspired by the workflows of Tinycast and Raycast and by
the GPUI interaction craft demonstrated in Comet/Zeron. Their code, assets, product names, and copy
are not part of Superspace.

## License

[MIT](LICENSE)
