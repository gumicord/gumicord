//! Host-side per-plugin key-value storage.
//!
//! Lives in the host so it survives a plugin reload, and one plugin can
//! never see another's keys. Written through on every mutation: values are
//! small and settings saves are rare, so durability beats batching.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::PluginError;

/// String pairs persisted as one JSON object.
#[derive(Debug, Default)]
pub struct Storage {
    path: PathBuf,
    map: HashMap<String, String>,
}

impl Storage {
    /// Opens `storage.json` inside `dir`, starting empty when absent.
    pub fn load(dir: &Path) -> Result<Self, PluginError> {
        let path = dir.join("storage.json");
        let map = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(PluginError::StorageUnreadable {
                    path: path.display().to_string(),
                    reason: e.to_string(),
                });
            }
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| PluginError::StorageCorrupt {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?,
        };
        Ok(Storage { path, map })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), PluginError> {
        self.map.insert(key.to_owned(), value.to_owned());
        self.flush()
    }

    pub fn remove(&mut self, key: &str) -> Result<(), PluginError> {
        self.map.remove(key);
        self.flush()
    }

    fn flush(&self) -> Result<(), PluginError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PluginError::StorageUnwritable {
                path: self.path.display().to_string(),
                reason: e.to_string(),
            })?;
        }
        let raw = serde_json::to_string(&self.map).map_err(|e| PluginError::StorageUnwritable {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        })?;
        std::fs::write(&self.path, raw).map_err(|e| PluginError::StorageUnwritable {
            path: self.path.display().to_string(),
            reason: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-plugin-storage-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn values_round_trip_and_survive_reload() {
        let dir = scratch("round-trip");
        let mut s = Storage::load(&dir).unwrap();
        assert_eq!(s.get("k"), None);
        s.set("k", "v").unwrap();
        s.set("n", "1").unwrap();
        s.remove("n").unwrap();

        let again = Storage::load(&dir).unwrap();
        assert_eq!(again.get("k"), Some("v"));
        assert_eq!(again.get("n"), None);
    }

    #[test]
    fn corrupt_storage_is_an_error_not_silent_loss() {
        let dir = scratch("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("storage.json"), "{oops").unwrap();
        assert!(Storage::load(&dir).is_err());
    }
}
