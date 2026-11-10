//! Layered settings for the terminal client.
//!
//! Each key is declared once in [`registry`] with its type, default,
//! environment variable and the labels a settings overlay draws. Values come
//! from four layers, lowest first:
//!
//! 1. the registry defaults,
//! 2. `$HORON_HOME/tui.toml`,
//! 3. `<cwd>/.horon/tui.toml`,
//! 4. every `--config <file>` in order,
//! 5. `HORON_TUI_*` environment variables.
//!
//! Tables merge deeply while arrays and scalars replace. Every resolved value
//! remembers which layer produced it, so a settings screen can say where a key
//! came from. [`reload::Reloader`] watches the files and re-resolves after one
//! changes, keeping the last good values from a file that stops parsing.

pub mod layers;
pub mod registry;
pub mod reload;

#[cfg(test)]
mod tests;

pub use layers::{Provenance, SettingValue, Settings, Sources, save_global};
pub use registry::{REGISTRY, SettingKind, SettingSpec, UiMeta, spec};
pub use reload::{Reload, Reloader};
