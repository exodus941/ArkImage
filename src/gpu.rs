//! GPU image layer (wgpu: Metal on the Mac, DirectX 12 or Vulkan on Windows).
//!
//! The processor still draws the whole interface into a u32 frame. Pixels where the image
//! layer shows through are "holes" (see `render::HOLE`): they carry what the interface drew
//! over them (premultiplied, with its coverage) and a blur class. Each frame the GPU:
//! 1. draws the image layer: background or checkerboard, the image (uploaded once per
//!    transform, with mipmaps), and during a slide the last settled layer shifted;
//! 2. blurs that layer at quarter size for each blur class the frame uses;
//! 3. lays the interface over it: a plain pixel is opaque, a hole shows the layer (or its
//!    blur) under the interface colour.
//! One device serves every window; each window has its surface and textures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use winit::window::Window;

/// Set by the device-lost callback: every window falls back to the processor.
pub static LOST: AtomicBool = AtomicBool::new(false);

/// Per window: (last drawn, image texture bytes); and windows asked to give theirs back.
struct Ledger {
    wins: std::collections::HashMap<u64, (std::time::Instant, u64)>,
    evict: std::collections::HashSet<u64>,
}

fn ledger() -> &'static std::sync::Mutex<Ledger> {
    static L: OnceLock<std::sync::Mutex<Ledger>> = OnceLock::new();
    L.get_or_init(|| std::sync::Mutex::new(Ledger { wins: Default::default(), evict: Default::default() }))
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub name: String,
    max_tex: u32,
    layer_pl: wgpu::RenderPipeline,
    comp_pl: wgpu::RenderPipeline,
    down_pl: wgpu::RenderPipeline,
    box_pl: wgpu::RenderPipeline,
    mip_pl: wgpu::RenderPipeline,
    mblur_pl: wgpu::RenderPipeline,
    resample_pl: wgpu::RenderPipeline,
    sharp_h_pl: wgpu::RenderPipeline,
    sharp_v_pl: wgpu::RenderPipeline,
    sharp_bgl: wgpu::BindGroupLayout,
    resi_pl: wgpu::RenderPipeline,
    resi_bgl: wgpu::BindGroupLayout,
    layer_bgl: wgpu::BindGroupLayout,
    comp_bgl: wgpu::BindGroupLayout,
    tex_bgl: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
}

static GPU: OnceLock<Option<Arc<Gpu>>> = OnceLock::new();

/// The shared GPU, made on first use; None when no suitable adapter exists.
pub fn get() -> Option<Arc<Gpu>> {
    if LOST.load(Ordering::Relaxed) {
        return None;
    }
    GPU.get_or_init(|| match pollster::block_on(Gpu::new()) {
        Ok(g) => {
            crate::trace(&format!("gpu: {}", g.name));
            Some(Arc::new(g))
        }
        Err(e) => {
            crate::trace(&format!("gpu: none ({e})"));
            None
        }
    })
    .clone()
}

const QUARTER: u32 = 4;
/// Rows per upload band: 256 rows of a 6016 px image is 6 MB.
pub const BAND: u32 = 256;
const LAYER_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const SHADER: &str = r#"
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VOut;
    o.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2<f32>(x, y);
    return o;
}

// ---- image layer ----
struct L {
    size: vec2<f32>,      // target px
    org: vec2<f32>,       // image top-left, target px (slide offset included)
    scale: vec2<f32>,     // target px per texel
    tex: vec2<f32>,       // texture size
    bg: vec4<f32>,        // premultiplied background
    flags: vec4<f32>,     // x: has image, y: smooth, z: checker, w: lod
    old: vec4<f32>,       // x: has old layer, y: its x shift
    xf: vec4<f32>,        // x: rotation 0-3 (clockwise quarter turns), y: flip h, z: flip v, w: hq texture (1)
    fx: vec4<f32>,        // x: negative, y: gamma
};
@group(0) @binding(0) var img: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var oldl: texture_2d<f32>;
@group(0) @binding(3) var<uniform> l: L;

// display position (in the rotated, flipped image, `r` = its size) to texture position,
// as decode::transform: rotate clockwise, then flip h, then flip v
fn unrotate(t0: vec2<f32>, r: vec2<f32>) -> vec2<f32> {
    var t = t0;
    if (l.xf.y > 0.5) { t.x = r.x - t.x; }
    if (l.xf.z > 0.5) { t.y = r.y - t.y; }
    let rot = u32(l.xf.x + 0.5);
    if (rot == 1u) { return vec2<f32>(t.y, r.x - t.x); }
    if (rot == 2u) { return vec2<f32>(r.x - t.x, r.y - t.y); }
    if (rot == 3u) { return vec2<f32>(r.y - t.y, t.x); }
    return t;
}

// the same in whole pixels, for the full-quality texture (one texel per pixel)
fn unrotate_i(t0: vec2<i32>, r: vec2<i32>) -> vec2<i32> {
    var t = t0;
    if (l.xf.y > 0.5) { t.x = r.x - 1 - t.x; }
    if (l.xf.z > 0.5) { t.y = r.y - 1 - t.y; }
    let rot = u32(l.xf.x + 0.5);
    if (rot == 1u) { return vec2<i32>(t.y, r.x - 1 - t.x); }
    if (rot == 2u) { return vec2<i32>(r.x - 1 - t.x, r.y - 1 - t.y); }
    if (rot == 3u) { return vec2<i32>(r.y - 1 - t.y, t.x); }
    return t;
}

// Gamma (1/1.6), then Negative, as decode::transform's table
fn tone(c: vec4<f32>) -> vec4<f32> {
    var rgb = c.rgb;
    if (l.fx.y > 0.5) { rgb = pow(rgb, vec3<f32>(1.0 / 1.6)); }
    if (l.fx.x > 0.5) { rgb = vec3<f32>(1.0) - rgb; }
    return vec4<f32>(rgb, c.a);
}

fn background(p: vec2<f32>) -> vec4<f32> {
    if (l.flags.z > 0.5) {
        let c = (u32(p.x) >> 3u) ^ (u32(p.y) >> 3u);
        if ((c & 1u) == 0u) { return vec4<f32>(1.0, 1.0, 1.0, 1.0); }
        return vec4<f32>(0.8, 0.8, 0.8, 1.0);
    }
    return l.bg;
}

@fragment
fn layer(v: VOut) -> @location(0) vec4<f32> {
    let p = v.pos.xy;
    var under = background(p);
    if (l.old.x > 0.5) {
        let q = vec2<i32>(i32(floor(p.x - l.old.y)), i32(floor(p.y)));
        let d = vec2<i32>(textureDimensions(oldl));
        if (q.x >= 0 && q.x < d.x && q.y >= 0 && q.y < d.y) {
            under = textureLoad(oldl, q, 0);
        }
    }
    if (l.flags.x < 0.5) { return under; }
    // `tex` is the shown (rotated) size; the texture itself is unrotated
    let t = (p - l.org) / l.scale;
    if (t.x < 0.0 || t.y < 0.0 || t.x >= l.tex.x || t.y >= l.tex.y) { return under; }
    var c: vec4<f32>;
    if (l.xf.w > 0.5) {
        // the full-quality copy: already resized, toned; only turned here
        c = textureLoad(img, unrotate_i(vec2<i32>(floor(t)), vec2<i32>(l.tex)), 0);
    } else {
        let s = unrotate(t, l.tex);
        let d = vec2<f32>(textureDimensions(img));
        if (l.flags.y > 0.5 || l.flags.w > 0.0) {
            c = textureSampleLevel(img, smp, s / d, l.flags.w);
        } else {
            c = textureLoad(img, vec2<i32>(clamp(floor(s), vec2<f32>(0.0), d - vec2<f32>(1.0))), 0);
        }
        c = tone(c);
    }
    // the image over its background (checker for transparent images, as the processor)
    let bgc = background(p);
    let rgb = c.rgb * c.a + bgc.rgb * (1.0 - c.a);
    let a = c.a + bgc.a * (1.0 - c.a);
    return vec4<f32>(rgb, a);
}

// ---- mipmaps and blur ----
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var ssm: sampler;
struct B { dir: vec2<f32>, radius: f32, pad: f32 };
@group(0) @binding(2) var<uniform> b: B;

@fragment
fn mip(v: VOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, ssm, v.uv, 0.0);
}

// motion blur along x during a slide: MB_TAPS bilinear taps spread evenly over
// [-radius, +radius] px (the distance moved in half a frame each way)
const MB_TAPS: i32 = 24;
@fragment
fn mblur(v: VOut) -> @location(0) vec4<f32> {
    let s = vec2<f32>(textureDimensions(src));
    var acc = vec4<f32>(0.0);
    for (var k = 0; k < MB_TAPS; k++) {
        let f = (f32(k) + 0.5) / f32(MB_TAPS) * 2.0 - 1.0;
        acc += textureSampleLevel(src, ssm, v.uv + vec2<f32>(f * b.radius / s.x, 0.0), 0.0);
    }
    return acc / f32(MB_TAPS);
}

// quarter size: the average of a 4x4 block (four bilinear taps)
@fragment
fn down(v: VOut) -> @location(0) vec4<f32> {
    let s = vec2<f32>(textureDimensions(src));
    let o = 1.0 / s;
    let c = v.uv;
    return 0.25 * (textureSampleLevel(src, ssm, c + vec2<f32>(-o.x, -o.y), 0.0)
        + textureSampleLevel(src, ssm, c + vec2<f32>(o.x, -o.y), 0.0)
        + textureSampleLevel(src, ssm, c + vec2<f32>(-o.x, o.y), 0.0)
        + textureSampleLevel(src, ssm, c + vec2<f32>(o.x, o.y), 0.0));
}

// one box pass along `dir`, edges clamped (as the processor's box blur)
@fragment
fn boxp(v: VOut) -> @location(0) vec4<f32> {
    let d = vec2<i32>(textureDimensions(src));
    let p = vec2<i32>(floor(v.pos.xy));
    let r = i32(b.radius);
    var acc = vec4<f32>(0.0);
    for (var k = -r; k <= r; k++) {
        let q = clamp(p + vec2<i32>(b.dir) * k, vec2<i32>(0), d - vec2<i32>(1));
        acc += textureLoad(src, q, 0);
    }
    return acc / f32(2 * r + 1);
}

// ---- full-quality shrink: the processor's resize, then its unsharp mask ----
struct R { dir: vec2<f32>, scale: f32, support: f32, kernel: f32, p0: f32, p1: f32, p2: f32 };
@group(0) @binding(2) var<uniform> rr: R;

fn sinc(x: f32) -> f32 {
    if (abs(x) < 1e-6) { return 1.0; }
    let px = 3.14159265358979 * x;
    return sin(px) / px;
}

fn cubic(x: f32, b: f32, c: f32) -> f32 {
    let a = abs(x);
    if (a < 1.0) { return ((12.0 - 9.0 * b - 6.0 * c) * a * a * a + (-18.0 + 12.0 * b + 6.0 * c) * a * a + (6.0 - 2.0 * b)) / 6.0; }
    if (a < 2.0) { return ((-b - 6.0 * c) * a * a * a + (6.0 * b + 30.0 * c) * a * a + (-12.0 * b - 48.0 * c) * a + (8.0 * b + 24.0 * c)) / 6.0; }
    return 0.0;
}

// kernels as render::resize codes: 1 bilinear, 2 Lanczos3, 4 Catmull-Rom, 5 Lanczos4, 6 Mitchell
fn kern(k: f32, x: f32) -> f32 {
    let a = abs(x);
    if (k < 1.5) { return max(0.0, 1.0 - a); }
    if (k < 2.5) { if (a < 3.0) { return sinc(x) * sinc(x / 3.0); } return 0.0; }
    if (k < 4.5) { return cubic(x, 0.0, 0.5); }
    if (k < 5.5) { if (a < 4.0) { return sinc(x) * sinc(x / 4.0); } return 0.0; }
    return cubic(x, 1.0 / 3.0, 1.0 / 3.0);
}

// one axis of a separable resize, as fast_image_resize: output pixel x reads the source
// around (x + 0.5) * scale, the kernel widened by the scale when shrinking, weights
// normalised over the pixels inside the image
@fragment
fn resample(v: VOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(v.pos.xy));
    let d = vec2<i32>(textureDimensions(src));
    let horiz = rr.dir.x > 0.5;
    let x = select(p.y, p.x, horiz);
    let n = select(d.y, d.x, horiz);
    let center = (f32(x) + 0.5) * rr.scale;
    if (rr.kernel < 0.5) {
        let i = clamp(i32(floor(center)), 0, n - 1);
        return textureLoad(src, select(vec2<i32>(p.x, i), vec2<i32>(i, p.y), horiz), 0);
    }
    let fs = max(rr.scale, 1.0);
    let r = rr.support * fs;
    let lo = max(i32(floor(center - r)), 0);
    let hi = min(i32(ceil(center + r)), n);
    var acc = vec4<f32>(0.0);
    var ws = 0.0;
    for (var i = lo; i < hi; i++) {
        let w = kern(rr.kernel, (f32(i) + 0.5 - center) / fs);
        acc += textureLoad(src, select(vec2<i32>(p.x, i), vec2<i32>(i, p.y), horiz), 0) * w;
        ws += w;
    }
    return clamp(acc / ws, vec4<f32>(0.0), vec4<f32>(1.0));
}

// the same resize in the library's own integer arithmetic: per output pixel a start, a
// count and i16 weights at `precision` bits, the sum started at half a unit, shifted
// back and clipped to 0-255 (fast_image_resize's U8x4 convolution, bit for bit)
struct RI { horiz: u32, prec: u32, lut: u32, p1: u32 };
@group(0) @binding(1) var<storage, read> co: array<i32>;
@group(0) @binding(2) var<uniform> ri: RI;

@fragment
fn resample_i(v: VOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(v.pos.xy));
    let horiz = ri.horiz == 1u;
    let x = select(p.y, p.x, horiz);
    let h = x * 3;
    let start = co[h];
    let size = co[h + 1];
    let off = co[h + 2];
    var s = vec4<i32>(0);
    if (ri.prec > 0u) { s = vec4<i32>(1 << (ri.prec - 1u)); }
    for (var k = 0; k < size; k++) {
        let i = start + k;
        let q = select(vec2<i32>(p.x, i), vec2<i32>(i, p.y), horiz);
        var px = vec4<i32>(round(textureLoad(src, q, 0) * 255.0));
        if (ri.lut > 0u) {
            // decode::transform's Gamma/Negative table, at `lut` in the same buffer
            px = vec4<i32>(co[ri.lut + u32(px.r)], co[ri.lut + u32(px.g)], co[ri.lut + u32(px.b)], px.a);
        }
        s += px * co[off + k];
    }
    let r = clamp(s >> vec4<u32>(ri.prec), vec4<i32>(0), vec4<i32>(255));
    return vec4<f32>(r) / 255.0;
}

struct S { amount: f32, thr: f32, r: f32, p: f32, k: array<vec4<f32>, 5> };
@group(0) @binding(2) var<uniform> su: S;
fn kw(i: i32) -> f32 { return su.k[i / 4][i % 4]; }

// unsharp mask, horizontal blur of the shrunk image (0-255 floats, as the processor)
@fragment
fn sharp_h(v: VOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(v.pos.xy));
    let d = vec2<i32>(textureDimensions(src));
    let r = i32(su.r);
    var acc = vec4<f32>(0.0);
    for (var i = 0; i <= 2 * r; i++) {
        let sx = clamp(p.x + i - r, 0, d.x - 1);
        acc += kw(i) * textureLoad(src, vec2<i32>(sx, p.y), 0) * 255.0;
    }
    return acc;
}

@group(0) @binding(0) var sc: texture_2d<f32>;
@group(0) @binding(1) var sht: texture_2d<f32>;

// vertical blur, then p + amount * (p - blur) where the difference passes the threshold
@fragment
fn sharp_v(v: VOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(v.pos.xy));
    let d = vec2<i32>(textureDimensions(sc));
    let r = i32(su.r);
    var blur = vec4<f32>(0.0);
    for (var i = 0; i <= 2 * r; i++) {
        let sy = clamp(p.y + i - r, 0, d.y - 1);
        blur += kw(i) * textureLoad(sht, vec2<i32>(p.x, sy), 0);
    }
    let c = textureLoad(sc, p, 0) * 255.0;
    var o = c;
    for (var k = 0; k < 3; k++) {
        let dd = c[k] - blur[k];
        if (abs(dd) > su.thr) { o[k] = clamp(round(c[k] + su.amount * dd), 0.0, 255.0); }
    }
    return o / 255.0;
}

// ---- composite: the interface over the layer ----
@group(0) @binding(0) var ui: texture_2d<u32>;
@group(0) @binding(1) var lay: texture_2d<f32>;
@group(0) @binding(2) var b1: texture_2d<f32>;
@group(0) @binding(3) var b2: texture_2d<f32>;
@group(0) @binding(4) var b3: texture_2d<f32>;
@group(0) @binding(5) var csm: sampler;
struct C { post: f32, p1: f32, p2: f32, p3: f32 };
@group(0) @binding(6) var<uniform> cu: C;

fn out(c: vec4<f32>) -> vec4<f32> {
    // a PostMultiplied surface wants straight colour
    if (cu.post > 0.5 && c.a > 0.0 && c.a < 1.0) { return vec4<f32>(c.rgb / c.a, c.a); }
    return c;
}

@fragment
fn comp(v: VOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(v.pos.xy));
    let o = textureLoad(ui, p, 0).r;
    let top = o >> 24u;
    if ((top & 3u) != 2u) {
        return out(vec4<f32>(f32((o >> 16u) & 255u), f32((o >> 8u) & 255u), f32(o & 255u), 255.0) / 255.0);
    }
    let cls = (top >> 2u) & 3u;
    let c = (o >> 8u) & 0xFFFFu;
    let ui_c = vec3<f32>(f32((c >> 11u) & 31u) / 31.0, f32((c >> 5u) & 63u) / 63.0, f32(c & 31u) / 31.0);
    let a = f32(o & 255u) / 255.0;
    var under: vec4<f32>;
    let uv = v.pos.xy / vec2<f32>(textureDimensions(lay));
    if (cls == 1u) { under = textureSampleLevel(b1, csm, uv, 0.0); }
    else if (cls == 2u) { under = textureSampleLevel(b2, csm, uv, 0.0); }
    else if (cls == 3u) { under = textureSampleLevel(b3, csm, uv, 0.0); }
    else { under = textureLoad(lay, p, 0); }
    return out(vec4<f32>(ui_c + under.rgb * (1.0 - a), a + under.a * (1.0 - a)));
}
"#;

fn tex_entry(binding: u32, kind: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: kind, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn smp_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn buf_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

const FLOAT: wgpu::TextureSampleType = wgpu::TextureSampleType::Float { filterable: true };

impl Gpu {
    async fn new() -> Result<Gpu, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = if cfg!(target_os = "macos") { wgpu::Backends::METAL } else { wgpu::Backends::DX12 };
        // Windows: the swap chain goes on a DirectComposition layer over the window, not on
        // the window itself. One made from the HWND (flip model) took the window over for
        // good: after it was released, the processor's drawing never reached the screen
        // again, so a window going back to the processor (the glass with one window left,
        // an image past the texture limit, a GPU failure) kept the GPU's last frame.
        // ARKIMAGE_GPU_HWND=1: the HWND swap chain again (to check the fault this avoids)
        desc.backend_options.dx12.presentation_system = if std::env::var_os("ARKIMAGE_GPU_HWND").is_some() { wgpu::Dx12SwapchainKind::DxgiFromHwnd } else { wgpu::Dx12SwapchainKind::DxgiFromVisual };
        let instance = wgpu::Instance::new(desc);
        // The user's rule: a desktop always takes its graphics card (LowPower picked the
        // Intel UHD 770 beside an RTX 4070 Ti SUPER); a laptop keeps the processor's own
        // chip when it has one, for the battery.
        let power_preference = if on_battery_machine() { wgpu::PowerPreference::LowPower } else { wgpu::PowerPreference::HighPerformance };
        crate::trace(&format!("gpu: {} machine, asking for {power_preference:?}", if on_battery_machine() { "battery" } else { "desktop" }));
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions { power_preference, ..Default::default() })
            .await
            .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err(format!("software adapter {}", info.name));
        }
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("arkimage"),
                required_limits: wgpu::Limits { max_texture_dimension_2d: limits.max_texture_dimension_2d, ..wgpu::Limits::downlevel_defaults() },
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        // a GPU error must not end the app (wgpu panics by default): log it and let
        // every window fall back to the processor
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
            crate::trace(&format!("gpu error: {e}"));
            eprintln!("gpu error: {e}");
            // out of memory or an internal fault: every window falls back. A validation
            // error is one call's (seen: a surface reconfigure that could not wait for the
            // GPU, with 100 windows opening); it is logged, and the device carries on.
            if !matches!(e, wgpu::Error::Validation { .. }) {
                LOST.store(true, Ordering::Relaxed);
            }
        }));
        device.set_device_lost_callback(|r, m| {
            crate::trace(&format!("gpu lost: {r:?} {m}"));
            LOST.store(true, Ordering::Relaxed);
        });
        let sh = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("arkimage"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layer_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[tex_entry(0, FLOAT), smp_entry(1), tex_entry(2, FLOAT), buf_entry(3)],
        });
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[tex_entry(0, FLOAT), smp_entry(1), buf_entry(2)],
        });
        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0, wgpu::TextureSampleType::Uint),
                tex_entry(1, FLOAT),
                tex_entry(2, FLOAT),
                tex_entry(3, FLOAT),
                tex_entry(4, FLOAT),
                smp_entry(5),
                buf_entry(6),
            ],
        });
        let pipe = |bgl: &wgpu::BindGroupLayout, entry: &str, fmt: wgpu::TextureFormat| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &sh, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &sh,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format: fmt, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let layer_pl = pipe(&layer_bgl, "layer", LAYER_FMT);
        let comp_pl = pipe(&comp_bgl, "comp", wgpu::TextureFormat::Bgra8Unorm);
        let down_pl = pipe(&tex_bgl, "down", LAYER_FMT);
        let box_pl = pipe(&tex_bgl, "boxp", LAYER_FMT);
        let mip_pl = pipe(&tex_bgl, "mip", LAYER_FMT);
        let mblur_pl = pipe(&tex_bgl, "mblur", LAYER_FMT);
        let sharp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[tex_entry(0, FLOAT), tex_entry(1, wgpu::TextureSampleType::Float { filterable: false }), buf_entry(2)],
        });
        let resample_pl = pipe(&tex_bgl, "resample", LAYER_FMT);
        let resi_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex_entry(0, FLOAT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                buf_entry(2),
            ],
        });
        let resi_pl = pipe(&resi_bgl, "resample_i", LAYER_FMT);
        let sharp_h_pl = pipe(&tex_bgl, "sharp_h", wgpu::TextureFormat::Rgba32Float);
        let sharp_v_pl = pipe(&sharp_bgl, "sharp_v", LAYER_FMT);
        let linear = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        Ok(Gpu {
            name: format!("{} ({:?})", info.name, info.backend),
            max_tex: limits.max_texture_dimension_2d,
            instance,
            adapter,
            device,
            queue,
            layer_pl,
            comp_pl,
            down_pl,
            box_pl,
            mip_pl,
            mblur_pl,
            resample_pl,
            sharp_h_pl,
            sharp_v_pl,
            sharp_bgl,
            resi_pl,
            resi_bgl,
            layer_bgl,
            comp_bgl,
            tex_bgl,
            linear,
        })
    }

    fn texture(&self, w: u32, h: u32, mips: u32, fmt: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage,
            view_formats: &[],
        })
    }

    fn uniform(&self, bytes: &[u8]) -> wgpu::Buffer {
        let b = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes.len().max(16) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&b, 0, bytes);
        b
    }

    /// One full-target pass with `pl` over bind group `bg` into `view`.
    fn pass(&self, enc: &mut wgpu::CommandEncoder, pl: &wgpu::RenderPipeline, bg: &wgpu::BindGroup, view: &wgpu::TextureView) {
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(pl);
        rp.set_bind_group(0, bg, &[]);
        rp.draw(0..3, 0..1);
    }

    /// Upload an image with its mipmaps (shrunk on the processor first when a side passes
    /// the GPU's texture limit).
    fn make_tex(&self, img: &RgbaImage) -> Tex {
        let (bw, bh) = img.dimensions();
        let lim = self.max_tex;
        let shrunk;
        let (data, w, h, shrink): (&[u8], u32, u32, f64) = if bw > lim || bh > lim {
            let k = (lim as f64 / bw as f64).min(lim as f64 / bh as f64);
            let (w, h) = (((bw as f64 * k).floor() as u32).max(1), ((bh as f64 * k).floor() as u32).max(1));
            shrunk = crate::render::resize(img, w, h, 3);
            (shrunk.as_raw(), w, h, bw as f64 / w as f64)
        } else {
            (img.as_raw(), bw, bh, 1.0)
        };
        let tex = self.new_tex(w, h);
        // in bands of BAND rows: each write gets a staging copy of its size only
        // (one whole-image write kept a staging copy as large as the image)
        let mut y = 0;
        let mut prev = None;
        while y < h {
            let n = BAND.min(h - y);
            prev = Some(self.write_band(&tex, &data[(y * 4 * w) as usize..((y + n) * 4 * w) as usize], w, y, n, prev));
            y += n;
        }
        self.finish_tex(tex, w, h, shrink)
    }

    /// The largest texture side this GPU takes.
    pub fn max_tex(&self) -> u32 {
        self.max_tex
    }

    /// One band of rows into mip 0, waiting for it so its staging copy is freed.
    /// One band of rows. Waits for the band before (`prev`), not for this one: one band
    /// is in flight while the next decodes, and at most two staging copies exist. (Waiting
    /// for each band to finish, with every window's frames on the same queue, made a
    /// 6000x4000 JPEG take 170 ms in uploads against 51 ms of decoding.)
    pub fn write_band(&self, tex: &wgpu::Texture, rows: &[u8], w: u32, y: u32, n: u32, prev: Option<wgpu::SubmissionIndex>) -> wgpu::SubmissionIndex {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            rows,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(n) },
            wgpu::Extent3d { width: w, height: n, depth_or_array_layers: 1 },
        );
        let idx = self.queue.submit([]);
        if let Some(p) = prev {
            let _ = self.device.poll(wgpu::PollType::Wait { submission_index: Some(p), timeout: None });
        }
        idx
    }

    /// An empty texture for band uploads, with its mip chain.
    pub fn new_tex(&self, w: u32, h: u32) -> wgpu::Texture {
        let mips = 32 - w.max(h).leading_zeros();
        self.texture(
            w,
            h,
            mips,
            LAYER_FMT,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
    }

    /// After the last band: the mipmaps, and the texture as a shared image.
    pub fn finish_tex(&self, tex: wgpu::Texture, w: u32, h: u32, shrink: f64) -> Tex {
        let mips = tex.mip_level_count();
        // mipmaps: each level a bilinear half of the one above
        let mut enc = self.device.create_command_encoder(&Default::default());
        let u = self.uniform(&f32s(&[0.0; 4]));
        for lvl in 1..mips {
            let src = tex.create_view(&wgpu::TextureViewDescriptor { base_mip_level: lvl - 1, mip_level_count: Some(1), ..Default::default() });
            let dst = tex.create_view(&wgpu::TextureViewDescriptor { base_mip_level: lvl, mip_level_count: Some(1), ..Default::default() });
            let bg = self.tex_bind(&src, &u);
            self.pass(&mut enc, &self.mip_pl, &bg, &dst);
        }
        self.queue.submit([enc.finish()]);
        crate::trace(&format!("gpu upload {w}x{h} mips {mips}"));
        Tex { tex, w, h, shrink, bytes: (w as u64 * h as u64 * 4) * 4 / 3 }
    }

    fn tex_bind(&self, src: &wgpu::TextureView, u: &wgpu::Buffer) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.tex_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 2, resource: u.as_entire_binding() },
            ],
        })
    }
}

// ---- fast_image_resize's weights (precompute_coefficients and Normalizer16, v5.5) ----

fn fir_sinc(mut x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        x *= std::f64::consts::PI;
        x.sin() / x
    }
}

/// (filter, support) for a render::resize code.
fn fir_filter(code: u8) -> (fn(f64) -> f64, f64) {
    fn bilinear(x: f64) -> f64 {
        let x = x.abs();
        if x < 1.0 { 1.0 - x } else { 0.0 }
    }
    fn lanczos3(x: f64) -> f64 {
        if (-3.0..3.0).contains(&x) { fir_sinc(x) * fir_sinc(x / 3.0) } else { 0.0 }
    }
    fn lanczos4(x: f64) -> f64 {
        if (-4.0..4.0).contains(&x) { fir_sinc(x) * fir_sinc(x / 4.0) } else { 0.0 }
    }
    fn catmul(x: f64) -> f64 {
        const A: f64 = -0.5;
        let x = x.abs();
        if x < 1.0 {
            ((A + 2.) * x - (A + 3.)) * x * x + 1.
        } else if x < 2.0 {
            (((x - 5.) * x + 8.) * x - 4.) * A
        } else {
            0.0
        }
    }
    fn mitchell(x: f64) -> f64 {
        let x = x.abs();
        if x < 1.0 {
            (7. * x / 6. - 2.) * x * x + 16. / 18.
        } else if x < 2.0 {
            ((2. - 7. * x / 18.) * x - 10. / 3.) * x + 16. / 9.
        } else {
            0.0
        }
    }
    match code {
        1 => (bilinear, 1.0),
        4 => (catmul, 2.0),
        5 => (lanczos4, 4.0),
        6 => (mitchell, 2.0),
        _ => (lanczos3, 3.0),
    }
}

/// One axis's table for `resample_i`: per output pixel [start, size, offset], then the
/// i16 weights; and the precision. Nearest (code 0) is one weight of 1 at precision 0.
fn fir_table(in_size: u32, out_size: u32, code: u8, vertical: bool) -> (Vec<i32>, u32) {
    let n = out_size as usize;
    let mut table = vec![0i32; n * 3];
    let scale = in_size as f64 / out_size as f64;
    if code == 0 {
        // resample_nearest: columns at floor(scale / 2 + scale * x), at most the last;
        // rows by adding the step to a running y (iter_rows_with_step)
        let mut y = scale * 0.5;
        for x in 0..n {
            let i = if vertical {
                let r = (y as u32).min(in_size - 1);
                y += scale;
                r
            } else {
                ((scale * 0.5 + scale * x as f64) as u32).min(in_size - 1)
            };
            table[x * 3] = i as i32;
            table[x * 3 + 1] = 1;
            table[x * 3 + 2] = (n * 3 + x) as i32;
        }
        table.extend(std::iter::repeat_n(1, n));
        return (table, 0);
    }
    let (filter, support) = fir_filter(code);
    let filter_scale = scale.max(1.0);
    let radius = support * filter_scale;
    let window = radius.ceil() as usize * 2 + 1;
    let recip = 1.0 / filter_scale;
    let mut coeffs: Vec<f64> = Vec::with_capacity(window * n);
    let mut bounds = Vec::with_capacity(n);
    for out_x in 0..out_size {
        let in_center = (out_x as f64 + 0.5) * scale;
        let x_min = (in_center - radius).floor().max(0.) as u32;
        let x_max = (in_center + radius).ceil().min(in_size as f64) as u32;
        let cur = coeffs.len();
        let mut ww = 0.0;
        let center = in_center - 0.5;
        let (mut b0, mut b1) = (x_min, x_max);
        for x in x_min..x_max {
            let w = filter((x as f64 - center) * recip);
            if x == b0 && w == 0. {
                b0 += 1;
            } else {
                coeffs.push(w);
                ww += w;
            }
        }
        for &c in coeffs.iter().rev() {
            if b1 <= b0 || c != 0. {
                break;
            }
            b1 -= 1;
        }
        if ww != 0.0 {
            coeffs[cur..].iter_mut().for_each(|w| *w /= ww);
        }
        coeffs.resize(cur + window, 0.);
        bounds.push((b0, b1 - b0));
    }
    // Normalizer16: the most bits that keep twice the largest weight under 2^15
    let max_w = coeffs.iter().cloned().fold(0.0f64, |a, b| if b > a { b } else { a });
    let mut precision = 0u32;
    for cur in 0..22u32 {
        precision = cur;
        let next = (max_w * (1u32 << (precision + 1)) as f64).round() as i32;
        if next >= (1 << 15) {
            break;
        }
    }
    let unit = (1u32 << precision) as f64;
    for (x, &(start, size)) in bounds.iter().enumerate() {
        table[x * 3] = start as i32;
        table[x * 3 + 1] = size as i32;
        table[x * 3 + 2] = table.len() as i32;
        let w = &coeffs[x * window..x * window + size as usize];
        let base = table.len();
        table.extend(w.iter().map(|&v| (v * unit).round() as i16 as i32));
        // the offsets were written before the weights were appended
        table[x * 3 + 2] = base as i32;
    }
    (table, precision)
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_ne_bytes()).collect()
}

/// Decoders upload while this is set (the GPU path is on and the GPU works): the image
/// then lives on the GPU, and RAM keeps only a thumbnail. Off: decoders keep RAM copies.
pub static DECODE_TO_GPU: AtomicBool = AtomicBool::new(false);

/// One decoded image (one animation frame) on the GPU, shared by every window that shows
/// it. The texture is freed when the last holder lets go: the image cache and windows.
pub struct Tex {
    pub tex: wgpu::Texture,
    pub w: u32,
    pub h: u32,
    /// full image pixels per texel (above 1 when shrunk to fit the GPU's limit)
    pub shrink: f64,
    /// GPU memory it takes, with its mipmaps
    pub bytes: u64,
}

impl std::fmt::Debug for Tex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tex({}x{})", self.w, self.h)
    }
}

/// Upload a decoded image as a shared texture (decoder threads); None when the GPU is off,
/// lost or missing, and then the caller keeps its RAM copy.
pub fn upload_shared(img: &RgbaImage) -> Option<Arc<Tex>> {
    if !DECODE_TO_GPU.load(Ordering::Relaxed) || LOST.load(Ordering::Relaxed) {
        return None;
    }
    let g = get()?;
    let t = g.make_tex(img);
    // wait for the upload here, on the decoder's thread: wgpu frees its staging copy of
    // the image (as large as the image) only once the GPU is done and the device polled
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    (!LOST.load(Ordering::Relaxed)).then(|| Arc::new(t))
}

/// Read a shared texture back (full size, mip 0), for the clipboard.
pub fn read_tex(t: &Tex) -> Option<RgbaImage> {
    let g = get()?;
    let (w, h) = (t.w, t.h);
    let row = (w * 4).div_ceil(256) * 256;
    let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        t.tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    g.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    let data = buf.slice(..).get_mapped_range().ok()?;
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        out.extend_from_slice(&data[y * row as usize..y * row as usize + (w * 4) as usize]);
    }
    RgbaImage::from_raw(w, h, out)
}

/// Configure a surface and report whether it took. With many windows opening at once a
/// configure can fail ("Failed to wait for GPU to come idle before reconfiguring the
/// Surface"); the surface is then unconfigured, and asking it for a texture is fatal in
/// wgpu: the app aborted inside AppKit's start-up callback (2026-11-18, 100-window run).
fn configure(surface: &wgpu::Surface<'static>, device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> bool {
    let t0 = std::time::Instant::now();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    surface.configure(device, config);
    let r = pollster::block_on(scope.pop());
    crate::trace(&format!("gpu: configure took {} ms", t0.elapsed().as_millis()));
    match r {
        None => true,
        Some(e) => {
            crate::trace(&format!("gpu: surface configure failed, retrying next frame: {e}"));
            false
        }
    }
}

/// The image as uploaded: which base it is, its texture, and texels per base pixel.
struct ImageTex {
    key: u64,
    tex: wgpu::Texture,
    w: u32,
    h: u32,
    /// base pixels per texel (above 1 when the base was shrunk to fit the GPU's limit)
    shrink: f64,
}

/// What the app asks the GPU to draw under the interface.
pub struct ImageDraw<'a> {
    pub xf: Xform,
    /// The full image's width: `img` may be a thumbnail standing in for it.
    pub full_w: u32,
    /// The decoder's shared texture of this image, if it uploaded one (`img` is then only
    /// a thumbnail).
    pub tex: Option<Arc<Tex>>,
    /// Identifies the base image (it changes with rotation, flips, filters, frames).
    pub key: u64,
    pub img: &'a RgbaImage,
    /// Top-left in target px, and target px per base pixel.
    pub ox: f64,
    pub oy: f64,
    pub z: f64,
    /// Bilinear when enlarged (the processor's "smooth"); always filtered when shrunk.
    pub smooth: bool,
}

/// The full-quality shrink of the image to `w` x `h`, as `App::scale_with_preset`.
#[derive(Clone, Copy)]
pub struct Hq {
    pub key: u64,
    pub w: u32,
    pub h: u32,
    /// render::resize code (0 nearest, 1 bilinear, 2 Lanczos3, 4 Catmull-Rom, 5 Lanczos4, 6 Mitchell)
    pub kernel: u8,
    /// Unsharp mask: strength, Gaussian sigma (px), threshold (0-255).
    pub amount: f32,
    pub sigma: f32,
    pub threshold: u8,
    /// Where its top-left lands, target px.
    pub x: i64,
    pub y: i64,
    /// The view transform it is shown with (`w` x `h` is the shown, rotated size).
    pub xf: Xform,
}

/// Rotation (clockwise quarter turns), flips, Negative, Gamma: decode::transform's steps,
/// done by the GPU instead of a new copy of the image.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Xform {
    pub rot: u8,
    pub flip_h: bool,
    pub flip_v: bool,
    pub negative: bool,
    pub gamma: bool,
}

impl Xform {
    /// The turn an EXIF orientation (1-8) asks for, as decode::orient does it.
    pub fn exif(o: u8) -> Xform {
        let (rot, flip_h, flip_v) = match o {
            2 => (0, true, false),
            3 => (2, false, false),
            4 => (0, false, true),
            5 => (1, true, false),
            6 => (1, false, false),
            7 => (3, true, false),
            8 => (3, false, false),
            _ => (0, false, false),
        };
        Xform { rot, flip_h, flip_v, ..Default::default() }
    }

    /// The geometric part as a 2x2 matrix on pixel directions (y down): rotate clockwise,
    /// then flip h, then flip v.
    fn matrix(&self) -> [[i32; 2]; 2] {
        let mut m = [[1, 0], [0, 1]];
        let mul = |a: [[i32; 2]; 2], b: [[i32; 2]; 2]| {
            [[a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]], [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]]]
        };
        for _ in 0..self.rot % 4 {
            m = mul([[0, -1], [1, 0]], m);
        }
        if self.flip_h {
            m = mul([[-1, 0], [0, 1]], m);
        }
        if self.flip_v {
            m = mul([[1, 0], [0, -1]], m);
        }
        m
    }

    /// `self` applied after `first` (the photo's EXIF turn, then the view's), as one
    /// rotation and flip; Negative and Gamma from `self`.
    pub fn after(&self, first: &Xform) -> Xform {
        let a = self.matrix();
        let b = first.matrix();
        let m = [[a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]], [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]]];
        for rot in 0..4u8 {
            for flip_h in [false, true] {
                let c = Xform { rot, flip_h, flip_v: false, ..Default::default() };
                if c.matrix() == m {
                    return Xform { rot, flip_h, flip_v: false, negative: self.negative, gamma: self.gamma };
                }
            }
        }
        *self
    }

    fn swaps(&self) -> bool {
        self.rot % 2 == 1
    }
    fn toned(&self) -> bool {
        self.negative || self.gamma
    }
    /// decode::transform's table, as i32.
    fn lut(&self) -> Vec<i32> {
        (0..256)
            .map(|v| {
                let mut x = v as f64 / 255.0;
                if self.gamma {
                    x = x.powf(1.0 / 1.6);
                }
                if self.negative {
                    x = 1.0 - x;
                }
                (x * 255.0).round() as i32
            })
            .collect()
    }
}

pub struct Frame<'a> {
    pub hq: Option<Hq>,
    pub ui: &'a [u32],
    /// Only these interface rows can have changed since the last frame (the animation row
    /// redrawn alone); None: any row.
    pub rows: Option<(usize, usize)>,
    pub w: u32,
    pub h: u32,
    pub image: Option<ImageDraw<'a>>,
    /// Background, straight RGB, and its opacity (below 1 while the window glass shows).
    pub bg: u32,
    pub bg_alpha: f32,
    pub checker: bool,
    /// Blur radii in target px for classes 1-3 (bar, card, picker), each as the
    /// processor's three box passes: per-pass radius.
    pub radii: [f32; 3],
    /// A slide: the last settled layer, shifted this many px.
    pub slide_old: Option<f64>,
    /// Keep this frame's layer as the settled one (for the next slide).
    pub settle: bool,
    /// A slide's motion blur: the half-length of the smear along x, px (0: none).
    pub motion: f32,
}

pub struct GpuWin {
    /// The surface's last configure took; never ask an unconfigured surface for a texture.
    configured: bool,
    id: u64,
    gpu: Arc<Gpu>,
    /// None while the window is covered: a surface keeps 2 to 3 window-sized buffers
    /// for presenting (29 MB each at 3420x2148), which 100 covered windows held for
    /// nothing (measured: 12.6 GB with 100 windows). Made again on the next frame.
    surface: Option<wgpu::Surface<'static>>,
    window: Arc<Window>,
    /// The see-through mode the surface offers (None: only opaque).
    clear_mode: Option<wgpu::CompositeAlphaMode>,
    config: wgpu::SurfaceConfiguration,
    alpha_ok: bool,
    image: Option<ImageTex>,
    layer: Option<wgpu::Texture>,
    settled: Option<wgpu::Texture>,
    quarter: Option<(wgpu::Texture, wgpu::Texture, [wgpu::Texture; 3])>,
    ui: Option<wgpu::Texture>,
    hq: Option<(u64, wgpu::Texture)>,
    /// The layer smeared by a slide's motion blur (made when first needed).
    moved: Option<wgpu::Texture>,
    moved2: Option<wgpu::Texture>,
    /// The interface frame last uploaded, and the blur classes in use per row: only the
    /// rows that changed are uploaded and rescanned.
    prev_ui: Vec<u32>,
    row_classes: Vec<u8>,
}

impl Drop for GpuWin {
    fn drop(&mut self) {
        if let Ok(mut l) = ledger().lock() {
            l.wins.remove(&self.id);
            l.evict.remove(&self.id);
        }
        // release the swap chain now, not when the device next idles: on Windows its last
        // frame stayed on screen over the processor's drawing after a switch
        self.surface = None;
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

pub enum Fail {
    /// Skip this frame (hidden, timeout); the next one may work.
    Skip,
    /// The GPU is gone: use the processor for this window.
    Lost,
    /// macOS reports the window covered: nothing of it is on screen.
    Occluded,
}

impl GpuWin {
    pub fn new(window: Arc<Window>) -> Option<GpuWin> {
        let gpu = get()?;
        let size = window.inner_size();
        let surface = gpu.instance.create_surface(window.clone()).ok()?;
        #[cfg(target_os = "macos")]
        crate::trace(&format!("gpu: stale layers removed: {}", crate::mac::drop_stale_layers(&window)));
        let caps = surface.get_capabilities(&gpu.adapter);
        if !caps.formats.contains(&wgpu::TextureFormat::Bgra8Unorm) {
            crate::trace("gpu: surface has no Bgra8Unorm");
            return None;
        }
        // see-through for the window glass: Metal offers PostMultiplied (straight colour)
        let alpha_mode = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied]
            .into_iter()
            .find(|m| caps.alpha_modes.contains(m))
            .filter(|_| std::env::var_os("ARKIMAGE_GPU_OPAQUE").is_none())
            .unwrap_or(wgpu::CompositeAlphaMode::Opaque);
        let alpha_ok = alpha_mode != wgpu::CompositeAlphaMode::Opaque;
        let mut config = surface.get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))?;
        config.format = wgpu::TextureFormat::Bgra8Unorm;
        config.present_mode = wgpu::PresentMode::Fifo;
        // opaque until the glass shows: a see-through window does not count as covering
        // the windows under it, so with 100 windows none was ever reported covered
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        // two frames in flight: one made acquire wait for the last frame to reach the
        // screen, which halved the rate to 30 frames a second
        config.desired_maximum_frame_latency = 2;
        // present without waiting for the display's refresh: the main thread then never
        // blocks in acquire, and each pinch event is drawn as it arrives. Measured on a
        // synthetic pinch: event to present 3.7 ms median against 15.6 ms with Fifo, 3.6 ms
        // of work per frame against 16.1 ms. macOS composites windows, so nothing tears.
        // ARKIMAGE_GPU_FIFO=1 waits for the refresh again (for comparison).
        // Windows: Mailbox first. DirectX composes windows too, but Immediate there may tear
        // (a flip swap chain allowed to tear); Mailbox also never waits and never tears.
        let order: &[wgpu::PresentMode] = if cfg!(windows) { &[wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate] } else { &[wgpu::PresentMode::Immediate] };
        if std::env::var_os("ARKIMAGE_GPU_FIFO").is_none() {
            if let Some(m) = order.iter().find(|m| caps.present_modes.contains(m)) {
                config.present_mode = *m;
            }
        }
        crate::trace(&format!("gpu: present {:?} of {:?}", config.present_mode, caps.present_modes));
        let configured = configure(&surface, &gpu.device, &config);
        crate::trace(&format!("gpu surface: alpha {alpha_mode:?} of {:?}", caps.alpha_modes));
        Some(GpuWin { configured, id: NEXT_ID.fetch_add(1, Ordering::Relaxed), gpu, surface: Some(surface), window, clear_mode: alpha_ok.then_some(alpha_mode), config, alpha_ok, image: None, layer: None, settled: None, quarter: None, ui: None, hq: None, moved: None, moved2: None, prev_ui: Vec::new(), row_classes: Vec::new() })
    }

    /// See-through only while the window glass shows (one window open); opaque otherwise.
    pub fn set_see_through(&mut self, on: bool) {
        let want = match (on, self.clear_mode) {
            (true, Some(m)) => m,
            _ => wgpu::CompositeAlphaMode::Opaque,
        };
        if self.config.alpha_mode != want {
            self.config.alpha_mode = want;
            if let Some(s) = &self.surface {
                self.configured = configure(s, &self.gpu.device, &self.config);
            }
        }
    }

    /// Covered: drop the surface and its presenting buffers (the textures go in
    /// `release`). The next frame makes a new one.
    pub fn park(&mut self) {
        if self.surface.take().is_some() {
            self.configured = false;
            crate::trace("gpu: surface dropped (window covered)");
        }
    }

    /// The surface back, configured, before a frame.
    fn unpark(&mut self) -> bool {
        if self.surface.is_some() {
            return true;
        }
        let Ok(s) = self.gpu.instance.create_surface(self.window.clone()) else { return false };
        #[cfg(target_os = "macos")]
        crate::trace(&format!("gpu: stale layers removed: {}", crate::mac::drop_stale_layers(&self.window)));
        self.configured = configure(&s, &self.gpu.device, &self.config);
        self.surface = Some(s);
        crate::trace("gpu: surface made again");
        true
    }

    pub fn adapter_name(&self) -> &str {
        &self.gpu.name
    }

    /// Upload the base image once per key, with mipmaps; shrunk on the processor first
    /// when a side passes the GPU's texture limit.
    /// A texture the decoder already uploaded (shared with other windows): no copy here.
    fn use_shared(&mut self, key: u64, t: &Tex) {
        if self.image.as_ref().is_some_and(|i| i.key == key) {
            return;
        }
        self.image = Some(ImageTex { key, tex: t.tex.clone(), w: t.w, h: t.h, shrink: t.shrink });
    }

    fn upload(&mut self, key: u64, img: &RgbaImage, full_w: u32) {
        if self.image.as_ref().is_some_and(|t| t.key == key) {
            return;
        }
        let g = self.gpu.clone();
        let t = g.make_tex(img);
        let (w, h) = (t.w, t.h);
        // a thumbnail stands in for a full image of `full_w` px: scale it up to that
        let shrink = if full_w > w { full_w as f64 / w as f64 } else { t.shrink };
        self.image = Some(ImageTex { key, tex: t.tex, w, h, shrink });
    }

    /// The full-quality shrink, made on the GPU once per key.
    fn make_hq(&mut self, q: &Hq) {
        if self.hq.as_ref().is_some_and(|(k, _)| *k == q.key) {
            return;
        }
        let Some(t) = &self.image else { return };
        if t.shrink > 1.01 && t.w < 1024 && t.h < 1024 {
            // a thumbnail standing in: keep the last full-quality copy rather than a blurry one
            return;
        }
        let g = self.gpu.clone();
        let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        let support = match q.kernel {
            1 => 1.0,
            4 | 6 => 2.0,
            5 => 4.0,
            _ => 3.0,
        };
        let _ = support;
        let src = t.tex.create_view(&wgpu::TextureViewDescriptor { base_mip_level: 0, mip_level_count: Some(1), ..Default::default() });
        let mut enc = g.device.create_command_encoder(&Default::default());
        // the library's order for 8-bit images: vertical first, then horizontal; an axis
        // that keeps its size is skipped, as there
        let axis = |enc: &mut wgpu::CommandEncoder, from: &wgpu::TextureView, horiz: bool, in_size: u32, out_size: u32, w: u32, h: u32, lut: Option<Vec<i32>>| {
            let (mut table, precision) = fir_table(in_size, out_size, q.kernel, !horiz);
            let lut_at = match lut {
                Some(t) => {
                    let at = table.len() as u32;
                    table.extend(t);
                    at
                }
                None => 0,
            };
            let bytes: Vec<u8> = table.iter().flat_map(|v| v.to_ne_bytes()).collect();
            let sb = g.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            g.queue.write_buffer(&sb, 0, &bytes);
            let u = g.uniform(&[horiz as u32, precision, lut_at, 0].iter().flat_map(|v| v.to_ne_bytes()).collect::<Vec<u8>>());
            let dst = g.texture(w, h, 1, LAYER_FMT, usage);
            let dv = dst.create_view(&Default::default());
            let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &g.resi_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(from) },
                    wgpu::BindGroupEntry { binding: 1, resource: sb.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: u.as_entire_binding() },
                ],
            });
            g.pass(enc, &g.resi_pl, &bg, &dv);
            (dst, dv)
        };
        // the texture is unrotated: resize it to the unrotated size of the shown copy;
        // the library runs the vertical pass first on the rotated image, which is the
        // horizontal one here when a quarter turn swaps the axes
        let (uw, uh) = if q.xf.swaps() { (q.h, q.w) } else { (q.w, q.h) };
        let lut = q.xf.toned().then(|| q.xf.lut());
        let first_horiz = q.xf.swaps();
        let (n1, n2) = if first_horiz { ((t.w, uw), (t.h, uh)) } else { ((t.h, uh), (t.w, uw)) };
        let (mut cur_v, mut cur_w, mut cur_h) = (None::<(wgpu::Texture, wgpu::TextureView)>, t.w, t.h);
        let mut lut_left = lut;
        for (k, (in_size, out_size)) in [n1, n2].into_iter().enumerate() {
            let horiz = (k == 0) == first_horiz;
            // an axis that keeps its size is skipped, unless the tone table must still run
            if in_size == out_size && lut_left.is_none() {
                continue;
            }
            let (w, h) = if horiz { (out_size, cur_h) } else { (cur_w, out_size) };
            let from = cur_v.as_ref().map(|c| &c.1).unwrap_or(&src);
            let next = axis(&mut enc, from, horiz, in_size, out_size, w, h, lut_left.take());
            cur_w = w;
            cur_h = h;
            cur_v = Some(next);
        }
        let (out, out_v) = cur_v.unwrap_or_else(|| {
            let d = g.texture(uw, uh, 1, LAYER_FMT, usage | wgpu::TextureUsages::COPY_DST);
            enc.copy_texture_to_texture(t.tex.as_image_copy(), d.as_image_copy(), wgpu::Extent3d { width: uw, height: uh, depth_or_array_layers: 1 });
            let v = d.create_view(&Default::default());
            (d, v)
        });
        let fin = if q.amount > 0.0 {
            // the processor's kernel: radius ceil(2.5 sigma), normalised Gaussian
            let r = (q.sigma * 2.5).ceil().max(1.0) as usize;
            let mut kv: Vec<f32> = (0..=2 * r).map(|i| (-(i as f32 - r as f32).powi(2) / (2.0 * q.sigma * q.sigma)).exp()).collect();
            let sum: f32 = kv.iter().sum();
            kv.iter_mut().for_each(|v| *v /= sum);
            kv.resize(20, 0.0);
            let mut u = vec![q.amount, q.threshold as f32, r.min(9) as f32, 0.0];
            u.extend_from_slice(&kv);
            let su = g.uniform(&f32s(&u));
            let tmp = g.texture(uw, uh, 1, wgpu::TextureFormat::Rgba32Float, usage);
            let tmp_v = tmp.create_view(&Default::default());
            g.pass(&mut enc, &g.sharp_h_pl, &g.tex_bind(&out_v, &su), &tmp_v);
            let fin = g.texture(uw, uh, 1, LAYER_FMT, usage);
            let fin_v = fin.create_view(&Default::default());
            let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &g.sharp_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&out_v) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tmp_v) },
                    wgpu::BindGroupEntry { binding: 2, resource: su.as_entire_binding() },
                ],
            });
            g.pass(&mut enc, &g.sharp_v_pl, &bg, &fin_v);
            fin
        } else {
            out
        };
        g.queue.submit([enc.finish()]);
        crate::trace(&format!("gpu hq {}x{} kernel {} sharpen {}", q.w, q.h, q.kernel, q.amount));
        self.hq = Some((q.key, fin));
    }

    /// Called every loop pass: give the textures back if the budget asked for them, so an
    /// idle window frees its memory without drawing. True when it did.
    pub fn poll_budget(&mut self) -> bool {
        let asked = ledger().lock().map(|mut l| l.evict.remove(&self.id)).unwrap_or(false);
        if asked {
            self.release(true);
            crate::trace("gpu: full texture given back (budget); the screen-sized copy stays");
        }
        asked
    }

    /// Each frame: this window uses `bytes` of full-size image texture. Past `budget` for
    /// all windows together, the least recently used windows that are not focused are
    /// asked to keep only their screen-sized copy.
    pub fn note_use(&mut self, bytes: u64, focused: bool, budget: u64) {
        let Ok(mut l) = ledger().lock() else { return };
        let now = std::time::Instant::now();
        // the focused window counts as the most recent always
        let t = if focused { now + std::time::Duration::from_secs(3600) } else { now };
        l.wins.insert(self.id, (t, bytes));
        let mut total: u64 = l.wins.values().map(|v| v.1).sum();
        if total <= budget {
            return;
        }
        let mut order: Vec<(std::time::Instant, u64, u64)> = l.wins.iter().filter(|(k, v)| **k != self.id && v.1 > 0).map(|(k, v)| (v.0, *k, v.1)).collect();
        order.sort();
        for (_, k, b) in order {
            if total <= budget {
                break;
            }
            l.evict.insert(k);
            total -= b;
        }
    }

    /// A covered or minimized window gives its GPU memory back; the next frame after it is
    /// shown again uploads the image anew. (Users keep 100 windows open: one 6016 px image
    /// with its mipmaps is about 190 MB.)
    pub fn release(&mut self, keep_hq: bool) {
        if let Ok(mut l) = ledger().lock() {
            if let Some(e) = l.wins.get_mut(&self.id) {
                e.1 = 0;
            }
        }
        let had = self.image.is_some();
        self.image = None;
        if !keep_hq {
            self.hq = None;
        }
        self.layer = None;
        self.settled = None;
        self.quarter = None;
        self.moved = None;
        self.moved2 = None;
        self.ui = None;
        // free the frame copies' memory, not only their contents
        self.prev_ui = Vec::new();
        self.row_classes = Vec::new();
        if had {
            crate::trace("gpu: textures released (window covered)");
        }
    }

    /// Test hook: the full-quality shrink of `img` as the GPU makes it, read back (RGBA).
    pub fn hq_readback(&mut self, key: u64, img: &RgbaImage, q: &Hq) -> Vec<u8> {
        self.upload(key, img, img.width());
        self.hq = None;
        self.make_hq(q);
        let g = self.gpu.clone();
        let Some((_, t)) = &self.hq else { return Vec::new() };
        let (qw, qh) = if q.xf.swaps() { (q.h, q.w) } else { (q.w, q.h) };
        let q = &Hq { w: qw, h: qh, ..*q };
        let row = (q.w * 4).div_ceil(256) * 256;
        let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * q.h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = g.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            t.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(q.h) } },
            wgpu::Extent3d { width: q.w, height: q.h, depth_or_array_layers: 1 },
        );
        g.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
        let Ok(data) = buf.slice(..).get_mapped_range() else { return Vec::new() };
        let mut out = Vec::with_capacity((q.w * q.h * 4) as usize);
        for y in 0..q.h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + (q.w * 4) as usize]);
        }
        out
    }

    fn sized(&mut self, w: u32, h: u32) {
        let g = self.gpu.clone();
        let full = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
        if self.layer.as_ref().map(|t| (t.width(), t.height())) != Some((w, h)) {
            self.layer = Some(g.texture(w, h, 1, LAYER_FMT, full));
            self.settled = None;
            let (qw, qh) = (w.div_ceil(QUARTER), h.div_ceil(QUARTER));
            let q = || g.texture(qw, qh, 1, LAYER_FMT, full);
            self.quarter = Some((q(), q(), [q(), q(), q()]));
            self.ui = Some(g.texture(w, h, 1, wgpu::TextureFormat::R32Uint, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST));
            // a new, empty texture: upload every row next
            self.prev_ui.clear();
        }
        if self.config.width != w || self.config.height != h || !self.configured {
            self.config.width = w;
            self.config.height = h;
            if let Some(s) = &self.surface {
                self.configured = configure(s, &g.device, &self.config);
            }
        }
    }

    pub fn render(&mut self, f: &Frame) -> Result<(), Fail> {
        // test hook: ARKIMAGE_GPU_LOSE_MS=<n> loses the GPU n ms after it was made
        if let Some(ms) = std::env::var("ARKIMAGE_GPU_LOSE_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
            static T0: OnceLock<std::time::Instant> = OnceLock::new();
            if T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 >= ms {
                crate::trace("gpu lost: test hook");
                LOST.store(true, Ordering::Relaxed);
            }
        }
        if LOST.load(Ordering::Relaxed) {
            return Err(Fail::Lost);
        }
        if !self.unpark() {
            return Err(Fail::Skip);
        }
        let g = self.gpu.clone();
        // over the budget: this window was asked to give its image texture back
        let asked = ledger().lock().map(|mut l| {
            let asked = l.evict.remove(&self.id);
            if let Some(e) = l.wins.get_mut(&self.id) {
                e.0 = std::time::Instant::now();
                if asked {
                    e.1 = 0;
                }
            }
            asked
        });
        if asked.unwrap_or(false) {
            self.release(true);
            crate::trace("gpu: textures given back (budget)");
        }
        self.sized(f.w, f.h);
        if let Some(im) = &f.image {
            match &im.tex {
                Some(t) => self.use_shared(im.key, t),
                None => self.upload(im.key, im.img, im.full_w),
            }
        }
        if let Some(q) = &f.hq {
            self.make_hq(q);
        }
        let hq = f.hq.as_ref().and_then(|q| self.hq.as_ref().filter(|(k, _)| *k == q.key).map(|(_, t)| (q, t)));
        if !self.configured {
            // retried at the top of the next frame
            return Err(Fail::Skip);
        }
        crate::trace("acquire");
        let Some(surface) = &self.surface else { return Err(Fail::Skip) };
        let st = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated => {
                if let Some(s) = &self.surface {
                    self.configured = configure(s, &g.device, &self.config);
                }
                return Err(Fail::Skip);
            }
            wgpu::CurrentSurfaceTexture::Lost => return Err(Fail::Lost),
            wgpu::CurrentSurfaceTexture::Occluded => return Err(Fail::Occluded),
            other => {
                crate::trace(&format!("gpu frame skipped: {}", match other {
                    wgpu::CurrentSurfaceTexture::Timeout => "timeout",
                    wgpu::CurrentSurfaceTexture::Occluded => "occluded",
                    wgpu::CurrentSurfaceTexture::Validation => "validation",
                    _ => "other",
                }));
                return Err(Fail::Skip);
            }
        };
        crate::trace("acquired");
        let layer = self.layer.as_ref().unwrap();
        let layer_v = layer.create_view(&Default::default());
        // the interface frame: only the rows that changed since the last upload
        let ui = self.ui.as_ref().unwrap();
        let (w, h) = (f.w as usize, f.h as usize);
        let fresh = self.prev_ui.len() != f.ui.len() || self.row_classes.len() != h;
        if fresh {
            self.prev_ui = vec![0; f.ui.len()];
            self.row_classes = vec![0; h];
        }
        let (mut y0, mut y1) = (h, 0);
        let scan = match f.rows {
            Some((a, b)) if !fresh => a.min(h)..b.min(h),
            _ => 0..h,
        };
        for y in scan {
            let (a, b) = (&f.ui[y * w..(y + 1) * w], &mut self.prev_ui[y * w..(y + 1) * w]);
            if fresh || a != b {
                b.copy_from_slice(a);
                let mut m = 0u8;
                for &p in a {
                    if crate::render::is_hole(p) {
                        let k = crate::render::hole_class(p);
                        if k != 0 {
                            m |= 1 << (k - 1);
                        }
                    }
                }
                self.row_classes[y] = m;
                y0 = y0.min(y);
                y1 = y + 1;
            }
        }
        let classes = self.row_classes.iter().fold(0u8, |a, &b| a | b);
        crate::trace(&format!("ui rows {}", y1.saturating_sub(y0)));
        if y1 > y0 {
            let rows = &f.ui[y0 * w..y1 * w];
            let bytes = unsafe { std::slice::from_raw_parts(rows.as_ptr() as *const u8, rows.len() * 4) };
            g.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: ui, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: y0 as u32, z: 0 }, aspect: wgpu::TextureAspect::All },
                bytes,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * f.w), rows_per_image: Some((y1 - y0) as u32) },
                wgpu::Extent3d { width: f.w, height: (y1 - y0) as u32, depth_or_array_layers: 1 },
            );
        }
        crate::trace("ui uploaded");
        let mut enc = g.device.create_command_encoder(&Default::default());
        // 1. image layer
        let ba = if self.alpha_ok { f.bg_alpha } else { 1.0 };
        let c = |s: u32| (f.bg >> s & 0xFF) as f32 / 255.0 * ba;
        let (mut has, mut org, mut scale, mut tsz, mut lod, mut smooth) = (0.0, [0.0; 2], [1.0; 2], [1.0; 2], 0.0, 0.0);
        let mut is_hq = 0.0;
        let xf = f.image.as_ref().map(|i| i.xf).unwrap_or_default();
        if let (Some(_), Some((q, _))) = (&f.image, hq) {
            // the shrunk copy, one texel per target pixel
            has = 1.0;
            is_hq = 1.0;
            org = [q.x as f32, q.y as f32];
            tsz = [q.w as f32, q.h as f32];
        } else if let (Some(im), Some(t)) = (&f.image, &self.image) {
            has = 1.0;
            org = [im.ox as f32, im.oy as f32];
            let s = (im.z * t.shrink) as f32;
            scale = [s, s];
            // the shown size: the texture's, turned
            tsz = if xf.swaps() { [t.h as f32, t.w as f32] } else { [t.w as f32, t.h as f32] };
            lod = if s < 1.0 { (1.0 / s).log2() } else { 0.0 };
            smooth = if im.smooth { 1.0 } else { 0.0 };
        }
        let slide = f.slide_old.filter(|_| self.settled.is_some());
        let lu = g.uniform(&f32s(&[
            f.w as f32,
            f.h as f32,
            org[0],
            org[1],
            scale[0],
            scale[1],
            tsz[0],
            tsz[1],
            c(16),
            c(8),
            c(0),
            ba,
            has,
            smooth,
            if f.checker { 1.0 } else { 0.0 },
            lod,
            if slide.is_some() { 1.0 } else { 0.0 },
            slide.unwrap_or(0.0) as f32,
            0.0,
            0.0,
            xf.rot as f32,
            xf.flip_h as u8 as f32,
            xf.flip_v as u8 as f32,
            is_hq,
            xf.negative as u8 as f32,
            xf.gamma as u8 as f32,
            0.0,
            0.0,
        ]));
        let dummy;
        let img_v = match (hq, &self.image) {
            (Some((_, t)), _) => t.create_view(&Default::default()),
            (None, Some(t)) => t.tex.create_view(&Default::default()),
            (None, None) => {
                dummy = g.texture(1, 1, 1, LAYER_FMT, wgpu::TextureUsages::TEXTURE_BINDING);
                dummy.create_view(&Default::default())
            }
        };
        let old_tex;
        let old_v = match (&self.settled, slide) {
            (Some(t), Some(_)) => t.create_view(&Default::default()),
            _ => {
                old_tex = g.texture(1, 1, 1, LAYER_FMT, wgpu::TextureUsages::TEXTURE_BINDING);
                old_tex.create_view(&Default::default())
            }
        };
        let lbg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &g.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&img_v) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&g.linear) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&old_v) },
                wgpu::BindGroupEntry { binding: 3, resource: lu.as_entire_binding() },
            ],
        });
        g.pass(&mut enc, &g.layer_pl, &lbg, &layer_v);
        // a slide: smear the layer along its motion; everything after uses the smeared one
        let moved_v;
        let layer_v = if f.motion > 1.0 {
            if self.moved.as_ref().map(|t| (t.width(), t.height())) != Some((f.w, f.h)) {
                self.moved = Some(g.texture(f.w, f.h, 1, LAYER_FMT, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT));
            }
            if self.moved2.as_ref().map(|t| (t.width(), t.height())) != Some((f.w, f.h)) {
                self.moved2 = Some(g.texture(f.w, f.h, 1, LAYER_FMT, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT));
            }
            let mid_v = self.moved2.as_ref().unwrap().create_view(&Default::default());
            moved_v = self.moved.as_ref().unwrap().create_view(&Default::default());
            // the 24 taps of the first pass sit 2r/24 apart, which showed as stripes at
            // r = 200 px; a second pass over one tap spacing joins them up
            let u = g.uniform(&f32s(&[1.0, 0.0, f.motion, 0.0]));
            g.pass(&mut enc, &g.mblur_pl, &g.tex_bind(&layer_v, &u), &mid_v);
            let u2 = g.uniform(&f32s(&[1.0, 0.0, f.motion / 12.0, 0.0]));
            g.pass(&mut enc, &g.mblur_pl, &g.tex_bind(&mid_v, &u2), &moved_v);
            moved_v
        } else {
            layer_v
        };
        // 2. blurs at quarter size, for the classes in use
        let (qa, qb, outs) = self.quarter.as_ref().unwrap();
        let (qa_v, qb_v) = (qa.create_view(&Default::default()), qb.create_view(&Default::default()));
        let outs_v: Vec<wgpu::TextureView> = outs.iter().map(|t| t.create_view(&Default::default())).collect();
        if classes != 0 {
            let u0 = g.uniform(&f32s(&[0.0; 4]));
            g.pass(&mut enc, &g.down_pl, &g.tex_bind(&layer_v, &u0), &qa_v);
            for k in 0..3 {
                if classes & (1 << k) == 0 {
                    continue;
                }
                let r = (f.radii[k] / QUARTER as f32).round().max(1.0);
                let ux = g.uniform(&f32s(&[1.0, 0.0, r, 0.0]));
                let uy = g.uniform(&f32s(&[0.0, 1.0, r, 0.0]));
                // three box passes each way; the last lands in this class's texture
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&qa_v, &ux), &qb_v);
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&qb_v, &uy), &outs_v[k]);
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&outs_v[k], &ux), &qb_v);
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&qb_v, &uy), &outs_v[k]);
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&outs_v[k], &ux), &qb_v);
                g.pass(&mut enc, &g.box_pl, &g.tex_bind(&qb_v, &uy), &outs_v[k]);
            }
        }
        // 3. the interface over the layer, into the window
        let ui_v = ui.create_view(&Default::default());
        // Core Animation composites a non-opaque CAMetalLayer as premultiplied, whatever
        // wgpu calls the mode (measured: straight colour came out 54 where 38 was right)
        let post = self.config.alpha_mode == wgpu::CompositeAlphaMode::PostMultiplied && !cfg!(target_os = "macos");
        let cu = g.uniform(&f32s(&[if post { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]));
        let out_v = st.texture.create_view(&Default::default());
        let cbg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &g.comp_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&ui_v) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&layer_v) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&outs_v[0]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&outs_v[1]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&outs_v[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&g.linear) },
                wgpu::BindGroupEntry { binding: 6, resource: cu.as_entire_binding() },
            ],
        });
        g.pass(&mut enc, &g.comp_pl, &cbg, &out_v);
        g.queue.submit([enc.finish()]);
        // keep this frame's layer as the settled one for the next slide: the two textures
        // swap (each frame draws the whole layer anew), where a copy of the full window
        // texture ran on every animation frame
        if f.settle && slide.is_none() {
            let full = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
            let next = self.settled.take().filter(|t| (t.width(), t.height()) == (f.w, f.h)).unwrap_or_else(|| g.texture(f.w, f.h, 1, LAYER_FMT, full));
            self.settled = self.layer.replace(next);
        }
        crate::trace("submitted");
        // reclaim finished uploads' staging memory (does not wait)
        let _ = g.device.poll(wgpu::PollType::Poll);
        if std::env::var_os("ARKIMAGE_TEST_RELIEF").is_some() {
            crate::decode::release_freed_memory();
        }
        g.queue.present(st);
        if LOST.load(Ordering::Relaxed) {
            return Err(Fail::Lost);
        }
        Ok(())
    }
}

/// True on a machine with a system battery (a laptop or tablet). Windows:
/// GetSystemPowerStatus, battery flag 128 = no system battery. macOS: every Mac with two
/// GPUs is a MacBook Pro, except the iMac and Mac Pro; it keeps LowPower, as before.
fn on_battery_machine() -> bool {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct PowerStatus {
            ac: u8,
            battery_flag: u8,
            percent: u8,
            saver: u8,
            life: u32,
            full: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetSystemPowerStatus(s: *mut PowerStatus) -> i32;
        }
        let mut s = PowerStatus { ac: 0, battery_flag: 0, percent: 0, saver: 0, life: 0, full: 0 };
        if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
            return false;
        }
        has_battery(s.battery_flag)
    }
    #[cfg(not(windows))]
    true
}

/// GetSystemPowerStatus' battery flag: 128 means no system battery, 255 unknown (both
/// count as a desktop); anything else (high, low, critical, charging) is a battery.
#[cfg_attr(not(windows), allow(dead_code))]
fn has_battery(flag: u8) -> bool {
    !matches!(flag, 128 | 255)
}

#[cfg(test)]
mod battery_tests {
    #[test]
    fn desktop_takes_the_card_laptop_the_chip() {
        assert!(!super::has_battery(128)); // desktop
        assert!(!super::has_battery(255)); // unknown
        for f in [0u8, 1, 2, 4, 8, 9] {
            assert!(super::has_battery(f), "flag {f}");
        }
    }
}
