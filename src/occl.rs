//! Windows: is a window covered? DWM does not say, and winit reports only a minimize.
//! A window is covered when one visible, opaque window above it in the z-order holds its
//! whole frame. Overlays never count (layered, click-through, tool, cloaked or minimized
//! windows): a wrong "covered" would stop a visible window from drawing. The window list
//! is built once per 100 ms for every window of ours (as on the Mac).

use std::ffi::c_void;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Rect {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Rect {
    fn holds(&self, o: &Rect) -> bool {
        self.l <= o.l && self.t <= o.t && self.r >= o.r && self.b >= o.b
    }
}

type Hwnd = isize;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, isize) -> i32, l: isize) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn IsIconic(h: Hwnd) -> i32;
    fn GetWindowLongPtrW(h: Hwnd, i: i32) -> isize;
}
#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(h: Hwnd, a: u32, v: *mut c_void, n: u32) -> i32;
}

/// Top to bottom: (window, frame on screen, may cover others).
static LIST: Mutex<Option<(Instant, Vec<(Hwnd, Rect, bool)>)>> = Mutex::new(None);

unsafe extern "system" fn add(h: Hwnd, l: isize) -> i32 {
    let v = &mut *(l as *mut Vec<(Hwnd, Rect, bool)>);
    if IsWindowVisible(h) == 0 {
        return 1;
    }
    let mut cloaked = 0u32;
    // DWMWA_CLOAKED
    DwmGetWindowAttribute(h, 14, &mut cloaked as *mut u32 as *mut c_void, 4);
    let mut r = Rect::default();
    // DWMWA_EXTENDED_FRAME_BOUNDS: the frame as drawn, without the invisible resize border
    if DwmGetWindowAttribute(h, 9, &mut r as *mut Rect as *mut c_void, 16) != 0 {
        return 1;
    }
    // GWL_EXSTYLE: WS_EX_LAYERED 0x80000, WS_EX_TRANSPARENT 0x20, WS_EX_TOOLWINDOW 0x80,
    // WS_EX_NOREDIRECTIONBITMAP 0x200000 (drawn by composition, may be see-through)
    let ex = GetWindowLongPtrW(h, -20) as u32;
    let opaque = cloaked == 0 && IsIconic(h) == 0 && ex & (0x80000 | 0x20 | 0x80 | 0x20_0000) == 0 && r.r > r.l && r.b > r.t;
    v.push((h, r, opaque));
    1
}

fn list() -> std::sync::MutexGuard<'static, Option<(Instant, Vec<(Hwnd, Rect, bool)>)>> {
    let mut g = LIST.lock().unwrap();
    if g.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_millis(100)) {
        let mut v: Vec<(Hwnd, Rect, bool)> = Vec::with_capacity(256);
        unsafe { EnumWindows(add, &mut v as *mut _ as isize) };
        *g = Some((Instant::now(), v));
    }
    g
}

/// True when an opaque window above `hwnd` holds its whole frame. `see_through`: this
/// window shows the glass; it still can be covered.
pub fn is_covered(hwnd: Hwnd) -> bool {
    let g = list();
    let Some((_, v)) = g.as_ref() else { return false };
    let Some(i) = v.iter().position(|x| x.0 == hwnd) else { return false };
    let me = v[i].1;
    v[..i].iter().any(|(_, r, opaque)| *opaque && r.holds(&me))
}

#[cfg(test)]
mod tests {
    use super::Rect;

    #[test]
    fn holds_only_the_whole_frame() {
        let big = Rect { l: 0, t: 0, r: 100, b: 100 };
        assert!(big.holds(&Rect { l: 10, t: 10, r: 90, b: 90 }));
        assert!(big.holds(&big));
        assert!(!big.holds(&Rect { l: 10, t: 10, r: 101, b: 90 }));
        assert!(!Rect { l: 10, t: 10, r: 90, b: 90 }.holds(&big));
    }
}
