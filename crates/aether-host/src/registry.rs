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
                let manifest = match PluginManifest::from_path(&manifest_path)
                    .and_then(|manifest| manifest.validate_identity().map(|()| manifest))
                {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        eprintln!("aether-host: skipping {}: {error}", manifest_path.display());
                        continue;
                    }
                };
                if let Some(existing) = registry.plugins.get(&manifest.id) {
                    eprintln!(
                        "aether-host: duplicate plugin id {} in {} (keeping {})",
                        manifest.id,
                        package_root.display(),
                        existing.root.display()
                    );
                    continue;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "aether-registry-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_manifest(root: &Path, dir: &str, id: &str, api: u16) {
        let package = root.join(dir);
        fs::create_dir_all(&package).unwrap();
        let manifest = serde_json::json!({
            "id": id,
            "name": id,
            "version": "0.1.0",
            "api": api,
            "protocols": ["http"],
            "executable": { aether_plugin_sdk::current_platform_key(): "bin/plugin" }
        });
        fs::write(
            package.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn skips_an_incompatible_manifest_instead_of_failing() {
        let root = temp_root("mixed");
        write_manifest(&root, "good", "good", aether_plugin_sdk::API_VERSION);
        write_manifest(&root, "stale", "stale", 1);

        let registry = PluginRegistry::discover(std::slice::from_ref(&root)).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("good").is_some());
        assert!(registry.get("stale").is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn duplicate_ids_keep_the_first_plugin() {
        // Separate roots make the discovery order deterministic.
        let first = temp_root("duplicate-first");
        let second = temp_root("duplicate-second");
        write_manifest(&first, "same", "same", aether_plugin_sdk::API_VERSION);
        write_manifest(&second, "same", "same", aether_plugin_sdk::API_VERSION);

        let registry = PluginRegistry::discover(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.root("same"), Some(first.join("same").as_path()));
        fs::remove_dir_all(first).ok();
        fs::remove_dir_all(second).ok();
    }
}
