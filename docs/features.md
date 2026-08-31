# Feature contract

This checklist defines product completion. A checked item requires implementation and automated or
documented platform verification; being represented by a type alone is not completion.

## Launcher and shell

- [ ] GPUI floating palette, search, keyboard navigation, previews, action menus, themes, and motion.
- [ ] App discovery/launch, running-state actions, aliases, favorites, ranking, and fallback search.
- [ ] Global, per-command, per-app, double-modifier, and Hyper Key shortcuts where supported.
- [ ] File search with scopes, ignores, previews, incremental indexing, and sharing actions.
- [ ] Tray/menu-bar lifecycle, launch at login, onboarding, permissions, diagnostics, and updates.

## Clipboard and nearby sharing

- [ ] Searchable history for text, rich text, links, images, and file lists with pins and retention.
- [ ] Sensitive-content markers, excluded applications, source attribution, and loop prevention.
- [ ] mDNS discovery, verified pairing, trusted-device management, and encrypted QUIC transport.
- [ ] Automatic bidirectional clipboard sync with offline reconciliation and conflict resolution.
- [ ] Resumable file/folder transfer, integrity checks, progress, cancellation, and disk preflight.
- [ ] Cross-platform copy-file/paste-file semantics and explicit Nearby drag-and-drop.

## Built-in productivity

- [x] Calculator: arithmetic, scientific functions, bases, percentages, ratios, and lists.
- [x] Units, typed quantities, dates, workdays, timespans, time zones, fiat, and crypto conversion.
- [ ] Quicklinks, Markdown snippets, keyword expansion, custom shell commands, notes, and emoji.
- [ ] Window/workspace management, system actions, calendar meetings, and app uninstall/cleanup.
- [ ] Versioned backup/restore and compatible Raycast data import.

## AI

- [ ] Streaming palette chat, Markdown, attachments, history, retention, model switching, and stop.
- [ ] OpenAI, Anthropic, Gemini, OpenRouter, OpenAI-compatible, Ollama/LM Studio, and Codex routes.
- [ ] Optional Apple Intelligence on supported Macs; secure OS credential storage everywhere.
- [ ] Grammar, rewrite, translate, summarize, custom Quick Actions, preview/diff/replace/copy.

## Extensions

- [ ] Versioned `superspace:extension@1` WIT interface and `.superspace-extension` package format.
- [ ] Wasmtime capability sandbox with limits and explicit clipboard/network/filesystem/process grants.
- [ ] Declarative list, grid, detail, Markdown, form, menu, action, progress, and navigation components.
- [ ] Rust SDK and `new/build/run/package/validate/install/publish` developer CLI.
- [ ] Hash-verified, publisher-signed static extension registry and in-app browser.

## Distribution and quality

- [ ] Universal macOS app/DMG and Homebrew cask; AppImage, deb, rpm, and Arch Linux packages.
- [ ] Unit, property, visual, protocol, security, package, and physical-device integration suites.
- [ ] No telemetry; dependency auditing, secret scanning, threat model, and release documentation.
