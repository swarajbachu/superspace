//! Superspace extension developer command line.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use superspace_extensions::{
    Sandbox, SandboxLimits, install_package, package_component, scaffold_extension,
    validate_package,
};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("new") => {
            let directory = required(&mut arguments, "directory")?;
            let id = required(&mut arguments, "extension id")?;
            let name = arguments.collect::<Vec<_>>().join(" ");
            if name.is_empty() {
                bail!("usage: superspace-extension new <directory> <id> <display name>");
            }
            scaffold_extension(directory, &id, &name)?;
        }
        Some("build") => build(arguments.next().as_deref().unwrap_or("."))?,
        Some("package") => {
            let manifest = required(&mut arguments, "manifest path")?;
            let component = required(&mut arguments, "component path")?;
            let output = required(&mut arguments, "output path")?;
            no_more(arguments)?;
            package_component(manifest, component, output)?;
        }
        Some("validate") => {
            let path = required(&mut arguments, "package path")?;
            no_more(arguments)?;
            let package = validate_package(path)?;
            println!(
                "{} {} is valid",
                package.manifest.id, package.manifest.version
            );
        }
        Some("install") => {
            let path = required(&mut arguments, "package path")?;
            let root = arguments
                .next()
                .map_or_else(default_install_root, PathBuf::from);
            no_more(arguments)?;
            let receipt = install_package(path, root)?;
            println!("installed {} {}", receipt.id, receipt.version);
        }
        Some("run") => {
            let path = required(&mut arguments, "package path")?;
            no_more(arguments)?;
            let package = validate_package(path)?;
            Sandbox::new(SandboxLimits::default())?.instantiate_unprivileged(&package)?;
            println!(
                "{} instantiated without ambient authority",
                package.manifest.id
            );
        }
        Some("--version" | "-V") => println!("superspace-extension {}", env!("CARGO_PKG_VERSION")),
        _ => bail!(
            "usage: superspace-extension <new|build|package|validate|install|run> [arguments]"
        ),
    }
    Ok(())
}

fn build(directory: &str) -> Result<()> {
    let status = Command::new("cargo")
        .args(["component", "build", "--release"])
        .current_dir(directory)
        .status()
        .context(
            "failed to start `cargo component`; install it with `cargo install cargo-component`",
        )?;
    if !status.success() {
        bail!("cargo component build failed");
    }
    Ok(())
}

fn required(arguments: &mut impl Iterator<Item = String>, label: &str) -> Result<String> {
    arguments.next().with_context(|| format!("missing {label}"))
}

fn no_more(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    if arguments.next().is_some() {
        bail!("too many arguments");
    }
    Ok(())
}

fn default_install_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map_or_else(
            || PathBuf::from("Superspace/Extensions"),
            |home| Path::new(&home).join("Library/Application Support/Superspace/Extensions"),
        )
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(
            || {
                std::env::var_os("HOME").map_or_else(
                    || PathBuf::from(".local/share/superspace/extensions"),
                    |home| Path::new(&home).join(".local/share/superspace/extensions"),
                )
            },
            |root| Path::new(&root).join("superspace/extensions"),
        )
    }
}
