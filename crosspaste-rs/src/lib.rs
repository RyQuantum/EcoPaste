//! crosspaste-rs: the part of the CrossPaste LAN sync protocol a desktop
//! clipboard manager needs to exchange text and images with CrossPaste
//! (SyncApi.VERSION 3) on other machines.
//!
//! Derived from CrossPaste (https://github.com/CrossPaste/crosspaste-desktop),
//! AGPL-3.0. See README.md for what is and is not implemented.

pub mod client;
pub mod crypto;
pub mod discovery;
pub mod hash;
pub mod model;
pub mod node;
mod server;
pub mod store;

pub use node::{Config, DeviceInfo, Event, EventSink, Node, PeerState};
