//! Cards drawn over the image: the info panel, the empty state, messages and hotkey help.
//! Style: dark surfaces, the slate text ramp, Geist / Figtree / Geist Mono, 14/20 rows, 4 px grid.

use crate::render::Canvas;
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

pub fn width(t: &mut Text, face: Face, s: &str, px: f32) -> f32 {
    let prev = t.face(face);
    let w = t.width(s, px);
    t.face(prev);
    w
}

/// Card surface: two-layer drop shadow, fill, 1 px border.
pub fn card(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, sf: f32, fill: u32, border: u32) {
    let r = RADIUS * sf;
    // Approximates `0 2px 8px rgb(0 0 0 / .88), 0 12px 32px rgb(0 0 0 / .66)` with stacked rings.
    for (dy, spread, a) in [(12.0, 16.0, 0.05), (8.0, 10.0, 0.08), (4.0, 6.0, 0.12), (2.0, 3.0, 0.18), (1.0, 1.0, 0.25)] {
        let (d, s) = (dy * sf, spread * sf);
        c.fill_rrect(x - s, y - s + d, w + 2.0 * s, h + 2.0 * s, r + s, 0x000000, a);
    }
    c.fill_rrect(x, y, w, h, r, fill, 0.96);
    c.stroke_rrect(x, y, w, h, r, 1.0 * sf, border, 1.0);
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

/// The empty state: app name, what to do, and the three hotkeys as key chips.
pub fn empty_card(c: &mut Canvas, t: &mut Text, cw: f32, ch: f32, sf: f32) {
    let d = |v: f32| v * sf;
    let keys = [("Open a file", "F2"), ("Open a folder", "F"), ("Show hotkeys", "F1")];
    let w = d(320.0);
    let h = d(24.0 * 2.0 + TITLE.1 + 8.0 + BODY.1 + 24.0 + 3.0 * 28.0 - 4.0);
    let (x, y) = (((cw - w) / 2.0).round(), ((ch - h) / 2.0).round());
    card(c, x, y, w, h, sf, PANEL, LINE);
    let x0 = x + d(24.0);
    let inner = w - d(48.0);
    let mut top = y + d(24.0);
    line(c, t, Face::Title, x0, top, "ArkImage", (d(TITLE.0), d(TITLE.1)), SLATE100);
    top += d(TITLE.1 + 8.0);
    line(c, t, Face::Regular, x0, top, "Drop an image, a folder or an archive here.", (d(BODY.0), d(BODY.1)), SLATE400);
    top += d(BODY.1 + 24.0);
    for (label, key) in keys {
        // row 24 px: label left, key chip right, 4 px apart vertically
        line(c, t, Face::Regular, x0, top + d(2.0), label, (d(BODY.0), d(BODY.1)), SLATE300);
        let kpx = d(LABEL.0);
        let kw = width(t, Face::Mono, key, kpx) + d(16.0);
        let kx = x0 + inner - kw;
        c.fill_rrect(kx, top, kw, d(24.0), d(4.0), LINE, 1.0);
        line(c, t, Face::Mono, kx + d(8.0), top + d(4.0), key, (kpx, d(LABEL.1)), SLATE200);
        top += d(28.0);
    }
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
    let w = (w + 2.0 * d(PAD)).min(max_w).round();
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
pub fn modal_card(c: &mut Canvas, t: &mut Text, sf: f32, title: &str, body: &[String], buttons: &[&str], hover: Option<usize>) -> Vec<(f32, f32, f32, f32)> {
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
        let bw = (width(t, Face::Medium, label, bpx) + d(24.0)).ceil().max(d(96.0));
        let bx = right - bw;
        let hv = hover == Some(i);
        if i == 0 {
            c.fill_rrect(bx, by, bw, d(32.0), d(4.0), if hv { ACCENT_HOVER } else { ACCENT_FILL }, 1.0);
        } else {
            c.fill_rrect(bx, by, bw, d(32.0), d(4.0), LINE, 1.0);
            c.fill_rrect(bx + sf, by + sf, bw - 2.0 * sf, d(32.0) - 2.0 * sf, d(3.0), if hv { PANEL2 } else { 0x10141D }, 1.0);
        }
        let tw = width(t, Face::Medium, label, bpx);
        line(c, t, Face::Medium, bx + (bw - tw) / 2.0, by + d(6.0), label, (bpx, blh), if i == 0 { 0xFFFFFF } else { SLATE200 });
        rects[i] = (bx, by, bx + bw, by + d(32.0));
        right = bx - d(8.0);
    }
    rects
}

/// Hotkey help: a card of "keys — action" rows in two columns.
pub fn help_card(c: &mut Canvas, t: &mut Text, sf: f32, title: &str, rows: &[(String, String)]) {
    let d = |v: f32| v * sf;
    let (bpx, blh) = (d(BODY.0), d(BODY.1));
    let kw = rows.iter().map(|(k, _)| width(t, Face::Mono, k, d(LABEL.0))).fold(0.0, f32::max);
    let aw = rows.iter().map(|(_, a)| width(t, Face::Regular, a, bpx)).fold(0.0, f32::max);
    let w = (kw + d(KV_GAP) + aw + 2.0 * d(PAD)).min(c.w as f32 - d(32.0)).round();
    let h = (2.0 * d(PAD) + d(CARD_TITLE.1) + d(SECTION_GAP) + rows.len() as f32 * (blh + d(ROW_GAP))).round();
    let (x, y) = (((c.w as f32 - w) / 2.0).max(0.0).round(), ((c.h as f32 - h) / 2.0).max(0.0).round());
    card(c, x, y, w, h, sf, PANEL2, SLATE600);
    let x0 = x + d(PAD);
    let mut top = y + d(PAD);
    line(c, t, Face::Title, x0, top, title, (d(CARD_TITLE.0), d(CARD_TITLE.1)), SLATE100);
    top += d(CARD_TITLE.1 + SECTION_GAP);
    for (k, a) in rows {
        line(c, t, Face::Mono, x0, top, k, (d(LABEL.0), blh), ACCENT);
        line(c, t, Face::Regular, x0 + kw + d(KV_GAP), top, a, (bpx, blh), SLATE200);
        top += blh + d(ROW_GAP);
    }
}
