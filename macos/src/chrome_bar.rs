//! Bottom control bar: spans the window width (BAR_GAP padding on each side).
//! Row 1: page slider across the whole bar. Row 2: the icons, centred.
//! Drawn as frosted glass: the image under the bar is blurred, then tinted.
//! All sizes are design px on the 4 px grid.

use super::*;

/// Icon centres along the centred row, design px from the row's left edge; 32 px apart.
/// open, menu | EXIF, full screen, view, edit, copy to |
/// zoom in, zoom out | rotate L, rotate R | lock | prev, next.
/// The second section is the old top toolbar, icon-only. (Moved here from the top toolbar 2026-11-16 by Claude on the Mac, for Windows and macOS; see docs/windows-handoff.md.)
const ROW_W: f32 = 528.0;
const X_OPEN: f32 = 16.0;
const X_MENU: f32 = 48.0;
const X_SEP1: f32 = 72.0;
const X_EXIF: f32 = 96.0;
const X_FULL: f32 = 128.0;
const X_VIEW: f32 = 160.0;
const X_EDIT: f32 = 192.0;
const X_COPY: f32 = 224.0;
const X_SEP2: f32 = 248.0;
const X_ZIN: f32 = 272.0;
const X_ZOUT: f32 = 304.0;
const X_SEP3: f32 = 328.0;
const X_ROTL: f32 = 352.0;
const X_ROTR: f32 = 384.0;
const X_SEP4: f32 = 408.0;
const X_LOCK: f32 = 432.0;
const X_SEP5: f32 = 456.0;
const X_PREV: f32 = 480.0;
const X_NEXT: f32 = 512.0;

/// Tooltip: 12 px text, 8 px side padding, 24 px tall, radius 8, 8 px above the hover plate.
const TIP_H: f32 = 24.0;
const TIP_PAD: f32 = 8.0;
const TIP_GAP: f32 = 8.0;

/// Every button of the row: id, centre, icon, tooltip.
fn row(s: &State) -> [(&'static str, f32, Icon, &'static str); 14] {
    [
        ("bar_open", X_OPEN, Icon::Eject, "Open file"),
        ("bar_menu", X_MENU, Icon::Menu, "Menu"),
        ("tb_exif", X_EXIF, Icon::Camera, "File and EXIF info"),
        ("tb_full", X_FULL, Icon::ArrowsMaximize, "Full screen"),
        ("tb_view", X_VIEW, Icon::Eye, "View"),
        ("tb_edit", X_EDIT, Icon::Pencil, "Edit"),
        ("tb_copyto", X_COPY, Icon::Copy, "Copy to"),
        ("bar_zin", X_ZIN, Icon::Plus, "Zoom in"),
        ("bar_zout", X_ZOUT, Icon::Minus, "Zoom out"),
        ("bar_rotl", X_ROTL, Icon::RotateLeft, "Rotate left"),
        ("bar_rotr", X_ROTR, Icon::RotateRight, "Rotate right"),
        ("bar_lock", X_LOCK, if s.lock_bottom { Icon::Lock } else { Icon::LockOpen }, if s.lock_bottom { "Unlock the bar" } else { "Keep the bar visible" }),
        ("bar_prev", X_PREV, Icon::ChevronLeft, "Previous image"),
        ("bar_next", X_NEXT, Icon::ChevronRight, "Next image"),
    ]
}
/// Row centre below the bar top: 16 inset + 12 slider + 16 gap + 12 (half the 24 px row).
const ROW_Y: f32 = 56.0;
/// Hover plate behind an icon, and its corner radius.
const PLATE: f32 = 32.0;
const PLATE_R: f32 = 8.0;
/// Bar corner radius.
const BAR_RADIUS: f32 = 16.0;
/// Icon size.
pub const ICON_PX: f32 = 20.0;

use crate::icons::{self, Icon};

/// "0.25", "0.5", "1", "1.75": a playback speed without trailing zeros.
pub fn fmt_speed(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub(super) fn draw_bottom(c: &mut Canvas, mut t: Option<&mut Text>, s: &State, hits: &mut Vec<Hit>) {
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
    // drawn 8 px tall, centred in the 12 px layout slot; the grab area keeps the slot + 8 px
    let vh = (8.0 * sf).round();
    let vy = sy + ((sh - vh) / 2.0).round();
    c.fill_rrect(sx, vy, sw, vh, vh / 2.0, 0xFFFFFF, 0.14);
    let frac = if s.len > 1 { (s.index + 1) as f32 / s.len as f32 } else { 1.0 };
    let fw = (frac * sw).max(vh);
    // 40% at rest, 56% under the cursor (was 62 / 82: too bright)
    let a = 0.40 + 0.16 * s.hover_amt("bar_slider");
    c.fill_rrect(sx, vy, fw, vh, vh / 2.0, 0xFFFFFF, a);
    hits.push(Hit { x0: sx - 8.0 * sf, y0: sy - 8.0 * sf, x1: sx + sw + 8.0 * sf, y1: sy + sh + 8.0 * sf, id: "bar_slider" });

    // Added 2026-11-16 by Claude on the Mac, for Windows and macOS (see docs/windows-handoff.md).
    // animation scrub row: format pill, frame counter | track | speed dropdown
    if let (Some((f, n)), Some(t)) = (s.anim, t.as_deref_mut()) {
        let ph = (PILL_H * sf).round();
        let pad = PILL_PAD * sf;
        let px = 12.0 * sf;
        let isz = 16.0 * sf;
        // frame counter: current frame zero-padded to the digits of the count, "001/300"
        let digits = n.to_string().len();
        let counter = format!("{:0digits$}/{n}", f + 1);
        let speed = format!("{}\u{d7} {}FPS", fmt_speed(s.anim_speed), s.anim_fps);
        // the format and the counter are set in Geist Mono; the speed in Geist Medium
        let prev = t.face(crate::text::Face::Mono);
        let fmt_w = t.width(s.anim_fmt, px);
        let cnt_w = t.width(&counter, px);
        t.face(crate::text::Face::Medium);
        let spd_w = t.width(&speed, px);
        // pill widths: text + padding; the dropdown adds a 4 px gap and a 16 px chevron
        let lw = (fmt_w + 2.0 * pad).round();
        let cw = (cnt_w + 2.0 * pad).round();
        let dw = (spd_w + 4.0 * sf + isz + 2.0 * pad).round();
        let space = (PILL_SPACE * sf).round();
        let (tx, ty, tw, th) = anim_track(c.w as f32, c.h as f32, sf, lw + space + cw, dw);
        let py = ty + ((th - ph) / 2.0).round();
        let mid = py + ph / 2.0;
        // frosted glass, as the bar: blur, then a dark tint; the dropdown lightens on hover
        let pill = |c: &mut Canvas, x: f32, w: f32, hover: f32| {
            c.blur(x as i32, py as i32, w as i32, ph as i32, (GLASS_BLUR * sf).round() as usize);
            c.fill_rrect(x, py, w, ph, ph / 2.0, 0x101010, GLASS_BAR_A as f32 / 255.0);
            if hover > 0.0 {
                c.fill_rrect(x, py, w, ph, ph / 2.0, 0xFFFFFF, 0.12 * hover);
            }
        };
        // format pill and frame counter from the bar's left edge
        let lx = bx;
        pill(c, lx, lw, 0.0);
        t.face(crate::text::Face::Mono);
        text_mid(c, t, lx + pad, mid, s.anim_fmt, px, ICON);
        let cx = lx + lw + space;
        pill(c, cx, cw, 0.0);
        text_mid(c, t, cx + pad, mid, &counter, px, ICON);
        // speed dropdown, ending on the bar's right edge
        let dx = bx + bw - dw;
        let ha = s.hover_amt("anim_speed");
        pill(c, dx, dw, ha);
        let col = mix(ICON, 0xFFFFFF, (ha * 255.0) as u32);
        t.face(crate::text::Face::Medium);
        text_mid(c, t, dx + pad, mid, &speed, px, col);
        icons::draw(c, Icon::ChevronDown, dx + dw - pad - isz / 2.0, mid, isz, col);
        hits.push(Hit { x0: dx, y0: py, x1: dx + dw, y1: py + ph, id: "anim_speed" });
        t.face(prev);
        // the track: 4 px tall, centred in the row; the grab area is the row + 8 px
        let vh = (4.0 * sf).round();
        let vy = ty + ((th - vh) / 2.0).round();
        c.fill_rrect(tx, vy, tw, vh, vh / 2.0, 0xFFFFFF, 0.14);
        let frac = (f + 1) as f32 / n.max(1) as f32;
        let a = 0.40 + 0.16 * s.hover_amt("anim_slider");
        c.fill_rrect(tx, vy, (frac * tw).max(vh), vh, vh / 2.0, 0xFFFFFF, a);
        hits.push(Hit { x0: tx - 8.0 * sf, y0: ty - 8.0 * sf, x1: tx + tw + 8.0 * sf, y1: ty + th + 8.0 * sf, id: "anim_slider" });
    }

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
    for sep in [X_SEP1, X_SEP2, X_SEP3, X_SEP4, X_SEP5] {
        c.rect_alpha(at(sep) as i32, (cy - 8.0 * sf) as i32, sf.max(1.0) as i32, (16.0 * sf) as i32, 0xFFFFFF, 60);
    }
    // lock: closed and blue while the bar is locked, open otherwise; EXIF blue while its
    // panel shows
    let blue = |id: &str| mix(BLUE, 0xFFFFFF, (s.hover_amt(id) * 96.0) as u32);
    for (id, off, icon, _) in row(s).into_iter().filter(|b| !b.0.is_empty()) {
        let col = match id {
            "bar_lock" if s.lock_bottom => blue(id),
            "tb_exif" if s.info_on => blue(id),
            _ => col_for(id),
        };
        button(c, id, at(off), icon, col);
    }
    // tooltips over the hovered buttons, 8 px above the plate, inside the window
    let Some(t) = t else { return };
    let prev = t.face(crate::text::Face::Medium);
    for (id, off, _, tip) in row(s).into_iter().filter(|b| !b.0.is_empty()) {
        let a = s.tip_amt(id);
        if a <= 0.0 {
            continue;
        }
        let px = 12.0 * sf;
        let w = (t.width(tip, px) + 2.0 * TIP_PAD * sf).round();
        let h = (TIP_H * sf).round();
        let x = (at(off) - w / 2.0).round().clamp(0.0, (c.w as f32 - w).max(0.0));
        let y = (cy - PLATE * sf / 2.0 - TIP_GAP * sf - h).round();
        c.fill_rrect(x, y, w, h, 8.0 * sf, 0x101010, 0.92 * a);
        let lh = t.line_height(px);
        let col = mix(0x101010, 0xF0F0F0, (a * 255.0) as u32);
        t.draw(c, (x + TIP_PAD * sf).round(), (y + (h - lh) / 2.0).round(), tip, px, col);
    }
    t.face(prev);
}
