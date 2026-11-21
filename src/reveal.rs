//! Show files selected in the system file manager: Explorer on Windows, Finder on macOS.

use std::path::{Path, PathBuf};

/// The helper's command-line flag. It reads the folder, then one file a line, from stdin.
pub const HELPER_FLAG: &str = "--reveal";

/// Open `folder` and select every file in `files` (all must sit in `folder`).
/// The shell call loads Explorer add-ins (cloud sync overlays, copy handlers, thumbnail
/// makers) into the calling process, and a crash in one of them ends that process. So a
/// short-lived copy of this exe makes the call, and a crash ends only that copy.
/// Runs on its own thread: the shell call can block for seconds and froze the UI.
/// Falls back to `explorer /select` for the first file if the helper fails.
#[cfg(windows)]
pub fn select_in_folder(folder: &Path, files: &[PathBuf]) -> bool {
    let (folder, files) = (folder.to_path_buf(), files.to_vec());
    std::thread::spawn(move || {
        let ok = run_helper(&folder, &files);
        if !ok {
            crate::events::log(&format!("reveal helper failed for {} files", files.len()));
            if let Some(f) = files.first() {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", f.display())).spawn();
            }
        }
    });
    true
}

/// Start the helper, hand it the paths on stdin, and wait. True when it reports success.
#[cfg(windows)]
fn run_helper(folder: &Path, files: &[PathBuf]) -> bool {
    use std::{io::Write, process::Stdio};
    let Ok(exe) = std::env::current_exe() else { return false };
    let Ok(mut ch) = std::process::Command::new(exe).arg(HELPER_FLAG).stdin(Stdio::piped()).spawn() else { return false };
    if let Some(mut i) = ch.stdin.take() {
        let mut s = format!("{}\n", folder.display());
        for f in files {
            s.push_str(&format!("{}\n", f.display()));
        }
        let _ = i.write_all(s.as_bytes());
    }
    ch.wait().is_ok_and(|s| s.success())
}

/// The helper's whole run: read the paths from stdin, make the shell call, exit 0 on success.
#[cfg(windows)]
pub fn helper_main() -> ! {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    // a byte order mark from a caller that writes UTF-8 with one (PowerShell does)
    let mut lines = s.trim_start_matches('\u{feff}').lines().filter(|l| !l.is_empty());
    let ok = match lines.next() {
        Some(dir) => {
            let files: Vec<PathBuf> = lines.map(PathBuf::from).collect();
            let ok = select_now(Path::new(dir), &files);
            crate::trace(&format!("reveal helper: {} files in {dir}: {ok}", files.len()));
            ok
        }
        None => {
            crate::trace("reveal helper: no input");
            false
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}

#[cfg(windows)]
fn select_now(folder: &Path, files: &[PathBuf]) -> bool {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    type Pidl = *mut c_void;
    #[link(name = "shell32")]
    extern "system" {
        fn ILCreateFromPathW(p: *const u16) -> Pidl;
        fn ILFindLastID(p: Pidl) -> Pidl;
        fn ILFree(p: Pidl);
        fn SHOpenFolderAndSelectItems(folder: Pidl, n: u32, items: *const Pidl, flags: u32) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(r: *mut c_void, mode: u32) -> i32;
    }
    let wide = |p: &Path| p.as_os_str().encode_wide().chain([0]).collect::<Vec<u16>>();
    unsafe {
        // COINIT_APARTMENTTHREADED; S_FALSE / RPC_E_CHANGED_MODE are fine here
        CoInitializeEx(std::ptr::null_mut(), 0x2);
        let dir = ILCreateFromPathW(wide(folder).as_ptr());
        if dir.is_null() {
            return false;
        }
        let full: Vec<Pidl> = files.iter().map(|f| ILCreateFromPathW(wide(f).as_ptr())).filter(|p| !p.is_null()).collect();
        let last: Vec<Pidl> = full.iter().map(|p| ILFindLastID(*p)).collect();
        let hr = SHOpenFolderAndSelectItems(dir, last.len() as u32, last.as_ptr(), 0);
        crate::trace(&format!("SHOpenFolderAndSelectItems: {} of {} items, hr {hr:#x}", last.len(), files.len()));
        full.into_iter().for_each(|p| ILFree(p));
        ILFree(dir);
        hr >= 0
    }
}

#[cfg(target_os = "macos")]
pub fn select_in_folder(_folder: &Path, files: &[PathBuf]) -> bool {
    crate::mac::reveal(files)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn select_in_folder(folder: &Path, _files: &[PathBuf]) -> bool {
    std::process::Command::new("xdg-open").arg(folder).spawn().is_ok()
}
