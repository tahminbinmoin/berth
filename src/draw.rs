//! Drawing primitives on top of tiny-skia.

use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pixmap,
    Point, Shader, SpreadMode, Stroke, Transform,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        RectF { x, y, w, h }
    }
    pub fn inset(&self, d: f32) -> RectF {
        RectF::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

pub fn rounded_rect(r: RectF, radius: f32) -> Option<Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let k = rad * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_rgba8(r, g, b, (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub fn solid(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

pub fn fill(pm: &mut Pixmap, path: &Path, color: Color) {
    pm.fill_path(
        path,
        &solid(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

pub fn fill_with(pm: &mut Pixmap, path: &Path, paint: &Paint) {
    pm.fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
}

pub fn stroke(pm: &mut Pixmap, path: &Path, color: Color, width: f32) {
    let s = Stroke {
        width,
        ..Stroke::default()
    };
    pm.stroke_path(path, &solid(color), &s, Transform::identity(), None);
}

pub fn stroke_with(pm: &mut Pixmap, path: &Path, paint: &Paint, width: f32) {
    let s = Stroke {
        width,
        ..Stroke::default()
    };
    pm.stroke_path(path, paint, &s, Transform::identity(), None);
}

pub fn gradient(p0: (f32, f32), p1: (f32, f32), c0: Color, c1: Color) -> Option<Shader<'static>> {
    LinearGradient::new(
        Point::from_xy(p0.0, p0.1),
        Point::from_xy(p1.0, p1.1),
        vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)],
        SpreadMode::Pad,
        Transform::identity(),
    )
}

pub fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r)
}

/// Clears (erases) a shape from a pixmap.
pub fn erase(pm: &mut Pixmap, path: &Path) {
    let mut p = solid(Color::BLACK);
    p.blend_mode = BlendMode::Clear;
    pm.fill_path(path, &p, FillRule::Winding, Transform::identity(), None);
}

/// A soft drop shadow drawn as stacked translucent rounded rects, with the
/// shape itself cut out so a translucent panel is not darkened from below.
pub fn soft_shadow(
    target: &mut Pixmap,
    scratch: &mut Pixmap,
    shape: RectF,
    radius: f32,
    spread: f32,
    offset: (f32, f32),
    strength: f32,
) {
    scratch.fill(Color::TRANSPARENT);
    let steps = 7;
    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let grow = spread * (1.0 - t);
        let r = RectF::new(
            shape.x - grow + offset.0,
            shape.y - grow + offset.1,
            shape.w + 2.0 * grow,
            shape.h + 2.0 * grow,
        );
        if let Some(p) = rounded_rect(r, radius + grow) {
            fill(scratch, &p, rgba(0, 0, 0, strength / steps as f32));
        }
    }
    if let Some(p) = rounded_rect(shape, radius) {
        erase(scratch, &p);
    }
    target.draw_pixmap(
        0,
        0,
        scratch.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}
