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
    format!("arkimage-{user}.sock").to_ns_name::<GenericNamespaced>()
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

/// Claim the master socket; None if another process holds it.
pub fn listen() -> Option<Listener> {
    ListenerOptions::new().name(name().ok()?).create_sync().ok()
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
