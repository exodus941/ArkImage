//! The app icon at run time: the window/taskbar icon and the title bar logo.
//! Rendered from assets/icon.svg by examples/make_icon.rs; the 256 px PNG is built in.

use std::sync::{Arc, Mutex, OnceLock};

use image::RgbaImage;

static PNG_256: &[u8] = include_bytes!("../assets/icon_256.png");

fn master() -> &'static RgbaImage {
    static M: OnceLock<RgbaImage> = OnceLock::new();
    M.get_or_init(|| image::load_from_memory(PNG_256).map(|i| i.into_rgba8()).unwrap_or_else(|_| RgbaImage::new(1, 1)))
}

/// The icon at `size` x `size` px (Lanczos from 256 px), cached for the last size asked.
pub fn scaled(size: u32) -> Arc<RgbaImage> {
    static C: Mutex<Option<(u32, Arc<RgbaImage>)>> = Mutex::new(None);
    let mut g = C.lock().unwrap();
    if let Some((s, img)) = &*g {
        if *s == size {
            return img.clone();
        }
    }
    let img = Arc::new(crate::render::resize(master(), size.max(1), size.max(1), 2));
    *g = Some((size, img.clone()));
    img
}

pub fn window_icon() -> Option<winit::window::Icon> {
    let m = crate::render::resize(master(), 64, 64, 2);
    winit::window::Icon::from_rgba(m.into_raw(), 64, 64).ok()
}
