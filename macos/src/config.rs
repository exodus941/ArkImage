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
    pub stretch_small: bool,
    /// 0 none (nearest), 1 soften (bilinear), 2 soften+sharpen (Lanczos3)
    pub filter: u8,
    pub exif_rotate: bool,
    pub esc_exits: bool,
    /// Draw the image layer on the GPU (Settings > Image Processing); off forces the processor.
    pub gpu: bool,
    pub confirm_delete: bool,
    /// 0 stop with message, 1 loop to first, 2 go to next folder/archive
    pub end_action: u8,
    pub photo_folder1: String,
    pub photo_folder2: String,
    pub editor: String,
    pub last_folder: String,
    /// The first-run "install ArkImage?" question was asked.
    pub install_offered: bool,
    /// Check GitHub for updates at each start, and when the last check ran (unix seconds).
    pub auto_update: bool,
    pub update_checked: u64,
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
    /// Closed in full screen: the next start opens in full screen.
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
            stretch_small: false,
            filter: 2,
            exif_rotate: true,
            esc_exits: true,
            gpu: true,
            confirm_delete: true,
            end_action: 0,
            photo_folder1: String::new(),
            photo_folder2: String::new(),
            editor: String::new(),
            last_folder: String::new(),
            install_offered: false,
            auto_update: true,
            update_checked: 0,
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

fn b(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// The image cache size, MB, from the machine's memory (not a setting; an old ini's
/// `cache_mb` line is ignored): 1024 with 16 GB or less, 2048 up to 32 GB, 4096 above.
/// In GPU mode the same limit applies to the images' GPU memory (on the Mac the GPU uses
/// the system memory).
pub fn cache_mb() -> u64 {
    static MB: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MB.get_or_init(|| {
        let gb = machine_memory().unwrap_or(8 << 30) as f64 / (1u64 << 30) as f64;
        if gb <= 16.0 {
            1024
        } else if gb <= 32.0 {
            2048
        } else {
            4096
        }
    })
}

/// The machine's memory, bytes.
#[cfg(target_os = "macos")]
pub fn machine_memory() -> Option<u64> {
    extern "C" {
        fn sysctlbyname(name: *const std::ffi::c_char, old: *mut std::ffi::c_void, oldlen: *mut usize, new: *mut std::ffi::c_void, newlen: usize) -> i32;
    }
    let mut v: u64 = 0;
    let mut n = std::mem::size_of::<u64>();
    let r = unsafe { sysctlbyname(c"hw.memsize".as_ptr(), &mut v as *mut u64 as _, &mut n, std::ptr::null_mut(), 0) };
    (r == 0).then_some(v)
}

#[cfg(not(target_os = "macos"))]
pub fn machine_memory() -> Option<u64> {
    None
}

impl Config {
    pub fn dir() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                if d.join("ArkImage.ini").exists() {
                    return d.to_path_buf();
                }
            }
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ArkImage")
    }

    /// "" for the normal copy; "-p<hash of its folder>" for a portable copy, so a portable
    /// copy (a test build, say) runs as its own instance and never hands its files to the
    /// installed app, nor shares its backup.
    pub fn instance_tag() -> String {
        let d = Self::dir();
        let normal = dirs::config_dir().map(|c| c.join("ArkImage")) == Some(d.clone());
        if normal {
            return String::new();
        }
        let h = d.to_string_lossy().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
        format!("-p{:08x}", h as u32)
    }

    pub fn path() -> PathBuf {
        Self::dir().join("ArkImage.ini")
    }

    pub fn load() -> Self {
        let mut c = Self::default();
        if let Ok(s) = fs::read_to_string(Self::path()) {
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
            "gpu" => self.gpu = b(v),
            "confirm_delete" => self.confirm_delete = b(v),
            "end_action" => self.end_action = n().unwrap_or(0).min(2) as u8,
            "photo_folder1" => self.photo_folder1 = v.to_string(),
            "photo_folder2" => self.photo_folder2 = v.to_string(),
            "editor" => self.editor = v.to_string(),
            "last_folder" => self.last_folder = v.to_string(),
            "install_offered" => self.install_offered = b(v),
            "auto_update" => self.auto_update = b(v),
            "update_checked" => self.update_checked = n().unwrap_or(0),
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
             stretch_small = {}\n\
             ; filter preset: 0 none, 1 soften, 2 soften+sharpen, 3 sharpen more,\n\
             ; 4 sharpen fine detail, 5 and up = filter_preset lines below, in order\n\
             filter = {}\n\
             exif_rotate = {}\n\
             esc_exits = {}\n\
             ; draw the image layer on the GPU: 1 on, 0 the processor only\n\
             gpu = {}\n\
             confirm_delete = {}\n\
             ; at last/first image: 0 stop, 1 loop, 2 next folder/archive\n\
             end_action = {}\n\
             photo_folder1 = {}\n\
             photo_folder2 = {}\n\
             ; image editor executable for Ctrl+E\n\
             editor = {}\n\
             last_folder = {}\n\
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
            self.stretch_small as u8,
            self.filter,
            self.exif_rotate as u8,
            self.esc_exits as u8,
            self.gpu as u8,
            self.confirm_delete as u8,
            self.end_action,
            self.photo_folder1,
            self.photo_folder2,
            self.editor,
            self.last_folder,
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
