//! Cards drawn over the image: the info panel, the empty state, messages and hotkey help.
//! Style: dark surfaces, the slate text ramp, Geist / Figtree / Geist Mono, 14/20 rows, 4 px grid.

use crate::icons::Icon;
use crate::render::{mix, Canvas};
use crate::text::{Face, Text};

// Dark design tokens.
pub const PANEL: u32 = 0x141925;
pub const PANEL2: u32 = 0x1C2230;
pub const LINE: u32 = 0x2A3142;
pub const SLATE100: u32 = 0xF1F5F9;
pub const SLATE200: u32 = 0xE2E8F0;
pub const SLATE300: u32 = 0xCBD5E1;
pub const SLATE400: u32 = 0x94A3B8;
pub const SLATE500: u32 = 0x64748B;
pub const SLATE600: u32 = 0x475569;
/// App colours, taken from the icon (assets/icon.svg): the grey of its outer triangle,
/// and points on its inner blue gradient (#00CCFF at the top to #005AA8 at the tip).
pub const ICON_GREY: u32 = 0x191D21;
/// Accent marks: focus edges, selection marker, highlighted keys.
pub const ACCENT: u32 = 0x00B2EB;
/// Accent fills: primary button, switches, title bar.
pub const ACCENT_FILL: u32 = 0x0080C5;
/// Accent fill on hover.
pub const ACCENT_HOVER: u32 = 0x0099D9;
/// The gradient's lightest point, for small marks on dark ground.
pub const ACCENT_LIGHT: u32 = 0x00CCFF;
pub const ROSE400: u32 = 0xFB7185;

/// Body text: 14 px on a 20 px line. Titles: 20/28. Section labels: 12/16.
const BODY: (f32, f32) = (14.0, 20.0);
const TITLE: (f32, f32) = (20.0, 28.0);
const CARD_TITLE: (f32, f32) = (14.0, 20.0);
const LABEL: (f32, f32) = (12.0, 16.0);
/// Card padding, corner radius, row gap, section gap.
const PAD: f32 = 16.0;
const RADIUS: f32 = 8.0;
const ROW_GAP: f32 = 4.0;
const SECTION_GAP: f32 = 12.0;
/// Least space between a key and its value.
const KV_GAP: f32 = 16.0;
/// Info card width limits.
const MIN_W: f32 = 320.0;
const MAX_W: f32 = 480.0;

/// Draw one line of text in a line box of height `lh` whose top is `top`; the glyphs sit
/// centred in the box by their ascent and descent.
pub fn line(c: &mut Canvas, t: &mut Text, face: Face, x: f32, top: f32, s: &str, (px, lh): (f32, f32), color: u32) -> f32 {
    let prev = t.face(face);
    let asc = t.ascent(px);
    let desc = t.line_height(px) - asc; // line_height covers ascent + descent (+ gap)
    let base = top + ((lh - (asc + desc)) / 2.0 + asc).round();
    t.draw_base(c, x.round(), base, s, px, color);
    let w = t.width(s, px);
    t.face(prev);
    w
}

/// The middle of the cap band of a line that `line` draws at `top`: where a mark beside it
/// is centred.
pub fn cap_mid(t: &mut Text, face: Face, top: f32, (px, lh): (f32, f32)) -> f32 {
    let prev = t.face(face);
    let asc = t.ascent(px);
    let desc = t.line_height(px) - asc;
    let base = top + ((lh - (asc + desc)) / 2.0 + asc).round();
    let cap = t.cap_height(px);
    t.face(prev);
    base - cap / 2.0
}

pub fn width(t: &mut Text, face: Face, s: &str, px: f32) -> f32 {
    let prev = t.face(face);
    let w = t.width(s, px);
    t.face(prev);
    w
}

/// Cards are dark frosted glass with no shadow (the user's call: shadows banded over
/// the window glass). What is under a card shows blurred over 64 px, under the card's
/// fill at CARD_TINT.
pub const CARD_BLUR: f32 = 64.0;
const CARD_TINT: f32 = 0.6;

/// One box-blur pass of radius `r` along rows (`step` 1) or columns (`step` = width)
/// over `v` (3 channels per pixel), `n` lines of `len` pixels.
fn box_pass(v: &mut [f32], tmp: &mut [f32], len: usize, n: usize, step: usize, line: usize, r: usize) {
    let k = 1.0 / (2 * r + 1) as f32;
    for l in 0..n {
        let at = |i: usize| (l * line + i * step) * 3;
        let mut acc = [0f32; 3];
        // edge pixels repeat outward
        for j in 0..=2 * r {
            let i = j.saturating_sub(r).min(len - 1);
            for ch in 0..3 {
                acc[ch] += v[at(i) + ch];
            }
        }
        for i in 0..len {
            for ch in 0..3 {
                tmp[at(i) + ch] = acc[ch] * k;
            }
            let out = i.saturating_sub(r);
            let inn = (i + r + 1).min(len - 1);
            for ch in 0..3 {
                acc[ch] += v[at(inn) + ch] - v[at(out) + ch];
            }
        }
    }
    v.copy_from_slice(tmp);
}

/// Dark frosted glass in the rounded rect: what is drawn under it, blurred over `blur`
/// px (three box passes), under `tint` at `ta`. Over the window glass the pixels stay
/// glass, darkened by `ta`.
#[allow(clippy::too_many_arguments)]
thread_local! {
    /// The last plain-pixel blur of `frost`: its key (box, blur, pixels) and its values.
    static KEPT: std::cell::RefCell<(u64, Vec<f32>)> = const { std::cell::RefCell::new((0, Vec::new())) };
}

#[cfg(test)]
fn frost_forget() {
    KEPT.with(|k| *k.borrow_mut() = (0, Vec::new()));
}

fn frost(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, blur: f32, tint: u32, ta: f32) {
    use crate::render::{glass_real, mix, sd_rrect};
    let (x0, y0) = (x.floor().max(0.0) as usize, y.floor().max(0.0) as usize);
    let (x1, y1) = (((x + w).ceil() as usize).min(c.w), ((y + h).ceil() as usize).min(c.h));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // Only the interface's own pixels under the card are blurred here: over the GPU
    // image (holes) the GPU blurs, over the glass the window's own blur shows. Blurring the
    // whole area first took 92 to 121 ms a frame for the Hotkeys card at 3420x2148 (the
    // empty card 32 to 39), so every fade and hover crawled. Blur only the box around
    // those pixels, and nothing when there are none.
    let plain = |p: u32| !crate::render::is_glass(p) && !crate::render::is_hole(p);
    let (mut bx0, mut by0, mut bx1, mut by1) = (usize::MAX, usize::MAX, 0, 0);
    for py in y0..y1 {
        let row = &c.buf[py * c.w + x0..py * c.w + x1];
        if let Some(a) = row.iter().position(|&p| plain(p)) {
            let b = row.iter().rposition(|&p| plain(p)).unwrap_or(a);
            bx0 = bx0.min(x0 + a);
            bx1 = bx1.max(x0 + b + 1);
            by0 = by0.min(py);
            by1 = py + 1;
        }
    }
    let any_plain = bx1 > bx0;
    // the source reaches `blur` past those pixels so their edge blurs what is beside them
    let e = blur.ceil() as usize;
    let (sx0, sy0, sx1, sy1) = if any_plain { (bx0.saturating_sub(e), by0.saturating_sub(e), (bx1 + e).min(c.w), (by1 + e).min(c.h)) } else { (x0, y0, x0, y0) };
    let (sw, sh) = (sx1 - sx0, sy1 - sy0);
    // A fade or a hover redraws the card over the same pixels each frame, so the blur of
    // those pixels is kept and reused while they and the box stay the same: over plain
    // interface the blur alone took 92 ms a frame at 1840x1040 (Windows' measure).
    let mut key = 0xcbf2_9ce4_8422_2325u64 ^ (sx0 as u64) << 48 ^ (sy0 as u64) << 32 ^ (sw as u64) << 16 ^ sh as u64 ^ (blur.to_bits() as u64) << 20;
    for yy in 0..sh {
        for &p in &c.buf[(sy0 + yy) * c.w + sx0..(sy0 + yy) * c.w + sx1] {
            key = (key ^ p as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    let kept = any_plain && KEPT.with(|k| k.borrow().0 == key && k.borrow().1.len() == sw * sh * 3);
    let mut v = if kept { KEPT.with(|k| std::mem::take(&mut k.borrow_mut().1)) } else { vec![0f32; sw * sh * 3] };
    if !kept {
        for yy in 0..sh {
            for xx in 0..sw {
                let mut p = c.buf[(sy0 + yy) * c.w + sx0 + xx];
                if crate::render::is_glass(p) {
                    p = glass_real(p);
                }
                let i = (yy * sw + xx) * 3;
                v[i] = (p >> 16 & 0xFF) as f32;
                v[i + 1] = (p >> 8 & 0xFF) as f32;
                v[i + 2] = (p & 0xFF) as f32;
            }
        }
    }
    if any_plain && !kept {
        let mut tmp = vec![0f32; v.len()];
        let br = ((blur / 3.0).round() as usize).max(1);
        for _ in 0..3 {
            box_pass(&mut v, &mut tmp, sw, sh, 1, sw, br);
            box_pass(&mut v, &mut tmp, sh, sw, sw, 1, br);
        }
    }
    let ka = (ta * 255.0).round() as u32;
    // pixels wholly inside the card and clear of its corners are covered fully: the
    // rounded-rect distance (a square root a pixel) is needed only at the edges and corners
    let (ix0, iy0, ix1, iy1) = (x.ceil(), y.ceil(), (x + w).floor(), (y + h).floor());
    let inside = |px: f32, py: f32| {
        px >= ix0 && px + 1.0 <= ix1 && py >= iy0 && py + 1.0 <= iy1 && ((px >= x + r && px + 1.0 <= x + w - r) || (py >= y + r && py + 1.0 <= y + h - r))
    };
    let mut last = (0u32, 0u32, false);
    for py in y0..y1 {
        for px in x0..x1 {
            let cov = if inside(px as f32, py as f32) { 1.0 } else { (0.5 - sd_rrect(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, r)).clamp(0.0, 1.0) };
            if cov <= 0.0 {
                continue;
            }
            let i = py * c.w + px;
            let orig = c.buf[i];
            // most pixels repeat the last one (a plain hole, glass): reuse its result
            if cov >= 1.0 && orig == last.0 && last.2 {
                c.buf[i] = last.1;
                continue;
            }
            let out = if crate::render::is_glass(orig) {
                mix(orig, 0, ka)
            } else if crate::render::is_hole(orig) {
                // over the GPU image layer: the GPU blurs the layer, the tint goes over it
                mix(crate::render::hole_with_class(orig, crate::render::BLUR_CARD), tint, ka)
            } else {
                let j = ((py - sy0) * sw + px - sx0) * 3;
                let q = |k: usize| v[j + k].round().clamp(0.0, 255.0) as u32;
                mix(q(0) << 16 | q(1) << 8 | q(2), tint, ka)
            };
            let plain_px = !crate::render::is_glass(orig) && !crate::render::is_hole(orig);
            c.buf[i] = if cov >= 1.0 { out } else { mix(orig, out, (cov * 255.0) as u32) };
            // a plain pixel's result depends on the blur at its place, not on its value
            last = (orig, out, cov >= 1.0 && !plain_px);
        }
    }
    if any_plain {
        KEPT.with(|k| *k.borrow_mut() = (key, v));
    }
}

/// Card surface: dark frosted glass, no outline, no shadow.
pub fn card(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, sf: f32, fill: u32, border: u32) {
    let r = RADIUS * sf;
    // no outline: the user's call
    let _ = border;
    frost(c, x, y, w, h, r, CARD_BLUR * sf, fill, CARD_TINT);
}


/// Shorten `s` from the middle with "…" until it fits `max` px.
fn fit_middle(t: &mut Text, face: Face, s: &str, px: f32, max: f32) -> String {
    if width(t, face, s, px) <= max {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    // largest k such that keeping k chars (split front/back) fits
    while lo < hi {
        let k = (lo + hi + 1) / 2;
        let front: String = chars[..k / 2 + k % 2].iter().collect();
        let back: String = chars[chars.len() - k / 2..].iter().collect();
        if width(t, face, &format!("{front}…{back}"), px) <= max {
            lo = k;
        } else {
            hi = k - 1;
        }
    }
    let k = lo;
    let front: String = chars[..k / 2 + k % 2].iter().collect();
    let back: String = chars[chars.len() - k / 2..].iter().collect();
    format!("{front}…{back}")
}

/// One row of the info card: label on the left, value on the right.
pub struct Row {
    pub key: String,
    pub value: String,
    pub mono: bool,
}

/// The info card (Tab): title, then sections of key/value rows split by dividers.
pub fn info_card(c: &mut Canvas, t: &mut Text, x: f32, y: f32, sf: f32, title: &str, sections: &[Vec<Row>]) {
    let d = |v: f32| v * sf;
    let (bpx, blh) = (d(BODY.0), d(BODY.1));
    // width: widest key + gap + widest value, clamped
    let mut need: f32 = width(t, Face::Title, title, d(CARD_TITLE.0));
    for r in sections.iter().flatten() {
        let vf = if r.mono { Face::Mono } else { Face::Regular };
        need = need.max(width(t, Face::Regular, &r.key, bpx) + d(KV_GAP) + width(t, vf, &r.value, bpx));
    }
    let w = (need + 2.0 * d(PAD)).clamp(d(MIN_W), d(MAX_W)).min(c.w as f32 - 2.0 * x).ceil();
    let rows: usize = sections.iter().map(|s| s.len()).sum();
    let gaps = sections.len() as f32 * (d(SECTION_GAP) * 2.0 + sf);
    let h = (d(PAD) * 2.0 + d(CARD_TITLE.1) + rows as f32 * (blh + d(ROW_GAP)) - d(ROW_GAP) + gaps).round();
    card(c, x, y, w, h, sf, PANEL2, SLATE600);
    let inner = w - 2.0 * d(PAD);
    let x0 = x + d(PAD);
    let mut top = y + d(PAD);
    let title = fit_middle(t, Face::Title, title, d(CARD_TITLE.0), inner);
    line(c, t, Face::Title, x0, top, &title, (d(CARD_TITLE.0), d(CARD_TITLE.1)), SLATE100);
    top += d(CARD_TITLE.1);
    for sec in sections {
        top += d(SECTION_GAP);
        c.rect_alpha(x0 as i32, top as i32, inner as i32, sf.max(1.0) as i32, LINE, 255);
        top += sf + d(SECTION_GAP);
        for r in sec {
            let kw = line(c, t, Face::Regular, x0, top, &r.key, (bpx, blh), SLATE500);
            let vf = if r.mono { Face::Mono } else { Face::Regular };
            let room = inner - kw - d(KV_GAP);
            let v = fit_middle(t, vf, &r.value, bpx, room);
            let vw = width(t, vf, &v, bpx);
            line(c, t, vf, x0 + inner - vw, top, &v, (bpx, blh), SLATE200);
            top += blh + d(ROW_GAP);
        }
        top -= d(ROW_GAP);
    }
}

/// The empty state's three actions: hit id, icon, label, hotkey.
pub const EMPTY_ACTIONS: [(&str, Icon, &str, &str); 3] = [
    ("empty_file", Icon::FileText, "Open a file", "F2"),
    ("empty_folder", Icon::FolderOpen, "Open a folder", "F"),
    ("empty_help", Icon::Keyboard, "Show hotkeys", "F1"),
];

/// The empty state: app name, what to do, and the three actions as clickable rows (icon,
/// label, key chip). `hover`: each action's hover amount 0-1. Returns each row's
/// rectangle (x0, y0, x1, y1), in EMPTY_ACTIONS order.
pub fn empty_card(c: &mut Canvas, t: &mut Text, cw: f32, ch: f32, sf: f32, hover: [f32; 3]) -> Vec<(f32, f32, f32, f32)> {
    let d = |v: f32| v * sf;
    // rows 32 px, 4 px apart; the hover fill reaches 8 px past the text on both sides,
    // inside the card's 24 px padding
    let (row, gap, reach) = (32.0, 4.0, 8.0);
    let w = d(320.0);
    let h = d(24.0 * 2.0 + TITLE.1 + 8.0 + BODY.1 + 16.0 + 3.0 * row + 2.0 * gap);
    let (x, y) = (((cw - w) / 2.0).round(), ((ch - h) / 2.0).round());
    card(c, x, y, w, h, sf, PANEL, LINE);
    let x0 = x + d(24.0);
    let inner = w - d(48.0);
    let mut top = y + d(24.0);
    line(c, t, Face::Title, x0, top, "ArkImage", (d(TITLE.0), d(TITLE.1)), SLATE100);
    top += d(TITLE.1 + 8.0);
    line(c, t, Face::Regular, x0, top, "Drop an image, a folder or an archive here.", (d(BODY.0), d(BODY.1)), SLATE400);
    top += d(BODY.1 + 16.0);
    let mut rects = Vec::new();
    for (i, (_, icon, label, key)) in EMPTY_ACTIONS.iter().enumerate() {
        let (rx, rw) = (x0 - d(reach), inner + 2.0 * d(reach));
        let ha = hover[i];
        if ha > 0.0 {
            c.fill_rrect(rx, top, rw, d(row), d(8.0), 0xFFFFFF, 0.08 * ha);
        }
        let col = mix(SLATE300, 0xFFFFFF, (ha * 255.0) as u32);
        // the label's line box centred in the row; the icon on its cap band
        let lt = top + d((row - BODY.1) / 2.0);
        let mid = cap_mid(t, Face::Regular, lt, (d(BODY.0), d(BODY.1)));
        let isz = d(16.0);
        crate::icons::draw(c, *icon, x0 + isz / 2.0, mid, isz, col);
        line(c, t, Face::Regular, x0 + isz + d(8.0), lt, label, (d(BODY.0), d(BODY.1)), col);
        // key chip: 24 px tall, centred in the row
        let kpx = d(LABEL.0);
        let kw = width(t, Face::Mono, key, kpx) + d(16.0);
        let kx = x0 + inner - kw;
        let ky = top + d((row - 24.0) / 2.0);
        c.fill_rrect(kx, ky, kw, d(24.0), d(4.0), LINE, 1.0);
        line(c, t, Face::Mono, kx + d(8.0), ky + d(4.0), key, (kpx, d(LABEL.1)), SLATE200);
        rects.push((rx, top, rx + rw, top + d(row)));
        top += d(row + gap);
    }
    rects
}

/// A plain message card: first line as the title, the rest as body text.
/// `at`: None centres it; Some((x, y)) puts its top-left there (negative x: from the right).
pub fn message_card(c: &mut Canvas, t: &mut Text, sf: f32, lines: &[String], at: Option<(f32, f32)>, error: bool) {
    let d = |v: f32| v * sf;
    let (bpx, blh) = (d(BODY.0), d(BODY.1));
    let max_w = (c.w as f32 - d(32.0)).max(d(100.0));
    let mut w: f32 = 0.0;
    for (i, l) in lines.iter().enumerate() {
        let f = if i == 0 { Face::Title } else { Face::Regular };
        w = w.max(width(t, f, l, bpx));
    }
    // ceil, never round: rounding down left the text 0.5 px short, and fit_middle cut it.
    let w = (w + 2.0 * d(PAD)).ceil().min(max_w);
    let h = (2.0 * d(PAD) + lines.len() as f32 * blh + if lines.len() > 1 { d(4.0) } else { 0.0 }).round();
    let (x, y) = match at {
        None => (((c.w as f32 - w) / 2.0).round(), ((c.h as f32 - h) / 2.0).round()),
        Some((x, y)) if x < 0.0 => ((c.w as f32 + x - w).round(), y),
        Some(p) => p,
    };
    card(c, x, y, w, h, sf, PANEL2, SLATE600);
    let mut top = y + d(PAD);
    for (i, l) in lines.iter().enumerate() {
        let (f, col) = if i == 0 { (Face::Title, if error { ROSE400 } else { SLATE100 }) } else { (Face::Regular, SLATE300) };
        let s = fit_middle(t, f, l, bpx, w - 2.0 * d(PAD));
        line(c, t, f, x + d(PAD), top, &s, (bpx, blh), col);
        top += blh + if i == 0 { d(4.0) } else { 0.0 };
    }
}

/// A modal card over a dimmed window: title, body lines, buttons on the right (the first
/// is primary). Returns each button's rectangle (x0, y0, x1, y1), in button order.
/// Fades in over 250 ms: `alpha` 0-1 blends the finished card over the frame below it.
pub fn modal_card(c: &mut Canvas, t: &mut Text, sf: f32, title: &str, body: &[String], buttons: &[&str], hover: Option<usize>, alpha: f32) -> Vec<(f32, f32, f32, f32)> {
    let below = (alpha < 1.0).then(|| c.buf.to_vec());
    let rects = modal_card_solid(c, t, sf, title, body, buttons, hover);
    if let Some(below) = below {
        let k = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
        // only the pixels the card drew: mixing the whole screen cost about 40 ms a
        // frame at 3456x2234
        // the same pair repeats over most of a card: reuse the last result
        let mut last = (0u32, 0u32, 0u32);
        for (p, b) in c.buf.iter_mut().zip(below) {
            if *p != b {
                if (*p, b) != (last.0, last.1) || last == (0, 0, 0) {
                    last = (*p, b, crate::render::mix(b, *p, k));
                }
                *p = last.2;
            }
        }
    }
    rects
}

/// Draw with `f`, then blend the result over the frame below at `alpha` 0-1: a card's
/// 250 ms fade without giving every fill and glyph an alpha of its own.
pub fn faded(c: &mut Canvas, alpha: f32, f: impl FnOnce(&mut Canvas)) {
    if alpha <= 0.0 {
        return;
    }
    let below = (alpha < 1.0).then(|| c.buf.to_vec());
    f(c);
    if let Some(below) = below {
        let k = (alpha.clamp(0.0, 1.0) * 255.0) as u32;
        // only the pixels the card drew: mixing the whole screen cost about 40 ms a
        // frame at 3456x2234
        // the same pair repeats over most of a card: reuse the last result
        let mut last = (0u32, 0u32, 0u32);
        for (p, b) in c.buf.iter_mut().zip(below) {
            if *p != b {
                if (*p, b) != (last.0, last.1) || last == (0, 0, 0) {
                    last = (*p, b, crate::render::mix(b, *p, k));
                }
                *p = last.2;
            }
        }
    }
}

/// How far a card that appeared (or left) at `at` has faded, over 250 ms; 0-1.
pub fn fade_amount(at: std::time::Instant, shown: bool) -> f32 {
    let k = (at.elapsed().as_secs_f32() / 0.25).min(1.0);
    if shown { k } else { 1.0 - k }
}

/// The Tabler icon drawn before a modal button's label, if any.
fn modal_icon(label: &str) -> Option<crate::icons::Icon> {
    use crate::icons::Icon;
    Some(match label {
        "Close All" => Icon::Power,
        "Cancel" | "Close" => Icon::Close,
        "Install" => Icon::Download,
        "Reinstall" => Icon::RotateCw2,
        _ => return None,
    })
}

fn modal_card_solid(c: &mut Canvas, t: &mut Text, sf: f32, title: &str, body: &[String], buttons: &[&str], hover: Option<usize>) -> Vec<(f32, f32, f32, f32)> {
    let d = |v: f32| v * sf;
    let (bpx, blh) = (d(BODY.0), d(BODY.1));
    // backdrop: black at 70%, so the image still shows through
    c.rect_alpha(0, 0, c.w as i32, c.h as i32, 0x000000, 178);
    let w = d(480.0).min(c.w as f32 - d(32.0)).round();
    let inner = w - d(48.0);
    let h = (d(24.0) + d(TITLE.1) + d(8.0) + body.len() as f32 * blh + d(24.0) + d(32.0) + d(24.0)).round();
    let (x, y) = (((c.w as f32 - w) / 2.0).round(), ((c.h as f32 - h) / 2.0).round());
    card(c, x, y, w, h, sf, PANEL, LINE);
    let x0 = x + d(24.0);
    let mut top = y + d(24.0);
    line(c, t, Face::Title, x0, top, title, (d(TITLE.0), d(TITLE.1)), SLATE100);
    top += d(TITLE.1 + 8.0);
    for l in body {
        let s = fit_middle(t, Face::Regular, l, bpx, inner);
        line(c, t, Face::Regular, x0, top, &s, (bpx, blh), SLATE300);
        top += blh;
    }
    // buttons, right-aligned, 8 px apart; the first (primary) is rightmost
    let by = y + h - d(24.0) - d(32.0);
    let mut right = x + w - d(24.0);
    let mut rects = vec![(0.0, 0.0, 0.0, 0.0); buttons.len()];
    for (i, label) in buttons.iter().enumerate() {
        // icon 16 px, 8 px before the label, as on the Settings buttons
        let icon = modal_icon(label);
        let cw = width(t, Face::Medium, label, bpx) + if icon.is_some() { d(24.0) } else { 0.0 };
        let bw = (cw + d(24.0)).ceil().max(d(96.0));
        let bx = right - bw;
        let hv = hover == Some(i);
        if i == 0 {
            c.fill_rrect(bx, by, bw, d(32.0), d(4.0), if hv { ACCENT_HOVER } else { ACCENT_FILL }, 1.0);
        } else {
            c.fill_rrect(bx, by, bw, d(32.0), d(4.0), LINE, 1.0);
            c.fill_rrect(bx + sf, by + sf, bw - 2.0 * sf, d(32.0) - 2.0 * sf, d(3.0), if hv { PANEL2 } else { 0x10141D }, 1.0);
        }
        let col = if i == 0 { 0xFFFFFF } else { SLATE200 };
        let mut lx = (bx + (bw - cw) / 2.0).round();
        if let Some(ic) = icon {
            crate::icons::draw(c, ic, lx + d(8.0), by + d(16.0), d(16.0), col);
            lx += d(24.0);
        }
        line(c, t, Face::Medium, lx, by + d(6.0), label, (bpx, blh), col);
        rects[i] = (bx, by, bx + bw, by + d(32.0));
        right = bx - d(8.0);
    }
    rects
}

/// Hotkey help: a card of "keys — action" rows in two columns.
/// Returns the close button's rectangle (x0, y0, x1, y1).
/// `band`: the free height (top, bottom) between the title strip and the bottom bar; the
/// card is centred in it (centred in the window, it ran under the bar).
pub fn help_card(c: &mut Canvas, t: &mut Text, sf: f32, title: &str, rows: &[(String, String)], close_hover: f32, band: (f32, f32)) -> (f32, f32, f32, f32) {
    let d = |v: f32| v * sf;
    let (bpx, blh) = (d(BODY.0), d(BODY.1));
    let kw = rows.iter().map(|(k, _)| width(t, Face::Mono, k, d(LABEL.0))).fold(0.0, f32::max);
    let aw = rows.iter().map(|(_, a)| width(t, Face::Regular, a, bpx)).fold(0.0, f32::max);
    let w = (kw + d(KV_GAP) + aw + 2.0 * d(PAD)).min(c.w as f32 - d(32.0)).round();
    let h = (2.0 * d(PAD) + d(TITLE.1) + d(SECTION_GAP) + rows.len() as f32 * (blh + d(ROW_GAP))).round();
    let x = ((c.w as f32 - w) / 2.0).max(0.0).round();
    let y = (band.0 + ((band.1 - band.0 - h) / 2.0).max(0.0)).round();
    card(c, x, y, w, h, sf, PANEL2, SLATE600);
    let x0 = x + d(PAD);
    let mut top = y + d(PAD);
    // the title as the other cards' titles (20/28), the close button on its right: a
    // 28 px target centred on the title's cap band, its 16 px glyph inside
    line(c, t, Face::Title, x0, top, title, (d(TITLE.0), d(TITLE.1)), SLATE100);
    let mid = cap_mid(t, Face::Title, top, (d(TITLE.0), d(TITLE.1)));
    let bs = d(28.0);
    // the icon's right edge on the content edge: the 28 px button overhangs the
    // padding by (28 - 16) / 2 = 6 px, off the 4 px grid for that reason
    let bx = x + w - d(PAD) - bs + d(6.0);
    let by = (mid - bs / 2.0).round();
    // no fill: the glyph brightens on hover
    let col = mix(SLATE400, 0xFFFFFF, (close_hover * 255.0) as u32);
    crate::icons::draw(c, Icon::Close, bx + bs / 2.0, mid, d(16.0), col);
    top += d(TITLE.1 + SECTION_GAP);
    for (k, a) in rows {
        line(c, t, Face::Mono, x0, top, k, (d(LABEL.0), blh), ACCENT);
        // the legend right-aligned on the content edge
        let lw = width(t, Face::Regular, a, bpx);
        line(c, t, Face::Regular, x + w - d(PAD) - lw, top, a, (bpx, blh), SLATE200);
        top += blh + d(ROW_GAP);
    }
    (bx, by, bx + bs, by + bs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card redrawn over the same plain interface reuses its blur and paints the same pixels.
    #[test]
    fn frost_reuses_its_blur() {
        let (w, h) = (3440usize, 1440usize);
        // plain interface pixels: top byte 0 (an odd top byte marks window glass)
        let base: Vec<u32> = (0..w * h).map(|i| (i as u32).wrapping_mul(2_654_435_761) >> 8 & 0xFF_FFFF).collect();
        let run = |n: usize| {
            let mut out = Vec::new();
            let mut ms = Vec::new();
            for _ in 0..n {
                let mut b = base.clone();
                let t = std::time::Instant::now();
                card(&mut Canvas { buf: &mut b, w, h }, 800.0, 200.0, 1840.0, 1040.0, 2.0, 0x101418, 0);
                ms.push(t.elapsed().as_secs_f64() * 1000.0);
                out = b;
            }
            (out, ms)
        };
        let (a, ms) = run(4);
        eprintln!("frost over plain interface, ms per frame: {ms:.1?}");
        let mut fresh = base.clone();
        frost_forget();
        card(&mut Canvas { buf: &mut fresh, w, h }, 800.0, 200.0, 1840.0, 1040.0, 2.0, 0x101418, 0);
        assert!(a == fresh, "kept blur painted other pixels");
        assert!(ms[3] * 3.0 < ms[0], "no reuse: {ms:?}");
    }

    /// The error card must fit its own text: no middle cut when the window has room.
    #[test]
    fn message_card_fits_its_text() {
        let mut t = Text::load().unwrap();
        let lines = ["Can't open the image file".to_string(), "arkimg.svg".into(), "The image format could not be determined".into()];
        for sf in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let d = |v: f32| v * sf;
            let bpx = d(BODY.0);
            let mut w: f32 = 0.0;
            for (i, l) in lines.iter().enumerate() {
                w = w.max(width(&mut t, if i == 0 { Face::Title } else { Face::Regular }, l, bpx));
            }
            let w = (w + 2.0 * d(PAD)).ceil();
            for (i, l) in lines.iter().enumerate() {
                let f = if i == 0 { Face::Title } else { Face::Regular };
                assert_eq!(&fit_middle(&mut t, f, l, bpx, w - 2.0 * d(PAD)), l, "sf {sf}");
            }
        }
    }
}
