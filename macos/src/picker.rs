//! Select Image (Enter): a scrolling grid of thumbnails over the viewer.
//! Only visible cells are decoded. The job list is replaced on every frame with the
//! visible thumbnails not yet made, and workers take the newest job first, so fast
//! scrolling through 46,000 images never queues work for rows already gone.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Condvar, Mutex,
    },
};

use image::RgbaImage;
use winit::event_loop::EventLoopProxy;

use crate::{
    app::UserEvent,
    render::{self, Canvas},
    source::Entry,
    text::{Face, Text},
};

/// Design px: thumbnail box, cell size, gaps.
pub const THUMB: f32 = 160.0;
const CELL_W: f32 = 184.0;
const CELL_H: f32 = 208.0;
const TOP: f32 = 52.0;
const SIDE: f32 = 24.0;
/// The grid is a panel inset from every window edge, with rounded corners, a close button
/// in its header and 16 px under the last row. (Panel and close button added 2026-11-16
/// by Claude on the Mac, for Windows and macOS; see docs/windows-handoff.md.)
const INSET: f32 = 32.0;
const PANEL_R: f32 = 16.0;
const BOTTOM: f32 = 16.0;
/// Close button: a 32 px square, 10 px from the panel's top and right edges, centred on the header.
const CLOSE: f32 = 32.0;
const CLOSE_INSET: f32 = 10.0;
/// Thumbnails kept in memory (160 px RGBA is about 100 KB each).
const CACHE_MAX: usize = 900;

pub struct Jobs {
    list: Mutex<Vec<Entry>>,
    cv: Condvar,
    busy: Mutex<HashSet<Entry>>,
    pub size: AtomicU32,
    /// Disk thumbnail cache on (setting thumb_cache_mb > 0).
    pub disk: AtomicBool,
}

impl Jobs {
    /// The grid received this thumbnail; it may be queued again later if evicted.
    pub fn done(&self, e: &Entry) {
        self.busy.lock().unwrap().remove(e);
    }

    pub fn is_busy(&self, e: &Entry) -> bool {
        self.busy.lock().unwrap().contains(e)
    }

    pub fn clear(&self) {
        self.list.lock().unwrap().clear();
    }
}

pub fn spawn_workers(n: usize, proxy: EventLoopProxy<UserEvent>, exif: Arc<AtomicBool>, disk: bool) -> Arc<Jobs> {
    let jobs = Arc::new(Jobs {
        list: Mutex::new(Vec::new()),
        cv: Condvar::new(),
        busy: Mutex::new(HashSet::new()),
        size: AtomicU32::new(160),
        disk: AtomicBool::new(disk),
    });
    for _ in 0..n {
        let jobs = jobs.clone();
        let proxy = proxy.clone();
        let exif = exif.clone();
        std::thread::spawn(move || loop {
            let e = {
                let mut g = jobs.list.lock().unwrap();
                while g.is_empty() {
                    g = jobs.cv.wait(g).unwrap();
                }
                g.pop().unwrap()
            };
            if !jobs.busy.lock().unwrap().insert(e.clone()) {
                continue;
            }
            let size = jobs.size.load(Ordering::Relaxed);
            let exif_on = exif.load(Ordering::Relaxed);
            // disk cache first; decode only on a miss, and store after the thumbnail is sent
            let cpath = if jobs.disk.load(Ordering::Relaxed) { crate::thumbcache::path_for(&e, size, exif_on) } else { None };
            let (t, fresh) = match cpath.as_deref().and_then(crate::thumbcache::load) {
                Some(img) => (Some(Arc::new(img)), false),
                None => (crate::decode::decode(&e, exif_on).ok().map(|d| Arc::new(fit(&d.frames[0].img, size))), true),
            };
            // stays busy until the grid has it (App calls Jobs::done), so a redraw in between
            // cannot queue it again: before this, 78 thumbnails were made for 40 cells.
            let store = if fresh { cpath.zip(t.clone()) } else { None };
            if proxy.send_event(UserEvent::Thumb(e, t)).is_err() {
                break;
            }
            if let Some((p, img)) = store {
                crate::thumbcache::store(&p, &img);
            }
        });
    }
    jobs
}

/// Scale `img` to fit a `size` x `size` box (never up).
pub fn fit(img: &RgbaImage, size: u32) -> RgbaImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let k = (size as f32 / w as f32).min(size as f32 / h as f32).min(1.0);
    let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
    if (tw, th) == (w, h) {
        img.clone()
    } else {
        render::resize(img, tw, th, 1)
    }
}

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    pub sel: usize,
    /// Scroll offset in physical px.
    pub scroll: f32,
    pub hover: Option<usize>,
    /// Thumbnails, with the frame they were last drawn on (for eviction).
    pub thumbs: HashMap<Entry, (Arc<RgbaImage>, u64)>,
    pub failed: HashSet<Entry>,
    frame: u64,
    /// Thumbnail size the cache holds; a change of scale factor clears it.
    size: u32,
    /// Blurred, dimmed copy of the viewer behind the grid, and its size.
    pub bg: Vec<u32>,
    pub bg_dims: (usize, usize),
    /// Hover fade of the close button, 0 or 1.
    pub close_hover: bool,
    /// While the scroll handle is dragged: the cursor's offset from the handle's top.
    pub bar_drag: Option<f32>,
    /// The cursor is over the handle's grab area (or dragging it), and when that last
    /// changed: the handle's colour fades over 250 ms.
    pub bar_hot: bool,
    pub bar_hot_at: Option<std::time::Instant>,
    /// True while the handle's colour is still fading: the window keeps drawing.
    pub bar_anim: bool,
}

/// Scroll handle, a slim column scrollbar: a 4 px
/// strip against the panel's right edge, rounded 4 px on its inner side only, no track,
/// slate 600 (#475569) at rest and slate 500 (#64748B) under the cursor. It ends 16 px
/// inside the scrolling area at the top and the bottom.
const BAR_W: f32 = 4.0;
const BAR_INSET: f32 = 16.0;
/// Grab area: 28 px wide, from the panel's right edge inwards.
const BAR_GRAB: f32 = 28.0;
const BAR_REST: u32 = 0x475569;
const BAR_HOT: u32 = 0x64748B;
/// Edge fades: at most 16 px, and never more than the distance
/// scrolled from that edge, so a list at rest has none.
const EDGE_FADE: f32 = 16.0;

/// The scroll handle: its rectangle, and the track it moves on.
pub struct Bar {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub track_top: f32,
    pub track_h: f32,
    pub max: f32,
}

impl Bar {
    /// The grab area: BAR_GRAB wide from the panel's right edge inwards, the full track
    /// tall; the handle looks the same.
    pub fn hit(&self, x: f32, y: f32, sf: f32) -> bool {
        let right = self.x + self.w;
        x >= right - BAR_GRAB * sf && x < right && y >= self.track_top && y < self.track_top + self.track_h
    }
    /// Scroll offset that puts the handle's top at `top`.
    pub fn scroll_for(&self, top: f32) -> f32 {
        let room = (self.track_h - self.h).max(1.0);
        ((top - self.track_top) / room * self.max).clamp(0.0, self.max)
    }
}

/// Panel rectangle (x, y, w, h) in physical px.
pub fn panel_rect(w: f32, h: f32, sf: f32) -> (f32, f32, f32, f32) {
    let i = (INSET * sf).round();
    (i, i, (w - 2.0 * i).max(1.0), (h - 2.0 * i).max(1.0))
}

/// Close button rectangle (x0, y0, x1, y1) in physical px.
pub fn close_rect(w: f32, h: f32, sf: f32) -> (f32, f32, f32, f32) {
    let (px, py, pw, _) = panel_rect(w, h, sf);
    let (s, i) = ((CLOSE * sf).round(), (CLOSE_INSET * sf).round());
    let x1 = px + pw - i;
    (x1 - s, py + i, x1, py + i + s)
}

pub struct Grid {
    pub cols: usize,
    pub cw: f32,
    pub ch: f32,
    pub x0: f32,
    pub top: f32,
    pub view_h: f32,
}

impl Picker {
    pub fn grid(&self, w: f32, h: f32, sf: f32) -> Grid {
        let (cw, ch) = (CELL_W * sf, CELL_H * sf);
        let (px, py, pw, ph) = panel_rect(w, h, sf);
        let cols = (((pw - 2.0 * SIDE * sf) / cw).floor() as usize).max(1);
        let x0 = (px + (pw - cols as f32 * cw) / 2.0).round();
        let top = py + TOP * sf;
        Grid { cols, cw, ch, x0, top, view_h: (py + ph - BOTTOM * sf - top).max(1.0) }
    }

    /// The scroll handle for `n` entries in a w x h window; None when nothing scrolls.
    pub fn bar(&self, w: f32, h: f32, sf: f32, n: usize) -> Option<Bar> {
        let g = self.grid(w, h, sf);
        let max = self.max_scroll(&g, n);
        if max <= 0.0 {
            return None;
        }
        let (px, _, pw, _) = panel_rect(w, h, sf);
        let track_top = (g.top + BAR_INSET * sf).round();
        let track_h = (g.view_h - 2.0 * BAR_INSET * sf).round();
        let thumb_h = (track_h * g.view_h / (g.view_h + max)).max(32.0 * sf).round();
        let y = (track_top + (track_h - thumb_h) * (self.scroll / max)).round();
        let bw = (BAR_W * sf).round();
        Some(Bar { x: px + pw - bw, y, w: bw, h: thumb_h, track_top, track_h, max })
    }

    fn max_scroll(&self, g: &Grid, n: usize) -> f32 {
        let rows = n.div_ceil(g.cols);
        (rows as f32 * g.ch - g.view_h).max(0.0)
    }

    pub fn clamp(&mut self, g: &Grid, n: usize) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(g, n));
    }

    /// Scroll so the selected cell is fully visible.
    pub fn reveal(&mut self, g: &Grid, n: usize) {
        let row = (self.sel / g.cols) as f32;
        let (y0, y1) = (row * g.ch, (row + 1.0) * g.ch);
        if y0 < self.scroll {
            self.scroll = y0;
        } else if y1 > self.scroll + g.view_h {
            self.scroll = y1 - g.view_h;
        }
        self.clamp(g, n);
    }

    pub fn center_on_sel(&mut self, g: &Grid, n: usize) {
        let row = (self.sel / g.cols) as f32;
        self.scroll = row * g.ch - (g.view_h - g.ch) / 2.0;
        self.clamp(g, n);
    }

    pub fn index_at(&self, g: &Grid, x: f32, y: f32, n: usize) -> Option<usize> {
        if y < g.top || y >= g.top + g.view_h || x < g.x0 {
            return None;
        }
        let col = ((x - g.x0) / g.cw) as usize;
        let row = ((y - g.top + self.scroll) / g.ch) as usize;
        if col >= g.cols {
            return None;
        }
        let i = row * g.cols + col;
        (i < n).then_some(i)
    }

    /// Indices of cells at least partly visible.
    pub fn visible(&self, g: &Grid, n: usize) -> std::ops::Range<usize> {
        let r0 = (self.scroll / g.ch).floor() as usize;
        let r1 = ((self.scroll + g.view_h) / g.ch).ceil() as usize;
        (r0 * g.cols).min(n)..((r1 + 1) * g.cols).min(n)
    }

    pub fn insert(&mut self, e: Entry, t: Option<Arc<RgbaImage>>) {
        match t {
            Some(t) => {
                self.thumbs.insert(e, (t, self.frame));
            }
            None => {
                self.failed.insert(e);
            }
        }
        if self.thumbs.len() > CACHE_MAX {
            let mut v: Vec<(u64, Entry)> = self.thumbs.iter().map(|(e, (_, f))| (*f, e.clone())).collect();
            v.sort_by_key(|x| x.0);
            for (_, e) in v.into_iter().take(self.thumbs.len() - CACHE_MAX) {
                self.thumbs.remove(&e);
            }
        }
    }

    /// Draw the overlay and queue the thumbnails it still needs.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, c: &mut Canvas, t: Option<&mut Text>, entries: &crate::source::Playlist, current: usize, sf: f32, jobs: &Jobs, main_cache: &dyn Fn(&Entry) -> Option<Arc<RgbaImage>>) {
        self.frame += 1;
        let size = (THUMB * sf).round() as u32;
        if size != self.size {
            self.thumbs.clear();
            self.size = size;
            jobs.size.store(size, Ordering::Relaxed);
        }
        let n = entries.len();
        let g = self.grid(c.w as f32, c.h as f32, sf);
        self.clamp(&g, n);
        // Background: the viewer, blurred and dimmed, with the panel on it; made once per
        // open/resize. (Re-blurring the full window every frame cost 41 ms at 1600x1000.)
        let (px, py, pw, ph) = panel_rect(c.w as f32, c.h as f32, sf);
        if self.bg_dims == (c.w, c.h) && self.bg.len() == c.buf.len() {
            c.buf.copy_from_slice(&self.bg);
        } else {
            c.blur(0, 0, c.w as i32, c.h as i32, (12.0 * sf) as usize);
            c.rect_alpha(0, 0, c.w as i32, c.h as i32, 0x0C0C0C, 160);
            // the panel: the icon's base grey, nearly opaque, with a faint edge
            c.fill_rrect(px, py, pw, ph, PANEL_R * sf, crate::ui::ICON_GREY, 0.94);
            c.stroke_rrect(px, py, pw, ph, PANEL_R * sf, sf.max(1.0), 0xFFFFFF, 0.08);
            self.bg = c.buf.to_vec();
            self.bg_dims = (c.w, c.h);
        }

        let vis = self.visible(&g, n);
        let mut want = Vec::new();
        let mut t = t;
        for i in vis.clone() {
            let Some(e) = entries.get(i) else { continue };
            let (col, row) = (i % g.cols, i / g.cols);
            let cx = g.x0 + col as f32 * g.cw;
            let cy = g.top + row as f32 * g.ch - self.scroll;
            let (bx, by, bs) = (cx + (g.cw - THUMB * sf) / 2.0, cy + 8.0 * sf, THUMB * sf);
            // cell background and selection
            let sel = i == self.sel;
            if sel || self.hover == Some(i) {
                let col = if sel { 0x2B6CB0 } else { 0x3A3A3A };
                c.fill_rrect(cx + 4.0 * sf, cy + 4.0 * sf, g.cw - 8.0 * sf, g.ch - 8.0 * sf, 8.0 * sf, col, if sel { 0.55 } else { 0.6 });
            }
            if i == current {
                c.stroke_rrect(cx + 4.0 * sf, cy + 4.0 * sf, g.cw - 8.0 * sf, g.ch - 8.0 * sf, 8.0 * sf, 2.0 * sf, 0xF2A33A, 1.0);
            }
            let thumb = match self.thumbs.get_mut(e) {
                Some((img, f)) => {
                    *f = self.frame;
                    Some(img.clone())
                }
                None => match main_cache(e) {
                    // already decoded for the viewer: scale it here, no disk read
                    Some(full) => {
                        let th = Arc::new(fit(&full, size));
                        self.thumbs.insert(e.clone(), (th.clone(), self.frame));
                        Some(th)
                    }
                    None => {
                        if !self.failed.contains(e) && !jobs.is_busy(e) {
                            want.push(e.clone());
                        }
                        None
                    }
                },
            };
            match thumb {
                Some(img) => {
                    let ox = (bx + (bs - img.width() as f32) / 2.0).round() as i64;
                    let oy = (by + (bs - img.height() as f32) / 2.0).round() as i64;
                    render::blit(c, &img, ox, oy, 0x2A2A2A, false);
                }
                None => {
                    let col = if self.failed.contains(e) { 0x4A2020 } else { 0x262626 };
                    c.fill_rrect(bx + 12.0 * sf, by + 12.0 * sf, bs - 24.0 * sf, bs - 24.0 * sf, 4.0 * sf, col, 1.0);
                }
            }
            if let Some(t) = t.as_deref_mut() {
                let px = 12.0 * sf;
                let mut name = e.name();
                let maxw = g.cw - 16.0 * sf;
                if t.width(&name, px) > maxw {
                    while t.width(&format!("{name}…"), px) > maxw && name.chars().count() > 1 {
                        name.pop();
                    }
                    name.push('…');
                }
                let tw = t.width(&name, px);
                let lh = t.line_height(px);
                let ty = by + bs + 8.0 * sf;
                t.draw(c, (cx + (g.cw - tw) / 2.0).round(), ty.round(), &name, px, if sel { 0xFFFFFF } else { 0xC8C8C8 });
                let _ = lh;
            }
        }
        // clip: cells that scrolled past the grid's top or bottom are covered with the
        // cached background (panel header and bottom padding, and the margin around it)
        let (y_top, y_bot) = (g.top.max(0.0) as usize, ((g.top + g.view_h).ceil() as usize).min(c.h));
        // edge fades: cells blend into the panel near an edge they scroll past, over at
        // most 16 px, with a soft ramp (0, 40% at 0.38, 87% at 0.72, 100%)
        let max = self.max_scroll(&g, n);
        let fade_top = (EDGE_FADE * sf).min(self.scroll).round() as usize;
        let fade_bot = (EDGE_FADE * sf).min((max - self.scroll).max(0.0)).round() as usize;
        let ramp = |t: f32| -> u32 {
            let a = if t < 0.38 { t / 0.38 * 0.4 } else if t < 0.72 { 0.4 + (t - 0.38) / 0.34 * 0.47 } else { 0.87 + (t - 0.72) / 0.28 * 0.13 };
            (a.clamp(0.0, 1.0) * 255.0) as u32
        };
        let (fx0, fx1) = ((px.max(0.0)) as usize, ((px + pw) as usize).min(c.w));
        for d in 0..fade_top {
            let a = ramp(d as f32 / fade_top as f32);
            let r = (y_top + d).min(c.h - 1) * c.w;
            for x in fx0..fx1 {
                c.buf[r + x] = crate::render::mix(self.bg[r + x], c.buf[r + x], a);
            }
        }
        for d in 0..fade_bot {
            let a = ramp(d as f32 / fade_bot as f32);
            let r = y_bot.saturating_sub(1 + d) * c.w;
            for x in fx0..fx1 {
                c.buf[r + x] = crate::render::mix(self.bg[r + x], c.buf[r + x], a);
            }
        }
        c.buf[..y_top.min(c.h) * c.w].copy_from_slice(&self.bg[..y_top.min(c.h) * c.w]);
        c.buf[y_bot * c.w..].copy_from_slice(&self.bg[y_bot * c.w..]);
        c.rect_alpha(px as i32, g.top as i32 - 1, pw as i32, sf.max(1.0) as i32, 0xFFFFFF, 20);
        let mid = py + (TOP * sf) / 2.0;
        // close button: a hover plate behind the Tabler X, as the bottom bar's buttons
        let (x0, y0, x1, y1) = close_rect(c.w as f32, c.h as f32, sf);
        if self.close_hover {
            c.fill_rrect(x0, y0, x1 - x0, y1 - y0, 8.0 * sf, 0xFFFFFF, 0.12);
        }
        let col = if self.close_hover { 0xFFFFFF } else { 0xC8C8C8 };
        crate::icons::draw(c, crate::icons::Icon::Close, (x0 + x1) / 2.0, (y0 + y1) / 2.0, 20.0 * sf, col);
        if let Some(t) = t.as_deref_mut() {
            // One baseline for the whole header: the title's cap band is centred on the
            // header, and the count and the hints sit on the title's baseline.
            let prev = t.face(Face::Title);
            let fpx = 14.0 * sf;
            let base = (mid + t.cap_height(fpx) / 2.0).round();
            let tx = (px + SIDE * sf).round();
            t.draw_base(c, tx, base, "Select Image", fpx, 0xF0F0F0);
            let tw = t.width("Select Image", fpx);
            // the count: Geist Mono in the dark pill of the page-number popup, 12 px after
            // the title; the pill is centred on the mono cap band, so its text keeps the line
            t.face(Face::Mono);
            let npx = 12.0 * sf;
            let label = crate::chrome::page_label(self.sel + 1, n);
            let (pad, ph) = ((8.0 * sf).round(), (24.0 * sf).round());
            let pw = (t.width(&label, npx) + 2.0 * pad).round();
            let pill_x = (tx + tw + 12.0 * sf).round();
            let pill_y = (base - t.cap_height(npx) / 2.0 - ph / 2.0).round();
            c.fill_rrect(pill_x, pill_y, pw, ph, 8.0 * sf, 0x101010, 0.92);
            t.draw_base(c, pill_x + pad, base, &label, npx, 0xF0F0F0);
            t.face(Face::Regular);
            let hint = "Enter open   Esc close   arrows, PgUp/PgDn, Home/End, wheel";
            let hpx = 12.0 * sf;
            let hw = t.width(hint, hpx);
            // 16 px clear of the close button
            t.draw_base(c, (x0 - 16.0 * sf - hw).round(), base, hint, hpx, 0x9A9A9A);
            t.face(prev);
        }
        // scrollbar: rounded on the inner side only. Drawn twice as wide with its radius,
        // then the outer half (past the panel's edge) is restored from the background.
        if let Some(b) = self.bar(c.w as f32, c.h as f32, sf, n) {
            let k = self.bar_hot_at.map(|t| (t.elapsed().as_secs_f32() / 0.25).min(1.0)).unwrap_or(1.0);
            let hot = if self.bar_hot || self.bar_drag.is_some() { k } else { 1.0 - k };
            let hot = if self.bar_hot_at.is_none() { 0.0 } else { hot };
            let col = crate::render::mix(BAR_REST, BAR_HOT, (hot * 255.0) as u32);
            c.fill_rrect(b.x, b.y, 2.0 * b.w, b.h, (4.0 * sf).round(), col, 1.0);
            let (x0, x1) = ((b.x + b.w) as usize, ((b.x + 2.0 * b.w) as usize).min(c.w));
            for y in (b.y as usize)..((b.y + b.h) as usize).min(c.h) {
                let r = y * c.w;
                c.buf[r + x0..r + x1].copy_from_slice(&self.bg[r + x0..r + x1]);
            }
            self.bar_anim = k < 1.0;
        }
        // nearest the selection first; workers pop from the end, so sort farthest-first
        let pos: HashMap<&Entry, usize> = vis.clone().filter_map(|i| entries.get(i).map(|e| (e, i))).collect();
        let sel = self.sel;
        want.sort_by_key(|e| std::cmp::Reverse(pos.get(e).map(|&i| i.abs_diff(sel)).unwrap_or(usize::MAX)));
        let mut g2 = jobs.list.lock().unwrap();
        *g2 = want;
        if !g2.is_empty() {
            jobs.cv.notify_all();
        }
    }
}
