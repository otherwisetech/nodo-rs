//! The embedded terminal: alacritty's grid and parser on a PTY (`model`), drawn and driven by a
//! GPUI view (`view`), in a dockable tabbed panel (`panel`, state in `state`). Adapted from
//! plate's terminal, which runs on the same GPUI rev.
mod colors;
mod keys;
pub mod model;
pub mod panel;
pub mod state;
pub mod view;

pub use view::{FONT_DEFAULT, FONT_MAX, FONT_MIN};
