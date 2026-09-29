//! Input documents by address.
//!
//! Every text box owns its document here, keyed by the node's address
//! (stable ID plus key). Adding a box registers an address; no struct
//! grows and no routing match grows with it. Focus still belongs to the
//! screens: this only stores what each box holds and what kind it holds.
use std::collections::HashMap;

use gumicord_platform::TextDocument;
use gumicord_uitree::{Key, NodeId};

/// Where one input box lives: the node's stable ID plus the key naming
/// the box under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InputAddr {
    pub(crate) node: NodeId,
    pub(crate) key: Option<Key>,
}

impl InputAddr {
    pub(crate) fn of(node: NodeId, key: Option<Key>) -> Self {
        InputAddr { node, key }
    }

    /// Whether the address names a login-form box.
    pub(crate) fn is_login(&self) -> bool {
        self.node == NodeId::AppScreenLoginField
    }
}

/// The composer's address: the composer field node, with no key. Focus
/// identity and kind live here; each channel's draft lives at
/// [`composer_doc_addr`].
pub(crate) fn composer_addr() -> InputAddr {
    InputAddr::of(NodeId::ChatInputField, None)
}

/// Where one channel's draft lives: the composer node plus the channel
/// id. Drafts fan out per channel, so switching channels never clobbers
/// what was typed elsewhere.
pub(crate) fn composer_doc_addr(channel: u64) -> InputAddr {
    InputAddr::of(NodeId::ChatInputField, Some(Key::Id(channel)))
}

/// What kind of text a box holds. Tells masking, IME and proxy kinds
/// apart without matching on the screen's field enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
    Email,
    Password,
    Code,
    Text,
}

impl InputKind {
    /// Whether the box shows bullets instead of its text.
    pub(crate) fn secret(self) -> bool {
        matches!(self, Self::Password)
    }
}

/// One box: its document plus what kind of text that is.
#[derive(Debug)]
struct InputEntry {
    doc: TextDocument,
    kind: InputKind,
}

/// Owns every input document, keyed by address. Screens register their
/// boxes once; routing then looks documents up instead of matching on
/// per-screen fields.
#[derive(Debug, Default)]
pub(crate) struct InputRegistry {
    entries: HashMap<InputAddr, InputEntry>,
}

impl InputRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers a box. Re-registering keeps the document and updates
    /// the kind.
    pub(crate) fn register(&mut self, addr: InputAddr, kind: InputKind) {
        self.ensure(addr, kind);
    }

    /// The box's document, creating it on first use. Per-channel drafts
    /// arrive nameless, so reads cannot all be pre-registered.
    pub(crate) fn ensure(&mut self, addr: InputAddr, kind: InputKind) -> &mut TextDocument {
        let entry = self.entries.entry(addr).or_insert_with(|| InputEntry {
            doc: TextDocument::new(),
            kind,
        });
        entry.kind = kind;
        &mut entry.doc
    }

    /// The box's document, if registered.
    pub(crate) fn doc(&self, addr: &InputAddr) -> Option<&TextDocument> {
        self.entries.get(addr).map(|entry| &entry.doc)
    }

    /// The box's document, mutably, if registered.
    pub(crate) fn doc_mut(&mut self, addr: &InputAddr) -> Option<&mut TextDocument> {
        self.entries.get_mut(addr).map(|entry| &mut entry.doc)
    }

    /// The box's document. Panics when unregistered: screens register
    /// their boxes at construction, so a miss is a bug, not input.
    pub(crate) fn must(&self, addr: &InputAddr) -> &TextDocument {
        self.doc(addr).expect("input box not registered")
    }

    /// The box's document, mutably. Same contract as [`Self::must`].
    pub(crate) fn must_mut(&mut self, addr: &InputAddr) -> &mut TextDocument {
        self.doc_mut(addr).expect("input box not registered")
    }

    /// The box's kind, if registered.
    pub(crate) fn kind(&self, addr: &InputAddr) -> Option<InputKind> {
        self.entries.get(addr).map(|entry| entry.kind)
    }

    /// Empties every document, keeping the registrations.
    pub(crate) fn clear(&mut self) {
        for entry in self.entries.values_mut() {
            entry.doc.take();
        }
    }
}

/// An empty document for read paths facing unregistered addresses: a
/// channel never typed in reads as empty without creating storage.
pub(crate) fn empty_doc() -> &'static TextDocument {
    static EMPTY: std::sync::OnceLock<TextDocument> = std::sync::OnceLock::new();
    EMPTY.get_or_init(TextDocument::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(slot: &'static str) -> InputAddr {
        InputAddr::of(
            gumicord_uitree::NodeId::AppScreenLoginField,
            Some(Key::Slot(slot)),
        )
    }

    /// Boxes at different addresses hold different documents: writing
    /// the password must never reach the code box.
    #[test]
    fn addresses_hold_separate_documents() {
        let mut registry = InputRegistry::new();
        registry.register(addr("password"), InputKind::Password);
        registry.register(addr("totp"), InputKind::Code);

        registry.must_mut(&addr("password")).insert("secret");

        assert_eq!(registry.must(&addr("password")).text(), "secret");
        assert!(registry.must(&addr("totp")).text().is_empty());
    }

    /// Re-registering (e.g. rebuilding the screen) keeps what was typed
    /// and updates the kind.
    #[test]
    fn re_registering_keeps_the_document() {
        let mut registry = InputRegistry::new();
        registry.register(addr("token"), InputKind::Text);
        registry.must_mut(&addr("token")).insert("tok");

        registry.register(addr("token"), InputKind::Code);

        assert_eq!(registry.must(&addr("token")).text(), "tok");
        assert_eq!(registry.kind(&addr("token")), Some(InputKind::Code));
    }

    /// Clearing empties every document but keeps the registrations, so
    /// the screens keep working without registering again.
    #[test]
    fn clearing_keeps_the_registrations() {
        let mut registry = InputRegistry::new();
        registry.register(addr("password"), InputKind::Password);
        registry.must_mut(&addr("password")).insert("secret");

        registry.clear();

        assert!(registry.must(&addr("password")).text().is_empty());
        assert_eq!(registry.kind(&addr("password")), Some(InputKind::Password));
    }

    /// An address nobody registered reads as missing, not as empty.
    #[test]
    fn unregistered_addresses_are_missing() {
        let registry = InputRegistry::new();
        assert!(registry.doc(&addr("password")).is_none());
    }
}
