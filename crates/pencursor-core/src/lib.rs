//! `pencursor-core` — Platform-agnostic domain model and state engine for PenCursor.
//!
//! This crate contains zero operating-system-specific dependencies and provides:
//! - [`config`]: Cursor styles, geometric dimensions, color palettes, and adaptive frame pacing.
//! - [`geometry`]: Hardware digitizer axis normalization into display viewports and vector cursor primitives.
//! - [`state`]: Lock-free 64-bit packed atomic state machine preventing coordinate tearing across threads.
//! - [`traits`]: Unified [`OverlayBackend`] and [`PlatformRuntime`] traits for Windows and Android backends.

pub mod config;
pub mod geometry;
pub mod state;
pub mod traits;

pub use config::{Config, CursorPalette, CursorStyle, RgbaColor};
pub use geometry::{
    CirclePrimitives, ContactDotPrimitives, CrosshairPrimitives, DeviceAxisRange, LineSegment2f,
    Point2f, ViewportBounds,
};
pub use state::{
    InputEvent, StylusSnapshot, StylusStateTracker, global_tracker, pack_coords, unpack_coords,
};
pub use traits::{OverlayBackend, PlatformRuntime};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_unpack_coords_roundtrip() {
        let cases = [
            (0.0_f32, 0.0_f32),
            (1920.5, 1080.25),
            (-1280.0, 720.75),
            (3392.0, 2400.0),
        ];
        for (x, y) in cases {
            let packed = pack_coords(x, y);
            let (ux, uy) = unpack_coords(packed);
            assert_eq!(x, ux);
            assert_eq!(y, uy);
        }
    }

    #[test]
    fn test_device_axis_mapping_to_viewport() {
        // Simulates Gaomon M8 digitizer range [0, 51694] x [0, 32308] mapped onto a 2400x3392 display.
        let range = DeviceAxisRange::from_max(51694.0, 32308.0);
        let viewport = ViewportBounds::new(0.0, 0.0, 2400.0, 3392.0);

        let origin = range.map_to_viewport(0.0, 0.0, viewport);
        assert_eq!(origin, Point2f::new(0.0, 0.0));

        let full = range.map_to_viewport(51694.0, 32308.0, viewport);
        assert!((full.x - 2400.0).abs() < 1e-3);
        assert!((full.y - 3392.0).abs() < 1e-3);

        // Multi-monitor negative offset viewport test.
        let multi_vp = ViewportBounds::new(-1920.0, 0.0, 3840.0, 1080.0);
        let mid = DeviceAxisRange::WIN32_ABSOLUTE_MOUSE.map_to_viewport(
            65535.0 * 0.5,
            65535.0 * 0.5,
            multi_vp,
        );
        assert!((mid.x - 0.0).abs() < 1e-2);
        assert!((mid.y - 540.0).abs() < 1e-2);
    }

    #[test]
    fn test_state_tracker_lifecycle_and_mouse_preemption() {
        let tracker = StylusStateTracker::new(150, 100);

        // Initial state: out of range and not in contact.
        let s0 = tracker.snapshot_at(150, 0, false);
        assert!(!s0.in_range);
        assert!(!s0.is_contact);

        // t = 10ms: stylus enters proximity in hover state.
        tracker.mark_pen_active_at(500.0, 600.0, Some(false), 10);
        let s1 = tracker.snapshot_at(150, 50, false);
        assert!(s1.in_range);
        assert!(!s1.is_contact);
        assert_eq!(s1.x, 500.0);
        assert_eq!(s1.y, 600.0);

        // t = 60ms: stylus tip contacts surface.
        tracker.mark_pen_active_at(510.0, 610.0, Some(true), 60);
        let s2 = tracker.snapshot_at(150, 80, false);
        assert!(s2.in_range);
        assert!(s2.is_contact);

        // t = 90ms (30ms since last pen report <= 100ms threshold): mouse jitter does not preempt stylus.
        tracker.mark_mouse_active_with_timeout_at(100, 90);
        assert!(tracker.is_pen_in_range_at(150, 90));

        // t = 180ms (120ms since last pen report > 100ms threshold): physical mouse movement preempts stylus.
        tracker.mark_mouse_active_with_timeout_at(100, 180);
        let s3 = tracker.snapshot_at(150, 180, false);
        assert!(!s3.in_range);
        assert!(!s3.is_contact);
    }

    #[test]
    fn test_crosshair_and_circle_primitives() {
        let cfg = Config::default();
        let center = Point2f::new(100.0, 200.0);

        let cross = CrosshairPrimitives::compute(center, &cfg);
        assert_eq!(cross.arms[0].start, Point2f::new(100.0, 200.0 - 3.5 - 10.0));
        assert_eq!(cross.arms[0].end, Point2f::new(100.0, 200.0 - 3.5));

        let circle = CirclePrimitives::compute(center, &cfg);
        assert_eq!(circle.radius, 12.0);
        assert!((circle.outline_thickness - 3.1).abs() < 1e-5);
    }
}
