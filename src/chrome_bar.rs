//! Bottom control bar: spans the window width (BAR_GAP padding on each side).
//! Row 1: page slider across the whole bar. Row 2: the icons, centred.
//! Drawn as frosted glass: the image under the bar is blurred, then tinted.
//! All sizes are design px on the 4 px grid.

use super::*;

/// Icon centres along the centred row, design px from the row's left edge; 32 px apart.
/// open, menu | zoom in, zoom out | rotate L, rotate R | lock | prev, next.
const ROW_W: f32 = 352.0;
const X_OPEN: f32 = 16.0;
const X_MENU: f32 = 48.0;
const X_SEP1: f32 = 72.0;
const X_ZIN: f32 = 96.0;
const X_ZOUT: f32 = 128.0;
const X_SEP2: f32 = 152.0;
const X_ROTL: f32 = 176.0;
const X_ROTR: f32 = 208.0;
const X_SEP3: f32 = 232.0;
const X_LOCK: f32 = 256.0;
const X_SEP4: f32 = 280.0;
const X_PREV: f32 = 304.0;
const X_NEXT: f32 = 336.0;
/// Row centre below the bar top: 16 inset + 12 slider + 16 gap + 12 (half the 24 px row).
const ROW_Y: f32 = 56.0;
/// Hover plate behind an icon, and its corner radius.
const PLATE: f32 = 32.0;
const PLATE_R: f32 = 8.0;
/// Bar corner radius.
const BAR_RADIUS: f32 = 16.0;
/// Icon size.
const ICON_PX: f32 = 20.0;

use crate::icons::{self, Icon};

pub(super) fn draw_bottom(c: &mut Canvas, _t: Option<&mut Text>, s: &State, hits: &mut Vec<Hit>) {
    let sf = s.sf;
    let (bx, by, bw, bh) = bar_rect(c.w as f32, c.h as f32, sf);
    let r = (BAR_RADIUS * sf).min(bh / 2.0);

    // frosted glass: blur and a dark tint, no outline
    c.blur(bx as i32, by as i32, bw as i32, bh as i32, (GLASS_BLUR * sf).round() as usize);
    c.fill_rrect(bx, by, bw, bh, r, 0x101010, GLASS_BAR_A as f32 / 255.0);
    hits.push(Hit { x0: bx, y0: by, x1: bx + bw, y1: by + bh, id: "bar" });

    // slider across the bar
    let (sx, sy, sw, sh) = slider_rect(c.w as f32, c.h as f32, sf);
    // progress bar: a faint track, filled up to the position; no handle.
    // The fill brightens with the slider's hover fade.
    c.fill_rrect(sx, sy, sw, sh, sh / 2.0, 0xFFFFFF, 0.14);
    let frac = if s.len > 1 { (s.index + 1) as f32 / s.len as f32 } else { 1.0 };
    let fw = (frac * sw).max(sh);
    // 40% at rest, 56% under the cursor (was 62 / 82: too bright)
    let a = 0.40 + 0.16 * s.hover_amt("bar_slider");
    c.fill_rrect(sx, sy, fw, sh, sh / 2.0, 0xFFFFFF, a);
    hits.push(Hit { x0: sx - 8.0 * sf, y0: sy - 8.0 * sf, x1: sx + sw + 8.0 * sf, y1: sy + sh + 8.0 * sf, id: "bar_slider" });

    // icon row, centred; Tabler icons, 20 px
    let ox = (bx + bw / 2.0 - ROW_W * sf / 2.0).round();
    let at = |d: f32| ox + d * sf;
    let cy = (by + ROW_Y * sf).round();
    let isz = ICON_PX * sf;
    // hover fades in and out over 250 ms: a light plate behind the icon, and a brighter icon
    let col_for = |id: &str| mix(ICON, 0xFFFFFF, (s.hover_amt(id) * 255.0) as u32);
    let mut button = |c: &mut Canvas, id: &'static str, x: f32, icon: Icon, col: u32| {
        let a = s.hover_amt(id);
        let p = PLATE * sf;
        if a > 0.0 {
            c.fill_rrect(x - p / 2.0, cy - p / 2.0, p, p, PLATE_R * sf, 0xFFFFFF, 0.12 * a);
        }
        icons::draw(c, icon, x, cy, isz, col);
        hits.push(Hit { x0: x - p / 2.0, y0: cy - p / 2.0, x1: x + p / 2.0, y1: cy + p / 2.0, id });
    };
    for sep in [X_SEP1, X_SEP2, X_SEP3, X_SEP4] {
        c.rect_alpha(at(sep) as i32, (cy - 8.0 * sf) as i32, sf.max(1.0) as i32, (16.0 * sf) as i32, 0xFFFFFF, 60);
    }
    // lock: closed and blue while the bar is locked, open otherwise
    let lock_col = if s.lock_bottom { mix(BLUE, 0xFFFFFF, (s.hover_amt("bar_lock") * 96.0) as u32) } else { col_for("bar_lock") };
    for (id, off, icon) in [
        ("bar_open", X_OPEN, Icon::Eject),
        ("bar_menu", X_MENU, Icon::Menu),
        ("bar_zin", X_ZIN, Icon::Plus),
        ("bar_zout", X_ZOUT, Icon::Minus),
        ("bar_rotl", X_ROTL, Icon::RotateLeft),
        ("bar_rotr", X_ROTR, Icon::RotateRight),
        ("bar_lock", X_LOCK, if s.lock_bottom { Icon::Lock } else { Icon::LockOpen }),
        ("bar_prev", X_PREV, Icon::ChevronLeft),
        ("bar_next", X_NEXT, Icon::ChevronRight),
    ] {
        let col = if id == "bar_lock" { lock_col } else { col_for(id) };
        button(c, id, at(off), icon, col);
    }
}
