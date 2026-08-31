//! Cross-platform operating-system adapters.

mod clipboard;

pub use clipboard::{
    ClipboardBackend, ClipboardError, ClipboardMonitor, ClipboardObservation, ClipboardValue,
    NativeClipboard,
};
