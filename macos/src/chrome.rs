//! Honeyview-style window chrome: title bar, toolbar and bottom control bar.
//! Sizes are design pixels measured from Honeyview 5.53 at 100% scale
//! (see docs/honeyview-ui.md); every value is multiplied by the scale factor.


use crate::icons::{self, Icon};
use crate::render::{mix, Canvas};
use crate::text::Text;

#[path = "chrome_bar.rs"]
mod bar;
use bar::draw_bottom;
pub use bar::fmt_speed;
/// Bottom-bar icon size, design px (menus align to the icon's edges).
pub use bar::ICON_PX as BAR_ICON_PX;

/// Title bar height: Windows draws its own 24 px bar; macOS uses the system's 28 pt bar
/// height so the real traffic-light buttons sit centred in it.
#[cfg(not(target_os = "macos"))]
pub const TITLE_H: f32 = 24.0;
#[cfg(target_os = "macos")]
pub const TITLE_H: f32 = 28.0;
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
const GLASS_BAR_A: u32 = 150;

/// Title bar and lock colour: the icon's blue (crate::ui::ACCENT_FILL).
const BLUE: u32 = crate::ui::ACCENT_FILL;
const CLOSE_RED: u32 = 0xC75050;
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
    /// Tooltip fade per bottom-bar button, 0-1: 125 ms in on hover, 125 ms out after.
    pub tip_fx: &'a [(&'static str, f32)],
    pub info_on: bool,
    pub index: usize,
    pub len: usize,
    /// (frame, frame count) for an animation; None for a still image.
    pub anim: Option<(usize, usize)>,
    /// Format pill beside the scrub bar: "GIF", "WEBP", "APNG", "JSON", "LOTTIE"...
    pub anim_fmt: &'a str,
    /// Playback speed (0.25-4) and the frame rate it gives, for the speed pill.
    pub anim_speed: f32,
    pub anim_fps: u32,
}

impl State<'_> {
    /// How far the hover on `id` has faded in, 0-1.
    pub fn tip_amt(&self, id: &str) -> f32 {
        self.tip_fx.iter().find(|(k, _)| *k == id).map(|(_, a)| *a).unwrap_or(0.0)
    }

    pub fn hover_amt(&self, id: &str) -> f32 {
        self.hover_fx.iter().find(|(k, _)| *k == id).map(|(_, a)| *a).unwrap_or(0.0)
    }
}

/// Heights of the top chrome block and the bottom hover zone, in physical pixels.
pub fn top_zone(sf: f32) -> f32 {
    TITLE_H * sf
}

/// Animation scrub row above the bottom bar: a bare 4 px track between the pills, which
/// are 24 px tall and set the row height.
pub const ANIM_H: f32 = 24.0;

/// The bottom hover zone; it grows by the scrub bar and its gap when one shows.
pub fn bottom_zone(sf: f32, anim: bool) -> f32 {
    (BAR_H + BAR_GAP + 24.0 + if anim { ANIM_H + BAR_GAP } else { 0.0 }) * sf
}

/// Scrub row (x, y, w, h) for the track, BAR_GAP above the bar and as wide as the bar:
/// [format] [counter] [track ...] [speed]. `left` and `right` are the physical px the pills
/// take at each end; the track keeps PILL_GAP clear of them.
pub fn anim_track(w: f32, h: f32, sf: f32, left: f32, right: f32) -> (f32, f32, f32, f32) {
    let (bx, by, bw, _) = bar_rect(w, h, sf);
    let th = (ANIM_H * sf).round();
    let gap = (PILL_GAP * sf).round();
    let x = bx + left + gap;
    (x, by - (BAR_GAP * sf).round() - th, (bx + bw - right - gap - x).max(1.0), th)
}

/// Pills beside the scrub track: 24 px tall, 12 px side padding, 4 px apart, 12 px from the track.
pub const PILL_H: f32 = 24.0;
pub const PILL_PAD: f32 = 12.0;
pub const PILL_GAP: f32 = 12.0;
pub const PILL_SPACE: f32 = 4.0;

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

    // Frosted glass: blur what is under the top chrome, then tint it.
    c.blur(0, 0, w as i32, th as i32 + 1, (GLASS_BLUR * sf).round() as usize);
    let isz = 16.0 * sf;
    // --- title bar ---
    #[cfg(target_os = "macos")]
    {
        // macOS: the system's traffic lights (shown by the app with the chrome) and a
        // centred title on the icon's base grey, as a standard dark title bar.
        c.rect_alpha(0, 0, w as i32, th as i32, crate::ui::ICON_GREY, GLASS_TITLE_A);
        hits.push(Hit { x0: 0.0, y0: 0.0, x1: w, y1: th, id: "title" });
        if let Some(t) = t.as_deref_mut() {
            let px = 13.0 * sf;
            // clear of the traffic lights (72 pt) on both sides, so the title stays centred
            let max_w = w - 2.0 * 80.0 * sf;
            let mut title = s.title.to_string();
            while t.width(&title, px) > max_w && title.chars().count() > 4 {
                title.pop();
            }
            let tw = t.width(&title, px);
            text_mid(c, t, ((w - tw) / 2.0).round(), th / 2.0, &title, px, 0xDCDCDC);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
    c.rect_alpha(0, 0, w as i32, th as i32, BLUE, GLASS_TITLE_A);
    hits.push(Hit { x0: 0.0, y0: 0.0, x1: w, y1: th, id: "title" });
    // app icon (assets/icon.svg), 16 px, 8 px from the left, vertically centred
    let logo = crate::icon::scaled((16.0 * sf).round() as u32);
    let ly = ((th - logo.height() as f32) / 2.0).round() as i64;
    crate::render::blit_over(c, &logo, (8.0 * sf).round() as i64, ly);
    // window buttons, right to left: close (48 px), maximize, minimize, full screen (32 px);
    // Tabler icons, 16 px, white on the blue bar
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
    }

}


#[cfg(test)]
mod tests {
    /// Speed labels drop trailing zeros; the 16 menu steps run 0.25 to 4.
    #[test]
    fn speed_labels() {
        assert_eq!(super::fmt_speed(0.25), "0.25");
        assert_eq!(super::fmt_speed(0.5), "0.5");
        assert_eq!(super::fmt_speed(1.0), "1");
        assert_eq!(super::fmt_speed(1.75), "1.75");
        assert_eq!(super::fmt_speed(4.0), "4");
        let steps: Vec<f32> = (1..=16).map(|q| q as f32 / 4.0).collect();
        assert_eq!((steps[0], steps[15], steps.len()), (0.25, 4.0, 16));
    }

    /// The frame counter pads the current frame to the digits of the count.
    #[test]
    fn counter_padding() {
        let fmt = |f: usize, n: usize| {
            let digits = n.to_string().len();
            format!("{:0digits$}/{n}", f)
        };
        assert_eq!(fmt(1, 300), "001/300");
        assert_eq!(fmt(5, 30), "05/30");
        assert_eq!(fmt(4, 4), "4/4");
        assert_eq!(fmt(42, 1200), "0042/1200");
    }

    /// Scrub row at 1280x860: 24 px tall, between the pills, 16 px above the bar.
    #[test]
    fn scrub_track_geometry() {
        for sf in [1.0f32, 1.5, 2.0] {
            let (w, h) = (1280.0 * sf, 860.0 * sf);
            let (bx, by, bw, _) = super::bar_rect(w, h, sf);
            let (left, right) = (160.0 * sf, 100.0 * sf);
            let (tx, ty, tw, th) = super::anim_track(w, h, sf, left, right);
            assert_eq!(th, 24.0 * sf, "height at sf {sf}");
            assert_eq!(tx - bx, left + (12.0 * sf).round(), "left pills at sf {sf}");
            assert_eq!(bx + bw - (tx + tw), right + (12.0 * sf).round(), "right pill at sf {sf}");
            assert_eq!(by - (ty + th), 16.0 * sf, "gap at sf {sf}");
        }
    }
}
