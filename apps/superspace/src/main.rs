//! Superspace command-line and desktop application entry point.

use anyhow::{Result, bail};
use superspace_core::builtin_features;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None | Some("features") => print_features(),
        Some("--version" | "-V") => println!("superspace {}", env!("CARGO_PKG_VERSION")),
        Some(command) => bail!("unknown command: {command}"),
    }
    Ok(())
}

fn print_features() {
    for feature in builtin_features() {
        println!("{:?}\t{}\t{}", feature.area, feature.id, feature.title);
    }
}
