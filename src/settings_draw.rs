//! Drawing for the Configuration dialog: WinUI 3 dark structure (navigation pane with a
//! pill marker, one card per setting, toggle switches, combo boxes, number boxes) with
//! the app's own fonts and colours. Design px, 4 px grid.

use super::*;
use crate::icons::Icon;
use crate::render::sd_circle;

/// "Up to date" check: emerald-400. A status colour, so it is outside the icon's palette.
const GREEN: u32 = 0x34D399;

/// Navigation pane width; header and footer heights; card height, gap and padding.
const NAV_W: f32 = 232.0;
const HEAD_H: f32 = 56.0;
/// Mica frame around the page panel (top, right, bottom); the pane sits left of it.
const FRAME: f32 = 12.0;
const PANEL_R: f32 = 8.0;
/// Neutral greys that sit with Mica's grey: page panel, cards, borders, fields, and the
/// frame where there is no Mica. Text keeps the slate text ramp.
const PANEL: u32 = 0x262626;
const PANEL2: u32 = 0x303030;
const LINE: u32 = 0x3B3B3B;
/// The frame colour: the icon's grey, laid over Mica at FRAME_A (87.5%) so the wallpaper tint
/// shows through 12.5%; solid where there is no Mica.
const NO_MICA: u32 = u::ICON_GREY;
const FRAME_A: u32 = 223; // 87.5%
const FOOT_H: f32 = 64.0;
const CARD_H: f32 = 48.0;
const CARD_GAP: f32 = 4.0;
const CARD_PAD: f32 = 16.0;
/// Combo box width; control height.
const COMBO_W: f32 = 280.0;
const CTRL_H: f32 = 32.0;
/// Primary blue (sky-600), its hover (sky-500), and the input field fill.
const PRIMARY: u32 = u::ACCENT_FILL;
const PRIMARY_HOVER: u32 = u::ACCENT_HOVER;
const FIELD: u32 = 0x1C1C1C;
/// Entry fields (combo, number, path, name, hotkey): #1C1C1C at 50% over the card (#303030).
const INPUT: u32 = 0x262626;
/// Buttons: #1C1C1C mixed 25% with the brand blue ACCENT_FILL (#0080C5).
const BUTTON: u32 = 0x153546;
const BODY: (f32, f32) = (14.0, 20.0);
const SMALL: (f32, f32) = (12.0, 16.0);
const TITLE: (f32, f32) = (20.0, 28.0);

pub(super) struct DrawState<'a> {
    pub tab: usize,
    /// Hover fade per control, 0-1 (250 ms each way).
    pub fx: &'a [(Act, f32)],
    pub capturing: Option<usize>,
    pub renaming: bool,
    pub num_edit: Option<(NumF, &'a str)>,
    pub scroll: f32,
    /// Version row: (now, before, fade 0-1, seconds since the change).
    pub upd: (&'a Upd, &'a Upd, f32, f32),
}

/// One state of the Version row, drawn at `a` opacity (0-1) from x, centred on `mid`.
fn upd_status(c: &mut Canvas, t: &mut Text, x: f32, mid: f32, sf: f32, u: &Upd, a: f32, secs: f32, max_x: f32) {
    if a <= 0.0 {
        return;
    }
    let d = |v: f32| v * sf;
    let body = (d(BODY.0), d(BODY.1));
    let col = |k: u32| lerp(PANEL2, k, a);
    // icon 20 px, then 8 px, then the text
    let (icon, icol, deg, msg, tcol): (Option<Icon>, u32, u16, String, u32) = match u {
        Upd::Idle => (None, 0, 0, format!("Version {}", env!("CARGO_PKG_VERSION")), SLATE200),
        // one turn every 0.9 s, in 10 degree steps (36 cached masks)
        Upd::Checking => (Some(Icon::Loader), SLATE300, (((secs / 0.9).fract() * 36.0) as u16) * 10, "Checking for updates...".into(), SLATE200),
        Upd::Found(v) => (Some(Icon::Download), ACCENT, 0, format!("Update found, downloading version {v}"), SLATE200),
        Upd::Latest => (Some(Icon::CircleCheck), GREEN, 0, "You are already on the latest version.".into(), SLATE200),
        Upd::Failed(e) => (None, 0, 0, format!("Update check failed: {e}"), u::ROSE400),
    };
    let mut tx = x;
    if let Some(i) = icon {
        let s = d(20.0);
        crate::icons::draw_ex(c, i, x + s / 2.0, mid, s, icol, deg, a);
        tx += s + d(8.0);
    }
    // end with an ellipsis rather than run under the button
    let room = max_x - tx;
    let mut msg = msg;
    if u::width(t, Face::Regular, &msg, body.0) > room {
        while !msg.is_empty() && u::width(t, Face::Regular, &format!("{msg}…"), body.0) > room {
            msg.pop();
        }
        msg = format!("{}…", msg.trim_end());
    }
    text(c, t, Face::Regular, tx, mid, &msg, body, col(tcol));
}

/// The page panel's rectangle (x, y, w, h), physical px.
fn panel_rect(cw: f32, ch: f32, sf: f32) -> (f32, f32, f32, f32) {
    let (x, y) = (NAV_W * sf, FRAME * sf);
    (x, y, cw - x - FRAME * sf, ch - 2.0 * FRAME * sf)
}

/// With Mica: frame pixels still showing the plain frame colour become FRAME_A
/// translucent (premultiplied), so Mica shows through them; everything drawn is opaque.
pub(super) fn see_through(c: &mut Canvas, sf: f32) {
    let (px, py, pw, ph) = panel_rect(c.w as f32, c.h as f32, sf);
    let r = PANEL_R * sf;
    for y in 0..c.h {
        let row = &mut c.buf[y * c.w..(y + 1) * c.w];
        for (x, p) in row.iter_mut().enumerate() {
            let rgb = *p & 0xFFFFFF;
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let inside = fx > px + r && fx < px + pw - r && fy > py + r && fy < py + ph - r;
            *p = if !inside && rgb == NO_MICA {
                let m = |v: u32| v * FRAME_A / 255;
                FRAME_A << 24 | m(rgb >> 16 & 0xFF) << 16 | m(rgb >> 8 & 0xFF) << 8 | m(rgb & 0xFF)
            } else {
                0xFF << 24 | rgb
            };
        }
    }
}

impl DrawState<'_> {
    /// How far the hover on `a` has faded in, 0-1.
    fn amt(&self, a: Act) -> f32 {
        self.fx.iter().find(|(k, _)| *k == a).map(|(_, v)| *v).unwrap_or(0.0)
    }
}

/// Colour `t` of the way from `a` to `b`.
fn lerp(a: u32, b: u32, t: f32) -> u32 {
    crate::render::mix(a, b, (t.clamp(0.0, 1.0) * 255.0) as u32)
}

/// Box with a 1 px border.
fn boxed(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, fill: u32, edge: u32, sf: f32) {
    c.fill_rrect(x, y, w, h, r, edge, 1.0);
    c.fill_rrect(x + sf, y + sf, w - 2.0 * sf, h - 2.0 * sf, (r - sf).max(0.0), fill, 1.0);
}

/// Text vertically centred on `mid` in a line box of `lh`; returns its width.
fn text(c: &mut Canvas, t: &mut Text, face: Face, x: f32, mid: f32, s: &str, (px, lh): (f32, f32), col: u32) -> f32 {
    u::line(c, t, face, x, (mid - lh / 2.0).round(), s, (px, lh), col)
}

fn caret(c: &mut Canvas, x: f32, mid: f32, sf: f32) {
    c.rect_alpha(x as i32, (mid - 8.0 * sf) as i32, sf.max(1.0) as i32, (16.0 * sf) as i32, SLATE100, 255);
}

/// Combo box chevron: Tabler chevron-down, 16 px.
fn chevron(c: &mut Canvas, ax: f32, ay: f32, sf: f32, col: u32) {
    crate::icons::draw(c, crate::icons::Icon::ChevronDown, ax, ay, 16.0 * sf, col);
}

/// WinUI toggle switch, 40 x 20: blue with a white knob on the right when on; an outline
/// with a grey knob on the left when off. The knob grows 12 -> 14 px on hover.
fn switch(c: &mut Canvas, x: f32, mid: f32, sf: f32, on: bool, hover: f32) {
    let d = |v: f32| v * sf;
    let (w, h) = (d(40.0), d(20.0));
    let y = (mid - h / 2.0).round();
    let kr = d(6.0 + hover);
    let cy = y + h / 2.0;
    if on {
        c.fill_rrect(x, y, w, h, h / 2.0, lerp(PRIMARY, PRIMARY_HOVER, hover), 1.0);
        let kx = x + w - h / 2.0;
        c.sdf((kx - kr - 2.0, cy - kr - 2.0, kx + kr + 2.0, cy + kr + 2.0), 0xFFFFFF, 1.0, |px, py| sd_circle(px, py, kx, cy, kr));
    } else {
        boxed(c, x, y, w, h, h / 2.0, lerp(FIELD, PANEL2, hover), SLATE400, sf);
        let kx = x + h / 2.0;
        c.sdf((kx - kr - 2.0, cy - kr - 2.0, kx + kr + 2.0, cy + kr + 2.0), SLATE400, 1.0, |px, py| sd_circle(px, py, kx, cy, kr));
    }
}

/// Combo box: field fill, 1 px border (blue on hover), text and a chevron.
fn combo(c: &mut Canvas, t: &mut Text, x: f32, mid: f32, w: f32, sf: f32, label: &str, on: bool, hover: f32) {
    let d = |v: f32| v * sf;
    let (h, y) = (d(CTRL_H), (mid - d(CTRL_H) / 2.0).round());
    boxed(c, x, y, w, h, d(4.0), if on { INPUT } else { PANEL }, lerp(LINE, ACCENT, hover), sf);
    let room = w - d(12.0 + 32.0);
    let mut s = label.to_string();
    let cut = u::width(t, Face::Regular, &s, d(BODY.0)) > room;
    while u::width(t, Face::Regular, &format!("{s}…"), d(BODY.0)) > room && s.chars().count() > 2 && cut {
        s.pop();
    }
    if cut {
        s.push('…');
    }
    text(c, t, Face::Regular, x + d(12.0), mid, &s, (d(BODY.0), d(BODY.1)), if on { SLATE200 } else { SLATE500 });
    chevron(c, x + w - d(16.0), mid, sf, if on { SLATE400 } else { SLATE500 });
}

/// The Tabler icon drawn before a button's label, if any.
fn button_icon(label: &str) -> Option<Icon> {
    Some(match label {
        "Reinstall…" => Icon::RotateCw2,
        "Reset…" => Icon::Restore,
        "GitHub" => Icon::GitHub,
        "Check for Updates" => Icon::Refresh,
        "Cancel" => Icon::Close,
        "OK" => Icon::Check,
        "New (copy of this preset)" => Icon::CopyPlus,
        "Delete" => Icon::Trash,
        "Browse…" => Icon::FolderOpen,
        "Clear" => Icon::Eraser,
        "Open" => Icon::FileText,
        _ => return None,
    })
}

/// Icon 16 px, then 8 px, then the label.
const BTN_ICON: f32 = 16.0;
const BTN_ICON_GAP: f32 = 8.0;

/// Width of the label plus its icon.
fn button_content_w(t: &mut Text, sf: f32, label: &str) -> f32 {
    let tw = u::width(t, Face::Medium, label, BODY.0 * sf);
    if button_icon(label).is_some() { tw + (BTN_ICON + BTN_ICON_GAP) * sf } else { tw }
}

fn button_w(t: &mut Text, sf: f32, label: &str) -> f32 {
    (button_content_w(t, sf, label) + 24.0 * sf).ceil().max(80.0 * sf)
}

/// Outline button with its label centred; `on` false greys it.
fn button(c: &mut Canvas, t: &mut Text, x: f32, mid: f32, w: f32, sf: f32, label: &str, primary: bool, on: bool, hover: f32) {
    let d = |v: f32| v * sf;
    let (h, y) = (d(CTRL_H), (mid - d(CTRL_H) / 2.0).round());
    if primary {
        c.fill_rrect(x, y, w, h, d(4.0), lerp(PRIMARY, PRIMARY_HOVER, hover), 1.0);
    } else {
        boxed(c, x, y, w, h, d(4.0), lerp(BUTTON, PANEL2, if on { hover } else { 0.0 }), LINE, sf);
    }
    let col = if primary { 0xFFFFFF } else if on { SLATE200 } else { SLATE500 };
    let cw = button_content_w(t, sf, label);
    let mut lx = (x + (w - cw) / 2.0).round();
    if let Some(i) = button_icon(label) {
        crate::icons::draw(c, i, lx + d(BTN_ICON) / 2.0, mid, d(BTN_ICON), col);
        lx += d(BTN_ICON + BTN_ICON_GAP);
    }
    text(c, t, Face::Medium, lx, mid, label, (d(BODY.0), d(BODY.1)), col);
}

/// Draw the dialog; returns how far the page can scroll (design px).
pub(super) fn draw(c: &mut Canvas, t: Option<&mut Text>, cfg: &Config, st: &DrawState, sf: f32, hits: &mut Vec<Hit>) -> f32 {
    let d = |v: f32| v * sf;
    // with Mica the pane is black, which see_through() turns clear
    c.fill(NO_MICA);
    let Some(t) = t else { return 0.0 };
    let (cw, ch) = (c.w as f32, c.h as f32);
    let body = (d(BODY.0), d(BODY.1));
    let small = (d(SMALL.0), d(SMALL.1));

    // ---- page content (scrolls): drawn first; the header and footer cover its overflow ----
    // page panel: rounded on all corners, inside a 12 px Mica frame
    let (lx, py, pw, ph) = panel_rect(cw, ch, sf);
    let pr = d(PANEL_R);
    c.fill_rrect(lx, py, pw, ph, pr, PANEL, 1.0);
    let x0 = lx + d(24.0);
    let card_w = pw - d(48.0);
    let right = x0 + card_w - d(CARD_PAD);
    let (top_clip, bot_clip) = (py + d(HEAD_H), py + ph - d(FOOT_H));
    let mut y = top_clip + d(4.0) - d(st.scroll);
    let mut ch_hits: Vec<Hit> = Vec::new();
    let mut hit = |x0: f32, y0: f32, x1: f32, y1: f32, act: Act| ch_hits.push(Hit { x0, y0, x1, y1, act });
    let card = |c: &mut Canvas, y: f32| boxed(c, x0, y, card_w, d(CARD_H), d(4.0), PANEL2, LINE, sf);
    let ctrl_y = |mid: f32| (mid - d(CTRL_H) / 2.0).round();
    for ctl in page(st.tab) {
        let mid = y + d(CARD_H) / 2.0;
        match ctl {
            Ctl::Gap => y += d(12.0),
            Ctl::Note(s) => {
                text(c, t, Face::Regular, x0 + d(4.0), y + d(4.0) + small.1 / 2.0, s, small, SLATE500);
                y += d(SMALL.1 + 8.0);
            }
            Ctl::Check(label, f) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label, body, SLATE200);
                let on = f.get(cfg);
                let sx = right - d(40.0);
                let word = if on { "On" } else { "Off" };
                let ww = u::width(t, Face::Regular, word, body.0);
                text(c, t, Face::Regular, sx - d(12.0) - ww, mid, word, body, SLATE400);
                switch(c, sx, mid, sf, on, st.amt(Act::Toggle(f)));
                hit(x0, y, x0 + card_w, y + d(CARD_H), Act::Toggle(f));
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Choice(label, f) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label.trim_end_matches(':'), body, SLATE200);
                let on = f.editable(cfg);
                let cur = f.options(cfg).get(f.get(cfg) as usize).cloned().unwrap_or_default();
                let bx = right - d(COMBO_W);
                combo(c, t, bx, mid, d(COMBO_W), sf, &cur, on, if on { st.amt(Act::Choice(f)) } else { 0.0 });
                if on {
                    hit(bx, ctrl_y(mid), right, ctrl_y(mid) + d(CTRL_H), Act::Choice(f));
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Num(label, f) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label.trim_end_matches(':'), body, SLATE200);
                let on = f.editable(cfg);
                let (fw, bw) = (d(96.0), d(32.0));
                let bx = right - fw - 2.0 * (d(4.0) + bw);
                let fy = ctrl_y(mid);
                let typing = st.num_edit.filter(|(g, _)| *g == f).map(|(_, s)| s);
                let edge = lerp(LINE, ACCENT, if typing.is_some() { 1.0 } else if on { st.amt(Act::NumEdit(f)) } else { 0.0 });
                boxed(c, bx, fy, fw, d(CTRL_H), d(4.0), if on { INPUT } else { PANEL }, edge, sf);
                if let Some(s) = typing {
                    let tw = text(c, t, Face::Mono, bx + d(12.0), mid, s, body, SLATE100);
                    caret(c, bx + d(13.0) + tw, mid, sf);
                } else {
                    text(c, t, Face::Mono, bx + d(12.0), mid, &f.show(cfg), body, if on { SLATE200 } else { SLATE500 });
                }
                if on {
                    hit(bx, fy, bx + fw, fy + d(CTRL_H), Act::NumEdit(f));
                }
                for (k, (sym, dir)) in [("−", -1i64), ("+", 1)].iter().enumerate() {
                    let sx = bx + fw + d(4.0) + (bw + d(4.0)) * k as f32;
                    let act = Act::Step(f, *dir);
                    boxed(c, sx, fy, bw, d(CTRL_H), d(4.0), lerp(BUTTON, PANEL2, if on { st.amt(act) } else { 0.0 }), LINE, sf);
                    let tw = u::width(t, Face::Medium, sym, body.0);
                    text(c, t, Face::Medium, sx + (bw - tw) / 2.0, mid, sym, body, if on { SLATE200 } else { SLATE500 });
                    if on {
                        hit(sx, fy, sx + bw, fy + d(CTRL_H), act);
                    }
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Swatches(label) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label.trim_end_matches(':'), body, SLATE200);
                let n = SWATCHES.len() as f32;
                let mut sx = right - (n * d(20.0) + (n - 1.0) * d(8.0));
                let sy = (mid - d(10.0)).round();
                for col in SWATCHES {
                    boxed(c, sx, sy, d(20.0), d(20.0), d(4.0), col, u::SLATE600, sf);
                    if cfg.bg_color == col {
                        c.stroke_rrect(sx - d(4.0), sy - d(4.0), d(28.0), d(28.0), d(8.0), d(2.0), ACCENT, 1.0);
                    }
                    hit(sx, sy, sx + d(20.0), sy + d(20.0), Act::Swatch(col));
                    sx += d(28.0);
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Path(label, f) => {
                card(c, y);
                let lw = text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label.trim_end_matches(':'), body, SLATE200);
                let (b1, b2) = (button_w(t, sf, "Browse…"), button_w(t, sf, "Clear"));
                let clear_x = right - b2;
                let browse_x = clear_x - d(8.0) - b1;
                let px0 = x0 + d(CARD_PAD) + lw.max(d(112.0)) + d(16.0);
                let pw = browse_x - d(8.0) - px0;
                let fy = ctrl_y(mid);
                boxed(c, px0, fy, pw, d(CTRL_H), d(4.0), INPUT, LINE, sf);
                let v = f.get(cfg);
                let shown = if v.is_empty() { "Not set".to_string() } else { v.to_string() };
                let mut s = shown.clone();
                while u::width(t, Face::Regular, &s, body.0) > pw - d(24.0) && s.chars().count() > 4 {
                    s.remove(0);
                }
                if s != shown {
                    s.insert(0, '…');
                }
                text(c, t, Face::Regular, px0 + d(12.0), mid, &s, body, if v.is_empty() { SLATE500 } else { SLATE200 });
                button(c, t, browse_x, mid, b1, sf, "Browse…", false, true, st.amt(Act::Browse(f)));
                hit(browse_x, fy, browse_x + b1, fy + d(CTRL_H), Act::Browse(f));
                button(c, t, clear_x, mid, b2, sf, "Clear", false, true, st.amt(Act::Clear(f)));
                hit(clear_x, fy, clear_x + b2, fy + d(CTRL_H), Act::Clear(f));
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Button(label, act) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, label, body, SLATE200);
                let verb = match act {
                    Act::OpenIni => "Open",
                    Act::Reset => "Reset…",
                    Act::Install if crate::install::is_installed() => "Reinstall…",
                    Act::Install => "Install…",
                    Act::CheckUpdate => "Check now",
                    _ => "Clear",
                };
                let bw = button_w(t, sf, verb);
                button(c, t, right - bw, mid, bw, sf, verb, false, true, st.amt(act));
                hit(right - bw, ctrl_y(mid), right, ctrl_y(mid) + d(CTRL_H), act);
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Version => {
                card(c, y);
                let (now, before, m, secs) = st.upd;
                // no second check while one runs or an update downloads
                let on = !matches!(now, Upd::Checking | Upd::Found(_));
                let verb = "Check for Updates";
                let bw = button_w(t, sf, verb);
                // GitHub button 8 px left of Check for Updates
                let gw = button_w(t, sf, "GitHub");
                let gx = right - bw - d(8.0) - gw;
                button(c, t, gx, mid, gw, sf, "GitHub", false, true, st.amt(Act::GitHub));
                hit(gx, ctrl_y(mid), gx + gw, ctrl_y(mid) + d(CTRL_H), Act::GitHub);
                let max_x = gx - d(16.0);
                upd_status(c, t, x0 + d(CARD_PAD), mid, sf, before, 1.0 - m, secs, max_x);
                upd_status(c, t, x0 + d(CARD_PAD), mid, sf, now, m, secs, max_x);
                button(c, t, right - bw, mid, bw, sf, verb, false, on, if on { st.amt(Act::CheckUpdate) } else { 0.0 });
                if on {
                    hit(right - bw, ctrl_y(mid), right, ctrl_y(mid) + d(CTRL_H), Act::CheckUpdate);
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::Buttons(l1, a1, l2, a2) => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, "Manage presets", body, SLATE200);
                let (w1, w2) = (button_w(t, sf, l1), button_w(t, sf, l2));
                let x2 = right - w2;
                let x1 = x2 - d(8.0) - w1;
                let fy = ctrl_y(mid);
                button(c, t, x1, mid, w1, sf, l1, false, true, st.amt(a1));
                hit(x1, fy, x1 + w1, fy + d(CTRL_H), a1);
                let on = !preset_locked(cfg);
                button(c, t, x2, mid, w2, sf, l2, false, on, if on { st.amt(a2) } else { 0.0 });
                if on {
                    hit(x2, fy, x2 + w2, fy + d(CTRL_H), a2);
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::PresetName => {
                card(c, y);
                text(c, t, Face::Regular, x0 + d(CARD_PAD), mid, "Name", body, SLATE200);
                let on = !preset_locked(cfg);
                let bx = right - d(COMBO_W);
                let fy = ctrl_y(mid);
                let edge = lerp(LINE, ACCENT, if st.renaming { 1.0 } else if on { st.amt(Act::PresetRename) } else { 0.0 });
                boxed(c, bx, fy, d(COMBO_W), d(CTRL_H), d(4.0), if on { INPUT } else { PANEL }, edge, sf);
                let name = crate::filters::get(&cfg.filter_presets, cfg.filter).name;
                let tw = text(c, t, Face::Regular, bx + d(12.0), mid, &name, body, if on { SLATE200 } else { SLATE500 });
                if st.renaming {
                    caret(c, bx + d(13.0) + tw, mid, sf);
                }
                if on {
                    hit(bx, fy, bx + d(COMBO_W), fy + d(CTRL_H), Act::PresetRename);
                }
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::KeyHeader => {
                let m = y + d(4.0) + small.1 / 2.0;
                text(c, t, Face::Medium, x0 + d(CARD_PAD), m, "ON", small, SLATE500);
                text(c, t, Face::Medium, x0 + d(72.0), m, "HOTKEY", small, SLATE500);
                text(c, t, Face::Medium, right - d(COMBO_W), m, "ACTION", small, SLATE500);
                y += d(SMALL.1 + 12.0);
            }
            Ctl::KeyRow(i) => {
                let b = &cfg.keys[i];
                card(c, y);
                switch(c, x0 + d(CARD_PAD), mid, sf, b.on, st.amt(Act::KeyToggle(i)));
                hit(x0 + d(CARD_PAD), mid - d(12.0), x0 + d(CARD_PAD + 40.0), mid + d(12.0), Act::KeyToggle(i));
                let (kx, kw) = (x0 + d(72.0), d(160.0));
                let fy = ctrl_y(mid);
                let cap = st.capturing == Some(i);
                let edge = lerp(LINE, ACCENT, if cap { 1.0 } else { st.amt(Act::KeyCapture(i)) });
                boxed(c, kx, fy, kw, d(CTRL_H), d(4.0), INPUT, edge, sf);
                let kt = if cap { "Press a key…".to_string() } else if b.key.is_empty() { "None".to_string() } else { crate::bindings::display(&b.key) };
                let face = if cap || b.key.is_empty() { Face::Regular } else { Face::Mono };
                text(c, t, face, kx + d(12.0), mid, &kt, body, if cap { ACCENT } else if b.key.is_empty() { SLATE500 } else { SLATE200 });
                hit(kx, fy, kx + kw, fy + d(CTRL_H), Act::KeyCapture(i));
                let ax = right - d(COMBO_W);
                combo(c, t, ax, mid, d(COMBO_W), sf, crate::bindings::label(&b.action), b.on, st.amt(Act::KeyAction(i)));
                hit(ax, fy, right, fy + d(CTRL_H), Act::KeyAction(i));
                y += d(CARD_H + CARD_GAP);
            }
            Ctl::MouseRow(i) => {
                let b = &cfg.mouse[i];
                card(c, y);
                switch(c, x0 + d(CARD_PAD), mid, sf, b.on, st.amt(Act::MouseToggle(i)));
                hit(x0 + d(CARD_PAD), mid - d(12.0), x0 + d(CARD_PAD + 40.0), mid + d(12.0), Act::MouseToggle(i));
                text(c, t, Face::Regular, x0 + d(72.0), mid, crate::bindings::MOUSE_SLOTS[i].1, body, SLATE200);
                let ax = right - d(COMBO_W);
                let fy = ctrl_y(mid);
                combo(c, t, ax, mid, d(COMBO_W), sf, crate::bindings::label(&b.action), b.on, st.amt(Act::MouseAction(i)));
                hit(ax, fy, right, fy + d(CTRL_H), Act::MouseAction(i));
                y += d(CARD_H + CARD_GAP);
            }
        }
    }
    let content_end = y + d(st.scroll) + d(12.0);
    let scroll_max = ((content_end - bot_clip) / sf).max(0.0);
    // only controls inside the visible band take clicks
    hits.extend(ch_hits.into_iter().filter(|h| h.y0 >= top_clip - 1.0 && h.y1 <= bot_clip + 1.0));

    // ---- header: page title over the content layer ----
    // Repaint the top band: first the frame above and around the panel's top corners,
    // then the panel's top (rounded), so cards scrolled up show nowhere above the header.
    c.rect_alpha(lx as i32, 0, (cw - lx) as i32 + 1, (py + 2.0 * pr).ceil() as i32, NO_MICA, 255);
    c.fill_rrect(lx, py, pw, 2.0 * pr, pr, PANEL, 1.0);
    c.rect_alpha(lx as i32, (py + pr) as i32, pw as i32, (top_clip - py - pr) as i32, PANEL, 255);
    // title on the first navigation item's line (both centred 38 px down)
    text(c, t, Face::Title, x0, d(38.0), TABS[st.tab], (d(TITLE.0), d(TITLE.1)), SLATE100);
    if st.scroll > 0.0 {
        c.rect_alpha(lx as i32, (top_clip - sf) as i32, pw as i32, sf.max(1.0) as i32, LINE, 255);
    }

    // ---- footer: Reset and OK on the right ----
    let bottom = py + ph;
    // the frame below the panel and around its bottom corners, then the footer
    c.rect_alpha(lx as i32, (bottom - 2.0 * pr).floor() as i32, (cw - lx) as i32 + 1, (ch - bottom + 2.0 * pr).ceil() as i32 + 1, NO_MICA, 255);
    c.rect_alpha(lx as i32, bot_clip as i32, pw as i32, (bottom - pr - bot_clip) as i32 + 1, PANEL, 255);
    c.fill_rrect(lx, bottom - 2.0 * pr, pw, 2.0 * pr, pr, PANEL, 1.0);
    c.rect_alpha(lx as i32, bot_clip as i32, pw as i32, sf.max(1.0) as i32, LINE, 255);
    let fmid = bot_clip + d(FOOT_H) / 2.0;
    let (wo, wr) = (button_w(t, sf, "OK"), button_w(t, sf, "Cancel"));
    let ox = lx + pw - d(24.0) - wo;
    let rx = ox - d(8.0) - wr;
    let fy = ctrl_y(fmid);
    button(c, t, rx, fmid, wr, sf, "Cancel", false, true, st.amt(Act::Cancel));
    hits.push(Hit { x0: rx, y0: fy, x1: rx + wr, y1: fy + d(CTRL_H), act: Act::Cancel });
    button(c, t, ox, fmid, wo, sf, "OK", true, true, st.amt(Act::Ok));
    hits.push(Hit { x0: ox, y0: fy, x1: ox + wo, y1: fy + d(CTRL_H), act: Act::Ok });

    // ---- navigation pane ----
    for (i, name) in TABS.iter().enumerate() {
        let (x, y, w, h) = (d(8.0), d(20.0) + d(40.0) * i as f32, d(NAV_W - 16.0), d(36.0));
        let sel = i == st.tab;
        let hv = st.amt(Act::Tab(i));
        if sel {
            c.fill_rrect(x, y, w, h, d(4.0), PANEL2, 1.0);
        } else if hv > 0.0 {
            c.fill_rrect(x, y, w, h, d(4.0), PANEL2, 0.6 * hv);
        }
        if sel {
            // WinUI selection pill: 3 x 16, blue
            c.fill_rrect(x, y + h / 2.0 - d(8.0), d(3.0), d(16.0), d(1.5), u::ACCENT_LIGHT, 1.0);
        }
        text(c, t, if sel { Face::Medium } else { Face::Regular }, x + d(16.0), y + h / 2.0, name, body, if sel { SLATE100 } else { SLATE300 });
        hits.push(Hit { x0: x, y0: y, x1: x + w, y1: y + h, act: Act::Tab(i) });
    }
    scroll_max
}
