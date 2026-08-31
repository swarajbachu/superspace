# Architecture

Superspace is a Rust workspace with strict dependency direction:

```text
GPUI app -> feature coordinators -> pure core
       \-> platform adapters
       \-> SQLite/blob storage
       \-> encrypted network protocol
       \-> Wasmtime extension host
```

The initial workspace establishes the executable, pure domain model, and versioned wire protocol.
Storage, platform, networking, calculator, AI, and extension-host crates are added behind traits as
their milestones land. UI code never performs blocking I/O.

## State ownership

The application composition root owns every long-lived service. Features expose coordinators to the
GPUI layer and communicate with services using immutable commands and events. There are no global
feature singletons.

## Persistence

Durable user data lives in platform application-support directories. SQLite uses WAL and numbered
migrations; binary payloads use a BLAKE3 content-addressed blob store. Caches contain only refetchable
data. Credentials are stored exclusively in Keychain or Secret Service and are excluded from logs and
backups.

## Networking

Peers are discovered through mDNS (`_superspace._tcp.local`) or entered manually. Pairing verifies a
six-digit short authentication string and pins long-term device identities. Trusted peers communicate
over mutually authenticated TLS 1.3/QUIC. Hybrid logical clocks order clipboard events; event IDs and
content hashes prevent loops and duplicates.

## UI and motion

GPUI is pinned and wrapped by Superspace primitives. Motion is a centralized catalog with stable
animation IDs, explicit enter/exit lifecycles, a shared throttled pulse clock, and automatic reduced
motion. Repeating animations stop scheduling frames when unmounted. This follows lessons learned from
Comet without copying its UI or identity.

## Platform truth

macOS and X11 use native accessibility/window APIs. Wayland uses XDG portals and optional GNOME/KWin
companions. When a compositor denies an operation, the command reports the limitation and offers the
closest safe fallback.
