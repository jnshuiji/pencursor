//! `pencursor-core` — Platform-agnostic domain model, state engine, and Linebender `vello_cpu`
//! cursor rasterizer for PenCursor.
//!
//! Built natively on the Linebender 2D graphics ecosystem (`kurbo`, `peniko`, and `vello_cpu`),
//! providing:
//! - [`config`]: Cursor styles, geometric dimensions, `peniko::Color` palettes, and frame pacing.
//! - [`geometry`]: Hardware digitizer axis normalization into display viewports and `kurbo` shapes.
//! - [`render`]: Unified `vello_cpu` sparse-strip SIMD rasterizer generating `64x64` cursor sprites.
//! - [`state`]: Lock-free 64-bit packed atomic state machine preventing coordinate tearing across threads.
//! - [`traits`]: Unified [`OverlayBackend`] and [`PlatformRuntime`] traits for Windows and Android backends.

pub mod config;
pub mod geometry;
pub mod render;
pub mod state;
pub mod traits;

#[cfg(test)]
mod tests;

pub use config::{
    Config, CursorPalette, CursorStyle, DEFAULT_ANDROID_CONFIG_PATH, ParseCursorStyleError,
    UsbPowerConfig,
};
pub use geometry::{
    CirclePrimitives, ContactDotPrimitives, CrosshairPrimitives, DeviceAxisRange, ViewportBounds,
};
pub use render::{
    SPRITE_CENTER_OFFSET, SPRITE_SIZE, encode_contact_cursor, encode_hover_cursor,
    render_contact_sprite, render_hover_sprite,
};
pub use state::{
    InputEvent, StylusSnapshot, StylusStateTracker, global_tracker, pack_coords, unpack_coords,
};
pub use traits::{OverlayBackend, PlatformRuntime};
pub use vello_cpu::{self, Pixmap, PixmapMut, RenderContext, Resources, color, kurbo, peniko};
