//! Renders assets/icon.svg to a macOS iconset (16-1024 px) in the folder given as the
//! first argument; scripts/build-macos.sh turns it into ArkImage.icns with iconutil.

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output folder"));
    std::fs::create_dir_all(&out).unwrap();
    let data = std::fs::read("assets/icon.svg").expect("assets/icon.svg");
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).expect("valid SVG");
    for (pt, scale) in [16u32, 32, 128, 256, 512].into_iter().flat_map(|p| [(p, 1u32), (p, 2)]) {
        let size = pt * scale;
        let mut pm = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
        let k = size as f32 / tree.size().width().max(tree.size().height());
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
        let name = if scale == 1 { format!("icon_{pt}x{pt}.png") } else { format!("icon_{pt}x{pt}@2x.png") };
        pm.save_png(out.join(name)).unwrap();
    }
}
