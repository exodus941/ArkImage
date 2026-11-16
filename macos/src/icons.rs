//! Interface icons: Tabler Icons v3.52.0 outline SVGs (MIT, assets/icons/), built into
//! the exe and drawn with resvg at the size and colour asked, then cached.

use std::{cell::RefCell, collections::HashMap};

use crate::render::{mix, Canvas};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    Eject,
    Menu,
    Plus,
    Minus,
    RotateLeft,
    RotateRight,
    Lock,
    LockOpen,
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    Close,
    Square,
    Squares,
    Maximize,
    ArrowsMaximize,
    Camera,
    Download,
    CircleCheck,
    Loader,
    RotateCw2,
    Restore,
    GitHub,
    Refresh,
    Check,
    CopyPlus,
    Trash,
    FolderOpen,
    Eraser,
    FileText,
    Eye,
    PlayerPlay,
    Bookmark,
    Pencil,
    Copy,
}

fn svg(i: Icon) -> &'static str {
    match i {
        Icon::Eject => include_str!("../assets/icons/player-eject.svg"),
        Icon::Menu => include_str!("../assets/icons/menu-2.svg"),
        Icon::Plus => include_str!("../assets/icons/plus.svg"),
        Icon::Minus => include_str!("../assets/icons/minus.svg"),
        Icon::RotateLeft => include_str!("../assets/icons/rotate.svg"),
        Icon::RotateRight => include_str!("../assets/icons/rotate-clockwise.svg"),
        Icon::Lock => include_str!("../assets/icons/lock.svg"),
        Icon::LockOpen => include_str!("../assets/icons/lock-open.svg"),
        Icon::ChevronLeft => include_str!("../assets/icons/chevron-left.svg"),
        Icon::ChevronRight => include_str!("../assets/icons/chevron-right.svg"),
        Icon::ChevronDown => include_str!("../assets/icons/chevron-down.svg"),
        Icon::Close => include_str!("../assets/icons/x.svg"),
        Icon::Square => include_str!("../assets/icons/square.svg"),
        Icon::Squares => include_str!("../assets/icons/squares.svg"),
        Icon::Maximize => include_str!("../assets/icons/maximize.svg"),
        Icon::ArrowsMaximize => include_str!("../assets/icons/arrows-maximize.svg"),
        Icon::Camera => include_str!("../assets/icons/camera.svg"),
        Icon::Download => include_str!("../assets/icons/download.svg"),
        Icon::CircleCheck => include_str!("../assets/icons/circle-check.svg"),
        Icon::Loader => include_str!("../assets/icons/loader-2.svg"),
        Icon::RotateCw2 => include_str!("../assets/icons/rotate-clockwise-2.svg"),
        Icon::Restore => include_str!("../assets/icons/restore.svg"),
        Icon::GitHub => include_str!("../assets/icons/brand-github.svg"),
        Icon::Refresh => include_str!("../assets/icons/refresh.svg"),
        Icon::Check => include_str!("../assets/icons/check.svg"),
        Icon::CopyPlus => include_str!("../assets/icons/copy-plus.svg"),
        Icon::Trash => include_str!("../assets/icons/trash.svg"),
        Icon::FolderOpen => include_str!("../assets/icons/folder-open.svg"),
        Icon::Eraser => include_str!("../assets/icons/eraser.svg"),
        Icon::FileText => include_str!("../assets/icons/file-text.svg"),
        Icon::Eye => include_str!("../assets/icons/eye.svg"),
        Icon::PlayerPlay => include_str!("../assets/icons/player-play.svg"),
        Icon::Bookmark => include_str!("../assets/icons/bookmark.svg"),
        Icon::Pencil => include_str!("../assets/icons/pencil.svg"),
        Icon::Copy => include_str!("../assets/icons/copy.svg"),
    }
}

/// Stroke width on Tabler's 24-unit grid.
const STROKE: f32 = 1.5;

thread_local! {
    /// Coverage masks by (icon, size px, rotation in degrees).
    static CACHE: RefCell<HashMap<(Icon, u32, u16), (u32, Vec<u8>)>> = RefCell::new(HashMap::new());
}

/// Coverage mask of `i` at `size` x `size` px, turned `deg` degrees clockwise.
fn mask(i: Icon, size: u32, deg: u16) -> Option<(u32, Vec<u8>)> {
    if let Some(m) = CACHE.with(|c| c.borrow().get(&(i, size, deg)).cloned()) {
        return Some(m);
    }
    let src = svg(i).replace("stroke-width=\"2\"", &format!("stroke-width=\"{STROKE}\"")).replace("currentColor", "#000");
    let tree = resvg::usvg::Tree::from_str(&src, &resvg::usvg::Options::default()).ok()?;
    let mut pm = resvg::tiny_skia::Pixmap::new(size, size)?;
    let s = size as f32 / 24.0;
    let tf = resvg::tiny_skia::Transform::from_scale(s, s).pre_rotate_at(deg as f32, 12.0, 12.0);
    resvg::render(&tree, tf, &mut pm.as_mut());
    let a: Vec<u8> = pm.pixels().iter().map(|p| p.alpha()).collect();
    let m = (size, a);
    CACHE.with(|c| c.borrow_mut().insert((i, size, deg), m.clone()));
    Some(m)
}

/// Draw `i` centred on (cx, cy), `size` px square, in `color`.
pub fn draw(c: &mut Canvas, i: Icon, cx: f32, cy: f32, size: f32, color: u32) {
    draw_ex(c, i, cx, cy, size, color, 0, 1.0);
}

/// As `draw`, turned `deg` degrees (a throbber passes steps of 10) at `alpha` 0-1.
pub fn draw_ex(c: &mut Canvas, i: Icon, cx: f32, cy: f32, size: f32, color: u32, deg: u16, alpha: f32) {
    let n = size.round().max(1.0) as u32;
    let Some((n, a)) = mask(i, n, deg % 360) else { return };
    let k255 = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
    let x0 = (cx - n as f32 / 2.0).round() as i64;
    let y0 = (cy - n as f32 / 2.0).round() as i64;
    for y in 0..n as i64 {
        let yy = y0 + y;
        if yy < 0 || yy >= c.h as i64 {
            continue;
        }
        for x in 0..n as i64 {
            let xx = x0 + x;
            let v = a[(y * n as i64 + x) as usize] as u32 * k255 >> 8;
            if v == 0 || xx < 0 || xx >= c.w as i64 {
                continue;
            }
            let k = yy as usize * c.w + xx as usize;
            c.buf[k] = mix(c.buf[k], color, v);
        }
    }
}
