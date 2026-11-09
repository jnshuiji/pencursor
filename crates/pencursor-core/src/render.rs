//! Unified 2D vector cursor rasterization powered by Linebender `vello_cpu`, `kurbo`, and `peniko`.
//!
//! Renders high-contrast, sub-pixel anti-aliased `64x64` premultiplied RGBA8 sprites for
//! the `Hover` (`Crosshair`, `Circle`, or `Both`) and `Contact` (`ContactDot`) cursor states.

use crate::config::{Config, CursorStyle};
use crate::geometry::{CirclePrimitives, ContactDotPrimitives, CrosshairPrimitives};
use vello_cpu::kurbo::{BezPath, PathEl, Point, Shape};
use vello_cpu::{Pixmap, RenderContext, Resources};

/// Default curve flattening tolerance in pixels for `kurbo::Shape::to_path`.
const PATH_TOLERANCE: f64 = 0.05;

/// Width and height of each hardware cursor sprite layer in pixels (`64x64`).
pub const SPRITE_SIZE: u16 = 64;

/// Local center offset (`32.0`) within the `64x64` sprite coordinate space.
pub const SPRITE_CENTER_OFFSET: f32 = (SPRITE_SIZE as f32) * 0.5;

/// Rasterizes the `64x64` hover cursor sprite into a premultiplied RGBA8 [`Pixmap`] using `vello_cpu`.
pub fn render_hover_sprite(config: &Config) -> Pixmap {
    let mut ctx = RenderContext::new(SPRITE_SIZE, SPRITE_SIZE);
    let mut resources = Resources::new();
    let center = Point::new(
        f64::from(SPRITE_CENTER_OFFSET),
        f64::from(SPRITE_CENTER_OFFSET),
    );

    encode_hover_cursor(&mut ctx, center, config);

    let mut pixmap = Pixmap::new(SPRITE_SIZE, SPRITE_SIZE);
    ctx.flush();
    ctx.render(&mut pixmap, &mut resources);
    pixmap
}

/// Rasterizes the `64x64` tip-contact indicator dot sprite into a premultiplied RGBA8 [`Pixmap`] using `vello_cpu`.
pub fn render_contact_sprite(config: &Config) -> Pixmap {
    let mut ctx = RenderContext::new(SPRITE_SIZE, SPRITE_SIZE);
    let mut resources = Resources::new();
    let center = Point::new(
        f64::from(SPRITE_CENTER_OFFSET),
        f64::from(SPRITE_CENTER_OFFSET),
    );

    encode_contact_cursor(&mut ctx, center, config);

    let mut pixmap = Pixmap::new(SPRITE_SIZE, SPRITE_SIZE);
    ctx.flush();
    ctx.render(&mut pixmap, &mut resources);
    pixmap
}

/// Encodes the hover cursor vector paths into `ctx` centered at `center`.
pub fn encode_hover_cursor(ctx: &mut RenderContext, center: Point, config: &Config) {
    match config.style {
        CursorStyle::Crosshair => {
            let cross = CrosshairPrimitives::compute(center, config);
            draw_crosshair(ctx, &cross, config);
        }
        CursorStyle::Circle => {
            let circle = CirclePrimitives::compute(center, config);
            draw_circle(ctx, &circle, config);
        }
        CursorStyle::Both => {
            let circle = CirclePrimitives::compute(center, config);
            let cross = CrosshairPrimitives::compute(center, config);
            draw_circle(ctx, &circle, config);
            draw_crosshair(ctx, &cross, config);
        }
    }
}

/// Encodes the tip-contact indicator dot vector paths into `ctx` centered at `center`.
pub fn encode_contact_cursor(ctx: &mut RenderContext, center: Point, config: &Config) {
    let dot = ContactDotPrimitives::compute(center, config);

    ctx.set_paint(config.palette.outline);
    ctx.fill_path(&dot.outer_dot.to_path(PATH_TOLERANCE));

    ctx.set_paint(config.palette.contact);
    ctx.fill_path(&dot.inner_dot.to_path(PATH_TOLERANCE));
}

fn draw_crosshair(ctx: &mut RenderContext, cross: &CrosshairPrimitives, config: &Config) {
    let mut arms_path = BezPath::with_capacity(8);
    for line in &cross.arms {
        arms_path.push(PathEl::MoveTo(line.p0));
        arms_path.push(PathEl::LineTo(line.p1));
    }

    ctx.set_paint(config.palette.outline);
    ctx.set_stroke(cross.outline_stroke.clone());
    ctx.stroke_path(&arms_path);

    ctx.set_paint(config.palette.primary);
    ctx.set_stroke(cross.inner_stroke.clone());
    ctx.stroke_path(&arms_path);

    ctx.fill_path(&cross.center_dot.to_path(PATH_TOLERANCE));
}

fn draw_circle(ctx: &mut RenderContext, circle: &CirclePrimitives, config: &Config) {
    let ring_path = circle.ring.to_path(PATH_TOLERANCE);

    ctx.set_paint(config.palette.outline);
    ctx.set_stroke(circle.outline_stroke.clone());
    ctx.stroke_path(&ring_path);

    ctx.set_paint(config.palette.primary);
    ctx.set_stroke(circle.inner_stroke.clone());
    ctx.stroke_path(&ring_path);

    ctx.set_paint(config.palette.outline);
    ctx.fill_path(&circle.center_outer_dot.to_path(PATH_TOLERANCE));

    ctx.set_paint(config.palette.primary);
    ctx.fill_path(&circle.center_inner_dot.to_path(PATH_TOLERANCE));
}
