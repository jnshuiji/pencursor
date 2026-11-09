//! Coordinate space normalization and vector cursor primitive geometry.
//!
//! Provides platform-independent mapping from hardware digitizer axis ranges
//! (e.g., Win32 HID logical bounds or Linux `evdev` `ABS_X`/`ABS_Y` ranges) to display
//! viewports, alongside precomputed geometric descriptors for cursor rendering.

use crate::config::Config;

/// 2D floating-point coordinate in screen or viewport space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point2f {
    pub x: f32,
    pub y: f32,
}

impl Point2f {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 2D line segment defined by start and end points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LineSegment2f {
    pub start: Point2f,
    pub end: Point2f,
}

impl LineSegment2f {
    #[inline]
    pub const fn new(start: Point2f, end: Point2f) -> Self {
        Self { start, end }
    }
}

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

    /// Transforms global screen coordinates into viewport-local coordinates.
    #[inline]
    pub const fn to_local(self, global_x: f32, global_y: f32) -> Point2f {
        Point2f::new(global_x - self.origin_x, global_y - self.origin_y)
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

    /// Maps raw hardware axis values linearly into global pixel coordinates within `viewport`.
    #[inline]
    pub fn map_to_viewport(self, raw_x: f32, raw_y: f32, viewport: ViewportBounds) -> Point2f {
        let span_x = (self.max_x - self.min_x).max(1.0);
        let span_y = (self.max_y - self.min_y).max(1.0);

        let norm_x = ((raw_x - self.min_x) / span_x).clamp(0.0, 1.0);
        let norm_y = ((raw_y - self.min_y) / span_y).clamp(0.0, 1.0);

        Point2f::new(
            viewport.origin_x + norm_x * viewport.width,
            viewport.origin_y + norm_y * viewport.height,
        )
    }
}

/// Precomputed geometric parameters for rendering the crosshair cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosshairPrimitives {
    pub center: Point2f,
    pub arms: [LineSegment2f; 4],
    pub inner_thickness: f32,
    pub outline_thickness: f32,
    pub center_dot_radius: f32,
}

impl CrosshairPrimitives {
    /// Computes crosshair arm segments and stroke widths centered at `center`.
    #[inline]
    pub fn compute(center: Point2f, config: &Config) -> Self {
        let px = center.x;
        let py = center.y;
        let gap = config.crosshair_gap;
        let size = config.crosshair_size;
        let inner_thickness = config.crosshair_thickness;
        let outline_thickness = inner_thickness + 1.6;

        let arms = [
            // Top arm
            LineSegment2f::new(Point2f::new(px, py - gap - size), Point2f::new(px, py - gap)),
            // Bottom arm
            LineSegment2f::new(Point2f::new(px, py + gap), Point2f::new(px, py + gap + size)),
            // Left arm
            LineSegment2f::new(Point2f::new(px - gap - size, py), Point2f::new(px - gap, py)),
            // Right arm
            LineSegment2f::new(Point2f::new(px + gap, py), Point2f::new(px + gap + size, py)),
        ];

        Self {
            center,
            arms,
            inner_thickness,
            outline_thickness,
            center_dot_radius: 1.0,
        }
    }
}

/// Precomputed geometric parameters for rendering the circular brush cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CirclePrimitives {
    pub center: Point2f,
    pub radius: f32,
    pub inner_thickness: f32,
    pub outline_thickness: f32,
    pub center_outer_dot_radius: f32,
    pub center_inner_dot_radius: f32,
}

impl CirclePrimitives {
    /// Computes circular ring and center dot geometry centered at `center`.
    #[inline]
    pub fn compute(center: Point2f, config: &Config) -> Self {
        let inner_thickness = config.circle_thickness;
        Self {
            center,
            radius: config.circle_radius,
            inner_thickness,
            outline_thickness: inner_thickness + 1.6,
            center_outer_dot_radius: 1.6,
            center_inner_dot_radius: 1.0,
        }
    }
}

/// Precomputed geometric parameters for rendering the contracted contact dot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactDotPrimitives {
    pub center: Point2f,
    pub inner_radius: f32,
    pub outline_radius: f32,
}

impl ContactDotPrimitives {
    /// Computes contact indicator dot radii centered at `center`.
    #[inline]
    pub fn compute(center: Point2f, config: &Config) -> Self {
        let inner_radius = config.contact_dot_radius;
        Self {
            center,
            inner_radius,
            outline_radius: inner_radius + 1.2,
        }
    }
}
