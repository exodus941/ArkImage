//! Show files selected in the system file manager: Explorer on Windows, Finder on macOS.

use std::path::{Path, PathBuf};

/// Open `folder` and select every file in `files` (all must sit in `folder`).
#[cfg(windows)]
pub fn select_in_folder(folder: &Path, files: &[PathBuf]) -> bool {
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
        full.into_iter().for_each(|p| ILFree(p));
        ILFree(dir);
        hr >= 0
    }
}

#[cfg(target_os = "macos")]
pub fn select_in_folder(_folder: &Path, files: &[PathBuf]) -> bool {
    let list: Vec<String> = files.iter().map(|f| format!("POSIX file \"{}\"", f.display().to_string().replace('"', "\\\""))).collect();
    let script = format!("tell application \"Finder\"\nreveal {{{}}}\nactivate\nend tell", list.join(", "));
    std::process::Command::new("osascript").arg("-e").arg(script).spawn().is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn select_in_folder(folder: &Path, _files: &[PathBuf]) -> bool {
    std::process::Command::new("xdg-open").arg(folder).spawn().is_ok()
}
