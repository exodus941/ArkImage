//! Auto-update from GitHub Releases (Windows).
//!
//! 1. Read the latest release of `REPO`; if its tag (vX.Y.Z) is newer than this build,
//!    download its `arkimage.exe` next to the running exe as `arkimage.exe.new`.
//! 2. Check its size, and its SHA-256 when GitHub lists one.
//! 3. Swap: Windows lets a running exe be renamed, so the running exe becomes
//!    `<name>.old` and the download takes its name. The next launch is the new
//!    version; it deletes the `.old` file.
//! 4. The master then restarts at once: it starts the new exe with `--resume <pid>`: it waits for this process
//!    to end, then reopens the same windows from the session file.
//!
//! Works in the install folder or any portable folder the user can write to.
//! ARKIMAGE_UPDATE_URL overrides the release API URL (tests).

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// GitHub owner/repo the releases come from.
pub const REPO: &str = "exodus941/ArkImage";
/// The release asset to download.
const ASSET: &str = "arkimage.exe";

/// Set when an update is in place; the master restarts on its next tick.
pub static RESTART: AtomicBool = AtomicBool::new(false);

/// What a check found.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A newer version exists and is downloading (sent before Ready).
    Found(String),
    /// Downloaded and swapped in; restart to run it.
    Ready(String),
    /// This build is the latest.
    UpToDate,
    Failed(String),
}

fn api_url() -> String {
    std::env::var("ARKIMAGE_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

/// "v1.2.3" or "1.2.3" -> (1, 2, 3).
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.').map(|p| p.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("").parse::<u64>());
    Some((it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

pub fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Delete `<exe>.old`, `.old2` to `.old9` and `.new` left by the last update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
        for i in 2..10 {
            let _ = std::fs::remove_file(exe.with_extension(format!("exe.old{i}")));
        }
        let _ = std::fs::remove_file(new_path(&exe));
    }
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

fn new_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.new")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent(concat!("ArkImage/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Check, and when newer, download and swap. Blocking: run it on a worker thread.
pub fn check_and_install(on_found: &dyn Fn(&str)) -> Outcome {
    match run(on_found) {
        Ok(o) => o,
        Err(e) => Outcome::Failed(e),
    }
}

/// The version this process already swapped in; a second check before the restart
/// reports it again instead of swapping twice (the first swap's `.old` is still locked).
static INSTALLED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn run(on_found: &dyn Fn(&str)) -> Result<Outcome, String> {
    if let Some(v) = INSTALLED.lock().unwrap().clone() {
        return Ok(Outcome::Ready(v));
    }
    let current = env!("CARGO_PKG_VERSION");
    let a = agent();
    let body = a.get(&api_url()).set("Accept", "application/vnd.github+json").call().map_err(|e| { crate::trace(&format!("update: {e}")); "Can't reach GitHub.".to_string() })?.into_string().map_err(|e| e.to_string())?;
    let rel: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    let tag = rel["tag_name"].as_str().ok_or("The latest release has no version tag.")?;
    crate::trace(&format!("update: latest {tag}, running {current}"));
    if !is_newer(tag, current) {
        return Ok(Outcome::UpToDate);
    }
    on_found(tag.trim_start_matches('v'));
    let asset = rel["assets"].as_array().and_then(|v| v.iter().find(|a| a["name"].as_str() == Some(ASSET))).ok_or("The latest release has no arkimage.exe.")?;
    let url = asset["browser_download_url"].as_str().ok_or("No download link.")?;
    let size = asset["size"].as_u64().unwrap_or(0);
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase);

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = new_path(&exe);
    let mut bytes = Vec::with_capacity(size as usize);
    a.get(url).call().map_err(|e| format!("Download failed: {e}"))?.into_reader().take(256 << 20).read_to_end(&mut bytes).map_err(|e| format!("Download failed: {e}"))?;
    if size != 0 && bytes.len() as u64 != size {
        return Err(format!("Download incomplete: {} of {size} bytes.", bytes.len()));
    }
    if let Some(want) = digest {
        use sha2::{Digest, Sha256};
        let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if got != want {
            return Err("Download corrupt: checksum mismatch.".into());
        }
    }
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        return Err("Download is not a Windows program.".into());
    }
    std::fs::write(&new, &bytes).map_err(|e| format!("Can't write next to the app ({}): {e}", new.display()))?;
    // swap: the running exe can be renamed but not overwritten
    // a locked `.old` (another process still runs it) cannot be deleted: use `.old2`, `.old3`
    let mut old = old_path(&exe);
    for i in 2..10 {
        if std::fs::remove_file(&old).is_ok() || !old.exists() {
            break;
        }
        old = exe.with_extension(format!("exe.old{i}"));
    }
    std::fs::rename(&exe, &old).map_err(|e| format!("Can't replace the app: {e}"))?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // put the old one back
        return Err(format!("Can't replace the app: {e}"));
    }
    crate::trace(&format!("update: {tag} installed at {}", exe.display()));
    let v = tag.trim_start_matches('v').to_string();
    *INSTALLED.lock().unwrap() = Some(v.clone());
    Ok(Outcome::Ready(v))
}

/// Start the (new) exe to reopen this session once this process has ended.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).args(["--resume", &std::process::id().to_string()]).spawn();
    }
}

/// `--resume <pid>`: wait (up to 10 s) for the old process to end.
pub fn wait_for_exit(pid: u32) {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn WaitForSingleObject(h: *mut std::ffi::c_void, ms: u32) -> u32;
            fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
        }
        // SYNCHRONIZE
        let h = unsafe { OpenProcess(0x0010_0000, 0, pid) };
        if !h.is_null() {
            unsafe {
                WaitForSingleObject(h, 10_000);
                CloseHandle(h);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Background check at launch; the outcome arrives as a UserEvent.
pub fn spawn_check(proxy: winit::event_loop::EventLoopProxy<crate::app::UserEvent>, manual: bool) {
    std::thread::spawn(move || {
        let p2 = proxy.clone();
        let o = check_and_install(&|v| {
            let _ = p2.send_event(crate::app::UserEvent::Update(Outcome::Found(v.to_string()), manual));
        });
        crate::trace(&format!("update check: {o:?}"));
        let _ = proxy.send_event(crate::app::UserEvent::Update(o, manual));
    });
}

/// Open the project page in the default browser.
pub fn open_repo_page() {
    let url = format!("https://github.com/{REPO}");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let r = std::process::Command::new("cmd").raw_arg(format!("/C start \"\" \"{url}\"")).creation_flags(0x0800_0000).spawn();
        crate::trace(&format!("open {url}: {}", if r.is_ok() { "started" } else { "failed" }));
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
}

pub fn request_restart() {
    RESTART.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v1.0.1", "1.0.0"));
        assert!(is_newer("v2.0.0", "1.9.9"));
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
        assert!(!is_newer("v0.9.9", "1.0.0"));
        assert!(!is_newer("garbage", "1.0.0"));
    }
}
