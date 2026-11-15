//! Renders assets/icon.svg to the app icon files:
//! assets/icon.ico (16-256 px, embedded in the Windows exe) and PNGs used at run time.
//! Run after changing the SVG: cargo run --release --example make_icon

use image::{codecs::ico::{IcoEncoder, IcoFrame}, ExtendedColorType, RgbaImage};

fn render(tree: &resvg::usvg::Tree, size: u32) -> RgbaImage {
    let mut pm = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let k = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(tree, resvg::tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    // tiny-skia stores premultiplied alpha; image files want straight alpha
    let mut px = pm.take();
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(size, size, px).unwrap()
}

fn main() {
    let data = std::fs::read("assets/icon.svg").expect("assets/icon.svg");
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).expect("valid SVG");
    let mut frames = Vec::new();
    for size in [16u32, 20, 24, 32, 40, 48, 64, 96, 128, 256] {
        let img = render(&tree, size);
        frames.push(IcoFrame::as_png(img.as_raw(), size, size, ExtendedColorType::Rgba8).unwrap());
    }
    let f = std::fs::File::create("assets/icon.ico").unwrap();
    IcoEncoder::new(f).encode_images(&frames).unwrap();
    for size in [64u32, 256, 512] {
        render(&tree, size).save(format!("assets/icon_{size}.png")).unwrap();
    }
    println!("wrote assets/icon.ico and assets/icon_{{64,256,512}}.png");
}
