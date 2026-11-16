//! Crash recovery: a backup master waits in a small invisible process.
//!
//! The master keeps `session.txt` current (each window's image, position, size,
//! maximized/full screen) and holds a connection open to the backup. On a normal exit it
//! sends "bye". If the connection drops without "bye", the master crashed: the backup
//! becomes the master, reopens every window from the session file, and starts a new
//! backup behind itself. After 60 s without a crash the master sends "stable" and the
//! crash count resets. The third crash in a row ends the chain with an error message.

use std::{
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Name, Stream};

use crate::config::Config;

/// Crashes in a row that end the chain.
pub const MAX_CRASHES: u32 = 3;
/// Running this long without a crash resets the count.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

fn name() -> io::Result<Name<'static>> {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    format!("arkimage-{user}{}-backup.sock", Config::instance_tag()).to_ns_name::<GenericNamespaced>()
}

// ---------- session file ----------

/// One window as saved in the session file.
#[derive(Clone, Debug, PartialEq)]
pub struct WinState {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub max: bool,
    pub fs: bool,
    /// Entry label: a file path, or "<archive>|<name inside>".
    pub label: String,
}

impl WinState {
    pub fn line(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}\t{}\t{}", self.x, self.y, self.w, self.h, self.max as u8, self.fs as u8, self.label)
    }

    fn parse(l: &str) -> Option<Self> {
        let mut it = l.splitn(7, '\t');
        Some(Self {
            x: it.next()?.parse().ok()?,
            y: it.next()?.parse().ok()?,
            w: it.next()?.parse().ok()?,
            h: it.next()?.parse().ok()?,
            max: it.next()? == "1",
            fs: it.next()? == "1",
            label: it.next()?.to_string(),
        })
    }
}

pub fn session_path() -> PathBuf {
    Config::dir().join("session.txt")
}

pub fn save_session(wins: &[WinState]) {
    let text: String = wins.iter().map(|w| w.line() + "\n").collect();
    let p = session_path();
    let _ = std::fs::create_dir_all(Config::dir());
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

pub fn load_session() -> Vec<WinState> {
    std::fs::read_to_string(session_path()).unwrap_or_default().lines().filter_map(WinState::parse).collect()
}

// ---------- the backup side ----------

/// What a backup hands to `main` when it takes over.
pub struct Takeover {
    pub crashes: u32,
    pub session: Vec<WinState>,
}

/// Run as the backup: wait for the master to end. Returns Some when the master crashed
/// and this process should become the master; None when the master exited normally
/// (or the chain stops).
pub fn run_backup(mut crashes: u32) -> Option<Takeover> {
    // the master starts listening before it spawns us; allow it a moment
    let deadline = Instant::now() + Duration::from_secs(5);
    let conn = loop {
        if let Ok(c) = name().and_then(Stream::connect) {
            break c;
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    crate::trace(&format!("backup connected, crash count {crashes}"));
    // Lines arrive on their own thread, so this one can also watch for a hang.
    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(conn);
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(_) => {
                    if tx.send(Some(line.trim().to_string())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut pid: Option<u32> = None;
    let mut hung_since: Option<Instant> = None;
    // macOS: the master's heartbeat ("alive", sent once its main thread answers)
    let mut alive: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break, // gone without bye
            Ok(Some(l)) => match l.as_str() {
                "bye" => return None,
                "stable" => crashes = 0,
                "alive" => alive = Some(Instant::now()),
                _ => {
                    if let Some(n) = l.strip_prefix("pid ") {
                        pid = n.parse().ok();
                    }
                }
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        // A master whose windows all stop answering messages for HANG_AFTER is hung:
        // save a dump for diagnosis, end it, and take over as for a crash.
        #[cfg(not(target_os = "macos"))]
        let _ = alive;
        #[cfg(target_os = "macos")]
        let not_responding = |_p: u32| hang::not_responding(alive);
        #[cfg(not(target_os = "macos"))]
        let not_responding = hang::not_responding;
        if let Some(p) = pid {
            if not_responding(p) {
                let since = *hung_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= HANG_AFTER {
                    crate::trace(&format!("master {p} not responding for {:?}", since.elapsed()));
                    hang::dump_and_kill(p);
                    break;
                }
            } else {
                hung_since = None;
            }
        }
    }
    crashes += 1;
    crate::trace(&format!("master crashed or hung; {crashes} in a row"));
    if crashes >= MAX_CRASHES {
        let msg = format!("ArkImage crashed {MAX_CRASHES} times in a row and was not restarted.");
        crash_dialog(&msg);
        crate::trace("crash dialog closed");
        return None;
    }
    let session = load_session();
    if session.is_empty() {
        return None;
    }
    Some(Takeover { crashes, session })
}

fn crash_dialog(msg: &str) {
    crate::dialog::tell(None, "ArkImage", msg, crate::dialog::Kind::Error);
}

/// Not answering window messages this long counts as a hang (Windows shows
/// "Not Responding" after 5 s).
pub const HANG_AFTER: Duration = Duration::from_secs(15);

/// Hang detection and recovery (Windows). A modal menu or file dialog keeps answering
/// messages, so it never counts as a hang.
#[cfg(windows)]
mod hang {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;
    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, l: isize) -> i32;
        fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
        fn IsWindowVisible(h: Hwnd) -> i32;
        fn SendMessageTimeoutW(h: Hwnd, msg: u32, w: usize, l: isize, flags: u32, ms: u32, res: *mut usize) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn TerminateProcess(h: *mut c_void, code: u32) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }
    #[link(name = "dbghelp")]
    extern "system" {
        fn MiniDumpWriteDump(p: *mut c_void, pid: u32, file: *mut c_void, ty: u32, e: *const c_void, u: *const c_void, c: *const c_void) -> i32;
    }

    extern "system" fn collect(h: Hwnd, l: isize) -> i32 {
        let v = unsafe { &mut *(l as *mut (u32, Vec<Hwnd>)) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, &mut pid) };
        if pid == v.0 && unsafe { IsWindowVisible(h) } != 0 {
            v.1.push(h);
        }
        1
    }

    /// True when the process has visible windows and none of them answers within 1 s.
    pub fn not_responding(pid: u32) -> bool {
        let mut v: (u32, Vec<Hwnd>) = (pid, Vec::new());
        unsafe { EnumWindows(collect, &mut v as *mut _ as isize) };
        if v.1.is_empty() {
            return false;
        }
        // WM_NULL, SMTO_ABORTIFHUNG
        v.1.iter().all(|&h| {
            let mut r = 0usize;
            unsafe { SendMessageTimeoutW(h, 0, 0, 0, 0x2, 1000, &mut r) == 0 }
        })
    }

    /// Write hang-<unix time>.dmp next to the config (keeping the newest 3), then end the process.
    pub fn dump_and_kill(pid: u32) {
        use std::os::windows::io::AsRawHandle;
        // PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_TERMINATE
        let h = unsafe { OpenProcess(0x0400 | 0x0010 | 0x0001, 0, pid) };
        if h.is_null() {
            return;
        }
        let dir = crate::config::Config::dir();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = dir.join(format!("hang-{secs}.dmp"));
        if let Ok(f) = std::fs::File::create(&path) {
            // MiniDumpWithDataSegs | MiniDumpWithHandleData | MiniDumpWithThreadInfo
            let ok = unsafe { MiniDumpWriteDump(h, pid, f.as_raw_handle() as _, 0x1 | 0x4 | 0x1000, std::ptr::null(), std::ptr::null(), std::ptr::null()) };
            crate::trace(&format!("hang dump {} written: {}", path.display(), ok != 0));
        }
        let mut dumps: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("hang-") && n.ends_with(".dmp")))
            .collect();
        dumps.sort();
        while dumps.len() > 3 {
            let _ = std::fs::remove_file(dumps.remove(0));
        }
        unsafe {
            TerminateProcess(h, 1);
            CloseHandle(h);
        }
    }
}

/// Hang detection and recovery (macOS). There is no "does this window answer" query
/// across processes, so the master proves it: a thread asks the main thread to touch a
/// timestamp once a second (through the main dispatch queue, which also runs during menus
/// and file dialogs, so those never count as a hang) and sends "alive" to the backup while
/// the main thread keeps answering. No "alive" for 3 s counts as not responding; HANG_AFTER
/// of that ends the master. A master that never sent "alive" is never judged.
#[cfg(target_os = "macos")]
mod hang {
    use std::time::{Duration, Instant};

    pub fn not_responding(alive: Option<Instant>) -> bool {
        alive.is_some_and(|t| t.elapsed() > Duration::from_secs(3))
    }

    /// Write hang-<unix time>.txt next to the config with `sample` (a 1 s stack sample of
    /// every thread; keeps the newest 3), then end the process.
    pub fn dump_and_kill(pid: u32) {
        let dir = crate::config::Config::dir();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = dir.join(format!("hang-{secs}.txt"));
        let ok = std::process::Command::new("/usr/bin/sample").arg(pid.to_string()).arg("1").arg("-file").arg(&path).output().is_ok_and(|o| o.status.success());
        crate::trace(&format!("hang sample {} written: {ok}", path.display()));
        let mut dumps: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("hang-") && n.ends_with(".txt")))
            .collect();
        dumps.sort();
        while dumps.len() > 3 {
            let _ = std::fs::remove_file(dumps.remove(0));
        }
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe { kill(pid as i32, 9) };
    }

    /// The master's side: a thread that sends "alive" while the main thread answers.
    pub fn heartbeat(send: impl Fn(&str) + Send + 'static) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        static SEEN: AtomicU64 = AtomicU64::new(0);
        let t0 = *T0.get_or_init(Instant::now);
        extern "C" {
            static _dispatch_main_q: u8;
            fn dispatch_async_f(queue: *const u8, ctx: *mut std::ffi::c_void, f: extern "C" fn(*mut std::ffi::c_void));
        }
        extern "C" fn touch(_: *mut std::ffi::c_void) {
            if let Some(t0) = T0.get() {
                SEEN.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        }
        std::thread::spawn(move || loop {
            unsafe { dispatch_async_f(&raw const _dispatch_main_q, std::ptr::null_mut(), touch) };
            std::thread::sleep(Duration::from_secs(1));
            let now = t0.elapsed().as_millis() as u64;
            if now.saturating_sub(SEEN.load(Ordering::Relaxed)) < 2000 {
                send("alive");
            }
        });
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod hang {
    pub fn not_responding(_pid: u32) -> bool {
        false
    }
    pub fn dump_and_kill(_pid: u32) {}
}

// ---------- the master side ----------

/// The master's end: listens for its backup, keeps it running, and talks to it.
pub struct Guard {
    conn: Arc<Mutex<Option<Stream>>>,
    child: Option<Child>,
    crashes: u32,
    started: Instant,
    stable_sent: bool,
    last_spawn: Instant,
}

impl Guard {
    /// Start listening and spawn the first backup. `crashes` is the count carried over
    /// from a takeover (0 on a normal start).
    pub fn start(crashes: u32) -> Option<Self> {
        // macOS: the socket is a file in /tmp, left behind when a master is killed or crashes;
        // take it over (only the one master gets here, so no live listener owns it)
        let listener = match ListenerOptions::new().name(name().ok()?).try_overwrite(cfg!(unix)).create_sync() {
            Ok(l) => l,
            Err(e) => {
                crate::trace(&format!("backup listener: {e}"));
                return None;
            }
        };
        let conn: Arc<Mutex<Option<Stream>>> = Arc::new(Mutex::new(None));
        let c2 = conn.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().filter_map(Result::ok) {
                // the backup needs our process ID to watch for a hang
                let _ = s.write_all(format!("pid {}\n", std::process::id()).as_bytes());
                *c2.lock().unwrap() = Some(s); // newest backup replaces any older one
            }
        });
        // macOS: prove the main thread answers, for the backup's hang check
        #[cfg(target_os = "macos")]
        {
            let c3 = conn.clone();
            hang::heartbeat(move |m| {
                if let Some(c) = c3.lock().unwrap().as_mut() {
                    let _ = c.write_all(format!("{m}\n").as_bytes());
                    let _ = c.flush();
                }
            });
        }
        let mut g = Self { conn, child: None, crashes, started: Instant::now(), stable_sent: false, last_spawn: Instant::now() };
        g.spawn();
        Some(g)
    }

    fn spawn(&mut self) {
        let Ok(exe) = std::env::current_exe() else { return };
        self.child = Command::new(exe).args(["--backup", &self.crashes.to_string()]).spawn().ok();
        self.last_spawn = Instant::now();
        crate::trace("backup spawned");
    }

    fn send(&self, msg: &str) {
        if let Some(c) = self.conn.lock().unwrap().as_mut() {
            let _ = c.write_all(format!("{msg}\n").as_bytes());
            let _ = c.flush();
        }
    }

    /// Call now and then: replaces a backup that died, and reports stability.
    pub fn tick(&mut self) {
        if let Some(ch) = self.child.as_mut() {
            if let Ok(Some(_)) = ch.try_wait() {
                // backup is gone; start another (at most once a second)
                if self.last_spawn.elapsed() > Duration::from_secs(1) {
                    *self.conn.lock().unwrap() = None;
                    self.spawn();
                    self.stable_sent = false;
                }
            }
        }
        if !self.stable_sent && self.started.elapsed() >= STABLE_AFTER {
            self.send("stable");
            self.crashes = 0;
            self.stable_sent = true;
        }
    }

    /// Normal exit: tell the backup to stand down.
    pub fn bye(&mut self) {
        self.send("bye");
    }
}
