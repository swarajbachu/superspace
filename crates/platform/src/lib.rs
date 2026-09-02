//! Cross-platform operating-system adapters.

mod apps;
mod clipboard;
mod locale;
mod text_input;

pub use apps::{
    AppDescriptor, AppDiscoveryError, LaunchSpec, default_app_roots, default_browser_name,
    discover_apps, open_path,
};
pub use clipboard::{
    CaptureDisposition, ClipboardBackend, ClipboardCapturePolicy, ClipboardContext, ClipboardError,
    ClipboardMonitor, ClipboardObservation, ClipboardValue, NativeClipboard,
};
pub use locale::{LocaleDefaults, locale_defaults};
pub use text_input::{TextInputError, focused_text_target, paste_from_clipboard};
