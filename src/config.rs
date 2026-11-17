use std::{fs, path::PathBuf};

use crate::bindings;

/// Settings, stored as `key = value` lines in `ArkImage.ini`.
/// Portable mode: if `ArkImage.ini` sits next to the executable, that copy is used.
#[derive(Clone, PartialEq)]
pub struct Config {
    pub bg_color: u32,
    pub transparent_grid: bool,
    /// 0 name, 1 date modified, 2 size, 3 type
    pub sort: u8,
    pub sort_desc: bool,
    /// 0 original, 1 fit window, 2 fit width
    pub view_mode: u8,
    /// 0 off, 1 left->right, 2 right->left
    pub double: u8,
    pub stretch_small: bool,
    /// 0 none (nearest), 1 soften (bilinear), 2 soften+sharpen (Lanczos3)
    pub filter: u8,
    pub exif_rotate: bool,
    pub esc_exits: bool,
    pub confirm_delete: bool,
    /// 0 stop with message, 1 loop to first, 2 go to next folder/archive
    pub end_action: u8,
    pub slideshow_secs: u32,
    pub slideshow_loop: bool,
    pub slideshow_random: bool,
    pub photo_folder1: String,
    pub photo_folder2: String,
    pub editor: String,
    pub last_folder: String,
    /// The first-run "install ArkImage?" question was asked.
    pub install_offered: bool,
    /// Check GitHub for updates at each start, and when the last check ran (unix seconds).
    pub auto_update: bool,
    pub update_checked: u64,
    pub cache_mb: u64,
    /// Disk thumbnail cache cap in MB; 0 turns it off.
    pub thumb_cache_mb: u64,
    pub align_top: bool,
    pub wheel_zoom: bool,
    pub click_side_turn: bool,
    /// 0 bilinear, 1 bicubic, 2 Lanczos3, 3 Lanczos4, 4 Mitchell
    pub interp: u8,
    pub lock_top: bool,
    pub lock_bottom: bool,
    /// Slide between images: 0 (off) or 250 ms (on).
    pub slide_ms: u16,
    /// Window at last close: restored (not maximized) outer position and inner size in
    /// physical px (win_w 0 = never saved), maximized flag, and the monitor name.
    pub win_x: i32,
    pub win_y: i32,
    pub win_w: u32,
    pub win_h: u32,
    pub win_max: bool,
    pub win_fs: bool,
    pub win_monitor: String,
    /// 11 custom hotkey slots.
    pub keys: Vec<bindings::Bind>,
    /// Mouse slots in bindings::MOUSE_SLOTS order.
    pub mouse: Vec<bindings::Bind>,
    /// The user's filter presets (the built-ins are in filters.rs).
    pub filter_presets: Vec<crate::filters::Preset>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bg_color: 0x202020,
            transparent_grid: false,
            sort: 0,
            sort_desc: false,
            view_mode: 1,
            double: 0,
            stretch_small: false,
            filter: 2,
            exif_rotate: true,
            esc_exits: true,
            confirm_delete: true,
            end_action: 0,
            slideshow_secs: 3,
            slideshow_loop: true,
            slideshow_random: false,
            photo_folder1: String::new(),
            photo_folder2: String::new(),
            editor: String::new(),
            last_folder: String::new(),
            install_offered: false,
            auto_update: true,
            update_checked: 0,
            cache_mb: 768,
            thumb_cache_mb: 2048,
            align_top: false,
            wheel_zoom: false,
            click_side_turn: false,
            interp: 2,
            lock_top: false,
            lock_bottom: false,
            slide_ms: 250,
            win_x: 0,
            win_y: 0,
            win_w: 0,
            win_h: 0,
            win_max: false,
            win_fs: false,
            win_monitor: String::new(),
            keys: bindings::default_keys(),
            mouse: bindings::default_mouse(),
            filter_presets: Vec::new(),
        }
    }
}

/// Where this copy keeps its settings (the user's rules, 2026-11-16):
/// 1. ARKIMAGE_CONFIG_DIR, when set (tests: a test copy never touches the user's settings).
/// 2. AppData's ArkImage.ini, when it exists: it always wins.
/// 3. Else ArkImage.ini beside the exe (portable).
/// 4. Else, on Windows, a new empty ArkImage.ini beside the exe (filled with the defaults
///    by `load`). A folder it cannot write to falls back to AppData.
/// An install moves the portable ini to AppData, so rule 2 then holds for every copy.
fn pick_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ARKIMAGE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
    pick(Config::appdata_dir(), exe_dir, cfg!(windows))
}

/// `pick_dir` without the environment: `create` allows a new ini beside the exe.
fn pick(appdata: PathBuf, exe_dir: Option<PathBuf>, create: bool) -> PathBuf {
    if appdata.join("ArkImage.ini").exists() {
        return appdata;
    }
    let Some(exe_dir) = exe_dir else { return appdata };
    let ini = exe_dir.join("ArkImage.ini");
    if ini.exists() {
        return exe_dir;
    }
    if create && fs::write(&ini, "").is_ok() {
        crate::trace(&format!("portable: created {}", ini.display()));
        return exe_dir;
    }
    appdata
}

#[cfg(test)]
mod pick_tests {
    use super::pick;
    use std::{fs, path::PathBuf};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arkimage-pick-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("appdata")).unwrap();
        fs::create_dir_all(d.join("exe")).unwrap();
        d
    }

    /// The user's rules: AppData's ini wins; else the local one; else a new local one.
    #[test]
    fn appdata_wins_then_local_then_new() {
        let d = tmp("rules");
        let (a, e) = (d.join("appdata"), d.join("exe"));
        // nothing anywhere: a new empty ini beside the exe
        assert_eq!(pick(a.clone(), Some(e.clone()), true), e);
        assert!(e.join("ArkImage.ini").exists());
        // a local ini and no AppData: the local one
        assert_eq!(pick(a.clone(), Some(e.clone()), true), e);
        // AppData's exists (after an install): it wins, and no new local ini is made
        fs::write(a.join("ArkImage.ini"), "").unwrap();
        fs::remove_file(e.join("ArkImage.ini")).unwrap();
        assert_eq!(pick(a.clone(), Some(e.clone()), true), a);
        assert!(!e.join("ArkImage.ini").exists());
        let _ = fs::remove_dir_all(&d);
    }

    /// A folder the copy cannot write to falls back to AppData.
    #[test]
    fn unwritable_folder_uses_appdata() {
        let d = tmp("ro");
        let (a, e) = (d.join("appdata"), d.join("exe").join("missing"));
        assert_eq!(pick(a.clone(), Some(e), true), a);
        let _ = fs::remove_dir_all(&d);
    }
}

fn b(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    /// The folder that holds ArkImage.ini, the session, bookmarks and crash log
    /// (see `pick_dir`).
    pub fn dir() -> PathBuf {
        pick_dir()
    }

    /// The per-user settings folder (%APPDATA%\ArkImage, ~/Library/Application Support/ArkImage).
    pub fn appdata_dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ArkImage")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("ArkImage.ini")
    }

    pub fn load() -> Self {
        let mut c = Self::default();
        let text = fs::read_to_string(Self::path());
        // a new portable ini starts empty: fill it with the defaults so it reads as settings
        if text.as_ref().is_ok_and(|s| s.trim().is_empty()) {
            c.save();
        }
        if let Ok(s) = text {
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with(['#', ';', '[']) {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    c.set(k.trim(), v.trim());
                }
            }
        }
        if c.filter as usize >= crate::filters::builtin_count() + c.filter_presets.len() {
            c.filter = 2;
        }
        c
    }

    fn set(&mut self, k: &str, v: &str) {
        let n = || v.parse::<u64>().ok();
        match k {
            "bg_color" => {
                if let Ok(x) = u32::from_str_radix(v.trim_start_matches('#'), 16) {
                    self.bg_color = x & 0xFFFFFF;
                }
            }
            "transparent_grid" => self.transparent_grid = b(v),
            "sort" => self.sort = n().unwrap_or(0).min(3) as u8,
            "sort_desc" => self.sort_desc = b(v),
            "view_mode" => self.view_mode = n().unwrap_or(1).min(2) as u8,
            "double" => self.double = n().unwrap_or(0).min(2) as u8,
            "stretch_small" => self.stretch_small = b(v),
            // checked against the preset count after loading
            "filter" => self.filter = n().unwrap_or(2).min(255) as u8,
            "filter_preset" => {
                if let Some(p) = crate::filters::Preset::parse(v) {
                    if self.filter_presets.len() + crate::filters::builtin_count() < 255 {
                        self.filter_presets.push(p);
                    }
                }
            }
            "exif_rotate" => self.exif_rotate = b(v),
            "esc_exits" => self.esc_exits = b(v),
            "confirm_delete" => self.confirm_delete = b(v),
            "end_action" => self.end_action = n().unwrap_or(0).min(2) as u8,
            "slideshow_secs" => self.slideshow_secs = n().unwrap_or(3).clamp(1, 3600) as u32,
            "slideshow_loop" => self.slideshow_loop = b(v),
            "slideshow_random" => self.slideshow_random = b(v),
            "photo_folder1" => self.photo_folder1 = v.to_string(),
            "photo_folder2" => self.photo_folder2 = v.to_string(),
            "editor" => self.editor = v.to_string(),
            "last_folder" => self.last_folder = v.to_string(),
            "install_offered" => self.install_offered = b(v),
            "auto_update" => self.auto_update = b(v),
            "update_checked" => self.update_checked = n().unwrap_or(0),
            "cache_mb" => self.cache_mb = n().unwrap_or(768).max(64),
            "thumb_cache_mb" => self.thumb_cache_mb = n().unwrap_or(2048),
            "align_top" => self.align_top = b(v),
            "wheel_zoom" => self.wheel_zoom = b(v),
            "click_side_turn" => self.click_side_turn = b(v),
            "interp" => self.interp = n().unwrap_or(2).min(4) as u8,
            "lock_top" => self.lock_top = b(v),
            "lock_bottom" => self.lock_bottom = b(v),
            "slide_ms" => self.slide_ms = if n() == Some(0) { 0 } else { 250 },
            "win_x" => self.win_x = v.parse().unwrap_or(0),
            "win_y" => self.win_y = v.parse().unwrap_or(0),
            "win_w" => self.win_w = v.parse().unwrap_or(0),
            "win_h" => self.win_h = v.parse().unwrap_or(0),
            "win_max" => self.win_max = b(v),
            "win_fs" => self.win_fs = b(v),
            "win_monitor" => self.win_monitor = v.to_string(),
            _ if k.starts_with("key") && k[3..].parse::<usize>().is_ok() => {
                let i = k[3..].parse::<usize>().unwrap();
                if (1..=bindings::KEY_SLOTS).contains(&i) {
                    if let Some(b) = bindings::Bind::parse(v, true) {
                        self.keys[i - 1] = b;
                    }
                }
            }
            _ if bindings::MOUSE_SLOTS.iter().any(|m| m.0 == k) => {
                let i = bindings::MOUSE_SLOTS.iter().position(|m| m.0 == k).unwrap();
                if let Some(b) = bindings::Bind::parse(v, false) {
                    self.mouse[i] = b;
                }
            }
            _ => {}
        }
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(Self::dir());
        let s = format!(
            "; ArkImage configuration. Edit, then press Shift+F5 in ArkImage to reload.\n\
             [config]\n\
             ; background colour, hex RRGGBB\n\
             bg_color = {:06X}\n\
             ; checkerboard behind transparent pixels\n\
             transparent_grid = {}\n\
             ; 0 name, 1 date modified, 2 size, 3 type\n\
             sort = {}\n\
             sort_desc = {}\n\
             ; 0 original size, 1 fit in window, 2 fit to width\n\
             view_mode = {}\n\
             ; double page: 0 off, 1 left->right, 2 right->left\n\
             double = {}\n\
             stretch_small = {}\n\
             ; filter preset: 0 none, 1 soften, 2 soften+sharpen, 3 sharpen more,\n\
             ; 4 sharpen fine detail, 5 and up = filter_preset lines below, in order\n\
             filter = {}\n\
             exif_rotate = {}\n\
             esc_exits = {}\n\
             confirm_delete = {}\n\
             ; at last/first image: 0 stop, 1 loop, 2 next folder/archive\n\
             end_action = {}\n\
             slideshow_secs = {}\n\
             slideshow_loop = {}\n\
             slideshow_random = {}\n\
             photo_folder1 = {}\n\
             photo_folder2 = {}\n\
             ; image editor executable for Ctrl+E\n\
             editor = {}\n\
             last_folder = {}\n\
             ; decoded image cache, megabytes\n\
             cache_mb = {}\n\
             ; show the top of an image taller than the window\n\
             align_top = {}\n\
             ; mouse wheel zooms instead of turning pages\n\
             wheel_zoom = {}\n\
             ; click the left/right 30% of the window to turn pages\n\
             click_side_turn = {}\n\
             ; resize filter when filtering is on: 0 bilinear, 1 bicubic, 2 Lanczos3, 3 Lanczos4, 4 Mitchell\n\
             interp = {}\n\
             ; keep the top menu / bottom control bar visible\n\
             lock_top = {}\n\
             lock_bottom = {}\n\
             ; slide between images: 0 off, 250 on\n\
             slide_ms = {}\n",
            self.bg_color,
            self.transparent_grid as u8,
            self.sort,
            self.sort_desc as u8,
            self.view_mode,
            self.double,
            self.stretch_small as u8,
            self.filter,
            self.exif_rotate as u8,
            self.esc_exits as u8,
            self.confirm_delete as u8,
            self.end_action,
            self.slideshow_secs,
            self.slideshow_loop as u8,
            self.slideshow_random as u8,
            self.photo_folder1,
            self.photo_folder2,
            self.editor,
            self.last_folder,
            self.cache_mb,
            self.align_top as u8,
            self.wheel_zoom as u8,
            self.click_side_turn as u8,
            self.interp,
            self.lock_top as u8,
            self.lock_bottom as u8,
            self.slide_ms,
        );
        let mut s = s;
        s.push_str(&format!(
            "; window at last close: restored position/size (physical px), maximized, full screen, monitor\n\
             win_x = {}\nwin_y = {}\nwin_w = {}\nwin_h = {}\nwin_max = {}\nwin_fs = {}\nwin_monitor = {}\n",
            self.win_x, self.win_y, self.win_w, self.win_h, self.win_max as u8, self.win_fs as u8, self.win_monitor
        ));
        s.push_str(&format!(
            "; thumbnails kept on disk for the Select Image grid, megabytes (0 = off)\nthumb_cache_mb = {}\n",
            self.thumb_cache_mb
        ));
        s.push_str("; custom hotkeys: enabled|key|action  (actions are listed in src/bindings.rs)\n");
        for (i, b) in self.keys.iter().enumerate() {
            s.push_str(&format!("key{} = {}\n", i + 1, b.format(true)));
        }
        s.push_str("; your filter presets: name|kernel|amount|radius|threshold\n");
        s.push_str("; kernel 1 none, 2 bilinear, 3 bicubic, 4 Lanczos3, 5 Lanczos4, 6 Mitchell;\n");
        s.push_str("; amount and radius in hundredths (60 = 0.60); threshold 0-64\n");
        for p in &self.filter_presets {
            s.push_str(&format!("filter_preset = {}\n", p.format()));
        }
        s.push_str("; mouse buttons and wheel: enabled|action\n");
        for (m, b) in bindings::MOUSE_SLOTS.iter().zip(&self.mouse) {
            s.push_str(&format!("{} = {}\n", m.0, b.format(false)));
        }
        s.push_str(&format!("; the first-run install question was asked
install_offered = {}
", self.install_offered as u8));
        s.push_str(&format!("; check GitHub for a new version at each start; last check (unix seconds)
auto_update = {}
update_checked = {}
", self.auto_update as u8, self.update_checked));
        let _ = fs::write(Self::path(), s);
    }
}
