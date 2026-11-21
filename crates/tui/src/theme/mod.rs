//! Colour themes and glyph presets.
//!
//! A theme is a JSON file naming every colour the interface draws (see
//! [`schema`]), and a glyph preset chooses the few marks that are not text.
//! [`load::ThemeLoader`] resolves the built-in `dark` and `light` themes or a
//! user file under `$HORON_HOME/themes`, reports a broken file as a warning
//! while keeping the previous theme, and re-reads the selected file on the
//! settings reload cadence.
//!
//! The render path reads the values through [`App`](crate::app::App)'s `theme`
//! and `symbols` fields, so a test can draw with a theme it built itself.
//! [`changes`] turns the opt-in accessible change pair on for that copy without
//! touching the file.

pub mod changes;
pub mod load;
pub mod schema;
pub mod symbols;

pub use changes::{Change, ChangePalette};
pub use load::ThemeLoader;
pub use schema::{CoreColors, PromptColors, StatusColors, Theme, TranscriptColors, builtin_source, parse};
pub use symbols::{SymbolPreset, Symbols};

#[cfg(test)]
mod tests;
