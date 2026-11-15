#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod backup;
mod bindings;
mod chrome;
mod config;
mod decode;
mod dialog;
mod filters;
mod heic;
mod icon;
mod install;
mod icons;
mod ipc;
#[cfg(windows)]
mod lottie;
mod master;
mod picker;
mod psd_fast;
mod render;
mod reveal;
mod settings;
mod source;
mod text;
mod thumbcache;
mod ui;
mod update;
mod webp_fast;

use std::{
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Instant,
};
use winit::event_loop::EventLoop;

static T0: OnceLock<Instant> = OnceLock::new();
static TRACE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

/// Appends "<ms since start> <msg>" to the file named by ARKIMAGE_TRACE, if set.
pub fn trace(msg: &str) {
    let Some(Some(f)) = TRACE.get() else { return };
    let ms = T0.get().map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
    if let Ok(mut f) = f.lock() {
        let _ = writeln!(f, "{ms:8.1} {msg}");
    }
}

fn main() {
    T0.get_or_init(Instant::now);
    TRACE.get_or_init(|| {
        std::env::var_os("ARKIMAGE_TRACE").and_then(|p| {
            std::fs::OpenOptions::new().create(true).append(true).open(p).ok().map(Mutex::new)
        })
    });
    // Release builds have no console, so a panic would vanish: write it to crash.log
    // (next to the config, newest last) and to the trace.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        trace(&format!("PANIC {info}"));
        let p = config::Config::dir().join("crash.log");
        let _ = std::fs::create_dir_all(config::Config::dir());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "--- {secs} (unix) ---\n{msg}");
        }
        default_hook(info);
    }));
    let mut arg = std::env::args_os().nth(1).map(PathBuf::from);
    // Command-line install and uninstall (Apps & features runs `--uninstall`).
    match arg.as_deref().and_then(|a| a.to_str()) {
        Some("--install") => {
            let r = install::install();
            trace(&format!("install: {r:?}"));
            std::process::exit(if r.is_ok() { 0 } else { 1 });
        }
        Some("--uninstall") => {
            let quiet = std::env::args().any(|a| a == "--quiet");
            let go = quiet
                || dialog::ask(
                    None,
                    "Uninstall ArkImage",
                    "Remove ArkImage, its shortcuts and its file type entries?\n\nYour settings stay in %APPDATA%\\ArkImage.",
                    dialog::Kind::Warn,
                );
            if go {
                let r = install::uninstall();
                trace(&format!("uninstall: {r:?}"));
                if !quiet {
                    let msg = match &r {
                        Ok(()) => "ArkImage is uninstalled.".to_string(),
                        Err(e) => e.clone(),
                    };
                    dialog::tell(None, "ArkImage", &msg, if r.is_ok() { dialog::Kind::Info } else { dialog::Kind::Error });
                }
            }
            return;
        }
        _ => {}
    }
    // A finished update leaves the previous exe as <name>.exe.old: remove it.
    update::cleanup();
    // Run as the backup master; on a crash of the master, become the master.
    let mut takeover = None;
    // `--resume <pid>`: restarted after an update; wait for the old process, then
    // reopen its windows from the session file.
    if arg.as_deref() == Some(std::path::Path::new("--resume")) {
        if let Some(pid) = std::env::args().nth(2).and_then(|s| s.parse().ok()) {
            update::wait_for_exit(pid);
        }
        update::cleanup();
        let session = backup::load_session();
        if !session.is_empty() {
            takeover = Some(backup::Takeover { crashes: 0, session });
        }
        arg = None;
        trace("resumed after update");
    }
    if arg.as_deref() == Some(std::path::Path::new("--backup")) {
        let n = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        match backup::run_backup(n) {
            Some(t) => takeover = Some(t),
            None => return,
        }
        arg = None;
        trace("backup takes over");
    }
    // A master is already running: hand it the file and leave.
    if takeover.is_none() && ipc::forward(&arg) {
        trace("forwarded to master");
        return;
    }
    // Become the master. If another launch claimed the socket first, hand over to it.
    let listener = loop {
        if let Some(l) = ipc::listen() {
            break Some(l);
        }
        if ipc::forward(&arg) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(l) = ipc::listen() {
            break Some(l);
        }
        break None; // run on our own rather than not at all
    };
    let event_loop = EventLoop::<app::UserEvent>::with_user_event()
        .build()
        .expect("create event loop");
    let proxy = event_loop.create_proxy();
    if let Some(l) = listener {
        ipc::serve(l, proxy.clone());
    }
    let mut master = master::Master::new(proxy, arg, takeover);
    event_loop.run_app(&mut master).expect("run event loop");
}
