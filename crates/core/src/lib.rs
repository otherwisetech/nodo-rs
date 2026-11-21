//! Nodo's engine, shared by the desktop app and the `nodo` CLI. No UI.
//!
//! - [`model`]: networks, devices, identities, observations
//! - [`db`] / [`doc`]: the map file (SQLite) and the open map
//! - [`discover`]: passive (neighbour table, mDNS, SSDP) and on-request sources (subnet scan,
//!   Tailscale, AWS, WireGuard), each a parser tested on fixtures plus a thin runner
//! - [`identity`]: observations → devices, merge / split
//! - [`graph`] / [`layout`]: the tree to draw and its positions
//! - [`monitor`]: reachability state machine and the probing engine
//! - [`export`] / [`render`]: CSV / JSON / Mermaid, SVG / PDF
//! - [`this_computer`]: which device is the computer Nodo runs on (worked out at runtime)
pub mod cidr;
pub mod cmd;
pub mod db;
pub mod discover;
pub mod doc;
pub mod export;
pub mod graph;
pub mod identity;
pub mod layout;
pub mod mac;
pub mod model;
pub mod monitor;
pub mod paths;
pub mod probe;
pub mod remote;
pub mod render;
pub mod shell_env;
pub mod time;
pub mod this_computer;
pub mod tools;
pub mod wol;
