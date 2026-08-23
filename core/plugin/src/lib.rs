//! QuickJS plugin host: loading, isolation, capability enforcement, and
//! patching the UITree.
//!
//! One `Runtime` + `Context` per plugin. Capabilities are enforced by *not
//! injecting* the API — an undeclared API does not exist rather than being
//! refused. Patches receive only the diff; handing over the whole tree costs
//! two orders of magnitude more.
//!
//! See `spec/05-plugin-api.md`.
