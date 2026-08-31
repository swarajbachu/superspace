//! Superspace command-line and desktop application entry point.

use anyhow::{Result, bail};
use superspace_core::builtin_features;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        None => superspace_ui::run(),
        Some("apps") => print_apps()?,
        Some("features") => print_features(),
        Some("launch") => {
            let id = arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: superspace launch <app-id>"))?;
            if arguments.next().is_some() {
                bail!("usage: superspace launch <app-id>");
            }
            launch_app(&id)?;
        }
        Some("--version" | "-V") => println!("superspace {}", env!("CARGO_PKG_VERSION")),
        Some(command) => bail!("unknown command: {command}"),
    }
    Ok(())
}

fn print_apps() -> Result<()> {
    for application in
        superspace_platform::discover_apps(&superspace_platform::default_app_roots())?
    {
        println!("{}\t{}", application.id, application.name);
    }
    Ok(())
}

fn launch_app(id: &str) -> Result<()> {
    let application =
        superspace_platform::discover_apps(&superspace_platform::default_app_roots())?
            .into_iter()
            .find(|application| application.id == id)
            .ok_or_else(|| anyhow::anyhow!("application not found: {id}"))?;
    let process_id = application.launch()?;
    println!("launched {} ({process_id})", application.name);
    Ok(())
}

fn print_features() {
    for feature in builtin_features() {
        println!("{:?}\t{}\t{}", feature.area, feature.id, feature.title);
    }
}
