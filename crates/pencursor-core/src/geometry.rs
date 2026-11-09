//! Coordinate space normalization and vector cursor primitive geometry built on Linebender `kurbo`.
//!
//! Provides platform-independent mapping from hardware digitizer axis ranges
//! (e.g., Win32 HID logical bounds or Linux `evdev` `ABS_X`/`ABS_Y` ranges) to display
//! viewports, alongside precomputed `kurbo` shapes (`Circle`, `Line`, `Stroke`) for cursor rendering.

use crate::config::Config;
use vello_cpu::kurbo::{Cap, Circle, Line, Point, Rect, Stroke};

/// Target display or virtual desktop viewport rectangle in global pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportBounds {
    /// Global X coordinate of the top-left corner in pixels.
    pub origin_x: f32,
    /// Global Y coordinate of the top-left corner in pixels.
    pub origin_y: f32,
    /// Total width of the viewport in pixels.
    pub width: f32,
    /// Total height of the viewport in pixels.
    pub height: f32,
}

impl ViewportBounds {
    #[inline]
    pub const fn new(origin_x: f32, origin_y: f32, width: f32, height: f32) -> Self {
        Self {
            origin_x,
            origin_y,
            width,
            height,
        }
    }

    /// Converts the viewport bounds into a [`Rect`].
    #[inline]
    pub fn to_rect(self) -> Rect {
        Rect::from_origin_size(
            (f64::from(self.origin_x), f64::from(self.origin_y)),
            (f64::from(self.width), f64::from(self.height)),
        )
    }

    /// Transforms global screen coordinates into viewport-local [`Point`] coordinates.
    #[inline]
    pub fn to_local(self, global_x: f32, global_y: f32) -> Point {
        Point::new(
            f64::from(global_x - self.origin_x),
            f64::from(global_y - self.origin_y),
        )
    }
}

/// Hardware digitizer axis bounds (`[min_x, max_x] x [min_y, max_y]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceAxisRange {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}

impl DeviceAxisRange {
    /// Standard Win32 normalized absolute mouse coordinate range `[0, 65535]`.
    pub const WIN32_ABSOLUTE_MOUSE: Self = Self::from_max(65535.0, 65535.0);

    #[inline]
    pub const fn new(min_x: f32, max_x: f32, min_y: f32, max_y: f32) -> Self {
        Self {
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }

    #[inline]
    pub const fn from_max(max_x: f32, max_y: f32) -> Self {
        Self::new(0.0, max_x, 0.0, max_y)
    }

    /// Maps raw hardware axis values linearly into global pixel coordinates `(x, y)` within `viewport`.
    #[inline]
    pub fn map_to_viewport(self, raw_x: f32, raw_y: f32, viewport: ViewportBounds) -> (f32, f32) {
        let span_x = (self.max_x - self.min_x).max(1.0);
        let span_y = (self.max_y - self.min_y).max(1.0);

        let norm_x = ((raw_x - self.min_x) / span_x).clamp(0.0, 1.0);
        let norm_y = ((raw_y - self.min_y) / span_y).clamp(0.0, 1.0);

        (
            viewport.origin_x + norm_x * viewport.width,
            viewport.origin_y + norm_y * viewport.height,
        )
    }
}

/// Precomputed `kurbo` geometric shapes and stroke styles for rendering the crosshair cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct CrosshairPrimitives {
    pub center: Point,
    pub arms: [Line; 4],
    pub inner_stroke: Stroke,
    pub outline_stroke: Stroke,
    pub center_dot: Circle,
}

impl CrosshairPrimitives {
    /// Computes `kurbo` crosshair arm lines, strokes, and center dot centered at `center`.
    #[inline]
    pub fn compute(center: Point, config: &Config) -> Self {
        let px = center.x;
        let py = center.y;
        let gap = config.crosshair_gap;
        let size = config.crosshair_size;
        let inner_width = config.crosshair_thickness;
        let outline_width = inner_width + 1.6;

        let arms = [
            // Top arm
            Line::new(Point::new(px, py - gap - size), Point::new(px, py - gap)),
            // Bottom arm
            Line::new(Point::new(px, py + gap), Point::new(px, py + gap + size)),
            // Left arm
            Line::new(Point::new(px - gap - size, py), Point::new(px - gap, py)),
            // Right arm
            Line::new(Point::new(px + gap, py), Point::new(px + gap + size, py)),
        ];

        Self {
            center,
            arms,
            inner_stroke: Stroke::new(inner_width).with_caps(Cap::Round),
            outline_stroke: Stroke::new(outline_width).with_caps(Cap::Round),
            center_dot: Circle::new(center, 1.0),
        }
    }
}

/// Precomputed `kurbo` geometric shapes and stroke styles for rendering the circular brush cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct CirclePrimitives {
    pub ring: Circle,
    pub inner_stroke: Stroke,
    pub outline_stroke: Stroke,
    pub center_outer_dot: Circle,
    pub center_inner_dot: Circle,
}

impl CirclePrimitives {
    /// Computes `kurbo` circular ring and center dot shapes centered at `center`.
    #[inline]
    pub fn compute(center: Point, config: &Config) -> Self {
        let inner_width = config.circle_thickness;
        let outline_width = inner_width + 1.6;

        Self {
            ring: Circle::new(center, config.circle_radius),
            inner_stroke: Stroke::new(inner_width),
            outline_stroke: Stroke::new(outline_width),
            center_outer_dot: Circle::new(center, 1.6),
            center_inner_dot: Circle::new(center, 1.0),
        }
    }
}

/// Precomputed `kurbo` geometric shapes for rendering the contracted contact dot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactDotPrimitives {
    pub outer_dot: Circle,
    pub inner_dot: Circle,
}

impl ContactDotPrimitives {
    /// Computes `kurbo` contact indicator dot circles centered at `center`.
    #[inline]
    pub fn compute(center: Point, config: &Config) -> Self {
        let inner_radius = config.contact_dot_radius;
        Self {
            outer_dot: Circle::new(center, inner_radius + 1.2),
            inner_dot: Circle::new(center, inner_radius),
        }
    }
}
