//! Per-user install on Windows, no admin rights needed:
//! - copies the exe to %LOCALAPPDATA%\Programs\ArkImage\ArkImage.exe
//! - Start menu and desktop shortcuts
//! - registers for every supported image and archive type (HKCU\Software\Classes), so
//!   ArkImage shows in "Open with" and in Settings > Apps > Default apps. Windows 10+ lets
//!   only the user pick the default app, so the installer opens that page afterwards.
//! - an "Apps & features" entry whose Uninstall runs `ArkImage.exe --uninstall`.
//!
//! ARKIMAGE_INSTALL_SANDBOX=<folder> redirects everything for tests: files and shortcuts
//! under that folder, registry under HKCU\Software\ArkImageSandbox.

use std::path::PathBuf;

use crate::source::{ARCHIVE_EXTS, IMAGE_EXTS};

const APP: &str = "ArkImage";
const PROG_IMAGE: &str = "ArkImage.Image";
const PROG_ARCHIVE: &str = "ArkImage.Archive";

fn sandbox() -> Option<PathBuf> {
    std::env::var_os("ARKIMAGE_INSTALL_SANDBOX").map(PathBuf::from)
}

/// Where the installed exe lives.
pub fn install_dir() -> PathBuf {
    match sandbox() {
        Some(s) => s.join("Programs").join(APP),
        None => dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Programs").join(APP),
    }
}

pub fn installed_exe() -> PathBuf {
    install_dir().join("ArkImage.exe")
}

fn start_menu_lnk() -> Option<PathBuf> {
    Some(match sandbox() {
        Some(s) => s.join("StartMenu").join("ArkImage.lnk"),
        None => dirs::data_dir()?.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("ArkImage.lnk"),
    })
}

fn desktop_lnk() -> Option<PathBuf> {
    Some(match sandbox() {
        Some(s) => s.join("Desktop").join("ArkImage.lnk"),
        None => dirs::desktop_dir()?.join("ArkImage.lnk"),
    })
}

/// True when this process runs from the installed copy.
pub fn running_installed() -> bool {
    let a = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let b = installed_exe().canonicalize().ok();
    a.is_some() && a == b
}

#[cfg(windows)]
mod reg {
    use super::*;
    use winreg::{enums::*, RegKey};

    /// HKCU\Software, or the sandbox key under it.
    fn software() -> std::io::Result<RegKey> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match sandbox() {
            Some(_) => hkcu.create_subkey("Software\\ArkImageSandbox").map(|(k, _)| k),
            None => hkcu.open_subkey_with_flags("Software", KEY_ALL_ACCESS),
        }
    }

    fn uninstall_key() -> &'static str {
        "Microsoft\\Windows\\CurrentVersion\\Uninstall\\ArkImage"
    }

    pub fn is_registered() -> bool {
        software().and_then(|s| s.open_subkey(uninstall_key())).is_ok()
    }

    pub fn register(exe: &std::path::Path) -> std::io::Result<()> {
        let sw = software()?;
        let exe_s = exe.display().to_string();
        let open = format!("\"{exe_s}\" \"%1\"");
        let icon = format!("\"{exe_s}\",0");
        let (classes, _) = sw.create_subkey("Classes")?;
        for (prog, name) in [(PROG_IMAGE, "ArkImage image"), (PROG_ARCHIVE, "ArkImage archive")] {
            let (k, _) = classes.create_subkey(prog)?;
            k.set_value("", &name)?;
            k.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
            k.create_subkey("shell\\open\\command")?.0.set_value("", &open)?;
        }
        // "Open with" list for each extension
        let exts: Vec<(&str, &str)> = IMAGE_EXTS.iter().map(|e| (*e, PROG_IMAGE)).chain(ARCHIVE_EXTS.iter().map(|e| (*e, PROG_ARCHIVE))).collect();
        for (e, prog) in &exts {
            classes.create_subkey(format!(".{e}\\OpenWithProgids"))?.0.set_value(*prog, &"")?;
        }
        // the app itself, for "Open with > Choose another app"
        let (app, _) = classes.create_subkey("Applications\\ArkImage.exe")?;
        app.set_value("FriendlyAppName", &APP)?;
        app.create_subkey("shell\\open\\command")?.0.set_value("", &open)?;
        let (st, _) = app.create_subkey("SupportedTypes")?;
        for (e, _) in &exts {
            st.set_value(format!(".{e}"), &"")?;
        }
        // Default apps page: capabilities, listed under RegisteredApplications
        let (cap, _) = sw.create_subkey("ArkImage\\Capabilities")?;
        cap.set_value("ApplicationName", &APP)?;
        cap.set_value("ApplicationDescription", &"Fast image viewer")?;
        cap.set_value("ApplicationIcon", &icon)?;
        let (fa, _) = cap.create_subkey("FileAssociations")?;
        for (e, prog) in &exts {
            fa.set_value(format!(".{e}"), prog)?;
        }
        let cap_path = match sandbox() {
            Some(_) => "Software\\ArkImageSandbox\\ArkImage\\Capabilities",
            None => "Software\\ArkImage\\Capabilities",
        };
        sw.create_subkey("RegisteredApplications")?.0.set_value(APP, &cap_path)?;
        // Apps & features entry
        let (u, _) = sw.create_subkey(uninstall_key())?;
        u.set_value("DisplayName", &APP)?;
        u.set_value("DisplayIcon", &icon)?;
        u.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
        u.set_value("Publisher", &APP)?;
        u.set_value("InstallLocation", &exe.parent().map(|p| p.display().to_string()).unwrap_or_default())?;
        u.set_value("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
        u.set_value("NoModify", &1u32)?;
        u.set_value("NoRepair", &1u32)?;
        let kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
        u.set_value("EstimatedSize", &kb)?;
        Ok(())
    }

    pub fn unregister() -> std::io::Result<()> {
        let sw = software()?;
        let classes = sw.open_subkey_with_flags("Classes", KEY_ALL_ACCESS)?;
        for prog in [PROG_IMAGE, PROG_ARCHIVE] {
            let _ = classes.delete_subkey_all(prog);
        }
        for e in IMAGE_EXTS.iter().chain(ARCHIVE_EXTS.iter()) {
            if let Ok(k) = classes.open_subkey_with_flags(format!(".{e}\\OpenWithProgids"), KEY_ALL_ACCESS) {
                let _ = k.delete_value(PROG_IMAGE);
                let _ = k.delete_value(PROG_ARCHIVE);
            }
        }
        let _ = classes.delete_subkey_all("Applications\\ArkImage.exe");
        let _ = sw.delete_subkey_all("ArkImage");
        if let Ok(k) = sw.open_subkey_with_flags("RegisteredApplications", KEY_ALL_ACCESS) {
            let _ = k.delete_value(APP);
        }
        let _ = sw.delete_subkey_all(uninstall_key());
        Ok(())
    }

    /// Tell Explorer the associations changed.
    pub fn notify() {
        if sandbox().is_some() {
            return;
        }
        #[link(name = "shell32")]
        extern "system" {
            fn SHChangeNotify(id: i32, flags: u32, a: *const std::ffi::c_void, b: *const std::ffi::c_void);
        }
        // SHCNE_ASSOCCHANGED, SHCNF_IDLIST
        unsafe { SHChangeNotify(0x0800_0000, 0, std::ptr::null(), std::ptr::null()) };
    }
}

/// Installed: registered and the exe is in place.
pub fn is_installed() -> bool {
    #[cfg(windows)]
    {
        reg::is_registered() && installed_exe().exists()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Install (or reinstall): copy this exe, make shortcuts, register file types.
#[cfg(windows)]
pub fn install() -> Result<PathBuf, String> {
    let dst = installed_exe();
    std::fs::create_dir_all(install_dir()).map_err(|e| format!("Can't create {}: {e}", install_dir().display()))?;
    if !running_installed() {
        let src = std::env::current_exe().map_err(|e| e.to_string())?;
        // the installed copy may be running in another window: move it aside first
        if dst.exists() {
            let old = dst.with_extension("old.exe");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&dst, &old);
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("Can't copy to {}: {e}", dst.display()))?;
    }
    for lnk in [start_menu_lnk(), desktop_lnk()].into_iter().flatten() {
        if let Some(d) = lnk.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let mut l = mslnk::ShellLink::new(&dst).map_err(|e| format!("Can't make a shortcut: {e}"))?;
        l.set_icon_location(Some(dst.display().to_string()));
        l.create_lnk(&lnk).map_err(|e| format!("Can't write {}: {e}", lnk.display()))?;
    }
    reg::register(&dst).map_err(|e| format!("Can't register file types: {e}"))?;
    reg::notify();
    Ok(dst)
}

/// Remove shortcuts, registry entries and the installed folder (after this process exits).
#[cfg(windows)]
pub fn uninstall() -> Result<(), String> {
    for lnk in [start_menu_lnk(), desktop_lnk()].into_iter().flatten() {
        let _ = std::fs::remove_file(lnk);
    }
    reg::unregister().map_err(|e| format!("Can't remove registry entries: {e}"))?;
    reg::notify();
    let dir = install_dir();
    if running_installed() {
        // a running exe can't delete itself: a detached shell waits, then removes the folder
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/C ping -n 3 127.0.0.1 >NUL & rmdir /S /Q \"{}\"", dir.display()))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn();
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// Open Settings > Apps > Default apps at ArkImage's page, where the user picks it.
#[cfg(windows)]
pub fn open_default_apps() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd")
        .raw_arg("/C start \"\" \"ms-settings:defaultapps?registeredAppUser=ArkImage\"")
        .creation_flags(0x0800_0000)
        .spawn();
}

#[cfg(not(windows))]
pub fn install() -> Result<PathBuf, String> {
    Err("Install is for Windows; on macOS, drag ArkImage to Applications.".into())
}
#[cfg(not(windows))]
pub fn uninstall() -> Result<(), String> {
    Ok(())
}
#[cfg(not(windows))]
pub fn open_default_apps() {}

/// The install folder relative to the user's profile folder (AppData\Local\Programs\ArkImage),
/// short enough for one line of the card; the full path when it is elsewhere.
fn short_install_dir() -> String {
    let d = install_dir();
    match dirs::home_dir().and_then(|h| d.strip_prefix(h).ok().map(|p| p.to_path_buf())) {
        Some(rel) => rel.display().to_string(),
        None => d.display().to_string(),
    }
}

/// The install card: a dark modal in the app's own style, drawn by ui::modal_card.
pub enum Card {
    /// Install or reinstall (true) question.
    Install(bool),
    Installed,
    Failed(String),
    /// Checking for an update (Settings > Check now).
    Checking,
    /// A newer version was downloaded and swapped in.
    UpdateReady(String),
    UpToDate,
    UpdateFailed(String),
}

impl Card {
    /// Title, body lines, and buttons (primary first).
    pub fn content(&self) -> (String, Vec<String>, Vec<&'static str>) {
        match self {
            Card::Install(re) => {
                let what = if *re { "Reinstall" } else { "Install" };
                (
                    format!("{what} ArkImage"),
                    vec![
                        format!("Copies it to {}", short_install_dir()),
                        "Adds Start menu and desktop shortcuts".into(),
                        "Registers it for all supported image and archive types".into(),
                        "No administrator rights needed.".into(),
                    ],
                    vec![what, "Not now"],
                )
            }
            Card::Installed => (
                "ArkImage is installed".into(),
                vec![
                    "Windows lets only you choose the default app for a file type.".into(),
                    "Open Default apps to make ArkImage your image viewer.".into(),
                ],
                vec!["Open Default apps", "Close"],
            ),
            Card::Failed(e) => ("Install failed".into(), vec![e.clone()], vec!["Close"]),
            Card::Checking => ("Checking for updates".into(), vec![format!("ArkImage {} is running.", env!("CARGO_PKG_VERSION"))], vec!["Close"]),
            Card::UpdateReady(v) => (
                format!("ArkImage {v} is ready"),
                vec!["The new version replaced the app file.".into(), "Restart to use it; your open images reopen.".into()],
                vec!["Restart now", "Later"],
            ),
            Card::UpToDate => (
                "ArkImage is up to date".into(),
                vec![format!("Version {} is the latest release.", env!("CARGO_PKG_VERSION"))],
                vec!["Close"],
            ),
            Card::UpdateFailed(e) => ("Update check failed".into(), vec![e.clone()], vec!["Close"]),
        }
    }

    /// Button `i` was pressed (0 is the primary); returns the card to show next.
    pub fn press(self, i: usize) -> Option<Card> {
        match (self, i) {
            (Card::Install(_), 0) => Some(match install() {
                Ok(_) => Card::Installed,
                Err(e) => Card::Failed(e),
            }),
            (Card::Installed, 0) => {
                open_default_apps();
                None
            }
            (Card::UpdateReady(_), 0) => {
                crate::update::request_restart();
                None
            }
            _ => None,
        }
    }
}
