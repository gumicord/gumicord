//! QuickJS plugin host: loading, isolation, capability enforcement, and
//! patching the UITree.
//!
//! One `Runtime` + `Context` per plugin. Capabilities are enforced by *not
//! injecting* the API — an undeclared API does not exist rather than being
//! refused. Patches receive only the diff; handing over the whole tree costs
//! two orders of magnitude more.
//!
//! See `spec/05-plugin-api.md`.

pub mod convert;
pub mod host;
pub mod manager;
pub mod manifest;
pub mod storage;

pub use convert::{PatchContext, apply_args, apply_result, data_key, js_to_node, node_to_js};
pub use host::{INTERRUPT_BUDGET, PluginHost, PluginSource};
pub use manager::{ManagerEvent, PluginManager, PluginState, PluginStateKind};
pub use manifest::{KNOWN_CAPABILITIES, Manifest};
pub use storage::Storage;

/// Failures are English: only developers read them.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("cannot read {path}: {reason}")]
    ManifestUnreadable { path: String, reason: String },
    #[error("invalid manifest {path}: {reason}")]
    ManifestInvalid { path: String, reason: String },
    #[error("plugin id is not reverse-domain: {0}")]
    BadManifestId(String),
    #[error("version is not semver: {0}")]
    BadVersion(String),
    #[error("entry must be a flat .js/.qjsc file name: {0}")]
    BadEntry(String),
    #[error("unknown capability: {0}")]
    UnknownCapability(String),
    #[error("duplicate capability: {0}")]
    DuplicateCapability(String),
    #[error("cannot read storage {path}: {reason}")]
    StorageUnreadable { path: String, reason: String },
    #[error("storage is corrupt, refusing to drop it silently {path}: {reason}")]
    StorageCorrupt { path: String, reason: String },
    #[error("cannot write storage {path}: {reason}")]
    StorageUnwritable { path: String, reason: String },
    #[error("plugin {id} failed to load: {reason}")]
    LoadFailed { id: String, reason: String },
    #[error("plugin {id} has no patch entry point")]
    NoPatchEntry { id: String },
    #[error("plugin {id} failed a patch on {node}: {reason}")]
    PatchFailed {
        id: String,
        node: String,
        reason: String,
    },
    #[error("plugin {id} returned an unreadable tree: {reason}")]
    BadPatchOutput { id: String, reason: String },
    #[error("plugin {id} disabled after {failures} failures: {reason}")]
    Disabled {
        id: String,
        failures: usize,
        reason: String,
    },
}
