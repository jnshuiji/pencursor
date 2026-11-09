//! 2D vector cursor sprite rasterization powered by `tiny-skia`.
//!
//! Pre-renders high-contrast, sub-pixel anti-aliased `64x64` premultiplied RGBA8888 sprites for
//! the `Hover` (`Crosshair`, `Circle`, or `Both`) and `Contact` (`ContactDot`) cursor states.

use pencursor_core::{
    CirclePrimitives, Config, ContactDotPrimitives, CrosshairPrimitives, CursorStyle, Point2f,
    RgbaColor,
};
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Width and height of each hardware cursor sprite layer in pixels.
pub const SPRITE_SIZE: u32 = 64;

/// Local center offset `(32.0, 32.0)` within the `64x64` sprite coordinate space.
pub const SPRITE_CENTER_OFFSET: f32 = (SPRITE_SIZE as f32) * 0.5;

#[inline]
fn to_skia_paint(color: RgbaColor) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(
        Color::from_rgba(
            color.r.clamp(0.0, 1.0),
            color.g.clamp(0.0, 1.0),
            color.b.clamp(0.0, 1.0),
            color.a.clamp(0.0, 1.0),
        )
        .unwrap_or(Color::WHITE),
    );
    paint.anti_alias = true;
    paint
}

/// Renders the `64x64` hover cursor sprite according to `config.style`.
pub fn render_hover_sprite(config: &Config) -> Pixmap {
    let mut pixmap = Pixmap::new(SPRITE_SIZE, SPRITE_SIZE).expect("valid 64x64 pixmap allocation");
    let center = Point2f::new(SPRITE_CENTER_OFFSET, SPRITE_CENTER_OFFSET);

    match config.style {
        CursorStyle::Crosshair => {
            let cross = CrosshairPrimitives::compute(center, config);
            draw_crosshair(&mut pixmap, &cross, config);
        }
        CursorStyle::Circle => {
            let circle = CirclePrimitives::compute(center, config);
            draw_circle(&mut pixmap, &circle, config);
        }
        CursorStyle::Both => {
            let circle = CirclePrimitives::compute(center, config);
            let cross = CrosshairPrimitives::compute(center, config);
            draw_circle(&mut pixmap, &circle, config);
            draw_crosshair(&mut pixmap, &cross, config);
        }
    }

    pixmap
}

/// Renders the `64x64` tip-contact indicator dot sprite.
pub fn render_contact_sprite(config: &Config) -> Pixmap {
    let mut pixmap = Pixmap::new(SPRITE_SIZE, SPRITE_SIZE).expect("valid 64x64 pixmap allocation");
    let center = Point2f::new(SPRITE_CENTER_OFFSET, SPRITE_CENTER_OFFSET);
    let dot = ContactDotPrimitives::compute(center, config);

    let outline_paint = to_skia_paint(config.palette.outline);
    let contact_paint = to_skia_paint(config.palette.contact);

    if let Some(outer_path) =
        PathBuilder::from_circle(dot.center.x, dot.center.y, dot.outline_radius)
    {
        pixmap.fill_path(
            &outer_path,
            &outline_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    if let Some(inner_path) = PathBuilder::from_circle(dot.center.x, dot.center.y, dot.inner_radius)
    {
        pixmap.fill_path(
            &inner_path,
            &contact_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    pixmap
}

fn draw_crosshair(pixmap: &mut Pixmap, cross: &CrosshairPrimitives, config: &Config) {
    let outline_paint = to_skia_paint(config.palette.outline);
    let primary_paint = to_skia_paint(config.palette.primary);

    let mut pb = PathBuilder::new();
    for seg in &cross.arms {
        pb.move_to(seg.start.x, seg.start.y);
        pb.line_to(seg.end.x, seg.end.y);
    }

    if let Some(arms_path) = pb.finish() {
        let outer_stroke = Stroke {
            width: cross.outline_thickness,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(
            &arms_path,
            &outline_paint,
            &outer_stroke,
            Transform::identity(),
            None,
        );

        let inner_stroke = Stroke {
            width: cross.inner_thickness,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(
            &arms_path,
            &primary_paint,
            &inner_stroke,
            Transform::identity(),
            None,
        );
    }

    if let Some(dot_path) = PathBuilder::from_circle(
        cross.center.x,
        cross.center.y,
        cross.center_dot_radius,
    ) {
        pixmap.fill_path(
            &dot_path,
            &primary_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn draw_circle(pixmap: &mut Pixmap, circle: &CirclePrimitives, config: &Config) {
    let outline_paint = to_skia_paint(config.palette.outline);
    let primary_paint = to_skia_paint(config.palette.primary);

    if let Some(ring_path) =
        PathBuilder::from_circle(circle.center.x, circle.center.y, circle.radius)
    {
        let outer_stroke = Stroke {
            width: circle.outline_thickness,
            ..Stroke::default()
        };
        pixmap.stroke_path(
            &ring_path,
            &outline_paint,
            &outer_stroke,
            Transform::identity(),
            None,
        );

        let inner_stroke = Stroke {
            width: circle.inner_thickness,
            ..Stroke::default()
        };
        pixmap.stroke_path(
            &ring_path,
            &primary_paint,
            &inner_stroke,
            Transform::identity(),
            None,
        );
    }

    if let Some(center_outer) = PathBuilder::from_circle(
        circle.center.x,
        circle.center.y,
        circle.center_outer_dot_radius,
    ) {
        pixmap.fill_path(
            &center_outer,
            &outline_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    if let Some(center_inner) = PathBuilder::from_circle(
        circle.center.x,
        circle.center.y,
        circle.center_inner_dot_radius,
    ) {
        pixmap.fill_path(
            &center_inner,
            &primary_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tiny_skia_hover_and_contact_sprites_non_empty() {
        let cfg = Config::default();
        let hover = render_hover_sprite(&cfg);
        let contact = render_contact_sprite(&cfg);

        assert_eq!(hover.width(), SPRITE_SIZE);
        assert_eq!(hover.height(), SPRITE_SIZE);
        assert_eq!(contact.width(), SPRITE_SIZE);
        assert_eq!(contact.height(), SPRITE_SIZE);

        // Verify the center pixel (32, 32) is opaque in both sprites.
        let center_idx = ((32 * SPRITE_SIZE + 32) * 4) as usize;
        assert!(hover.data()[center_idx + 3] > 200);
        assert!(contact.data()[center_idx + 3] > 200);

        // Verify corner pixel (0, 0) remains completely transparent.
        assert_eq!(&hover.data()[0..4], &[0, 0, 0, 0]);
        assert_eq!(&contact.data()[0..4], &[0, 0, 0, 0]);
    }
}
