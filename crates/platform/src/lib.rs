//! Cross-platform operating-system adapters.

mod apps;
mod clipboard;
mod text_input;

pub use apps::{
    AppDescriptor, AppDiscoveryError, LaunchSpec, default_app_roots, discover_apps, open_path,
};
pub use clipboard::{
    CaptureDisposition, ClipboardBackend, ClipboardCapturePolicy, ClipboardContext, ClipboardError,
    ClipboardMonitor, ClipboardObservation, ClipboardValue, NativeClipboard,
};
pub use text_input::{TextInputError, focused_text_target, paste_from_clipboard};
