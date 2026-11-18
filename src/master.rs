//! The master: one invisible owner of every ArkImage window in this process.
//! It holds the shared state (decoded-image cache, decode and thumbnail threads, font),
//! routes window events to their window, keeps one config for all windows, evicts the
//! shared cache by the smallest distance any window has to an image, and opens a new
//! window when another launch hands it a file.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy},
    window::WindowId,
};

use crate::{
    app::{App, Place, Shared, UserEvent},
    backup,
    config::Config,
};

pub struct Master {
    sh: Shared,
    cfg: Config,
    windows: Vec<App>,
    next_id: u64,
    /// Files to open once the event loop is running.
    pending: Vec<Option<PathBuf>>,
    started: bool,
    guard: Option<backup::Guard>,
    /// Windows to restore exactly, after a takeover.
    restore: Vec<backup::WinState>,
    last_session: Vec<backup::WinState>,
    last_save: Instant,
    /// Test hook: ARKIMAGE_CRASH_AFTER_MS makes the master panic after that long.
    crash_at: Option<Instant>,
    /// Test hook: ARKIMAGE_TEST_SPEED_MENU opens the speed menu of the first window after 3 s.
    speed_menu_at: Option<Instant>,
    hang_at: Option<Instant>,
}

impl Master {
    pub fn new(proxy: EventLoopProxy<UserEvent>, arg: Option<PathBuf>, takeover: Option<backup::Takeover>) -> Self {
        let cfg = Config::load();
        let sh = Shared::new(proxy, &cfg);
        let crashes = takeover.as_ref().map(|t| t.crashes).unwrap_or(0);
        let restore = takeover.map(|t| t.session).unwrap_or_default();
        let hang_at = std::env::var("ARKIMAGE_HANG_AFTER_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        let crash_at = std::env::var("ARKIMAGE_CRASH_AFTER_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        let mut m = Self {
            sh,
            cfg,
            windows: Vec::new(),
            next_id: 1,
            pending: Vec::new(),
            started: false,
            guard: backup::Guard::start(crashes),
            restore,
            last_session: Vec::new(),
            last_save: Instant::now(),
            crash_at,
            speed_menu_at: std::env::var_os("ARKIMAGE_TEST_SPEED_MENU").or(std::env::var_os("ARKIMAGE_TEST_CHOICE")).or(std::env::var_os("ARKIMAGE_TEST_CLICK")).or(std::env::var_os("ARKIMAGE_TEST_PICKER")).or(std::env::var("ARKIMAGE_TEST_FULLSCREEN").ok().filter(|v| v == "3").map(Into::into)).map(|_| Instant::now() + Duration::from_secs(3)),
            hang_at,
        };
        if m.restore.is_empty() {
            // the first window starts decoding before the event loop runs (see App::new)
            m.add_window(arg);
        } else {
            for _ in 0..m.restore.len() {
                m.add_window(None);
            }
            if arg.is_some() {
                m.pending.push(arg);
            }
        }
        m
    }

    /// Keep session.txt current for the backup (at most once a second).
    fn save_session(&mut self, force: bool) {
        if !force && self.last_save.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_save = Instant::now();
        let s: Vec<_> = self.windows.iter().filter_map(|w| w.win_state()).filter(|s| !s.label.is_empty()).collect();
        if s != self.last_session && !s.is_empty() {
            backup::save_session(&s);
            self.last_session = s;
        }
    }

    fn add_window(&mut self, arg: Option<PathBuf>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.windows.push(App::new(&self.sh, id, self.cfg.clone(), arg));
        self.update_glass();
        self.windows.len() - 1
    }

    /// Open a window for a file handed over by another launch.
    fn open(&mut self, el: &ActiveEventLoop, arg: Option<PathBuf>) {
        // Follow the newest window: maximized on its monitor if it is, else cascade from it.
        let newest = self.windows.iter().rev().find_map(|w| w.win_state().map(|s| (s, w.outer_position())));
        let place = match newest {
            Some((s, _)) if s.max => Place::Exact(backup::WinState { fs: false, label: String::new(), ..s }),
            Some((_, Some((x, y)))) => Place::Cascade(x, y),
            _ => Place::Saved,
        };
        let i = self.add_window(arg);
        self.windows[i].resumed(el, place);
        self.windows[i].focus();
    }

    /// One config for all windows: a change made in any window reaches the others.
    /// Frosted glass only while one window is open: a second window fades the first to
    /// the solid colour, and the last one left fades back to glass. A new window starts
    /// in its state at once.
    fn update_glass(&mut self) {
        let one = self.windows.len() == 1;
        let n = self.windows.len();
        for (i, w) in self.windows.iter_mut().enumerate() {
            w.set_glass(one, i + 1 == n && w.is_new());
        }
    }

    /// Give every window in a folder whose list changed the master's new list.
    fn adopt_lists(&mut self) {
        for d in crate::source::changed() {
            let mut busy = false;
            for w in &mut self.windows {
                busy |= !w.adopt(&d);
            }
            if busy {
                crate::source::retry_changed(&d);
            }
        }
    }

    fn sync_config(&mut self) {
        let changed = self.windows.iter().position(|w| w.cfg != self.cfg);
        if let Some(i) = changed {
            self.cfg = self.windows[i].cfg.clone();
            for (k, w) in self.windows.iter_mut().enumerate() {
                if k != i && w.cfg != self.cfg {
                    w.set_config(self.cfg.clone());
                }
            }
        }
    }

    /// Keep the shared cache under config::cache_mb(), dropping the images farthest from every window.
    fn evict(&mut self) {
        let budget = crate::config::cache_mb() * 1024 * 1024;
        let mut cache = self.sh.cache.borrow_mut();
        let mut total: u64 = cache.values().map(|d| d.bytes()).sum();
        if total <= budget {
            return;
        }
        let mut by_dist: Vec<(i64, _)> = cache
            .keys()
            .map(|e| (self.windows.iter().map(|w| w.distance(e)).min().unwrap_or(i64::MAX), e.clone()))
            .collect();
        by_dist.sort_by(|a, b| b.0.cmp(&a.0));
        for (d, e) in by_dist {
            if total <= budget || d == 0 {
                break;
            }
            if let Some(x) = cache.remove(&e) {
                total -= x.bytes();
            }
        }
    }

    /// A window asked to select every open image of its folder in the file manager.
    fn reveal_all(&mut self) {
        let Some(i) = self.windows.iter().position(|w| w.reveal_all) else { return };
        self.windows[i].reveal_all = false;
        let Some(dir) = self.windows[i].cur_fs_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
        let mut files: Vec<PathBuf> = self
            .windows
            .iter()
            .filter_map(|w| w.cur_fs_path())
            .filter(|p| p.parent() == Some(dir.as_path()))
            .collect();
        files.sort();
        files.dedup();
        crate::trace(&format!("reveal {} files in {}", files.len(), dir.display()));
        crate::reveal::select_in_folder(&dir, &files);
    }

    /// Drop closed windows; quit after the last one.
    fn reap(&mut self, el: &ActiveEventLoop) {
        let before = self.windows.len();
        let wanted = self.sh.wanted.clone();
        self.windows.retain(|w| {
            if w.closed {
                wanted.lock().unwrap().remove(&w.id);
            }
            !w.closed
        });
        if self.windows.len() != before {
            self.evict();
            self.update_glass();
        }
        if self.windows.is_empty() && self.started {
            el.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for Master {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        let takeover = !self.restore.is_empty();
        let mut restore = std::mem::take(&mut self.restore).into_iter();
        for w in &mut self.windows {
            w.resumed(el, restore.next().map(Place::Exact).unwrap_or(Place::Saved));
        }
        self.save_session(true);
        crate::trace(&format!(
            "install offer check: takeover {takeover}, offered {}, running installed {}, installed {}",
            self.cfg.install_offered,
            crate::install::running_installed(),
            crate::install::is_installed()
        ));
        // Every start of the master: check GitHub for a newer version (in the background).
        // Later launches hand their files to this master and exit, so they never check;
        // a backup taking over after a crash is not a new start and does not check either.
        if self.cfg.auto_update && !takeover {
            crate::trace("update check at master start");
            let now = crate::update::now_unix();
            self.cfg.update_checked = now;
            for w in &mut self.windows {
                w.cfg.update_checked = now;
            }
            self.cfg.save();
            crate::update::spawn_check(self.sh.proxy.clone(), false);
        }
        // Test hook: ARKIMAGE_TEST_MAKE_DEFAULT runs Settings' "Make Default".
        #[cfg(target_os = "macos")]
        if std::env::var_os("ARKIMAGE_TEST_MAKE_DEFAULT").is_some() {
            crate::mac::spawn_make_default(self.sh.proxy.clone());
        }
        // Test hook: ARKIMAGE_TEST_FULLSCREEN=1 enters full screen, =2 maximizes (zooms),
        // in the first window, as the toolbar button and a title double-click do.
        if let Ok(v) = std::env::var("ARKIMAGE_TEST_FULLSCREEN") {
            if let Some(w) = self.windows.first_mut() {
                // =4 presses the green title-bar button instead
                if v == "4" {
                    #[cfg(target_os = "macos")]
                    w.test_press_green();
                } else {
                    w.test_fullscreen(v == "2");
                }
            }
        }
        // Test hook: ARKIMAGE_TEST_SETTINGS opens Settings > General in the first window.
        if std::env::var_os("ARKIMAGE_TEST_SETTINGS").is_some() {
            if let Some(w) = self.windows.first_mut() {
                w.test_open_settings(el);
            }
        }
        // First run of a copy that is not installed: offer to install, once.
        if cfg!(windows)
            && !takeover
            && !self.cfg.install_offered
            && !crate::install::running_installed()
            && !crate::install::is_installed()
        {
            self.cfg.install_offered = true;
            for w in &mut self.windows {
                w.cfg.install_offered = true;
            }
            self.cfg.save();
            // a card drawn in the window, not a message box (one opened this early never
            // appeared and blocked the window)
            if let Some(w) = self.windows.first_mut() {
                w.show_install(false);
            }
        }
        for arg in std::mem::take(&mut self.pending) {
            self.open(el, arg);
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        #[cfg(windows)]
        crate::stackdump::beat();
        match ev {
            UserEvent::Loaded(e, res) => {
                self.sh.inflight.borrow_mut().remove(&e);
                // decoded to the GPU while a window still used it, arriving after every
                // window went to the processor: RAM holds only its 512 px thumbnail, which
                // the processor drew as the image (the first image of a lone window stayed
                // pixelated). Decode it again for the processor.
                let stale_gpu = res.as_ref().is_ok_and(|d| d.frames.iter().any(|f| f.gpu.is_some())) && !crate::gpu::DECODE_TO_GPU.load(std::sync::atomic::Ordering::Relaxed);
                if stale_gpu {
                    crate::trace(&format!("decoded to the GPU after the switch to the processor: decoding {} again", e.name()));
                    if self.sh.is_wanted(&e) {
                        self.sh.request(e);
                    }
                    return;
                }
                match &res {
                    Ok(d) => {
                        self.sh.cache.borrow_mut().insert(e.clone(), d.clone());
                    }
                    Err(m) => {
                        self.sh.failed.borrow_mut().insert(e.clone(), m.clone());
                    }
                }
                for w in &mut self.windows {
                    w.on_loaded_view(&e, &res);
                }
                self.evict();
            }
            UserEvent::Skipped(e) => {
                self.sh.inflight.borrow_mut().remove(&e);
                if self.sh.is_wanted(&e) {
                    self.sh.request(e);
                }
            }
            UserEvent::Font(t) => {
                *self.sh.text.borrow_mut() = t;
                for w in &mut self.windows {
                    w.request_redraw();
                }
            }
            UserEvent::Thumb(e, t) => {
                crate::trace("thumb");
                self.sh.thumb_jobs.done(&e);
                for w in &mut self.windows {
                    w.on_thumb(&e, &t);
                }
            }
            UserEvent::Listed(e, r) => {
                // Only the window still showing `e` alone takes the list. Cloning it for
                // every window hung the master: 100 windows in a 46,000-file folder made
                // 100 copies of the list per listing.
                for w in &mut self.windows {
                    if w.wants_listed(&e) {
                        w.on_listed(e.clone(), r.clone());
                    }
                }
                self.adopt_lists();
            }
            UserEvent::Update(o, manual) => {
                // shown once: in the window whose Settings asked, else the first window
                let i = self.windows.iter().position(|w| w.settings_open()).unwrap_or(0);
                if let Some(w) = self.windows.get_mut(i) {
                    w.on_update(&o, manual);
                }
            }
            UserEvent::MadeDefault(msg) => {
                // the test hook only traces; a click in Settings shows the result
                if std::env::var_os("ARKIMAGE_TEST_MAKE_DEFAULT").is_none() {
                    crate::dialog::tell(None, "Default App", &msg, crate::dialog::Kind::Info);
                }
            }
            UserEvent::Open(arg) => {
                // Finder launches the app first and sends the file after: fill the blank
                // first window instead of opening a second one beside it.
                if let (true, Some(p)) = (self.started, arg.as_ref()) {
                    if let [w] = self.windows.as_mut_slice() {
                        if w.is_blank() {
                            w.open_external(p);
                            w.focus();
                            return;
                        }
                    }
                }
                if self.started {
                    self.open(el, arg);
                } else {
                    self.pending.push(arg);
                }
            }
        }
        self.sync_config();
        self.reveal_all();
        self.reap(el);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        #[cfg(windows)]
        crate::stackdump::beat();
        if let Some(w) = self.windows.iter_mut().find(|w| w.owns(id)) {
            w.window_event(el, id, ev);
        }
        self.sync_config();
        self.adopt_lists();
        self.reveal_all();
        self.reap(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        #[cfg(windows)]
        crate::stackdump::beat();
        // Test hook: ARKIMAGE_TEST_STALL stops the main thread once for 5 s (the watchdog's check)
        static STALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if std::env::var_os("ARKIMAGE_TEST_STALL").is_some() && self.started && !STALLED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(5));
        }
        // a window found its file gone and read the folder again
        self.adopt_lists();
        if self.speed_menu_at.is_some_and(|t| Instant::now() >= t) {
            self.speed_menu_at = None;
            if std::env::var("ARKIMAGE_TEST_FULLSCREEN").as_deref() == Ok("3") {
                // mode 3: leave full screen again after 3 s
                if let Some(w) = self.windows.first_mut() {
                    w.test_fullscreen(false);
                }
            } else {
                if std::env::var_os("ARKIMAGE_TEST_CHOICE").is_some() {
                    if let Some(w) = self.windows.first_mut() {
                        w.test_choice(el);
                    }
                    return;
                }
                let click = std::env::var("ARKIMAGE_TEST_CLICK").ok().map(|v| &*Box::leak(v.into_boxed_str()));
                let id = if let Some(c) = click { c } else if std::env::var_os("ARKIMAGE_TEST_PICKER").is_some() { "picker" } else { "anim_speed" };
                if let Some(w) = self.windows.first_mut() {
                    w.test_click(el, id);
                }
            }
        }
        // After an update is in place: start the new exe, which reopens this session
        // once this process has ended, then end
        if crate::update::RESTART.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.save_session(true);
            crate::update::restart();
            for w in &mut self.windows {
                w.exiting();
            }
            el.exit();
            return;
        }
        // Close All Instances (confirmed on a card): every window exits now; the last one
        // ends the process through reap
        if crate::install::CLOSE_ALL.swap(false, std::sync::atomic::Ordering::SeqCst) {
            crate::trace("close all instances");
            for w in &mut self.windows {
                w.exit(el);
            }
            self.reap(el);
            return;
        }
        // muda may deliver a menu choice after its popup returned; the focused window gets it
        let i = self.windows.iter().position(|w| w.is_focused()).or(self.windows.len().checked_sub(1));
        if let Some(i) = i {
            self.windows[i].drain_menu_events(el);
        }
        let mut dl: Option<Instant> = self.windows.iter_mut().filter_map(|w| w.about_to_wait()).min();
        self.sync_config();
        self.reveal_all();
        self.reap(el);
        if let Some(t) = self.crash_at {
            if Instant::now() >= t {
                panic!("ARKIMAGE_CRASH_AFTER_MS test crash");
            }
        }
        // Test hook: ARKIMAGE_HANG_AFTER_MS blocks the main thread for good after that long.
        if let Some(t) = self.hang_at {
            if Instant::now() >= t {
                crate::trace("test hang");
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
        }
        if let Some(g) = self.guard.as_mut() {
            g.tick();
        }
        if !self.windows.is_empty() {
            self.save_session(false);
        }
        // wake at least once a second for the backup and the session file
        let tick = Instant::now() + Duration::from_secs(1);
        dl = Some(dl.map_or(tick, |d| d.min(tick)));
        el.set_control_flow(match dl {
            Some(d) => ControlFlow::WaitUntil(d),
            None => ControlFlow::Wait,
        });
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        for w in &mut self.windows {
            w.exiting();
        }
        if let Some(g) = self.guard.as_mut() {
            g.bye();
        }
    }
}
