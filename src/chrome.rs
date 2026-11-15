//! Honeyview-style window chrome: title bar, toolbar and bottom control bar.
//! Sizes are design pixels measured from Honeyview 5.53 at 100% scale
//! (see docs/honeyview-ui.md); every value is multiplied by the scale factor.


use crate::icons::{self, Icon};
use crate::render::{mix, Canvas};
use crate::text::Text;

#[path = "chrome_bar.rs"]
mod bar;
use bar::draw_bottom;

pub const TITLE_H: f32 = 24.0;
pub const TOOL_H: f32 = 32.0;
/// Bottom bar: full window width minus BAR_GAP on each side; slider on top, icon row below.
/// 16 inset + 12 slider + 16 gap + 24 icon row + 16 below.
pub const BAR_H: f32 = 84.0;
pub const BAR_GAP: f32 = 16.0;
/// Slider track gap from the bar's top, left and right edges, design px.
pub const SLIDER_INSET: f32 = 16.0;
/// Slider and scrub track height, design px.
pub const TRACK_H: f32 = 12.0;
/// Frosted glass: blur radius (design px) and tint opacities (0-255).
const GLASS_BLUR: f32 = 32.0;
const GLASS_TITLE_A: u32 = 205;
const GLASS_TOOL_A: u32 = 190;
const GLASS_BAR_A: u32 = 150;

/// Title bar and lock colour: the icon's blue (crate::ui::ACCENT_FILL).
const BLUE: u32 = crate::ui::ACCENT_FILL;
const CLOSE_RED: u32 = 0xC75050;
const TOOL_TOP: u32 = 0xFBFBFB;
const TOOL_MID: u32 = 0xEDEDED;
const TOOL_BOT: u32 = 0xDEDEDE;
const TOOL_RULE: u32 = 0x666666;
const TOOL_TEXT: u32 = 0x333333;
/// Toolbar icons on the light toolbar.
const TOOL_ICON: u32 = 0x444444;
const ICON: u32 = 0xC8C8C8;

/// A clickable area in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub id: &'static str,
}

impl Hit {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

pub struct State<'a> {
    pub sf: f32,
    pub title: &'a str,
    pub show_top: bool,
    pub show_bottom: bool,
    pub maximized: bool,
    pub lock_top: bool,
    pub lock_bottom: bool,
    /// Hover fade per control, 0-1: rises over 250 ms while hovered, falls over 250 ms after.
    pub hover_fx: &'a [(&'static str, f32)],
    pub info_on: bool,
    pub index: usize,
    pub len: usize,
    /// (frame, frame count) for an animation; None for a still image.
    pub anim: Option<(usize, usize)>,
}

impl State<'_> {
    /// How far the hover on `id` has faded in, 0-1.
    pub fn hover_amt(&self, id: &str) -> f32 {
        self.hover_fx.iter().find(|(k, _)| *k == id).map(|(_, a)| *a).unwrap_or(0.0)
    }
}

/// Heights of the top chrome block and the bottom hover zone, in physical pixels.
pub fn top_zone(sf: f32) -> f32 {
    (TITLE_H + TOOL_H + 1.0) * sf
}

/// Animation scrub bar above the bottom bar: a bare 8 px track, no panel.
pub const ANIM_H: f32 = 8.0;

/// The bottom hover zone; it grows by the scrub bar and its gap when one shows.
pub fn bottom_zone(sf: f32, anim: bool) -> f32 {
    (BAR_H + BAR_GAP + 24.0 + if anim { ANIM_H + BAR_GAP } else { 0.0 }) * sf
}

/// Scrub track (x, y, w, h): 75% of the bar's width, centred, BAR_GAP above the bar.
pub fn anim_track(w: f32, h: f32, sf: f32) -> (f32, f32, f32, f32) {
    let (bx, by, bw, _) = bar_rect(w, h, sf);
    let th = (ANIM_H * sf).round();
    let tw = (bw * 0.75).round();
    ((bx + (bw - tw) / 2.0).round(), by - (BAR_GAP * sf).round() - th, tw.max(1.0), th)
}

/// Bar rectangle (x, y, w, h) in physical pixels.
pub fn bar_rect(w: f32, h: f32, sf: f32) -> (f32, f32, f32, f32) {
    let g = (BAR_GAP * sf).round();
    let bh = (BAR_H * sf).round();
    (g, h - g - bh, (w - 2.0 * g).max(1.0), bh)
}

/// Slider track (x, y, w, h): the bar's full width minus the rounded ends.
pub fn slider_rect(w: f32, h: f32, sf: f32) -> (f32, f32, f32, f32) {
    let (bx, by, bw, _) = bar_rect(w, h, sf);
    // the same gap above, left and right of the track
    let inset = (SLIDER_INSET * sf).round();
    (bx + inset, by + inset, (bw - 2.0 * inset).max(1.0), (TRACK_H * sf).round())
}

pub fn draw(c: &mut Canvas, t: Option<&mut Text>, s: &State, hits: &mut Vec<Hit>) {
    let mut t = t;
    if s.show_top {
        draw_top(c, t.as_deref_mut(), s, hits);
    }
    if s.show_bottom {
        draw_bottom(c, t.as_deref_mut(), s, hits);
    }
}

fn text_mid(c: &mut Canvas, t: &mut Text, x: f32, mid_y: f32, s: &str, px: f32, color: u32) -> f32 {
    let lh = t.line_height(px);
    t.draw(c, x.round(), (mid_y - lh / 2.0).round(), s, px, color);
    t.width(s, px)
}

fn draw_top(c: &mut Canvas, mut t: Option<&mut Text>, s: &State, hits: &mut Vec<Hit>) {
    let sf = s.sf;
    let w = c.w as f32;
    let th = (TITLE_H * sf).round();
    let tb = (TOOL_H * sf).round();

    // Frosted glass: blur what is under the top chrome, then tint it.
    c.blur(0, 0, w as i32, (th + tb) as i32 + 1, (GLASS_BLUR * sf).round() as usize);
    // --- title bar ---
    c.rect_alpha(0, 0, w as i32, th as i32, BLUE, GLASS_TITLE_A);
    hits.push(Hit { x0: 0.0, y0: 0.0, x1: w, y1: th, id: "title" });
    // app icon (assets/icon.svg), 16 px, 8 px from the left, vertically centred
    let logo = crate::icon::scaled((16.0 * sf).round() as u32);
    let ly = ((th - logo.height() as f32) / 2.0).round() as i64;
    crate::render::blit_over(c, &logo, (8.0 * sf).round() as i64, ly);
    // window buttons, right to left: close (48 px), maximize, minimize, full screen (32 px);
    // Tabler icons, 16 px, white on the blue bar
    let isz = 16.0 * sf;
    let close_w = 48.0 * sf;
    let bw = 32.0 * sf;
    let cx0 = w - close_w;
    c.rect_alpha(cx0 as i32, 0, close_w as i32, th as i32, CLOSE_RED, 255);
    let a = s.hover_amt("win_close");
    if a > 0.0 {
        c.rect_alpha(cx0 as i32, 0, close_w as i32, th as i32, 0xFFFFFF, (40.0 * a) as u32);
    }
    hits.push(Hit { x0: cx0, y0: 0.0, x1: w, y1: th, id: "win_close" });
    icons::draw(c, Icon::Close, cx0 + close_w / 2.0, th / 2.0, isz, 0xFFFFFF);
    let max_icon = if s.maximized { Icon::Squares } else { Icon::Square };
    for (i, (id, icon)) in [("win_max", max_icon), ("win_min", Icon::Minus), ("win_full", Icon::Maximize)].into_iter().enumerate() {
        let x1 = cx0 - bw * i as f32;
        let x0 = x1 - bw;
        let a = s.hover_amt(id);
        if a > 0.0 {
            c.rect_alpha(x0 as i32, 0, bw as i32, th as i32, 0xFFFFFF, (40.0 * a) as u32);
        }
        hits.push(Hit { x0, y0: 0.0, x1, y1: th, id });
        icons::draw(c, icon, x0 + bw / 2.0, th / 2.0, isz, 0xFFFFFF);
    }
    if let Some(t) = t.as_deref_mut() {
        let px = 12.0 * sf;
        // title from x 32 to 16 px short of the window buttons
        let max_w = cx0 - bw * 3.0 - 48.0 * sf;
        let mut title = s.title.to_string();
        while t.width(&title, px) > max_w && title.chars().count() > 4 {
            title.pop();
        }
        text_mid(c, t, 32.0 * sf, th / 2.0, &title, px, 0xFFFFFF);
    }

    // --- toolbar ---
    let y0 = th;
    // 1 px highlight row, then the gradient (Honeyview: #FBFBFB, then #EDEDED to #DEDEDE)
    c.rect_alpha(0, y0 as i32, w as i32, 1, TOOL_TOP, GLASS_TOOL_A);
    c.vgrad(0, y0 as i32 + 1, w as i32, tb as i32 - 1, TOOL_MID, TOOL_BOT, GLASS_TOOL_A);
    c.rect_alpha(0, (y0 + tb) as i32, w as i32, sf.round().max(1.0) as i32, TOOL_RULE, 160);
    hits.push(Hit { x0: 0.0, y0, x1: w, y1: y0 + tb + sf, id: "toolbar" });
    let mid = y0 + tb / 2.0;
    let Some(t) = t else { return };
    let px = 12.0 * sf;
    // hover plates: 24 px tall (4 px above and below), 8 px past the content each side
    let (py0, ph) = (y0 + 4.0 * sf, tb - 8.0 * sf);
    let plate = |c: &mut Canvas, x0: f32, x1: f32, a: f32| {
        if a > 0.0 {
            c.rect_alpha(x0 as i32, py0 as i32, (x1 - x0) as i32, ph as i32, 0x000000, (20.0 * a) as u32);
        }
    };

    // left: camera + EXIF
    let ew = t.width("EXIF", px);
    let (ex0, ex1) = (8.0 * sf, 32.0 * sf + ew + 8.0 * sf);
    plate(c, ex0, ex1, if s.info_on { 1.4 } else { s.hover_amt("tb_exif") });
    icons::draw(c, Icon::Camera, 20.0 * sf, mid, isz, TOOL_ICON);
    text_mid(c, t, 32.0 * sf, mid, "EXIF", px, TOOL_TEXT);
    hits.push(Hit { x0: ex0, y0, x1: ex1, y1: y0 + tb, id: "tb_exif" });

    // right side, laid out right to left: lock 24 px from the edge
    let lx = w - 24.0 * sf;
    plate(c, lx - 12.0 * sf, lx + 12.0 * sf, s.hover_amt("tb_lock"));
    icons::draw(c, if s.lock_top { Icon::Lock } else { Icon::LockOpen }, lx, mid, isz, if s.lock_top { BLUE } else { TOOL_ICON });
    hits.push(Hit { x0: lx - 12.0 * sf, y0, x1: lx + 12.0 * sf, y1: y0 + tb, id: "tb_lock" });
    // menus: label, 4 px, 12 px chevron; 8 px plate padding, 8 px between plates
    let mut right = lx - 12.0 * sf - 16.0 * sf;
    let chev = 12.0 * sf;
    for (label, id) in [("Copy to", "tb_copyto"), ("Edit", "tb_edit"), ("Bookmark", "tb_bookmark"), ("Slideshow", "tb_slideshow"), ("View", "tb_view")] {
        let tw = t.width(label, px);
        let x0 = right - chev - 4.0 * sf - tw;
        plate(c, x0 - 8.0 * sf, right + 8.0 * sf, s.hover_amt(id));
        text_mid(c, t, x0, mid, label, px, TOOL_TEXT);
        icons::draw(c, Icon::ChevronDown, right - chev / 2.0, mid, chev, TOOL_ICON);
        hits.push(Hit { x0: x0 - 8.0 * sf, y0, x1: right + 8.0 * sf, y1: y0 + tb, id });
        right = x0 - 24.0 * sf;
    }
    // full screen
    let fx = right - 4.0 * sf;
    plate(c, fx - 12.0 * sf, fx + 12.0 * sf, s.hover_amt("tb_full"));
    icons::draw(c, Icon::ArrowsMaximize, fx, mid, isz, TOOL_ICON);
    hits.push(Hit { x0: fx - 12.0 * sf, y0, x1: fx + 12.0 * sf, y1: y0 + tb, id: "tb_full" });
}


#[cfg(test)]
mod tests {
    /// Scrub track at 1280x860: 8 px tall, 75% of the bar, centred, 16 px above the bar.
    #[test]
    fn scrub_track_geometry() {
        for sf in [1.0f32, 1.5, 2.0] {
            let (w, h) = (1280.0 * sf, 860.0 * sf);
            let (bx, by, bw, _) = super::bar_rect(w, h, sf);
            let (tx, ty, tw, th) = super::anim_track(w, h, sf);
            assert_eq!(th, 8.0 * sf, "height at sf {sf}");
            assert_eq!(tw, (bw * 0.75).round());
            assert_eq!(tx - bx, bx + bw - (tx + tw), "centred at sf {sf}");
            assert_eq!(by - (ty + th), 16.0 * sf, "gap at sf {sf}");
        }
    }
}
