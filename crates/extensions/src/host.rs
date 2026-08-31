use thiserror::Error;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

use crate::ExtensionPackage;

/// Hard resource ceilings applied to each extension invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SandboxLimits {
    /// Maximum linear-memory bytes across the store.
    pub memory_bytes: usize,
    /// Maximum elements in any table.
    pub table_elements: usize,
    /// Maximum WebAssembly instances.
    pub instances: usize,
    /// Maximum instruction fuel supplied per invocation.
    pub fuel: u64,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            table_elements: 10_000,
            instances: 16,
            fuel: 10_000_000,
        }
    }
}

struct HostState {
    limits: StoreLimits,
}

/// Wasmtime component host with deny-by-default imports and bounded resources.
pub struct Sandbox {
    engine: Engine,
    limits: SandboxLimits,
}

impl Sandbox {
    /// Build a component-model engine with metering and interruption enabled.
    ///
    /// # Errors
    ///
    /// Returns an engine configuration failure for unsupported hosts.
    pub fn new(limits: SandboxLimits) -> Result<Self, SandboxError> {
        if limits.memory_bytes == 0 || limits.instances == 0 || limits.fuel == 0 {
            return Err(SandboxError::InvalidLimits);
        }
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(SandboxError::Runtime)?;
        Ok(Self { engine, limits })
    }

    /// Compile and instantiate a package without granting any host capabilities.
    ///
    /// This succeeds only for self-contained components. Imports are added later through
    /// capability-specific linkers after policy approval.
    ///
    /// # Errors
    ///
    /// Returns a compile, resource-limit, fuel, or missing-import failure.
    pub fn instantiate_unprivileged(&self, package: &ExtensionPackage) -> Result<(), SandboxError> {
        let component = Component::from_binary(&self.engine, package.module())
            .map_err(SandboxError::Runtime)?;
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .table_elements(self.limits.table_elements)
            .instances(self.limits.instances)
            .build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                limits: store_limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.limits.fuel)
            .map_err(SandboxError::Runtime)?;
        store.set_epoch_deadline(1);
        Linker::<HostState>::new(&self.engine)
            .instantiate(&mut store, &component)
            .map_err(SandboxError::Runtime)?;
        Ok(())
    }

    /// Interrupt stores configured with the current engine at their next epoch check.
    pub fn interrupt(&self) {
        self.engine.increment_epoch();
    }
}

/// Extension sandbox configuration and runtime failures.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Resource ceilings must all be positive.
    #[error("extension sandbox limits must be positive")]
    InvalidLimits,
    /// Wasmtime rejected compilation, instantiation, or metering configuration.
    #[error("extension sandbox runtime failed")]
    Runtime(#[source] wasmtime::Error),
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::{Command, ExtensionManifest, INTERFACE_ID};

    fn empty_package() -> ExtensionPackage {
        ExtensionPackage::new(
            ExtensionManifest {
                id: "dev.superspace.empty".into(),
                name: "Empty".into(),
                version: Version::new(1, 0, 0),
                interface: INTERFACE_ID.into(),
                commands: vec![Command {
                    id: "open".into(),
                    title: "Open".into(),
                    keywords: vec![],
                }],
                capabilities: vec![],
            },
            vec![0, b'a', b's', b'm', 0x0d, 0, 1, 0],
        )
        .expect("valid empty component")
    }

    #[test]
    fn self_contained_component_instantiates_without_ambient_authority() {
        let sandbox = Sandbox::new(SandboxLimits::default()).expect("sandbox");
        sandbox
            .instantiate_unprivileged(&empty_package())
            .expect("instantiate empty component");
    }

    #[test]
    fn rejects_zero_resource_limits() {
        let limits = SandboxLimits {
            fuel: 0,
            ..SandboxLimits::default()
        };
        assert!(matches!(
            Sandbox::new(limits),
            Err(SandboxError::InvalidLimits)
        ));
    }
}
