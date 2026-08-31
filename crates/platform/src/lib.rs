//! Cross-platform operating-system adapters.

mod apps;
mod clipboard;

pub use apps::{
    AppDescriptor, AppDiscoveryError, LaunchSpec, default_app_roots, discover_apps, open_path,
};
pub use clipboard::{
    ClipboardBackend, ClipboardError, ClipboardMonitor, ClipboardObservation, ClipboardValue,
    NativeClipboard,
};
