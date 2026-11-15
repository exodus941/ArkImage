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
    text::Text,
};

/// Design px: thumbnail box, cell size, gaps.
pub const THUMB: f32 = 160.0;
const CELL_W: f32 = 184.0;
const CELL_H: f32 = 208.0;
const TOP: f32 = 52.0;
const SIDE: f32 = 24.0;
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
        let cols = (((w - 2.0 * SIDE * sf) / cw).floor() as usize).max(1);
        let x0 = ((w - cols as f32 * cw) / 2.0).round();
        let top = TOP * sf;
        Grid { cols, cw, ch, x0, top, view_h: (h - top).max(1.0) }
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
        if y < g.top || x < g.x0 {
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
    pub fn draw(&mut self, c: &mut Canvas, t: Option<&mut Text>, entries: &[Entry], current: usize, sf: f32, jobs: &Jobs, main_cache: &dyn Fn(&Entry) -> Option<Arc<RgbaImage>>) {
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
        // Background: the viewer, blurred and dimmed, made once per open/resize.
        // (Re-blurring the full window every frame cost 41 ms at 1600x1000.)
        if self.bg_dims == (c.w, c.h) && self.bg.len() == c.buf.len() {
            c.buf.copy_from_slice(&self.bg);
        } else {
            c.blur(0, 0, c.w as i32, c.h as i32, (12.0 * sf) as usize);
            c.rect_alpha(0, 0, c.w as i32, c.h as i32, 0x0C0C0C, 215);
            self.bg = c.buf.to_vec();
            self.bg_dims = (c.w, c.h);
        }

        let vis = self.visible(&g, n);
        let mut want = Vec::new();
        let mut t = t;
        for i in vis.clone() {
            let e = &entries[i];
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
        // header (opaque, so cells scrolling under it do not show through)
        c.rect_alpha(0, 0, c.w as i32, g.top as i32, 0x141414, 255);
        c.rect_alpha(0, g.top as i32 - 1, c.w as i32, 1, 0x3A3A3A, 255);
        if let Some(t) = t.as_deref_mut() {
            let px = 14.0 * sf;
            let head = format!("Select Image   {} / {}", self.sel + 1, n);
            t.draw(c, (SIDE * sf).round(), (16.0 * sf).round(), &head, px, 0xF0F0F0);
            let hint = "Enter open   Esc close   arrows, PgUp/PgDn, Home/End, wheel";
            let hw = t.width(hint, 12.0 * sf);
            t.draw(c, (c.w as f32 - SIDE * sf - hw).round(), (16.0 * sf).round(), hint, 12.0 * sf, 0x9A9A9A);
        }
        // scrollbar
        let max = self.max_scroll(&g, n);
        if max > 0.0 {
            let track = g.view_h - 8.0 * sf;
            let thumb_h = (track * g.view_h / (g.view_h + max)).max(24.0 * sf);
            let y = g.top + 4.0 * sf + (track - thumb_h) * (self.scroll / max);
            c.fill_rrect(c.w as f32 - 12.0 * sf, y, 4.0 * sf, thumb_h, 2.0 * sf, 0xFFFFFF, 0.35);
        }
        // nearest the selection first; workers pop from the end, so sort farthest-first
        let pos: HashMap<&Entry, usize> = vis.clone().map(|i| (&entries[i], i)).collect();
        let sel = self.sel;
        want.sort_by_key(|e| std::cmp::Reverse(pos.get(e).map(|&i| i.abs_diff(sel)).unwrap_or(usize::MAX)));
        let mut g2 = jobs.list.lock().unwrap();
        *g2 = want;
        if !g2.is_empty() {
            jobs.cv.notify_all();
        }
    }
}
