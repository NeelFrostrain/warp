//! Deterministic color assignment for the pricing-transparency usage popover
//! (Surfaces 2, 6): per-model stacked-bar colors, plus the shared chart
//! palette also used by the pill bar's per-agent color logic, so both
//! "stacked bar" treatments in the popover share one palette.
//!
//! Colors are taken directly from the Figma "Pricing transparency" file's
//! chart palette (`191:367` / `408:23019`), rather than the app's ANSI
//! palette, so the bars visually read as data-visualization chart segments
//! rather than terminal-themed content.
//!
//! Note: the context-window breakdown (Surface 4) that originally lived in
//! this popover has been split out into its own, separately-triggered
//! surface, so this module no longer carries context-window-category color
//! assignment. See git history for the prior `color_for_context_window_category`
//! implementation if that surface needs it again.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use pathfinder_color::ColorU;

/// The six chart colors used across the popover's stacked bars, in the
/// order sampled from Figma: magenta, blue, yellow, cyan/lavender, green,
/// red.
///
/// A plain function (rather than a `const` array) because `ColorU::new` is
/// not a `const fn`.
fn chart_palette() -> [ColorU; 6] {
    [
        ColorU::new(0xff, 0x8f, 0xfd, 0xff), // magenta
        ColorU::new(0xa5, 0xd5, 0xfe, 0xff), // blue
        ColorU::new(0xfe, 0xfd, 0xc2, 0xff), // yellow
        ColorU::new(0xd0, 0xd1, 0xfe, 0xff), // cyan / lavender
        ColorU::new(0xb4, 0xfa, 0x72, 0xff), // green
        ColorU::new(0xff, 0x82, 0x72, 0xff), // red
    ]
}

/// Deterministic per-model color for the MODEL USAGE stacked bar and its
/// row swatches (Surface 2, resolved decision 8). Hashing the model id
/// keeps a given model's color stable across renders and popover reopens
/// without needing to persist an assignment anywhere.
pub fn color_for_model(model_id: &str) -> ColorU {
    let palette = chart_palette();
    let mut hasher = DefaultHasher::new();
    model_id.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % palette.len();
    palette[idx]
}

#[cfg(test)]
#[path = "colors_tests.rs"]
mod tests;
