//! Text drawing. The interface fonts are bundled: Geist for body
//! text, Figtree for titles, Geist Mono for numbers. System fonts load on the first glyph
//! the bundled fonts lack.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

use crate::render::{mix, Canvas};

/// A bundled typeface and weight.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    /// Geist 400: body text, values.
    Regular = 0,
    /// Geist 500: card titles, buttons, labels.
    Medium = 1,
    /// Figtree 600: titles and headings.
    Title = 2,
    /// Geist Mono 400: numbers, sizes, dates.
    Mono = 3,
}

const BUNDLED: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Figtree-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
];

#[cfg(windows)]
const FALLBACK: &[&str] = &[
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "C:\\Windows\\Fonts\\malgun.ttf",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\YuGothM.ttc",
    "C:\\Windows\\Fonts\\seguisym.ttf",
];
#[cfg(not(windows))]
const FALLBACK: &[&str] = &[
    "/System/Library/Fonts/Helvetica.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
];

fn load(path: &str) -> Option<Font> {
    let bytes = std::fs::read(path).ok()?;
    Font::from_bytes(bytes, FontSettings::default()).ok()
}

pub struct Text {
    /// The four bundled faces (indexed by Face), then system fallbacks once needed.
    fonts: Vec<Font>,
    fallback_loaded: bool,
    /// The face draw() and width() use; set with face().
    cur: Face,
    cache: HashMap<(Face, char, u32), (Metrics, Vec<u8>)>,
}

impl Text {
    pub fn load() -> Option<Self> {
        let fonts: Option<Vec<Font>> = BUNDLED.iter().map(|b| Font::from_bytes(*b, FontSettings::default()).ok()).collect();
        Some(Self { fonts: fonts?, fallback_loaded: false, cur: Face::Regular, cache: HashMap::new() })
    }

    /// Choose the face for the following draw() and width() calls; returns the previous one.
    pub fn face(&mut self, f: Face) -> Face {
        std::mem::replace(&mut self.cur, f)
    }

    fn font_for(&mut self, face: Face, ch: char) -> usize {
        let fi = face as usize;
        if self.fonts[fi].lookup_glyph_index(ch) != 0 {
            return fi;
        }
        let from = BUNDLED.len();
        if !self.fallback_loaded {
            self.fallback_loaded = true;
            self.fonts.extend(FALLBACK.iter().filter_map(|p| load(p)));
        }
        (from..self.fonts.len()).find(|&i| self.fonts[i].lookup_glyph_index(ch) != 0).unwrap_or(fi)
    }

    fn glyph(&mut self, ch: char, px: f32) -> &(Metrics, Vec<u8>) {
        let key = (self.cur, ch, (px * 10.0) as u32);
        if !self.cache.contains_key(&key) {
            let fi = self.font_for(self.cur, ch);
            let g = self.fonts[fi].rasterize(ch, px);
            self.cache.insert(key, g);
        }
        &self.cache[&key]
    }

    pub fn line_height(&self, px: f32) -> f32 {
        self.fonts[self.cur as usize].horizontal_line_metrics(px).map(|m| m.new_line_size).unwrap_or(px * 1.3)
    }

    pub fn ascent(&self, px: f32) -> f32 {
        self.fonts[self.cur as usize].horizontal_line_metrics(px).map(|m| m.ascent).unwrap_or(px)
    }

    pub fn width(&mut self, s: &str, px: f32) -> f32 {
        s.chars().map(|c| self.glyph(c, px).0.advance_width).sum()
    }

    /// Draw `s` with its baseline at `base`.
    pub fn draw_base(&mut self, c: &mut Canvas, x: f32, base: f32, s: &str, px: f32, color: u32) {
        let base = base.round() as i32;
        let mut pen = x;
        for ch in s.chars() {
            let (m, bmp) = self.glyph(ch, px);
            let m = *m;
            let gx = pen.round() as i32 + m.xmin;
            let gy = base - m.height as i32 - m.ymin;
            for row in 0..m.height {
                let yy = gy + row as i32;
                if yy < 0 || yy >= c.h as i32 {
                    continue;
                }
                for col in 0..m.width {
                    let a = bmp[row * m.width + col] as u32;
                    let xx = gx + col as i32;
                    if a == 0 || xx < 0 || xx >= c.w as i32 {
                        continue;
                    }
                    let i = yy as usize * c.w + xx as usize;
                    c.buf[i] = mix(c.buf[i], color, a);
                }
            }
            pen += m.advance_width;
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, x: f32, y_top: f32, s: &str, px: f32, color: u32) {
        let base = y_top + self.ascent(px);
        self.draw_base(c, x, base, s, px, color);
    }

    /// A dark box with lines of white text; returns its size.
    pub fn panel(&mut self, c: &mut Canvas, x: f32, y: f32, lines: &[String], px: f32) -> (f32, f32) {
        let (w, h) = self.panel_size(lines, px);
        let lh = self.line_height(px);
        let pad = (px * 0.6).round();
        c.rect_alpha(x as i32, y as i32, w as i32, h as i32, 0x000000, 175);
        for (i, l) in lines.iter().enumerate() {
            self.draw(c, x + pad, y + pad + lh * i as f32, l, px, 0xF0F0F0);
        }
        (w, h)
    }

    pub fn panel_size(&mut self, lines: &[String], px: f32) -> (f32, f32) {
        let lh = self.line_height(px);
        let pad = (px * 0.6).round();
        let w = lines.iter().map(|l| self.width(l, px)).fold(0.0, f32::max) + pad * 2.0;
        (w.ceil(), (lh * lines.len() as f32 + pad * 2.0).ceil())
    }
}
