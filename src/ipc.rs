//! One ArkImage process per user. A launch first tries to hand its file to a running
//! master over a local socket (named pipe on Windows, Unix socket on macOS) and exits;
//! if no master answers, it becomes the master and listens.
//! Protocol: one line per request, "open\t<absolute path>" or "open", answered "ok".

use std::{
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
};

use interprocess::local_socket::{prelude::*, GenericNamespaced, Listener, ListenerOptions, Name, Stream};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

fn name() -> io::Result<Name<'static>> {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    format!("arkimage-{user}{}.sock", crate::config::Config::instance_tag()).to_ns_name::<GenericNamespaced>()
}

/// Send the request line `line` to the running master; true if it answered "ok".
pub fn send(line: &str) -> bool {
    let Ok(n) = name() else { return false };
    let Ok(conn) = Stream::connect(n) else { return false };
    let mut conn = BufReader::new(conn);
    if conn.get_mut().write_all(format!("{line}\n").as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    conn.read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Hand `arg` to a running master; true if one took it (this process should exit).
pub fn forward(arg: &Option<PathBuf>) -> bool {
    let line = match arg.as_ref().and_then(|p| std::path::absolute(p).ok()) {
        Some(p) => format!("open\t{}", p.display()),
        None => "open".to_string(),
    };
    send(&line)
}

/// Unix: decide who is the master with an exclusive lock on a file. The socket alone could
/// not: a launch that finds no master answering overwrites the socket file, so two launches
/// started 0.1 s apart could both become masters, the later one taking the socket from the
/// earlier (seen on the Mac in a burst of 20 launches: one image went to a second copy).
/// The lock is held for the life of the process and released by the system when it ends,
/// so the backup can take it after a crash. None: another live process holds it.
#[cfg(unix)]
pub fn claim_master() -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let dir = crate::config::Config::dir();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(format!("master{}.lock", crate::config::Config::instance_tag()));
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(p).ok()?;
    (unsafe { flock(f.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0).then_some(f)
}

/// Claim the master socket; None if another process holds it.
pub fn listen() -> Option<Listener> {
    // macOS: a crashed master leaves its socket file; callers only listen after
    // `forward` found no master answering, so taking the file over is safe
    ListenerOptions::new().name(name().ok()?).try_overwrite(cfg!(unix)).create_sync().ok()
}

/// Serve requests on a background thread, turning them into events for the master.
pub fn serve(listener: Listener, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        for conn in listener.incoming().filter_map(Result::ok) {
            let mut conn = BufReader::new(conn);
            let mut line = String::new();
            if conn.read_line(&mut line).is_err() {
                continue;
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let ev = match line.split_once('\t') {
                Some(("open", p)) => Some(UserEvent::Open(Some(PathBuf::from(p)))),
                None if line == "open" => Some(UserEvent::Open(None)),
                _ => None,
            };
            let ok = match ev {
                Some(ev) => proxy.send_event(ev).is_ok(),
                None => false,
            };
            let _ = conn.get_mut().write_all(if ok { b"ok\n" } else { b"no\n" });
        }
    });
}
