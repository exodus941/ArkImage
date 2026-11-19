//! Window, input, navigation and drawing.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use image::RgbaImage;
use winit::{
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Fullscreen, ResizeDirection, Window, WindowId, WindowLevel},
};

use crate::chrome;
use crate::settings;
use crate::picker;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    config::Config,
    decode::{self, Decoded},
    render::{self, Canvas},
    source::{self, Entry, Playlist},
    text::Text,
};

/// Copy `old` (a full-canvas frame) into the canvas shifted `dx` px sideways; rows only,
/// one memcpy each, so a full 3440x1440 frame moves without per-pixel work.
fn shift_rows(c: &mut Canvas, old: &[u32], dx: i64) {
    let w = c.w as i64;
    if old.len() != c.buf.len() || dx.abs() >= w {
        return;
    }
    let n = (w - dx.abs()) as usize;
    let (src, dst) = if dx >= 0 { (0usize, dx as usize) } else { ((-dx) as usize, 0usize) };
    for y in 0..c.h {
        let row = y * c.w;
        c.buf[row + dst..row + dst + n].copy_from_slice(&old[row + src..row + src + n]);
    }
}

/// A slide between images: the last settled frame moves out while the new image moves in,
/// purely sideways. `start` is set when the new image is first ready to draw.
struct Slide {
    old: Vec<u32>,
    w: usize,
    h: usize,
    /// +1: the new image enters from the right (forward); -1: from the left.
    dir: i32,
    start: Option<Instant>,
    /// The old image's rows: the motion blur changes only those and the new image's.
    oy0: usize,
    oy1: usize,
    /// When the newest page took the incoming place of this running slide: its motion blur
    /// counts from then (else a fast scroll blurred only the first page).
    entered: Option<Instant>,
}



pub enum MenuEntry {
    Item(String, String, &'static str, bool),
    Check(String, String, &'static str, bool),
    Sep,
    Sub(String, Vec<MenuEntry>),
}

/// Hide a shown window from the screen without hiding it from the system (DWM cloak),
/// so it can paint its first frame unseen.
#[cfg(windows)]
fn cloak(w: &Window, on: bool) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(h: isize, attr: u32, val: *const std::ffi::c_void, size: u32) -> i32;
    }
    let Ok(h) = w.window_handle() else { return };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return };
    let v: i32 = on as i32;
    // DWMWA_CLOAK = 13
    unsafe { DwmSetWindowAttribute(h.hwnd.get(), 13, &v as *const i32 as _, 4) };
}

#[cfg(not(windows))]
fn cloak(_w: &Window, _on: bool) {}

/// Dark native title bar (Windows 10 2004+: DWMWA_USE_IMMERSIVE_DARK_MODE = 20).
#[cfg(windows)]
fn dark_title_bar(w: &Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(h: isize, attr: u32, val: *const std::ffi::c_void, size: u32) -> i32;
    }
    let Ok(h) = w.window_handle() else { return };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return };
    let on: i32 = 1;
    unsafe { DwmSetWindowAttribute(h.hwnd.get(), 20, &on as *const i32 as _, 4) };
}

#[cfg(not(windows))]
fn dark_title_bar(w: &Window) {
    w.set_theme(Some(winit::window::Theme::Dark));
}

/// Mica backdrop (Windows 11 22H2+): DWMWA_SYSTEMBACKDROP_TYPE = 38, DWMSBT_MAINWINDOW = 2,
/// with the frame extended over the whole client area so it shows through pixels drawn
/// with alpha below 255. Returns false where the system has no Mica (the window then
/// keeps its solid fill).
#[cfg(windows)]
fn mica(w: &Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[repr(C)]
    struct Margins(i32, i32, i32, i32);
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(h: isize, attr: u32, val: *const std::ffi::c_void, size: u32) -> i32;
        fn DwmExtendFrameIntoClientArea(h: isize, m: *const Margins) -> i32;
    }
    let Ok(h) = w.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return false };
    let hwnd = h.hwnd.get();
    let kind: i32 = 2;
    if unsafe { DwmSetWindowAttribute(hwnd, 38, &kind as *const i32 as _, 4) } < 0 {
        return false;
    }
    unsafe { DwmExtendFrameIntoClientArea(hwnd, &Margins(-1, -1, -1, -1)) >= 0 }
}

#[cfg(target_os = "macos")]
fn mica(w: &Window) -> bool {
    crate::mac::glass(w)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn mica(_w: &Window) -> bool {
    false
}

/// Windows 11's acrylic backdrop (DWMWA_SYSTEMBACKDROP_TYPE = 38, DWMSBT_TRANSIENTWINDOW
/// = 3) over the frame extended into the whole client area. The accent below lets the
/// sharp desktop through where a maximized window meets the screen edge: 4 px stripes
/// behind kept a contrast of 21 grey levels at the edge, fading to 2 over 80 px; this
/// backdrop measured 0. The extended frame draws the system caption buttons unless the
/// window drops WS_SYSMENU, so it does while the backdrop is on. False where the system
/// has no backdrop (Windows 10): the accent is used then.
#[cfg(windows)]
fn backdrop(hwnd: isize, on: bool) -> bool {
    #[repr(C)]
    struct Margins(i32, i32, i32, i32);
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(h: isize, attr: u32, val: *const std::ffi::c_void, size: u32) -> i32;
        fn DwmExtendFrameIntoClientArea(h: isize, m: *const Margins) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetWindowLongPtrW(h: isize, i: i32) -> isize;
        fn SetWindowLongPtrW(h: isize, i: i32, v: isize) -> isize;
    }
    const WS_SYSMENU: isize = 0x0008_0000;
    let kind: i32 = if on { 3 } else { 1 };
    if unsafe { DwmSetWindowAttribute(hwnd, 38, &kind as *const i32 as _, 4) } < 0 {
        return false;
    }
    if on {
        keep_active(hwnd);
    }
    unsafe {
        let s = GetWindowLongPtrW(hwnd, -16);
        SetWindowLongPtrW(hwnd, -16, if on { s & !WS_SYSMENU } else { s | WS_SYSMENU });
        let m = if on { Margins(-1, -1, -1, -1) } else { Margins(0, 0, 0, 0) };
        DwmExtendFrameIntoClientArea(hwnd, &m);
    }
    true
}

/// The backdrop turns into a flat lighter grey when the window loses focus. DWM keys
/// that on the frame's active state, so WM_NCACTIVATE always passes "active" on. Our own
/// title bar draws focus itself; winit takes focus from WM_SETFOCUS and WM_KILLFOCUS.
#[cfg(windows)]
fn keep_active(hwnd: isize) {
    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(h: isize, f: unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize, id: usize, data: usize) -> i32;
        fn DefSubclassProc(h: isize, m: u32, w: usize, l: isize) -> isize;
    }
    unsafe extern "system" fn proc(h: isize, m: u32, w: usize, l: isize, _id: usize, _d: usize) -> isize {
        const WM_NCACTIVATE: u32 = 0x0086;
        if m == WM_NCACTIVATE {
            return unsafe { DefSubclassProc(h, m, 1, l) };
        }
        unsafe { DefSubclassProc(h, m, w, l) }
    }
    // the same id again only updates the existing subclass
    unsafe { SetWindowSubclass(hwnd, proc, 0x41524B, 0) };
}

/// Frosted glass behind a lone window: the acrylic blur-behind accent
/// (SetWindowCompositionAttribute, WCA_ACCENT_POLICY = 19, ACCENT_ENABLE_ACRYLICBLURBEHIND
/// = 4). Live content behind the window shows blurred through pixels drawn with alpha
/// below 255. The frame is not extended: that drew the system caption buttons on the
/// borderless window. `on` false sets ACCENT_DISABLED. Returns false if the call fails.
#[cfg(windows)]
fn acrylic(w: &Window, on: bool, bg: u32) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[repr(C)]
    struct Accent {
        state: u32,
        flags: u32,
        /// AABBGGRR tint over the blur
        color: u32,
        anim: u32,
    }
    #[repr(C)]
    struct Data {
        attr: u32,
        data: *const Accent,
        size: usize,
    }
    // not in the import library: look it up in user32 at run time
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(n: *const u16) -> isize;
        fn GetProcAddress(m: isize, n: *const u8) -> usize;
    }
    let name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
    let f = unsafe { GetProcAddress(GetModuleHandleW(name.as_ptr()), b"SetWindowCompositionAttribute\0".as_ptr()) };
    if f == 0 {
        return false;
    }
    let set: unsafe extern "system" fn(isize, *const Data) -> i32 = unsafe { std::mem::transmute(f) };
    let Ok(h) = w.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return false };
    if backdrop(h.hwnd.get(), on) {
        return on;
    }
    // flags 0: no system tint. Its tint darkened far past its alpha (a 33% grey tint
    // gave #1A310C over lime); the 33% grey is drawn in our own pixels instead.
    let _ = bg;
    let acc = Accent { state: if on { 4 } else { 0 }, flags: 0, color: 0, anim: 0 };
    let d = Data { attr: 19, data: &acc, size: std::mem::size_of::<Accent>() };
    unsafe { set(h.hwnd.get(), &d) != 0 }
}

#[cfg(target_os = "macos")]
fn acrylic(w: &Window, on: bool, _bg: u32) -> bool {
    // on: true when the glass is now behind the window; off: always reports false (unloaded)
    crate::mac::window_glass(w, on) && on
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn acrylic(_w: &Window, _on: bool, _bg: u32) -> bool {
    false
}

/// Background pixels while the glass is loaded: the background colour with a marker in
/// the alpha byte. Everything drawn over them writes alpha 0, so after the draw the
/// pixels still marked are background, and only those turn see-through.
const GLASS_MARK: u32 = 0x0100_0000;
/// The background grey over the blur at this alpha: 33%, the user's value (black at 38%
/// read as too dark and grey).
const GLASS_OPACITY: u32 = 84;
const GLASS_FADE: Duration = Duration::from_millis(250);



/// Whether the window is maximized (zoomed). On macOS this asks AppKit directly: winit's
/// is_maximized() adds a title bar for a moment to a window without one (in full screen,
/// for instance), which moved the window and sent Moved events in a loop, 64 px up and down.
fn is_maximized(w: &Window) -> bool {
    #[cfg(target_os = "macos")]
    return crate::mac::is_zoomed(w);
    #[cfg(not(target_os = "macos"))]
    w.is_maximized()
}

pub(crate) fn menu_label(text: &str, key: &str) -> String {
    // Windows right-aligns text after a tab; macOS would print it literally.
    if cfg!(windows) && !key.is_empty() {
        format!("{text}\t{key}")
    } else {
        text.to_string()
    }
}

fn build_menu(items: Vec<MenuEntry>) -> Vec<Box<dyn muda::IsMenuItem>> {
    items
        .into_iter()
        .map(|e| -> Box<dyn muda::IsMenuItem> {
            match e {
                MenuEntry::Item(id, t, k, en) => Box::new(muda::MenuItem::with_id(id, menu_label(&t, k), en, None)),
                MenuEntry::Check(id, t, k, on) => Box::new(muda::CheckMenuItem::with_id(id, menu_label(&t, k), true, on, None)),
                MenuEntry::Sep => Box::new(muda::PredefinedMenuItem::separator()),
                MenuEntry::Sub(t, v) => {
                    let s = muda::Submenu::new(t, true);
                    for b in build_menu(v) {
                        let _ = s.append(b.as_ref());
                    }
                    Box::new(s)
                }
            }
        })
        .collect()
}

pub enum UserEvent {
    Loaded(Entry, Result<Arc<Decoded>, String>),
    Skipped(Entry),
    Font(Option<Text>),
    /// Folder listing for a file opened alone: (the file, the full list).
    Listed(Entry, Result<Playlist, String>),
    /// A finished thumbnail for the Select Image grid (None: could not decode).
    Thumb(Entry, Option<Arc<RgbaImage>>),
    /// Another launch asked the running master to open a window (with this file, if any).
    Open(Option<PathBuf>),
    /// macOS: "Make Default" finished; the message to show.
    MadeDefault(String),
    /// An update check finished; true when the user asked for it (Settings).
    Update(crate::update::Outcome, bool),
    /// The folder poll replaced the list of at least one open folder.
    Polled,
}

/// Two-finger swipe: logical px of sideways travel for one image, the quiet time that ends
/// a gesture, px per line for wheels that report lines, and the sign that makes a drag to
/// the right mean "next" (flip it if a platform reports the other way).
const SWIPE_PX: f64 = 64.0;
const SWIPE_GAP: Duration = Duration::from_millis(300);
/// The full-quality redraw follows the last pinch event after this long.
const PINCH_SETTLE: Duration = Duration::from_millis(150);
/// A stored image with no paint after this long gets its repaint asked for again.
const REPAINT_RETRY: Duration = Duration::from_millis(250);
const SWIPE_LINE_PX: f64 = 40.0;
const SWIPE_SIGN: f64 = 1.0;

/// Chrome fade in/out duration.
const FADE_SECS: f32 = 0.25;
/// How often a window checks that its file was not deleted outside the app.
const GONE_EVERY: Duration = Duration::from_millis(2500);

/// The next tick of a clock shared by every window, `every` apart. Periodic checks wait for
/// it, so all windows wake together: each on its own timer, 100 windows woke the loop about
/// 200 times a second at idle, and each wake ran every window's timers.
/// Whether another window covers `w` now (macOS: mac::is_occluded; Windows: occl).
#[cfg(target_os = "macos")]
fn covered_now(w: &Arc<Window>) -> bool {
    crate::mac::is_occluded(w)
}

#[cfg(windows)]
fn covered_now(w: &Arc<Window>) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(h) = w.window_handle() else { return false };
    let RawWindowHandle::Win32(h) = h.as_raw() else { return false };
    crate::occl::is_covered(h.hwnd.get())
}

fn shared_tick(now: Instant, every: Duration) -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let e = *EPOCH.get_or_init(Instant::now);
    let p = every.as_nanos().max(1);
    let n = now.saturating_duration_since(e).as_nanos() / p + 1;
    e + Duration::from_nanos((n * p) as u64)
}

/// Zoom In/Out stops, as Honeyview 5.53 measured (drawn width of a test square):
/// 5%, then 10-300% by 10, 320-500% by 20, 550-2000% by 50.
static ZOOM_STEPS: std::sync::LazyLock<Vec<f64>> = std::sync::LazyLock::new(|| {
    let mut v = vec![5];
    v.extend((10..=300).step_by(10));
    v.extend((320..=500).step_by(20));
    v.extend((550..=2000).step_by(50));
    v.into_iter().map(|p| p as f64 / 100.0).collect()
});

/// Hotkey help: (keys, action).
const HELP: &[(&str, &str)] = &[
    ("Right  Left  PgDn  PgUp  Space  Bksp", "Next / previous image"),
    ("Home  End", "First / last image"),
    ("Ctrl+PgDn  Ctrl+PgUp", "10 images forward / back"),
    ("Ctrl+Alt+PgDn  Ctrl+Alt+PgUp", "Random image"),
    ("]  [", "Next / previous folder or archive"),
    ("0  9  8", "Original size / fit window / fit width"),
    ("Z", "Stretch small images"),
    ("+  -  *", "Zoom in / out / 100%"),
    ("Ctrl++  Ctrl+-", "Zoom 1%"),
    ("Ctrl+K", "Fix zoom"),
    ("Alt+Arrows", "Rotate CW / CCW / 180 / reset"),
    ("Ctrl+F  Ctrl+M", "Flip vertical / mirror"),
    ("Ctrl+I  Ctrl+G", "Negative / gamma"),
    ("U  I  S", "No filter / soften / soften+sharpen"),
    ("Tab  Shift+Tab", "Info / file name"),
    ("Ctrl+Shift+C", "Copy info"),
    ("F2  Ctrl+O  F  F4", "Open file / open folder / close"),
    ("Ctrl+Enter", "Show in folder"),
    ("Del", "Delete"),
    ("Ins  Shift+Ins  Ctrl+Ins", "Copy to photo folder 1 / 2, move to 1"),
    ("Ctrl+C  Ctrl+E", "Copy image / open in editor"),
    ("Alt+Enter  F11  Middle click", "Full screen"),
    ("Ctrl+A", "Always on top"),
    ("F5  Shift+F5", "Settings / reload settings file"),
    ("`  ~", "Lock top menu / lock control bar"),
    ("F1  X  Esc", "This help / exit"),
];

#[derive(Clone, Copy)]
struct Layout {
    z: f64,
    ox: f64,
    oy: f64,
    sw: f64,
    sh: f64,
    ww: f64,
    wh: f64,
}

#[derive(Default)]
struct View {
    rotation: u8,
    flip_h: bool,
    flip_v: bool,
    negative: bool,
    gamma: bool,
    user_zoom: Option<f64>,
    zoom_fixed: bool,
    pan_x: f64,
    pan_y: f64,
    pan_reset: bool,
}

type ScaledKey = (u64, u32, u32, u64);

/// Where a new window goes.
pub enum Place {
    /// The geometry saved at the last close (first window of a launch).
    Saved,
    /// 32 px down and right of this position (another window is open).
    Cascade(i32, i32),
    /// Exactly as recorded before a crash.
    Exact(crate::backup::WinState),
}

/// State every window shares, owned by the master: one decoded-image cache, one set of
/// decode and thumbnail threads, one font. In-flight and wanted sets cover all windows,
/// so two windows on the same folder never decode or hold the same image twice.
#[derive(Clone)]
pub struct Shared {
    pub proxy: EventLoopProxy<UserEvent>,
    pub job_tx: Arc<Jobs>,
    /// Images each window wants, by window id; a queued decode no window wants is skipped.
    pub wanted: Arc<Mutex<HashMap<u64, HashSet<Entry>>>>,
    pub exif_flag: Arc<AtomicBool>,
    pub thumb_jobs: Arc<picker::Jobs>,
    pub cache: Rc<RefCell<HashMap<Entry, Arc<Decoded>>>>,
    pub failed: Rc<RefCell<HashMap<Entry, String>>>,
    pub inflight: Rc<RefCell<HashSet<Entry>>>,
    pub text: Rc<RefCell<Option<Text>>>,
}

impl Shared {
    pub fn new(proxy: EventLoopProxy<UserEvent>, cfg: &Config) -> Self {
        // the first image decodes before any window has a GPU surface: upload from the
        // start when the setting is on (a GPU that then fails turns this off again)
        // Windows: off until a window takes the GPU. The first window is alone and draws on
        // the processor (the glass); its image went to the GPU and showed as the 512 px
        // thumbnail.
        let gpu_on = cfg.gpu && std::env::var("ARKIMAGE_GPU").as_deref() != Ok("0") && !cfg!(windows);
        crate::gpu::DECODE_TO_GPU.store(gpu_on, Ordering::Relaxed);
        let wanted = Arc::new(Mutex::new(HashMap::new()));
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 3);
        let exif_flag = Arc::new(AtomicBool::new(cfg.exif_rotate));
        let job_tx = spawn_workers(workers, proxy.clone(), wanted.clone(), exif_flag.clone());
        let thumb_jobs = picker::spawn_workers(2, proxy.clone(), exif_flag.clone(), cfg.thumb_cache_mb > 0);
        if cfg.thumb_cache_mb > 0 {
            let cap = cfg.thumb_cache_mb;
            std::thread::spawn(move || crate::thumbcache::trim(cap));
        }
        // Font parsing takes ~20 ms; keep it off the path to the first image.
        let fp = proxy.clone();
        std::thread::spawn(move || {
            let t = Text::load();
            crate::trace("font loaded");
            let _ = fp.send_event(UserEvent::Font(t));
        });
        Self {
            proxy,
            job_tx,
            wanted,
            exif_flag,
            thumb_jobs,
            cache: Rc::new(RefCell::new(HashMap::new())),
            failed: Rc::new(RefCell::new(HashMap::new())),
            inflight: Rc::new(RefCell::new(HashSet::new())),
            text: Rc::new(RefCell::new(None)),
        }
    }

    /// Queue a decode unless it is cached, failed or already queued.
    pub fn request(&self, e: Entry) {
        if self.cache.borrow().contains_key(&e) || self.inflight.borrow().contains(&e) || self.failed.borrow().contains_key(&e) {
            return;
        }
        self.inflight.borrow_mut().insert(e.clone());
        let _ = self.job_tx.send(e, false);
    }

    pub fn is_wanted(&self, e: &Entry) -> bool {
        self.wanted.lock().unwrap().values().any(|s| s.contains(e))
    }
}

pub struct App {
    /// Window number inside the master; keys this window's wanted set.
    pub id: u64,
    /// Set when the window closes; the master then drops it.
    pub closed: bool,
    /// Set by the menu; the master selects every open image in this folder.
    pub reveal_all: bool,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    _context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// The GPU image layer; None draws everything on the processor (no GPU, the setting
    /// off, or the GPU lost).
    gpu: Option<crate::gpu::GpuWin>,
    /// The interface frame the GPU path draws into (kept between frames).
    gpu_buf: Vec<u32>,
    /// Something other than an animation frame asked for a redraw: the whole interface is
    /// drawn again. False: only the frame advanced, and the animation row is redrawn alone
    /// when `anim_band` allows it.
    ui_dirty: bool,
    /// The animation row's rows (y0..y1), checked after the last full draw to come out the
    /// same when drawn on its own (no card, toast or fade over it); (0, 0) when no row is
    /// on screen; None otherwise.
    anim_band: Option<(usize, usize)>,
    /// The GPU setting this window last acted on (a change switches the path).
    gpu_cfg: bool,
    /// macOS reports the window covered (or minimized): nothing of it is on screen.
    occluded: bool,
    /// GPU path: this window gave its full texture back (covered, or past the budget) and
    /// shows its screen-sized copy, or the thumbnail; it decodes again when used.
    stripped: bool,
    /// When the covered check last ran (macOS polls it).
    occ_checked: Instant,
    /// This frame's image placement for the GPU: (ox, oy, z, smooth), slide shift of the
    /// old layer, and whether the frame is a settled one.
    gpu_img: Option<(f64, f64, f64, bool)>,
    gpu_hq: Option<crate::gpu::Hq>,
    gpu_slide: Option<f64>,
    /// The decoded image the GPU base is (its address), so a transform does not re-upload.
    gpu_src: u64,
    /// The decoder's shared texture of the base (the base itself is then a thumbnail).
    base_tex: Option<Arc<crate::gpu::Tex>>,
    gpu_settle: bool,
    /// This frame's slide motion blur (half-length of the smear, px).
    gpu_motion: f32,
    pub cfg: Config,
    pending_open: Option<PathBuf>,
    playlist: Option<Playlist>,
    /// macOS: whether the traffic-light buttons are shown (None: not set yet).
    #[cfg(target_os = "macos")]
    lights_on: Option<bool>,
    pos_map: Arc<source::PosMap>,
    cache: Rc<RefCell<HashMap<Entry, Arc<Decoded>>>>,
    failed: Rc<RefCell<HashMap<Entry, String>>>,
    inflight: Rc<RefCell<HashSet<Entry>>>,
    wanted: Arc<Mutex<HashMap<u64, HashSet<Entry>>>>,
    job_tx: Arc<Jobs>,
    current: Option<(Entry, Arc<Decoded>)>,
    frame_idx: usize,
    /// Two-finger swipe state: sideways travel so far, whether this gesture already stepped,
    /// and the time of its last event.
    swipe_acc: f64,
    swipe_done: bool,
    swipe_at: Option<Instant>,
    /// Animation playback speed, 0.25 to 4 in 0.25 steps (the speed pill); per window.
    anim_speed: f32,
    next_frame_at: Option<Instant>,
    view: View,
    base: Option<Arc<RgbaImage>>,
    base_gen: u64,
    scaled: Option<(ScaledKey, RgbaImage)>,
    scaled_hq: bool,
    layout: Option<Layout>,
    text: Rc<RefCell<Option<Text>>>,
    toast: Option<(String, Instant)>,
    show_info: bool,
    show_name: bool,
    show_help: bool,
    /// When the hotkeys card was last shown or hidden, for its 250 ms fade.
    help_at: Instant,
    /// When the "Drop an image" card appeared, for its fade-in.
    empty_at: Option<Instant>,
    mods: ModifiersState,
    cursor: (f64, f64),
    drag: Option<((f64, f64), (f64, f64), bool)>,
    /// The left drag draws a zoom rectangle instead of panning (ACDSee style): always
    /// with the right button held, and without it when the image fits the window.
    drag_rect: bool,
    /// Zoom rectangle being drawn: start and current corner, window px.
    zoom_rect: Option<((f64, f64), (f64, f64))>,
    /// Right button down, and whether a zoom rectangle was drawn while it was down
    /// (then its release opens no menu).
    right_down: bool,
    right_used: bool,
    /// A modal card over the image (install question and its outcome), its button
    /// rectangles from the last frame, and the hovered button.
    modal: Option<crate::install::Card>,
    modal_hits: Vec<(f32, f32, f32, f32)>,
    modal_hover: Option<usize>,
    /// When the modal card appeared, for its 250 ms fade-in.
    modal_at: Option<Instant>,
    last_click: Option<(Instant, (f64, f64))>,
    wheel_acc: f64,
    /// Last pinch event: while set, the image is drawn by sampling only the visible pixels,
    /// and the high-quality resize runs PINCH_SETTLE after the last event (or when a macOS
    /// pinch ends). (Added 2026-11-16 by Claude on the Mac, for both; see the handoff.)
    pinch_at: Option<Instant>,
    /// Set when a new image is stored for display, cleared by the next finished paint. If no
    /// paint follows within REPAINT_RETRY, the idle loop asks again: a repaint request can be
    /// lost (Windows drops one made while that window paints; seen after a 100-window
    /// crash takeover, where windows stayed grey until the cursor entered).
    unpainted: Option<Instant>,
    /// The image layer of the last settled frame (before the chrome), and what it showed.
    last_frame: Vec<u32>,
    last_key: Option<(u64, u64, u64, u64, usize, usize)>,
    slide: Option<Slide>,
    /// Direction of the step that caused the next go_to (wrap-around keeps it right).
    nav_hint: Option<i32>,
    /// The archive whose pages are reversed (right-to-left reading), if any.
    rtl_for: Option<PathBuf>,
    /// When the last slide frame was drawn; the next one is due 8 ms later.
    slide_drawn: Instant,
    /// Frosted glass: wanted (the only window), loaded (backdrop set), and the fade
    /// from `glass_from` (0 glass, 1 solid) toward the wanted state, started at `glass_at`.
    glass_on: bool,
    /// Reopen in full screen at this time (closed in full screen, or a crash restore).
    fs_restore_at: Option<Instant>,
    /// What the master asked for (one window open), before the full-screen exception.
    glass_want: bool,
    /// set_glass has run once: until then the window does not know whether it is alone,
    /// and stays on the processor (taking the GPU for one frame sent the first image there).
    glass_known: bool,
    glass_loaded: bool,
    glass_from: f32,
    glass_at: Instant,
    fullscreen: bool,
    /// macOS: the window's full-screen flag as last seen (see render)
    fs_native: Option<bool>,
    on_top: bool,
    rng: u64,
    rand_hist: Vec<usize>,
    /// Last navigation direction (1 forward, -1 back), for preloading.
    dir: i8,
    last_title: String,
    hits: Vec<chrome::Hit>,
    hover: Option<&'static str>,
    cursor_in: bool,
    slider_drag: bool,
    /// The animation scrub bar is held: playback pauses on the frame under the cursor.
    anim_drag: bool,
    /// Next check that the file on screen still exists.
    gone_at: Instant,
    chrome_shown: (bool, bool),
    /// Opacity of the top and bottom chrome, 0..1, stepped toward the target each frame.
    fade: (f32, f32),
    fade_last: Instant,
    fade_active: bool,
    /// Last restored (not maximized, not full screen) outer position and inner size.
    geom: Option<(i32, i32, u32, u32)>,
    /// Whether the window was maximized, as last seen outside full screen.
    was_max: bool,
    /// Maximized when full screen was entered; restored on leaving it.
    fs_was_max: bool,
    settings: Option<settings::Dialog>,
    /// The choice box whose popup menu is open in the settings dialog.
    pending_choice: Option<settings::ChoiceF>,
    /// When a popup last closed, which control opened it, and whether the cursor moved since.
    popup_closed: Option<(Instant, String, bool)>,
    /// Open the next popup above its control, this many physical px wide; true: the main
    /// menu, left-aligned with the button (Windows: popup_win; macOS: mac::popup_over).
    popup_select: Option<(f64, bool)>,
    /// The next popup is a dropdown of this width (physical px): macOS opens it under the
    /// field, exactly as wide.
    below_w: Option<f64>,
    /// The next popup is a right-click menu: macOS places it at the pointer.
    at_pointer: bool,
    /// A preset's name before renaming started (Esc puts it back).
    rename_orig: Option<String>,
    /// Hover fade per chrome control (0-1), and when it was last stepped.
    hover_fx: Vec<(&'static str, f32)>,
    /// The empty card's action rows and the hotkeys card's close button, as drawn last.
    empty_hits: Vec<chrome::Hit>,
    help_hit: Option<chrome::Hit>,
    /// Tooltip fades of the bottom-bar buttons (see chrome::State::tip_fx).
    tip_fx: Vec<(&'static str, f32)>,
    hover_at: Instant,
    /// A number field's value before typing started (Esc puts it back).
    num_edit_orig: Option<i64>,
    /// The binding whose action menu is open: (keyboard slot?, index).
    pending_bind: Option<(bool, usize)>,
    /// Shared with the decode threads so the EXIF setting applies without a restart.
    exif_flag: Arc<AtomicBool>,
    picker: picker::Picker,
    thumb_jobs: Arc<picker::Jobs>,
}

/// The decoders' queue. Two lanes: the page a focused window turned to goes first, the
/// newest such page first; preloads and every other window's images follow in order.
/// One first-in-first-out queue made a turned page wait behind 60 windows' start-up
/// decodes and preloads.
pub struct Jobs {
    q: Mutex<(std::collections::VecDeque<Entry>, std::collections::VecDeque<Entry>)>,
    cv: std::sync::Condvar,
}

impl Jobs {
    /// Queue `e`. An urgent page replaces the urgent pages still waiting: they were
    /// passed over (after a jump on the scrub bar the decoders worked through pages
    /// already left: 33 of 137 turned pages drew). Returns the entries dropped, which
    /// the caller takes out of its in-flight set.
    pub fn send(&self, e: Entry, urgent: bool) -> Vec<Entry> {
        let mut g = self.q.lock().unwrap();
        let dropped = if urgent {
            let old: Vec<Entry> = g.0.drain(..).collect();
            g.0.push_back(e);
            old
        } else {
            g.1.push_back(e);
            Vec::new()
        };
        drop(g);
        self.cv.notify_one();
        dropped
    }

    fn take(&self) -> Entry {
        let mut g = self.q.lock().unwrap();
        loop {
            if let Some(e) = g.0.pop_back() {
                return e;
            }
            if let Some(e) = g.1.pop_front() {
                return e;
            }
            g = self.cv.wait(g).unwrap();
        }
    }
}

fn spawn_workers(
    n: usize,
    proxy: EventLoopProxy<UserEvent>,
    wanted: Arc<Mutex<HashMap<u64, HashSet<Entry>>>>,
    exif_rotate: Arc<AtomicBool>,
) -> Arc<Jobs> {
    let jobs = Arc::new(Jobs { q: Mutex::new(Default::default()), cv: std::sync::Condvar::new() });
    for _ in 0..n {
        let rx = jobs.clone();
        let proxy = proxy.clone();
        let wanted = wanted.clone();
        let exif_rotate = exif_rotate.clone();
        std::thread::spawn(move || loop {
            let e = rx.take();
            if !wanted.lock().unwrap().values().any(|s| s.contains(&e)) {
                if proxy.send_event(UserEvent::Skipped(e)).is_err() {
                    break;
                }
                continue;
            }
            let to_gpu = crate::gpu::DECODE_TO_GPU.load(Ordering::Relaxed);
            let res = decode::decode(&e, exif_rotate.load(Ordering::Relaxed), to_gpu).map(Arc::new);
            if proxy.send_event(UserEvent::Loaded(e, res)).is_err() {
                break;
            }
        });
    }
    jobs
}

fn fmt_size(b: u64) -> String {
    let b = b as f64;
    if b >= 1048576.0 {
        format!("{:.2} MB", b / 1048576.0)
    } else if b >= 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{b} B")
    }
}

fn fmt_time(t: SystemTime) -> String {
    let s = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = s.div_euclid(86400);
    let rem = s.rem_euclid(86400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

fn open_text_file(p: &Path) {
    #[cfg(windows)]
    let _ = Command::new("notepad").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-t").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(p).spawn();
}

impl App {
    /// A new window viewer on the shared state. `cfg` is the master's current config.
    pub fn new(sh: &Shared, id: u64, cfg: Config, arg: Option<PathBuf>) -> Self {
        let mut app = Self::build(sh, id, cfg, arg);
        // Start decoding before the window exists; window creation takes 30-45 ms.
        if let Some(p) = app.pending_open.take() {
            app.open_path(&p);
        }
        app
    }

    fn build(sh: &Shared, id: u64, cfg: Config, arg: Option<PathBuf>) -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
        Self {
            id,
            closed: false,
            reveal_all: false,
            proxy: sh.proxy.clone(),
            window: None,
            _context: None,
            surface: None,
            gpu: None,
            gpu_buf: Vec::new(),
            ui_dirty: true,
            anim_band: None,
            gpu_cfg: true,
            occluded: false,
            stripped: false,
            occ_checked: Instant::now(),
            gpu_img: None,
            gpu_hq: None,
            gpu_slide: None,
            gpu_src: 0,
            base_tex: None,
            gpu_settle: false,
            gpu_motion: 0.0,
            cfg,
            pending_open: arg,
            playlist: None,
            #[cfg(target_os = "macos")]
            lights_on: None,
            pos_map: Default::default(),
            cache: sh.cache.clone(),
            failed: sh.failed.clone(),
            inflight: sh.inflight.clone(),
            wanted: sh.wanted.clone(),
            job_tx: sh.job_tx.clone(),
            current: None,
            frame_idx: 0,
            swipe_acc: 0.0,
            swipe_done: false,
            swipe_at: None,
            anim_speed: 1.0,
            next_frame_at: None,
            view: View::default(),
            base: None,
            base_gen: 0,
            scaled: None,
            scaled_hq: true,
            layout: None,
            text: sh.text.clone(),
            toast: None,
            show_info: false,
            show_name: false,
            show_help: false,
            help_at: Instant::now() - Duration::from_secs(1),
            empty_at: None,
            mods: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            drag: None,
            drag_rect: false,
            zoom_rect: None,
            right_down: false,
            right_used: false,
            modal: None,
            modal_hits: Vec::new(),
            modal_hover: None,
            modal_at: None,
            last_click: None,
            wheel_acc: 0.0,
            pinch_at: None,
            unpainted: None,
            last_frame: Vec::new(),
            last_key: None,
            slide: None,
            nav_hint: None,
            rtl_for: None,
            slide_drawn: Instant::now(),
            glass_on: false,
            fs_restore_at: None,
            glass_known: false,
            glass_want: false,
            glass_loaded: false,
            glass_from: 1.0,
            glass_at: Instant::now(),
            fullscreen: false,
            fs_native: None,
            on_top: false,
            rng: seed,
            rand_hist: Vec::new(),
            dir: 1,
            last_title: String::new(),
            hits: Vec::new(),
            hover: None,
            cursor_in: false,
            slider_drag: false,
            anim_drag: false,
            gone_at: Instant::now() + GONE_EVERY,
            chrome_shown: (false, false),
            fade: (0.0, 0.0),
            fade_last: Instant::now(),
            fade_active: false,
            geom: None,
            was_max: false,
            fs_was_max: false,
            settings: None,
            pending_choice: None,
            popup_closed: None,
            popup_select: None,
            below_w: None,
            at_pointer: false,
            rename_orig: None,
            hover_fx: Vec::new(),
            empty_hits: Vec::new(),
            help_hit: None,
            tip_fx: Vec::new(),
            hover_at: Instant::now(),
            num_edit_orig: None,
            pending_bind: None,
            exif_flag: sh.exif_flag.clone(),
            picker: picker::Picker::default(),
            thumb_jobs: sh.thumb_jobs.clone(),
        }
    }

    // ---------- small helpers ----------

    fn primary(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.mods.super_key()
        } else {
            self.mods.control_key()
        }
    }

    pub fn request_redraw(&mut self) {
        self.ui_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn msg(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now() + Duration::from_millis(1600)));
        self.request_redraw();
    }

    fn invalidate(&mut self) {
        self.base = None;
        self.scaled = None;
        self.request_redraw();
    }

    fn cur_entry(&self) -> Option<Entry> {
        self.playlist.as_ref().and_then(|p| p.get(p.index).cloned())
    }

    fn displaying_current(&self) -> bool {
        match (&self.current, self.cur_entry()) {
            (Some((e, _)), Some(c)) => *e == c,
            _ => false,
        }
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    // ---------- loading ----------

    /// True for a window showing no file (the one a Finder launch opens before its files arrive).
    pub fn is_blank(&self) -> bool {
        self.playlist.is_none() && self.current.is_none()
    }

    pub fn open_external(&mut self, p: &Path) {
        self.open_path(p);
        self.request_redraw();
    }

    fn open_path(&mut self, p: &Path) {
        // A single image: show it at once and list its folder on a thread.
        // Listing 46,000 files takes ~110 ms; the image should not wait for it.
        if p.is_file() && !source::is_archive(p) {
            if let Ok(abs) = std::path::absolute(p) {
                let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                let first = Playlist { container: dir, entries: Arc::new(vec![Entry::File(abs.clone())]), rev: false, index: 0 };
                self.set_playlist(first);
                let (proxy, sort, desc) = (self.proxy.clone(), self.cfg.sort, self.cfg.sort_desc);
                std::thread::spawn(move || {
                    crate::trace("list start");
                    let r = source::open_fresh(&abs, sort, desc).map_err(|e| e.to_string());
                    crate::trace("listed");
                    let _ = proxy.send_event(UserEvent::Listed(Entry::File(abs), r));
                });
                return;
            }
        }
        crate::trace("list start");
        let r = source::open(p, self.cfg.sort, self.cfg.sort_desc);
        crate::trace(&format!("listed {}", r.as_ref().map(|pl| pl.entries.len()).unwrap_or(0)));
        match r {
            Ok(pl) if pl.entries.is_empty() => self.msg("There is no image in this folder or file."),
            Ok(pl) => self.set_playlist(pl),
            Err(e) => self.msg(format!("Failed to open the file: {e}")),
        }
    }

    /// How solid the background is now: 0 frosted glass, 1 the solid colour.
    /// The background's opacity over the window glass, as `glass_finish` draws it.
    fn glass_bg_alpha(&self) -> f32 {
        let o = GLASS_OPACITY as f32 / 255.0;
        o + (1.0 - o) * self.glass_solid()
    }

    fn glass_solid(&self) -> f32 {
        let to = if self.glass_on { 0.0 } else { 1.0 };
        let k = (self.glass_at.elapsed().as_secs_f32() / GLASS_FADE.as_secs_f32()).min(1.0);
        self.glass_from + (to - self.glass_from) * k
    }

    fn glass_fading(&self) -> bool {
        self.glass_at.elapsed() < GLASS_FADE + Duration::from_millis(20)
    }

    /// True until the window exists on screen.
    pub fn is_new(&self) -> bool {
        self.window.is_none()
    }

    /// The master calls this when the window count changes: glass while this is the only
    /// window. `instant` skips the fade (a window just opened).
    pub fn set_glass(&mut self, on: bool, instant: bool) {
        if !self.glass_known {
            self.glass_known = true;
            self.request_redraw();
        }
        self.glass_want = on;
        // macOS full screen has nothing behind the window to blur
        let on = on && !(cfg!(target_os = "macos") && self.fullscreen);
        if on == self.glass_on {
            return;
        }
        self.glass_from = if instant { if on { 0.0 } else { 1.0 } } else { self.glass_solid() };
        self.glass_on = on;
        self.glass_at = Instant::now();
        self.request_redraw();
    }

    /// Before a draw: load the backdrop when glass is wanted, unload it once the fade to
    /// solid has ended. Returns true while background pixels must be marked.
    fn glass_prepare(&mut self) -> bool {
        let Some(w) = self.window.clone() else { return false };
        if self.glass_on && !self.glass_loaded {
            self.glass_loaded = acrylic(&w, true, self.cfg.bg_color);
            crate::trace(&format!("glass loaded: {}", self.glass_loaded));
        }
        if !self.glass_on && self.glass_loaded && !self.glass_fading() {
            acrylic(&w, false, self.cfg.bg_color);
            self.glass_loaded = false;
            crate::trace("glass unloaded");
            // Windows: the window can take the GPU now; draw again so it does
            self.request_redraw();
        }
        self.glass_loaded
    }

    /// After a draw: background pixels become the glass, faded toward the solid colour by
    /// `glass_solid`; every other pixel is opaque. Alpha is premultiplied.
    fn glass_finish(&self, buf: &mut [u32]) {
        let t = self.glass_solid();
        // the background grey at GLASS_OPACITY over the blur, faded to solid by t
        let o = GLASS_OPACITY as f32 / 255.0;
        let t = o + (1.0 - o) * t;
        let a = 255.0 * t;
        let a = a.round() as u32;
        let bg = self.cfg.bg_color;
        let m = |v: u32| ((v as f32) * t).round() as u32;
        let glass = a << 24 | m(bg >> 16 & 0xFF) << 16 | m(bg >> 8 & 0xFF) << 8 | m(bg & 0xFF);
        for p in buf.iter_mut() {
            *p = if *p >> 24 != 1 {
                0xFF00_0000 | (*p & 0xFF_FFFF)
            } else if *p & 0xFF == 0xFF {
                glass
            } else {
                // a shadow over the glass: black at strength s, with 1 - s in the blue
                // byte (see render::GLASS_BG); premultiplied
                let k = (*p & 0xFF) as f32 / 255.0;
                let ga = 255.0 - (255.0 - a as f32) * k;
                let c = |v: u32| (((glass >> v) & 0xFF) as f32 * k).round() as u32;
                (ga.round() as u32) << 24 | c(16) << 16 | c(8) << 8 | c(0)
            };
        }
    }

    /// Take the master's new list for folder `dir` (re-read or a file deleted elsewhere),
    /// staying on the image shown.
    /// False: the folder's slot was busy; ask again later.
    pub fn adopt(&mut self, dir: &Path) -> bool {
        let Some(pl) = &self.playlist else { return true };
        if pl.container != dir || pl.rev {
            return true;
        }
        let Ok(h) = source::held(dir) else { return false };
        let Some((l, pos)) = h else { return true };
        if Arc::ptr_eq(&l, &pl.entries) || l.is_empty() {
            return true;
        }
        let cur = self.cur_entry();
        let at = cur.as_ref().and_then(|c| pos.get(c)).copied();
        let old = pl.index;
        let Some(pl) = &mut self.playlist else { return true };
        pl.entries = l;
        self.pos_map = pos;
        match at {
            Some(i) => {
                pl.index = i;
                self.update_wanted();
                self.preload();
                self.update_title();
                self.request_redraw();
            }
            None => {
                let i = old.min(pl.entries.len() - 1);
                self.go_to(i);
            }
        }
        true
    }

    /// True while this window shows `e` as a one-entry list, waiting for its folder list.
    pub fn wants_listed(&self, e: &Entry) -> bool {
        self.playlist.as_ref().is_some_and(|pl| pl.entries.len() == 1 && pl.entries[0] == *e)
    }

    /// Swap the one-entry list for the full folder list, keeping the view as it is.
    pub fn on_listed(&mut self, e: Entry, r: Result<Playlist, String>) {
        if !self.wants_listed(&e) {
            return;
        }
        let Ok(np) = r else { return };
        if np.entries.is_empty() {
            return;
        }
        self.pos_map = source::pos_map(&np.entries);
        self.playlist = Some(np);
        self.update_wanted();
        self.preload();
        self.update_title();
        self.request_redraw();
    }

    fn set_playlist(&mut self, pl: Playlist) {
        self.cfg.last_folder = if pl.container.is_dir() {
            pl.container.display().to_string()
        } else {
            pl.container.parent().map(|d| d.display().to_string()).unwrap_or_default()
        };
        self.pos_map = source::pos_map(&pl.entries);
        self.failed.borrow_mut().clear();
        self.rand_hist.clear();
        let mut pl = pl;
        if self.rtl_for.as_deref() == Some(pl.container.as_path()) && source::is_archive(&pl.container) {
            pl.rev = true;
            pl.index = pl.flip(pl.index);
        }
        let idx = pl.index;
        self.playlist = Some(pl);
        self.go_to(idx);
    }

    fn update_wanted(&mut self) {
        let mut w = HashSet::new();
        if let Some(pl) = &self.playlist {
            let i = pl.index as isize;
            let d = self.dir as isize;
            let offsets: &[isize] = if self.neighbours_wanted() { &[-2, -1, 0, 1, 2, 3, 4, 5, 6, 7] } else { &[0] };
            for o in offsets.iter().map(|o| o * d) {
                let j = i + o;
                if let Some(e) = (j >= 0).then(|| pl.get(j as usize)).flatten() {
                    w.insert(e.clone());
                }
            }
        }
        self.wanted.lock().unwrap().insert(self.id, w);
    }

    fn request(&mut self, e: Entry) {
        if self.cache.borrow().contains_key(&e) || self.inflight.borrow().contains(&e) || self.failed.borrow().contains_key(&e) {
            return;
        }
        self.inflight.borrow_mut().insert(e.clone());
        // the page a focused window shows now jumps the queue
        let urgent = self.cur_entry().as_ref() == Some(&e) && self.window.as_ref().is_some_and(|w| w.has_focus());
        for d in self.job_tx.send(e, urgent) {
            self.inflight.borrow_mut().remove(&d);
        }
    }

    /// True if `e` is nearer the current index than the image now on screen.
    fn closer_than_shown(&self, e: &Entry) -> bool {
        let idx = self.index() as i64;
        let Some(pl) = &self.playlist else { return true };
        let dist = |x: &Entry| self.pos_map.get(x).map(|&p| (pl.flip(p) as i64 - idx).abs()).unwrap_or(i64::MAX);
        match &self.current {
            Some((shown, _)) => dist(e) < dist(shown),
            None => true,
        }
    }

    fn preload(&mut self) {
        // No neighbour preloading while the slider is dragged: it competes with the image under it.
        if self.slider_drag {
            return;
        }
        let Some(pl) = &self.playlist else { return };
        let i = pl.index as isize;
        // Look 6 ahead in the direction of travel and 1 behind: at 30 flips/s from a cold
        // 7200 rpm disk, 3 ahead missed 6 of 40 images.
        let d = self.dir as isize;
        // GPU mode: only the focused window preloads its neighbours (100 windows asked
        // for up to 800 images against a cache of about 32 and decoded each one 6 times)
        let offsets: &[isize] = if self.neighbours_wanted() { &[0, 1, 2, 3, -1, 4, 5, 6] } else { &[0] };
        let list: Vec<Entry> = offsets
            .iter()
            .map(|o| o * d)
            .filter_map(|o| {
                let j = i + o;
                if j >= 0 {
                    pl.get(j as usize).cloned()
                } else {
                    None
                }
            })
            .collect();
        for e in list {
            self.request(e);
        }
    }

    /// How far `e` is from this window's current image (i64::MAX if not in its list).
    /// The master evicts the shared cache by the smallest distance over all windows.
    pub fn distance(&self, e: &Entry) -> i64 {
        let Some(pl) = &self.playlist else { return i64::MAX };
        // GPU mode, a window without focus keeps only its own image in the cache
        if !self.neighbours_wanted() && self.cur_entry().as_ref() != Some(e) {
            return i64::MAX;
        }
        // a window that gave its full texture back does not keep its image in the cache
        if self.stripped {
            return i64::MAX;
        }
        self.pos_map.get(e).map(|&p| (pl.flip(p) as i64 - pl.index as i64).abs()).unwrap_or(i64::MAX)
    }

    fn set_current(&mut self, e: Entry, d: Arc<Decoded>) {
        crate::trace(&format!("current {}", e.name()));
        self.frame_idx = 0;
        self.next_frame_at = if d.frame_count() > 1 {
            Some(Instant::now() + Duration::from_millis(self.frame_ms(d.frame_delay(0))))
        } else {
            None
        };
        self.current = Some((e, d));
        self.unpainted = Some(Instant::now());
        self.invalidate();
        self.update_title();
    }

    fn show_current(&mut self) {
        let Some(e) = self.cur_entry() else {
            self.current = None;
            self.next_frame_at = None;
            self.invalidate();
            self.update_title();
            return;
        };
        self.update_wanted();
        let cached = self.cache.borrow().get(&e).cloned();
        if let Some(d) = cached {
            self.set_current(e, d);
            self.preload();
        } else if self.failed.borrow().contains_key(&e) {
            self.current = None;
            self.next_frame_at = None;
            self.preload();
        } else {
            // Decode the current image alone first; neighbours would compete for memory bandwidth.
            self.request(e);
        }
        self.invalidate();
        self.update_title();
    }

    /// A decode finished. The master has already put it in the shared cache (or the
    /// failed list); this window shows it if it wants it.
    pub fn on_loaded_view(&mut self, e: &Entry, res: &Result<Arc<Decoded>, String>) {
        let is_cur = self.cur_entry().as_ref() == Some(e);
        match res {
            Ok(d) => {
                if is_cur {
                    self.set_current(e.clone(), d.clone());
                    self.preload();
                } else if self.slider_drag && self.closer_than_shown(e) {
                    // While dragging, the exact image is rarely ready before the slider moves
                    // on (1 of 39 decodes was shown in a 672 ms drag). Show the nearest one.
                    self.set_current(e.clone(), d.clone());
                }
            }
            Err(_) => {
                if is_cur {
                    self.preload();
                    self.current = None;
                    self.next_frame_at = None;
                    self.invalidate();
                    self.update_title();
                }
            }
        }
    }

    // ---------- navigation ----------

    fn go_to(&mut self, i: usize) {
        let Some(pl) = &mut self.playlist else { return };
        if pl.entries.is_empty() {
            return;
        }
        let before = pl.index;
        pl.index = i.min(pl.entries.len() - 1);
        crate::trace(&format!("go_to {} in window {}", pl.index, self.id));
        // slide from the last settled frame; a fast scrub keeps sliding from that one
        let hint = self.nav_hint.take();
        let after = self.index();
        if self.cfg.slide_ms > 0 && after != before && !self.last_frame.is_empty() {
            if let Some((_, _, _, _, w, h)) = self.last_key {
                let dir = hint.unwrap_or(if after > before { 1 } else { -1 });
                match &mut self.slide {
                    // a slide already running the same way: keep it going and let the
                    // newest image take the incoming place, so a fast scroll moves on
                    // smoothly instead of restarting from the old image every step
                    Some(s) if s.w == w && s.h == h && s.dir == dir => s.entered = Some(Instant::now()),
                    _ => {
                        // the old image's rows, from the layout it was drawn with
                        let (oy0, oy1) = self.layout.map(|l| (l.oy.floor().max(0.0) as usize, (l.oy + l.sh).ceil().max(0.0) as usize)).unwrap_or((0, h));
                        self.slide = Some(Slide { old: self.last_frame.clone(), w, h, dir, start: None, oy0, oy1, entered: None })
                    }
                }
            }
        }
        self.view.rotation = 0;
        self.view.flip_h = false;
        self.view.flip_v = false;
        if !self.view.zoom_fixed {
            self.view.user_zoom = None;
        }
        self.view.pan_reset = true;
        self.show_current();
    }

    fn len(&self) -> usize {
        self.playlist.as_ref().map(|p| p.entries.len()).unwrap_or(0)
    }

    fn index(&self) -> usize {
        self.playlist.as_ref().map(|p| p.index).unwrap_or(0)
    }

    fn step(&mut self, n: isize, single: bool) {
        self.dir = if n < 0 { -1 } else { 1 };
        self.nav_hint = Some(self.dir as i32);
        let len = self.len() as isize;
        if len == 0 {
            return;
        }
        let idx = self.index() as isize;
        let _ = single;
        let t = idx + n;
        if t >= len {
            if idx + 1 >= len || n.abs() == 1 && idx == len - 1 {
                self.at_end(true);
            } else {
                self.go_to((len - 1) as usize);
            }
        } else if t < 0 {
            if idx == 0 {
                self.at_end(false);
            } else {
                self.go_to(0);
            }
        } else {
            self.go_to(t as usize);
        }
    }

    fn at_end(&mut self, forward: bool) {
        match self.cfg.end_action {
            1 => {
                let len = self.len();
                self.go_to(if forward { 0 } else { len.saturating_sub(1) });
            }
            // an archive is read on its own: it never moves on to another folder or archive
            // by itself (the user can, with [ and ])
            2 if !self.archive_open() => self.container_step(forward, false),
            _ => self.msg(if forward { "This is the last image." } else { "This is the first image." }),
        }
    }

    fn next(&mut self) {
        self.step(1, false);
    }

    fn prev(&mut self) {
        self.step(-1, false);
    }

    fn random(&mut self) {
        let len = self.len();
        if len < 2 {
            return;
        }
        self.rand_hist.push(self.index());
        let mut t = (self.rand() % len as u64) as usize;
        if t == self.index() {
            t = (t + 1) % len;
        }
        self.go_to(t);
    }

    fn random_back(&mut self) {
        if let Some(i) = self.rand_hist.pop() {
            self.go_to(i);
        } else {
            self.msg("This is the first image.");
        }
    }

    /// Next or previous folder; `manual` (the [ and ] keys) also steps into archives.
    fn container_step(&mut self, forward: bool, manual: bool) {
        let Some(pl) = &self.playlist else {
            self.msg("Move Folder: Unable to navigate (Please open a file)");
            return;
        };
        let c = pl.container.clone();
        match source::sibling(&c, forward, self.cfg.sort, self.cfg.sort_desc, manual) {
            Some(np) => {
                let name = np.container.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                self.set_playlist(np);
                self.msg(format!("{} {}", if forward { "Next Folder:" } else { "Previous Folder:" }, name));
            }
            None => self.msg(if forward {
                "Next Folder: Unable to navigate"
            } else {
                "Previous Folder: Unable to navigate"
            }),
        }
    }

    fn ensure_base(&mut self) {
        if self.base.is_some() {
            return;
        }
        let Some((_, cur)) = self.current.clone() else { return };
        let fi = self.frame_idx.min(cur.frame_count() - 1);
        // a Lottie: GPU mode replays its frame textures (each drawn once); the processor
        // draws the frame now
        let lottie_tex = match (&cur.lottie, self.gpu.is_some()) {
            (Some(p), true) => p.frame_tex(fi),
            _ => None,
        };
        let img = match (&cur.lottie, &lottie_tex) {
            // the texture is the image; the first frame stands for it in RAM
            (Some(_), Some(_)) => cur.frames[0].img.clone(),
            (Some(p), None) => p.frame(fi),
            (None, _) => cur.frames[fi].img.clone(),
        };
        self.base_tex = if cur.lottie.is_some() { lottie_tex } else { cur.frames[fi].gpu.clone() };
        // GPU: the image as decoded; rotation, flips, Negative and Gamma are shader steps,
        // so they make no new copy and no new upload (the key is the image itself)
        if self.gpu.is_some() {
            let key = self.base_tex.as_ref().map(|t| Arc::as_ptr(t) as u64).unwrap_or(Arc::as_ptr(&img) as u64);
            if self.gpu_src != key {
                self.gpu_src = key;
                self.base_gen += 1;
            }
            self.base = Some(img);
            self.scaled = None;
            return;
        }
        let v = &self.view;
        self.base = Some(decode::transform(&img, v.rotation, v.flip_h, v.flip_v, v.negative, v.gamma));
        self.base_gen += 1;
        self.scaled = None;
    }

    /// Neighbour images are preloaded and kept for this window: always on the processor
    /// path; in GPU mode only for the focused window.
    fn neighbours_wanted(&self) -> bool {
        self.gpu.is_none() || self.window.as_ref().is_some_and(|w| w.has_focus())
    }

    /// GPU path: keep only the screen-sized copy (and the thumbnail): give back the full
    /// texture and the window-sized frame buffers. Covered windows, and windows past the
    /// shared budget, do this; the focused and most recently used windows keep theirs.
    fn strip(&mut self) {
        // no image yet: nothing to give back. (Marked anyway, a new window covered before
        // it was first shown let its arriving image go from the cache and decoded it again
        // when uncovered: 267 decodes for 200 in the 100-window test.) The budget check
        // still strips it later if needed.
        if self.gpu.is_none() || self.stripped || self.current.is_none() {
            if let (Some(g), true) = (&mut self.gpu, self.current.is_none()) {
                g.release(true);
            }
            return;
        }
        self.stripped = true;
        self.gpu_buf = Vec::new();
        if let Some(g) = &mut self.gpu {
            g.release(true);
        }
        // the shown image without its texture: frames keep only their thumbnails
        if let Some((e, d)) = self.current.clone() {
            if d.frames.iter().any(|f| f.gpu.is_some()) {
                let frames = d.frames.iter().map(|f| decode::Frame { img: f.img.clone(), delay_ms: f.delay_ms, gpu: None }).collect();
                // the thumbnails are stored turned: nothing left to turn
                let thin = Decoded { frames, format: d.format.clone(), file_size: d.file_size, modified: d.modified, exif: d.exif.clone(), orig_w: d.orig_w, orig_h: d.orig_h, orient: 1, lottie: d.lottie.clone() };
                self.current = Some((e, Arc::new(thin)));
            }
        }
        self.base = None;
        self.base_tex = None;
        self.last_frame.clear();
        self.last_key = None;
        crate::trace("gpu: window keeps its screen-sized copy only");
    }

    /// Used again (focused, uncovered): the full image comes back from the cache, or is
    /// decoded again; the screen-sized copy shows meanwhile.
    fn unstrip(&mut self) {
        if !self.stripped {
            return;
        }
        self.stripped = false;
        if let Some(e) = self.cur_entry() {
            let cached = self.cache.borrow().get(&e).cloned();
            match cached {
                Some(d) if d.frames.iter().any(|f| f.gpu.is_some()) => self.set_current(e, d),
                _ => {
                    self.cache.borrow_mut().remove(&e);
                    self.request(e);
                }
            }
        }
        self.request_redraw();
    }

    /// Covered (nothing of the window on screen): stop drawing, keep only the screen-sized
    /// copy, drop the surface; uncovered: draw again.
    fn set_covered(&mut self, o: bool) {
        if o == self.occluded {
            return;
        }
        crate::trace(&format!("window {} covered: {o}", self.id));
        self.occluded = o;
        if o {
            // Windows: a covered window keeps its swap chain and textures, so it shows
            // its last frame at once and the sharp copy without a new upload when raised.
            // The GPU budget (note_use, cache_mb) still strips the least recent windows.
            #[cfg(not(windows))]
            {
                self.strip();
                if let Some(g) = &mut self.gpu {
                    g.park();
                }
            }
        } else {
            self.unstrip();
            self.request_redraw();
        }
    }

    /// The view transform for the GPU.
    fn gpu_xf(&self) -> crate::gpu::Xform {
        let v = &self.view;
        let view = crate::gpu::Xform { rot: v.rotation % 4, flip_h: v.flip_h, flip_v: v.flip_v, negative: v.negative, gamma: v.gamma };
        // the shared texture is the photo as stored: its EXIF turn first, then the view's
        // (a thumbnail standing in is stored turned already)
        match (&self.base_tex, &self.current) {
            (Some(_), Some((_, d))) if d.orient > 1 => view.after(&crate::gpu::Xform::exif(d.orient)),
            _ => view,
        }
    }

    /// The width of the texture's full image: the stored photo's for a shared texture,
    /// the turned one's for a thumbnail standing in.
    fn gpu_full_w(&self, b: &RgbaImage) -> u32 {
        match &self.current {
            Some((_, d)) if self.base_tex.is_some() && d.orient >= 5 => d.orig_h,
            Some((_, d)) => d.orig_w,
            None => b.width(),
        }
    }

    fn compute_layout(&mut self, ww: f64, wh: f64) -> Option<Layout> {
        let b = self.base.as_ref()?;
        let (mut bw, mut bh) = (b.width() as f64, b.height() as f64);
        // GPU: the base may be a thumbnail (of a texture, or standing in); lay out the
        // full image
        if self.base_tex.is_some() || self.gpu.is_some() {
            if let Some((_, d)) = &self.current {
                (bw, bh) = (d.orig_w as f64, d.orig_h as f64);
            }
        }
        // GPU: the base is unrotated; lay out the turned image
        if self.gpu.is_some() && self.view.rotation % 2 == 1 {
            std::mem::swap(&mut bw, &mut bh);
        }
        let z = match self.view.user_zoom {
            Some(z) => z,
            None => {
                let fit = match self.cfg.view_mode {
                    0 => 1.0,
                    1 => (ww / bw).min(wh / bh),
                    _ => ww / bw,
                };
                if self.cfg.view_mode != 0 && !self.cfg.stretch_small {
                    fit.min(1.0)
                } else {
                    fit
                }
            }
        };
        let sw = bw * z;
        let sh = bh * z;
        if self.view.pan_reset && self.displaying_current() {
            self.view.pan_reset = false;
            self.view.pan_x = (sw - ww) / 2.0;
            self.view.pan_y = if self.cfg.view_mode == 2 || self.cfg.align_top { 0.0 } else { (sh - wh) / 2.0 };
        }
        self.view.pan_x = if sw > ww { self.view.pan_x.clamp(0.0, sw - ww) } else { 0.0 };
        self.view.pan_y = if sh > wh { self.view.pan_y.clamp(0.0, sh - wh) } else { 0.0 };
        let ox = if sw > ww { -self.view.pan_x } else { ((ww - sw) / 2.0).floor() };
        let oy = if sh > wh { -self.view.pan_y } else { ((wh - sh) / 2.0).floor() };
        let l = Layout { z, ox, oy, sw, sh, ww, wh };
        self.layout = Some(l);
        Some(l)
    }

    fn zoom_to(&mut self, nz: f64, anchor: Option<(f64, f64)>) {
        let Some(l) = self.layout else { return };
        let nz = nz.clamp(0.01, 20.0); // Honeyview stops at 2000%
        let (ax, ay) = anchor.unwrap_or((l.ww / 2.0, l.wh / 2.0));
        let ix = (ax - l.ox) / l.z;
        let iy = (ay - l.oy) / l.z;
        self.view.user_zoom = Some(nz);
        self.view.pan_x = ix * nz - ax;
        self.view.pan_y = iy * nz - ay;
        self.view.pan_reset = false;
        self.msg(format!("{:.0}%", nz * 100.0));
    }

    /// Zoom so the window shows the rectangle between two window points, as large as
    /// fits (the tighter of its width and height decides), centred.
    fn zoom_to_rect(&mut self, a: (f64, f64), b: (f64, f64)) {
        let Some(l) = self.layout else { return };
        let (rw, rh) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
        if rw < 8.0 || rh < 8.0 {
            return;
        }
        let nz = (l.z * (l.ww / rw).min(l.wh / rh)).clamp(0.01, 20.0);
        // image point at the rectangle's centre, then pan it to the window's centre
        let ix = ((a.0 + b.0) / 2.0 - l.ox) / l.z;
        let iy = ((a.1 + b.1) / 2.0 - l.oy) / l.z;
        self.view.user_zoom = Some(nz);
        self.view.pan_x = ix * nz - l.ww / 2.0;
        self.view.pan_y = iy * nz - l.wh / 2.0;
        self.view.pan_reset = false;
        self.msg(format!("{:.0}%", nz * 100.0));
    }

    fn zoom_step(&mut self, zin: bool, anchor: Option<(f64, f64)>) {
        let Some(l) = self.layout else { return };
        let z = l.z;
        let nz = if zin {
            ZOOM_STEPS.iter().copied().find(|&s| s > z * 1.001).unwrap_or(z)
        } else {
            ZOOM_STEPS.iter().rev().copied().find(|&s| s < z * 0.999).unwrap_or(z)
        };
        self.zoom_to(nz, anchor);
    }

    fn zoom_pct(&mut self, d: f64) {
        if let Some(l) = self.layout {
            self.zoom_to(((l.z * 100.0).round() + d) / 100.0, None);
        }
    }

    fn set_mode(&mut self, m: u8) {
        self.cfg.view_mode = m;
        self.view.user_zoom = None;
        self.view.pan_reset = true;
        self.invalidate();
        self.msg(match m {
            0 => "View: Original Size (100%)",
            1 => "View: Fit in Window",
            _ => "View: Fit to Width",
        });
    }

    fn rotate(&mut self, q: u8) {
        self.view.rotation = (self.view.rotation + q) % 4;
        self.view.pan_reset = true;
        self.invalidate();
        self.msg(format!("Rotation: {}°", self.view.rotation as u32 * 90));
    }

    fn scroll(&mut self, dy: f64) {
        if let Some(l) = self.layout {
            self.view.pan_y += dy * l.wh * 0.15;
            self.request_redraw();
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(w) = self.window.clone() else { return };
        self.fullscreen = !self.fullscreen;
        if self.fullscreen {
            // A maximized window kept its maximized client area in full screen: the
            // bottom 36 px (taskbar height) showed the unpainted window frame.
            // (macOS full screen keeps and restores the window's own frame)
            self.fs_was_max = cfg!(not(target_os = "macos")) && is_maximized(&w);
            if self.fs_was_max {
                w.set_maximized(false);
            }
            // macOS: native full screen (its own Space; the system slides the title bar and
            // menu bar down at the top edge and lays the window out below the notch)
            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
        } else {
            w.set_fullscreen(None);
            if self.fs_was_max {
                w.set_maximized(true);
            }
        }
    }

    /// Test hook: click the chrome control `id` as last drawn ("picker" opens Select Image).
    pub fn test_click(&mut self, el: &ActiveEventLoop, id: &str) {
        if id == "picker" {
            self.open_picker();
            return;
        }
        if let Some(h) = self.hits.iter().find(|h| h.id == id).cloned() {
            self.on_chrome_click(el, h, false);
        }
    }

    #[cfg(target_os = "macos")]
    pub fn test_press_green(&self) {
        if let Some(w) = &self.window {
            crate::mac::press_green(w);
        }
    }

    pub fn test_fullscreen(&mut self, maximize: bool) {
        if maximize {
            self.toggle_maximize();
        } else {
            self.toggle_fullscreen();
        }
    }

    fn toggle_maximize(&mut self) {
        if self.fullscreen {
            self.toggle_fullscreen();
            return;
        }
        if let Some(w) = &self.window {
            w.set_maximized(!is_maximized(&w));
        }
    }

    // ---------- file operations ----------

    fn open_dialog(&mut self) {
        let exts: Vec<&str> = source::IMAGE_EXTS.iter().chain(source::ARCHIVE_EXTS).copied().collect();
        let mut d = rfd::FileDialog::new()
            .add_filter("Image File", &exts)
            .add_filter("All Files", &["*"]);
        if !self.cfg.last_folder.is_empty() {
            d = d.set_directory(&self.cfg.last_folder);
        }
        if let Some(p) = d.pick_file() {
            self.open_path(&p);
        }
    }

    fn open_folder_dialog(&mut self) {
        let mut d = rfd::FileDialog::new().set_title("Select a folder.");
        if !self.cfg.last_folder.is_empty() {
            d = d.set_directory(&self.cfg.last_folder);
        }
        if let Some(p) = d.pick_folder() {
            self.open_path(&p);
        }
    }

    fn close(&mut self) {
        self.playlist = None;
        self.current = None;
        self.next_frame_at = None;
        self.pos_map = Default::default();
        self.update_wanted();
        self.invalidate();
        self.update_title();
    }

    /// The file on screen was deleted outside the app (checked every GONE_EVERY): show the
    /// first image left in its folder, or close the window when none is left.
    fn check_gone(&mut self) {
        let Some(e) = self.cur_entry() else { return };
        let p = e.fs_path().to_path_buf();
        if p.exists() {
            return;
        }
        crate::trace(&format!("gone: {}", p.display()));
        self.cache.borrow_mut().remove(&e);
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
        let entries = if dir.is_dir() { source::open_fresh(&dir, self.cfg.sort, self.cfg.sort_desc).map(|pl| pl.entries).unwrap_or_default() } else { Default::default() };
        if entries.is_empty() {
            crate::trace("gone: folder has no image left, closing the window");
            self.store_geometry();
            self.cfg.save();
            self.closed = true;
            self.settings = None;
            if let Some(w) = &self.window {
                w.set_visible(false);
            }
            return;
        }
        self.set_playlist(Playlist { container: dir, entries, rev: false, index: 0 });
    }

    fn remove_current_from_list(&mut self) {
        let Some(pl) = &mut self.playlist else { return };
        if pl.index >= pl.entries.len() {
            return;
        }
        let Some(e) = pl.get(pl.index).cloned() else { return };
        match source::remove(&pl.container, &e) {
            Some(l) => pl.entries = l,
            None => {
                let at = pl.flip(pl.index);
                Arc::make_mut(&mut pl.entries).remove(at);
            }
        }
        self.cache.borrow_mut().remove(&e);
        if pl.entries.is_empty() {
            self.close();
            return;
        }
        let idx = pl.index.min(pl.entries.len() - 1);
        self.pos_map = source::pos_map(&pl.entries);
        self.go_to(idx);
    }

    fn delete_current(&mut self) {
        let Some(e) = self.cur_entry() else { return };
        let Entry::File(p) = &e else {
            self.msg("This image is inside a compressed file and can't be deleted alone.");
            return;
        };
        if self.cfg.confirm_delete {
            let text = format!("Are you sure you want to move this file to the Recycle Bin?\n\n{}", p.display());
            if !crate::dialog::ask(self.window.as_deref(), "ArkImage", &text, crate::dialog::Kind::Warn) {
                return;
            }
        }
        match trash::delete(p) {
            Ok(()) => {
                self.remove_current_from_list();
                self.msg(format!("Deleted: {}", e.name()));
            }
            Err(err) => self.msg(format!("Delete failed: {err}")),
        }
    }

    fn to_photo_folder(&mut self, which: u8, mv: bool) {
        let Some(e) = self.cur_entry() else { return };
        let mut dir = if which == 1 { self.cfg.photo_folder1.clone() } else { self.cfg.photo_folder2.clone() };
        if dir.is_empty() {
            let Some(d) = rfd::FileDialog::new()
                .set_title(format!("Select Photo Folder {which}"))
                .pick_folder()
            else {
                return;
            };
            dir = d.display().to_string();
            if which == 1 {
                self.cfg.photo_folder1 = dir.clone();
            } else {
                self.cfg.photo_folder2 = dir.clone();
            }
            self.cfg.save();
        }
        let dst = Path::new(&dir).join(e.name());
        if dst.exists() {
            self.msg(format!("The file already exists: {}", dst.display()));
            return;
        }
        let r = match (&e, mv) {
            (Entry::File(p), true) => std::fs::rename(p, &dst)
                .or_else(|_| std::fs::copy(p, &dst).and_then(|_| std::fs::remove_file(p))),
            (Entry::File(p), false) => std::fs::copy(p, &dst).map(|_| ()),
            (Entry::Zip { .. }, true) => Err(std::io::Error::other("can't move an image out of a compressed file")),
            (Entry::Zip { .. }, false) => e.read_bytes().and_then(|b| std::fs::write(&dst, b)),
        };
        match r {
            Ok(()) => {
                let verb = if mv { "Move to" } else { "Copy to" };
                if mv {
                    self.remove_current_from_list();
                }
                self.msg(format!("{verb} Photo Folder {which}: {}", e.name()));
            }
            Err(err) => self.msg(format!("Can't save the file: {err}")),
        }
    }

    fn reveal(&mut self) {
        let Some(e) = self.cur_entry() else { return };
        let p = e.fs_path().to_path_buf();
        // Reuses an Explorer window already open on the folder ("explorer /select" opened a new one each time).
        #[cfg(windows)]
        if let Some(d) = p.parent() {
            if !crate::reveal::select_in_folder(d, std::slice::from_ref(&p)) {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
            }
        }
        #[cfg(target_os = "macos")]
        {
            crate::mac::reveal(std::slice::from_ref(&p));
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(d) = p.parent() {
            let _ = Command::new("xdg-open").arg(d).spawn();
        }
    }

    fn open_editor(&mut self) {
        let Some(e) = self.cur_entry() else { return };
        let Entry::File(p) = &e else {
            self.msg("Can't edit an image inside a compressed file.");
            return;
        };
        let r = if !self.cfg.editor.is_empty() {
            Command::new(&self.cfg.editor).arg(p).spawn()
        } else if cfg!(windows) {
            Command::new("mspaint").arg(p).spawn()
        } else {
            Command::new("open").arg("-a").arg("Preview").arg(p).spawn()
        };
        match r {
            Ok(_) => self.msg("Opening with Image Editor"),
            Err(_) => self.msg("Please set 'editor' in the configuration (F5)."),
        }
    }

    fn copy_image(&mut self) {
        self.ensure_base();
        let Some(b) = self.base.clone() else { return };
        // GPU: the full image is on the GPU; read it back
        let b = match self.base_tex.as_ref().and_then(|t| crate::gpu::read_tex(t)) {
            Some(full) => match &self.current {
                Some((_, d)) if d.orient > 1 => Arc::new(decode::orient(&full, d.orient as u32)),
                _ => Arc::new(full),
            },
            None => b,
        };
        // GPU: the base is untransformed; copy the image as shown
        let b = if self.gpu.is_some() {
            let v = &self.view;
            decode::transform(&b, v.rotation, v.flip_h, v.flip_v, v.negative, v.gamma)
        } else {
            b
        };
        let r = arboard::Clipboard::new().and_then(|mut cb| {
            cb.set_image(arboard::ImageData {
                width: b.width() as usize,
                height: b.height() as usize,
                bytes: std::borrow::Cow::Borrowed(b.as_raw()),
            })
        });
        self.msg(if r.is_ok() { "Copied to clipboard." } else { "Failed to copy to clipboard." });
    }

    fn copy_info(&mut self) {
        let text = self.info_lines().join("\n");
        let r = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text));
        self.msg(if r.is_ok() { "Copied to clipboard." } else { "Failed to copy to clipboard." });
    }

    /// Open an entry label: a file path, or "<archive>|<name inside>" (the crash restore).
    pub fn open_label(&mut self, line: &str) {
        let line = line.to_string();
        match line.split_once('|') {
            Some((arch, name)) => {
                self.open_path(Path::new(arch));
                let pos = self.playlist.as_ref().and_then(|pl| {
                    pl.iter().position(|e| matches!(e, Entry::Zip { name: n, .. } if n == name))
                });
                if let Some(i) = pos {
                    self.go_to(i);
                }
            }
            None => self.open_path(Path::new(&line)),
        }
    }

    /// This window as the session file records it (None before it has a window and image).
    /// The file on disk behind the current image (the archive, for an image inside one).
    pub fn cur_fs_path(&self) -> Option<PathBuf> {
        self.cur_entry().map(|e| e.fs_path().to_path_buf())
    }

    pub fn win_state(&self) -> Option<crate::backup::WinState> {
        let w = self.window.as_ref()?;
        // a window still loading its first image has no label yet
        let label = self.cur_entry().map(|e| e.label()).unwrap_or_default();
        let (x, y, gw, gh) = self.geom.or_else(|| {
            let p = w.outer_position().ok()?;
            let s = w.inner_size();
            Some((p.x, p.y, s.width, s.height))
        })?;
        Some(crate::backup::WinState { x, y, w: gw, h: gh, max: self.was_max, fs: self.fullscreen, label })
    }

    fn reload_config(&mut self) {
        self.cfg = Config::load();
        self.view.user_zoom = None;
        self.view.pan_reset = true;
        self.invalidate();
        self.msg("Settings reloaded.");
    }

    // ---------- drawing ----------

    fn update_title(&mut self) {
        let Some(w) = self.window.clone() else { return };
        let t = match &self.playlist {
            Some(pl) if !pl.entries.is_empty() => {
                let Some(e) = pl.get(pl.index) else { return };
                let dims = self
                    .current
                    .as_ref()
                    .filter(|c| &c.0 == e)
                    .map(|c| format!(" - {}x{}", c.1.orig_w, c.1.orig_h))
                    .unwrap_or_default();
                let z = self.layout.map(|l| format!(" - {:.0}%", l.z * 100.0)).unwrap_or_default();
                format!("{} - [{}/{}]{}{} - ArkImage", e.name(), pl.index + 1, pl.entries.len(), dims, z)
            }
            _ => "ArkImage".to_string(),
        };
        if t != self.last_title {
            w.set_title(&t);
            self.last_title = t;
        }
    }

    /// The info card: title (file name) and sections of label/value rows.
    fn info_card(&self) -> Option<(String, Vec<Vec<crate::ui::Row>>)> {
        use crate::ui::Row;
        let row = |k: &str, v: String, mono: bool| Row { key: k.to_string(), value: v, mono };
        let pl = self.playlist.as_ref()?;
        let e = pl.get(pl.index)?;
        let mut secs = vec![vec![
            row("Folder", e.fs_path().parent().map(|p| p.display().to_string()).unwrap_or_default(), false),
            row("Index", format!("{} / {}", pl.index + 1, pl.entries.len()), true),
        ]];
        if let Some((ce, d)) = &self.current {
            if ce == e {
                let mut img = vec![
                    row("Dimensions", format!("{} × {}", d.orig_w, d.orig_h), true),
                    row("Format", if d.frame_count() > 1 { format!("{}, {} frames", d.format, d.frame_count()) } else { d.format.to_string() }, false),
                    row("File size", fmt_size(d.file_size), true),
                ];
                if let Some(t) = d.modified {
                    img.push(row("Modified", fmt_time(t), true));
                }
                if let Some(l) = self.layout {
                    img.push(row("Zoom", format!("{:.1}%", l.z * 100.0), true));
                }
                img.push(row("Rotation", format!("{}°", self.view.rotation as u32 * 90), true));
                secs.push(img);
                if !d.exif.is_empty() {
                    secs.push(d.exif.iter().map(|(k, v)| row(k, v.clone(), false)).collect());
                }
            }
        }
        Some((e.name(), secs))
    }

    fn info_lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        let Some(pl) = &self.playlist else { return v };
        let Some(e) = pl.get(pl.index) else { return v };
        v.push(format!("File Name: {}", e.name()));
        v.push(format!("Path: {}", e.fs_path().display()));
        v.push(format!("Index: {} / {}", pl.index + 1, pl.entries.len()));
        if let Some((ce, d)) = &self.current {
            if ce == e {
                let frames = if d.frame_count() > 1 { format!("  {} frames", d.frame_count()) } else { String::new() };
                v.push(format!("Image Size: {} x {}  {}{}", d.orig_w, d.orig_h, d.format, frames));
                v.push(format!("File Size: {}", fmt_size(d.file_size)));
                if let Some(t) = d.modified {
                    v.push(format!("Date Modified: {}", fmt_time(t)));
                }
                if let Some(l) = self.layout {
                    v.push(format!("Zoom: {:.1}%   Rotation: {}°", l.z * 100.0, self.view.rotation as u32 * 90));
                }
                if !d.exif.is_empty() {
                    v.push(String::new());
                    for (k, val) in &d.exif {
                        v.push(format!("{k}: {val}"));
                    }
                }
            }
        }
        v
    }

    /// The GPU setting, with the ARKIMAGE_GPU=0 test override (never saved).
    /// Windows: never while the window glass shows (one window open). A DirectX swap chain
    /// covers the glass: the blur behind the window reached neither an HWND swap chain
    /// (Opaque only) nor a DirectComposition one (#000000 under a clear GPU frame, also
    /// with the Windows 11 Acrylic backdrop). The window switches to the GPU once the
    /// glass has faded out, when a second window opens.
    fn gpu_wanted(&self) -> bool {
        let glass = cfg!(windows) && (self.glass_on || self.glass_loaded || !self.glass_known) && std::env::var_os("ARKIMAGE_GPU_SELFTEST").is_none();
        self.cfg.gpu && !glass && std::env::var("ARKIMAGE_GPU").as_deref() != Ok("0") && !self.too_big_for_gpu()
    }

    /// The image shown has a side past the GPU's texture limit (16,384 px on Apple
    /// silicon): this window draws it with the processor at full size rather than shrink
    /// it to fit, and goes back to the GPU at the next image that fits.
    fn too_big_for_gpu(&self) -> bool {
        let Some((_, d)) = &self.current else { return false };
        let lim = crate::gpu::get().map(|g| g.max_tex()).unwrap_or(u32::MAX);
        d.lottie.is_none() && (d.orig_w > lim || d.orig_h > lim)
    }

    /// The processor's window surface (softbuffer): at start without a GPU, or after the
    /// GPU path failed or was turned off.
    fn cpu_surface(&mut self) {
        let Some(win) = self.window.clone() else { return };
        self.gpu = None;
        let ctx = softbuffer::Context::new(win.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&ctx, win.clone()).expect("softbuffer surface");
        // the GPU's layer would stay under this one with its last frame (see mac.rs)
        #[cfg(target_os = "macos")]
        crate::trace(&format!("stale layers removed: {}", crate::mac::drop_stale_layers(&win)));
        self._context = Some(ctx);
        self.surface = Some(surface);
        self.last_frame.clear();
        self.last_key = None;
        self.base = None;
        self.scaled = None;
        // the GPU copies are gone or unusable here: decoders keep RAM copies from now on,
        // images that lived on the GPU are dropped from the cache, and the one shown is
        // decoded again (its thumbnail stays on screen meanwhile)
        crate::gpu::DECODE_TO_GPU.store(false, Ordering::Relaxed);
        self.cache.borrow_mut().retain(|_, d| d.frames.iter().all(|f| f.gpu.is_none()));
        self.base_tex = None;
        if let Some(e) = self.cur_entry() {
            self.request(e);
        }
    }

    /// Test hook ARKIMAGE_GPU_SELFTEST: the GPU's full-quality shrink against the
    /// processor's, per kernel and preset, on the current image; prints and exits.
    fn gpu_selftest(&mut self) {
        let Some(base) = self.base.clone() else { return };
        // the base is a thumbnail when the image lives on the GPU: test on the full image
        let base = match self.base_tex.as_ref().and_then(|t| crate::gpu::read_tex(t)) {
            Some(full) => Arc::new(full),
            None => base,
        };
        let (bw, bh) = base.dimensions();
        let mut user = self.cfg.filter_presets.clone();
        let mut presets = crate::filters::builtins();
        presets.append(&mut user);
        // every kernel, with and without sharpening
        for k in 2..=6u8 {
            presets.push(crate::filters::Preset { name: format!("kernel {k}"), kernel: k, amount: 0, radius: 100, threshold: 0 });
            presets.push(crate::filters::Preset { name: format!("kernel {k} sharp"), kernel: k, amount: 150, radius: 150, threshold: 3 });
        }
        let interp = self.cfg.interp;
        for z in [0.29f64, 0.5, 0.11] {
            let (sw, sh) = (((bw as f64 * z).round() as u32).max(1), ((bh as f64 * z).round() as u32).max(1));
            for (i, p) in presets.iter().enumerate() {
                let t0 = Instant::now();
                let cpu = self.scale_with_preset(&base, sw, sh);
                let tc = t0.elapsed();
                let q = crate::gpu::Hq {
                    key: 0xC0FFEE ^ i as u64 ^ (sw as u64) << 32,
                    w: sw,
                    h: sh,
                    kernel: p.resize_code(interp),
                    amount: p.amount as f32 / 100.0,
                    sigma: p.radius as f32 / 100.0,
                    threshold: p.threshold,
                    x: 0,
                    y: 0,
                    xf: Default::default(),
                };
                // the processor uses the selected preset; set it for this case
                let _ = &cpu;
                // the processor's own path for this preset
                let cpu = {
                    let mut img = render::resize(&base, sw, sh, p.resize_code(interp));
                    render::sharpen(&mut img, p.amount as f32 / 100.0, p.radius as f32 / 100.0, p.threshold);
                    img
                };
                let t1 = Instant::now();
                let gpu = self.gpu.as_mut().map(|g| g.hq_readback(self.base_gen as u64, &base, &q)).unwrap_or_default();
                let tg = t1.elapsed();
                let mut hist = [0u64; 256];
                for (k, (a, b)) in cpu.as_raw().iter().zip(gpu.iter()).enumerate() {
                    if k % 4 != 3 {
                        hist[(*a as i32 - *b as i32).unsigned_abs() as usize] += 1;
                    }
                }
                let n: u64 = hist.iter().sum();
                let max = hist.iter().rposition(|&c| c > 0).unwrap_or(0);
                if max > 2 {
                    let over: u64 = hist[3..].iter().sum();
                    // the first few: position, channel, both values
                    let mut ex = Vec::new();
                    for (k, (a, b)) in cpu.as_raw().iter().zip(gpu.iter()).enumerate() {
                        if k % 4 != 3 && a.abs_diff(*b) > 2 && ex.len() < 4 {
                            let px = k / 4;
                            ex.push(format!("({},{}) c{} cpu {} gpu {}", px % sw as usize, px / sw as usize, k % 4, a, b));
                        }
                    }
                    eprintln!("selftest   over 2: {over} values; {}", ex.join("; "));
                }
                let le = |l: usize| 100.0 * hist[..=l].iter().sum::<u64>() as f64 / n.max(1) as f64;
                eprintln!(
                    "selftest z={z} {sw}x{sh} preset {i} {:?} kernel {}: <=0 {:.2}% <=1 {:.3}% <=2 {:.3}% max {max}  cpu {:.0} ms gpu {:.0} ms",
                    p.name,
                    p.resize_code(interp),
                    le(0),
                    le(1),
                    le(2),
                    tc.as_secs_f64() * 1000.0,
                    tg.as_secs_f64() * 1000.0
                );
            }
        }
        // the GPU path's band decode against the processor path's decode of the same file
        // (the processor path, not the image crate: their JPEG decoders differ by up to 5)
        if let Some(e) = self.cur_entry() {
            if let Ok(d) = decode::decode(&e, self.cfg.exif_rotate, false) {
                {
                    let img = d.frames[0].img.clone();
                    let mut max = 0u8;
                    let mut over1 = 0u64;
                    for (k, (a, b)) in img.as_raw().iter().zip(base.as_raw().iter()).enumerate() {
                        if k % 4 != 3 {
                            let d = a.abs_diff(*b);
                            max = max.max(d);
                            over1 += (d > 1) as u64;
                        }
                    }
                    eprintln!("selftest decoder: GPU texture vs processor decode {}x{}: max {max}, over 1: {over1} of {}", img.width(), img.height(), img.as_raw().len() / 4 * 3);
                }
            }
        }
        // the view transform: rotations, flips, Negative, Gamma, with Soften+Sharpen
        let p = crate::filters::builtins()[2].clone();
        let xfs = [
            (1u8, false, false, false, false),
            (2, false, false, false, false),
            (3, false, false, false, false),
            (0, true, false, false, false),
            (0, false, true, false, false),
            (1, true, false, false, false),
            (0, false, false, true, false),
            (0, false, false, false, true),
            (3, false, true, true, true),
        ];
        for (n, &(rot, fh, fv, neg, gam)) in xfs.iter().enumerate() {
            let xf = crate::gpu::Xform { rot, flip_h: fh, flip_v: fv, negative: neg, gamma: gam };
            let shown = decode::transform(&base, rot, fh, fv, neg, gam);
            let z = 0.29;
            let (sw, sh) = (((shown.width() as f64 * z).round() as u32).max(1), ((shown.height() as f64 * z).round() as u32).max(1));
            let mut cpu = render::resize(&shown, sw, sh, p.resize_code(interp));
            render::sharpen(&mut cpu, p.amount as f32 / 100.0, p.radius as f32 / 100.0, p.threshold);
            let q = crate::gpu::Hq { key: 0xBEEF ^ n as u64, w: sw, h: sh, kernel: p.resize_code(interp), amount: p.amount as f32 / 100.0, sigma: p.radius as f32 / 100.0, threshold: p.threshold, x: 0, y: 0, xf };
            let raw = self.gpu.as_mut().map(|g| g.hq_readback(self.base_gen as u64, &base, &q)).unwrap_or_default();
            let (uw, uh) = if rot % 2 == 1 { (sh, sw) } else { (sw, sh) };
            let Some(un) = RgbaImage::from_raw(uw, uh, raw) else { eprintln!("selftest xf {xf:?}: readback failed"); continue };
            // the GPU copy is unrotated: turn it as the layer shader does, to compare
            let gpu = decode::transform(&Arc::new(un), rot, fh, fv, false, false);
            let mut max = 0u8;
            let mut over1 = 0u64;
            for (k, (a, b)) in cpu.as_raw().iter().zip(gpu.as_raw().iter()).enumerate() {
                if k % 4 != 3 {
                    let d = a.abs_diff(*b);
                    max = max.max(d);
                    if d > 1 {
                        over1 += 1;
                    }
                }
            }
            eprintln!("selftest xf {xf:?}: size {}x{} vs {}x{}, max {max}, over 1: {over1}", cpu.width(), cpu.height(), gpu.width(), gpu.height());
        }
        std::process::exit(0);
    }

    /// Draw the animation row alone into `rows` (the window's rows y0..y1, `w` wide, of a
    /// window `h` tall): holes, then the row. True if it landed on exactly those rows.
    fn draw_anim_band(&mut self, rows: &mut [u32], w: usize, y0: usize, y1: usize, h: usize) -> bool {
        let Some(mut t) = self.text.borrow_mut().take() else { return false };
        let hover_fx = self.hover_fx.clone();
        let anim_fmt = self.anim_format();
        let st = chrome::State {
            sf: self.sf(),
            title: "",
            show_top: false,
            show_bottom: true,
            maximized: false,
            lock_bottom: self.cfg.lock_bottom,
            hover_fx: &hover_fx,
            tip_fx: &[],
            info_on: false,
            index: self.index(),
            len: self.len(),
            anim: self.anim_frames().map(|n| (self.frame_idx.min(n - 1), n)),
            anim_fmt: &anim_fmt,
            anim_speed: self.anim_speed,
            anim_fps: self.anim_fps(),
            page_drag: None,
            archive: false,
            rtl: false,
            flip_h: false,
            flip_v: false,
        };
        rows.fill(render::HOLE);
        let mut c = Canvas { buf: rows, w, h: y1 - y0 };
        let mut hits = Vec::new();
        let band = chrome::draw_anim_row(&mut c, &mut t, &st, &mut hits, h as f32, y0 as f32);
        *self.text.borrow_mut() = Some(t);
        band == Some((y0, y1))
    }

    /// The GPU path: the interface on the processor with holes, the image on the GPU.
    fn render_gpu(&mut self, w: u32, h: u32) {
        if std::env::var_os("ARKIMAGE_GPU_SELFTEST").is_some() && self.displaying_current() {
            self.ensure_base();
            self.gpu_selftest();
        }
        let n = (w * h) as usize;
        let mut buf = std::mem::take(&mut self.gpu_buf);
        let (wu, hu) = (w as usize, h as usize);
        // an animation frame alone: redraw only the animation row (counter and track); the
        // rest of the interface and the image placement stay as last drawn. Measured on a
        // 3420x2148 window: the full interface took 6% CPU at 30 frames a second and the
        // change scan another 4%.
        let fast = !self.ui_dirty && buf.len() == n && self.gpu_img.is_some() && self.gpu_slide.is_none() && self.gpu_hq.is_none();
        self.ui_dirty = false;
        let mut rows = None;
        if let (true, Some((y0, y1))) = (fast, self.anim_band) {
            self.ensure_base();
            // (0, 0): no animation row on screen (bar hidden); nothing in the interface
            // follows the frame
            if y0 == y1 || self.draw_anim_band(&mut buf[y0 * wu..y1 * wu], wu, y0, y1, hu) {
                // the new frame is the layer a slide starts from, as draw() marks it
                self.gpu_settle = self.cfg.slide_ms > 0;
                rows = Some((y0, y1));
                crate::trace("anim row only");
            }
        }
        if rows.is_none() {
            buf.resize(n, 0);
            self.gpu_img = None;
            self.gpu_hq = None;
            self.gpu_slide = None;
            self.gpu_settle = false;
            self.gpu_motion = 0.0;
            {
                let mut c = Canvas { buf: &mut buf, w: wu, h: hu };
                crate::trace("draw start");
                crate::chrome::reset_anim_band();
                self.draw(&mut c);
            }
            crate::trace("draw end");
            // the animation row may be redrawn alone only if, drawn alone, it matches what
            // the full draw left in its rows
            let was = self.anim_band.is_some();
            self.anim_band = None;
            let band = crate::chrome::last_anim_band();
            if band.is_none() && self.gpu_img.is_some() && self.gpu_slide.is_none() {
                self.anim_band = Some((0, 0));
            }
            if let Some((y0, y1)) = band.filter(|&(a, b)| a < b && b <= hu) {
                if self.gpu_img.is_some() && self.gpu_slide.is_none() {
                    let mut tmp = vec![0u32; (y1 - y0) * wu];
                    if self.draw_anim_band(&mut tmp, wu, y0, y1, hu) && tmp[..] == buf[y0 * wu..y1 * wu] {
                        self.anim_band = Some((y0, y1));
                    }
                }
            }
            if was != self.anim_band.is_some() {
                crate::trace(&format!("anim row alone: {}", self.anim_band.is_some()));
            }
        }
        let sf = self.sf();
        let glass = self.glass_prepare();
        let bg_alpha = if glass { self.glass_bg_alpha() } else { 1.0 };
        if let Some(g) = &mut self.gpu {
            g.set_see_through(glass);
        }
        let base = self.base.clone();
        let image = match (&self.gpu_img, &base) {
            (Some((ox, oy, z, smooth)), Some(b)) => Some(crate::gpu::ImageDraw { full_w: self.gpu_full_w(b), tex: self.base_tex.clone(), xf: self.gpu_xf(), key: self.base_gen as u64, img: b, ox: *ox, oy: *oy, z: *z, smooth: *smooth }),
            _ => None,
        };
        let frame = crate::gpu::Frame {
            hq: self.gpu_hq,
            ui: &buf,
            w,
            h,
            image,
            bg: self.cfg.bg_color,
            bg_alpha,
            checker: self.cfg.transparent_grid,
            radii: [crate::chrome::GLASS_BLUR * sf, crate::ui::CARD_BLUR * sf / 3.0, 12.0 * sf],
            slide_old: self.gpu_slide,
            motion: if self.gpu_slide.is_some() { self.gpu_motion } else { 0.0 },
            settle: self.gpu_settle,
            rows,
        };
        crate::trace("scan end");
        let used = self.base_tex.as_ref().map(|t| t.bytes).unwrap_or(0);
        let focused = self.window.as_ref().is_some_and(|w| w.has_focus());
        let budget = crate::config::cache_mb() << 20;
        if let Some(g) = &mut self.gpu {
            g.note_use(used, focused, budget);
        }
        // a panic here would abort the whole app (it runs inside AppKit's callbacks): catch
        // it, and let this window carry on with the processor
        let res = self.gpu.as_mut().map(|g| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g.render(&frame))).unwrap_or_else(|_| {
                crate::trace("gpu: frame panicked; processor drawing for this window");
                Err(crate::gpu::Fail::Lost)
            })
        });
        drop(frame);
        self.gpu_buf = buf;
        match res {
            Some(Ok(())) => {
                self.unpainted = None;
                crate::trace("present");
            }
            Some(Err(crate::gpu::Fail::Skip)) | None => {}
            // the same state the covered check reads: covered now, not at the next check
            // half a second later (meanwhile the repaint failsafe asked again 4 times a
            // second, and 100 covered windows drew 100 frames a second at idle)
            Some(Err(crate::gpu::Fail::Occluded)) => self.set_covered(true),
            Some(Err(crate::gpu::Fail::Lost)) => {
                // the GPU is gone: this window carries on with the processor
                crate::trace("gpu lost: processor drawing");
                self.cpu_surface();
                self.request_redraw();
            }
        }
        self.update_title();
    }

    fn render(&mut self) {
        let Some(win) = self.window.clone() else { return };
        let sz = win.inner_size();
        let (w, h) = (sz.width.max(1), sz.height.max(1));
        // Settings > Image Processing > Use GPU acceleration: switch this window's path
        if self.gpu_wanted() != self.gpu_cfg {
            self.gpu_cfg = self.gpu_wanted();
            if self.gpu_cfg {
                if let Some(g) = crate::gpu::GpuWin::new(win.clone()) {
                    self.gpu = Some(g);
                    crate::gpu::DECODE_TO_GPU.store(true, Ordering::Relaxed);
                    self.surface = None;
                    self._context = None;
                    self.base = None;
                }
            } else if self.gpu.is_some() {
                self.cpu_surface();
            }
        }
        if self.gpu.is_some() {
            if self.occluded {
                // nothing to show; the uncovering redraws
                return;
            }
            self.render_gpu(w, h);
            return;
        }
        let Some(mut surface) = self.surface.take() else { return };
        if surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).is_ok() {
            if let Ok(mut buf) = surface.buffer_mut() {
                {
                    let glass = self.glass_prepare();
                    let mut c = Canvas { buf: &mut buf, w: w as usize, h: h as usize };
                    crate::trace("draw start");
                    self.draw(&mut c);
                    if glass {
                        self.glass_finish(c.buf);
                    }
                }
                if buf.present().is_ok() {
                    self.unpainted = None;
                }
                crate::trace("present");
            }
        }
        self.surface = Some(surface);
        self.update_title();
    }

    fn draw(&mut self, c: &mut Canvas) {
        // Grid open with its background cached: skip the viewer entirely.
        if self.picker.open && self.picker.bg_dims == (c.w, c.h) && self.picker.bg.len() == c.buf.len() {
            let sf = self.sf();
            let mut t = self.text.borrow_mut().take();
            if let Some(pl) = &self.playlist {
                let cache = &self.cache;
                let mc = |e: &Entry| cache.borrow().get(e).map(|d| d.frames[0].img.clone());
                self.picker.draw(c, t.as_mut(), pl, pl.index, sf, &self.thumb_jobs, &mc);
            }
            *self.text.borrow_mut() = t;
            if self.picker.bar_anim {
                self.request_redraw();
            }
            return;
        }
        let on_gpu = self.gpu.is_some();
        if on_gpu {
            // everything starts as a hole: the GPU draws background and image under it
            c.fill(crate::render::HOLE);
        } else if self.glass_loaded {
            crate::render::GLASS_BG.store(self.cfg.bg_color, std::sync::atomic::Ordering::Relaxed);
            c.fill(GLASS_MARK | 0xFF_FFFF);
        } else {
            c.fill(self.cfg.bg_color);
        }
        self.ensure_base();
        let bg = self.cfg.bg_color;
        let checker = self.cfg.transparent_grid;
        // a slide in progress: drop it if the window changed size, start it once the new
        // image is ready, end it after slide_ms
        if self.slide.as_ref().is_some_and(|s| s.w != c.w || s.h != c.h) || self.cfg.slide_ms == 0 {
            self.slide = None;
        }
        let ready = self.displaying_current();
        let mut slide_at: Option<(f64, i64)> = None;
        if let Some(s) = &mut self.slide {
            if s.start.is_none() && ready {
                s.start = Some(Instant::now());
            }
            if let Some(t0) = s.start {
                let t = (t0.elapsed().as_secs_f64() * 1000.0 / self.cfg.slide_ms as f64).min(1.0);
                if t >= 1.0 {
                    self.slide = None;
                } else {
                    // ease-out cubic: leaves fast, lands soft
                    let e = 1.0 - (1.0 - t).powi(3);
                    let w = c.w as f64;
                    let d = s.dir as f64;
                    slide_at = Some((d * w * (1.0 - e), (-d * w * e).round() as i64));
                    // GPU motion blur: the distance moved over one frame (the ease's speed,
                    // 3 (1 - t)^2, times the frame time), smeared over half of it each way
                    // of the frame's position, so a fast start blurs and the landing is sharp
                    // a first frame has no previous one: count one 60 Hz frame (measured: an
                    // old timestamp gave a 349 px smear on the first frame)
                    let since = self.slide_drawn.elapsed().as_secs_f64();
                    let frame_ms = if since > 0.05 { 16.7 } else { since.max(0.004) * 1000.0 };
                    // a page that entered a running slide blurs as at its own start
                    let te = s.entered.map_or(t, |at| (at.elapsed().as_secs_f64() * 1000.0 / self.cfg.slide_ms as f64).min(t));
                    let per_frame = w * 3.0 * (1.0 - te).powi(2) * frame_ms / self.cfg.slide_ms as f64;
                    self.gpu_motion = (per_frame * 0.5 * 0.5) as f32;
                    crate::trace(&format!("slide motion blur {:.1} px", self.gpu_motion));
                }
            }
        }
        if let (Some((_, dx_old)), Some(s)) = (slide_at, &self.slide) {
            if on_gpu {
                self.gpu_slide = Some(dx_old as f64);
            } else {
                shift_rows(c, &s.old, dx_old);
            }
        }
        if let Some(l) = self.compute_layout(c.w as f64, c.h as f64).filter(|_| on_gpu) {
            // the GPU draws the image; slides and pinch zoom move it there
            let dx_new = slide_at.map(|s| s.0).unwrap_or(0.0);
            let smooth = self.pinch_at.is_some() || self.preset().smooth();
            self.gpu_img = Some((l.ox + dx_new, l.oy, l.z, smooth));
            // settled: the full-quality shrink (and sharpening), as the processor does it
            let preset = self.preset();
            let up_sharp = preset.amount > 0 && preset.smooth() && l.z <= 4.0 && l.sw * l.sh <= 40e6;
            // a playing animation draws its frames from their mipmaps: the full-quality
            // shrink for every frame cost 38% CPU on a 30 fps Lottie
            let playing = self.next_frame_at.is_some();
            if slide_at.is_none() && self.pinch_at.is_none() && !playing && (l.z < 0.999 || (l.z > 1.001 && up_sharp)) {
                let (sw, sh) = ((l.sw.round() as u32).max(1), (l.sh.round() as u32).max(1));
                let fp = preset.fingerprint(self.cfg.interp);
                let xf = self.gpu_xf();
                let xk = xf.rot as u64 | (xf.flip_h as u64) << 2 | (xf.flip_v as u64) << 3 | (xf.negative as u64) << 4 | (xf.gamma as u64) << 5;
                let key = (self.base_gen as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (sw as u64) << 40 ^ (sh as u64) << 20 ^ fp.rotate_left(7) ^ xk << 58;
                self.gpu_hq = Some(crate::gpu::Hq {
                    key,
                    w: sw,
                    h: sh,
                    kernel: preset.resize_code(self.cfg.interp),
                    amount: preset.amount as f32 / 100.0,
                    sigma: preset.radius as f32 / 100.0,
                    threshold: preset.threshold,
                    x: l.ox.round() as i64,
                    y: l.oy.round() as i64,
                    xf,
                });
            }
            if slide_at.is_some() {
                self.slide_drawn = Instant::now();
            }
            let key = (self.base_gen as u64, l.ox.to_bits(), l.oy.to_bits(), l.z.to_bits(), c.w, c.h);
            if self.cfg.slide_ms > 0 && ready && self.pinch_at.is_none() && slide_at.is_none() && self.last_key != Some(key) {
                // the GPU keeps the settled layer; this only marks that one exists
                self.gpu_settle = true;
                self.last_frame.clear();
                self.last_frame.push(0);
                self.last_key = Some(key);
            }
        } else if let Some(l) = self.compute_layout(c.w as f64, c.h as f64) {
            let base = self.base.clone().unwrap();
            let preset = self.preset();
            if let Some((dx_new, _)) = slide_at {
                // during the slide: sample only the visible pixels, no filters (as pinch zoom)
                render::blit_sample(c, &base, l.ox + dx_new, l.oy, l.z, true, bg, checker);
                // the GPU's slide motion blur, on the processor (a lone window draws here)
                if self.gpu_motion > 1.0 {
                    let t0 = Instant::now();
                    // the old image slides out at the same height, sized for this window
                    let (y0, y1) = (l.oy.floor().max(0.0) as usize, (l.oy + l.sh).ceil().max(0.0) as usize);
                    let (o0, o1) = self.slide.as_ref().map(|s| (s.oy0, s.oy1)).unwrap_or((y0, y1));
                    render::motion_blur(c, self.gpu_motion, y0.min(o0), y1.max(o1));
                    crate::trace(&format!("cpu motion blur {:.1} px in {:.1} ms", self.gpu_motion, t0.elapsed().as_secs_f64() * 1000.0));
                }
                self.slide_drawn = Instant::now();
            } else {
            // Enlarged images go through the resize path too when the preset sharpens,
            // up to 4x and 40 MP; past that, per-pixel sampling (no sharpening).
            let up_sharp = preset.amount > 0 && preset.smooth() && l.z <= 4.0 && l.sw * l.sh <= 40e6;
            if self.pinch_at.is_none() && (l.z < 0.999 || (l.z > 1.001 && up_sharp)) {
                let sw = (l.sw.round() as u32).max(1);
                let sh = (l.sh.round() as u32).max(1);
                let key = (self.base_gen, sw, sh, preset.fingerprint(self.cfg.interp));
                // Large images: show a quick downscale first, then refine on the next frame.
                let big = preset.smooth() && base.width() as u64 * base.height() as u64 > 4_000_000;
                if self.scaled.as_ref().map(|s| s.0) != Some(key) {
                    crate::trace("resize start");
                    let img = if big { render::resize(&base, sw, sh, 3) } else { self.scale_with_preset(&base, sw, sh) };
                    self.scaled = Some((key, img));
                    self.scaled_hq = !big;
                    if big {
                        self.request_redraw();
                    }
                    crate::trace("resize end");
                } else if !self.scaled_hq {
                    let img = self.scale_with_preset(&base, sw, sh);
                    self.scaled = Some((key, img));
                    self.scaled_hq = true;
                    crate::trace("refined");
                }
                let img = &self.scaled.as_ref().unwrap().1;
                render::blit(c, img, l.ox.round() as i64, l.oy.round() as i64, bg, checker);
            } else {
                render::blit_sample(c, &base, l.ox, l.oy, l.z, self.pinch_at.is_some() || self.preset().smooth(), bg, checker);
            }
            // keep the settled image layer for the next slide (only when it changed)
            let key = (self.base_gen as u64, l.ox.to_bits(), l.oy.to_bits(), l.z.to_bits(), c.w, c.h);
            if self.cfg.slide_ms > 0 && ready && self.pinch_at.is_none() && self.last_key != Some(key) {
                self.last_frame.clear();
                self.last_frame.extend_from_slice(c.buf);
                self.last_key = Some(key);
            }
            }
        } else {
            self.layout = None;
        }

        let sf = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
        let px = 15.0 * sf;
        let margin = 12.0 * sf;
        let info = if self.show_info { self.info_card() } else { None };
        let cur = self.cur_entry();
        let failed = cur.as_ref().and_then(|e| self.failed.borrow().get(e).cloned());
        let loading = cur.is_some() && failed.is_none() && !self.displaying_current();
        let name_line = cur.as_ref().map(|e| {
            let z = self.layout.map(|l| format!("  {:.0}%", l.z * 100.0)).unwrap_or_default();
            format!("{}  [{}/{}]{}", e.name(), self.index() + 1, self.len(), z)
        });

        let Some(mut t) = self.text.borrow_mut().take() else { return };
        let (cw, ch) = (c.w as f32, c.h as f32);

        // the "Drop an image" card fades in over 250 ms when it appears
        if self.playlist.is_none() {
            let at = *self.empty_at.get_or_insert_with(Instant::now);
            // the hotkeys card covers it: it fades out as that card fades in (seen through
            // the frost over the window glass, its text stayed readable)
            let own = crate::ui::fade_amount(at, true);
            let a = own * (1.0 - crate::ui::fade_amount(self.help_at, self.show_help));
            let hover = crate::ui::EMPTY_ACTIONS.map(|(id, ..)| self.hover_fx.iter().find(|(k, _)| *k == id).map_or(0.0, |(_, v)| *v));
            let mut rects = Vec::new();
            crate::ui::faded(c, a, |c| rects = crate::ui::empty_card(c, &mut t, cw, ch, sf, hover));
            self.empty_hits = rects.into_iter().zip(crate::ui::EMPTY_ACTIONS).map(|((x0, y0, x1, y1), (id, ..))| chrome::Hit { x0, y0, x1, y1, id }).collect();
            // its own fade-in only; the hotkeys card's fade asks for its own frames
            if own < 1.0 {
                self.request_redraw();
            }
        } else {
            self.empty_at = None;
            self.empty_hits.clear();
        }
        if self.picker.open {
            if let Some(pl) = &self.playlist {
                let cache = &self.cache;
                let mc = |e: &Entry| cache.borrow().get(e).map(|d| d.frames[0].img.clone());
                self.picker.draw(c, Some(&mut t), pl, pl.index, sf, &self.thumb_jobs, &mc);
            }
            *self.text.borrow_mut() = Some(t);
            if self.picker.bar_anim {
                self.request_redraw();
            }
            return;
        }
        if let (Some(m), Some(e)) = (&failed, &cur) {
            let lines = vec!["Can't open the image file".to_string(), e.name(), m.clone()];
            crate::ui::message_card(c, &mut t, sf, &lines, None, true);
        }
        if loading {
            let lines = vec!["Loading…".to_string()];
            crate::ui::message_card(c, &mut t, sf, &lines, Some((-margin, margin)), false);
        }
        // Chrome fades in and out over FADE_SECS: step each bar's opacity toward its target.
        let target = self.chrome_visibility(ch);
        let now = Instant::now();
        let dt = if self.fade_active { now.duration_since(self.fade_last).as_secs_f32() } else { 1.0 / 60.0 };
        self.fade_last = now;
        let step = |a: f32, on: bool| if on { (a + dt / FADE_SECS).min(1.0) } else { (a - dt / FADE_SECS).max(0.0) };
        self.fade = (step(self.fade.0, target.0), step(self.fade.1, target.1));
        let goal = |on: bool| if on { 1.0 } else { 0.0 };
        self.fade_active = self.fade.0 != goal(target.0) || self.fade.1 != goal(target.1);
        // Further fade frames come from about_to_wait, paced at 60 per second.
        let (show_top, show_bottom) = (self.fade.0 > 0.0, self.fade.1 > 0.0);
        // macOS: the green button, the Window menu and Esc enter and leave full screen
        // without the app; follow the window's own flag
        #[cfg(target_os = "macos")]
        if let Some(w) = self.window.clone() {
            let fs = crate::mac::is_fullscreen(&w);
            if self.fs_native != Some(fs) {
                self.fs_native = Some(fs);
                self.fullscreen = fs;
                self.fs_was_max = false;
                self.lights_on = None;
                crate::mac::fullscreen_titlebar(&w, fs);
                let want = self.glass_want;
                self.set_glass(want, false);
            }
        }
        // macOS: the real traffic lights come and go with the top chrome
        #[cfg(target_os = "macos")]
        if let Some(w) = &self.window {
            let on = self.fade.0 > 0.5 || self.fullscreen;
            if self.lights_on != Some(on) {
                self.lights_on = Some(on);
                crate::mac::traffic_lights(w, on);
            }
        }
        if let Some((title, secs)) = &info {
            // under the top chrome when that is visible
            let y = if target.0 { chrome::top_zone(sf) + margin } else { margin };
            crate::ui::info_card(c, &mut t, margin, y.round(), sf, title, secs);
        }
        if self.show_name {
            if let Some(n) = &name_line {
                let lines = vec![n.clone()];
                let h = (16.0 * 2.0 + 20.0) * sf;
                crate::ui::message_card(c, &mut t, sf, &lines, Some((margin, (ch - h - margin).round())), false);
            }
        }
        if let Some(m) = &self.modal {
            let (title, body, buttons) = m.content();
            let at = *self.modal_at.get_or_insert_with(Instant::now);
            let a = (at.elapsed().as_secs_f32() / 0.25).min(1.0);
            if a < 1.0 {
                self.request_redraw();
            }
            self.modal_hits = crate::ui::modal_card(c, &mut t, sf, &title, &body, &buttons, self.modal_hover, a);
        } else {
            self.modal_at = None;
        }
        if let Some((a, b)) = self.zoom_rect {
            // zoom rectangle: a light blue tint with a 1 px edge
            let (x0, y0) = (a.0.min(b.0) as f32, a.1.min(b.1) as f32);
            let (w, h) = ((a.0 - b.0).abs() as f32, (a.1 - b.1).abs() as f32);
            c.fill_rrect(x0, y0, w, h, 0.0, crate::ui::ACCENT, 0.15);
            c.stroke_rrect(x0, y0, w, h, 0.0, sf.max(1.0), crate::ui::ACCENT_LIGHT, 1.0);
        }
        // the hotkeys card fades in and out over 250 ms
        let ha = crate::ui::fade_amount(self.help_at, self.show_help);
        self.help_hit = None;
        if ha > 0.0 {
            let rows: Vec<(String, String)> = HELP.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect();
            let close_hover = self.hover_fx.iter().find(|(k, _)| *k == "help_close").map_or(0.0, |(_, v)| *v);
            // 16 px clear of the title strip and of the bottom bar
            let band = ((chrome::TITLE_H + 16.0) * sf, ch - (chrome::BAR_H + chrome::BAR_GAP + 16.0) * sf);
            let mut r = (0.0, 0.0, 0.0, 0.0);
            crate::ui::faded(c, ha, |c| r = crate::ui::help_card(c, &mut t, sf, "Hotkeys", &rows, close_hover, band));
            self.help_hit = self.show_help.then_some(chrome::Hit { x0: r.0, y0: r.1, x1: r.2, y1: r.3, id: "help_close" });
            if ha < 1.0 {
                self.request_redraw();
            }
        } else if !self.show_help && self.help_at.elapsed().as_secs_f32() < 0.3 {
            self.request_redraw();
        }
        self.hits.clear();
        // the empty card's actions, under the chrome; none while the hotkeys card covers it
        if !self.show_help {
            self.hits.extend(self.empty_hits.iter().copied());
        }
        let title = self.chrome_title();
        // Hover fades: each control's amount moves toward 1 while hovered and 0 after,
        // linearly over 250 ms, both ways.
        // capped: the first frame after an idle spell must not count the idle time
        let dt = self.hover_at.elapsed().as_secs_f32().min(0.05);
        self.hover_at = Instant::now();
        let step = (dt / 0.25).min(1.0);
        if let Some(h) = self.hover {
            if !self.hover_fx.iter().any(|(k, _)| *k == h) {
                self.hover_fx.push((h, 0.0));
            }
        }
        // Test hook: ARKIMAGE_TEST_HOVER=<control id> keeps that control hovered.
        if let Some(id) = std::env::var("ARKIMAGE_TEST_HOVER").ok().map(|v| &*Box::leak(v.into_boxed_str())) {
            if self.hover.is_none() {
                self.hover = Some(id);
            }
        }
        let hov = self.hover;
        for (k, a) in &mut self.hover_fx {
            let to = if Some(*k) == hov { 1.0 } else { 0.0 };
            *a = if (*a - to).abs() <= step { to } else { *a + step * (to - *a).signum() };
        }
        self.hover_fx.retain(|(k, a)| *a > 0.0 || Some(*k) == hov);
        if self.hover_fx.iter().any(|(k, a)| *a != if Some(*k) == hov { 1.0 } else { 0.0 }) {
            self.request_redraw();
        }
        // Tooltips: the same idea over 125 ms, for the bottom bar's buttons only.
        let tip_step = (dt / 0.125).min(1.0);
        let tip_hov = hov.filter(|h| h.starts_with("bar_") && *h != "bar_slider" || h.starts_with("tb_"));
        // moving from one button to the next hands the tooltip over at once, at the shown
        // opacity, so two never overlap; the 125 ms fade-out is for leaving the buttons
        if let Some(h) = tip_hov {
            if !self.tip_fx.iter().any(|(k, _)| *k == h) {
                let shown = self.tip_fx.iter().map(|(_, a)| *a).fold(0.0, f32::max);
                self.tip_fx.clear();
                self.tip_fx.push((h, shown));
            }
        }
        for (k, a) in &mut self.tip_fx {
            let to = if Some(*k) == tip_hov { 1.0 } else { 0.0 };
            *a = if (*a - to).abs() <= tip_step { to } else { *a + tip_step * (to - *a).signum() };
        }
        self.tip_fx.retain(|(k, a)| *a > 0.0 || Some(*k) == tip_hov);
        if self.tip_fx.iter().any(|(k, a)| *a != if Some(*k) == tip_hov { 1.0 } else { 0.0 }) {
            self.request_redraw();
        }
        let tip_fx = self.tip_fx.clone();
        let hover_fx = self.hover_fx.clone();
        let anim_fmt = self.anim_format();
        let st = chrome::State {
            sf,
            title: &title,
            show_top,
            show_bottom,
            maximized: self.window.as_ref().map(|w| is_maximized(&w)).unwrap_or(false),
            lock_bottom: self.cfg.lock_bottom,
            hover_fx: &hover_fx,
            tip_fx: &tip_fx,
            info_on: self.show_info,
            index: self.index(),
            len: self.len(),
            anim: self.anim_frames().map(|n| (self.frame_idx.min(n - 1), n)),
            anim_fmt: &anim_fmt,
            anim_speed: self.anim_speed,
            anim_fps: self.anim_fps(),
            page_drag: self.page_drag(),
            archive: self.archive_open(),
            rtl: self.rtl_on(),
            flip_h: self.view.flip_h,
            flip_v: self.view.flip_v,
        };
        // Keep the rows under a partly faded bar, draw the bar, then blend the two.
        let top_rows = ((chrome::top_zone(sf) + 2.0).ceil() as usize).min(c.h);
        let anim_h = if st.anim.is_some() { chrome::ANIM_H + chrome::BAR_GAP } else { 0.0 };
        let bot_from = c.h.saturating_sub(((chrome::BAR_H + chrome::BAR_GAP + anim_h + 6.0) * sf).ceil() as usize);
        let saved_top = (show_top && self.fade.0 < 1.0).then(|| c.buf[..top_rows * c.w].to_vec());
        let saved_bot = (show_bottom && self.fade.1 < 1.0).then(|| c.buf[bot_from * c.w..].to_vec());
        let mut hits = std::mem::take(&mut self.hits);
        chrome::draw(c, Some(&mut t), &st, &mut hits);
        self.hits = hits;
        // the hotkeys card's close button, over everything
        if let Some(h) = self.help_hit {
            self.hits.push(h);
        }
        for (saved, off, a) in [(saved_top, 0, self.fade.0), (saved_bot, bot_from * c.w, self.fade.1)] {
            if let Some(saved) = saved {
                let a = (a * 255.0) as u32;
                for (i, s) in saved.iter().enumerate() {
                    let p = &mut c.buf[off + i];
                    *p = render::mix(*s, *p, a);
                }
            }
        }
        self.chrome_shown = target;
        if let Some((m, _)) = &self.toast {
            let lines = vec![m.clone()];
            let (pw, ph) = t.panel_size(&lines, px * 1.1);
            t.panel(c, ((cw - pw) / 2.0).round(), (ch * 0.78 - ph / 2.0).round(), &lines, px * 1.1);
        }
        *self.text.borrow_mut() = Some(t);
    }

    // ---------- input ----------

    /// Run mouse slot `i` (bindings::MOUSE_SLOTS order) if it is enabled.
    fn mouse_bind(&mut self, el: &ActiveEventLoop, i: usize) {
        let b = self.cfg.mouse[i].clone();
        if b.on {
            self.run_action(el, &b.action);
        }
    }

    /// Run a bindable action by id (bindings::ACTIONS).
    fn run_action(&mut self, el: &ActiveEventLoop, id: &str) {
        match id {
            "none" => {}
            "fullscreen" => self.toggle_fullscreen(),
            "maximize" => self.toggle_maximize(),
            "minimize" => {
                if let Some(w) = &self.window {
                    w.set_minimized(true);
                }
            }
            "scroll_up" => self.scroll(-1.0),
            "scroll_down" => self.scroll(1.0),
            "scroll_left" | "scroll_right" => {
                if let Some(l) = self.layout {
                    self.view.pan_x += if id == "scroll_left" { -1.0 } else { 1.0 } * l.ww * 0.15;
                    self.request_redraw();
                }
            }
            "context" => {
                let (x, y) = (self.cursor.0 as f32, self.cursor.1 as f32);
                // a menu at the cursor never takes a button's placement
                self.popup_select = None;
                self.at_pointer = true;
                self.show_menu(el, "context", x, y);
                self.at_pointer = false;
            }
            other => self.menu_action(el, other),
        }
    }

    /// Show an update result: in Settings when it is open, else over the image.
    /// A check the user did not ask for shows only a ready update.
    pub fn on_update(&mut self, o: &crate::update::Outcome, manual: bool) {
        use crate::{install::Card, update::Outcome};
        use crate::settings::Upd;
        // the Version row in Settings shows every step of a check
        if let Some(s) = &mut self.settings {
            s.set_upd(match o {
                Outcome::Found(v) | Outcome::Ready(v) => Upd::Found(v.clone()),
                Outcome::UpToDate => Upd::Latest,
                Outcome::Failed(e) => Upd::Failed(e.clone()),
            });
        }
        let card = match o {
            Outcome::Found(_) => return,
            // the new exe is in place: restart now; the session reopens in the new version
            Outcome::Ready(v) => {
                crate::trace(&format!("update {v} ready: restarting"));
                crate::update::request_restart();
                return;
            }
            // a check from Settings answers in its Version row
            _ if self.settings.is_some() => return,
            Outcome::UpToDate if manual => Card::UpToDate,
            Outcome::Failed(e) if manual => Card::UpdateFailed(e.clone()),
            _ => return,
        };
        if let Some(s) = &mut self.settings {
            s.card = Some(card);
            s.card_hover = None;
            s.window.request_redraw();
        } else {
            self.modal = Some(card);
            self.modal_hover = None;
            self.request_redraw();
        }
    }

    pub fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// Show the install card (first run, or Settings > General).
    pub fn show_install(&mut self, reinstall: bool) {
        self.modal = Some(crate::install::Card::Install(reinstall));
        self.modal_hover = None;
        self.request_redraw();
    }

    /// Run a modal card's button: 0 is the primary (rightmost), 1 the other.
    fn modal_button(&mut self, i: usize) {
        let Some(m) = self.modal.take() else { return };
        self.modal_hover = None;
        self.modal = m.press(i);
        self.request_redraw();
    }

    fn on_key(&mut self, el: &ActiveEventLoop, code: Option<KeyCode>, ch: Option<char>) {
        // a modal card takes the keyboard: Enter is its primary button, Esc its other
        if self.modal.is_some() {
            match code {
                Some(KeyCode::Enter | KeyCode::NumpadEnter) => self.modal_button(0),
                Some(KeyCode::Escape) => self.modal_button(1),
                _ => {}
            }
            let _ = (el, ch);
            return;
        }
        if self.picker.open {
            if let Some(code) = code {
                self.picker_key(code);
            }
            return;
        }
        // Custom hotkeys first; they override the built-in keys.
        if let Some(code) = code {
            let text = crate::bindings::key_text(self.mods, code);
            let hit = self.cfg.keys.iter().find(|b| b.on && b.key == text && b.action != "none").map(|b| b.action.clone());
            if let Some(a) = hit {
                self.run_action(el, &a);
                return;
            }
        }
        let p = self.primary();
        let alt = self.mods.alt_key();
        let sh = self.mods.shift_key();
        if !alt {
            match ch {
                Some('+') | Some('=') => return if p { self.zoom_pct(1.0) } else { self.zoom_step(true, None) },
                Some('-') | Some('_') => return if p { self.zoom_pct(-1.0) } else { self.zoom_step(false, None) },
                Some('*') => return self.zoom_to(1.0, None),
                Some('[') | Some('{') => return self.container_step(false, true),
                Some(']') | Some('}') => return self.container_step(true, true),
                _ => {}
            }
        }
        let Some(code) = code else { return };
        use KeyCode as K;
        match code {
            K::NumpadAdd => self.zoom_step(true, None),
            K::NumpadSubtract => self.zoom_step(false, None),
            K::NumpadMultiply => self.zoom_to(1.0, None),

            K::ArrowRight if alt => self.rotate(1),
            K::ArrowLeft if alt => self.rotate(3),
            K::ArrowDown if alt => self.rotate(2),
            K::ArrowUp if alt => {
                self.view.rotation = 0;
                self.rotate(0);
            }
            K::ArrowRight | K::Space => self.next(),
            K::ArrowLeft | K::Backspace => self.prev(),
            K::PageDown if p && alt => self.random(),
            K::PageUp if p && alt => self.random_back(),
            K::PageDown if p => self.step(10, true),
            K::PageUp if p => self.step(-10, true),
            K::PageDown => self.next(),
            K::PageUp => self.prev(),
            K::Home => self.go_to(0),
            K::End => {
                let l = self.len();
                self.go_to(l.saturating_sub(1));
            }
            K::ArrowUp => self.scroll(-1.0),
            K::ArrowDown => self.scroll(1.0),

            K::Enter | K::NumpadEnter if alt => self.toggle_fullscreen(),
            K::Enter | K::NumpadEnter if p => self.reveal(),
            K::Enter | K::NumpadEnter => self.open_picker(),
            K::F11 => self.toggle_fullscreen(),
            K::F2 => self.open_dialog(),
            K::KeyO if p => self.open_dialog(),
            K::KeyF if p => {
                self.view.flip_v = !self.view.flip_v;
                self.invalidate();
            }
            K::KeyF => self.open_folder_dialog(),
            K::F4 => self.close(),

            K::KeyU => {
                self.cfg.filter = 0;
                self.invalidate();
                self.msg("No Filter");
            }
            K::KeyI if p => {
                self.view.negative = !self.view.negative;
                self.invalidate();
            }
            K::KeyI => {
                self.cfg.filter = 1;
                self.invalidate();
                self.msg("Filter: Soften");
            }
            K::KeyS if !p => {
                self.cfg.filter = 2;
                self.invalidate();
                self.msg("Filter: Soften+Sharpen");
            }
            K::KeyG if p => {
                self.view.gamma = !self.view.gamma;
                self.invalidate();
            }
            K::KeyM if p => {
                self.view.flip_h = !self.view.flip_h;
                self.invalidate();
            }
            K::KeyZ if !alt => {
                self.cfg.stretch_small = !self.cfg.stretch_small;
                self.view.user_zoom = None;
                self.invalidate();
                self.msg(if self.cfg.stretch_small { "Stretch Small Images: On" } else { "Stretch Small Images: Off" });
            }
            K::KeyK if p => {
                self.view.zoom_fixed = !self.view.zoom_fixed;
                self.msg(if self.view.zoom_fixed { "Fix zoom in/out: On" } else { "Fix zoom in/out: Off" });
            }

            K::Digit0 | K::Numpad0 => self.set_mode(0),
            K::Digit9 | K::Numpad9 => self.set_mode(1),
            K::Digit8 | K::Numpad8 => self.set_mode(2),

            K::Tab if sh => {
                self.show_name = !self.show_name;
                self.request_redraw();
            }
            K::Tab => {
                self.show_info = !self.show_info;
                self.request_redraw();
            }
            K::KeyC if p && sh => self.copy_info(),
            K::KeyC if p => self.copy_image(),
            K::Delete => self.delete_current(),
            K::Insert if p => self.to_photo_folder(1, true),
            K::Insert if sh => self.to_photo_folder(2, false),
            K::Insert => self.to_photo_folder(1, false),
            K::KeyE if p => self.open_editor(),
            K::F5 if sh => self.reload_config(),
            K::F5 => self.open_settings(el, 0),
            K::F1 => {
                self.show_help = !self.show_help;
                self.help_at = Instant::now();
                self.request_redraw();
            }
            K::KeyA if p => {
                self.on_top = !self.on_top;
                if let Some(w) = &self.window {
                    w.set_window_level(if self.on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
                }
                self.msg(if self.on_top { "Always On Top: On" } else { "Always On Top: Off" });
            }
            // ` locks the top menu, ~ (Shift+`) locks the bottom control bar, as in Honeyview.
            K::Backquote if sh => {
                self.cfg.lock_bottom = !self.cfg.lock_bottom;
                self.request_redraw();
            }
            K::Backquote => {
                self.cfg.lock_top = !self.cfg.lock_top;
                self.request_redraw();
            }
            K::Escape => {
                if self.show_help {
                    self.show_help = false;
                    self.help_at = Instant::now();
                    self.request_redraw();
                } else if self.fullscreen {
                    self.toggle_fullscreen();
                } else if self.cfg.esc_exits {
                    self.exit(el);
                }
            }
            K::KeyX if !p => self.exit(el),
            K::KeyQ | K::KeyW if p => self.exit(el),
            _ => {}
        }
    }

    // ---------- chrome ----------

    fn sf(&self) -> f32 {
        self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32
    }

    /// The active filter preset.
    fn preset(&self) -> crate::filters::Preset {
        crate::filters::get(&self.cfg.filter_presets, self.cfg.filter)
    }

    /// Resize with the active preset, then sharpen if it asks for it.
    fn scale_with_preset(&self, base: &RgbaImage, sw: u32, sh: u32) -> RgbaImage {
        let p = self.preset();
        let mut img = render::resize(base, sw, sh, p.resize_code(self.cfg.interp));
        render::sharpen(&mut img, p.amount as f32 / 100.0, p.radius as f32 / 100.0, p.threshold);
        img
    }

    /// (top, bottom) chrome visible. Honeyview shows each bar while the cursor is near its edge.
    fn chrome_visibility(&self, ch: f32) -> (bool, bool) {
        let sf = self.sf();
        let y = self.cursor.1 as f32;
        let empty = self.playlist.is_none();
        let menu_open = false;
        let top = self.cfg.lock_top || empty || menu_open || (self.cursor_in && y < chrome::top_zone(sf));
        // macOS full screen: the system's own title bar slides down at the top edge
        let top = top && !(cfg!(target_os = "macos") && self.fullscreen);
        let anim = self.anim_frames().is_some();
        let bottom = self.cfg.lock_bottom || empty || self.slider_drag || self.anim_drag || (self.cursor_in && y > ch - chrome::bottom_zone(sf, anim));
        (top, bottom)
    }

    fn chrome_title(&self) -> String {
        match &self.playlist {
            Some(pl) if !pl.entries.is_empty() => {
                let folder = pl.container.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                format!("{folder} > {} [{}/{}] - ArkImage", pl.get(pl.index).map(|e| e.name()).unwrap_or_default(), pl.index + 1, pl.entries.len())
            }
            _ => "ArkImage".to_string(),
        }
    }

    fn hit_at(&self, p: (f64, f64)) -> Option<chrome::Hit> {
        let (x, y) = (p.0 as f32, p.1 as f32);
        self.hits.iter().rev().find(|h| h.contains(x, y)).copied()
    }

    fn resize_edge(&self) -> Option<ResizeDirection> {
        let w = self.window.as_ref()?;
        if self.fullscreen || is_maximized(&w) {
            return None;
        }
        let sz = w.inner_size();
        let e = 5.0 * self.sf() as f64;
        let (x, y) = self.cursor;
        let (l, r, t, b) = (x < e, x > sz.width as f64 - e, y < e, y > sz.height as f64 - e);
        use ResizeDirection as D;
        Some(match (l, r, t, b) {
            (true, _, true, _) => D::NorthWest,
            (_, true, true, _) => D::NorthEast,
            (true, _, _, true) => D::SouthWest,
            (_, true, _, true) => D::SouthEast,
            (true, _, _, _) => D::West,
            (_, true, _, _) => D::East,
            (_, _, true, _) => D::North,
            (_, _, _, true) => D::South,
            _ => return None,
        })
    }

    fn on_cursor_moved(&mut self) {
        let hover = self.hit_at(self.cursor).map(|h| h.id).filter(|id| !matches!(*id, "title" | "toolbar" | "bar"));
        let ch = self.window.as_ref().map(|w| w.inner_size().height as f32).unwrap_or(0.0);
        let vis = self.chrome_visibility(ch);
        if hover != self.hover || vis != self.chrome_shown {
            self.hover = hover;
            self.request_redraw();
        }
        if let Some(w) = &self.window {
            use ResizeDirection as D;
            let icon = match self.resize_edge() {
                Some(D::East | D::West) => CursorIcon::EwResize,
                Some(D::North | D::South) => CursorIcon::NsResize,
                Some(D::NorthEast | D::SouthWest) => CursorIcon::NeswResize,
                Some(D::NorthWest | D::SouthEast) => CursorIcon::NwseResize,
                None if self.hover.is_some() => CursorIcon::Pointer,
                None => CursorIcon::Default,
            };
            w.set_cursor(icon);
        }
    }

    /// While the page track is dragged: (cursor x, cursor y, 0-1 along the track).
    fn page_drag(&self) -> Option<(f32, f32, f32)> {
        if !self.slider_drag || self.len() < 2 {
            return None;
        }
        let sz = self.window.as_ref()?.inner_size();
        let (sx, _, sw, sh) = chrome::slider_rect(sz.width as f32, sz.height as f32, self.sf());
        let f = ((self.cursor.0 as f32 - sx - sh / 2.0) / (sw - sh)).clamp(0.0, 1.0);
        Some((self.cursor.0 as f32, self.cursor.1 as f32, f))
    }

    fn slider_seek(&mut self) {
        let Some(w) = &self.window else { return };
        let sz = w.inner_size();
        let (sx, _, sw, sh) = chrome::slider_rect(sz.width as f32, sz.height as f32, self.sf());
        let len = self.len();
        if len == 0 {
            return;
        }
        let f = ((self.cursor.0 as f32 - sx - sh / 2.0) / (sw - sh)).clamp(0.0, 1.0);
        let i = (f * (len - 1) as f32).round() as usize;
        if i != self.index() {
            self.go_to(i);
        }
    }

    fn on_chrome_click(&mut self, el: &ActiveEventLoop, h: chrome::Hit, right: bool) {
        let n = if right { 10 } else { 1 };
        match h.id {
            "empty_file" => self.open_dialog(),
            "empty_folder" => self.open_folder_dialog(),
            "empty_help" | "help_close" => {
                self.show_help = !self.show_help;
                self.help_at = Instant::now();
                self.request_redraw();
            }
            "title" => {
                let now = Instant::now();
                let dbl = self.last_click.map(|(t, _)| now - t < Duration::from_millis(400)).unwrap_or(false);
                self.last_click = Some((now, self.cursor));
                if dbl {
                    self.last_click = None;
                    self.toggle_maximize();
                } else if let Some(w) = &self.window {
                    let _ = w.drag_window();
                }
            }
            "win_close" => self.exit(el),
            "win_max" => self.toggle_maximize(),
            "win_min" => {
                if let Some(w) = &self.window {
                    w.set_minimized(true);
                }
            }
            "win_full" | "tb_full" => self.toggle_fullscreen(),
            "tb_exif" => {
                self.show_info = !self.show_info;
                self.request_redraw();
            }
            "tb_lock" => {
                self.cfg.lock_top = !self.cfg.lock_top;
                self.request_redraw();
            }
            "bar_lock" => {
                self.cfg.lock_bottom = !self.cfg.lock_bottom;
                self.request_redraw();
            }
            "tb_view" | "tb_edit" | "tb_copyto" | "bar_menu" => {
                // above the button: the main menu left-aligned with the button's left edge,
                // the others right-aligned with its right edge (mac::popup_over)
                let kind = if h.id == "bar_menu" { "context" } else { &h.id[3..] };
                self.popup_select = Some(((h.x1 - h.x0) as f64, h.id == "bar_menu"));
                self.show_menu(el, kind, h.x0, h.y0);
            }
            "bar_open" => self.open_dialog(),
            "bar_grid" => self.open_picker(),
            "bar_fliph" => self.menu_action(el, "fliph"),
            "bar_flipv" => self.menu_action(el, "flipv"),
            "bar_rtl" => self.toggle_rtl(),
            "bar_slider" => {
                self.slider_drag = true;
                self.slider_seek();
            }
            "anim_speed" => {
                // above the pill, at least as wide, right-aligned
                self.popup_select = Some(((h.x1 - h.x0) as f64, false));
                self.show_menu(el, "speed", h.x0, h.y0)
            }
            "anim_slider" => {
                self.anim_drag = true;
                self.anim_seek();
            }
            "bar_zin" => self.zoom_step(true, None),
            "bar_zout" => self.zoom_step(false, None),
            "bar_rotl" => self.rotate(3),
            "bar_rotr" => self.rotate(1),
            "bar_prev" => self.step(-n, n > 1),
            "bar_next" => self.step(n, n > 1),
            _ => {}
        }
    }

    fn menu_items(&mut self, kind: &str) -> Vec<MenuEntry> {
        use MenuEntry::{Check as C, Item as I, Sep as S, Sub};
        let cfg = &self.cfg;
        let v = &self.view;
        let has = self.playlist.is_some();
        let view_items = || {
            vec![
                C("stretch".into(), "Stretch Small Images".into(), "Z", cfg.stretch_small),
                S,
                C("mode0".into(), "Original Size (100%)".into(), "0", cfg.view_mode == 0),
                C("mode1".into(), "Fit in Window".into(), "9", cfg.view_mode == 1),
                C("mode2".into(), "Fit to Width".into(), "8", cfg.view_mode == 2),
            ]
        };
        match kind {
            "view" => view_items(),
            // playback speed: 0.25x to 4x in 0.25 steps
            "speed" => (1..=16)
                .map(|q| {
                    let v = q as f32 / 4.0;
                    C(format!("spd:{q}"), format!("{}\u{d7}", chrome::fmt_speed(v)), "", (self.anim_speed - v).abs() < 0.01)
                })
                .collect(),
            "edit" => vec![
                I("clip".into(), "Copy to Clipboard".into(), "Ctrl+C", has),
                I("editor".into(), "Open with Image Editor".into(), "Ctrl+E", has),
            ],
            "copyto" => vec![
                I("copy1".into(), "Copy to Photo Folder 1".into(), "Insert", has),
                I("move1".into(), "Move to Photo Folder 1".into(), "Ctrl+Insert", has),
                I("copy2".into(), "Copy to Photo Folder 2".into(), "Shift+Insert", has),
                S,
                I("pfset".into(), "Photo Folder Settings".into(), "", true),
                I("pfopen".into(), "Open Photo Folder 1".into(), "", !cfg.photo_folder1.is_empty()),
            ],
            _ => vec![
                I("open".into(), "Open".into(), "F2", true),
                I("open_folder".into(), "Open Folder".into(), "F", true),
                I("pageselect".into(), "Select Image".into(), "Enter", has),
                I("close".into(), "Close".into(), "F4", has),
                I("reveal".into(), if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in Windows Explorer" }.into(), "Ctrl+Enter", has),
                I("reveal_all".into(), if cfg!(target_os = "macos") { "Show Open Images from Same Folder in Finder" } else { "Select Open Images from Same Folder in Explorer" }.into(), "", has),
                S,
                // every preset in one list, the three with hotkeys first (U, I, S)
                Sub("Filters".into(), {
                    let keys = ["U", "I", "S"];
                    let mut v: Vec<MenuEntry> = crate::filters::all(&cfg.filter_presets)
                        .into_iter()
                        .enumerate()
                        .map(|(i, p)| C(format!("f{i}"), p.name, keys.get(i).copied().unwrap_or(""), cfg.filter as usize == i))
                        .collect();
                    v.push(S);
                    v.push(I("fedit".into(), "Edit Filters...".into(), "", true));
                    v
                }),
                S,
                Sub("Navigation".into(), vec![
                    I("next".into(), "Next Image".into(), "->", has),
                    I("prev".into(), "Previous Image".into(), "<-", has),
                    I("first".into(), "First Image".into(), "Home", has),
                    I("last".into(), "Last Image".into(), "End", has),
                    I("next10".into(), "Next 10th Image".into(), "Ctrl+PageDown", has),
                    I("prev10".into(), "Previous 10th Image".into(), "Ctrl+PageUp", has),
                    I("rnext".into(), "Next Random Image".into(), "Ctrl+Alt+PageDown", has),
                    I("rprev".into(), "Previous Random Image".into(), "Ctrl+Alt+PageUp", has),
                    I("nextdir".into(), "Next Folder/Compressed File".into(), "]", has),
                    I("prevdir".into(), "Previous Folder/Compressed File".into(), "[", has),
                ]),
                Sub("Image Effect".into(), vec![
                    C("neg".into(), "Reverse Image".into(), "Ctrl+I", v.negative),
                    C("gamma".into(), "Gamma Compensation".into(), "Ctrl+G", v.gamma),
                    C("flipv".into(), "Flip Vertical".into(), "Ctrl+F", v.flip_v),
                    C("fliph".into(), "Flip Horizontal".into(), "Ctrl+M", v.flip_h),
                ]),
                Sub("Sort by".into(), vec![
                    C("sort0".into(), "Name".into(), "", cfg.sort == 0),
                    C("sort1".into(), "Date Modified".into(), "", cfg.sort == 1),
                    C("sort2".into(), "Size".into(), "", cfg.sort == 2),
                    C("sort3".into(), "Type".into(), "", cfg.sort == 3),
                    S,
                    C("sortdesc".into(), "Descending".into(), "", cfg.sort_desc),
                ]),
                S,
                Sub("View Image".into(), view_items()),
                Sub("Zoom".into(), vec![
                    I("zin".into(), "Zoom In".into(), "+", has),
                    I("zout".into(), "Zoom Out".into(), "-", has),
                    C("zfix".into(), "Fix".into(), "Ctrl+K", v.zoom_fixed),
                    I("z100".into(), "100%".into(), "*", has),
                    I("zin1".into(), "Zoom In (1%)".into(), "Ctrl++", has),
                    I("zout1".into(), "Zoom Out (1%)".into(), "Ctrl+-", has),
                ]),
                Sub("Rotate/Flip".into(), vec![
                    C("rot0".into(), "Do Not Rotate".into(), "Alt+Up", v.rotation == 0),
                    C("rotl".into(), "Rotate 90° Counter-Clockwise".into(), "Alt+Left", v.rotation == 3),
                    C("rotr".into(), "Rotate 90° Clockwise".into(), "Alt+Right", v.rotation == 1),
                    C("rot180".into(), "Rotate 180°".into(), "Alt+Down", v.rotation == 2),
                    S,
                    C("fliph".into(), "Flip Horizontal".into(), "Ctrl+M", v.flip_h),
                    C("flipv".into(), "Flip Vertical".into(), "Ctrl+F", v.flip_v),
                ]),
                // on: 250 ms; off: none
                C("slide_toggle".into(), "Slide Animation".into(), "", cfg.slide_ms > 0),
                S,
                C("info".into(), "File/EXIF Information".into(), "Tab", self.show_info),
                I("delete".into(), "Delete".into(), "Del", has),
                I("copy1".into(), "Copy to Photo Folder".into(), "Ins", has),
                I("editor".into(), "Open with Image Editor".into(), "Ctrl+E", has),
                I("clip".into(), "Copy to Clipboard".into(), "Ctrl+C", has),
                S,
                I("config".into(), "Settings".into(), "F5", true),
                I("closeall".into(), "Close All Instances".into(), "", true),
                I("exit".into(), "Exit".into(), "X", true),
            ],
        }
    }

    fn show_menu(&mut self, el: &ActiveEventLoop, kind: &str, x: f32, y: f32) {
        let items = self.menu_items(kind);
        let Some(w) = self.window.clone() else { return };
        self.popup(el, &w, items, x, y, kind);
    }

    /// True when a click must be dropped: it came right after a popup closed, with no
    /// cursor move since. It is the click that closed the popup, and the window holds a
    /// cursor position from before the popup opened (the popup takes all moves).
    fn stale_click(&self) -> bool {
        self.popup_closed.as_ref().is_some_and(|(t, _, moved)| !moved && t.elapsed() < Duration::from_millis(500))
    }

    /// Show a native popup menu over `w` at (x, y) physical px, then run what was chosen.
    /// `opener` names the control; a click on it within 300 ms of its popup closing
    /// closes rather than reopens.
    fn popup(&mut self, el: &ActiveEventLoop, w: &Arc<Window>, items: Vec<MenuEntry>, x: f32, y: f32, opener: &str) {
        if self.popup_closed.as_ref().is_some_and(|(t, o, _)| o == opener && t.elapsed() < Duration::from_millis(300)) {
            // a dropped opener must not leave its placement for the next menu, or a later
            // right-click opens above and beside the cursor as if over the button
            self.popup_select = None;
            self.below_w = None;
            return;
        }
        self.popup_show(el, w, items, x, y);
        self.popup_closed = Some((Instant::now(), opener.to_string(), false));
    }

    fn popup_show(&mut self, el: &ActiveEventLoop, w: &Arc<Window>, items: Vec<MenuEntry>, x: f32, y: f32) {
        use muda::ContextMenu;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // Windows: native dark Win32 menus (popup_win). Bar menus open above the button,
        // 4 px over it; a settings dropdown opens 2 px under the field, right edges aligned.
        #[cfg(windows)]
        if let Ok(h) = w.window_handle() {
            if let RawWindowHandle::Win32(h) = h.as_raw() {
                use crate::popup_win::{show_above, show_below, Edge};
                let hwnd = h.hwnd.get();
                let sf = w.scale_factor() as f32;
                let chosen = if let Some((pw, left)) = self.popup_select.take() {
                    let pw = pw as f32;
                    let (ax, edge) = if left { (x, Edge::Left) } else { (x + pw, Edge::Right) };
                    // menu text is 9 pt Segoe UI: a figure space is about 7 px at 100%, and the
                    // check column and margins take about 64 px; the main menu needs no minimum
                    let min_chars = if left { 0 } else { ((pw / sf - 64.0) / 7.0).ceil().max(0.0) as usize };
                    show_above(hwnd, items, ax as i32, (y - 4.0 * sf).round() as i32, edge, min_chars)
                } else if let Some(bw) = self.below_w.take() {
                    show_below(hwnd, items, (x + bw as f32) as i32, (y + (2.0 * sf).round()) as i32, Edge::Right)
                } else {
                    show_below(hwnd, items, x as i32, y as i32, Edge::Left)
                };
                if let Some(id) = chosen {
                    self.menu_action(el, &id);
                }
                self.request_redraw();
                return;
            }
        }
        let menu = muda::Menu::new();
        for b in build_menu(items) {
            let _ = menu.append(b.as_ref());
        }
        let pos = muda::dpi::Position::Physical(muda::dpi::PhysicalPosition::new(x as i32, y as i32));
        if let Ok(h) = w.window_handle() {
            match h.as_raw() {
                #[cfg(windows)]
                RawWindowHandle::Win32(h) => unsafe {
                    menu.show_context_menu_for_hwnd(h.hwnd.get(), Some(pos));
                },
                #[cfg(target_os = "macos")]
                RawWindowHandle::AppKit(h) => unsafe {
                    match self.popup_select.take() {
                        // a pop-up button: the chosen item opens over the control
                        Some((pw, left)) => crate::mac::popup_over(menu.ns_menu(), h.ns_view.as_ptr() as _, x as f64 / w.scale_factor(), y as f64 / w.scale_factor(), pw / w.scale_factor(), left),
                        None if self.below_w.is_some() => {
                            let bw = self.below_w.take().unwrap_or(0.0);
                            let sf = w.scale_factor();
                            crate::mac::menu_below(menu.ns_menu(), h.ns_view.as_ptr() as _, x as f64 / sf, y as f64 / sf, bw / sf);
                        }
                        None if self.at_pointer => crate::mac::context_menu_at_pointer(menu.ns_menu(), h.ns_view.as_ptr() as _),
                        None => {
                            let _ = pos;
                            crate::mac::menu_below(menu.ns_menu(), h.ns_view.as_ptr() as _, x as f64 / w.scale_factor(), y as f64 / w.scale_factor(), 0.0);
                        }
                    }
                },
                _ => {}
            }
        }
        self.drain_menu_events(el);
        self.request_redraw();
    }

    // ---------- configuration dialog ----------

    /// For the ARKIMAGE_TEST_SETTINGS hook.
    pub fn test_open_settings(&mut self, el: &ActiveEventLoop) {
        self.open_settings(el, 0);
    }

    /// For the ARKIMAGE_TEST_CHOICE hook: open the first dropdown in Settings.
    pub fn test_choice(&mut self, el: &ActiveEventLoop) {
        if let Some(a) = self.settings.as_ref().and_then(|s| s.first_choice()) {
            self.settings_act(el, a);
        }
    }

    fn open_settings(&mut self, el: &ActiveEventLoop, tab: usize) {
        if let Some(s) = &mut self.settings {
            s.tab = tab;
            s.window.focus_window();
            s.window.request_redraw();
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_title("Settings")
            .with_window_icon(crate::icon::window_icon())
            .with_inner_size(LogicalSize::new(settings::W, settings::H))
            .with_resizable(false)
            .with_enabled_buttons(winit::window::WindowButtons::CLOSE);
        if let Some(main) = &self.window {
            // centre over the main window
            let (mp, ms) = (main.outer_position().unwrap_or_default(), main.outer_size());
            let sf = main.scale_factor();
            let (dw, dh) = ((settings::W * sf) as i32, (settings::H * sf) as i32);
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                mp.x + (ms.width as i32 - dw) / 2,
                mp.y + (ms.height as i32 - dh) / 2,
            ));
            #[cfg(windows)]
            {
                use winit::platform::windows::WindowAttributesExtWindows;
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(h) = main.window_handle() {
                    if let RawWindowHandle::Win32(h) = h.as_raw() {
                        attrs = attrs.with_owner_window(h.hwnd.get());
                    }
                }
            }
        }
        let Ok(w) = el.create_window(attrs) else { return };
        dark_title_bar(&w);
        let backdrop = mica(&w);
        crate::trace(&format!("configuration mica: {backdrop}"));
        self.settings = settings::Dialog::new(Arc::new(w), self.cfg.clone(), tab);
        if let Some(s) = &mut self.settings {
            s.mica = backdrop;
        }
    }

    fn settings_redraw(&self) {
        if let Some(s) = &self.settings {
            s.window.request_redraw();
        }
    }

    fn settings_event(&mut self, el: &ActiveEventLoop, ev: WindowEvent) {
        // a key held when the dialog opened (synthetic press) must not cancel it
        if let WindowEvent::KeyboardInput { is_synthetic: true, .. } = ev {
            return;
        }
        // the install card takes the mouse and keyboard while it is up
        if let Some(s) = self.settings.as_mut().filter(|s| s.card.is_some()) {
            let press = |s: &mut settings::Dialog, i: usize| {
                if let Some(card) = s.card.take() {
                    s.card = card.press(i);
                    s.card_hover = None;
                }
                s.window.request_redraw();
            };
            match &ev {
                WindowEvent::CursorMoved { position, .. } => {
                    s.cursor = (position.x as f32, position.y as f32);
                    let (x, y) = s.cursor;
                    let h = s.card_hits.iter().position(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3);
                    if h != s.card_hover {
                        s.card_hover = h;
                        s.window.request_redraw();
                    }
                    return;
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    let (x, y) = s.cursor;
                    if let Some(i) = s.card_hits.iter().position(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3) {
                        press(s, i);
                    }
                    return;
                }
                WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => press(s, 0),
                        PhysicalKey::Code(KeyCode::Escape) => press(s, 1),
                        _ => {}
                    }
                    return;
                }
                WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::KeyboardInput { .. } => return,
                _ => {}
            }
        }
        match ev {
            WindowEvent::CloseRequested => self.settings_cancel(),
            WindowEvent::RedrawRequested => {
                let mut t = self.text.borrow_mut().take();
                if let Some(s) = &mut self.settings {
                    s.render(&self.cfg, t.as_mut());
                }
                *self.text.borrow_mut() = t;
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.settings_redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(s) = &mut self.settings {
                    s.cursor = (position.x as f32, position.y as f32);
                    let h = s.hit(s.cursor.0, s.cursor.1);
                    if h != s.hover {
                        s.hover = h;
                        s.window.request_redraw();
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(s) = &mut self.settings {
                    s.hover = None;
                    s.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // pages scroll: 48 px (one card) a notch
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 48.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / self.settings.as_ref().map(|s| s.window.scale_factor() as f32).unwrap_or(1.0),
                };
                if let Some(s) = &mut self.settings {
                    s.scroll_by(dy);
                    let h = s.hit(s.cursor.0, s.cursor.1);
                    s.hover = h;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let act = self.settings.as_ref().and_then(|s| s.hit(s.cursor.0, s.cursor.1));
                if let Some(a) = act {
                    self.settings_act(el, a);
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.settings.as_ref().and_then(|s| s.capturing).is_some() =>
            {
                // Hotkey capture: Esc cancels, Backspace/Delete clears, modifiers alone wait.
                let Some(i) = self.settings.as_ref().and_then(|s| s.capturing) else { return };
                let PhysicalKey::Code(code) = event.physical_key else { return };
                if crate::bindings::is_modifier(code) {
                    return;
                }
                match code {
                    KeyCode::Escape => {}
                    KeyCode::Backspace | KeyCode::Delete => self.cfg.keys[i].key.clear(),
                    _ => {
                        self.cfg.keys[i].key = crate::bindings::key_text(self.mods, code);
                        self.cfg.keys[i].on = true;
                    }
                }
                if let Some(s) = &mut self.settings {
                    s.capturing = None;
                }
                self.settings_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && self.settings.as_ref().is_some_and(|s| s.num_edit.is_some()) => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.num_edit_key(code, event.text.as_deref().unwrap_or(""));
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && self.settings.as_ref().is_some_and(|s| s.renaming) => {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => self.rename_end(false),
                    PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => self.rename_end(true),
                    PhysicalKey::Code(KeyCode::Backspace) => {
                        if let Some(p) = settings::user_preset(&mut self.cfg) {
                            p.name.pop();
                        }
                    }
                    _ => {
                        let typed: String = event.text.as_deref().unwrap_or("").chars().filter(|c| !c.is_control() && *c != '|').collect();
                        if let Some(p) = settings::user_preset(&mut self.cfg) {
                            for ch in typed.chars() {
                                if p.name.chars().count() < 40 {
                                    p.name.push(ch);
                                }
                            }
                        }
                    }
                }
                self.settings_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => self.settings_cancel(),
                PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => self.settings_act(el, settings::Act::Ok),
                _ => {}
            },
            _ => {}
        }
    }

    /// Stop renaming a preset: keep the typed name, or put the old one back.
    /// An empty name always puts the old one back.
    fn rename_end(&mut self, keep: bool) {
        let Some(s) = &mut self.settings else { return };
        if !s.renaming {
            return;
        }
        s.renaming = false;
        let orig = self.rename_orig.take().unwrap_or_default();
        if let Some(p) = settings::user_preset(&mut self.cfg) {
            if !keep || p.name.trim().is_empty() {
                p.name = orig;
            } else {
                p.name = p.name.trim().to_string();
            }
        }
        self.settings_redraw();
    }

    /// Stop typing into a number field: keep the value (clamped to its range), or put
    /// the old one back. Text that is not a number puts the old one back.
    fn num_edit_end(&mut self, keep: bool) {
        let Some((f, text)) = self.settings.as_mut().and_then(|s| s.num_edit.take()) else { return };
        let before = self.cfg.clone();
        let orig = self.num_edit_orig.take();
        match (keep, f.parse(&text), orig) {
            (true, Some(v), _) => f.set(&mut self.cfg, v),
            (_, _, Some(o)) => f.set(&mut self.cfg, o),
            _ => {}
        }
        self.apply_settings(&before);
        self.settings_redraw();
    }

    /// Typing into a number field: show each valid value live.
    fn num_edit_key(&mut self, code: KeyCode, text: &str) {
        let Some(s) = &mut self.settings else { return };
        let Some((f, buf)) = &mut s.num_edit else { return };
        let f = *f;
        match code {
            KeyCode::Escape => return self.num_edit_end(false),
            KeyCode::Enter | KeyCode::NumpadEnter => return self.num_edit_end(true),
            KeyCode::Backspace => {
                buf.pop();
            }
            _ => {
                for ch in text.chars() {
                    let ok = ch.is_ascii_digit() || (ch == '.' && f.decimals() && !buf.contains('.'));
                    if ok && buf.len() < 8 {
                        buf.push(ch);
                    }
                }
            }
        }
        // live preview: apply when the text is a number, without ending the edit
        if let Some(v) = f.parse(buf) {
            let before = self.cfg.clone();
            f.set(&mut self.cfg, v);
            self.apply_settings(&before);
        }
        self.settings_redraw();
    }

    fn settings_act(&mut self, el: &ActiveEventLoop, a: settings::Act) {
        use settings::Act;
        if a != Act::PresetRename {
            self.rename_end(true);
        }
        if !matches!(a, Act::NumEdit(_)) {
            self.num_edit_end(true);
        }
        let before = self.cfg.clone();
        match a {
            Act::Tab(i) => {
                if let Some(s) = &mut self.settings {
                    s.tab = i;
                }
                self.settings_redraw();
                return;
            }
            Act::Toggle(f) => f.toggle(&mut self.cfg),
            Act::Step(f, d) => f.step(&mut self.cfg, d),
            Act::Swatch(c) => self.cfg.bg_color = c,
            Act::Clear(f) => f.set(&mut self.cfg, String::new()),
            Act::Choice(f) => {
                let Some((x, y, w)) = self.settings.as_ref().and_then(|s| s.anchor(a).map(|(x, y, bw)| (x, y, bw, s.window.clone()))).map(|(x, y, bw, w)| {
                    self.below_w = Some(bw as f64);
                    (x, y, w)
                }) else { return };
                let cur = f.get(&self.cfg);
                let items = f
                    .options(&self.cfg)
                    .iter()
                    .enumerate()
                    .map(|(i, o)| MenuEntry::Check(format!("cfgopt:{i}"), o.to_string(), "", i as u8 == cur))
                    .collect();
                self.pending_choice = Some(f);
                self.popup(el, &w, items, x, y, &format!("{a:?}"));
                self.settings_redraw();
                return;
            }
            Act::Browse(f) => {
                let Some(w) = self.settings.as_ref().map(|s| s.window.clone()) else { return };
                let d = rfd::FileDialog::new().set_parent(&*w);
                let picked = if f == settings::PathF::Editor {
                    d.set_title("Select Image Editor").pick_file()
                } else {
                    d.set_title("Select Photo Folder").pick_folder()
                };
                match picked {
                    Some(p) => f.set(&mut self.cfg, p.display().to_string()),
                    None => return,
                }
            }
            Act::KeyToggle(i) => self.cfg.keys[i].on = !self.cfg.keys[i].on,
            Act::MouseToggle(i) => self.cfg.mouse[i].on = !self.cfg.mouse[i].on,
            Act::KeyCapture(i) => {
                if let Some(s) = &mut self.settings {
                    s.capturing = if s.capturing == Some(i) { None } else { Some(i) };
                }
                self.settings_redraw();
                return;
            }
            Act::KeyAction(i) | Act::MouseAction(i) => {
                let key = matches!(a, Act::KeyAction(_));
                let Some((x, y, w)) = self.settings.as_ref().and_then(|s| s.anchor(a).map(|(x, y, bw)| (x, y, bw, s.window.clone()))).map(|(x, y, bw, w)| {
                    self.below_w = Some(bw as f64);
                    (x, y, w)
                }) else { return };
                let cur = if key { &self.cfg.keys[i].action } else { &self.cfg.mouse[i].action };
                let cur = crate::bindings::index(cur);
                let items = crate::bindings::ACTIONS
                    .iter()
                    .enumerate()
                    .map(|(k, (_, lbl))| MenuEntry::Check(format!("bindopt:{k}"), lbl.to_string(), "", k == cur))
                    .collect();
                self.pending_bind = Some((key, i));
                self.popup(el, &w, items, x, y, &format!("{a:?}"));
                self.settings_redraw();
                return;
            }
            Act::ClearThumbs => {
                let (bytes, n) = crate::thumbcache::usage();
                crate::thumbcache::clear();
                self.msg(format!("Thumbnail cache cleared: {n} files, {}", fmt_size(bytes)));
                return;
            }
            Act::OpenIni => {
                self.cfg.save();
                open_text_file(&Config::path());
                return;
            }
            Act::NumEdit(f) => {
                if self.settings.as_ref().and_then(|s| s.num_edit.as_ref()).is_some_and(|(g, _)| *g == f) {
                    return; // already typing here
                }
                self.num_edit_end(true);
                if f.editable(&self.cfg) {
                    let start = f.plain(&self.cfg);
                    self.num_edit_orig = Some(f.spec(&self.cfg).0);
                    if let Some(s) = &mut self.settings {
                        s.num_edit = Some((f, start));
                    }
                }
                self.settings_redraw();
                return;
            }
            Act::PresetRename => {
                let name = self.preset().name;
                if let Some(s) = &mut self.settings {
                    if !s.renaming && !settings::preset_locked(&self.cfg) {
                        s.renaming = true;
                        self.rename_orig = Some(name);
                    }
                }
                self.settings_redraw();
                return;
            }
            Act::PresetNew => {
                let mut p = self.preset();
                p.name = crate::filters::new_name(&self.cfg.filter_presets);
                if crate::filters::builtin_count() + self.cfg.filter_presets.len() < 255 {
                    self.cfg.filter_presets.push(p);
                    self.cfg.filter = (crate::filters::builtin_count() + self.cfg.filter_presets.len() - 1) as u8;
                }
                self.invalidate();
            }
            Act::PresetDelete => {
                if let Some(i) = (self.cfg.filter as usize).checked_sub(crate::filters::builtin_count()) {
                    if i < self.cfg.filter_presets.len() {
                        self.cfg.filter_presets.remove(i);
                        // the one before it, or the last built-in
                        self.cfg.filter = self.cfg.filter.saturating_sub(1);
                    }
                }
                self.invalidate();
            }
            Act::Reset => {
                let w = self.settings.as_ref().map(|s| s.window.clone());
                let text = "Reset every setting, hotkey and mouse button to its default?\n\nYour filter presets, window position and last folder stay.";
                if !crate::dialog::ask(w.as_deref(), "Reset all settings", text, crate::dialog::Kind::Warn) {
                    return;
                }
                // presets, window geometry, last folder and the install flag are not settings
                let old = std::mem::take(&mut self.cfg);
                self.cfg = Config {
                    filter_presets: old.filter_presets,
                    last_folder: old.last_folder,
                    install_offered: old.install_offered,
                    update_checked: old.update_checked,
                    win_x: old.win_x,
                    win_y: old.win_y,
                    win_w: old.win_w,
                    win_h: old.win_h,
                    win_max: old.win_max,
                    win_monitor: old.win_monitor,
                    ..Config::default()
                };
                self.msg("Settings reset. OK keeps them, Cancel undoes.");
            }
            Act::GitHub => {
                crate::update::open_repo_page();
                return;
            }
            Act::CheckUpdate => {
                if let Some(s) = &mut self.settings {
                    s.set_upd(crate::settings::Upd::Checking);
                }
                crate::update::spawn_check(self.proxy.clone(), true);
                self.settings_redraw();
                return;
            }
            Act::MakeDefault => {
                #[cfg(target_os = "macos")]
                crate::mac::spawn_make_default(self.proxy.clone());
                self.settings_redraw();
                return;
            }
            Act::Install => {
                if let Some(s) = &mut self.settings {
                    s.card = Some(crate::install::Card::Install(crate::install::is_installed()));
                    s.card_hover = None;
                }
                self.settings_redraw();
                return;
            }
            Act::Cancel => {
                self.settings_cancel();
                return;
            }
            Act::Ok => {
                self.cfg.save();
                self.settings = None;
                return;
            }
        }
        self.apply_settings(&before);
    }

    fn settings_cancel(&mut self) {
        let Some(s) = self.settings.take() else { return };
        let before = self.cfg.clone();
        self.cfg = s.snapshot;
        self.apply_settings(&before);
    }

    /// Bring the running app in line with `self.cfg` after it changed from `before`.
    fn apply_settings(&mut self, before: &Config) {
        let c = self.cfg.clone();
        if c.exif_rotate != before.exif_rotate {
            self.exif_flag.store(c.exif_rotate, Ordering::Relaxed);
            self.cache.borrow_mut().clear();
            self.failed.borrow_mut().clear();
            self.current = None;
            self.show_current();
        }
        if c.sort != before.sort || c.sort_desc != before.sort_desc {
            self.resort();
        }
        if c.view_mode != before.view_mode || c.stretch_small != before.stretch_small || c.align_top != before.align_top {
            self.view.user_zoom = None;
            self.view.pan_reset = true;
        }
        // live preview: redraw the viewer with the changed filter preset
        // (the scaled-image key holds the preset's fingerprint, so a redraw re-filters)
        if c.filter != before.filter || c.filter_presets != before.filter_presets || c.interp != before.interp {
            self.request_redraw();
        }
        if c.thumb_cache_mb != before.thumb_cache_mb {
            self.thumb_jobs.disk.store(c.thumb_cache_mb > 0, Ordering::Relaxed);
        }
        self.invalidate();
        self.settings_redraw();
    }

    pub fn drain_menu_events(&mut self, el: &ActiveEventLoop) {
        let mut ids = Vec::new();
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            ids.push(ev.id.0.clone());
        }
        for id in ids {
            self.menu_action(el, &id);
        }
    }

    /// True when the open playlist is a comic-book archive.
    fn archive_open(&self) -> bool {
        self.playlist.as_ref().is_some_and(|p| source::is_archive(&p.container))
    }

    /// True when the open archive reads right to left (its pages reversed).
    fn rtl_on(&self) -> bool {
        self.playlist.as_ref().is_some_and(|p| self.rtl_for.as_deref() == Some(p.container.as_path()))
    }

    /// Reverse the open archive's pages, keeping the current page shown.
    fn toggle_rtl(&mut self) {
        if !self.archive_open() {
            return;
        }
        let on = self.rtl_on();
        let Some(pl) = &mut self.playlist else { return };
        // read the shared list backwards; the index stays on the same page
        let last = pl.len().saturating_sub(1);
        pl.index = last - pl.index.min(last);
        pl.rev = !pl.rev;
        self.rtl_for = if on { None } else { Some(pl.container.clone()) };
        self.request_redraw();
    }

    fn resort(&mut self) {
        let Some(pl) = &self.playlist else { return };
        let c = pl.container.clone();
        // an archive keeps its page order (and its reading direction) whatever the sort
        if source::is_archive(&c) {
            return;
        }
        let cur = self.cur_entry();
        if let Ok(mut np) = source::open(&c, self.cfg.sort, self.cfg.sort_desc) {
            if let Some(i) = cur.and_then(|e| np.iter().position(|x| *x == e)) {
                np.index = i;
            }
            self.set_playlist(np);
        }
    }

    fn open_in_shell(p: &Path) {
        #[cfg(windows)]
        let _ = Command::new("explorer").arg(p).spawn();
        #[cfg(target_os = "macos")]
        let _ = Command::new("open").arg(p).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = Command::new("xdg-open").arg(p).spawn();
    }

    fn menu_action(&mut self, el: &ActiveEventLoop, id: &str) {
        if let Some(n) = id.strip_prefix("bindopt:") {
            if let (Some((key, i)), Some((aid, _))) = (self.pending_bind.take(), n.parse::<usize>().ok().and_then(|k| crate::bindings::ACTIONS.get(k))) {
                let b = if key { &mut self.cfg.keys[i] } else { &mut self.cfg.mouse[i] };
                b.action = aid.to_string();
                b.on = true;
                self.settings_redraw();
            }
            return;
        }
        if let Some(n) = id.strip_prefix("cfgopt:") {
            if let (Some(f), Ok(v)) = (self.pending_choice.take(), n.parse::<u8>()) {
                let before = self.cfg.clone();
                f.set(&mut self.cfg, v);
                self.apply_settings(&before);
            }
            return;
        }
        if let Some(q) = id.strip_prefix("spd:").and_then(|q| q.parse::<u32>().ok()) {
            self.set_anim_speed(q as f32 / 4.0);
            return;
        }
        if id == "slide_toggle" {
            self.cfg.slide_ms = if self.cfg.slide_ms > 0 { 0 } else { 250 };
            self.cfg.save();
            return;
        }
        match id {
            "open" => self.open_dialog(),
            "open_folder" => self.open_folder_dialog(),
            "close" => self.close(),
            "reveal" => self.reveal(),
            "reveal_all" => self.reveal_all = true,
            "fedit" => self.open_settings(el, settings::FILTERS_TAB),
            _ if id.starts_with('f') && id[1..].parse::<u8>().is_ok() => {
                let i = id[1..].parse::<u8>().unwrap();
                if (i as usize) < crate::filters::builtin_count() + self.cfg.filter_presets.len() {
                    self.cfg.filter = i;
                    self.invalidate();
                    self.msg(&format!("Filter: {}", self.preset().name));
                }
            }
            "ip0" | "ip1" | "ip2" | "ip3" | "ip4" => {
                self.cfg.interp = id.as_bytes()[2] - b'0';
                if self.cfg.filter == 0 {
                    self.cfg.filter = 1;
                }
                self.invalidate();
            }
            "next" => self.next(),
            "prev" => self.prev(),
            "first" => self.go_to(0),
            "last" => {
                let l = self.len();
                self.go_to(l.saturating_sub(1));
            }
            "next10" => self.step(10, true),
            "prev10" => self.step(-10, true),
            "rnext" => self.random(),
            "rprev" => self.random_back(),
            "nextdir" => self.container_step(true, true),
            "prevdir" => self.container_step(false, true),
            "neg" => {
                self.view.negative = !self.view.negative;
                self.invalidate();
            }
            "gamma" => {
                self.view.gamma = !self.view.gamma;
                self.invalidate();
            }
            "flipv" => {
                self.view.flip_v = !self.view.flip_v;
                self.invalidate();
            }
            "fliph" => {
                self.view.flip_h = !self.view.flip_h;
                self.invalidate();
            }
            "sort0" | "sort1" | "sort2" | "sort3" => {
                self.cfg.sort = id.as_bytes()[4] - b'0';
                self.resort();
            }
            "sortdesc" => {
                self.cfg.sort_desc = !self.cfg.sort_desc;
                self.resort();
            }
            "stretch" => {
                self.cfg.stretch_small = !self.cfg.stretch_small;
                self.view.user_zoom = None;
                self.invalidate();
            }
            "mode0" => self.set_mode(0),
            "mode1" => self.set_mode(1),
            "mode2" => self.set_mode(2),
            "zin" => self.zoom_step(true, None),
            "zout" => self.zoom_step(false, None),
            "zfix" => self.view.zoom_fixed = !self.view.zoom_fixed,
            "z100" => self.zoom_to(1.0, None),
            "zin1" => self.zoom_pct(1.0),
            "zout1" => self.zoom_pct(-1.0),
            "rot0" => {
                self.view.rotation = 0;
                self.rotate(0);
            }
            "rotl" => self.rotate(3),
            "rotr" => self.rotate(1),
            "rot180" => self.rotate(2),
            "info" => {
                self.show_info = !self.show_info;
                self.request_redraw();
            }
            "delete" => self.delete_current(),
            "copy1" => self.to_photo_folder(1, false),
            "move1" => self.to_photo_folder(1, true),
            "copy2" => self.to_photo_folder(2, false),
            "pfset" => self.open_settings(el, settings::TABS.len() - 1),
            "pfopen" => {
                let p = PathBuf::from(&self.cfg.photo_folder1);
                Self::open_in_shell(&p);
            }
            "editor" => self.open_editor(),
            "clip" => self.copy_image(),
            "config" => self.open_settings(el, 0),
            "exit" => self.exit(el),
            "closeall" => {
                self.modal = Some(crate::install::Card::CloseAll);
                self.modal_hover = None;
                self.request_redraw();
            }
            "pageselect" => self.open_picker(),
            _ => {}
        }
    }

    // ---------- Select Image (thumbnail grid) ----------

    fn picker_grid(&self) -> Option<picker::Grid> {
        let w = self.window.as_ref()?;
        let sz = w.inner_size();
        Some(self.picker.grid(sz.width as f32, sz.height as f32, self.sf()))
    }

    fn open_picker(&mut self) {
        if self.len() == 0 {
            return;
        }
        crate::trace("picker open");
        self.picker.open = true;
        self.picker.sel = self.index();
        self.picker.hover = None;
        if let Some(g) = self.picker_grid() {
            let n = self.len();
            self.picker.center_on_sel(&g, n);
        }
        self.request_redraw();
    }

    fn close_picker(&mut self) {
        self.picker.open = false;
        // drop queued thumbnail work and the cached background
        self.picker.bg.clear();
        self.thumb_jobs.clear();
        self.request_redraw();
    }

    /// Where the cursor is over the picker: on the close button, outside the panel, or neither.
    fn picker_close_at_cursor(&self) -> (bool, bool) {
        let Some(sz) = self.window.as_ref().map(|w| w.inner_size()) else { return (false, false) };
        let (w, h, sf) = (sz.width as f32, sz.height as f32, self.sf());
        let (x, y) = (self.cursor.0 as f32, self.cursor.1 as f32);
        let (x0, y0, x1, y1) = picker::close_rect(w, h, sf);
        let (px, py, pw, ph) = picker::panel_rect(w, h, sf);
        (x >= x0 && x < x1 && y >= y0 && y < y1, x < px || x >= px + pw || y < py || y >= py + ph)
    }

    fn picker_bar(&self) -> Option<picker::Bar> {
        let sz = self.window.as_ref()?.inner_size();
        self.picker.bar(sz.width as f32, sz.height as f32, self.sf(), self.len())
    }

    fn picker_index_at_cursor(&self) -> Option<usize> {
        let g = self.picker_grid()?;
        self.picker.index_at(&g, self.cursor.0 as f32, self.cursor.1 as f32, self.len())
    }

    fn picker_key(&mut self, code: KeyCode) {
        let Some(g) = self.picker_grid() else { return };
        let n = self.len() as isize;
        let cols = g.cols as isize;
        let page = cols * ((g.view_h / g.ch).floor() as isize).max(1);
        let s = self.picker.sel as isize;
        let t = match code {
            KeyCode::Escape | KeyCode::Backspace => return self.close_picker(),
            KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => {
                let i = self.picker.sel;
                self.go_to(i);
                return self.close_picker();
            }
            KeyCode::ArrowLeft => s - 1,
            KeyCode::ArrowRight => s + 1,
            KeyCode::ArrowUp => s - cols,
            KeyCode::ArrowDown => s + cols,
            KeyCode::PageUp => s - page,
            KeyCode::PageDown => s + page,
            KeyCode::Home => 0,
            KeyCode::End => n - 1,
            _ => return,
        };
        self.picker.sel = t.clamp(0, n - 1) as usize;
        self.picker.reveal(&g, n as usize);
        self.request_redraw();
    }

    // ---------- window geometry ----------

    /// The saved window rect, if it still lands on a connected monitor (at least 64x64 px
    /// visible). The monitor it was closed on is preferred; a rect larger than that
    /// monitor is shrunk to fit. None means first run or a monitor that is gone.
    fn saved_rect(&self, el: &ActiveEventLoop) -> Option<(i32, i32, u32, u32)> {
        let c = &self.cfg;
        if c.win_w == 0 || c.win_h == 0 {
            return None;
        }
        let (x, y, w, h) = (c.win_x, c.win_y, c.win_w as i32, c.win_h as i32);
        let overlap = |m: &winit::monitor::MonitorHandle| {
            let (mp, ms) = (m.position(), m.size());
            let ox = (x + w).min(mp.x + ms.width as i32) - x.max(mp.x);
            let oy = (y + h).min(mp.y + ms.height as i32) - y.max(mp.y);
            ox.max(0) as i64 * oy.max(0) as i64
        };
        let monitors: Vec<_> = el.available_monitors().collect();
        let named = monitors.iter().find(|m| m.name().as_deref() == Some(c.win_monitor.as_str()) && overlap(m) >= 64 * 64);
        let best = named.or_else(|| monitors.iter().filter(|m| overlap(m) >= 64 * 64).max_by_key(|m| overlap(m)))?;
        let ms = best.size();
        Some((x, y, (w as u32).min(ms.width), (h as u32).min(ms.height)))
    }

    /// Remember the restored rect and maximized state; called on every move and resize.
    fn track_geometry(&mut self) {
        let Some(w) = &self.window else { return };
        if self.fullscreen || w.is_minimized() == Some(true) {
            return;
        }
        let max = is_maximized(&w);
        self.was_max = max;
        if !max {
            if let Ok(p) = w.outer_position() {
                let s = w.inner_size();
                self.geom = Some((p.x, p.y, s.width, s.height));
            }
        }
    }

    fn store_geometry(&mut self) {
        let Some(w) = &self.window else { return };
        if let Some((x, y, gw, gh)) = self.geom {
            (self.cfg.win_x, self.cfg.win_y, self.cfg.win_w, self.cfg.win_h) = (x, y, gw, gh);
        }
        self.cfg.win_max = self.was_max;
        self.cfg.win_fs = self.fullscreen;
        if let Some(name) = w.current_monitor().and_then(|m| m.name()) {
            self.cfg.win_monitor = name;
        }
    }

    /// Close this window. The master drops closed viewers and quits after the last one.
    pub fn exit(&mut self, _el: &ActiveEventLoop) {
        self.store_geometry();
        self.cfg.save();
        self.closed = true;
        self.settings = None;
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
    }

    /// Frame count of the current image when it animates and plays; None otherwise.
    fn anim_frames(&self) -> Option<usize> {
        let n = self.current.as_ref()?.1.frame_count();
        (n > 1).then_some(n)
    }

    /// Scrub: show the frame under the cursor and hold it while the button is down.
    fn anim_seek(&mut self) {
        let Some(n) = self.anim_frames() else { return };
        // the track as last drawn: its grab area is the track plus 8 px on each side
        let Some(h) = self.hits.iter().find(|h| h.id == "anim_slider") else { return };
        let pad = 8.0 * self.sf();
        let (tx, tw) = (h.x0 + pad, (h.x1 - h.x0 - 2.0 * pad).max(1.0));
        let frac = ((self.cursor.0 as f32 - tx) / tw).clamp(0.0, 1.0);
        let f = ((frac * n as f32) as usize).min(n - 1);
        self.next_frame_at = None;
        if f != self.frame_idx {
            self.frame_idx = f;
            self.base = None;
        }
        self.request_redraw();
    }

    // Speed pill helpers: added 2026-11-16 by Claude on the Mac (see docs/windows-handoff.md).
    /// A frame's display time at the chosen speed: its delay (10 ms floor) over the speed,
    /// and never under 4 ms, the shortest wait the event loop keeps reliably.
    fn frame_ms(&self, delay_ms: u32) -> u64 {
        ((delay_ms.max(10) as f32 / self.anim_speed).round() as u64).max(4)
    }

    /// Format pill text: the file's extension in capitals ("GIF", "WEBP", "JSON", "LOTTIE"),
    /// except an animated PNG, which reads "APNG".
    fn anim_format(&self) -> String {
        let Some((e, d)) = &self.current else { return String::new() };
        if d.format.starts_with("APNG") {
            return "APNG".into();
        }
        e.ext().to_ascii_uppercase()
    }

    /// Frames per second shown in the speed pill: the current frame's rate at this speed.
    fn anim_fps(&self) -> u32 {
        let Some((_, d)) = &self.current else { return 0 };
        let ms = self.frame_ms(d.frame_delay(self.frame_idx));
        (1000.0 / ms as f32).round() as u32
    }

    fn set_anim_speed(&mut self, v: f32) {
        self.anim_speed = v.clamp(0.25, 4.0);
        // re-time the frame on screen from now, so a slower speed takes effect at once
        if self.next_frame_at.is_some() {
            if let Some(d) = self.current.as_ref().map(|c| c.1.clone()) {
                let ms = self.frame_ms(d.frame_delay(self.frame_idx));
                self.next_frame_at = Some(Instant::now() + Duration::from_millis(ms));
            }
        }
        self.request_redraw();
    }

    fn advance_frame(&mut self, now: Instant) {
        let Some(d) = self.current.as_ref().map(|c| c.1.clone()) else { return };
        let n = d.frame_count();
        if n < 2 {
            self.next_frame_at = None;
            return;
        }
        self.frame_idx = (self.frame_idx + 1) % n;
        let delay = self.frame_ms(d.frame_delay(self.frame_idx));
        // Keep cadence; do not fall behind after a slow frame.
        let mut next = self.next_frame_at.unwrap_or(now) + Duration::from_millis(delay);
        if next < now {
            next = now + Duration::from_millis(delay);
        }
        self.next_frame_at = Some(next);
        self.base = None;
        // only the frame changed: the animation row can be redrawn alone (ui_dirty stays)
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl App {
    /// Create this viewer's window. `cascade` places it at that position (another window
    /// is already open) instead of the saved geometry.
    pub fn resumed(&mut self, el: &ActiveEventLoop, place: Place) {
        if self.window.is_some() {
            return;
        }
        let (cascade, exact) = match &place {
            Place::Saved => (None, None),
            Place::Cascade(x, y) => (Some((*x, *y)), None),
            Place::Exact(s) => (None, Some(s.clone())),
        };
        let mut attrs = Window::default_attributes()
            .with_title("ArkImage")
            .with_inner_size(LogicalSize::new(1280.0, 860.0))
            .with_decorations(false)
            .with_window_icon(crate::icon::window_icon())
            .with_visible(false);
        #[cfg(windows)]
        {
            // large icon (taskbar, Alt+Tab); with_window_icon sets only the small one
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_taskbar_icon(crate::icon::window_icon());
        }
        // macOS: a titled, resizable window with its title bar hidden and the content drawn
        // beneath it, not a borderless one. It keeps native corners, shadow and resize edges,
        // and winit can read the zoomed state directly: for a borderless window it adds a title
        // bar for a moment, which moved the window 32 pt, sent Moved, and looped forever.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs
                .with_decorations(true)
                .with_titlebar_transparent(true)
                .with_title_hidden(true)
                .with_fullsize_content_view(true)
                .with_titlebar_buttons_hidden(true);
        }
        // Reopen where the window was closed: same monitor, size, position, maximized state.
        // A second window opens 32 px down and right of the newest one, not maximized.
        let restored = match (cascade, &exact) {
            (_, Some(s)) => {
                // crash restore: the window's own last geometry and state
                self.cfg.win_max = s.max;
                Some((s.x, s.y, s.w, s.h))
            }
            (Some((x, y)), None) => {
                self.cfg.win_max = false;
                self.saved_rect(el).map(|(_, _, w, h)| (x + 32, y + 32, w, h)).or(Some((x + 32, y + 32, 1280, 860)))
            }
            (None, None) => self.saved_rect(el),
        };
        if let Some((x, y, w, h)) = restored {
            attrs = attrs
                .with_position(winit::dpi::PhysicalPosition::new(x, y))
                .with_inner_size(winit::dpi::PhysicalSize::new(w, h));
        }
        crate::trace("resumed");
        let win = Arc::new(el.create_window(attrs).expect("create window"));
        self.geom = restored.map(|r| (r.0, r.1, r.2, r.3));
        self.was_max = self.cfg.win_max && restored.is_some();
        if self.was_max {
            win.set_maximized(true);
        }
        crate::trace("window created");
        self.gpu_cfg = self.gpu_wanted();
        if self.gpu_cfg {
            self.gpu = crate::gpu::GpuWin::new(win.clone());
            crate::gpu::DECODE_TO_GPU.store(self.gpu.is_some(), Ordering::Relaxed);
        }
        self.window = Some(win);
        if self.gpu.is_none() {
            self.cpu_surface();
        }
        if let Some(p) = self.pending_open.take() {
            self.open_path(&p);
        }
        if let Some(s) = exact.as_ref().filter(|s| !s.label.is_empty()) {
            self.open_label(&s.label);
        }
        self.update_title();
        // Show only now: placed, maximized if saved so, and with a first frame drawn.
        // (Created visible, a saved-maximized window showed for 19 ms on the primary
        // monitor at 312,312 before it moved to its own monitor.)
        // A hidden window drops the frame, and a shown one is white until its first
        // paint; so show it cloaked, draw, then uncloak.
        if let Some(w) = self.window.clone() {
            cloak(&w, true);
            w.set_visible(true);
            self.render();
            cloak(&w, false);
        }
        // full screen: a crash restore uses the window's own state; a normal start (not a
        // cascaded second window) reopens in full screen if it was closed so
        if exact.as_ref().map(|s| s.fs).unwrap_or(cascade.is_none() && self.cfg.win_fs) {
            // asked while the window is still appearing, macOS enters full screen and
            // leaves it again within 2 s; so ask a moment after it is shown
            self.fs_restore_at = Some(Instant::now() + Duration::from_millis(400));
        }
        self.request_redraw();
    }

    /// A thumbnail finished (the master already marked it done); keep it if the grid is open.
    pub fn on_thumb(&mut self, e: &Entry, t: &Option<Arc<RgbaImage>>) {
        if self.picker.open {
            self.picker.insert(e.clone(), t.clone());
            self.request_redraw();
        }
    }

    pub fn outer_position(&self) -> Option<(i32, i32)> {
        self.window.as_ref()?.outer_position().ok().map(|p| (p.x, p.y))
    }

    pub fn focus(&self) {
        if let Some(w) = &self.window {
            w.focus_window();
        }
    }

    pub fn is_focused(&self) -> bool {
        self.window.as_ref().map(|w| w.has_focus()).unwrap_or(false)
    }

    /// Take a config changed in another window and apply it here.
    pub fn set_config(&mut self, c: Config) {
        let before = std::mem::replace(&mut self.cfg, c);
        self.apply_settings(&before);
        // the newest sort order holds for every window
        if (before.sort, before.sort_desc) != (self.cfg.sort, self.cfg.sort_desc) {
            self.resort();
        }
    }

    /// True if this window's main window or settings dialog has id `id`.
    pub fn owns(&self, id: WindowId) -> bool {
        self.window.as_ref().map(|w| w.id()) == Some(id) || self.settings.as_ref().map(|s| s.window.id()) == Some(id)
    }

    pub fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        // Drop the click that closed a popup (see stale_click), in every window.
        match &ev {
            WindowEvent::CursorMoved { .. } => {
                if let Some(p) = &mut self.popup_closed {
                    p.2 = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, .. } if self.stale_click() => {
                crate::trace("click after popup dropped");
                self.popup_closed = None;
                return;
            }
            _ => {}
        }
        if self.settings.as_ref().map(|s| s.window.id()) == Some(id) {
            self.settings_event(el, ev);
            return;
        }
        // zooming, panning, clicking or typing in a window that kept only its screen-sized
        // copy brings the full image back (a scroll or pinch reaches a window without
        // focusing it on the Mac)
        if self.stripped
            && matches!(
                ev,
                WindowEvent::MouseWheel { .. } | WindowEvent::PinchGesture { .. } | WindowEvent::MouseInput { .. } | WindowEvent::KeyboardInput { .. }
            )
        {
            self.unstrip();
        }
        match ev {
            WindowEvent::CloseRequested => self.exit(el),
            WindowEvent::Moved(_) => self.track_geometry(),
            // macOS: the notch band is a window of its own at menu-bar level; it must not
            // stay over other apps
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.track_geometry();
                self.scaled = None;
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render(),
            // covered or minimized: stop drawing and give the GPU memory back; uncovered:
            // draw again (the image is uploaded anew)
            // macOS: the event says "visible" for a window under one of ours (see
            // is_occluded), and taking it as is flipped such windows between covered and
            // uncovered, each flip a redraw and a new decode; the event asks again instead
            #[cfg(target_os = "macos")]
            WindowEvent::Occluded(o) => {
                let c = o || self.window.as_ref().is_some_and(|w| crate::mac::is_occluded(w));
                self.set_covered(c);
            }
            #[cfg(windows)]
            WindowEvent::Occluded(o) => {
                let c = o || self.window.as_ref().is_some_and(covered_now);
                self.set_covered(c);
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            WindowEvent::Occluded(o) => self.set_covered(o),
            WindowEvent::Focused(true) => {
                // a focused window is in front: draw it now, not at the next covered check
                self.set_covered(false);
                self.unstrip();
                // the focused window preloads its neighbours (GPU mode)
                self.update_wanted();
                self.preload();
            }
            WindowEvent::DroppedFile(p) => self.open_path(&p),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            // A synthetic press is a key already held when the window gained focus; it was
            // pressed for another window. A held Esc closed every new window on launch.
            WindowEvent::KeyboardInput { is_synthetic: true, .. } => {}
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    let code = match event.physical_key {
                        PhysicalKey::Code(c) => Some(c),
                        _ => None,
                    };
                    let ch = match &event.logical_key {
                        Key::Character(s) => s.chars().next(),
                        _ => None,
                    };
                    self.on_key(el, code, ch);
                }
            }
            WindowEvent::CursorEntered { .. } => {
                self.cursor_in = true;
                self.request_redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_in = false;
                self.hover = None;
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } if self.picker.open => {
                self.cursor = (position.x, position.y);
                // the handle lights up under the cursor (250 ms fade)
                let (cx, cy) = (position.x as f32, position.y as f32);
                let hot = self.picker_bar().is_some_and(|b| b.hit(cx, cy, self.sf()));
                if hot != self.picker.bar_hot && self.picker.bar_drag.is_none() {
                    self.picker.bar_hot = hot;
                    self.picker.bar_hot_at = Some(Instant::now());
                    self.request_redraw();
                }
                // dragging the scroll handle: keep the grab point under the cursor
                if let Some(grab) = self.picker.bar_drag {
                    if let Some(b) = self.picker_bar() {
                        self.picker.scroll = b.scroll_for(position.y as f32 - grab);
                        self.request_redraw();
                    }
                    return;
                }
                let h = self.picker_index_at_cursor();
                let ch = self.picker_close_at_cursor().0;
                if h != self.picker.hover || ch != self.picker.close_hover {
                    self.picker.hover = h;
                    self.picker.close_hover = ch;
                    self.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } if self.modal.is_some() => {
                self.cursor = (position.x, position.y);
                let (x, y) = (position.x as f32, position.y as f32);
                let h = self.modal_hits.iter().position(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3);
                if h != self.modal_hover {
                    self.modal_hover = h;
                    self.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                self.cursor_in = true;
                self.on_cursor_moved();
                if self.slider_drag {
                    self.slider_seek();
                    return;
                }
                if self.anim_drag {
                    self.anim_seek();
                    return;
                }
                if let (true, Some((sc, sp, moved))) = (self.drag_rect, self.drag) {
                    let moved = moved || (position.x - sc.0).abs() + (position.y - sc.1).abs() > 4.0;
                    self.drag = Some((sc, sp, moved));
                    if moved {
                        self.zoom_rect = Some((sc, self.cursor));
                        if self.right_down {
                            self.right_used = true;
                        }
                        self.request_redraw();
                    }
                    return;
                }
                if let Some((sc, sp, moved)) = self.drag {
                    let dx = position.x - sc.0;
                    let dy = position.y - sc.1;
                    let moved = moved || dx.abs() + dy.abs() > 4.0;
                    self.drag = Some((sc, sp, moved));
                    self.view.pan_x = sp.0 - dx;
                    self.view.pan_y = sp.1 - dy;
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) if self.picker.open => {
                    // the close button, or a click outside the panel, closes the grid
                    let (on_close, outside) = self.picker_close_at_cursor();
                    let (x, y) = (self.cursor.0 as f32, self.cursor.1 as f32);
                    if let Some(b) = self.picker_bar().filter(|b| b.hit(x, y, self.sf())) {
                        // on the handle: grab it where pressed; on the track: centre it there
                        let grab = if y >= b.y && y < b.y + b.h { y - b.y } else { b.h / 2.0 };
                        self.picker.scroll = b.scroll_for(y - grab);
                        self.picker.bar_drag = Some(grab);
                        self.request_redraw();
                    } else if on_close || outside {
                        self.close_picker();
                    } else if let Some(i) = self.picker_index_at_cursor() {
                        self.go_to(i);
                        self.close_picker();
                    }
                }
                (MouseButton::Left, ElementState::Released) if self.picker.open => {
                    if self.picker.bar_drag.take().is_some() {
                        self.request_redraw();
                    }
                }
                (MouseButton::Left, ElementState::Pressed) if self.modal.is_some() => {
                    let (x, y) = (self.cursor.0 as f32, self.cursor.1 as f32);
                    if let Some(i) = self.modal_hits.iter().position(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3) {
                        self.modal_button(i);
                    }
                }
                (_, _) if self.modal.is_some() => {}
                (MouseButton::Left, ElementState::Pressed) => {
                    if let Some(dir) = self.resize_edge() {
                        if let Some(w) = &self.window {
                            let _ = w.drag_resize_window(dir);
                        }
                        return;
                    }
                    // the hotkeys card closes on a click anywhere but a toolbar button: on
                    // the card, its close button or the image
                    if self.show_help && self.hit_at(self.cursor).is_none_or(|h| h.id == "help_close") {
                        self.show_help = false;
                        self.help_at = Instant::now();
                        self.request_redraw();
                        return;
                    }
                    if let Some(h) = self.hit_at(self.cursor) {
                        self.on_chrome_click(el, h, false);
                        return;
                    }
                    let now = Instant::now();
                    if let Some((t, pos)) = self.last_click {
                        if now - t < Duration::from_millis(400)
                            && (pos.0 - self.cursor.0).abs() < 6.0
                            && (pos.1 - self.cursor.1).abs() < 6.0
                        {
                            self.last_click = None;
                            self.drag = None;
                            if self.view.user_zoom.is_some() {
                                // zoomed by hand: back to the default view (100%, fit
                                // window or fit width, as set in Settings > View)
                                self.view.user_zoom = None;
                                self.view.pan_reset = true;
                                self.scaled = None;
                                self.request_redraw();
                                self.msg(match self.cfg.view_mode {
                                    0 => "Original Size (100%)",
                                    1 => "Fit in Window",
                                    _ => "Fit to Width",
                                });
                            } else {
                                // default view: the same as the title bar's full-screen button
                                self.toggle_fullscreen();
                            }
                            return;
                        }
                    }
                    self.last_click = Some((now, self.cursor));
                    self.drag = Some((self.cursor, (self.view.pan_x, self.view.pan_y), false));
                    // pan when the image is bigger than the window, unless the right button is held
                    let pannable = self.layout.is_some_and(|l| l.sw > l.ww + 0.5 || l.sh > l.wh + 0.5);
                    self.drag_rect = self.right_down || !pannable;
                }
                (MouseButton::Right, ElementState::Pressed) if self.picker.open => self.close_picker(),
                (MouseButton::Right, ElementState::Pressed) => {
                    // the right button's action waits for its release: held, it turns a
                    // left drag into a zoom rectangle
                    self.right_down = true;
                    self.right_used = false;
                    if self.drag.is_some() {
                        // pressed during a left drag: switch that drag to a rectangle
                        self.drag_rect = true;
                    }
                }
                (MouseButton::Right, ElementState::Released) if self.picker.open => {
                    self.right_down = false;
                }
                (MouseButton::Right, ElementState::Released) => {
                    let used = self.right_used || self.zoom_rect.is_some();
                    self.right_down = false;
                    self.right_used = false;
                    if !used {
                        match self.hit_at(self.cursor) {
                            Some(h) if matches!(h.id, "bar_prev" | "bar_next") => self.on_chrome_click(el, h, true),
                            Some(h) if h.id.starts_with("tb_") || h.id.starts_with("win_") || h.id == "title" => {}
                            _ => self.mouse_bind(el, 2),
                        }
                    }
                }
                (MouseButton::Left, ElementState::Released) => {
                    if self.slider_drag {
                        self.slider_drag = false;
                        self.preload();
                        return;
                    }
                    if self.anim_drag {
                        // resume playback from the frame let go on
                        self.anim_drag = false;
                        if let Some(d) = self.current.as_ref().map(|c| c.1.clone()) {
                            let ms = self.frame_ms(d.frame_delay(self.frame_idx));
                            self.next_frame_at = Some(Instant::now() + Duration::from_millis(ms));
                        }
                        self.request_redraw();
                        return;
                    }
                    if let Some(((sx, sy), _, moved)) = self.drag.take() {
                        let rect = self.drag_rect && moved;
                        self.drag_rect = false;
                        self.zoom_rect = None;
                        if rect {
                            self.zoom_to_rect((sx, sy), self.cursor);
                            self.request_redraw();
                            return;
                        }
                        let left = self.cfg.mouse[0].clone();
                        if !moved && left.on && left.action != "none" {
                            self.run_action(el, &left.action);
                        } else if !moved && self.cfg.click_side_turn {
                            let w = self.layout.map(|l| l.ww).unwrap_or(1.0);
                            if self.cursor.0 < w * 0.3 {
                                self.prev();
                            } else if self.cursor.0 > w * 0.7 {
                                self.next();
                            }
                        }
                    }
                }
                (MouseButton::Middle, ElementState::Pressed) => self.mouse_bind(el, 1),
                (MouseButton::Back, ElementState::Pressed) => self.mouse_bind(el, 3),
                (MouseButton::Forward, ElementState::Pressed) => self.mouse_bind(el, 4),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } if self.picker.open => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 90.0 * self.sf(),
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                self.picker.scroll -= dy;
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                // Two-finger sideways drag on a touchpad: previous / next image.
                // (Added 2026-11-16 by Claude on the Mac, for Windows and macOS; see docs/windows-handoff.md.)
                let (sx, sy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64 * SWIPE_LINE_PX, y as f64 * SWIPE_LINE_PX),
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                };
                // Windows: a touchpad pinch arrives as wheel events with Ctrl held (as does
                // Ctrl+wheel on a mouse): zoom continuously, 1.25x per notch of 120
                if cfg!(windows) && self.mods.control_key() {
                    let notches = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f64,
                        MouseScrollDelta::PixelDelta(p) => p.y / 120.0,
                    };
                    if let Some(l) = self.layout {
                        self.pinch_at = Some(Instant::now());
                        self.zoom_to(l.z * 1.25f64.powf(notches), Some(self.cursor));
                        self.request_redraw();
                    }
                    return;
                }
                if self.swipe(sx, sy, phase) {
                    return;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => {
                        self.wheel_acc += p.y;
                        if self.wheel_acc.abs() < 60.0 {
                            return;
                        }
                        let v = self.wheel_acc.signum();
                        self.wheel_acc = 0.0;
                        v
                    }
                };
                if dy == 0.0 {
                    return;
                }
                crate::trace(&format!("wheel {dy} in window {}", self.id));
                // one page per event, as 1.1.9: a fast spin packs 12 to 16 notches into one
                // event, and a page per notch jumped that far with no slide in between
                if self.primary() || self.cfg.wheel_zoom {
                    let a = Some(self.cursor);
                    self.zoom_step(dy > 0.0, a);
                } else {
                    self.mouse_bind(el, if dy > 0.0 { 5 } else { 6 });
                }
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                self.pinch_at = (!matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled)).then(Instant::now);
                self.request_redraw();
                if let Some(l) = self.layout {
                    let a = Some(self.cursor);
                    self.zoom_to(l.z * (1.0 + delta), a);
                }
            }
            _ => {}
        }
    }

    /// A sideways two-finger drag: one image per gesture, previous for a drag to the left,
    /// next for a drag to the right, once SWIPE_PX of mostly-sideways travel adds up. A new
    /// gesture starts with the touchpad's Started phase, or after SWIPE_GAP without events
    /// (Windows reports no phases). Returns true when the event was taken as a swipe.
    fn swipe(&mut self, dx: f64, dy: f64, phase: TouchPhase) -> bool {
        let now = Instant::now();
        if phase == TouchPhase::Started || self.swipe_at.is_none_or(|t| now - t > SWIPE_GAP) {
            self.swipe_acc = 0.0;
            self.swipe_done = false;
        }
        if dx.abs() <= dy.abs() {
            return false;
        }
        self.swipe_at = Some(now);
        if self.swipe_done {
            return true;
        }
        self.swipe_acc += dx * SWIPE_SIGN;
        if self.swipe_acc.abs() >= SWIPE_PX * self.sf() as f64 {
            self.swipe_done = true;
            self.step(if self.swipe_acc > 0.0 { 1 } else { -1 }, false);
        }
        true
    }

    /// Timers for this window (animation, toast, fade); returns the next deadline.
    pub fn about_to_wait(&mut self) -> Option<Instant> {
        let now = Instant::now();
        // ask whether the window is covered, twice a second (winit reports only "visible"
        // on macOS and only a minimize on Windows: stacked windows were never covered)
        #[cfg(any(windows, target_os = "macos"))]
        if self.gpu.is_some() && now >= self.occ_checked {
            self.occ_checked = shared_tick(now, Duration::from_millis(500));
            if let Some(w) = self.window.clone() {
                let minimized = w.is_minimized() == Some(true);
                self.set_covered(minimized || covered_now(&w));
            }
            // a window covered before its image arrived was not stripped then (see strip);
            // still covered half a second on, it gives the full image back now
            if self.occluded && !self.stripped && self.current.is_some() {
                self.strip();
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        let occ_at = self.gpu.is_some().then_some(self.occ_checked);
        #[cfg(not(any(windows, target_os = "macos")))]
        let occ_at: Option<Instant> = None;
        // test hook: ARKIMAGE_TEST_PINCH_MS=<n> pinches from n ms after start: a zoom step
        // every 8 ms for 1.5 s (in, then out), as trackpad events arrive
        let mut pinch_hook_at = None;
        if let Some(ms) = std::env::var("ARKIMAGE_TEST_PINCH_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let t0 = *T0.get_or_init(Instant::now);
            let start = t0 + Duration::from_millis(ms);
            let end = start + Duration::from_millis(1500);
            if now >= start && now < end {
                let mut last = LAST.lock().unwrap();
                if last.is_none_or(|l| now.duration_since(l) >= Duration::from_millis(8)) {
                    *last = Some(now);
                    if let Some(l) = self.layout {
                        let k = if now < start + Duration::from_millis(750) { 1.01 } else { 1.0 / 1.01 };
                        self.pinch_at = Some(now);
                        crate::trace("test: pinch step");
                        let c = Some((l.ww / 2.0, l.wh / 2.0));
                        self.zoom_to(l.z * k, c);
                        self.request_redraw();
                    }
                }
                pinch_hook_at = Some(now + Duration::from_millis(8));
            } else if now < start {
                pinch_hook_at = Some(start);
            }
        }
        // test hook: ARKIMAGE_TEST_NEXT_MS=<n> steps to the next image n ms after start
        let mut hook_at = None;
        if let Some(ms) = std::env::var("ARKIMAGE_TEST_NEXT_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            if T0.get_or_init(Instant::now).elapsed().as_millis() as u64 >= ms && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
                // ARKIMAGE_TEST_XF="rot,fh,fv,neg,gamma" sets the view transform instead
                if let Ok(x) = std::env::var("ARKIMAGE_TEST_XF") {
                    let v: Vec<u8> = x.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                    if v.len() == 5 {
                        self.view.rotation = v[0] % 4;
                        self.view.flip_h = v[1] != 0;
                        self.view.flip_v = v[2] != 0;
                        self.view.negative = v[3] != 0;
                        self.view.gamma = v[4] != 0;
                        self.view.user_zoom = None;
                        self.view.pan_reset = true;
                        crate::trace("test: transform set");
                        self.invalidate();
                    }
                    return hook_at;
                }
                crate::trace("test: next image");
                self.step(1, false);
            } else if !DONE.load(std::sync::atomic::Ordering::Relaxed) {
                hook_at = Some(*T0.get().unwrap() + Duration::from_millis(ms));
            }
        }
        // test hook: ARKIMAGE_TEST_FLIP_MS=<n> turns a page every 30 ms for 3 s from n ms
        // after start, as a spun mouse wheel does (the first window to ask takes it)
        if let Some(ms) = std::env::var("ARKIMAGE_TEST_FLIP_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
            static OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
            static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            let t0 = *T0.get_or_init(Instant::now);
            let (start, end) = (t0 + Duration::from_millis(ms), t0 + Duration::from_millis(ms + 3000));
            let me = self.id as u64;
            if now >= start && now < end && !self.occluded && OWNER.compare_exchange(u64::MAX, me, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed).map_or_else(|o| o == me, |_| true) {
                let mut last = LAST.lock().unwrap();
                if last.is_none_or(|l| now >= l + Duration::from_millis(30)) {
                    *last = Some(now);
                    drop(last);
                    crate::trace(&format!("test: flip in window {}", self.id));
                    self.step(1, false);
                }
                hook_at = Some(now + Duration::from_millis(30));
            } else if now < start {
                hook_at = Some(start);
            }
        }
        if self.gpu.as_mut().is_some_and(|g| g.poll_budget()) {
            self.strip();
        }
        if let Some(t) = self.next_frame_at {
            if now >= t {
                self.advance_frame(now);
            }
        }
        if let Some((_, t)) = &self.toast {
            if now >= *t {
                self.toast = None;
                self.request_redraw();
            }
        }
        // a file deleted outside the app is noticed within GONE_EVERY
        let gone_at = self.playlist.is_some().then(|| self.gone_at);
        if gone_at.is_some_and(|t| now >= t) {
            self.gone_at = shared_tick(now, GONE_EVERY);
            self.check_gone();
        }
        let gone_at = self.playlist.is_some().then(|| self.gone_at);
        // pinch settled: draw once more at full quality
        let pinch_end = self.pinch_at.map(|t| t + PINCH_SETTLE);
        if pinch_end.is_some_and(|t| now >= t) {
            self.pinch_at = None;
            self.request_redraw();
        }
        let pinch_end = self.pinch_at.map(|t| t + PINCH_SETTLE);
        // Chrome fade: one frame every 16 ms until it settles (unpaced it drew ~380 fps).
        // A covered window draws nothing, so nothing it waits for here would settle: it
        // asked again on every loop pass (44,811 times in 3 s with 100 windows), and the
        // glass check below every 16 ms per window (the idle CPU of covered windows).
        // Uncovering redraws it.
        let live = !self.occluded;
        let fade_at = (live && self.fade_active).then(|| self.fade_last + Duration::from_millis(16));
        if let Some(t) = fade_at {
            if now >= t {
                self.request_redraw();
            }
        }
        // 250 ms card and handle fades: one frame every 16 ms while any runs (a redraw asked
        // for inside a draw is dropped on Windows, so the fades are paced from here)
        let fading = self.picker.bar_anim
            || self.help_at.elapsed() < Duration::from_millis(260)
            || self.empty_at.is_some_and(|t| t.elapsed() < Duration::from_millis(260))
            || self.modal_at.is_some_and(|t| t.elapsed() < Duration::from_millis(260))
            || self.glass_fading()
            || (!self.glass_on && self.glass_loaded);
        let ui_at = (live && fading).then(|| self.fade_last + Duration::from_millis(16));
        if ui_at.is_some_and(|t| now >= t) {
            self.fade_last = now;
            self.request_redraw();
        }
        // a running slide: one frame every 8 ms until it ends
        let slide_at = self.slide.as_ref().and_then(|s| s.start).map(|_| self.slide_drawn + Duration::from_millis(8));
        if slide_at.is_some_and(|t| now >= t) {
            self.request_redraw();
        }
        if self.fs_restore_at.is_some_and(|t| now >= t) {
            self.fs_restore_at = None;
            if !self.fullscreen {
                self.toggle_fullscreen();
            }
        }
        // failsafe: an image stored for display with no paint since; ask for the paint again
        // a hidden or minimized window paints when shown, so it needs no retry
        if self.occluded || self.window.as_ref().is_some_and(|w| w.is_visible() == Some(false) || w.is_minimized() == Some(true)) {
            self.unpainted = None;
        }
        let retry_at = self.unpainted.map(|t| t + REPAINT_RETRY);
        if retry_at.is_some_and(|t| now >= t) {
            if std::env::var_os("ARKIMAGE_REDRAW_LOG").is_some() {
                eprintln!("repaint retry: no paint {} ms after the image was stored", self.unpainted.map(|t| t.elapsed().as_millis()).unwrap_or(0));
            }
            self.unpainted = Some(now);
            self.request_redraw();
        }
        [self.next_frame_at, self.toast.as_ref().map(|t| t.1), fade_at, gone_at, pinch_end, slide_at, ui_at, retry_at, self.fs_restore_at, hook_at, pinch_hook_at, occ_at]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn exiting(&mut self) {
        if !self.closed {
            self.store_geometry();
            self.cfg.save();
        }
    }
}
