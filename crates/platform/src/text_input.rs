use thiserror::Error;

/// Native text insertion failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TextInputError {
    /// Text insertion is not implemented on this operating system.
    #[error("native text insertion is unsupported")]
    Unsupported,
    /// The operating system could not dispatch a paste command.
    #[error("native text insertion is unavailable")]
    Unavailable,
}

/// Return whether the currently focused application exposes an editable target.
///
/// Call this before activating Superspace so the result describes the app the
/// user intends to insert into. A missing accessibility permission safely
/// returns `false`.
#[must_use]
pub fn focused_text_target() -> bool {
    focused_text_target_impl()
}

/// Paste the current clipboard into the application revealed after Superspace hides.
///
/// The native command waits briefly for focus restoration before pressing the
/// platform paste shortcut.
///
/// # Errors
/// Returns an error when the operation is unsupported or cannot be dispatched.
pub fn paste_from_clipboard() -> Result<(), TextInputError> {
    paste_from_clipboard_impl()
}

#[cfg(target_os = "macos")]
fn focused_text_target_impl() -> bool {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\"",
            "-e",
            "try",
            "-e",
            "set frontProcess to first application process whose frontmost is true",
            "-e",
            "set focusedElement to value of attribute \"AXFocusedUIElement\" of frontProcess",
            "-e",
            "set roleName to value of attribute \"AXRole\" of focusedElement",
            "-e",
            "return roleName",
            "-e",
            "on error",
            "-e",
            "return \"\"",
            "-e",
            "end try",
            "-e",
            "end tell",
        ])
        .output();
    output.ok().is_some_and(|output| {
        output.status.success() && is_editable_role(String::from_utf8_lossy(&output.stdout).trim())
    })
}

#[cfg(not(target_os = "macos"))]
const fn focused_text_target_impl() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn paste_from_clipboard_impl() -> Result<(), TextInputError> {
    std::process::Command::new("osascript")
        .args([
            "-e",
            "delay 0.12",
            "-e",
            "tell application \"System Events\" to keystroke \"v\" using command down",
        ])
        .spawn()
        .map(|_| ())
        .map_err(|_| TextInputError::Unavailable)
}

#[cfg(not(target_os = "macos"))]
const fn paste_from_clipboard_impl() -> Result<(), TextInputError> {
    Err(TextInputError::Unsupported)
}

#[cfg(any(target_os = "macos", test))]
fn is_editable_role(role: &str) -> bool {
    matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_editable_accessibility_roles_are_insert_targets() {
        for role in ["AXTextField", "AXTextArea", "AXSearchField", "AXComboBox"] {
            assert!(is_editable_role(role));
        }
        for role in ["", "AXButton", "AXStaticText", "AXWebArea"] {
            assert!(!is_editable_role(role));
        }
    }
}
