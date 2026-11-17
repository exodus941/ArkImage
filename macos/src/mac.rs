//! macOS integration: Liquid Glass behind Settings, files sent by Finder, Show in Finder.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use objc2::{
    class, ffi, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Imp, Sel},
    sel,
};
use objc2_foundation::{NSArray, NSRect, NSString, NSURL};
use winit::{
    event_loop::EventLoopProxy,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

use crate::app::UserEvent;

static PROXY: OnceLock<Mutex<EventLoopProxy<UserEvent>>> = OnceLock::new();

/// Finder sends files to a running app as `application:openURLs:` (double-click, Open With,
/// a drop on the Dock icon), not on the command line. winit 0.30 does not forward it, so add
/// the method to winit's application delegate. Call after the event loop is built.
pub fn hook_open_files(proxy: EventLoopProxy<UserEvent>) {
    let _ = PROXY.set(Mutex::new(proxy));
    unsafe extern "C-unwind" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut NSArray<NSURL>) {
        let Some(urls) = (unsafe { urls.as_ref() }) else { return };
        let Some(proxy) = PROXY.get() else { return };
        let Ok(proxy) = proxy.lock() else { return };
        for u in urls.iter() {
            if let Some(p) = u.path() {
                crate::trace(&format!("finder open {p}"));
                let _ = proxy.send_event(UserEvent::Open(Some(PathBuf::from(p.to_string()))));
            }
        }
    }
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let del: *mut AnyObject = msg_send![app, delegate];
        let Some(del) = del.as_ref() else { return };
        let cls = del.class() as *const AnyClass as *mut AnyClass;
        let imp: Imp = std::mem::transmute(open_urls as unsafe extern "C-unwind" fn(_, _, _, _));
        ffi::class_addMethod(cls, sel!(application:openURLs:), imp, c"v@:@@".as_ptr());
    }
}

/// Select the files in a Finder window, as Explorer's /select does. No process is started.
pub fn reveal(files: &[PathBuf]) -> bool {
    let urls: Vec<Retained<NSURL>> = files.iter().map(|f| NSURL::fileURLWithPath(&NSString::from_str(&f.to_string_lossy()))).collect();
    if urls.is_empty() {
        return false;
    }
    let arr = NSArray::from_retained_slice(&urls);
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: () = msg_send![ws, activateFileViewerSelectingURLs: &*arr];
    }
    true
}

fn ns_view(w: &Window) -> Option<*mut AnyObject> {
    let h = w.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = h.as_raw() else { return None };
    Some(h.ns_view.as_ptr() as *mut AnyObject)
}

/// Liquid Glass behind the window's content (macOS 26: NSGlassEffectView), or behind-window
/// vibrancy on older systems (NSVisualEffectView). The glass sits in the window's frame view
/// below the content view, so winit keeps its own view and its events. The content layer is
/// marked so softbuffer keeps alpha; pixels drawn with alpha below 255 then show the glass.
pub fn glass(w: &Window) -> bool {
    let Some(view) = ns_view(w) else { return false };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        let frame_view: *mut AnyObject = msg_send![view, superview];
        if win.is_null() || frame_view.is_null() {
            return false;
        }
        let bounds: NSRect = msg_send![frame_view, bounds];
        let fx: *mut AnyObject = if let Some(c) = AnyClass::get(c"NSGlassEffectView") {
            let a: *mut AnyObject = msg_send![c, alloc];
            msg_send![a, initWithFrame: bounds]
        } else {
            let a: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let v: *mut AnyObject = msg_send![a, initWithFrame: bounds];
            let _: () = msg_send![v, setMaterial: 21isize]; // under-window background
            let _: () = msg_send![v, setBlendingMode: 0isize]; // behind window
            let _: () = msg_send![v, setState: 1isize]; // always active
            v
        };
        if fx.is_null() {
            return false;
        }
        let _: () = msg_send![fx, setAutoresizingMask: 18usize]; // width + height sizable
        let ident = NSString::from_str("arkimage.glass");
        let _: () = msg_send![fx, setIdentifier: &*ident];
        let _: () = msg_send![frame_view, addSubview: fx, positioned: -1isize, relativeTo: view]; // below
        let _: () = msg_send![fx, release];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![win, setOpaque: false];
        let _: () = msg_send![win, setBackgroundColor: clear];
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return false;
        }
        let name = NSString::from_str("arkimage.alpha");
        let _: () = msg_send![layer, setName: &*name];
        let _: () = msg_send![layer, setOpaque: false];
    }
    true
}

/// Frosted glass behind a main window, on or off. On: the glass view from `glass` (made
/// once, then shown again), the content layer keeping alpha. Off: the glass hidden and the
/// window and layer opaque again, so pixels whose alpha byte is not set draw solid.
pub fn window_glass(w: &Window, on: bool) -> bool {
    let Some(view) = ns_view(w) else { return false };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        let frame_view: *mut AnyObject = msg_send![view, superview];
        if win.is_null() || frame_view.is_null() {
            return false;
        }
        let subs: *mut AnyObject = msg_send![frame_view, subviews];
        let n: usize = msg_send![subs, count];
        let mut fx: *mut AnyObject = std::ptr::null_mut();
        for i in 0..n {
            let v: *mut AnyObject = msg_send![subs, objectAtIndex: i];
            let id: *mut AnyObject = msg_send![v, identifier];
            if !id.is_null() {
                let id = &*(id as *const NSString);
                if id.to_string() == "arkimage.glass" {
                    fx = v;
                }
            }
        }
        if on {
            if fx.is_null() {
                return glass(w);
            }
            let _: () = msg_send![fx, setHidden: false];
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![win, setOpaque: false];
            let _: () = msg_send![win, setBackgroundColor: clear];
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let name = NSString::from_str("arkimage.alpha");
                let _: () = msg_send![layer, setName: &*name];
                let _: () = msg_send![layer, setOpaque: false];
            }
            true
        } else {
            if !fx.is_null() {
                let _: () = msg_send![fx, setHidden: true];
            }
            let _: () = msg_send![win, setOpaque: true];
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let none: *mut AnyObject = std::ptr::null_mut();
                let _: () = msg_send![layer, setName: none];
                let _: () = msg_send![layer, setOpaque: true];
            }
            true
        }
    }
}

/// Show or hide the window's close, minimize and zoom buttons (the traffic lights).
pub fn traffic_lights(w: &Window, on: bool) {
    let Some(view) = ns_view(w) else { return };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return;
        }
        for kind in 0usize..3 {
            let b: *mut AnyObject = msg_send![win, standardWindowButton: kind];
            if !b.is_null() {
                let _: () = msg_send![b, setHidden: !on];
            }
        }
    }
}

/// Open `menu` fully above a control at (x, y, w) in logical px from the view's top-left:
/// its bottom edge 4 px over the control, at least as wide as the control, and aligned with
/// its right edge when wider (or its left edge, with `left`), so nothing hangs off the window or scrolls behind an overflow arrow.
/// Blocks until the menu closes.
pub unsafe fn popup_over(menu: *mut std::ffi::c_void, view: *mut std::ffi::c_void, x: f64, y: f64, w: f64, left: bool) {
    let (menu, view) = (menu as *mut AnyObject, view as *mut AnyObject);
    if menu.is_null() || view.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![menu, setMinimumWidth: w];
        let size: objc2_foundation::NSSize = msg_send![menu, size];
        let flipped: bool = msg_send![view, isFlipped];
        let frame: NSRect = msg_send![view, frame];
        // the menu hangs down from the location, so lift it by its own height plus 4 px
        // AppKit draws the top MENU_TOP_INSET above the point asked for
        let top = y - 4.0 - size.height + MENU_TOP_INSET;
        let ly = if flipped { top } else { frame.size.height - top };
        if std::env::var_os("ARKIMAGE_MENU_LOG").is_some() {
            eprintln!("popup_over x={x} y={y} w={w} menu_h={} view_h={}", size.height, frame.size.height);
        }
        let at = objc2_foundation::NSPoint::new(if left { x } else { x + w - size.width.max(w) }, ly);
        let none: *mut AnyObject = std::ptr::null_mut();
        let _: bool = msg_send![menu, popUpMenuPositioningItem: none, atLocation: at, inView: view];
    }
}

/// A dropdown under its field: its own width, right edge on the field's right edge, its top 2 pt below the field's
/// bottom edge (x, y, w: logical px from the view's top-left). AppKit draws the menu's
/// frame a few points above the requested point; the measured offset is added back.
pub unsafe fn menu_below(menu: *mut std::ffi::c_void, view: *mut std::ffi::c_void, x: f64, y: f64, w: f64) {
    const GAP: f64 = 2.0;
    let (menu, view) = (menu as *mut AnyObject, view as *mut AnyObject);
    if menu.is_null() || view.is_null() {
        return;
    }
    unsafe {
        // the menu keeps its own width; its right edge lines up with the field's
        let size: objc2_foundation::NSSize = msg_send![menu, size];
        let left = if w > 0.0 { x + w - size.width } else { x };
        let flipped: bool = msg_send![view, isFlipped];
        let frame: NSRect = msg_send![view, frame];
        let top = y + GAP + MENU_TOP_INSET;
        let ly = if flipped { top } else { frame.size.height - top };
        if std::env::var_os("ARKIMAGE_MENU_LOG").is_some() {
            eprintln!("menu_below field_left={x} field_bottom={y} w={w} menu_w={} flipped={flipped} view_h={}", size.width, frame.size.height);
        }
        let none: *mut AnyObject = std::ptr::null_mut();
        let _: bool = msg_send![menu, popUpMenuPositioningItem: none, atLocation: objc2_foundation::NSPoint::new(left, ly), inView: view];
    }
}

/// How far above the requested point AppKit puts a menu's top edge, in points.
const MENU_TOP_INSET: f64 = 5.0; // measured 2026-11-16: 448 asked 453

/// A context menu at the pointer, placed by AppKit as Finder does: beside the pointer,
/// flipped up or left to stay on screen, with no scroll arrows unless it is taller than
/// the screen. Reads the pointer from the system, not from the app's cursor state.
pub unsafe fn context_menu_at_pointer(menu: *mut std::ffi::c_void, view: *mut std::ffi::c_void) {
    let (menu, view) = (menu as *mut AnyObject, view as *mut AnyObject);
    if menu.is_null() || view.is_null() {
        return;
    }
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return;
        }
        let screen_pt: objc2_foundation::NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let pt: objc2_foundation::NSPoint = msg_send![win, convertPointFromScreen: screen_pt];
        let num: isize = msg_send![win, windowNumber];
        let none: *mut AnyObject = std::ptr::null_mut();
        // NSEventTypeRightMouseDown = 3
        let ev: *mut AnyObject = msg_send![class!(NSEvent),
            mouseEventWithType: 3usize, location: pt, modifierFlags: 0usize, timestamp: 0.0f64,
            windowNumber: num, context: none, eventNumber: 0isize, clickCount: 1isize, pressure: 1.0f32];
        if ev.is_null() {
            return;
        }
        let _: () = msg_send![class!(NSMenu), popUpContextMenu: menu, withEvent: ev, forView: view];
    }
}

/// macOS full screen: the system's title bar slides down at the top edge. Show it as a
/// standard bar there (title and material); outside full screen it stays transparent and
/// untitled under the app's own title strip.
pub fn fullscreen_titlebar(w: &Window, on: bool) {
    let Some(view) = ns_view(w) else { return };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return;
        }
        let _: () = msg_send![win, setTitlebarAppearsTransparent: !on];
        // NSWindowTitleVisible (0), NSWindowTitleHidden (1)
        let _: () = msg_send![win, setTitleVisibility: if on { 0isize } else { 1isize }];
    }
    traffic_lights(w, true);
}

/// Make this app the default for files with these extensions, as Finder's Get Info >
/// Open with > Change All does. Each type first goes through the silent
/// LSSetDefaultRoleHandlerForContentType; only a type still held by another app afterwards
/// (macOS ignores that call for types the user set in Finder) goes through NSWorkspace,
/// which asks the user to confirm, one dialog per type. There is no silent way for those.
/// Several extensions share one type (jpg, jpeg); each type is set once. Returns
/// (types set, types tried), or why nothing could be done. Blocks up to 10 s for the
/// answers, which arrive as callbacks: call it off the main thread (spawn_make_default).
pub fn make_default(exts: &[&str]) -> Result<(usize, usize), String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    #[link(name = "UniformTypeIdentifiers", kind = "framework")]
    extern "C" {}
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn LSSetDefaultRoleHandlerForContentType(uti: *const std::ffi::c_void, role: u32, bundle: *const std::ffi::c_void) -> i32;
    }
    fn id_ns(s: &str) -> Retained<NSString> {
        NSString::from_str(s)
    }
    let app: Option<Retained<NSURL>> = unsafe {
        let b: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        let id: Option<Retained<NSString>> = msg_send![b, bundleIdentifier];
        if id.is_none() { None } else { msg_send![b, bundleURL] }
    };
    let Some(app) = app else {
        return Err("Run ArkImage from ArkImage.app to make it the default app.".into());
    };
    let (ok, answered) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut seen = std::collections::HashSet::new();
    let mut tried = 0;
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let Some(utt) = AnyClass::get(c"UTType") else { return Err("This macOS has no UTType.".into()) };
        for e in exts {
            let ext = NSString::from_str(e);
            let t: *mut AnyObject = msg_send![utt, typeWithFilenameExtension: &*ext];
            if t.is_null() {
                continue;
            }
            let id: Retained<NSString> = msg_send![t, identifier];
            let id = id.to_string();
            if id.starts_with("dyn.") || !seen.insert(id.clone()) {
                continue;
            }
            tried += 1;
            // the silent route first; it holds for every type the user never set by hand
            let bid: Retained<NSString> = {
                let b: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
                msg_send![b, bundleIdentifier]
            };
            LSSetDefaultRoleHandlerForContentType(Retained::as_ptr(&id_ns(&id)) as _, 0xFFFF_FFFF, Retained::as_ptr(&bid) as _);
            let cur: *mut AnyObject = msg_send![ws, URLForApplicationToOpenContentType: t];
            let held = !cur.is_null() && {
                let cb: *mut AnyObject = msg_send![class!(NSBundle), bundleWithURL: cur];
                let cid: Option<Retained<NSString>> = if cb.is_null() { None } else { msg_send![cb, bundleIdentifier] };
                cid.is_some_and(|c| c.to_string() == bid.to_string())
            };
            if held {
                ok.fetch_add(1, Ordering::SeqCst);
                answered.fetch_add(1, Ordering::SeqCst);
                crate::trace(&format!("default app for {e} ({id}): ok, silently"));
                continue;
            }
            let (ok, answered, e) = (ok.clone(), answered.clone(), e.to_string());
            let done = block2::RcBlock::new(move |err: *mut AnyObject| {
                if err.is_null() {
                    ok.fetch_add(1, Ordering::SeqCst);
                }
                crate::trace(&format!("default app for {e} ({id}): {}", if err.is_null() { "ok" } else { "refused" }));
                answered.fetch_add(1, Ordering::SeqCst);
            });
            let _: () = msg_send![ws, setDefaultApplicationAtURL: &*app, toOpenContentType: t, completionHandler: &*done];
        }
        let t0 = std::time::Instant::now();
        while answered.load(Ordering::SeqCst) < tried && t0.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    Ok((ok.load(Ordering::SeqCst), tried))
}

/// Run make_default on a worker thread and send the message to show as a UserEvent.
pub fn spawn_make_default(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let exts: Vec<&str> = crate::source::IMAGE_EXTS.iter().filter(|e| !crate::source::NOT_ASSOCIATED.contains(e)).copied().collect();
        let r = make_default(&exts);
        crate::trace(&format!("make default: {r:?}"));
        let msg = match r {
            Ok((done, all)) if done == all => format!("ArkImage now opens all {all} supported image types by default."),
            Ok((done, all)) => format!("ArkImage now opens {done} of {all} supported image types by default. macOS kept the others with their current apps."),
            Err(e) => e,
        };
        let _ = proxy.send_event(UserEvent::MadeDefault(msg));
    });
}

/// The window is in native full screen (NSWindowStyleMaskFullScreen, 1 << 14).
pub fn is_fullscreen(w: &Window) -> bool {
    let Some(view) = ns_view(w) else { return false };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return false;
        }
        let mask: usize = msg_send![win, styleMask];
        mask & (1 << 14) != 0
    }
}

/// NSWindow isZoomed, read without changing the style mask (see app::is_maximized).
/// A window without a title bar is never zoomed.
pub fn is_zoomed(w: &Window) -> bool {
    let Some(view) = ns_view(w) else { return false };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        if win.is_null() {
            return false;
        }
        let mask: usize = msg_send![win, styleMask];
        mask & 1 != 0 && msg_send![win, isZoomed]
    }
}

/// Test hook: click the green title-bar button as a person would.
pub fn press_green(w: &Window) {
    let Some(view) = ns_view(w) else { return };
    unsafe {
        let win: *mut AnyObject = msg_send![view, window];
        let zoom: *mut AnyObject = if win.is_null() { std::ptr::null_mut() } else { msg_send![win, standardWindowButton: 2usize] };
        if !zoom.is_null() {
            let _: () = msg_send![zoom, performClick: std::ptr::null::<AnyObject>()];
        }
    }
}

/// Started from a mounted disk image or from App Translocation (a downloaded app not yet
/// moved), the bundle is read-only: the updater cannot replace it, and Make Default points
/// at a copy that goes away. Offer to copy it to /Applications (or ~/Applications), start
/// the copy and quit. Asked once per copy. Returns true when this process should exit.
pub fn offer_move_to_applications(arg: Option<&std::path::Path>) -> bool {
    use std::process::Command;
    let Some(app) = crate::update::bundle_path() else { return false };
    let s = app.to_string_lossy().to_string();
    if !(s.starts_with("/Volumes/") || s.contains("/AppTranslocation/")) {
        return false;
    }
    // a translocated path changes at every launch; the disk image's name does not
    let key = if s.contains("/AppTranslocation/") { format!("translocated {}", env!("CARGO_PKG_VERSION")) } else { s.clone() };
    let note = crate::config::Config::dir().join("move-declined.txt");
    let declined = std::fs::read_to_string(&note).unwrap_or_default();
    if declined.lines().any(|l| l == key) {
        return false;
    }
    // test hook: ARKIMAGE_TEST_MOVE=<folder> answers Yes and moves into that folder
    let test_dir = std::env::var_os("ARKIMAGE_TEST_MOVE").map(std::path::PathBuf::from);
    let ask = test_dir.is_some()
        || alert_choice(
            "Move ArkImage to the Applications folder?",
            "It is running from a disk image or a download folder, where it cannot update itself.",
            "Move to Applications",
            "Not Now",
        );
    if !ask {
        let _ = std::fs::create_dir_all(note.parent().unwrap_or(std::path::Path::new("/tmp")));
        let _ = std::fs::write(&note, format!("{declined}{key}\n"));
        return false;
    }
    let writable = |d: &std::path::Path| {
        let t = d.join(".arkimage-write-test");
        let ok = std::fs::write(&t, b"").is_ok();
        let _ = std::fs::remove_file(&t);
        ok
    };
    let home_apps = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Applications"));
    let dir = if let Some(d) = test_dir {
        d
    } else if writable(std::path::Path::new("/Applications")) {
        std::path::PathBuf::from("/Applications")
    } else if let Some(h) = home_apps {
        let _ = std::fs::create_dir_all(&h);
        h
    } else {
        return false;
    };
    let dest = dir.join("ArkImage.app");
    let tmp = dir.join(".ArkImage-move.app");
    let _ = std::fs::remove_dir_all(&tmp);
    let copied = Command::new("/usr/bin/ditto").arg(&app).arg(&tmp).status().is_ok_and(|s| s.success());
    if !copied {
        let _ = std::fs::remove_dir_all(&tmp);
        crate::dialog::tell(None, "Move to Applications", &format!("Could not copy ArkImage to {}.", dir.display()), crate::dialog::Kind::Error);
        return false;
    }
    // an older copy there is replaced
    let old = dir.join(".ArkImage.app.old");
    let _ = std::fs::remove_dir_all(&old);
    if dest.exists() && std::fs::rename(&dest, &old).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        crate::dialog::tell(None, "Move to Applications", "Quit the ArkImage that runs from Applications, then try again.", crate::dialog::Kind::Error);
        return false;
    }
    if std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::rename(&old, &dest);
        return false;
    }
    let _ = std::fs::remove_dir_all(&old);
    crate::trace(&format!("moved to {}", dest.display()));
    let mut open = Command::new("/usr/bin/open");
    open.arg("-n").arg(&dest);
    if let Some(a) = arg {
        open.arg("--args").arg(a);
    }
    open.spawn().is_ok()
}

/// A modal alert before the event loop runs, with named buttons. True for the first one.
fn alert_choice(title: &str, text: &str, yes: &str, no: &str) -> bool {
    use objc2_foundation::NSString;
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyRegular (0)
        let _: bool = msg_send![app, setActivationPolicy: 0isize];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
        let a: *mut AnyObject = msg_send![class!(NSAlert), new];
        let _: () = msg_send![a, setMessageText: &*NSString::from_str(title)];
        let _: () = msg_send![a, setInformativeText: &*NSString::from_str(text)];
        let _: *mut AnyObject = msg_send![a, addButtonWithTitle: &*NSString::from_str(yes)];
        let _: *mut AnyObject = msg_send![a, addButtonWithTitle: &*NSString::from_str(no)];
        // NSAlertFirstButtonReturn (1000)
        let r: isize = msg_send![a, runModal];
        crate::trace(&format!("move alert returned {r}"));
        r == 1000
    }
}
