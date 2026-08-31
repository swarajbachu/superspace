use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// One application registered with the launcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppDescriptor {
    /// Stable desktop-file name or macOS bundle identifier.
    pub id: String,
    /// Human-readable application name.
    pub name: String,
    /// Icon theme name or bundle icon path when available.
    pub icon: Option<String>,
    /// Search aliases derived from desktop metadata.
    pub keywords: Vec<String>,
    /// Platform-specific launch operation.
    pub launch: LaunchSpec,
}

impl AppDescriptor {
    /// Launch without a shell and return the spawned process identifier.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure if the executable or platform launcher cannot start.
    pub fn launch(&self) -> Result<u32, AppDiscoveryError> {
        let child = match &self.launch {
            LaunchSpec::Command {
                program,
                arguments,
                terminal,
            } if *terminal => Command::new("x-terminal-emulator")
                .arg("-e")
                .arg(program)
                .args(arguments)
                .spawn()?,
            LaunchSpec::Command {
                program,
                arguments,
                terminal: _,
            } => Command::new(program).args(arguments).spawn()?,
            LaunchSpec::MacBundle(path) => Command::new("open").arg("-a").arg(path).spawn()?,
        };
        Ok(child.id())
    }
}

/// Shell-free platform launch target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchSpec {
    /// Freedesktop `Exec` command split according to desktop-entry quoting rules.
    Command {
        /// Executable name or absolute path.
        program: String,
        /// Literal arguments after unsupported field-code arguments are removed.
        arguments: Vec<String>,
        /// Open through the user's terminal emulator.
        terminal: bool,
    },
    /// macOS application bundle opened through Launch Services.
    MacBundle(PathBuf),
}

/// Application discovery and launch failures.
#[derive(Debug, Error)]
pub enum AppDiscoveryError {
    /// Reading metadata or spawning a process failed.
    #[error("application operation failed")]
    Io(#[from] std::io::Error),
}

/// Open a file or directory with the operating system's registered default application.
///
/// No shell is involved, so path contents cannot be interpreted as commands.
///
/// # Errors
/// Returns an I/O failure if the platform opener cannot be started.
pub fn open_path(path: impl AsRef<Path>) -> Result<u32, AppDiscoveryError> {
    let path = path.as_ref();
    #[cfg(target_os = "linux")]
    let child = Command::new("xdg-open").arg(path).spawn()?;
    #[cfg(target_os = "macos")]
    let child = Command::new("open").arg(path).spawn()?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(AppDiscoveryError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "path opening is supported only on Linux and macOS",
    )));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Ok(child.id())
}

/// Standard application roots for the current operating system.
#[must_use]
pub fn default_app_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let mut roots = Vec::new();
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            roots.push(PathBuf::from(data_home).join("applications"));
        } else if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join(".local/share/applications"));
        }
        if let Some(data_dirs) = std::env::var_os("XDG_DATA_DIRS") {
            roots.extend(std::env::split_paths(&data_dirs).map(|path| path.join("applications")));
        } else {
            roots.extend([
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]);
        }
        roots
    }
    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/System/Applications"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("Applications"));
        }
        roots
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Discover valid applications beneath ordered roots, with earlier roots overriding duplicates.
///
/// Individual malformed or unreadable entries are skipped so one bad installation cannot break
/// the launcher index.
///
/// # Errors
///
/// Currently discovery is best-effort and returns an error only for future fatal platform faults.
pub fn discover_apps(roots: &[PathBuf]) -> Result<Vec<AppDescriptor>, AppDiscoveryError> {
    let mut applications = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        discover_root(root, &mut seen, &mut applications);
    }
    applications.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(applications)
}

#[cfg(target_os = "linux")]
fn discover_root(root: &Path, seen: &mut HashSet<String>, output: &mut Vec<AppDescriptor>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
            continue;
        }
        let Some(id) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if seen.contains(id) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(application) = parse_desktop_entry(id, &contents) {
            seen.insert(id.to_owned());
            output.push(application);
        }
    }
}

#[cfg(target_os = "macos")]
fn discover_root(root: &Path, seen: &mut HashSet<String>, output: &mut Vec<AppDescriptor>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("app") {
            continue;
        }
        let Ok(value) = plist::Value::from_file(path.join("Contents/Info.plist")) else {
            continue;
        };
        let Some(dictionary) = value.as_dictionary() else {
            continue;
        };
        let id = dictionary
            .get("CFBundleIdentifier")
            .and_then(plist::Value::as_string)
            .map_or_else(|| path.file_stem().and_then(|name| name.to_str()), Some);
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            continue;
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let name = dictionary
            .get("CFBundleDisplayName")
            .or_else(|| dictionary.get("CFBundleName"))
            .and_then(plist::Value::as_string)
            .or_else(|| path.file_stem().and_then(|name| name.to_str()))
            .unwrap_or(id)
            .to_owned();
        let icon = dictionary
            .get("CFBundleIconFile")
            .and_then(plist::Value::as_string)
            .map(str::to_owned);
        output.push(AppDescriptor {
            id: id.to_owned(),
            name,
            icon,
            keywords: Vec::new(),
            launch: LaunchSpec::MacBundle(path),
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn discover_root(_root: &Path, _seen: &mut HashSet<String>, _output: &mut Vec<AppDescriptor>) {}

#[cfg(target_os = "linux")]
fn parse_desktop_entry(id: &str, contents: &str) -> Option<AppDescriptor> {
    let mut in_entry = false;
    let mut name = None;
    let mut command = None;
    let mut icon = None;
    let mut keywords = Vec::new();
    let mut application_type = None;
    let mut hidden = false;
    let mut no_display = false;
    let mut terminal = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Type" => application_type = Some(value),
            "Name" => name = Some(value.to_owned()),
            "Exec" => command = split_exec(value),
            "Icon" => icon = (!value.is_empty()).then(|| value.to_owned()),
            "Keywords" => {
                keywords = value
                    .split(';')
                    .filter(|keyword| !keyword.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if application_type != Some("Application") || hidden || no_display {
        return None;
    }
    let name = name.filter(|value| !value.trim().is_empty())?;
    let mut command = command?;
    let program = command.first()?.clone();
    command.remove(0);
    Some(AppDescriptor {
        id: id.to_owned(),
        name,
        icon,
        keywords,
        launch: LaunchSpec::Command {
            program,
            arguments: command,
            terminal,
        },
    })
}

#[cfg(target_os = "linux")]
fn split_exec(value: &str) -> Option<Vec<String>> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else {
            match character {
                '\\' => escaped = true,
                '"' => quoted = !quoted,
                character if character.is_whitespace() && !quoted => {
                    if !current.is_empty() {
                        push_exec_argument(&mut arguments, &current);
                        current.clear();
                    }
                }
                _ => current.push(character),
            }
        }
    }
    if quoted || escaped {
        return None;
    }
    if !current.is_empty() {
        push_exec_argument(&mut arguments, &current);
    }
    (!arguments.is_empty()).then_some(arguments)
}

#[cfg(target_os = "linux")]
fn push_exec_argument(arguments: &mut Vec<String>, value: &str) {
    if value.contains('%') && value != "%%" {
        return;
    }
    arguments.push(value.replace("%%", "%"));
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_desktop_commands_without_a_shell() {
        let entry = parse_desktop_entry(
            "demo.desktop",
            "[Desktop Entry]\nType=Application\nName=Demo App\nExec=/opt/demo --name \"hello world\" %U --safe\nIcon=demo\nKeywords=tool;sample;\n",
        )
        .expect("desktop entry");
        assert_eq!(entry.name, "Demo App");
        assert_eq!(entry.keywords, ["tool", "sample"]);
        assert_eq!(
            entry.launch,
            LaunchSpec::Command {
                program: "/opt/demo".into(),
                arguments: vec!["--name".into(), "hello world".into(), "--safe".into()],
                terminal: false,
            }
        );
    }

    #[test]
    fn hidden_malformed_and_non_application_entries_are_skipped() {
        assert!(
            parse_desktop_entry(
                "hidden",
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nHidden=true"
            )
            .is_none()
        );
        assert!(
            parse_desktop_entry("link", "[Desktop Entry]\nType=Link\nName=X\nExec=x").is_none()
        );
        assert!(
            parse_desktop_entry(
                "bad",
                "[Desktop Entry]\nType=Application\nName=X\nExec=\"unterminated"
            )
            .is_none()
        );
    }

    #[test]
    fn ordered_roots_override_duplicate_desktop_ids() {
        let first = tempfile::tempdir().expect("first root");
        let second = tempfile::tempdir().expect("second root");
        fs::write(
            first.path().join("demo.desktop"),
            "[Desktop Entry]\nType=Application\nName=Preferred\nExec=preferred",
        )
        .expect("first entry");
        fs::write(
            second.path().join("demo.desktop"),
            "[Desktop Entry]\nType=Application\nName=Fallback\nExec=fallback",
        )
        .expect("second entry");
        let apps = discover_apps(&[first.path().into(), second.path().into()]).expect("discover");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Preferred");
    }
}
