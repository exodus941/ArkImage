//! Configuration dialog (F5). Pages and settings follow Honeyview's; the look follows
//! WinUI 3 dark (see settings_draw.rs). Changes apply live; OK saves, closing the
//! window (X or Esc) restores the settings it opened with.

use std::{num::NonZeroU32, sync::Arc};

use winit::window::Window;

use crate::config::Config;
use crate::render::Canvas;
use crate::text::Face;
use crate::ui::{self as u, SLATE100, SLATE200, SLATE300, SLATE400, SLATE500, ACCENT};
use crate::text::Text;

#[path = "settings_draw.rs"]
mod drawing;
use drawing::{draw, DrawState};

pub const W: f64 = 880.0;
pub const H: f64 = 600.0;

pub const SWATCHES: [u32; 15] = [
    0x202020, 0x101010, 0x000000, 0x404040, 0x808080, 0xC0C0C0, 0xFFFFFF, 0x1B3A5C, 0x2B5797, 0x5A2D82, 0x7A1F3D, 0x8A4B08, 0x2E5E2E,
    0x3D5A5A, 0x4B3B2F,
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BoolF {
    ConfirmDelete,
    EscExits,
    LockTop,
    LockBottom,
    AlignTop,
    StretchSmall,
    ExifRotate,
    Grid,
    WheelZoom,
    ClickSide,
    SortDesc,
    SsLoop,
    SsRandom,
    AutoUpdate,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChoiceF {
    EndAction,
    ViewMode,
    Filter,
    Sort,
    Double,
    /// Resize kernel of the selected filter preset.
    Kernel,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NumF {
    CacheMb,
    ThumbCacheMb,
    SlideSecs,
    /// Sharpen amount, radius and threshold of the selected filter preset.
    Amount,
    Radius,
    Threshold,
}

/// The Filters tab's index in TABS.
pub const FILTERS_TAB: usize = 3;

/// The selected preset, if the user may edit it.
pub fn user_preset(c: &mut Config) -> Option<&mut crate::filters::Preset> {
    let i = (c.filter as usize).checked_sub(crate::filters::builtin_count())?;
    c.filter_presets.get_mut(i)
}

/// True when the selected preset is one of the read-only built-ins.
pub fn preset_locked(c: &Config) -> bool {
    crate::filters::is_builtin(c.filter)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PathF {
    Photo1,
    Photo2,
    Editor,
}

impl BoolF {
    pub fn get(self, c: &Config) -> bool {
        match self {
            BoolF::ConfirmDelete => c.confirm_delete,
            BoolF::EscExits => c.esc_exits,
            BoolF::LockTop => c.lock_top,
            BoolF::LockBottom => c.lock_bottom,
            BoolF::AlignTop => c.align_top,
            BoolF::StretchSmall => c.stretch_small,
            BoolF::ExifRotate => c.exif_rotate,
            BoolF::Grid => c.transparent_grid,
            BoolF::WheelZoom => c.wheel_zoom,
            BoolF::ClickSide => c.click_side_turn,
            BoolF::SortDesc => c.sort_desc,
            BoolF::SsLoop => c.slideshow_loop,
            BoolF::SsRandom => c.slideshow_random,
            BoolF::AutoUpdate => c.auto_update,
        }
    }
    pub fn toggle(self, c: &mut Config) {
        let v = !self.get(c);
        match self {
            BoolF::ConfirmDelete => c.confirm_delete = v,
            BoolF::EscExits => c.esc_exits = v,
            BoolF::LockTop => c.lock_top = v,
            BoolF::LockBottom => c.lock_bottom = v,
            BoolF::AlignTop => c.align_top = v,
            BoolF::StretchSmall => c.stretch_small = v,
            BoolF::ExifRotate => c.exif_rotate = v,
            BoolF::Grid => c.transparent_grid = v,
            BoolF::WheelZoom => c.wheel_zoom = v,
            BoolF::ClickSide => c.click_side_turn = v,
            BoolF::SortDesc => c.sort_desc = v,
            BoolF::SsLoop => c.slideshow_loop = v,
            BoolF::SsRandom => c.slideshow_random = v,
            BoolF::AutoUpdate => c.auto_update = v,
        }
    }
}

impl ChoiceF {
    pub fn options(self, c: &Config) -> Vec<String> {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        match self {
            ChoiceF::EndAction => s(&["Do nothing", "Go to the First File", "Go to the Next/Previous Folder/File"]),
            ChoiceF::ViewMode => s(&["Original Size (100%)", "Fit in Window", "Fit to Width"]),
            ChoiceF::Filter => crate::filters::all(&c.filter_presets)
                .into_iter()
                .enumerate()
                .map(|(i, p)| if crate::filters::is_builtin(i as u8) { format!("{} (built-in)", p.name) } else { p.name })
                .collect(),
            ChoiceF::Sort => s(&["Name", "Date modified", "Size", "Type"]),
            ChoiceF::Double => s(&["Off", "Smart Double page (Left->Right)", "Smart Double page (Left<-Right)"]),
            ChoiceF::Kernel => s(&crate::filters::KERNELS),
        }
    }
    pub fn get(self, c: &Config) -> u8 {
        match self {
            ChoiceF::EndAction => c.end_action,
            ChoiceF::ViewMode => c.view_mode,
            ChoiceF::Filter => c.filter,
            ChoiceF::Sort => c.sort,
            ChoiceF::Double => c.double,
            ChoiceF::Kernel => crate::filters::get(&c.filter_presets, c.filter).kernel,
        }
    }
    /// False for a control the user may not change now (a built-in preset's fields).
    pub fn editable(self, c: &Config) -> bool {
        self != ChoiceF::Kernel || !preset_locked(c)
    }
    pub fn set(self, c: &mut Config, v: u8) {
        match self {
            ChoiceF::EndAction => c.end_action = v,
            ChoiceF::ViewMode => c.view_mode = v,
            ChoiceF::Filter => c.filter = v,
            ChoiceF::Sort => c.sort = v,
            ChoiceF::Double => c.double = v,
            ChoiceF::Kernel => {
                if let Some(p) = user_preset(c) {
                    p.kernel = v.min(crate::filters::KERNELS.len() as u8 - 1);
                }
            }
        }
    }
}

impl NumF {
    /// (value, step, min, max, unit)
    pub fn spec(self, c: &Config) -> (i64, i64, i64, i64, &'static str) {
        use crate::filters as f;
        let p = f::get(&c.filter_presets, c.filter);
        match self {
            NumF::CacheMb => (c.cache_mb as i64, 128, 128, 16384, "MB"),
            NumF::ThumbCacheMb => (c.thumb_cache_mb as i64, 256, 0, 16384, "MB"),
            NumF::SlideSecs => (c.slideshow_secs as i64, 1, 1, 3600, "s"),
            NumF::Amount => (p.amount as i64, 10, 0, f::AMOUNT_MAX as i64, ""),
            NumF::Radius => (p.radius as i64, 10, f::RADIUS_MIN as i64, f::RADIUS_MAX as i64, "px"),
            NumF::Threshold => (p.threshold as i64, 1, 0, f::THRESHOLD_MAX as i64, ""),
        }
    }
    /// The value as shown: amount and radius are stored in hundredths.
    pub fn show(self, c: &Config) -> String {
        let (v, _, _, _, unit) = self.spec(c);
        let num = match self {
            NumF::Amount | NumF::Radius => format!("{:.2}", v as f64 / 100.0),
            _ => v.to_string(),
        };
        if unit.is_empty() { num } else { format!("{num} {unit}") }
    }
    pub fn editable(self, c: &Config) -> bool {
        !matches!(self, NumF::Amount | NumF::Radius | NumF::Threshold) || !preset_locked(c)
    }
    pub fn step(self, c: &mut Config, dir: i64) {
        if !self.editable(c) {
            return;
        }
        let (v, st, _, _, _) = self.spec(c);
        // snap to the step grid, then move one step
        self.set(c, (v / st) * st + dir * st);
    }
    /// Amount and radius take two decimals.
    pub fn decimals(self) -> bool {
        matches!(self, NumF::Amount | NumF::Radius)
    }
    /// The value as typed: the shown number without its unit.
    pub fn plain(self, c: &Config) -> String {
        let v = self.spec(c).0;
        if self.decimals() { format!("{:.2}", v as f64 / 100.0) } else { v.to_string() }
    }
    /// Parse typed text into the stored unit; None if it is not a number.
    pub fn parse(self, s: &str) -> Option<i64> {
        let x: f64 = s.trim().parse().ok().filter(|x: &f64| x.is_finite())?;
        Some(if self.decimals() { (x * 100.0).round() as i64 } else { x.round() as i64 })
    }
    /// Store a value (in the stored unit), clamped to the field's range.
    pub fn set(self, c: &mut Config, v: i64) {
        if !self.editable(c) {
            return;
        }
        let (_, _, lo, hi, _) = self.spec(c);
        let nv = v.clamp(lo, hi);
        match self {
            NumF::CacheMb => c.cache_mb = nv as u64,
            NumF::ThumbCacheMb => c.thumb_cache_mb = nv as u64,
            NumF::SlideSecs => c.slideshow_secs = nv as u32,
            NumF::Amount => user_preset(c).into_iter().for_each(|p| p.amount = nv as u16),
            NumF::Radius => user_preset(c).into_iter().for_each(|p| p.radius = nv as u16),
            NumF::Threshold => user_preset(c).into_iter().for_each(|p| p.threshold = nv as u8),
        }
    }
}

impl PathF {
    pub fn get(self, c: &Config) -> &str {
        match self {
            PathF::Photo1 => &c.photo_folder1,
            PathF::Photo2 => &c.photo_folder2,
            PathF::Editor => &c.editor,
        }
    }
    pub fn set(self, c: &mut Config, v: String) {
        match self {
            PathF::Photo1 => c.photo_folder1 = v,
            PathF::Photo2 => c.photo_folder2 = v,
            PathF::Editor => c.editor = v,
        }
    }
}

enum Ctl {
    Check(&'static str, BoolF),
    Choice(&'static str, ChoiceF),
    Num(&'static str, NumF),
    Swatches(&'static str),
    Path(&'static str, PathF),
    Button(&'static str, Act),
    /// Two buttons on one row; the second is greyed when `bool` is false.
    Buttons(&'static str, Act, &'static str, Act),
    Note(&'static str),
    /// Name of the selected filter preset, editable for the user's own.
    PresetName,
    KeyHeader,
    KeyRow(usize),
    MouseRow(usize),
    Gap,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Tab(usize),
    Toggle(BoolF),
    Choice(ChoiceF),
    Step(NumF, i64),
    Swatch(u32),
    Browse(PathF),
    Clear(PathF),
    OpenIni,
    ClearThumbs,
    /// Copy the selected filter preset into a new user preset.
    PresetNew,
    /// Delete the selected user preset.
    PresetDelete,
    /// Start typing a new name for the selected user preset.
    PresetRename,
    /// Start typing a value into a number field.
    NumEdit(NumF),
    KeyToggle(usize),
    KeyCapture(usize),
    KeyAction(usize),
    MouseToggle(usize),
    MouseAction(usize),
    /// Reset every setting to its default (asks first).
    Reset,
    /// Install or reinstall ArkImage for this user.
    Install,
    /// Check GitHub for a newer version now.
    CheckUpdate,
    Ok,
    /// Close without saving.
    Cancel,
}

pub const TABS: [&str; 8] = ["General", "View", "Image Processing", "Filters", "Navigation", "Keyboard", "Mouse", "Miscellaneous"];

fn page(i: usize) -> Vec<Ctl> {
    use Ctl::*;
    match i {
        0 => vec![
            Check("Ask for confirmation before deleting a file", BoolF::ConfirmDelete),
            Check("ESC key to exit window", BoolF::EscExits),
            Gap,
            Choice("Action when on last/first image:", ChoiceF::EndAction),
            Gap,
            Button("ArkImage for this Windows user", Act::Install),
            Check("Check for updates once a day", BoolF::AutoUpdate),
            Button("Check for updates now", Act::CheckUpdate),
            Button("Reset all settings to their defaults", Act::Reset),
        ],
        1 => vec![
            Check("Keep the top menu bar visible (Lock, hotkey `)", BoolF::LockTop),
            Check("Keep the bottom control bar visible (Lock, hotkey ~)", BoolF::LockBottom),
            Check("Show the top of the image if the image is bigger than window", BoolF::AlignTop),
            Check("Stretch Small Images", BoolF::StretchSmall),
            Gap,
            Choice("Default view:", ChoiceF::ViewMode),
            Swatches("Background Color:"),
        ],
        2 => vec![
            Check("Auto rotate images based on EXIF info", BoolF::ExifRotate),
            Check("Grid background for transparent images", BoolF::Grid),
            Gap,
            Num("Cache memory size:", NumF::CacheMb),
            Num("Thumbnail cache on disk (0 = off):", NumF::ThumbCacheMb),
        ],
        FILTERS_TAB => vec![
            Choice("Preset:", ChoiceF::Filter),
            Buttons("New (copy of this preset)", Act::PresetNew, "Delete", Act::PresetDelete),
            PresetName,
            Gap,
            Choice("Resize kernel:", ChoiceF::Kernel),
            Num("Sharpen amount:", NumF::Amount),
            Num("Sharpen radius:", NumF::Radius),
            Num("Sharpen threshold:", NumF::Threshold),
            Gap,
            Note("Built-in presets are read-only. Make a copy to change one."),
            Note("Click a name or a value to type it. Enter or a click elsewhere keeps it; Esc undoes."),
            Note("Amount 0 turns sharpening off. Threshold skips smaller edges."),
        ],
        4 => vec![
            Choice("Sort by:", ChoiceF::Sort),
            Check("Descending", BoolF::SortDesc),
            Choice("Double page:", ChoiceF::Double),
            Gap,
            Num("Slideshow interval:", NumF::SlideSecs),
            Check("Repeat slideshow", BoolF::SsLoop),
            Check("Random slideshow order", BoolF::SsRandom),
        ],
        5 => {
            let mut v = vec![KeyHeader];
            v.extend((0..crate::bindings::KEY_SLOTS).map(KeyRow));
            v
        }
        6 => {
            let mut v = vec![];
            v.extend((0..crate::bindings::MOUSE_SLOTS.len()).map(MouseRow));
            v.extend([Gap, Check("Turn page when left or right side of screen is clicked", BoolF::ClickSide), Check("Mouse wheel zooms (overrides the wheel actions)", BoolF::WheelZoom)]);
            v
        }
        _ => vec![
            Path("Photo Folder 1:", PathF::Photo1),
            Path("Photo Folder 2:", PathF::Photo2),
            Path("Image Editor:", PathF::Editor),
            Gap,
            Button("Open configuration file", Act::OpenIni),
            Button("Clear thumbnail cache", Act::ClearThumbs),
        ],
    }
}

pub struct Hit {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    pub act: Act,
}

pub struct Dialog {
    pub window: Arc<Window>,
    _ctx: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    pub tab: usize,
    pub snapshot: Config,
    pub cursor: (f32, f32),
    pub hover: Option<Act>,
    /// Hover fade per control (0-1), and when it was last stepped.
    hover_fx: Vec<(Act, f32)>,
    hover_at: std::time::Instant,
    /// Keyboard slot waiting for a key press.
    pub capturing: Option<usize>,
    /// Typing a new name for the selected filter preset.
    pub renaming: bool,
    /// A number field being typed into, and the text so far.
    pub num_edit: Option<(NumF, String)>,
    /// Page scroll, design px, and the most it can scroll on the current page.
    pub scroll: f32,
    pub scroll_max: f32,
    /// The window has a Mica backdrop: the navigation pane is drawn see-through.
    pub mica: bool,
    /// The install card over the dialog, its button rectangles and hovered button.
    pub card: Option<crate::install::Card>,
    pub card_hits: Vec<(f32, f32, f32, f32)>,
    pub card_hover: Option<usize>,
    hits: Vec<Hit>,
}

impl Dialog {
    pub fn new(window: Arc<Window>, snapshot: Config, tab: usize) -> Option<Self> {
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        Some(Self { window, _ctx: ctx, surface, tab, snapshot, cursor: (0.0, 0.0), hover: None, hover_fx: Vec::new(), hover_at: std::time::Instant::now(), capturing: None, renaming: false, num_edit: None, scroll: 0.0, scroll_max: 0.0, mica: false, card: None, card_hits: Vec::new(), card_hover: None, hits: Vec::new() })
    }

    /// Scroll the page by `dy` design px.
    pub fn scroll_by(&mut self, dy: f32) {
        self.scroll = (self.scroll + dy).clamp(0.0, self.scroll_max);
        self.window.request_redraw();
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Act> {
        self.hits.iter().rev().find(|h| x >= h.x0 && x < h.x1 && y >= h.y0 && y < h.y1).map(|h| h.act)
    }

    /// Where a choice box sits, for placing its popup menu (physical px, bottom-left).
    pub fn anchor(&self, act: Act) -> Option<(f32, f32)> {
        self.hits.iter().find(|h| h.act == act).map(|h| (h.x0, h.y1))
    }

    pub fn render(&mut self, cfg: &Config, text: Option<&mut Text>) {
        let sz = self.window.inner_size();
        let (w, h) = (sz.width.max(1), sz.height.max(1));
        if self.surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).is_err() {
            return;
        }
        let Ok(mut buf) = self.surface.buffer_mut() else { return };
        let mut c = Canvas { buf: &mut buf, w: w as usize, h: h as usize };
        let sf = self.window.scale_factor() as f32;
        let mut hits = Vec::new();
        // hover fades: 250 ms in and out, linear; the step is capped so an idle spell
        // before the first frame does not count
        let step = (self.hover_at.elapsed().as_secs_f32().min(0.05) / 0.25).min(1.0);
        self.hover_at = std::time::Instant::now();
        if let Some(h) = self.hover {
            if !self.hover_fx.iter().any(|(k, _)| *k == h) {
                self.hover_fx.push((h, 0.0));
            }
        }
        let hov = self.hover;
        for (k, a) in &mut self.hover_fx {
            let to = if Some(*k) == hov { 1.0 } else { 0.0 };
            *a = if (*a - to).abs() <= step { to } else { *a + step * (to - *a).signum() };
        }
        self.hover_fx.retain(|(k, a)| *a > 0.0 || Some(*k) == hov);
        let moving = self.hover_fx.iter().any(|(k, a)| *a != if Some(*k) == hov { 1.0 } else { 0.0 });
        let fx = self.hover_fx.clone();
        let st = DrawState { tab: self.tab, fx: &fx, capturing: self.capturing, renaming: self.renaming, num_edit: self.num_edit.as_ref().map(|(f, s)| (*f, s.as_str())), scroll: self.scroll };
        let mut text = text;
        self.scroll_max = draw(&mut c, text.as_deref_mut(), cfg, &st, sf, &mut hits);
        self.scroll = self.scroll.min(self.scroll_max);
        if let (Some(card), Some(t)) = (&self.card, text) {
            let (title, body, buttons) = card.content();
            self.card_hits = crate::ui::modal_card(&mut c, t, sf, &title, &body, &buttons, self.card_hover);
        }
        if self.mica {
            drawing::see_through(&mut c, sf);
        }
        self.hits = hits;
        if moving {
            self.window.request_redraw();
        }
        let _ = buf.present();
    }
}

