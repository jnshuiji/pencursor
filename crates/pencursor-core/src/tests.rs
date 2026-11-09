//! Unit tests for `pencursor-core`.

use super::*;
use vello_cpu::kurbo::{Line, Point};

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
    let range = DeviceAxisRange::from_max(51694.0, 32308.0);
    let viewport = ViewportBounds::new(0.0, 0.0, 2400.0, 3392.0);

    let origin = range.map_to_viewport(0.0, 0.0, viewport);
    assert_eq!(origin, (0.0, 0.0));

    let full = range.map_to_viewport(51694.0, 32308.0, viewport);
    assert!((full.0 - 2400.0).abs() < 1e-3);
    assert!((full.1 - 3392.0).abs() < 1e-3);

    let multi_vp = ViewportBounds::new(-1920.0, 0.0, 3840.0, 1080.0);
    let mid = DeviceAxisRange::WIN32_ABSOLUTE_MOUSE.map_to_viewport(
        65535.0 * 0.5,
        65535.0 * 0.5,
        multi_vp,
    );
    assert!((mid.0 - 0.0).abs() < 1e-2);
    assert!((mid.1 - 540.0).abs() < 1e-2);
}

#[test]
fn test_state_tracker_lifecycle_and_mouse_preemption() {
    let tracker = StylusStateTracker::new(150, 100);

    let s0 = tracker.snapshot_at(150, 0, false);
    assert!(!s0.in_range);
    assert!(!s0.is_contact);

    tracker.mark_pen_active_at(500.0, 600.0, Some(false), 10);
    let s1 = tracker.snapshot_at(150, 50, false);
    assert!(s1.in_range);
    assert!(!s1.is_contact);
    assert_eq!(s1.x, 500.0);
    assert_eq!(s1.y, 600.0);

    tracker.mark_pen_active_at(510.0, 610.0, Some(true), 60);
    let s2 = tracker.snapshot_at(150, 80, false);
    assert!(s2.in_range);
    assert!(s2.is_contact);

    tracker.mark_mouse_active_with_timeout_at(100, 90);
    assert!(tracker.is_pen_in_range_at(150, 90));

    tracker.mark_mouse_active_with_timeout_at(100, 180);
    let s3 = tracker.snapshot_at(150, 180, false);
    assert!(!s3.in_range);
    assert!(!s3.is_contact);
}

#[test]
fn test_kurbo_crosshair_and_circle_primitives() {
    let cfg = Config::default();
    let center = Point::new(100.0, 200.0);

    let cross = CrosshairPrimitives::compute(center, &cfg);
    assert_eq!(
        cross.arms[0],
        Line::new(
            Point::new(100.0, 200.0 - 3.5 - 10.0),
            Point::new(100.0, 200.0 - 3.5)
        )
    );

    let circle = CirclePrimitives::compute(center, &cfg);
    assert_eq!(circle.ring.radius, 12.0);
    assert!((circle.outline_stroke.width - 3.1).abs() < 1e-5);
}

#[test]
fn test_vello_cpu_hover_and_contact_sprites() {
    let cfg = Config::default();
    let hover = render_hover_sprite(&cfg);
    let contact = render_contact_sprite(&cfg);

    assert_eq!(hover.width(), SPRITE_SIZE);
    assert_eq!(hover.height(), SPRITE_SIZE);
    assert_eq!(contact.width(), SPRITE_SIZE);
    assert_eq!(contact.height(), SPRITE_SIZE);

    let center_idx = 32 * usize::from(SPRITE_SIZE) + 32;
    assert!(hover.data()[center_idx].a > 200);
    assert!(contact.data()[center_idx].a > 200);

    assert_eq!(hover.data()[0].a, 0);
    assert_eq!(contact.data()[0].a, 0);
}

#[test]
fn test_config_kv_parse_and_roundtrip() {
    let mut cfg = Config {
        style: CursorStyle::Crosshair,
        crosshair_size: 14.5,
        crosshair_gap: 4.0,
        crosshair_thickness: 2.0,
        circle_radius: 16.0,
        contact_dot_radius: 3.5,
        hover_timeout_ms: 250,
        ..Config::default()
    };
    cfg.palette.primary = CursorPalette::parse_hex_color("#00FF88FF").unwrap();
    cfg.palette.contact = CursorPalette::parse_hex_color("#FF3366").unwrap();
    cfg.usb_power.usb_vid = "256c".to_string();
    cfg.usb_power.usb_pid = "006d".to_string();
    cfg.usb_power.disable_usbcore_autosuspend = false;

    let serialized = cfg.to_string();
    let parsed: Config = serialized.parse().unwrap();

    assert_eq!(parsed.style, CursorStyle::Crosshair);
    assert!((parsed.crosshair_size - 14.5).abs() < 1e-4);
    assert!((parsed.crosshair_gap - 4.0).abs() < 1e-4);
    assert!((parsed.crosshair_thickness - 2.0).abs() < 1e-4);
    assert!((parsed.circle_radius - 16.0).abs() < 1e-4);
    assert!((parsed.contact_dot_radius - 3.5).abs() < 1e-4);
    assert_eq!(parsed.hover_timeout_ms, 250);
    assert_eq!(
        CursorPalette::format_hex_color(parsed.palette.primary),
        "#00FF88FF"
    );
    assert_eq!(
        CursorPalette::format_hex_color(parsed.palette.contact),
        "#FF3366FF"
    );
    assert_eq!(parsed.usb_power.usb_vid, "256c");
    assert_eq!(parsed.usb_power.usb_pid, "006d");
    assert_eq!(parsed.usb_power.vid_u16(), 0x256c);
    assert_eq!(parsed.usb_power.pid_u16(), 0x006d);
    assert!(!parsed.usb_power.disable_usbcore_autosuspend);
    assert!(cfg.visuals_eq(&parsed));
}

#[test]
fn test_config_bounds_clamping() {
    let raw = "
        crosshair_size=999.0
        crosshair_gap=50.0
        circle_radius=500.0
        hover_timeout_ms=1
    ";
    let cfg = Config::parse_kv(raw);
    assert!(cfg.crosshair_gap <= 14.0);
    assert!(cfg.crosshair_size + cfg.crosshair_gap <= 30.0);
    assert!(cfg.circle_radius <= 27.0);
    assert_eq!(cfg.hover_timeout_ms, 20);
}
