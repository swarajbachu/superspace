# Superspace

Superspace is a local-first command launcher and nearby-sharing utility for macOS and Linux. It
combines universal search, clipboard history, encrypted LAN clipboard/file transfer, productivity
tools, AI, and sandboxed WebAssembly extensions in one fast GPUI interface.

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
