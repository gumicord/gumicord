//! Screen code, one module per screen. Each owns its state outright;
//! the shell in `super` keeps only what screens share.
pub mod chat;
pub mod login;
pub mod overlays;
pub mod settings;
