//! Auto-update from GitHub Releases (Windows).
//!
//! 1. Read the recent releases of `REPO` and take the newest one that carries this
//!    platform's file (`arkimage.exe`, or `ArkImage-macOS.zip` on the Mac); if its tag
//!    (vX.Y.Z) is newer than this build, download that file next to the running exe as
//!    `arkimage.exe.new`. (Reading only the release marked Latest, one platform's updater
//!    reported a new version and failed while the other platform's files were up alone.)
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
    std::env::var("ARKIMAGE_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases?per_page=20"))
}

/// This platform's release file.
fn platform_asset() -> &'static str {
    #[cfg(target_os = "macos")]
    return mac::ASSET;
    #[cfg(not(target_os = "macos"))]
    ASSET
}

/// From a release list (or one release), the newest published release that carries
/// `asset`; None when none does.
fn pick_release(v: &serde_json::Value, asset: &str) -> Option<serde_json::Value> {
    let list: Vec<&serde_json::Value> = match v.as_array() {
        Some(a) => a.iter().collect(),
        None => vec![v],
    };
    list.into_iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        .filter(|r| r["assets"].as_array().is_some_and(|a| a.iter().any(|x| x["name"].as_str() == Some(asset))))
        .filter_map(|r| Some((parse_version(r["tag_name"].as_str()?)?, r)))
        .max_by_key(|(v, _)| *v)
        .map(|(_, r)| r.clone())
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
#[cfg(target_os = "macos")]
pub fn cleanup() {
    mac::cleanup();
}

/// Delete `<exe>.old`, `.old2` to `.old9` and `.new` left by the last update.
#[cfg(not(target_os = "macos"))]
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

/// What a failed request tells the user. GitHub answers 403 or 429 with no requests left
/// once an address has used its 60 an hour; that was shown as "Can't reach GitHub".
fn reach_error(e: &ureq::Error) -> String {
    if let ureq::Error::Status(code, r) = e {
        if (*code == 403 || *code == 429) && r.header("x-ratelimit-remaining") == Some("0") {
            let mins = r
                .header("x-ratelimit-reset")
                .and_then(|v| v.parse::<u64>().ok())
                .and_then(|t| t.checked_sub(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()))
                .map(|s| s.div_ceil(60).max(1));
            return match mins {
                Some(1) => "GitHub's check limit is used up. Try again in 1 minute.".into(),
                Some(m) => format!("GitHub's check limit is used up. Try again in {m} minutes."),
                None => "GitHub's check limit is used up. Try again later.".into(),
            };
        }
        return format!("GitHub answered with an error ({code}).");
    }
    "Can't reach GitHub.".into()
}

#[cfg_attr(target_os = "macos", allow(unreachable_code, unused_imports))]
fn run(on_found: &dyn Fn(&str)) -> Result<Outcome, String> {
    if let Some(v) = INSTALLED.lock().unwrap().clone() {
        return Ok(Outcome::Ready(v));
    }
    // Test hook: ARKIMAGE_TEST_UPDATE_FAIL makes every check fail with that text
    if let Ok(e) = std::env::var("ARKIMAGE_TEST_UPDATE_FAIL") {
        return Err(e);
    }
    let current = env!("CARGO_PKG_VERSION");
    let a = agent();
    let body = a.get(&api_url()).set("Accept", "application/vnd.github+json").call().map_err(|e| {
        crate::trace(&format!("update: {e}"));
        reach_error(&e)
    })?.into_string().map_err(|e| e.to_string())?;
    let list: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("Unexpected reply from GitHub: {e}"))?;
    // the newest release with this platform's file; none yet: nothing newer for us
    let Some(rel) = pick_release(&list, platform_asset()) else {
        crate::trace("update: no release carries this platform's file");
        return Ok(Outcome::UpToDate);
    };
    let tag = rel["tag_name"].as_str().ok_or("The release has no version tag.")?;
    crate::trace(&format!("update: newest for this platform {tag}, running {current}"));
    if !is_newer(tag, current) {
        return Ok(Outcome::UpToDate);
    }
    on_found(tag.trim_start_matches('v'));
    #[cfg(target_os = "macos")]
    return mac::install(&a, &rel, tag);
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
    // macOS: relaunch the bundle through Launch Services, which reads the new Info.plist
    #[cfg(target_os = "macos")]
    if let Some(app) = mac::bundle() {
        let _ = std::process::Command::new("/usr/bin/open").arg("-n").arg(&app).args(["--args", "--resume", &std::process::id().to_string()]).spawn();
        return;
    }
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
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // signal 0 only asks whether the process still exists
        let t = std::time::Instant::now();
        while unsafe { kill(pid as i32, 0) } == 0 && t.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// macOS: the .app bundle this process runs from, if any.
#[cfg(target_os = "macos")]
pub fn bundle_path() -> Option<std::path::PathBuf> {
    mac::bundle()
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

/// macOS: the release carries `ArkImage-macOS.zip`, the .app zipped with `ditto`. The
/// update downloads it, checks its size and SHA-256, unpacks it beside the running bundle
/// (same volume, so the swap is two renames), moves the running bundle to a hidden
/// `.ArkImage.app.old`, moves the new one into its place, and relaunches with `open -n`.
/// A running app's bundle can be renamed; the old process keeps its open files.
#[cfg(target_os = "macos")]
mod mac {
    use super::*;

    pub const ASSET: &str = "ArkImage-macOS.zip";

    /// The .app bundle this process runs from, if any.
    pub fn bundle() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let app = exe.parent()?.parent()?.parent()?.to_path_buf();
        (app.extension().and_then(|e| e.to_str()) == Some("app")).then_some(app)
    }

    fn side(app: &Path, name: &str) -> Option<PathBuf> {
        Some(app.parent()?.join(name))
    }

    pub fn cleanup() {
        if let Some(app) = bundle() {
            for n in [".ArkImage.app.old", ".ArkImage-update"] {
                if let Some(p) = side(&app, n) {
                    let _ = std::fs::remove_dir_all(p);
                }
            }
        }
    }

    pub fn install(a: &ureq::Agent, rel: &serde_json::Value, tag: &str) -> Result<Outcome, String> {
        let app = bundle().ok_or("Updates install only into ArkImage.app.")?;
        let asset = rel["assets"].as_array().and_then(|v| v.iter().find(|x| x["name"].as_str() == Some(ASSET))).ok_or(format!("The latest release has no {ASSET}."))?;
        let url = asset["browser_download_url"].as_str().ok_or("No download link.")?;
        let size = asset["size"].as_u64().unwrap_or(0);
        let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase);
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
        if bytes.len() < 4 || &bytes[..2] != b"PK" {
            return Err("Download is not a zip file.".into());
        }
        let work = side(&app, ".ArkImage-update").ok_or("No folder for the update.")?;
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| format!("Can't write next to the app ({}): {e}", work.display()))?;
        let zip = work.join(ASSET);
        std::fs::write(&zip, &bytes).map_err(|e| format!("Can't write the download: {e}"))?;
        let ok = std::process::Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip).arg(&work).status().is_ok_and(|s| s.success());
        let new = work.join("ArkImage.app");
        if !ok || !new.join("Contents/MacOS/ArkImage").exists() {
            return Err("The download did not hold ArkImage.app.".into());
        }
        let old = side(&app, ".ArkImage.app.old").ok_or("No folder for the update.")?;
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&app, &old).map_err(|e| format!("Can't replace the app: {e}"))?;
        if let Err(e) = std::fs::rename(&new, &app) {
            let _ = std::fs::rename(&old, &app); // put the old one back
            return Err(format!("Can't replace the app: {e}"));
        }
        let _ = std::fs::remove_dir_all(&work);
        crate::trace(&format!("update: {tag} installed at {}", app.display()));
        let v = tag.trim_start_matches('v').to_string();
        *INSTALLED.lock().unwrap() = Some(v.clone());
        Ok(Outcome::Ready(v))
    }
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

#[cfg(test)]
mod pick_tests {
    use super::pick_release;

    /// The newest release carrying this platform's file wins; one with only the other
    /// platform's file, a draft and a pre-release are passed over.
    #[test]
    fn newest_with_our_file() {
        let list: serde_json::Value = serde_json::from_str(
            r#"[
            {"tag_name": "v1.3.0", "draft": true, "prerelease": false, "assets": [{"name": "arkimage.exe"}, {"name": "ArkImage-macOS.zip"}]},
            {"tag_name": "v1.2.1", "draft": false, "prerelease": true, "assets": [{"name": "arkimage.exe"}]},
            {"tag_name": "v1.2.0", "draft": false, "prerelease": false, "assets": [{"name": "ArkImage-macOS.zip"}, {"name": "ArkImage-1.2.0-macOS.dmg"}]},
            {"tag_name": "v1.1.9", "draft": false, "prerelease": false, "assets": [{"name": "arkimage.exe"}, {"name": "ArkImage-macOS.zip"}]}
        ]"#,
        )
        .unwrap();
        let tag = |a: &str| pick_release(&list, a).map(|r| r["tag_name"].as_str().unwrap().to_string());
        assert_eq!(tag("ArkImage-macOS.zip").as_deref(), Some("v1.2.0"));
        assert_eq!(tag("arkimage.exe").as_deref(), Some("v1.1.9"));
        assert_eq!(tag("nothing.bin"), None);
        // one release object (the old /latest reply) still works
        let one = list[2].clone();
        assert_eq!(pick_release(&one, "ArkImage-macOS.zip").map(|r| r["tag_name"].clone()), Some("v1.2.0".into()));
    }
}
