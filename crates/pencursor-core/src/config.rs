//! Cursor configuration, visual styling, and frame-pacing policy definitions.
//!
//! Defines platform-agnostic parameters for stylus overlay geometry, color palettes,
//! inactivity timeouts, and adaptive polling intervals.

use std::time::Duration;

/// Visual presentation style rendered while the stylus is hovering in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// High-contrast crosshair with a transparent center gap.
    Crosshair,
    /// High-contrast circular brush ring with a precision center dot.
    Circle,
    /// Composite overlay combining both crosshair and circular ring.
    #[default]
    Both,
}

/// Normalized floating-point RGBA color representation (`0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbaColor {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl RgbaColor {
    #[inline]
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

/// Color palette for cursor vector primitives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorPalette {
    /// Primary foreground stroke and center dot color.
    pub primary: RgbaColor,
    /// High-contrast outer shadow/border color for visibility against arbitrary backgrounds.
    pub outline: RgbaColor,
    /// Highlight color used when the stylus tip is in physical contact with the surface.
    pub contact: RgbaColor,
}

impl Default for CursorPalette {
    fn default() -> Self {
        Self {
            primary: RgbaColor::new(1.0, 1.0, 1.0, 1.0),
            outline: RgbaColor::new(0.05, 0.05, 0.05, 0.95),
            contact: RgbaColor::new(0.0, 0.9, 1.0, 1.0),
        }
    }
}

/// Global runtime and rendering configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Hover cursor visual style.
    pub style: CursorStyle,

    /// Length of each crosshair arm in pixels.
    pub crosshair_size: f32,
    /// Radius of the transparent center gap of the crosshair in pixels.
    pub crosshair_gap: f32,
    /// Stroke thickness of the primary crosshair lines in pixels.
    pub crosshair_thickness: f32,

    /// Radius of the circular cursor ring in pixels.
    pub circle_radius: f32,
    /// Stroke thickness of the circular cursor ring in pixels.
    pub circle_thickness: f32,

    /// Radius of the contracted indicator dot during tip contact in pixels.
    pub contact_dot_radius: f32,

    /// Inactivity threshold in milliseconds before the stylus is considered out of range.
    pub hover_timeout_ms: u64,
    /// Grace period in milliseconds after the last stylus report before physical mouse movement hides the overlay.
    pub mouse_inactivity_timeout_ms: u64,

    /// Frame polling interval in milliseconds while the stylus is active in range.
    pub active_poll_interval_ms: u64,
    /// Frame polling interval in milliseconds while the stylus is idle or out of range.
    pub idle_poll_interval_ms: u64,

    /// Vector rendering color palette.
    pub palette: CursorPalette,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            style: CursorStyle::Both,
            crosshair_size: 10.0,
            crosshair_gap: 3.5,
            crosshair_thickness: 1.5,
            circle_radius: 12.0,
            circle_thickness: 1.5,
            contact_dot_radius: 2.0,
            hover_timeout_ms: 150,
            mouse_inactivity_timeout_ms: 100,
            active_poll_interval_ms: 1,
            idle_poll_interval_ms: 10,
            palette: CursorPalette::default(),
        }
    }
}

impl Config {
    /// Returns the target frame sleep duration based on whether the stylus is currently in proximity.
    #[inline]
    pub const fn frame_sleep_duration(&self, in_range: bool) -> Duration {
        if in_range {
            Duration::from_millis(self.active_poll_interval_ms)
        } else {
            Duration::from_millis(self.idle_poll_interval_ms)
        }
    }
}
