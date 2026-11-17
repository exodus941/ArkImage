//! Windows popup menu that opens above a point, aligned on its left or right edge.
//! muda's popup always opens below and left-aligned, so the bar menus build their own
//! HMENU here and call TrackPopupMenu with TPM_BOTTOMALIGN.

use crate::app::{menu_label, MenuEntry};

type Hmenu = *mut std::ffi::c_void;
type Hwnd = isize;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[link(name = "user32")]
extern "system" {
    fn CreatePopupMenu() -> Hmenu;
    fn AppendMenuW(m: Hmenu, flags: u32, id: usize, text: *const u16) -> i32;
    fn TrackPopupMenu(m: Hmenu, flags: u32, x: i32, y: i32, r: i32, h: Hwnd, rc: *const std::ffi::c_void) -> i32;
    fn DestroyMenu(m: Hmenu) -> i32;
    fn ClientToScreen(h: Hwnd, p: *mut Point) -> i32;
    fn SetForegroundWindow(h: Hwnd) -> i32;
}

const MF_STRING: u32 = 0x0;
const MF_GRAYED: u32 = 0x1;
const MF_CHECKED: u32 = 0x8;
const MF_POPUP: u32 = 0x10;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTALIGN: u32 = 0x8;
const TPM_BOTTOMALIGN: u32 = 0x20;
const TPM_RETURNCMD: u32 = 0x100;

/// Which edge of the menu sits on the given point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Left,
    Right,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Append `items` to `m`; command ids index into `ids`.
unsafe fn fill(m: Hmenu, items: Vec<MenuEntry>, ids: &mut Vec<String>, min_w: usize) {
    for e in items {
        match e {
            MenuEntry::Item(id, t, k, en) => {
                ids.push(id);
                AppendMenuW(m, MF_STRING | if en { 0 } else { MF_GRAYED }, ids.len(), wide(&pad(menu_label(&t, k), min_w)).as_ptr());
            }
            MenuEntry::Check(id, t, k, on) => {
                ids.push(id);
                AppendMenuW(m, MF_STRING | if on { MF_CHECKED } else { 0 }, ids.len(), wide(&pad(menu_label(&t, k), min_w)).as_ptr());
            }
            MenuEntry::Sep => {
                AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            }
            MenuEntry::Sub(t, v) => {
                let s = CreatePopupMenu();
                fill(s, v, ids, 0);
                AppendMenuW(m, MF_POPUP, s as usize, wide(&t).as_ptr());
            }
        }
    }
}

/// Pad a label to `min_w` characters with a tab column, so a short menu is not narrower than its opener.
fn pad(s: String, min_w: usize) -> String {
    if min_w == 0 || s.contains('\t') {
        return s;
    }
    let n = s.chars().count();
    if n >= min_w { s } else { format!("{s}{}", "\u{2007}".repeat(min_w - n)) }
}

/// Show `items` with the menu's bottom edge on (x, y) client px of `hwnd` and its `edge` there.
/// `min_chars` pads every label to that many figure-width characters (0: none).
/// Returns the chosen id.
pub fn show_above(hwnd: Hwnd, items: Vec<MenuEntry>, x: i32, y: i32, edge: Edge, min_chars: usize) -> Option<String> {
    show(hwnd, items, x, y, edge, min_chars, true)
}

/// Show `items` with the menu's top edge on (x, y) client px of `hwnd` and its `edge` there.
pub fn show_below(hwnd: Hwnd, items: Vec<MenuEntry>, x: i32, y: i32, edge: Edge) -> Option<String> {
    show(hwnd, items, x, y, edge, 0, false)
}

fn show(hwnd: Hwnd, items: Vec<MenuEntry>, x: i32, y: i32, edge: Edge, min_chars: usize, above: bool) -> Option<String> {
    unsafe {
        let m = CreatePopupMenu();
        if m.is_null() {
            return None;
        }
        let mut ids = Vec::new();
        fill(m, items, &mut ids, min_chars);
        let mut p = Point { x, y };
        ClientToScreen(hwnd, &mut p);
        SetForegroundWindow(hwnd);
        let align = if above { TPM_BOTTOMALIGN } else { 0 } | TPM_RETURNCMD | if edge == Edge::Right { TPM_RIGHTALIGN } else { 0 };
        let cmd = TrackPopupMenu(m, align, p.x, p.y, 0, hwnd, std::ptr::null());
        DestroyMenu(m);
        (cmd > 0).then(|| ids.get(cmd as usize - 1).cloned()).flatten()
    }
}

/// Dark Win32 menus for the whole process: uxtheme ordinal 135 is SetPreferredAppMode
/// (2 = ForceDark), ordinal 136 is FlushMenuThemes. Undocumented, present since Windows 10 1903;
/// a missing export leaves the menus light.
pub fn dark_menus() {
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(n: *const u16) -> isize;
        fn GetProcAddress(m: isize, n: *const u8) -> usize;
    }
    unsafe {
        let m = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if m == 0 {
            return;
        }
        for (ord, arg) in [(135usize, 2), (136, 0)] {
            let f = GetProcAddress(m, ord as *const u8);
            if f != 0 {
                let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
                f(arg);
            }
        }
    }
}
