//! The dock window: layout, drawing, hover/drag interaction and animation.
//!
//! The dock is a top-most, per-pixel-alpha layered window
//! (`WS_EX_LAYERED` + `UpdateLayeredWindow`). It never takes keyboard focus
//! (`WS_EX_NOACTIVATE`) and never appears on the taskbar or in Alt+Tab
//! (`WS_EX_TOOLWINDOW`). Fully transparent pixels let clicks through, so
//! only the panel and icons are interactive.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::time::{Duration, Instant};

use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};
use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, KillTimer, SetTimer, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA,
};

use crate::apps::{self, MatchKeys};
use crate::config::{self, Edge, HoverEffect, PinnedItem, Settings};
use crate::draw::{self, rgba, RectF};
use crate::icons::IconCache;
use crate::system::{self, Monitor};
use crate::text;

pub const TIMER_FRAME: usize = 1;
pub const TIMER_POLL: usize = 2;
const FRAME_MS: u32 = 15;
const POLL_MS: u32 = 150;

/// One icon on the dock.
pub struct Entry {
    pub item: PinnedItem,
    pub pinned: bool,
    pub keys: MatchKeys,
    pub windows: Vec<HWND>,
    /// Hover emphasis, animated between 0 and 1.
    pub hl: f32,
    pub bounce: Option<Instant>,
    /// One of its windows is asking for attention (flashing): the icon keeps
    /// bouncing until the window is activated.
    pub attention: bool,
    bounce_done: Option<Instant>,
    label: Option<(String, i32, bool, Pixmap)>,
    /// Grouping key for running apps that are not pinned.
    run_key: String,
}

impl Entry {
    fn new(item: PinnedItem, pinned: bool, keys: MatchKeys) -> Self {
        Entry {
            item,
            pinned,
            keys,
            windows: Vec::new(),
            hl: 0.0,
            bounce: None,
            attention: false,
            bounce_done: None,
            label: None,
            run_key: String::new(),
        }
    }
    pub fn running(&self) -> bool {
        !self.windows.is_empty()
    }
    fn is_bin(&self) -> bool {
        self.pinned && config::is_recycle_bin(&self.item.target)
    }
}

/// What a released mouse button did on the dock.
pub enum Released {
    Nothing,
    Click(usize),
    /// Pinned item moved from one position to another.
    Reorder(usize, usize),
    /// Pinned item at this position was dragged off the dock.
    Remove(usize),
}

/// Size and placement derived from the settings and the chosen display.
#[derive(Clone)]
pub struct Geometry {
    pub monitor: Monitor,
    pub scale: f32,
    pub edge: Edge,
    /// Icon size at rest (physical pixels).
    pub s: f32,
    pub gap: f32,
    pub pad: f32,
    pub thick: f32,
    pub margin: f32,
    pub radius: f32,
    pub mag: f32,
    pub lift_h: f32,
    pub bounce_h: f32,
    pub sep_w: f32,
    pub label_font: i32,
    /// Window rectangle in screen coordinates.
    pub win: RECT,
    pub w: i32,
    pub h: i32,
}

impl Geometry {
    fn main_len(&self) -> f32 {
        match self.edge {
            Edge::Bottom => self.w as f32,
            _ => self.h as f32,
        }
    }
    /// Window coordinates -> (along the dock, away from the screen edge).
    fn to_uv(&self, x: f32, y: f32) -> (f32, f32) {
        match self.edge {
            Edge::Bottom => (x, self.h as f32 - y),
            Edge::Left => (y, x),
            Edge::Right => (y, self.w as f32 - x),
        }
    }
    fn rect(&self, u0: f32, ulen: f32, v0: f32, vlen: f32) -> RectF {
        match self.edge {
            Edge::Bottom => RectF::new(u0, self.h as f32 - v0 - vlen, ulen, vlen),
            Edge::Left => RectF::new(v0, u0, vlen, ulen),
            Edge::Right => RectF::new(self.w as f32 - v0 - vlen, u0, vlen, ulen),
        }
    }
    fn point(&self, u: f32, v: f32) -> (f32, f32) {
        match self.edge {
            Edge::Bottom => (u, self.h as f32 - v),
            Edge::Left => (v, u),
            Edge::Right => (self.w as f32 - v, u),
        }
    }
    fn slide_distance(&self) -> f32 {
        self.margin + self.thick + 24.0 * self.scale
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SlotKind {
    Entry(usize),
    Separator,
    DropGap,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    kind: SlotKind,
    u0: f32,
    len: f32,
    lift: f32,
}

struct Press {
    idx: usize,
    x: f32,
    y: f32,
}

struct Drag {
    idx: usize,
    insert: usize,
    u: f32,
    v: f32,
    /// Far enough from the dock that letting go removes the icon.
    outside: bool,
}

struct Surface {
    dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct Dock {
    pub hwnd: HWND,
    pub entries: Vec<Entry>,
    pub icons: IconCache,
    pub geo: Option<Geometry>,
    mouse: Option<(f32, f32)>,
    hover: f32,
    hover_target: f32,
    pub hovered: Option<usize>,
    press: Option<Press>,
    drag: Option<Drag>,
    hide: f32,
    hide_target: f32,
    fullscreen: bool,
    pub user_hidden: bool,
    pub menu_open: bool,
    last_tick: Instant,
    outside_since: Option<Instant>,
    edge_since: Option<Instant>,
    reveal_grace: Option<Instant>,
    frame_timer: bool,
    poll_count: u32,
    shown: bool,
    surface: Option<Surface>,
    pixmap: Option<Pixmap>,
    scratch: Option<Pixmap>,
    names: HashMap<String, String>,
    own_pid: u32,
    pub dark: bool,
    pub animate: bool,
    effect: HoverEffect,
    trash_end: bool,
    bin_full: Option<bool>,
    remove_label: Option<(i32, bool, Pixmap)>,
    pub last_error: Option<String>,
}

fn falloff(d: f32) -> f32 {
    const R: f32 = 2.6;
    if d >= R {
        0.0
    } else {
        ((PI * d / R).cos() + 1.0) / 2.0
    }
}

fn approach(v: &mut f32, target: f32, dt: f32, tau: f32, animate: bool) -> bool {
    if !animate {
        let changed = *v != target;
        *v = target;
        return changed;
    }
    if (*v - target).abs() < 0.002 {
        let changed = *v != target;
        *v = target;
        return changed;
    }
    let k = 1.0 - (-dt / tau).exp();
    *v += (target - *v) * k;
    true
}

const BOUNCE: Duration = Duration::from_millis(520);

fn bounce_offset(start: Instant, h: f32) -> Option<f32> {
    let t = start.elapsed().as_secs_f32() / BOUNCE.as_secs_f32();
    if t >= 1.0 {
        None
    } else {
        Some(h * (PI * t).sin() * (1.0 - 0.35 * t))
    }
}

impl Dock {
    pub fn new(hwnd: HWND) -> Self {
        Dock {
            hwnd,
            entries: Vec::new(),
            icons: IconCache::default(),
            geo: None,
            mouse: None,
            hover: 0.0,
            hover_target: 0.0,
            hovered: None,
            press: None,
            drag: None,
            hide: 0.0,
            hide_target: 0.0,
            fullscreen: false,
            user_hidden: false,
            menu_open: false,
            last_tick: Instant::now(),
            outside_since: None,
            edge_since: None,
            reveal_grace: None,
            frame_timer: false,
            poll_count: 0,
            shown: false,
            surface: None,
            pixmap: None,
            scratch: None,
            names: HashMap::new(),
            own_pid: std::process::id(),
            dark: false,
            animate: true,
            effect: HoverEffect::Magnify,
            trash_end: true,
            bin_full: None,
            remove_label: None,
            last_error: None,
        }
    }

    // ----- items ---------------------------------------------------------

    /// Rebuilds pinned entries from the settings, keeping per-entry state.
    pub fn sync_items(&mut self, settings: &Settings) {
        let mut old: Vec<Entry> = self.entries.drain(..).collect();
        let mut pinned = Vec::new();
        for item in &settings.items {
            let pos = old
                .iter()
                .position(|e| e.pinned && e.item.target.eq_ignore_ascii_case(&item.target));
            let mut e = match pos {
                Some(i) => old.remove(i),
                None => Entry::new(item.clone(), true, apps::resolve(&item.target)),
            };
            e.item = item.clone();
            pinned.push(e);
        }
        let running: Vec<Entry> = old.into_iter().filter(|e| !e.pinned).collect();
        self.entries = pinned;
        if settings.show_running_apps {
            self.entries.extend(running);
        }
        self.hovered = None;
        self.press = None;
        self.drag = None;
        self.refresh_running(settings);
    }

    pub fn pinned_count(&self) -> usize {
        self.entries.iter().filter(|e| e.pinned).count()
    }

    /// The Recycle Bin entry when it is kept at the end of the dock.
    fn trailing_bin(&self) -> Option<usize> {
        if !self.trash_end {
            return None;
        }
        self.entries.iter().position(|e| e.is_bin())
    }

    fn separator_count(&self) -> usize {
        let bin = self.trailing_bin();
        let pinned = self
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| e.pinned && Some(*i) != bin)
            .count();
        let running = self.entries.iter().filter(|e| !e.pinned).count();
        let mut n = 0;
        if pinned > 0 && running > 0 {
            n += 1;
        }
        if bin.is_some() && pinned + running > 0 {
            n += 1;
        }
        n
    }

    /// Index of the entry that is the Recycle Bin, if any.
    pub fn bin_index(&self) -> Option<usize> {
        self.entries.iter().position(|e| e.is_bin())
    }

    /// A window is asking for attention: bounce its app's icon until the
    /// window is activated.
    pub fn flash(&mut self, hwnd: HWND) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.windows.contains(&hwnd)) {
            if !e.attention {
                e.attention = true;
                e.bounce_done = None;
            }
        }
        self.ensure_frame_timer();
    }

    /// A window was activated: stop asking for attention for its app.
    pub fn activated(&mut self, hwnd: HWND) {
        let mut changed = false;
        for e in self.entries.iter_mut().filter(|e| e.attention) {
            if e.windows.contains(&hwnd) {
                e.attention = false;
                changed = true;
            }
        }
        if changed {
            self.ensure_frame_timer();
        }
    }

    pub fn clear_attention(&mut self, idx: usize) {
        if let Some(e) = self.entries.get_mut(idx) {
            e.attention = false;
        }
    }

    /// The icon of an entry in screen coordinates (at rest size).
    pub fn entry_screen_rect(&self, idx: usize) -> Option<RECT> {
        let g = self.geo.as_ref()?;
        let (slots, _, _) = self.layout(g);
        let sl = slots.iter().find(|s| s.kind == SlotKind::Entry(idx))?;
        let off = -self.hide * g.slide_distance();
        let r = g.rect(sl.u0, sl.len, g.margin + off + g.pad + sl.lift, sl.len);
        Some(RECT {
            left: g.win.left + r.x.round() as i32,
            top: g.win.top + r.y.round() as i32,
            right: g.win.left + (r.x + r.w).round() as i32,
            bottom: g.win.top + (r.y + r.h).round() as i32,
        })
    }

    /// The entry under the pointer while the dock is simply being hovered
    /// (no press, drag or menu).
    pub fn hover_entry(&self) -> Option<usize> {
        if self.press.is_some() || self.drag.is_some() || self.menu_open || self.hover_target <= 0.0
        {
            return None;
        }
        self.hovered
    }

    pub fn edge(&self) -> Option<Edge> {
        self.geo.as_ref().map(|g| g.edge)
    }

    /// The panel rectangle in screen coordinates.
    pub fn panel_screen_rect(&self) -> Option<RECT> {
        let g = self.geo.as_ref()?;
        let (_, p0, plen) = self.layout(g);
        let off = -self.hide * g.slide_distance();
        let r = g.rect(p0, plen, g.margin + off, g.thick);
        Some(RECT {
            left: g.win.left + r.x as i32,
            top: g.win.top + r.y as i32,
            right: g.win.left + (r.x + r.w) as i32,
            bottom: g.win.top + (r.y + r.h) as i32,
        })
    }

    /// Re-checks which apps have windows open. Returns true if anything changed.
    pub fn refresh_running(&mut self, settings: &Settings) -> bool {
        let wins = apps::app_windows(self.own_pid);
        let mut used = vec![false; wins.len()];
        let mut changed = false;
        for e in self.entries.iter_mut().filter(|e| e.pinned) {
            let mut list = Vec::new();
            for (i, w) in wins.iter().enumerate() {
                if !used[i] && apps::window_matches(&e.keys, w) {
                    used[i] = true;
                    list.push(w.hwnd);
                }
            }
            if list != e.windows {
                changed = true;
                e.windows = list;
            }
        }

        let mut groups: Vec<(String, Entry)> = Vec::new();
        if settings.show_running_apps {
            for (i, w) in wins.iter().enumerate() {
                if used[i] {
                    continue;
                }
                let packaged = w.exe.is_empty() || w.exe.contains(r"\windowsapps\");
                let key = match (&w.aumid, packaged) {
                    (Some(a), true) => format!("aumid:{a}"),
                    _ if !w.exe.is_empty() => format!("exe:{}", w.exe),
                    (Some(a), false) => format!("aumid:{a}"),
                    _ => continue,
                };
                if let Some((_, e)) = groups.iter_mut().find(|(k, _)| *k == key) {
                    e.windows.push(w.hwnd);
                    continue;
                }
                let target = if packaged {
                    match &w.aumid_raw {
                        Some(a) => format!("{}{}", apps::APPS_FOLDER_PREFIX, a),
                        None => continue,
                    }
                } else {
                    w.exe.clone()
                };
                let name = self
                    .names
                    .entry(key.clone())
                    .or_insert_with(|| apps::friendly_name(&target))
                    .clone();
                let keys = MatchKeys {
                    exe: (!w.exe.is_empty()).then(|| w.exe.clone()),
                    aumid: w.aumid.clone(),
                    explorer: false,
                    exists: true,
                };
                let mut e = Entry::new(PinnedItem { name, target }, false, keys);
                e.run_key = key.clone();
                e.windows.push(w.hwnd);
                groups.push((key, e));
            }
        }

        // Keep the existing order of running (unpinned) entries.
        let mut old_running: Vec<Entry> = Vec::new();
        let mut kept: Vec<Entry> = Vec::new();
        for e in self.entries.drain(..) {
            if e.pinned {
                kept.push(e);
            } else {
                old_running.push(e);
            }
        }
        let mut new_running = Vec::new();
        for mut old in old_running {
            if let Some(pos) = groups.iter().position(|(k, _)| *k == old.run_key) {
                let (_, fresh) = groups.remove(pos);
                if fresh.windows != old.windows {
                    changed = true;
                }
                old.windows = fresh.windows;
                new_running.push(old);
            } else {
                changed = true;
            }
        }
        for (_, e) in groups {
            changed = true;
            new_running.push(e);
        }
        kept.extend(new_running);
        self.entries = kept;
        if changed {
            if let Some(h) = self.hovered {
                if h >= self.entries.len() {
                    self.hovered = None;
                }
            }
        }
        changed
    }

    // ----- geometry ------------------------------------------------------

    /// Recomputes size and placement. Call after settings, display or item
    /// changes.
    pub fn relayout(&mut self, settings: &Settings) {
        self.dark = system::is_dark(settings.theme);
        self.animate = system::animations_enabled(settings.animations);
        self.effect = settings.hover_effect;
        self.trash_end = settings.trash_at_end;
        let Some(mon) = system::pick_monitor(&settings.display) else {
            self.geo = None;
            return;
        };
        let scale = mon.scale();
        let edge = settings.position;
        let work = mon.work;
        let (avail_main, avail_cross) = match edge {
            Edge::Bottom => (
                (work.right - work.left) as f32,
                (work.bottom - work.top) as f32,
            ),
            _ => (
                (work.bottom - work.top) as f32,
                (work.right - work.left) as f32,
            ),
        };
        let n = self.entries.len().max(1) as f32;
        let seps = self.separator_count() as f32;
        let mut s = (settings.icon_size as f32 * scale).round();
        let gap = (settings.spacing as f32 * scale).round();
        let sep_w = (2.0 * scale).round().max(1.0) + gap;
        let sep_total = seps * sep_w;
        let mag = if settings.hover_effect == HoverEffect::Magnify {
            settings.magnification as f32 / 100.0
        } else {
            1.0
        };
        let sm = (20.0 * scale).round();
        let pad_for = |s: f32| (s * 0.16 + 4.0 * scale).round();

        // Shrink icons if they would not fit on the display.
        let fixed = |s: f32| {
            (n - 1.0) * gap
                + 2.0 * pad_for(s)
                + sep_total
                + 2.0 * sm
                + (mag - 1.0) * s * 2.62f32.min(n)
        };
        let max_len = avail_main - 8.0 * scale;
        if n * s + fixed(s) > max_len {
            let per = max_len / (n + 0.32 + (mag - 1.0) * 2.62f32.min(n));
            s = per.floor().max((16.0 * scale).round());
        }
        let pad = pad_for(s);
        let thick = s + 2.0 * pad;
        let margin = (settings.edge_margin as f32 * scale).round();
        let lift_h = (s * 0.22).round();
        let bounce_h = (s * 0.32).round();
        let label_font = (12.0 * scale).round() as i32;
        let label_extent = match edge {
            Edge::Bottom => label_font as f32 * 2.2 + 10.0 * scale,
            _ => 240.0 * scale,
        };
        let main = (n * s + fixed(s)).min(avail_main).ceil() as i32;
        let cross_need = margin
            + pad
            + s * mag.max(1.0)
            + lift_h.max(bounce_h)
            + 8.0 * scale
            + label_extent
            + sm;
        let cross = cross_need.min(avail_cross).ceil() as i32;

        let (w, h) = match edge {
            Edge::Bottom => (main, cross),
            _ => (cross, main),
        };
        let x = match edge {
            Edge::Bottom => work.left + ((work.right - work.left) - w) / 2,
            Edge::Left => work.left,
            Edge::Right => work.right - w,
        };
        let y = match edge {
            Edge::Bottom => work.bottom - h,
            _ => work.top + ((work.bottom - work.top) - h) / 2,
        };
        let radius = (thick * 0.36).round();
        self.geo = Some(Geometry {
            monitor: mon,
            scale,
            edge,
            s,
            gap,
            pad,
            thick,
            margin,
            radius,
            mag,
            lift_h,
            bounce_h,
            sep_w,
            label_font,
            win: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            w,
            h,
        });
        for e in &mut self.entries {
            e.label = None;
        }
        self.preload_icons();
        if !settings.auto_hide {
            self.hide_target = 0.0;
            self.hide = 0.0;
        }
    }

    fn icon_sizes(&self) -> (u32, u32) {
        match &self.geo {
            Some(g) => {
                let base = g.s.round().max(1.0) as u32;
                let big = (g.s * g.mag).round().max(1.0) as u32;
                (base, big)
            }
            None => (48, 48),
        }
    }

    fn preload_icons(&mut self) {
        let (base, big) = self.icon_sizes();
        let targets: Vec<String> = self.entries.iter().map(|e| e.item.target.clone()).collect();
        for t in &targets {
            let _ = self.icons.get(t, base);
            if big != base {
                let _ = self.icons.get(t, big);
            }
        }
    }

    // ----- layout --------------------------------------------------------

    fn layout(&self, g: &Geometry) -> (Vec<Slot>, f32, f32) {
        // Element order, with the dragged icon replaced by a drop gap.
        let mut order: Vec<SlotKind> = Vec::new();
        let bin = self.trailing_bin();
        let pinned: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].pinned && Some(i) != bin)
            .collect();
        let running: Vec<usize> = (0..self.entries.len())
            .filter(|&i| !self.entries[i].pinned)
            .collect();
        match &self.drag {
            Some(d) if d.outside => order.extend(
                pinned
                    .iter()
                    .copied()
                    .filter(|&i| i != d.idx)
                    .map(SlotKind::Entry),
            ),
            Some(d) => {
                let mut rest: Vec<usize> = pinned.iter().copied().filter(|&i| i != d.idx).collect();
                let at = d.insert.min(rest.len());
                rest.insert(at, usize::MAX);
                for i in rest {
                    order.push(if i == usize::MAX {
                        SlotKind::DropGap
                    } else {
                        SlotKind::Entry(i)
                    });
                }
            }
            None => order.extend(pinned.iter().map(|&i| SlotKind::Entry(i))),
        }
        if !pinned.is_empty() && !running.is_empty() {
            order.push(SlotKind::Separator);
        }
        order.extend(running.iter().map(|&i| SlotKind::Entry(i)));
        if let Some(b) = bin {
            if !order.is_empty() {
                order.push(SlotKind::Separator);
            }
            order.push(SlotKind::Entry(b));
        }

        let base_len = |k: &SlotKind| match k {
            SlotKind::Separator => g.sep_w,
            _ => g.s,
        };
        let count = order.len().max(1) as f32;
        let total: f32 =
            order.iter().map(base_len).sum::<f32>() + (count - 1.0) * g.gap + 2.0 * g.pad;
        let center = g.main_len() / 2.0;

        // Magnification follows the pointer position in the resting layout,
        // so icons never "run away" from the pointer.
        let mut sizes: Vec<f32> = order.iter().map(base_len).collect();
        if g.mag > 1.0 && self.hover > 0.0 && self.drag.is_none() {
            if let Some((mx, my)) = self.mouse {
                let (cu, _) = g.to_uv(mx, my);
                let mut u = center - total / 2.0 + g.pad;
                for (i, k) in order.iter().enumerate() {
                    let len = base_len(k);
                    if let SlotKind::Entry(_) = k {
                        let c = u + len / 2.0;
                        let f = falloff((cu - c).abs() / (g.s + g.gap));
                        sizes[i] = g.s * (1.0 + (g.mag - 1.0) * f * self.hover);
                    }
                    u += len + g.gap;
                }
            }
        }
        let len: f32 = sizes.iter().sum::<f32>() + (count - 1.0) * g.gap + 2.0 * g.pad;
        let start = center - len / 2.0;
        let mut u = start + g.pad;
        let mut slots = Vec::with_capacity(order.len());
        for (i, k) in order.iter().enumerate() {
            let lift = match k {
                SlotKind::Entry(e) => {
                    let ent = &self.entries[*e];
                    let mut l = 0.0;
                    if self.drag.is_none() && g.mag <= 1.0 {
                        l += ent.hl
                            * g.lift_h
                            * if g.lift_h > 0.0 && self.lift_effect() {
                                1.0
                            } else {
                                0.0
                            };
                    }
                    if let Some(b) = ent.bounce.and_then(|t| bounce_offset(t, g.bounce_h)) {
                        l += b;
                    }
                    l
                }
                _ => 0.0,
            };
            slots.push(Slot {
                kind: *k,
                u0: u,
                len: sizes[i],
                lift,
            });
            u += sizes[i] + g.gap;
        }
        (slots, start, len)
    }

    fn lift_effect(&self) -> bool {
        self.effect == HoverEffect::Lift
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let g = self.geo.as_ref()?;
        let (u, v) = g.to_uv(x, y);
        let off = self.hide * g.slide_distance();
        let (slots, _, _) = self.layout(g);
        for sl in &slots {
            if let SlotKind::Entry(i) = sl.kind {
                let top = g.margin + g.pad + sl.len + sl.lift - off;
                if u >= sl.u0 - g.gap / 2.0
                    && u < sl.u0 + sl.len + g.gap / 2.0
                    && v >= 0.0
                    && v <= top
                {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Where a dragged or dropped icon would land among the pinned icons.
    fn insert_index_at(&self, x: f32, y: f32) -> usize {
        let Some(g) = self.geo.as_ref() else {
            return self.pinned_count();
        };
        let (u, _) = g.to_uv(x, y);
        let (slots, _, _) = self.layout(g);
        let mut idx = 0;
        for sl in &slots {
            match sl.kind {
                SlotKind::Entry(i) if self.entries[i].pinned => {
                    if Some(i) == self.drag.as_ref().map(|d| d.idx) {
                        continue;
                    }
                    if u > sl.u0 + sl.len / 2.0 {
                        idx += 1;
                    }
                }
                SlotKind::DropGap => {
                    if u > sl.u0 + sl.len / 2.0 {
                        // the gap itself occupies no index
                    }
                }
                _ => {}
            }
        }
        idx
    }

    /// The area in which the pointer keeps the dock "hovered".
    fn in_hover_zone(&self, sx: i32, sy: i32) -> Option<(f32, f32)> {
        let g = self.geo.as_ref()?;
        let x = (sx - g.win.left) as f32;
        let y = (sy - g.win.top) as f32;
        if x < 0.0 || y < 0.0 || x >= g.w as f32 || y >= g.h as f32 {
            return None;
        }
        let (u, v) = g.to_uv(x, y);
        let (_, p0, plen) = self.layout(g);
        let top = g.margin + g.pad + g.s * g.mag.max(1.0) + g.lift_h;
        if u >= p0 - g.gap && u <= p0 + plen + g.gap && v >= 0.0 && v <= top {
            Some((x, y))
        } else {
            None
        }
    }

    // ----- input ---------------------------------------------------------

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = Some((x, y));
        self.hover_target = 1.0;
        crate::preview::poke();
        if let Some(p) = &self.press {
            let dist = ((x - p.x).powi(2) + (y - p.y).powi(2)).sqrt();
            let threshold = 6.0 * self.geo.as_ref().map(|g| g.scale).unwrap_or(1.0);
            if self.drag.is_none()
                && dist > threshold
                && self.entries.get(p.idx).map(|e| e.pinned) == Some(true)
                && self.trailing_bin() != Some(p.idx)
            {
                self.drag = Some(Drag {
                    idx: p.idx,
                    insert: 0,
                    u: 0.0,
                    v: 0.0,
                    outside: false,
                });
            }
        }
        if self.drag.is_some() {
            let outside = self.far_from_dock(x, y);
            if let Some(d) = &mut self.drag {
                d.outside = outside;
            }
            crate::preview::hide();
            let insert = self.insert_index_at(x, y);
            let (u, v) = self
                .geo
                .as_ref()
                .map(|g| g.to_uv(x, y))
                .unwrap_or((0.0, 0.0));
            if let Some(d) = &mut self.drag {
                d.insert = insert;
                d.u = u;
                d.v = v;
            }
        } else {
            self.hovered = self.hit(x, y);
        }
        self.ensure_frame_timer();
    }

    pub fn on_button_down(&mut self, x: f32, y: f32) -> bool {
        match self.hit(x, y) {
            Some(i) => {
                self.press = Some(Press { idx: i, x, y });
                self.ensure_frame_timer();
                true
            }
            None => false,
        }
    }

    /// Whether a dragged icon is far enough from the dock to be removed
    /// when released.
    fn far_from_dock(&self, x: f32, y: f32) -> bool {
        let Some(g) = self.geo.as_ref() else {
            return false;
        };
        let (u, v) = g.to_uv(x, y);
        let (_, p0, plen) = self.layout(g);
        v > g.margin + g.thick + g.s * 0.9 || u < p0 - g.s * 1.5 || u > p0 + plen + g.s * 1.5
    }

    pub fn on_button_up(&mut self, x: f32, y: f32) -> Released {
        let press = self.press.take();
        if let Some(d) = self.drag.take() {
            self.ensure_frame_timer();
            let from = self.entries[..d.idx].iter().filter(|e| e.pinned).count();
            if self.far_from_dock(x, y) {
                return Released::Remove(from);
            }
            return Released::Reorder(from, d.insert);
        }
        match press {
            Some(p) if self.hit(x, y) == Some(p.idx) => Released::Click(p.idx),
            _ => Released::Nothing,
        }
    }

    pub fn cancel_press(&mut self) {
        self.press = None;
        if self.drag.take().is_some() {
            self.ensure_frame_timer();
        }
    }

    pub fn drop_index(&self, x: f32, y: f32) -> usize {
        self.insert_index_at(x, y)
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        self.hit(x, y)
    }

    pub fn start_bounce(&mut self, idx: usize) {
        if self.animate {
            if let Some(e) = self.entries.get_mut(idx) {
                e.bounce = Some(Instant::now());
                self.ensure_frame_timer();
            }
        }
    }

    // ----- timers --------------------------------------------------------

    pub fn ensure_frame_timer(&mut self) {
        if !self.frame_timer {
            self.frame_timer = true;
            self.last_tick = Instant::now();
            unsafe { SetTimer(Some(self.hwnd), TIMER_FRAME, FRAME_MS, None) };
        }
    }

    fn stop_frame_timer(&mut self) {
        if self.frame_timer {
            self.frame_timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_FRAME);
            }
        }
    }

    pub fn start_poll_timer(&self) {
        unsafe { SetTimer(Some(self.hwnd), TIMER_POLL, POLL_MS, None) };
    }

    fn cursor() -> Option<(i32, i32)> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p).ok()? };
        Some((p.x, p.y))
    }

    /// Animation frame.
    pub fn on_frame(&mut self, settings: &Settings) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().clamp(0.0, 0.1);
        self.last_tick = now;
        let animate = self.animate;
        let mut busy = false;

        // Track the pointer while hovered (layered windows only report
        // movement over opaque pixels).
        if self.hover_target > 0.0 && !self.menu_open && self.drag.is_none() && self.press.is_none()
        {
            let inside = Self::cursor().and_then(|(x, y)| self.in_hover_zone(x, y));
            match inside {
                Some((x, y)) => {
                    self.mouse = Some((x, y));
                    self.hovered = self.hit(x, y);
                }
                None if crate::preview::holds_hover() => {}
                None => {
                    self.hover_target = 0.0;
                    self.hovered = None;
                }
            }
        }
        busy |= approach(&mut self.hover, self.hover_target, dt, 0.07, animate);
        for i in 0..self.entries.len() {
            let target = if self.hovered == Some(i) && self.drag.is_none() {
                1.0
            } else {
                0.0
            };
            busy |= approach(&mut self.entries[i].hl, target, dt, 0.06, animate);
            if let Some(b) = self.entries[i].bounce {
                if b.elapsed() >= BOUNCE {
                    self.entries[i].bounce = None;
                    self.entries[i].bounce_done = Some(Instant::now());
                }
                busy = true;
            }
        }
        busy |= approach(&mut self.hide, self.hide_target, dt, 0.08, animate);
        if self.hover_target > 0.0 || self.press.is_some() || self.drag.is_some() {
            busy = true;
        }
        self.render(settings);
        if !busy {
            self.stop_frame_timer();
        }
    }

    /// Slow timer: running apps, full-screen detection, auto-hide.
    pub fn on_poll(&mut self, settings: &Settings) {
        self.poll_count = self.poll_count.wrapping_add(1);
        let mut needs_frame = false;

        if self.poll_count % 7 == 1 {
            let before = self.entries.len();
            if self.refresh_running(settings) {
                if self.entries.len() != before {
                    self.relayout(settings);
                }
                needs_frame = true;
            }
        }

        // Keep bouncing icons whose windows want attention.
        if self.animate {
            for e in self
                .entries
                .iter_mut()
                .filter(|e| e.attention && e.bounce.is_none())
            {
                let rested = e
                    .bounce_done
                    .map(|t| t.elapsed() >= Duration::from_millis(650))
                    .unwrap_or(true);
                if rested {
                    e.bounce = Some(Instant::now());
                    needs_frame = true;
                }
            }
        }

        // Show the Recycle Bin as empty or full.
        if self.poll_count % 20 == 3 && self.bin_index().is_some() {
            let full = apps::recycle_bin_full();
            if full.is_some() && full != self.bin_full {
                if self.bin_full.is_some() {
                    self.icons.forget(config::RECYCLE_BIN);
                    needs_frame = true;
                }
                self.bin_full = full;
            }
        }

        // Hide while a full-screen app (video, game, slideshow) is in front.
        let fullscreen = settings.hide_in_fullscreen
            && self
                .geo
                .as_ref()
                .map(|g| system::fullscreen_app_on(g.monitor.handle, &[self.hwnd]))
                .unwrap_or(false);
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            needs_frame = true;
        }

        if settings.auto_hide {
            self.update_auto_hide();
        }

        if self.poll_count % 14 == 0 && self.shown {
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
        }
        if needs_frame || self.hide != self.hide_target {
            self.ensure_frame_timer();
        }
    }

    fn update_auto_hide(&mut self) {
        let Some(g) = self.geo.clone() else { return };
        let Some((cx, cy)) = Self::cursor() else {
            return;
        };
        let busy = self.hover_target > 0.0
            || crate::preview::is_open()
            || self.menu_open
            || self.drag.is_some()
            || self.press.is_some();
        let in_window =
            cx >= g.win.left && cx < g.win.right && cy >= g.win.top && cy < g.win.bottom;
        if self.hide_target == 0.0 {
            let grace = self
                .reveal_grace
                .map(|t| t.elapsed() < Duration::from_millis(1200))
                .unwrap_or(false);
            if busy || grace || (in_window && self.hide < 0.5) {
                self.outside_since = None;
            } else {
                let since = *self.outside_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(700) {
                    self.hide_target = 1.0;
                    self.outside_since = None;
                }
            }
        } else {
            // Reveal when the pointer rests against the screen edge in
            // front of the dock.
            let m = g.monitor.rect;
            let band = (2.0 * g.scale).ceil() as i32;
            let at_edge = match g.edge {
                Edge::Bottom => cy >= m.bottom - band && cx >= g.win.left && cx < g.win.right,
                Edge::Left => cx < m.left + band && cy >= g.win.top && cy < g.win.bottom,
                Edge::Right => cx >= m.right - band && cy >= g.win.top && cy < g.win.bottom,
            };
            if at_edge {
                let since = *self.edge_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(250) {
                    self.reveal();
                }
            } else {
                self.edge_since = None;
            }
        }
    }

    pub fn reveal(&mut self) {
        self.hide_target = 0.0;
        self.edge_since = None;
        self.outside_since = None;
        self.reveal_grace = Some(Instant::now());
        self.ensure_frame_timer();
    }

    // ----- rendering -----------------------------------------------------

    fn ensure_buffers(&mut self, w: i32, h: i32) -> bool {
        let ok = matches!(&self.surface, Some(s) if s.w == w && s.h == h);
        if !ok {
            self.surface = None;
            unsafe {
                let dc = CreateCompatibleDC(None);
                if dc.is_invalid() {
                    return false;
                }
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                    Ok(dib) if !bits.is_null() => {
                        let old = SelectObject(dc, HGDIOBJ(dib.0));
                        self.surface = Some(Surface {
                            dc,
                            dib,
                            old,
                            bits: bits as *mut u8,
                            w,
                            h,
                        });
                    }
                    _ => {
                        let _ = DeleteDC(dc);
                        return false;
                    }
                }
            }
            self.pixmap = Pixmap::new(w as u32, h as u32);
            self.scratch = Pixmap::new(w as u32, h as u32);
        }
        self.pixmap.is_some() && self.scratch.is_some()
    }

    /// Draws the dock into an off-screen pixmap (also used for previews).
    pub fn paint(&mut self, settings: &Settings) -> bool {
        let Some(g) = self.geo.clone() else {
            return false;
        };
        if !self.ensure_buffers(g.w, g.h) {
            return false;
        }
        let (slots, p0, plen) = self.layout(&g);
        let (base_px, big_px) = self.icon_sizes();
        let dark = self.dark;
        let off = -self.hide * g.slide_distance();
        let mut pm = self.pixmap.take().unwrap();
        let mut scratch = self.scratch.take().unwrap();
        pm.fill(Color::TRANSPARENT);

        // Panel.
        let opacity = settings.opacity as f32 / 100.0;
        let panel = g.rect(p0, plen, g.margin + off, g.thick);
        let shadow_off = match g.edge {
            Edge::Bottom => (0.0, 2.0 * g.scale),
            Edge::Left => (2.0 * g.scale, 0.0),
            Edge::Right => (-2.0 * g.scale, 0.0),
        };
        draw::soft_shadow(
            &mut pm,
            &mut scratch,
            panel,
            g.radius,
            16.0 * g.scale,
            (shadow_off.0 * 2.0, shadow_off.1 * 2.0),
            if dark { 0.60 } else { 0.28 },
        );
        if let Some(path) = draw::rounded_rect(panel, g.radius) {
            // Frosted glass: a translucent body, brighter towards the side
            // facing away from the screen edge, with a bright rim on that side.
            let base = if dark {
                rgba(30, 30, 34, opacity)
            } else {
                rgba(246, 246, 249, opacity)
            };
            draw::fill(&mut pm, &path, base);
            let far = g.point(p0, g.margin + off + g.thick);
            let near = g.point(p0, g.margin + off);
            let (top_a, bot_a) = if dark { (0.10, 0.0) } else { (0.45, 0.06) };
            if let Some(sh) = draw::gradient(
                far,
                near,
                rgba(255, 255, 255, top_a),
                rgba(255, 255, 255, bot_a),
            ) {
                let mut paint = draw::solid(Color::WHITE);
                paint.shader = sh;
                draw::fill_with(&mut pm, &path, &paint);
            }
            let outer = if dark {
                rgba(0, 0, 0, 0.60)
            } else {
                rgba(0, 0, 0, 0.10)
            };
            draw::stroke(&mut pm, &path, outer, 1.0);
            if let Some(inner) = draw::rounded_rect(panel.inset(1.0), (g.radius - 1.0).max(0.0)) {
                let (rim_far, rim_near) = if dark { (0.30, 0.04) } else { (0.95, 0.25) };
                if let Some(sh) = draw::gradient(
                    far,
                    g.point(p0, g.margin + off + g.thick * 0.35),
                    rgba(255, 255, 255, rim_far),
                    rgba(255, 255, 255, rim_near),
                ) {
                    let mut paint = draw::solid(Color::WHITE);
                    paint.shader = sh;
                    draw::stroke_with(&mut pm, &inner, &paint, 1.0);
                }
            }
        }

        let dot_color = if dark {
            rgba(240, 240, 245, 0.85)
        } else {
            rgba(28, 28, 32, 0.65)
        };
        let hl_color = if dark {
            rgba(255, 255, 255, 0.10)
        } else {
            rgba(0, 0, 0, 0.07)
        };
        let highlight = settings.hover_effect == HoverEffect::Highlight;

        let mut label_for: Option<(usize, Slot)> = None;
        for sl in &slots {
            match sl.kind {
                SlotKind::Separator => {
                    let r = g.rect(
                        sl.u0 + sl.len / 2.0 - 0.5 * g.scale,
                        (1.0 * g.scale).max(1.0),
                        g.margin + off + g.pad * 0.7,
                        g.thick - 1.4 * g.pad,
                    );
                    if let Some(p) = draw::rounded_rect(r, 0.5 * g.scale) {
                        draw::fill(
                            &mut pm,
                            &p,
                            if dark {
                                rgba(255, 255, 255, 0.18)
                            } else {
                                rgba(0, 0, 0, 0.14)
                            },
                        );
                    }
                }
                SlotKind::DropGap => {}
                SlotKind::Entry(i) => {
                    let (target, hl, exists, pinned, running) = {
                        let e = &self.entries[i];
                        (
                            e.item.target.clone(),
                            e.hl,
                            e.keys.exists,
                            e.pinned,
                            e.running(),
                        )
                    };
                    let v0 = g.margin + off + g.pad + sl.lift;
                    let r = g.rect(sl.u0, sl.len, v0, sl.len);
                    if highlight && hl > 0.01 {
                        let hr = g.rect(
                            sl.u0 - g.pad * 0.5,
                            sl.len + g.pad,
                            g.margin + off + g.pad * 0.5,
                            sl.len + g.pad,
                        );
                        if let Some(p) = draw::rounded_rect(hr, g.radius * 0.7) {
                            let mut c = hl_color;
                            c.set_alpha(c.alpha() * hl);
                            draw::fill(&mut pm, &p, c);
                        }
                    }
                    let pressed = self.press.as_ref().map(|p| p.idx == i).unwrap_or(false)
                        && self.drag.is_none();
                    let mut alpha = if exists || !pinned { 1.0 } else { 0.45 };
                    if pressed {
                        alpha *= 0.7;
                    }
                    self.draw_icon(&mut pm, &target, r, base_px, big_px, alpha, dark);
                    let attention = self.entries[i].attention;
                    if (settings.show_running_indicators && running) || attention {
                        let (cx, cy) = g.point(sl.u0 + sl.len / 2.0, g.margin + off + g.pad * 0.42);
                        if let Some(p) = draw::circle(cx, cy, (2.2 * g.scale).max(1.5)) {
                            let c = if attention {
                                rgba(255, 149, 0, 1.0)
                            } else {
                                dot_color
                            };
                            draw::fill(&mut pm, &p, c);
                        }
                    }
                    if self.hovered == Some(i) && self.drag.is_none() && !self.menu_open {
                        label_for = Some((i, *sl));
                    }
                }
            }
        }

        // The icon being dragged follows the pointer; away from the dock it
        // turns translucent with a "Remove" tag.
        if let Some((idx, du, dv, outside)) =
            self.drag.as_ref().map(|d| (d.idx, d.u, d.v, d.outside))
        {
            if let Some(e) = self.entries.get(idx) {
                let target = e.item.target.clone();
                let u0 = du - g.s / 2.0;
                let rest = g.margin + off + g.pad + g.lift_h * 0.5;
                let cross = match g.edge {
                    Edge::Bottom => g.h as f32,
                    _ => g.w as f32,
                };
                let v0 = if outside {
                    let tag = g.label_font as f32 * 2.4 + 12.0 * g.scale;
                    (dv - g.s / 2.0).clamp(rest, (cross - g.s - tag).max(rest))
                } else {
                    rest
                };
                let r = g.rect(u0, g.s, v0, g.s);
                self.draw_icon(
                    &mut pm,
                    &target,
                    r,
                    base_px,
                    big_px,
                    if outside { 0.5 } else { 0.85 },
                    dark,
                );
                if outside {
                    self.draw_remove_tag(&mut pm, &g, du, v0 + g.s + 6.0 * g.scale);
                }
            }
        }

        if let Some((i, sl)) = label_for {
            if self.hover > 0.5 {
                self.draw_label(&mut pm, &g, i, sl, off);
            }
        }

        self.pixmap = Some(pm);
        self.scratch = Some(scratch);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_icon(
        &mut self,
        pm: &mut Pixmap,
        target: &str,
        r: RectF,
        base_px: u32,
        big_px: u32,
        alpha: f32,
        dark: bool,
    ) {
        let use_base = (r.w - base_px as f32).abs() < 0.5;
        let px = if use_base { base_px } else { big_px };
        let paint = PixmapPaint {
            opacity: alpha,
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        match self.icons.get(target, px) {
            Some(icon) => {
                if use_base {
                    pm.draw_pixmap(
                        r.x.round() as i32,
                        r.y.round() as i32,
                        icon.as_ref(),
                        &paint,
                        Transform::identity(),
                        None,
                    );
                } else {
                    let k = r.w / icon.width() as f32;
                    pm.draw_pixmap(
                        0,
                        0,
                        icon.as_ref(),
                        &paint,
                        Transform::from_row(k, 0.0, 0.0, k, r.x, r.y),
                        None,
                    );
                }
            }
            None => {
                // Placeholder for a target whose icon is unavailable.
                let inner = r.inset(r.w * 0.1);
                if let Some(p) = draw::rounded_rect(inner, inner.w * 0.22) {
                    let fillc = if dark {
                        rgba(255, 255, 255, 0.14 * alpha)
                    } else {
                        rgba(0, 0, 0, 0.10 * alpha)
                    };
                    draw::fill(pm, &p, fillc);
                    let line = if dark {
                        rgba(255, 255, 255, 0.45 * alpha)
                    } else {
                        rgba(0, 0, 0, 0.35 * alpha)
                    };
                    draw::stroke(pm, &p, line, (r.w / 32.0).max(1.0));
                }
                if let Some(p) = draw::circle(inner.cx(), inner.cy(), inner.w * 0.12) {
                    let c = if dark {
                        rgba(255, 255, 255, 0.45 * alpha)
                    } else {
                        rgba(0, 0, 0, 0.35 * alpha)
                    };
                    draw::fill(pm, &p, c);
                }
            }
        }
    }

    fn draw_label(&mut self, pm: &mut Pixmap, g: &Geometry, i: usize, sl: Slot, off: f32) {
        let dark = self.dark;
        let e = &mut self.entries[i];
        let text = if e.pinned && !e.keys.exists {
            format!("{} (not found)", e.item.name)
        } else {
            e.item.name.clone()
        };
        let stale = !matches!(&e.label, Some((t, f, d, _)) if *t == text && *f == g.label_font && *d == dark);
        if stale {
            let color = if dark { (242, 242, 245) } else { (28, 28, 32) };
            e.label = text::render(&text, g.label_font, (220.0 * g.scale) as i32, color)
                .map(|p| (text.clone(), g.label_font, dark, p));
        }
        let Some((_, _, _, tp)) = &e.label else {
            return;
        };
        let padx = 9.0 * g.scale;
        let pady = 3.5 * g.scale;
        let pw = tp.width() as f32 + 2.0 * padx;
        let ph = tp.height() as f32 + 2.0 * pady;
        let gap = 8.0 * g.scale;
        let v = g.margin + off + g.pad + sl.lift + sl.len + gap;
        let cu = sl.u0 + sl.len / 2.0;
        let main = g.main_len();
        let rect = match g.edge {
            Edge::Bottom => {
                let u0 = (cu - pw / 2.0).clamp(2.0, (main - pw - 2.0).max(2.0));
                g.rect(u0, pw, v, ph)
            }
            _ => {
                let u0 = (cu - ph / 2.0).clamp(2.0, (main - ph - 2.0).max(2.0));
                g.rect(u0, ph, v, pw)
            }
        };
        let rect = RectF::new(
            rect.x.round(),
            rect.y.round(),
            rect.w.round(),
            rect.h.round(),
        );
        if let Some(p) = draw::rounded_rect(rect, 6.0 * g.scale) {
            let bg = if dark {
                rgba(44, 44, 48, 0.94)
            } else {
                rgba(250, 250, 252, 0.96)
            };
            draw::fill(pm, &p, bg);
            let border = if dark {
                rgba(255, 255, 255, 0.12)
            } else {
                rgba(0, 0, 0, 0.12)
            };
            draw::stroke(pm, &p, border, 1.0);
        }
        pm.draw_pixmap(
            (rect.x + padx).round() as i32,
            (rect.y + pady).round() as i32,
            tp.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    fn draw_remove_tag(&mut self, pm: &mut Pixmap, g: &Geometry, cu: f32, v: f32) {
        let dark = self.dark;
        let stale =
            !matches!(&self.remove_label, Some((f, d, _)) if *f == g.label_font && *d == dark);
        if stale {
            let color = if dark { (242, 242, 245) } else { (28, 28, 32) };
            self.remove_label =
                text::render("Remove", g.label_font, (200.0 * g.scale) as i32, color)
                    .map(|p| (g.label_font, dark, p));
        }
        let Some((_, _, tp)) = &self.remove_label else {
            return;
        };
        let padx = 9.0 * g.scale;
        let pady = 3.5 * g.scale;
        let pw = tp.width() as f32 + 2.0 * padx;
        let ph = tp.height() as f32 + 2.0 * pady;
        let rect = match g.edge {
            Edge::Bottom => g.rect(cu - pw / 2.0, pw, v, ph),
            _ => g.rect(cu - ph / 2.0, ph, v, pw),
        };
        let rect = RectF::new(
            rect.x.round(),
            rect.y.round(),
            rect.w.round(),
            rect.h.round(),
        );
        if let Some(p) = draw::rounded_rect(rect, 6.0 * g.scale) {
            let bg = if dark {
                rgba(44, 44, 48, 0.94)
            } else {
                rgba(250, 250, 252, 0.96)
            };
            draw::fill(pm, &p, bg);
            draw::stroke(pm, &p, rgba(0, 0, 0, 0.12), 1.0);
        }
        pm.draw_pixmap(
            (rect.x + padx).round() as i32,
            (rect.y + pady).round() as i32,
            tp.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    /// Paints and pushes the result to the screen.
    pub fn render(&mut self, settings: &Settings) {
        let visible = !self.user_hidden
            && !self.fullscreen
            && !(settings.auto_hide && self.hide >= 0.999)
            && self.geo.is_some();
        if visible && self.paint(settings) {
            let g = self.geo.as_ref().unwrap();
            let (Some(pm), Some(surf)) = (&self.pixmap, &self.surface) else {
                return;
            };
            unsafe {
                let dst = std::slice::from_raw_parts_mut(surf.bits, (surf.w * surf.h * 4) as usize);
                for (d, s) in dst.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
                    d[0] = s[2];
                    d[1] = s[1];
                    d[2] = s[0];
                    d[3] = s[3];
                }
                let pos = POINT {
                    x: g.win.left,
                    y: g.win.top,
                };
                let size = SIZE { cx: g.w, cy: g.h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: 0,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: 1,
                };
                if let Err(e) = UpdateLayeredWindow(
                    self.hwnd,
                    None,
                    Some(&pos),
                    Some(&size),
                    Some(surf.dc),
                    Some(&src),
                    windows::Win32::Foundation::COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                ) {
                    if self.last_error.is_none() {
                        crate::util::log(&format!("UpdateLayeredWindow failed: {e}"));
                    }
                    self.last_error = Some(e.to_string());
                } else {
                    self.last_error = None;
                }
            }
        }
        self.set_shown(visible);
    }

    fn set_shown(&mut self, show: bool) {
        if show != self.shown {
            self.shown = show;
            unsafe {
                let _ = ShowWindow(self.hwnd, if show { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            if show {
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                    );
                }
            }
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Writes the current frame to a PNG (used by `--render-preview`).
    pub fn save_png(&self, path: &str) -> Result<(), String> {
        let pm = self.pixmap.as_ref().ok_or("nothing rendered")?;
        pm.save_png(path).map_err(|e| e.to_string())
    }

    /// Automated checks of the dock's own interaction logic (`--self-test`).
    pub fn self_test(&mut self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let Some(g) = self.geo.clone() else {
            out.push(("dock geometry".into(), false));
            return out;
        };
        let center = |d: &Dock, i: usize| -> Option<(f32, f32)> {
            let (slots, _, _) = d.layout(&g);
            let sl = slots.iter().find(|s| s.kind == SlotKind::Entry(i))?;
            Some(g.point(sl.u0 + sl.len / 2.0, g.margin + g.pad + sl.len / 2.0))
        };
        // The Recycle Bin is drawn last, after a divider.
        if let Some(b) = self.bin_index() {
            let (slots, _, _) = self.layout(&g);
            let last = slots.last().map(|s| s.kind);
            let before = slots.len().checked_sub(2).map(|i| slots[i].kind);
            out.push((
                "Recycle Bin at the end".into(),
                !self.trash_end
                    || (last == Some(SlotKind::Entry(b)) && before == Some(SlotKind::Separator)),
            ));
        }
        let first =
            (0..self.entries.len()).find(|&i| self.entries[i].pinned && !self.entries[i].is_bin());
        let third = (0..self.entries.len())
            .filter(|&i| self.entries[i].pinned && !self.entries[i].is_bin())
            .nth(2);
        if let (Some(a), Some(c)) = (first, third) {
            // Drag the first icon over the third: a reorder.
            if let (Some((ax, ay)), Some((cx, cy))) = (center(self, a), center(self, c)) {
                self.on_button_down(ax, ay);
                self.on_mouse_move((ax + cx) / 2.0, (ay + cy) / 2.0);
                self.on_mouse_move(cx, cy);
                let r = self.on_button_up(cx, cy);
                out.push((
                    "drag to reorder".into(),
                    matches!(r, Released::Reorder(0, to) if to >= 1),
                ));
            }
            // Drag the first icon well away from the dock: a removal.
            if let Some((ax, ay)) = center(self, a) {
                let (au, av) = g.to_uv(ax, ay);
                let (fx, fy) = g.point(au, av + g.s * 3.0);
                self.on_button_down(ax, ay);
                self.on_mouse_move(ax, ay);
                self.on_mouse_move((ax + fx) / 2.0, (ay + fy) / 2.0);
                self.on_mouse_move(fx, fy);
                let outside = self.drag.as_ref().map(|d| d.outside).unwrap_or(false);
                let painted = self.paint(&Settings::default());
                let r = self.on_button_up(fx, fy);
                out.push((
                    "drag off the dock to remove".into(),
                    outside && painted && matches!(r, Released::Remove(0)),
                ));
            }
            // A plain click.
            if let Some((ax, ay)) = center(self, a) {
                self.on_button_down(ax, ay);
                let r = self.on_button_up(ax, ay);
                out.push(("click".into(), matches!(r, Released::Click(i) if i == a)));
            }
            // Attention: a flashing window makes its icon bounce until it is
            // activated.
            let fake = HWND(0x5eed as *mut _);
            let saved = std::mem::take(&mut self.entries[a].windows);
            self.entries[a].windows = vec![fake];
            self.flash(fake);
            let on = self.entries[a].attention;
            self.activated(fake);
            let off = !self.entries[a].attention;
            self.entries[a].windows = saved;
            out.push(("attention bounce".into(), on && off));
        }
        self.press = None;
        self.drag = None;
        out
    }

    /// Forces the hover state for previews.
    pub fn preview_hover(&mut self, idx: usize) {
        if let Some(g) = &self.geo {
            let (slots, _, _) = self.layout(g);
            if let Some(sl) = slots.iter().find(|s| s.kind == SlotKind::Entry(idx)) {
                let u = sl.u0 + sl.len / 2.0;
                let (x, y) = g.point(u, g.margin + g.thick / 2.0);
                self.mouse = Some((x, y));
                self.hover = 1.0;
                self.hover_target = 1.0;
                self.hovered = Some(idx);
                for (i, e) in self.entries.iter_mut().enumerate() {
                    e.hl = if i == idx { 1.0 } else { 0.0 };
                }
            }
        }
    }
}
