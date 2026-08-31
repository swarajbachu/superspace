use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;

use thiserror::Error;

/// Normalized clipboard representations shared across platform adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardValue {
    /// UTF-8 plain text.
    Text(String),
    /// HTML fragment paired with its searchable plain-text representation.
    Html {
        /// Text used for search and applications that do not accept HTML.
        plain: String,
        /// UTF-8 HTML fragment.
        html: String,
    },
    /// Rich Text Format paired with its searchable plain-text representation.
    Rtf {
        /// Text used for search and applications that do not accept RTF.
        plain: String,
        /// RTF document text.
        rtf: String,
    },
    /// Unpremultiplied RGBA pixels in row-major order.
    Image {
        /// Pixel width.
        width: usize,
        /// Pixel height.
        height: usize,
        /// Exactly `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
    /// Local files copied for later paste or Nearby transfer.
    Files(Vec<PathBuf>),
}

impl ClipboardValue {
    /// Validate shape and calculate a domain-separated content digest.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardError::InvalidContent`] for empty or malformed values.
    pub fn digest(&self) -> Result<blake3::Hash, ClipboardError> {
        let mut hasher = blake3::Hasher::new();
        match self {
            Self::Text(text) if !text.is_empty() => {
                hasher.update(b"text\0");
                hasher.update(text.as_bytes());
            }
            Self::Html { plain, html } if !plain.is_empty() && !html.is_empty() => {
                hasher.update(b"html\0");
                hash_field(&mut hasher, plain.as_bytes());
                hash_field(&mut hasher, html.as_bytes());
            }
            Self::Rtf { plain, rtf } if !plain.is_empty() && !rtf.is_empty() => {
                hasher.update(b"rtf\0");
                hash_field(&mut hasher, plain.as_bytes());
                hash_field(&mut hasher, rtf.as_bytes());
            }
            Self::Image {
                width,
                height,
                rgba,
            } if *width > 0
                && *height > 0
                && width
                    .checked_mul(*height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    == Some(rgba.len()) =>
            {
                hasher.update(b"rgba\0");
                hasher.update(&width.to_le_bytes());
                hasher.update(&height.to_le_bytes());
                hasher.update(rgba);
            }
            Self::Files(paths)
                if !paths.is_empty() && paths.iter().all(|path| path.is_absolute()) =>
            {
                hasher.update(b"files\0");
                for path in paths {
                    let encoded = path.to_string_lossy();
                    hasher.update(&encoded.len().to_le_bytes());
                    hasher.update(encoded.as_bytes());
                }
            }
            _ => return Err(ClipboardError::InvalidContent),
        }
        Ok(hasher.finalize())
    }
}

/// Read/write abstraction used to test monitoring without touching the desktop clipboard.
pub trait ClipboardBackend {
    /// Read the best available supported representation.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardError::Empty`] when no supported content is currently available.
    fn read(&mut self) -> Result<ClipboardValue, ClipboardError>;

    /// Replace the OS clipboard.
    ///
    /// # Errors
    ///
    /// Returns a platform or unsupported-format failure.
    fn write(&mut self, value: &ClipboardValue) -> Result<(), ClipboardError>;
}

/// macOS/X11/Wayland clipboard implementation backed by native selection APIs.
pub struct NativeClipboard {
    inner: arboard::Clipboard,
}

impl NativeClipboard {
    /// Connect to the current graphical session's clipboard.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardError::Unavailable`] outside a usable desktop session.
    pub fn connect() -> Result<Self, ClipboardError> {
        Ok(Self {
            inner: arboard::Clipboard::new().map_err(|_| ClipboardError::Unavailable)?,
        })
    }
}

impl ClipboardBackend for NativeClipboard {
    fn read(&mut self) -> Result<ClipboardValue, ClipboardError> {
        if let Ok(image) = self.inner.get_image() {
            return Ok(ClipboardValue::Image {
                width: image.width,
                height: image.height,
                rgba: image.bytes.into_owned(),
            });
        }
        if let Ok(text) = self.inner.get_text() {
            return Ok(ClipboardValue::Text(text));
        }
        #[cfg(target_os = "macos")]
        if let Some(files) = read_macos_files() {
            return Ok(ClipboardValue::Files(files));
        }
        Err(ClipboardError::Empty)
    }

    fn write(&mut self, value: &ClipboardValue) -> Result<(), ClipboardError> {
        match value {
            ClipboardValue::Text(text) => self
                .inner
                .set_text(text)
                .map_err(|_| ClipboardError::Unavailable),
            ClipboardValue::Html { plain, .. } | ClipboardValue::Rtf { plain, .. } => self
                .inner
                .set_text(plain)
                .map_err(|_| ClipboardError::Unavailable),
            ClipboardValue::Image {
                width,
                height,
                rgba,
            } => {
                value.digest()?;
                self.inner
                    .set_image(arboard::ImageData {
                        width: *width,
                        height: *height,
                        bytes: Cow::Borrowed(rgba),
                    })
                    .map_err(|_| ClipboardError::Unavailable)
            }
            ClipboardValue::Files(paths) => {
                #[cfg(target_os = "macos")]
                {
                    return write_macos_files(paths);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = paths;
                    Err(ClipboardError::Unsupported)
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn read_macos_files() -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "on run",
            "-e",
            "try",
            "-e",
            "set clipboardItems to the clipboard as list",
            "-e",
            "set outputText to \"\"",
            "-e",
            "repeat with clipboardItem in clipboardItems",
            "-e",
            "set outputText to outputText & POSIX path of clipboardItem & (ASCII character 0)",
            "-e",
            "end repeat",
            "-e",
            "return outputText",
            "-e",
            "on error",
            "-e",
            "return \"\"",
            "-e",
            "end try",
            "-e",
            "end run",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|bytes| {
            let value = std::str::from_utf8(bytes).ok()?.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        })
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(paths)
}

#[cfg(target_os = "macos")]
fn write_macos_files(paths: &[PathBuf]) -> Result<(), ClipboardError> {
    if paths.is_empty() || paths.iter().any(|path| !path.is_absolute()) {
        return Err(ClipboardError::InvalidContent);
    }
    let status = std::process::Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "set clipboardFiles to {}",
            "-e",
            "repeat with filePath in argv",
            "-e",
            "set end of clipboardFiles to POSIX file filePath",
            "-e",
            "end repeat",
            "-e",
            "set the clipboard to clipboardFiles",
            "-e",
            "end run",
            "--",
        ])
        .args(paths)
        .status()
        .map_err(|_| ClipboardError::Unavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(ClipboardError::Unavailable)
    }
}

fn hash_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&bytes.len().to_le_bytes());
    hasher.update(bytes);
}

/// A newly observed local clipboard value and its stable digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardObservation {
    /// Normalized content.
    pub value: ClipboardValue,
    /// Digest used for deduplication and network loop prevention.
    pub digest: blake3::Hash,
}

/// Desktop context accompanying a physical clipboard change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClipboardContext {
    /// Bundle ID or desktop-file ID of the foreground source application.
    pub application_id: Option<String>,
    /// Sensitive marker supplied by an OS integration or explicit user action.
    pub sensitive: bool,
}

/// Privacy decision made before history persistence or network replication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureDisposition {
    /// Drop the event entirely.
    Exclude,
    /// Store normally and permit configured synchronization.
    Normal,
    /// Store concealed locally and do not automatically synchronize.
    Sensitive,
}

/// User-controlled clipboard privacy policy.
#[derive(Clone, Debug, Default)]
pub struct ClipboardCapturePolicy {
    excluded_applications: HashSet<String>,
}

impl ClipboardCapturePolicy {
    /// Construct a case-insensitive excluded-application set.
    #[must_use]
    pub fn new(excluded_applications: impl IntoIterator<Item = String>) -> Self {
        Self {
            excluded_applications: excluded_applications
                .into_iter()
                .map(|application| application.to_lowercase())
                .collect(),
        }
    }

    /// Decide whether content may enter history or automatic synchronization.
    #[must_use]
    pub fn assess(&self, context: &ClipboardContext) -> CaptureDisposition {
        if context.application_id.as_ref().is_some_and(|application| {
            self.excluded_applications
                .contains(&application.to_lowercase())
        }) {
            CaptureDisposition::Exclude
        } else if context.sensitive {
            CaptureDisposition::Sensitive
        } else {
            CaptureDisposition::Normal
        }
    }
}

/// Edge-triggered clipboard monitor with remote-application loop suppression.
pub struct ClipboardMonitor<B> {
    backend: B,
    last_digest: Option<blake3::Hash>,
}

impl<B: ClipboardBackend> ClipboardMonitor<B> {
    /// Wrap a platform or test backend.
    #[must_use]
    pub const fn new(backend: B) -> Self {
        Self {
            backend,
            last_digest: None,
        }
    }

    /// Return content only when it differs from the last local or remote-applied value.
    ///
    /// # Errors
    ///
    /// Returns backend and validation failures. An empty clipboard is treated as no observation.
    pub fn poll(&mut self) -> Result<Option<ClipboardObservation>, ClipboardError> {
        let value = match self.backend.read() {
            Ok(value) => value,
            Err(ClipboardError::Empty) => return Ok(None),
            Err(error) => return Err(error),
        };
        let digest = value.digest()?;
        if self.last_digest == Some(digest) {
            return Ok(None);
        }
        self.last_digest = Some(digest);
        Ok(Some(ClipboardObservation { value, digest }))
    }

    /// Apply remote content and suppress its subsequent capture as a new local copy.
    ///
    /// # Errors
    ///
    /// Returns backend or validation failures without advancing suppression state.
    pub fn apply_remote(&mut self, value: &ClipboardValue) -> Result<(), ClipboardError> {
        let digest = value.digest()?;
        self.backend.write(value)?;
        self.last_digest = Some(digest);
        Ok(())
    }

    /// Recover the wrapped backend, primarily for controlled shutdown and tests.
    #[must_use]
    pub fn into_inner(self) -> B {
        self.backend
    }
}

/// Clipboard availability, format, and validation failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClipboardError {
    /// No supported representation is currently present.
    #[error("clipboard has no supported content")]
    Empty,
    /// Desktop clipboard service is unavailable or rejected the operation.
    #[error("desktop clipboard is unavailable")]
    Unavailable,
    /// Adapter cannot represent this format on the current platform.
    #[error("clipboard format is unsupported on this platform")]
    Unsupported,
    /// Content shape is empty, malformed, or unsafe.
    #[error("clipboard content is invalid")]
    InvalidContent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryClipboard {
        value: Option<ClipboardValue>,
        writes: usize,
    }

    impl ClipboardBackend for MemoryClipboard {
        fn read(&mut self) -> Result<ClipboardValue, ClipboardError> {
            self.value.clone().ok_or(ClipboardError::Empty)
        }

        fn write(&mut self, value: &ClipboardValue) -> Result<(), ClipboardError> {
            self.value = Some(value.clone());
            self.writes += 1;
            Ok(())
        }
    }

    #[test]
    fn captures_only_edges_and_suppresses_remote_echoes() {
        let backend = MemoryClipboard {
            value: Some(ClipboardValue::Text("copied on Linux".into())),
            writes: 0,
        };
        let mut monitor = ClipboardMonitor::new(backend);
        assert!(monitor.poll().expect("first poll").is_some());
        assert!(monitor.poll().expect("duplicate poll").is_none());
        monitor
            .apply_remote(&ClipboardValue::Text("copied on Mac".into()))
            .expect("apply remote");
        assert!(monitor.poll().expect("remote echo poll").is_none());
        let backend = monitor.into_inner();
        assert_eq!(backend.writes, 1);
    }

    #[test]
    fn image_shape_and_file_paths_are_validated() {
        assert!(
            ClipboardValue::Image {
                width: 2,
                height: 2,
                rgba: vec![0; 15],
            }
            .digest()
            .is_err()
        );
        assert!(
            ClipboardValue::Files(vec![PathBuf::from("relative")])
                .digest()
                .is_err()
        );
    }

    #[test]
    fn content_kinds_have_domain_separated_hashes() {
        let text = ClipboardValue::Text("same".into()).digest().expect("text");
        let image = ClipboardValue::Image {
            width: 1,
            height: 1,
            rgba: b"same".to_vec(),
        }
        .digest()
        .expect("image");
        assert_ne!(text, image);
        let html = ClipboardValue::Html {
            plain: "same".into(),
            html: "<b>same</b>".into(),
        }
        .digest()
        .expect("html");
        let rtf = ClipboardValue::Rtf {
            plain: "same".into(),
            rtf: r"{\rtf1 same}".into(),
        }
        .digest()
        .expect("rtf");
        assert_ne!(text, html);
        assert_ne!(html, rtf);
    }

    #[test]
    fn privacy_policy_excludes_sources_and_preserves_sensitive_markers() {
        let policy = ClipboardCapturePolicy::new(["com.password.Manager".into()]);
        assert_eq!(
            policy.assess(&ClipboardContext {
                application_id: Some("COM.PASSWORD.MANAGER".into()),
                sensitive: false,
            }),
            CaptureDisposition::Exclude
        );
        assert_eq!(
            policy.assess(&ClipboardContext {
                application_id: Some("dev.editor".into()),
                sensitive: true,
            }),
            CaptureDisposition::Sensitive
        );
        assert_eq!(
            policy.assess(&ClipboardContext::default()),
            CaptureDisposition::Normal
        );
    }
}
