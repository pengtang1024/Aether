//! Discover `plugin.json` manifests from plugin package directories.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aether_plugin_sdk::PluginManifest;

use crate::error::HostError;

#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub enabled: bool,
}

impl PluginEntry {
    pub fn executable(&self) -> Result<PathBuf, HostError> {
        Ok(self.manifest.resolve_executable(&self.root)?)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, PluginEntry>,
}

impl PluginRegistry {
    /// Scan `root/<plugin-id>/plugin.json` packages. Missing roots are ignored.
    pub fn discover(roots: &[PathBuf]) -> Result<Self, HostError> {
        let mut registry = Self::default();
        for root in roots {
            if !root.is_dir() {
                continue;
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let package_root = entry.path();
                let manifest_path = package_root.join("plugin.json");
                if !manifest_path.is_file() {
                    continue;
                }
                let manifest = PluginManifest::from_path(&manifest_path)?;
                manifest.validate_identity()?;
                if registry.plugins.contains_key(&manifest.id) {
                    return Err(HostError::Protocol(format!(
                        "duplicate plugin id {} in {}",
                        manifest.id,
                        package_root.display()
                    )));
                }
                registry.plugins.insert(
                    manifest.id.clone(),
                    PluginEntry {
                        manifest,
                        root: package_root,
                        enabled: true,
                    },
                );
            }
        }
        Ok(registry)
    }

    pub fn get(&self, id: &str) -> Option<&PluginEntry> {
        self.plugins.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut PluginEntry> {
        self.plugins.get_mut(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PluginEntry> {
        self.plugins.values()
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), HostError> {
        let entry = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| HostError::PluginNotFound(id.to_string()))?;
        entry.enabled = enabled;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn root(&self, id: &str) -> Option<&Path> {
        self.plugins.get(id).map(|entry| entry.root.as_path())
    }
}
