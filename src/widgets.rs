//! A small immediate-mode widget toolkit for Berth's windows, drawn with
//! tiny-skia in the style of Windows 11 Settings: rounded cards, toggle
//! switches, sliders, dropdowns and Fluent icons (Segoe Fluent Icons).
//!
//! Each input event runs one "frame": the window's build function calls
//! widget functions with logical (96-DPI) coordinates; widgets draw
//! themselves and report clicks/changes. Work that can open dialogs is
//! returned as commands and run after the frame.

use std::collections::HashMap;

use tiny_skia::{
    Color, FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use crate::draw::{rgba, rounded_rect, RectF};
use crate::text;

pub type Id = u64;

/// Stable id from a string (FNV-1a).
pub fn id(s: &str) -> Id {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn idx(s: &str, i: usize) -> Id {
    id(s) ^ ((i as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15))
}

// ----- theme -----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color,
    pub card: Color,
    pub card_border: Color,
    pub hover: Color,
    pub pressed: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub control: Color,
    pub control_hover: Color,
    pub control_border: Color,
    pub divider: Color,
    pub nav_selected: Color,
    pub input: Color,
    pub scrim: Color,
    pub flyout: Color,
}

fn c(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

impl Palette {
    /// `accent` is the Windows accent colour suited to the theme, if known.
    pub fn new(dark: bool, accent: Option<(u8, u8, u8)>) -> Palette {
        if dark {
            let a = accent.unwrap_or((76, 194, 255));
            Palette {
                dark,
                bg: c(32, 32, 32),
                card: c(43, 43, 43),
                card_border: c(29, 29, 29),
                hover: rgba(255, 255, 255, 0.06),
                pressed: rgba(255, 255, 255, 0.04),
                text: c(255, 255, 255),
                text2: c(200, 200, 200),
                text3: c(120, 120, 120),
                accent: c(a.0, a.1, a.2),
                on_accent: c(0, 0, 0),
                control: c(55, 55, 55),
                control_hover: c(62, 62, 62),
                control_border: c(70, 70, 70),
                divider: c(29, 29, 29),
                nav_selected: rgba(255, 255, 255, 0.07),
                input: c(45, 45, 45),
                scrim: rgba(0, 0, 0, 0.45),
                flyout: c(44, 44, 44),
            }
        } else {
            let a = accent.unwrap_or((0, 95, 184));
            Palette {
                dark,
                bg: c(243, 243, 243),
                card: c(251, 251, 251),
                card_border: c(229, 229, 229),
                hover: rgba(0, 0, 0, 0.04),
                pressed: rgba(0, 0, 0, 0.06),
                text: c(26, 26, 26),
                text2: c(96, 96, 96),
                text3: c(160, 160, 160),
                accent: c(a.0, a.1, a.2),
                on_accent: c(255, 255, 255),
                control: c(254, 254, 254),
                control_hover: c(249, 249, 249),
                control_border: c(222, 222, 222),
                divider: c(234, 234, 234),
                nav_selected: rgba(0, 0, 0, 0.05),
                input: c(255, 255, 255),
                scrim: rgba(0, 0, 0, 0.30),
                flyout: c(249, 249, 249),
            }
        }
    }

    pub fn rgb(col: Color) -> (u8, u8, u8) {
        let u = col.to_color_u8();
        (u.red(), u.green(), u.blue())
    }
}

// ----- fonts and text ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Font {
    Body,
    Strong,
    Title,
    Icon,
}

impl Font {
    fn face(self) -> &'static str {
        match self {
            Font::Body | Font::Strong => "Segoe UI Variable Text",
            Font::Title => "Segoe UI Variable Display",
            Font::Icon => "Segoe Fluent Icons",
        }
    }
    fn weight(self) -> i32 {
        match self {
            Font::Body | Font::Icon => 400,
            Font::Strong | Font::Title => 600,
        }
    }
}

#[derive(Default)]
pub struct TextCache {
    map: HashMap<(String, Font, i32, i32, u32, bool), Option<Pixmap>>,
}

impl TextCache {
    pub fn get(
        &mut self,
        s: &str,
        font: Font,
        px: i32,
        width: i32,
        col: Color,
        wrap: bool,
    ) -> Option<&Pixmap> {
        let u = col.to_color_u8();
        let key_col = u32::from_be_bytes([u.red(), u.green(), u.blue(), u.alpha()]);
        let key = (s.to_string(), font, px, width, key_col, wrap);
        if self.map.len() > 3000 {
            self.map.clear();
        }
        self.map
            .entry(key)
            .or_insert_with(|| {
                let mut pm = text::render_with(
                    s,
                    font.face(),
                    font.weight(),
                    px,
                    width.max(1),
                    wrap,
                    Palette::rgb(col),
                )?;
                if u.alpha() < 255 {
                    let a = u.alpha() as u16;
                    for v in pm.data_mut() {
                        *v = ((*v as u16 * a + 127) / 255) as u8;
                    }
                }
                Some(pm)
            })
            .as_ref()
    }
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

// ----- input ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Tab,
    Enter,
    Space,
    Left,
    Right,
    Up,
    Down,
    Escape,
    Home,
    End,
}

#[derive(Default, Clone, Debug)]
pub struct Input {
    /// Pointer position in physical pixels (client area).
    pub mx: f32,
    pub my: f32,
    pub inside: bool,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub wheel: f32,
    pub key: Option<Key>,
    pub shift: bool,
}

/// Widget state that persists between frames.
#[derive(Default)]
pub struct UiState {
    pub hot: Option<Id>,
    pub active: Option<Id>,
    pub focus: Option<Id>,
    /// Show focus rings (after keyboard navigation).
    pub focus_visible: bool,
    pub focusables: Vec<Id>,
    /// The open dropdown and the result chosen in it.
    pub dropdown: Option<Id>,
    pub dropdown_rect: RectF,
    pub dropdown_items: Vec<String>,
    pub dropdown_selected: usize,
    pub dropdown_result: Option<(Id, usize)>,
    pub want_frame: bool,
}

// ----- the frame context -----------------------------------------------------------

pub struct Ui<'a> {
    pub pm: &'a mut Pixmap,
    pub s: f32,
    pub p: Palette,
    pub input: &'a Input,
    pub st: &'a mut UiState,
    pub text: &'a mut TextCache,
    /// True while a modal layer (dropdown or sheet) above has the input.
    pub blocked: bool,
    /// Vertical scroll offset (logical) applied to content widgets.
    pub dy: f32,
    /// Clip rectangle for content (physical), with its mask.
    clip: Option<(RectF, Mask)>,
    focus_order: Vec<Id>,
}

impl<'a> Ui<'a> {
    pub fn new(
        pm: &'a mut Pixmap,
        s: f32,
        p: Palette,
        input: &'a Input,
        st: &'a mut UiState,
        text: &'a mut TextCache,
    ) -> Ui<'a> {
        let blocked = st.dropdown.is_some();
        pm.fill(p.bg);
        Ui {
            pm,
            s,
            p,
            input,
            st,
            text,
            blocked,
            dy: 0.0,
            clip: None,
            focus_order: Vec::new(),
        }
    }

    // --- geometry ---

    pub fn px(&self, r: RectF) -> RectF {
        RectF::new(
            r.x * self.s,
            (r.y - self.dy) * self.s,
            r.w * self.s,
            r.h * self.s,
        )
    }

    fn mouse_in(&self, r: RectF) -> bool {
        let q = self.px(r);
        let (x, y) = (self.input.mx, self.input.my);
        let in_clip = self
            .clip
            .as_ref()
            .map(|(c, _)| x >= c.x && y >= c.y && x < c.x + c.w && y < c.y + c.h)
            .unwrap_or(true);
        self.input.inside && in_clip && x >= q.x && y >= q.y && x < q.x + q.w && y < q.y + q.h
    }

    pub fn hovered(&self, r: RectF) -> bool {
        !self.blocked && self.mouse_in(r)
    }

    /// Restricts content drawing and input to `r` (logical, unscrolled).
    pub fn set_clip(&mut self, r: Option<RectF>) {
        self.clip = r.and_then(|r| {
            let q = RectF::new(r.x * self.s, r.y * self.s, r.w * self.s, r.h * self.s);
            let mut m = Mask::new(self.pm.width(), self.pm.height())?;
            let path = PathBuilder::from_rect(tiny_skia::Rect::from_xywh(q.x, q.y, q.w, q.h)?);
            m.fill_path(&path, FillRule::Winding, false, Transform::identity());
            Some((q, m))
        });
    }

    fn mask(&self) -> Option<&Mask> {
        self.clip.as_ref().map(|(_, m)| m)
    }

    // --- primitives ---

    pub fn fill_round(&mut self, r: RectF, radius: f32, col: Color) {
        let q = self.px(r);
        if let Some(path) = rounded_rect(q, radius * self.s) {
            let mut paint = Paint::default();
            paint.set_color(col);
            paint.anti_alias = true;
            let mask = self.clip.as_ref().map(|(_, m)| m);
            self.pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask,
            );
        }
    }

    pub fn stroke_round(&mut self, r: RectF, radius: f32, col: Color, width: f32) {
        let q = self.px(r);
        let inset = width * self.s / 2.0;
        let q = RectF::new(
            q.x + inset,
            q.y + inset,
            q.w - 2.0 * inset,
            q.h - 2.0 * inset,
        );
        if let Some(path) = rounded_rect(q, radius * self.s) {
            let mut paint = Paint::default();
            paint.set_color(col);
            paint.anti_alias = true;
            let stroke = Stroke {
                width: width * self.s,
                ..Stroke::default()
            };
            let mask = self.clip.as_ref().map(|(_, m)| m);
            self.pm
                .stroke_path(&path, &paint, &stroke, Transform::identity(), mask);
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, col: Color) {
        let (x, y) = (cx * self.s, (cy - self.dy) * self.s);
        if let Some(path) = PathBuilder::from_circle(x, y, r * self.s) {
            let mut paint = Paint::default();
            paint.set_color(col);
            paint.anti_alias = true;
            let mask = self.clip.as_ref().map(|(_, m)| m);
            self.pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                mask,
            );
        }
    }

    /// Draws a pixmap (already at physical size) with its top-left at a
    /// logical point.
    pub fn image(&mut self, img: &Pixmap, x: f32, y: f32, opacity: f32) {
        let (px, py) = (
            (x * self.s).round() as i32,
            ((y - self.dy) * self.s).round() as i32,
        );
        let paint = PixmapPaint {
            opacity,
            ..PixmapPaint::default()
        };
        let mask = self.clip.as_ref().map(|(_, m)| m);
        self.pm
            .draw_pixmap(px, py, img.as_ref(), &paint, Transform::identity(), mask);
    }

    /// Text size in logical pixels.
    pub fn measure(
        &mut self,
        s: &str,
        font: Font,
        size: f32,
        max_w: f32,
        wrap: bool,
    ) -> (f32, f32) {
        let px = (size * self.s).round() as i32;
        let w = (max_w * self.s) as i32;
        match self.text.get(s, font, px, w, self.p.text, wrap) {
            Some(pm) => (pm.width() as f32 / self.s, pm.height() as f32 / self.s),
            None => (0.0, size * 1.35),
        }
    }

    /// Draws text with its top-left at (x, y). Returns its logical size.
    #[allow(clippy::too_many_arguments)]
    pub fn text_at(
        &mut self,
        s: &str,
        x: f32,
        y: f32,
        font: Font,
        size: f32,
        col: Color,
        max_w: f32,
        wrap: bool,
    ) -> (f32, f32) {
        let px = (size * self.s).round() as i32;
        let w = (max_w * self.s) as i32;
        let (sx, dy) = (self.s, self.dy);
        let Some(pm) = self.text.get(s, font, px, w, col, wrap) else {
            return (0.0, 0.0);
        };
        let size = (pm.width() as f32 / sx, pm.height() as f32 / sx);
        let (ix, iy) = (
            (x * sx).round() as i32 - 1,
            ((y - dy) * sx).round() as i32 - 1,
        );
        let mask = self.clip.as_ref().map(|(_, m)| m);
        self.pm.draw_pixmap(
            ix,
            iy,
            pm.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            mask,
        );
        size
    }

    /// Single-line text vertically centred on `cy`.
    pub fn text_mid(
        &mut self,
        s: &str,
        x: f32,
        cy: f32,
        font: Font,
        size: f32,
        col: Color,
        max_w: f32,
    ) -> f32 {
        let (_, h) = self.measure(s, font, size, max_w, false);
        self.text_at(s, x, cy - h / 2.0, font, size, col, max_w, false)
            .0
    }

    /// A Fluent icon glyph centred on (cx, cy).
    pub fn icon(&mut self, glyph: char, cx: f32, cy: f32, size: f32, col: Color) {
        let s = glyph.to_string();
        let (w, h) = self.measure(&s, Font::Icon, size, size * 3.0, false);
        self.text_at(
            &s,
            cx - w / 2.0,
            cy - h / 2.0,
            Font::Icon,
            size,
            col,
            size * 3.0,
            false,
        );
    }

    // --- interaction ---

    /// Standard press/click handling. Returns (hovered, clicked).
    pub fn interact(&mut self, wid: Id, r: RectF, focusable: bool, enabled: bool) -> (bool, bool) {
        if focusable && enabled {
            self.focus_order.push(wid);
        }
        if !enabled {
            return (false, false);
        }
        let hov = self.hovered(r);
        if hov {
            self.st.hot = Some(wid);
        }
        let mut clicked = false;
        if hov && self.input.pressed {
            self.st.active = Some(wid);
            self.st.focus = Some(wid);
            self.st.focus_visible = false;
        }
        if self.input.released && self.st.active == Some(wid) {
            clicked = hov;
        }
        if !self.blocked
            && self.st.focus == Some(wid)
            && matches!(self.input.key, Some(Key::Enter) | Some(Key::Space))
        {
            clicked = true;
        }
        (hov, clicked)
    }

    pub fn is_pressed(&self, wid: Id) -> bool {
        self.st.active == Some(wid) && self.input.down
    }

    fn focus_ring(&mut self, wid: Id, r: RectF, radius: f32) {
        if self.st.focus_visible && self.st.focus == Some(wid) && !self.blocked {
            let ring = RectF::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
            let col = if self.p.dark {
                c(255, 255, 255)
            } else {
                c(0, 0, 0)
            };
            self.stroke_round(ring, radius + 3.0, col, 2.0);
        }
    }

    // --- widgets ---

    /// A settings card.
    pub fn card(&mut self, r: RectF) {
        self.fill_round(r, 6.0, self.p.card);
        self.stroke_round(r, 6.0, self.p.card_border, 1.0);
    }

    pub fn divider(&mut self, x: f32, y: f32, w: f32) {
        self.fill_round(RectF::new(x, y, w, 1.0), 0.0, self.p.divider);
    }

    /// Title and optional description at the left of a setting row.
    pub fn row_label(
        &mut self,
        x: f32,
        y: f32,
        h: f32,
        max_w: f32,
        title: &str,
        desc: &str,
        enabled: bool,
    ) {
        let tc = if enabled { self.p.text } else { self.p.text3 };
        let dc = if enabled { self.p.text2 } else { self.p.text3 };
        if desc.is_empty() {
            self.text_mid(title, x, y + h / 2.0, Font::Body, 14.0, tc, max_w);
        } else {
            let (_, th) = self.measure(title, Font::Body, 14.0, max_w, false);
            let (_, dh) = self.measure(desc, Font::Body, 12.0, max_w, true);
            let top = y + (h - th - dh + 2.0) / 2.0;
            self.text_at(title, x, top, Font::Body, 14.0, tc, max_w, false);
            self.text_at(desc, x, top + th - 2.0, Font::Body, 12.0, dc, max_w, true);
        }
    }

    /// Windows 11 toggle switch with "On/Off" text; `right` is its right edge.
    pub fn toggle(
        &mut self,
        wid: Id,
        right: f32,
        cy: f32,
        value: bool,
        enabled: bool,
    ) -> Option<bool> {
        let sw = RectF::new(right - 40.0, cy - 10.0, 40.0, 20.0);
        let hit = RectF::new(right - 90.0, cy - 16.0, 90.0, 32.0);
        let (hov, clicked) = self.interact(wid, hit, true, enabled);
        let label = if value { "On" } else { "Off" };
        let lc = if enabled { self.p.text } else { self.p.text3 };
        let (lw, _) = self.measure(label, Font::Body, 14.0, 60.0, false);
        self.text_mid(label, sw.x - 12.0 - lw, cy, Font::Body, 14.0, lc, 60.0);
        let pressed = self.is_pressed(wid);
        if value {
            let mut fill = self.p.accent;
            if !enabled {
                fill = self.p.text3;
            } else if hov {
                fill.set_alpha(0.9);
            }
            self.fill_round(sw, 10.0, fill);
            let kr = if pressed {
                7.0
            } else if hov {
                7.0
            } else {
                6.0
            };
            self.circle(sw.x + sw.w - 10.0, cy, kr, self.p.on_accent);
        } else {
            if hov {
                self.fill_round(sw, 10.0, self.p.hover);
            }
            let border = if enabled { self.p.text2 } else { self.p.text3 };
            self.stroke_round(sw, 10.0, border, 1.0);
            let kr = if pressed || hov { 7.0 } else { 6.0 };
            self.circle(sw.x + 10.0, cy, kr, border);
        }
        self.focus_ring(wid, sw, 10.0);
        if clicked {
            Some(!value)
        } else {
            None
        }
    }

    /// Slider with value text to its left. Returns a new value while dragging.
    #[allow(clippy::too_many_arguments)]
    pub fn slider(
        &mut self,
        wid: Id,
        r: RectF,
        min: u32,
        max: u32,
        value: u32,
        label: &str,
        enabled: bool,
    ) -> Option<u32> {
        let (hov, _) = self.interact(
            wid,
            RectF::new(r.x - 8.0, r.y, r.w + 16.0, r.h),
            true,
            enabled,
        );
        let cy = r.y + r.h / 2.0;
        let span = (max - min).max(1) as f32;
        let t = (value.clamp(min, max) - min) as f32 / span;
        let mut new = None;
        if enabled && self.st.active == Some(wid) && self.input.down {
            let mx = self.input.mx / self.s;
            let tt = ((mx - r.x) / r.w).clamp(0.0, 1.0);
            let v = min + (tt * span).round() as u32;
            if v != value {
                new = Some(v);
            }
        }
        if enabled && !self.blocked && self.st.focus == Some(wid) {
            let step = ((max - min) / 20).max(1);
            match self.input.key {
                Some(Key::Left) | Some(Key::Down) => {
                    new = Some(value.saturating_sub(step).max(min))
                }
                Some(Key::Right) | Some(Key::Up) => new = Some((value + step).min(max)),
                Some(Key::Home) => new = Some(min),
                Some(Key::End) => new = Some(max),
                _ => {}
            }
        }
        let shown_t = new.map(|v| (v - min) as f32 / span).unwrap_or(t);
        let lc = if enabled { self.p.text2 } else { self.p.text3 };
        let (lw, _) = self.measure(label, Font::Body, 14.0, 80.0, false);
        self.text_mid(label, r.x - 14.0 - lw, cy, Font::Body, 14.0, lc, 80.0);
        let track = RectF::new(r.x, cy - 2.0, r.w, 4.0);
        self.fill_round(
            track,
            2.0,
            if self.p.dark {
                rgba(255, 255, 255, 0.54)
            } else {
                rgba(0, 0, 0, 0.45)
            },
        );
        let fill = if enabled { self.p.accent } else { self.p.text3 };
        self.fill_round(RectF::new(r.x, cy - 2.0, r.w * shown_t, 4.0), 2.0, fill);
        let tx = r.x + r.w * shown_t;
        let outer = if self.p.dark {
            c(69, 69, 69)
        } else {
            c(255, 255, 255)
        };
        let ring = if self.p.dark {
            rgba(0, 0, 0, 0.25)
        } else {
            rgba(0, 0, 0, 0.14)
        };
        self.circle(tx, cy, 11.0, ring);
        self.circle(tx, cy, 10.0, outer);
        let inner = if self.is_pressed(wid) {
            5.0
        } else if hov {
            7.0
        } else {
            6.0
        };
        self.circle(tx, cy, inner, fill);
        self.focus_ring(wid, RectF::new(tx - 10.0, cy - 10.0, 20.0, 20.0), 10.0);
        new
    }

    /// Push button. `accent` draws the filled primary style.
    pub fn button(&mut self, wid: Id, r: RectF, label: &str, accent: bool, enabled: bool) -> bool {
        let (hov, clicked) = self.interact(wid, r, true, enabled);
        let pressed = self.is_pressed(wid);
        if accent {
            let mut fill = if enabled { self.p.accent } else { self.p.text3 };
            if pressed {
                fill.set_alpha(0.8);
            } else if hov {
                fill.set_alpha(0.9);
            }
            self.fill_round(r, 4.0, fill);
            let tc = self.p.on_accent;
            let (tw, _) = self.measure(label, Font::Body, 14.0, r.w, false);
            self.text_mid(
                label,
                r.x + (r.w - tw) / 2.0,
                r.y + r.h / 2.0,
                Font::Body,
                14.0,
                tc,
                r.w,
            );
        } else {
            let fill = if pressed {
                self.p.control
            } else if hov {
                self.p.control_hover
            } else {
                self.p.control
            };
            self.fill_round(r, 4.0, fill);
            self.stroke_round(r, 4.0, self.p.control_border, 1.0);
            if pressed {
                self.fill_round(r, 4.0, self.p.pressed);
            }
            let tc = if enabled { self.p.text } else { self.p.text3 };
            let (tw, _) = self.measure(label, Font::Body, 14.0, r.w, false);
            self.text_mid(
                label,
                r.x + (r.w - tw) / 2.0,
                r.y + r.h / 2.0,
                Font::Body,
                14.0,
                tc,
                r.w,
            );
        }
        self.focus_ring(wid, r, 4.0);
        clicked && enabled
    }

    /// Small square button showing a Fluent glyph.
    pub fn icon_button(&mut self, wid: Id, r: RectF, glyph: char, enabled: bool) -> bool {
        let (hov, clicked) = self.interact(wid, r, true, enabled);
        if hov {
            let f = if self.is_pressed(wid) {
                self.p.pressed
            } else {
                self.p.hover
            };
            self.fill_round(r, 4.0, f);
        }
        let col = if enabled { self.p.text } else { self.p.text3 };
        self.icon(glyph, r.x + r.w / 2.0, r.y + r.h / 2.0, 14.0, col);
        self.focus_ring(wid, r, 4.0);
        clicked && enabled
    }

    /// Checkbox square (used in lists).
    pub fn checkbox_box(&mut self, x: f32, cy: f32, checked: bool) {
        let r = RectF::new(x, cy - 10.0, 20.0, 20.0);
        if checked {
            self.fill_round(r, 4.0, self.p.accent);
            self.icon('\u{E73E}', r.x + 10.0, cy, 12.0, self.p.on_accent);
        } else {
            self.fill_round(r, 4.0, self.p.input);
            self.stroke_round(r, 4.0, self.p.text2, 1.0);
        }
    }

    /// Dropdown (combo box). Opens a flyout; returns the newly chosen index.
    pub fn dropdown(
        &mut self,
        wid: Id,
        r: RectF,
        items: &[String],
        selected: usize,
        enabled: bool,
    ) -> Option<usize> {
        let result = match self.st.dropdown_result {
            Some((w, i)) if w == wid => {
                self.st.dropdown_result = None;
                Some(i)
            }
            _ => None,
        };
        let (hov, clicked) = self.interact(wid, r, true, enabled);
        let fill = if hov {
            self.p.control_hover
        } else {
            self.p.control
        };
        self.fill_round(r, 4.0, fill);
        self.stroke_round(r, 4.0, self.p.control_border, 1.0);
        let tc = if enabled { self.p.text } else { self.p.text3 };
        let label = items.get(selected).cloned().unwrap_or_default();
        self.text_mid(
            &label,
            r.x + 11.0,
            r.y + r.h / 2.0,
            Font::Body,
            14.0,
            tc,
            r.w - 40.0,
        );
        self.icon(
            '\u{E70D}',
            r.x + r.w - 18.0,
            r.y + r.h / 2.0,
            12.0,
            self.p.text2,
        );
        self.focus_ring(wid, r, 4.0);
        let mut open = clicked;
        if enabled && !self.blocked && self.st.focus == Some(wid) {
            match self.input.key {
                Some(Key::Down) | Some(Key::Right) if selected + 1 < items.len() => {
                    return Some(selected + 1)
                }
                Some(Key::Up) | Some(Key::Left) if selected > 0 => return Some(selected - 1),
                Some(Key::Enter) | Some(Key::Space) => open = true,
                _ => {}
            }
        }
        if open && enabled && !items.is_empty() {
            let pr = self.px(r);
            self.st.dropdown = Some(wid);
            self.st.dropdown_rect =
                RectF::new(pr.x / self.s, pr.y / self.s, pr.w / self.s, pr.h / self.s);
            self.st.dropdown_items = items.to_vec();
            self.st.dropdown_selected = selected;
            self.st.want_frame = true;
        }
        result
    }

    /// Sidebar navigation item.
    pub fn nav_item(
        &mut self,
        wid: Id,
        r: RectF,
        glyph: char,
        label: &str,
        selected: bool,
    ) -> bool {
        let (hov, clicked) = self.interact(wid, r, true, true);
        if selected {
            self.fill_round(r, 4.0, self.p.nav_selected);
            self.fill_round(
                RectF::new(r.x, r.y + r.h / 2.0 - 8.0, 3.0, 16.0),
                1.5,
                self.p.accent,
            );
        } else if hov {
            self.fill_round(r, 4.0, self.p.hover);
        }
        self.icon(glyph, r.x + 26.0, r.y + r.h / 2.0, 16.0, self.p.text);
        let f = if selected { Font::Strong } else { Font::Body };
        self.text_mid(
            label,
            r.x + 50.0,
            r.y + r.h / 2.0,
            f,
            14.0,
            self.p.text,
            r.w - 60.0,
        );
        self.focus_ring(wid, r, 4.0);
        clicked
    }

    /// Rounded text-input frame (a native edit control is placed on top).
    /// Returns the inner rectangle in physical pixels.
    pub fn input_frame(&mut self, r: RectF, focused: bool) -> RectF {
        self.fill_round(r, 4.0, self.p.input);
        self.stroke_round(r, 4.0, self.p.control_border, 1.0);
        if focused {
            self.fill_round(
                RectF::new(r.x + 1.0, r.y + r.h - 2.0, r.w - 2.0, 2.0),
                1.0,
                self.p.accent,
            );
        }
        self.px(RectF::new(r.x + 10.0, r.y + 6.0, r.w - 20.0, r.h - 12.0))
    }

    /// Draws a scrollbar for a scrolled region and handles the wheel.
    /// Returns the new scroll offset.
    pub fn scroll_area(&mut self, area: RectF, content_h: f32, scroll: f32) -> f32 {
        let max = (content_h - area.h).max(0.0);
        let mut sc = scroll.clamp(0.0, max);
        let dy = self.dy;
        self.dy = 0.0;
        let over = !self.blocked && self.mouse_in(area);
        if over && self.input.wheel != 0.0 {
            sc = (sc - self.input.wheel * 60.0).clamp(0.0, max);
        }
        if max > 0.0 {
            let th = (area.h * area.h / content_h).max(30.0);
            let ty = area.y + (area.h - th) * (sc / max);
            let col = if self.p.dark {
                rgba(255, 255, 255, 0.35)
            } else {
                rgba(0, 0, 0, 0.35)
            };
            let w = if over { 4.0 } else { 2.0 };
            self.fill_round(
                RectF::new(area.x + area.w - w - 3.0, ty, w, th),
                w / 2.0,
                col,
            );
        }
        self.dy = dy;
        sc
    }

    /// Draws the open dropdown flyout and handles its input. Call last.
    pub fn finish(mut self) {
        self.clip = None;
        self.dy = 0.0;
        if let Some(wid) = self.st.dropdown {
            self.blocked = false;
            let anchor = self.st.dropdown_rect;
            let items = self.st.dropdown_items.clone();
            let item_h = 36.0;
            let h = items.len() as f32 * item_h + 8.0;
            let win_h = self.pm.height() as f32 / self.s;
            let mut y = anchor.y - (self.st.dropdown_selected as f32 * item_h) - 4.0
                + (anchor.h - item_h) / 2.0;
            y = y.clamp(8.0, (win_h - h - 8.0).max(8.0));
            let w = anchor.w.max(160.0);
            let fr = RectF::new(anchor.x, y, w, h);
            // Shadow + flyout.
            for i in 0..6 {
                let g = 6.0 - i as f32;
                self.fill_round(
                    RectF::new(fr.x - g, fr.y - g + 3.0, fr.w + 2.0 * g, fr.h + 2.0 * g),
                    8.0 + g,
                    rgba(0, 0, 0, 0.035),
                );
            }
            self.fill_round(fr, 8.0, self.p.flyout);
            self.stroke_round(fr, 8.0, self.p.card_border, 1.0);
            let mut chosen = None;
            let mut hover_any = false;
            for (i, it) in items.iter().enumerate() {
                let ir = RectF::new(
                    fr.x + 4.0,
                    fr.y + 4.0 + i as f32 * item_h,
                    fr.w - 8.0,
                    item_h - 2.0,
                );
                let hov = self.mouse_in(ir);
                hover_any |= hov;
                if i == self.st.dropdown_selected {
                    self.fill_round(ir, 4.0, self.p.nav_selected);
                    self.fill_round(
                        RectF::new(ir.x, ir.y + ir.h / 2.0 - 8.0, 3.0, 16.0),
                        1.5,
                        self.p.accent,
                    );
                } else if hov {
                    self.fill_round(ir, 4.0, self.p.hover);
                }
                self.text_mid(
                    it,
                    ir.x + 12.0,
                    ir.y + ir.h / 2.0,
                    Font::Body,
                    14.0,
                    self.p.text,
                    ir.w - 20.0,
                );
                if hov && self.input.released {
                    chosen = Some(i);
                }
            }
            match self.input.key {
                Some(Key::Down) => {
                    self.st.dropdown_selected =
                        (self.st.dropdown_selected + 1).min(items.len().saturating_sub(1));
                    self.st.want_frame = true;
                }
                Some(Key::Up) => {
                    self.st.dropdown_selected = self.st.dropdown_selected.saturating_sub(1);
                    self.st.want_frame = true;
                }
                Some(Key::Enter) | Some(Key::Space) => chosen = Some(self.st.dropdown_selected),
                Some(Key::Escape) | Some(Key::Tab) => {
                    self.st.dropdown = None;
                    self.st.want_frame = true;
                }
                _ => {}
            }
            if let Some(i) = chosen {
                self.st.dropdown_result = Some((wid, i));
                self.st.dropdown = None;
                self.st.want_frame = true;
            } else if self.input.pressed && !hover_any && !self.mouse_in(anchor) {
                self.st.dropdown = None;
                self.st.want_frame = true;
            }
            if self.input.released && self.st.active == Some(wid) {
                self.st.active = None;
            }
        } else if self.input.key == Some(Key::Tab) && !self.focus_order.is_empty() {
            let order = &self.focus_order;
            let cur = self
                .st
                .focus
                .and_then(|f| order.iter().position(|&x| x == f));
            let next = match (cur, self.input.shift) {
                (Some(i), false) => (i + 1) % order.len(),
                (Some(i), true) => (i + order.len() - 1) % order.len(),
                (None, false) => 0,
                (None, true) => order.len() - 1,
            };
            self.st.focus = Some(order[next]);
            self.st.focus_visible = true;
            self.st.want_frame = true;
        }
        if self.input.released {
            self.st.active = None;
        }
        self.st.focusables = std::mem::take(&mut self.focus_order);
        let _ = self.mask();
    }
}
