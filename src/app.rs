//! Window, input, navigation and drawing.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io::Write as _,
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use image::RgbaImage;
use winit::{
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
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
fn dark_title_bar(_w: &Window) {}

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

#[cfg(not(windows))]
fn mica(_w: &Window) -> bool {
    false
}


fn menu_label(text: &str, key: &str) -> String {
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
    /// An update check finished; true when the user asked for it (Settings).
    Update(crate::update::Outcome, bool),
}

/// Chrome fade in/out duration.
const FADE_SECS: f32 = 0.25;

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
    ("Shift+Right  Shift+Left", "1 page in double view"),
    ("0  9  8", "Original size / fit window / fit width"),
    ("7  6", "Double page left-to-right / right-to-left"),
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
    ("Ctrl+1..9  Ctrl+0", "Slideshow seconds / stop"),
    ("B  Shift+B  F3", "Bookmark / open last / edit bookmarks"),
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
    pub job_tx: mpsc::Sender<Entry>,
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
        let _ = self.job_tx.send(e);
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
    pub cfg: Config,
    pending_open: Option<PathBuf>,
    playlist: Option<Playlist>,
    pos_map: HashMap<Entry, usize>,
    cache: Rc<RefCell<HashMap<Entry, Arc<Decoded>>>>,
    failed: Rc<RefCell<HashMap<Entry, String>>>,
    inflight: Rc<RefCell<HashSet<Entry>>>,
    wanted: Arc<Mutex<HashMap<u64, HashSet<Entry>>>>,
    job_tx: mpsc::Sender<Entry>,
    current: Option<(Entry, Arc<Decoded>)>,
    frame_idx: usize,
    next_frame_at: Option<Instant>,
    view: View,
    base: Option<Arc<RgbaImage>>,
    base_gen: u64,
    shown: usize,
    scaled: Option<(ScaledKey, RgbaImage)>,
    scaled_hq: bool,
    layout: Option<Layout>,
    text: Rc<RefCell<Option<Text>>>,
    toast: Option<(String, Instant)>,
    show_info: bool,
    show_name: bool,
    show_help: bool,
    slideshow: Option<(u32, Instant)>,
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
    last_click: Option<(Instant, (f64, f64))>,
    wheel_acc: f64,
    fullscreen: bool,
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
    bm_cache: Vec<String>,
    settings: Option<settings::Dialog>,
    /// The choice box whose popup menu is open in the settings dialog.
    pending_choice: Option<settings::ChoiceF>,
    /// When a popup last closed, which control opened it, and whether the cursor moved since.
    popup_closed: Option<(Instant, String, bool)>,
    /// A preset's name before renaming started (Esc puts it back).
    rename_orig: Option<String>,
    /// Hover fade per chrome control (0-1), and when it was last stepped.
    hover_fx: Vec<(&'static str, f32)>,
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

fn spawn_workers(
    n: usize,
    proxy: EventLoopProxy<UserEvent>,
    wanted: Arc<Mutex<HashMap<u64, HashSet<Entry>>>>,
    exif_rotate: Arc<AtomicBool>,
) -> mpsc::Sender<Entry> {
    let (tx, rx) = mpsc::channel::<Entry>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..n {
        let rx = rx.clone();
        let proxy = proxy.clone();
        let wanted = wanted.clone();
        let exif_rotate = exif_rotate.clone();
        std::thread::spawn(move || loop {
            let job = { rx.lock().unwrap().recv() };
            let Ok(e) = job else { break };
            if !wanted.lock().unwrap().values().any(|s| s.contains(&e)) {
                if proxy.send_event(UserEvent::Skipped(e)).is_err() {
                    break;
                }
                continue;
            }
            let res = decode::decode(&e, exif_rotate.load(Ordering::Relaxed)).map(Arc::new);
            if proxy.send_event(UserEvent::Loaded(e, res)).is_err() {
                break;
            }
        });
    }
    tx
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
            cfg,
            pending_open: arg,
            playlist: None,
            pos_map: HashMap::new(),
            cache: sh.cache.clone(),
            failed: sh.failed.clone(),
            inflight: sh.inflight.clone(),
            wanted: sh.wanted.clone(),
            job_tx: sh.job_tx.clone(),
            current: None,
            frame_idx: 0,
            next_frame_at: None,
            view: View::default(),
            base: None,
            base_gen: 0,
            shown: 1,
            scaled: None,
            scaled_hq: true,
            layout: None,
            text: sh.text.clone(),
            toast: None,
            show_info: false,
            show_name: false,
            show_help: false,
            slideshow: None,
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
            last_click: None,
            wheel_acc: 0.0,
            fullscreen: false,
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
            chrome_shown: (false, false),
            fade: (0.0, 0.0),
            fade_last: Instant::now(),
            fade_active: false,
            geom: None,
            was_max: false,
            fs_was_max: false,
            bm_cache: Vec::new(),
            settings: None,
            pending_choice: None,
            popup_closed: None,
            rename_orig: None,
            hover_fx: Vec::new(),
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

    pub fn request_redraw(&self) {
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
        self.playlist.as_ref().and_then(|p| p.entries.get(p.index).cloned())
    }

    fn next_entry(&self) -> Option<Entry> {
        self.playlist.as_ref().and_then(|p| p.entries.get(p.index + 1).cloned())
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

    fn open_path(&mut self, p: &Path) {
        // A single image: show it at once and list its folder on a thread.
        // Listing 46,000 files takes ~110 ms; the image should not wait for it.
        if p.is_file() && !source::is_archive(p) {
            if let Ok(abs) = std::path::absolute(p) {
                let dir = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                let first = Playlist { container: dir, entries: vec![Entry::File(abs.clone())], index: 0 };
                self.set_playlist(first);
                let (proxy, sort, desc) = (self.proxy.clone(), self.cfg.sort, self.cfg.sort_desc);
                std::thread::spawn(move || {
                    crate::trace("list start");
                    let r = source::open(&abs, sort, desc).map_err(|e| e.to_string());
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

    /// Swap the one-entry list for the full folder list, keeping the view as it is.
    pub fn on_listed(&mut self, e: Entry, r: Result<Playlist, String>) {
        let still_here = self.playlist.as_ref().map(|pl| pl.entries.len() == 1 && pl.entries[0] == e).unwrap_or(false);
        if !still_here {
            return;
        }
        let Ok(np) = r else { return };
        if np.entries.is_empty() {
            return;
        }
        self.pos_map = np.entries.iter().cloned().enumerate().map(|(i, e)| (e, i)).collect();
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
        self.pos_map = pl.entries.iter().cloned().enumerate().map(|(i, e)| (e, i)).collect();
        self.failed.borrow_mut().clear();
        self.rand_hist.clear();
        let idx = pl.index;
        self.playlist = Some(pl);
        self.go_to(idx);
    }

    fn update_wanted(&mut self) {
        let mut w = HashSet::new();
        if let Some(pl) = &self.playlist {
            let i = pl.index as isize;
            let d = self.dir as isize;
            for o in [-2isize, -1, 0, 1, 2, 3, 4, 5, 6, 7].map(|o| o * d) {
                let j = i + o;
                if j >= 0 && (j as usize) < pl.entries.len() {
                    w.insert(pl.entries[j as usize].clone());
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
        let _ = self.job_tx.send(e);
    }

    /// True if `e` is nearer the current index than the image now on screen.
    fn closer_than_shown(&self, e: &Entry) -> bool {
        let idx = self.index() as i64;
        let dist = |x: &Entry| self.pos_map.get(x).map(|&p| (p as i64 - idx).abs()).unwrap_or(i64::MAX);
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
        let list: Vec<Entry> = [0isize, d, 2 * d, 3 * d, -d, 4 * d, 5 * d, 6 * d]
            .iter()
            .filter_map(|o| {
                let j = i + o;
                if j >= 0 {
                    pl.entries.get(j as usize).cloned()
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
        let idx = self.playlist.as_ref().map(|p| p.index as i64).unwrap_or(0);
        self.pos_map.get(e).map(|&p| (p as i64 - idx).abs()).unwrap_or(i64::MAX)
    }

    fn set_current(&mut self, e: Entry, d: Arc<Decoded>) {
        crate::trace(&format!("current {}", e.name()));
        self.frame_idx = 0;
        self.next_frame_at = if d.frames.len() > 1 && self.cfg.double == 0 {
            Some(Instant::now() + Duration::from_millis(d.frames[0].delay_ms as u64))
        } else {
            None
        };
        self.current = Some((e, d));
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
                } else if self.cfg.double != 0 && self.next_entry().as_ref() == Some(e) {
                    self.invalidate();
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
        pl.index = i.min(pl.entries.len() - 1);
        crate::trace(&format!("go_to {}", pl.index));
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
        let len = self.len() as isize;
        if len == 0 {
            return;
        }
        let idx = self.index() as isize;
        let n = if !single && self.cfg.double != 0 {
            if n > 0 {
                n * self.shown as isize
            } else {
                n * 2
            }
        } else {
            n
        };
        let t = idx + n;
        if t >= len {
            if idx + self.shown as isize >= len || n.abs() == 1 && idx == len - 1 {
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
            2 => self.container_step(forward),
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

    fn container_step(&mut self, forward: bool) {
        let Some(pl) = &self.playlist else {
            self.msg("Move Folder: Unable to navigate (Please open a file)");
            return;
        };
        let c = pl.container.clone();
        match source::sibling(&c, forward, self.cfg.sort, self.cfg.sort_desc) {
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

    fn slideshow_tick(&mut self) {
        if self.cfg.slideshow_random {
            self.random();
            return;
        }
        let len = self.len();
        if self.index() + self.shown >= len {
            if self.cfg.slideshow_loop {
                self.go_to(0);
            } else {
                self.slideshow = None;
                self.msg("Stop Slideshow");
            }
        } else {
            self.next();
        }
    }

    fn set_slideshow(&mut self, secs: u32) {
        if secs == 0 {
            self.slideshow = None;
            self.msg("Stop Slideshow");
        } else {
            self.cfg.slideshow_secs = secs;
            self.slideshow = Some((secs, Instant::now() + Duration::from_secs(secs as u64)));
            self.msg(format!("Start Slideshow: {secs}sec"));
        }
    }

    // ---------- view ----------

    fn ensure_base(&mut self) {
        if self.base.is_some() {
            return;
        }
        let Some((_, cur)) = self.current.clone() else { return };
        let fi = self.frame_idx.min(cur.frames.len() - 1);
        let mut img = cur.frames[fi].img.clone();
        self.shown = 1;
        if self.cfg.double != 0 && self.displaying_current() {
            let sec = self.next_entry().and_then(|e| self.cache.borrow().get(&e).cloned());
            if let Some(sec) = sec {
                let b = &sec.frames[0].img;
                let portrait = |i: &RgbaImage| i.height() >= i.width();
                if portrait(&img) && portrait(b) {
                    img = Arc::new(if self.cfg.double == 1 {
                        decode::compose(&img, b)
                    } else {
                        decode::compose(b, &img)
                    });
                    self.shown = 2;
                }
            }
        }
        let v = &self.view;
        self.base = Some(decode::transform(&img, v.rotation, v.flip_h, v.flip_v, v.negative, v.gamma));
        self.base_gen += 1;
        self.scaled = None;
    }

    fn compute_layout(&mut self, ww: f64, wh: f64) -> Option<Layout> {
        let b = self.base.as_ref()?;
        let (bw, bh) = (b.width() as f64, b.height() as f64);
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
            self.view.pan_x = if self.cfg.double == 2 { sw - ww } else { (sw - ww) / 2.0 };
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

    fn set_double(&mut self, d: u8) {
        self.cfg.double = if self.cfg.double == d { 0 } else { d };
        self.view.pan_reset = true;
        if self.cfg.double != 0 {
            self.next_frame_at = None;
            if let Some(e) = self.next_entry() {
                self.request(e);
            }
        } else if let Some((e, d)) = self.current.clone() {
            self.set_current(e, d);
        }
        self.invalidate();
        self.msg(match self.cfg.double {
            1 => "Smart Double page (Left->Right)",
            2 => "Smart Double page (Left<-Right)",
            _ => "Double page: Off",
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
            self.fs_was_max = w.is_maximized();
            if self.fs_was_max {
                w.set_maximized(false);
            }
            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
        } else {
            w.set_fullscreen(None);
            if self.fs_was_max {
                w.set_maximized(true);
            }
        }
    }

    fn toggle_maximize(&mut self) {
        if self.fullscreen {
            self.toggle_fullscreen();
            return;
        }
        if let Some(w) = &self.window {
            w.set_maximized(!w.is_maximized());
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
        self.slideshow = None;
        self.next_frame_at = None;
        self.pos_map.clear();
        self.update_wanted();
        self.invalidate();
        self.update_title();
    }

    fn remove_current_from_list(&mut self) {
        let Some(pl) = &mut self.playlist else { return };
        if pl.index >= pl.entries.len() {
            return;
        }
        let e = pl.entries.remove(pl.index);
        self.cache.borrow_mut().remove(&e);
        if pl.entries.is_empty() {
            self.close();
            return;
        }
        let idx = pl.index.min(pl.entries.len() - 1);
        self.pos_map = pl.entries.iter().cloned().enumerate().map(|(i, e)| (e, i)).collect();
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
            let _ = Command::new("open").arg("-R").arg(&p).spawn();
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

    fn bookmarks_path() -> PathBuf {
        Config::dir().join("bookmarks.txt")
    }

    fn add_bookmark(&mut self) {
        let Some(e) = self.cur_entry() else { return };
        let _ = std::fs::create_dir_all(Config::dir());
        let r = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::bookmarks_path())
            .and_then(|mut f| writeln!(f, "{}", e.label()));
        match r {
            Ok(()) => self.msg(format!("Add to Bookmarks: {}", e.name())),
            Err(err) => self.msg(format!("Failed to save the bookmarks: {err}")),
        }
    }

    fn open_last_bookmark(&mut self) {
        let Ok(s) = std::fs::read_to_string(Self::bookmarks_path()) else {
            self.msg("Failed to open the bookmarks.");
            return;
        };
        let Some(line) = s.lines().map(str::trim).filter(|l| !l.is_empty()).last() else {
            self.msg("Failed to open the bookmarks.");
            return;
        };
        let line = line.to_string();
        self.open_bookmark_line(&line);
    }

    fn open_bookmark_line(&mut self, line: &str) {
        self.open_label(line);
        self.msg(format!("Open recently saved bookmark: {line}"));
    }

    /// Open an entry label: a file path, or "<archive>|<name inside>" (bookmarks, crash restore).
    pub fn open_label(&mut self, line: &str) {
        let line = line.to_string();
        match line.split_once('|') {
            Some((arch, name)) => {
                self.open_path(Path::new(arch));
                let pos = self.playlist.as_ref().and_then(|pl| {
                    pl.entries.iter().position(|e| matches!(e, Entry::Zip { name: n, .. } if n == name))
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

    fn edit_bookmarks(&mut self) {
        let p = Self::bookmarks_path();
        if !p.exists() {
            let _ = std::fs::create_dir_all(Config::dir());
            let _ = std::fs::write(&p, "");
        }
        open_text_file(&p);
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
                let e = &pl.entries[pl.index];
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
        let e = pl.entries.get(pl.index)?;
        let mut secs = vec![vec![
            row("Folder", e.fs_path().parent().map(|p| p.display().to_string()).unwrap_or_default(), false),
            row("Index", format!("{} / {}", pl.index + 1, pl.entries.len()), true),
        ]];
        if let Some((ce, d)) = &self.current {
            if ce == e {
                let mut img = vec![
                    row("Dimensions", format!("{} × {}", d.orig_w, d.orig_h), true),
                    row("Format", if d.frames.len() > 1 { format!("{}, {} frames", d.format, d.frames.len()) } else { d.format.to_string() }, false),
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
        let Some(e) = pl.entries.get(pl.index) else { return v };
        v.push(format!("File Name: {}", e.name()));
        v.push(format!("Path: {}", e.fs_path().display()));
        v.push(format!("Index: {} / {}", pl.index + 1, pl.entries.len()));
        if let Some((ce, d)) = &self.current {
            if ce == e {
                let frames = if d.frames.len() > 1 { format!("  {} frames", d.frames.len()) } else { String::new() };
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

    fn render(&mut self) {
        let Some(win) = self.window.clone() else { return };
        let sz = win.inner_size();
        let (w, h) = (sz.width.max(1), sz.height.max(1));
        let Some(mut surface) = self.surface.take() else { return };
        if surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).is_ok() {
            if let Ok(mut buf) = surface.buffer_mut() {
                {
                    let mut c = Canvas { buf: &mut buf, w: w as usize, h: h as usize };
                    crate::trace("draw start");
                    self.draw(&mut c);
                }
                let _ = buf.present();
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
                self.picker.draw(c, t.as_mut(), &pl.entries, pl.index, sf, &self.thumb_jobs, &mc);
            }
            *self.text.borrow_mut() = t;
            return;
        }
        c.fill(self.cfg.bg_color);
        self.ensure_base();
        let bg = self.cfg.bg_color;
        let checker = self.cfg.transparent_grid;
        if let Some(l) = self.compute_layout(c.w as f64, c.h as f64) {
            let base = self.base.clone().unwrap();
            let preset = self.preset();
            // Enlarged images go through the resize path too when the preset sharpens,
            // up to 4x and 40 MP; past that, per-pixel sampling (no sharpening).
            let up_sharp = preset.amount > 0 && preset.smooth() && l.z <= 4.0 && l.sw * l.sh <= 40e6;
            if l.z < 0.999 || (l.z > 1.001 && up_sharp) {
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
                render::blit_sample(c, &base, l.ox, l.oy, l.z, self.preset().smooth(), bg, checker);
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

        if self.playlist.is_none() {
            crate::ui::empty_card(c, &mut t, cw, ch, sf);
        }
        if self.picker.open {
            if let Some(pl) = &self.playlist {
                let cache = &self.cache;
                let mc = |e: &Entry| cache.borrow().get(e).map(|d| d.frames[0].img.clone());
                self.picker.draw(c, Some(&mut t), &pl.entries, pl.index, sf, &self.thumb_jobs, &mc);
            }
            *self.text.borrow_mut() = Some(t);
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
            self.modal_hits = crate::ui::modal_card(c, &mut t, sf, &title, &body, &buttons, self.modal_hover);
        }
        if let Some((a, b)) = self.zoom_rect {
            // zoom rectangle: a light blue tint with a 1 px edge
            let (x0, y0) = (a.0.min(b.0) as f32, a.1.min(b.1) as f32);
            let (w, h) = ((a.0 - b.0).abs() as f32, (a.1 - b.1).abs() as f32);
            c.fill_rrect(x0, y0, w, h, 0.0, crate::ui::ACCENT, 0.15);
            c.stroke_rrect(x0, y0, w, h, 0.0, sf.max(1.0), crate::ui::ACCENT_LIGHT, 1.0);
        }
        if self.show_help {
            let rows: Vec<(String, String)> = HELP.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect();
            crate::ui::help_card(c, &mut t, sf, "Hotkeys", &rows);
        }
        self.hits.clear();
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
        let hov = self.hover;
        for (k, a) in &mut self.hover_fx {
            let to = if Some(*k) == hov { 1.0 } else { 0.0 };
            *a = if (*a - to).abs() <= step { to } else { *a + step * (to - *a).signum() };
        }
        self.hover_fx.retain(|(k, a)| *a > 0.0 || Some(*k) == hov);
        if self.hover_fx.iter().any(|(k, a)| *a != if Some(*k) == hov { 1.0 } else { 0.0 }) {
            self.request_redraw();
        }
        let hover_fx = self.hover_fx.clone();
        let st = chrome::State {
            sf,
            title: &title,
            show_top,
            show_bottom,
            maximized: self.window.as_ref().map(|w| w.is_maximized()).unwrap_or(false),
            lock_top: self.cfg.lock_top,
            lock_bottom: self.cfg.lock_bottom,
            hover_fx: &hover_fx,
            info_on: self.show_info,
            index: self.index(),
            len: self.len(),
            anim: self.anim_frames().map(|n| (self.frame_idx.min(n - 1), n)),
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
            "next1" => self.step(1, true),
            "prev1" => self.step(-1, true),
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
                self.show_menu(el, "context", x, y);
            }
            other => self.menu_action(el, other),
        }
    }

    /// Show an update result: in Settings when it is open, else over the image.
    /// A check the user did not ask for shows only a ready update.
    pub fn on_update(&mut self, o: &crate::update::Outcome, manual: bool) {
        use crate::{install::Card, update::Outcome};
        let card = match o {
            // the new exe is in place: restart now; the session reopens in the new version
            Outcome::Ready(v) => {
                crate::trace(&format!("update {v} ready: restarting"));
                crate::update::request_restart();
                return;
            }
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
                Some('[') | Some('{') => return self.container_step(false),
                Some(']') | Some('}') => return self.container_step(true),
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
            K::ArrowRight if sh => self.step(1, true),
            K::ArrowLeft if sh => self.step(-1, true),
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

            K::Digit0 | K::Numpad0 if p => self.set_slideshow(0),
            K::Digit1 if p => self.set_slideshow(1),
            K::Digit2 if p => self.set_slideshow(2),
            K::Digit3 if p => self.set_slideshow(3),
            K::Digit4 if p => self.set_slideshow(4),
            K::Digit5 if p => self.set_slideshow(5),
            K::Digit6 if p => self.set_slideshow(6),
            K::Digit7 if p => self.set_slideshow(7),
            K::Digit8 if p => self.set_slideshow(8),
            K::Digit9 if p => self.set_slideshow(9),
            K::Digit0 | K::Numpad0 => self.set_mode(0),
            K::Digit9 | K::Numpad9 => self.set_mode(1),
            K::Digit8 | K::Numpad8 => self.set_mode(2),
            K::Digit7 | K::Numpad7 => self.set_double(1),
            K::Digit6 | K::Numpad6 => self.set_double(2),

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
                self.request_redraw();
            }
            K::KeyA if p => {
                self.on_top = !self.on_top;
                if let Some(w) = &self.window {
                    w.set_window_level(if self.on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
                }
                self.msg(if self.on_top { "Always On Top: On" } else { "Always On Top: Off" });
            }
            K::KeyB if sh => self.open_last_bookmark(),
            K::KeyB => self.add_bookmark(),
            K::F3 => self.edit_bookmarks(),
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
        let anim = self.anim_frames().is_some();
        let bottom = self.cfg.lock_bottom || empty || self.slider_drag || self.anim_drag || (self.cursor_in && y > ch - chrome::bottom_zone(sf, anim));
        (top, bottom)
    }

    fn chrome_title(&self) -> String {
        match &self.playlist {
            Some(pl) if !pl.entries.is_empty() => {
                let folder = pl.container.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                format!("{folder} > {} [{}/{}] - ArkImage", pl.entries[pl.index].name(), pl.index + 1, pl.entries.len())
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
        if self.fullscreen || w.is_maximized() {
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
            "tb_view" | "tb_slideshow" | "tb_bookmark" | "tb_edit" | "tb_copyto" => {
                self.show_menu(el, &h.id[3..], h.x0, h.y1);
            }
            "bar_open" => self.open_dialog(),
            "bar_menu" => self.show_menu(el, "context", h.x0, h.y0),
            "bar_slider" => {
                self.slider_drag = true;
                self.slider_seek();
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
                C("dbl1".into(), "Smart Double page (Left->Right)".into(), "7", cfg.double == 1),
                C("dbl2".into(), "Smart Double page (Left<-Right)".into(), "6", cfg.double == 2),
            ]
        };
        match kind {
            "view" => view_items(),
            "slideshow" => {
                let cur = self.slideshow.map(|s| s.0).unwrap_or(0);
                let mut m = vec![C("ss:0".into(), "Stop Slideshow".into(), "Ctrl+0", cur == 0), S];
                for s in [1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 15, 20, 25, 30, 60, 90] {
                    let label = if s == 1 { "1 second".to_string() } else { format!("{s} seconds") };
                    let key = if s < 10 { ["", "Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7", "Ctrl+8", "Ctrl+9"][s as usize] } else { "" };
                    m.push(C(format!("ss:{s}"), label, key, cur == s));
                }
                m.extend([S, C("ssloop".into(), "Repeat".into(), "", cfg.slideshow_loop), C("ssrand".into(), "Random".into(), "", cfg.slideshow_random)]);
                m
            }
            "bookmark" => {
                self.bm_cache = std::fs::read_to_string(Self::bookmarks_path())
                    .unwrap_or_default()
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                let mut m = vec![
                    I("bmadd".into(), "Add to Bookmarks".into(), "B", has),
                    I("bmdel".into(), "Delete All Bookmarks".into(), "", !self.bm_cache.is_empty()),
                    I("bmedit".into(), "Edit Bookmarks".into(), "F3", true),
                ];
                if !self.bm_cache.is_empty() {
                    m.push(S);
                    let start = self.bm_cache.len().saturating_sub(30);
                    for i in (start..self.bm_cache.len()).rev() {
                        m.push(I(format!("bm:{i}"), self.bm_cache[i].replace('&', "&&"), "", true));
                    }
                }
                m
            }
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
                C("f0".into(), "No Filter".into(), "U", cfg.filter == 0),
                C("f1".into(), "Soften".into(), "I", cfg.filter == 1),
                C("f2".into(), "Soften+Sharpen".into(), "S", cfg.filter == 2),
                Sub("Filter Presets".into(), {
                    let mut v: Vec<MenuEntry> = crate::filters::all(&cfg.filter_presets)
                        .into_iter()
                        .enumerate()
                        .skip(3)
                        .map(|(i, p)| C(format!("f{i}"), p.name, "", cfg.filter as usize == i))
                        .collect();
                    v.push(S);
                    v.push(I("fedit".into(), "Edit Filter Presets...".into(), "", true));
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
                Sub("Interpolation".into(), vec![
                    C("ip0".into(), "Bilinear Interpolation (Fast)".into(), "", cfg.interp == 0),
                    C("ip1".into(), "Bicubic Interpolation (Slow, High Quality)".into(), "", cfg.interp == 1),
                    C("ip2".into(), "Lanczos Interpolation (Slow, High Quality)".into(), "", cfg.interp == 2),
                    C("ip3".into(), "Lanczos4 Interpolation (Sharpest)".into(), "", cfg.interp == 3),
                    C("ip4".into(), "Mitchell Interpolation (No Halos)".into(), "", cfg.interp == 4),
                ]),
                Sub("Image Effect".into(), vec![
                    C("neg".into(), "Reverse Image".into(), "Ctrl+I", v.negative),
                    C("gamma".into(), "Gamma Compensation".into(), "Ctrl+G", v.gamma),
                    C("flipv".into(), "Flip Vertical".into(), "Ctrl+F", v.flip_v),
                    C("fliph".into(), "Flip Horizontal".into(), "Ctrl+M", v.flip_h),
                ]),
                Sub("Sort by".into(), vec![
                    C("sort0".into(), "Name".into(), "", cfg.sort == 0),
                    C("sort1".into(), "Date modified".into(), "", cfg.sort == 1),
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
                    I("zin1".into(), "Zoom In(1%)".into(), "Ctrl++", has),
                    I("zout1".into(), "Zoom Out(1%)".into(), "Ctrl+-", has),
                ]),
                Sub("Rotate".into(), vec![
                    C("rot0".into(), "Do not rotate".into(), "Alt+Up", v.rotation == 0),
                    C("rotl".into(), "Rotate 90˚ counter-clockwise".into(), "Alt+Left", v.rotation == 3),
                    C("rotr".into(), "Rotate 90˚ clockwise".into(), "Alt+Right", v.rotation == 1),
                    C("rot180".into(), "180˚".into(), "Alt+Down", v.rotation == 2),
                ]),
                S,
                C("info".into(), "File/EXIF information".into(), "TAB", self.show_info),
                I("delete".into(), "Delete".into(), "Del", has),
                I("copy1".into(), "Copy to Photo Folder".into(), "Ins", has),
                I("editor".into(), "Open with Image Editor".into(), "Ctrl+E", has),
                I("clip".into(), "Copy to Clipboard".into(), "Ctrl+C", has),
                S,
                I("config".into(), "Settings".into(), "F5", true),
                I("about".into(), "About ArkImage".into(), "F1", true),
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
            return;
        }
        self.popup_show(el, w, items, x, y);
        self.popup_closed = Some((Instant::now(), opener.to_string(), false));
    }

    fn popup_show(&mut self, el: &ActiveEventLoop, w: &Arc<Window>, items: Vec<MenuEntry>, x: f32, y: f32) {
        use muda::ContextMenu;
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
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
                    menu.show_context_menu_for_nsview(h.ns_view.as_ptr() as _, Some(pos));
                },
                _ => {}
            }
        }
        self.drain_menu_events(el);
        self.request_redraw();
    }

    // ---------- configuration dialog ----------

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
                let Some((x, y, w)) = self.settings.as_ref().and_then(|s| s.anchor(a).map(|(x, y)| (x, y, s.window.clone()))) else { return };
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
                let Some((x, y, w)) = self.settings.as_ref().and_then(|s| s.anchor(a).map(|(x, y)| (x, y, s.window.clone()))) else { return };
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
            Act::CheckUpdate => {
                if let Some(s) = &mut self.settings {
                    s.card = Some(crate::install::Card::Checking);
                    s.card_hover = None;
                }
                crate::update::spawn_check(self.proxy.clone(), true);
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
        if c.view_mode != before.view_mode || c.stretch_small != before.stretch_small || c.double != before.double || c.align_top != before.align_top {
            self.view.user_zoom = None;
            self.view.pan_reset = true;
            if c.double != 0 {
                self.next_frame_at = None;
                self.preload();
            }
        }
        // live preview: redraw the viewer with the changed filter preset
        // (the scaled-image key holds the preset's fingerprint, so a redraw re-filters)
        if c.filter != before.filter || c.filter_presets != before.filter_presets || c.interp != before.interp {
            self.request_redraw();
        }
        if c.thumb_cache_mb != before.thumb_cache_mb {
            self.thumb_jobs.disk.store(c.thumb_cache_mb > 0, Ordering::Relaxed);
        }
        if c.cache_mb != before.cache_mb {

        }
        if let Some((secs, t)) = self.slideshow {
            if c.slideshow_secs != before.slideshow_secs {
                let _ = (secs, t);
                self.slideshow = Some((c.slideshow_secs, Instant::now() + Duration::from_secs(c.slideshow_secs as u64)));
            }
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

    fn resort(&mut self) {
        let Some(pl) = &self.playlist else { return };
        let c = pl.container.clone();
        let cur = self.cur_entry();
        if let Ok(mut np) = source::open(&c, self.cfg.sort, self.cfg.sort_desc) {
            if let Some(i) = cur.and_then(|e| np.entries.iter().position(|x| *x == e)) {
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
        if let Some(n) = id.strip_prefix("ss:") {
            self.set_slideshow(n.parse().unwrap_or(0));
            return;
        }
        if let Some(n) = id.strip_prefix("bm:") {
            if let Some(line) = n.parse::<usize>().ok().and_then(|i| self.bm_cache.get(i).cloned()) {
                self.open_bookmark_line(&line);
            }
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
            "nextdir" => self.container_step(true),
            "prevdir" => self.container_step(false),
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
            "dbl1" => self.set_double(1),
            "dbl2" => self.set_double(2),
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
            "ssloop" => self.cfg.slideshow_loop = !self.cfg.slideshow_loop,
            "ssrand" => self.cfg.slideshow_random = !self.cfg.slideshow_random,
            "bmadd" => self.add_bookmark(),
            "bmedit" => self.edit_bookmarks(),
            "bmdel" => {
                if crate::dialog::ask(self.window.as_deref(), "ArkImage", "Do you want to delete all the saved bookmarks?", crate::dialog::Kind::Warn) {
                    let _ = std::fs::write(Self::bookmarks_path(), "");
                }
            }
            "config" => self.open_settings(el, 0),
            "about" => {
                let text = format!(
                    "ArkImage {}\nA fast, light, multi-instance image viewer.\nOpen source (MIT). Press F1 in the window for hotkeys.",
                    env!("CARGO_PKG_VERSION")
                );
                crate::dialog::tell(self.window.as_deref(), "About ArkImage", &text, crate::dialog::Kind::Info);
            }
            "exit" => self.exit(el),
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
        let max = w.is_maximized();
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
        if let Some(name) = w.current_monitor().and_then(|m| m.name()) {
            self.cfg.win_monitor = name;
        }
    }

    /// Close this window. The master drops closed viewers and quits after the last one.
    fn exit(&mut self, _el: &ActiveEventLoop) {
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
        let n = self.current.as_ref()?.1.frames.len();
        (n > 1 && self.cfg.double == 0).then_some(n)
    }

    /// Scrub: show the frame under the cursor and hold it while the button is down.
    fn anim_seek(&mut self) {
        let Some(n) = self.anim_frames() else { return };
        let Some(sz) = self.window.as_ref().map(|w| w.inner_size()) else { return };
        let (tx, _, tw, _) = chrome::anim_track(sz.width as f32, sz.height as f32, self.sf());
        let frac = ((self.cursor.0 as f32 - tx) / tw).clamp(0.0, 1.0);
        let f = ((frac * n as f32) as usize).min(n - 1);
        self.next_frame_at = None;
        if f != self.frame_idx {
            self.frame_idx = f;
            self.base = None;
        }
        self.request_redraw();
    }

    fn advance_frame(&mut self, now: Instant) {
        let Some(d) = self.current.as_ref().map(|c| c.1.clone()) else { return };
        let n = d.frames.len();
        if n < 2 || self.cfg.double != 0 {
            self.next_frame_at = None;
            return;
        }
        self.frame_idx = (self.frame_idx + 1) % n;
        let delay = d.frames[self.frame_idx].delay_ms.max(10) as u64;
        // Keep cadence; do not fall behind after a slow frame.
        let mut next = self.next_frame_at.unwrap_or(now) + Duration::from_millis(delay);
        if next < now {
            next = now + Duration::from_millis(delay);
        }
        self.next_frame_at = Some(next);
        self.base = None;
        self.request_redraw();
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
        let ctx = softbuffer::Context::new(win.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&ctx, win.clone()).expect("softbuffer surface");
        self.window = Some(win);
        self._context = Some(ctx);
        self.surface = Some(surface);
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
        if exact.as_ref().map(|s| s.fs).unwrap_or(false) {
            self.toggle_fullscreen();
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
        match ev {
            WindowEvent::CloseRequested => self.exit(el),
            WindowEvent::Moved(_) => self.track_geometry(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.track_geometry();
                self.scaled = None;
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render(),
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
                let h = self.picker_index_at_cursor();
                if h != self.picker.hover {
                    self.picker.hover = h;
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
                    if let Some(i) = self.picker_index_at_cursor() {
                        self.go_to(i);
                        self.close_picker();
                    }
                }
                (MouseButton::Left, ElementState::Released) if self.picker.open => {}
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
                                self.toggle_maximize();
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
                            let ms = d.frames[self.frame_idx.min(d.frames.len() - 1)].delay_ms.max(10) as u64;
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
            WindowEvent::MouseWheel { delta, .. } => {
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
                if self.primary() || self.cfg.wheel_zoom {
                    let a = Some(self.cursor);
                    self.zoom_step(dy > 0.0, a);
                } else {
                    self.mouse_bind(el, if dy > 0.0 { 5 } else { 6 });
                }
            }
            WindowEvent::PinchGesture { delta, .. } => {
                if let Some(l) = self.layout {
                    let a = Some(self.cursor);
                    self.zoom_to(l.z * (1.0 + delta), a);
                }
            }
            _ => {}
        }
    }

    /// Timers for this window (animation, slideshow, toast, fade); returns the next deadline.
    pub fn about_to_wait(&mut self) -> Option<Instant> {
        let now = Instant::now();
        if let Some(t) = self.next_frame_at {
            if now >= t {
                self.advance_frame(now);
            }
        }
        if let Some((secs, t)) = self.slideshow {
            if now >= t {
                self.slideshow = Some((secs, now + Duration::from_secs(secs as u64)));
                self.slideshow_tick();
            }
        }
        if let Some((_, t)) = &self.toast {
            if now >= *t {
                self.toast = None;
                self.request_redraw();
            }
        }
        // Chrome fade: one frame every 16 ms until it settles (unpaced it drew ~380 fps).
        let fade_at = self.fade_active.then(|| self.fade_last + Duration::from_millis(16));
        if let Some(t) = fade_at {
            if now >= t {
                self.request_redraw();
            }
        }
        [self.next_frame_at, self.slideshow.map(|s| s.1), self.toast.as_ref().map(|t| t.1), fade_at]
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
