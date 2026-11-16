//! PNG rasterizer for drawing sheets: anti-aliased (2× supersampled) lines
//! and real glyph outlines from the bundled Ubuntu font.

use crate::export::pen_style;
use crate::{Anchor, Sheet};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

const SS: f64 = 2.0;

struct Canvas {
    w: usize,
    h: usize,
    /// Ink coverage 0..1 per supersampled pixel.
    ink: Vec<f32>,
}

impl Canvas {
    fn dot(&mut self, x: f64, y: f64, r: f64) {
        let (x0, x1) = ((x - r).floor().max(0.0) as usize, ((x + r).ceil() as usize).min(self.w.saturating_sub(1)));
        let (y0, y1) = ((y - r).floor().max(0.0) as usize, ((y + r).ceil() as usize).min(self.h.saturating_sub(1)));
        if x + r < 0.0 || y + r < 0.0 {
            return;
        }
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 + 0.5 - x).powi(2) + (py as f64 + 0.5 - y).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0) as f32;
                let i = py * self.w + px;
                if a > self.ink[i] {
                    self.ink[i] = a;
                }
            }
        }
    }

    fn polyline(&mut self, pts: &[[f64; 2]], r: f64, dash: Option<[f64; 2]>) {
        let mut along = 0.0;
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let n = (len / 0.5).ceil().max(1.0) as usize;
            for i in 0..=n {
                let t = i as f64 / n as f64;
                let s = along + len * t;
                if let Some([on, off]) = dash {
                    if s.rem_euclid(on + off) > on {
                        continue;
                    }
                }
                self.dot(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, r);
            }
            along += len;
        }
    }

    /// Non-zero winding fill of closed contours.
    fn fill(&mut self, contours: &[Vec<[f64; 2]>]) {
        let mut ylo = f64::INFINITY;
        let mut yhi = f64::NEG_INFINITY;
        for c in contours {
            for p in c {
                ylo = ylo.min(p[1]);
                yhi = yhi.max(p[1]);
            }
        }
        if !ylo.is_finite() {
            return;
        }
        let y0 = ylo.floor().max(0.0) as usize;
        let y1 = (yhi.ceil() as usize).min(self.h);
        for py in y0..y1 {
            let y = py as f64 + 0.5;
            let mut xs: Vec<(f64, i32)> = vec![];
            for c in contours {
                for i in 0..c.len() {
                    let (a, b) = (c[i], c[(i + 1) % c.len()]);
                    if (a[1] <= y) != (b[1] <= y) {
                        let x = a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                        xs.push((x, if b[1] > a[1] { 1 } else { -1 }));
                    }
                }
            }
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0;
            for k in 0..xs.len() {
                let prev = wind;
                wind += xs[k].1;
                if prev == 0 && wind != 0 {
                    // span start; find its end
                    let mut w2 = wind;
                    let mut j = k + 1;
                    while j < xs.len() && w2 != 0 {
                        w2 += xs[j].1;
                        if w2 == 0 {
                            break;
                        }
                        j += 1;
                    }
                    let (xa, xb) = (xs[k].0, xs.get(j).map_or(xs[k].0, |e| e.0));
                    let (pa, pb) = (xa.max(0.0) as usize, (xb.ceil().max(0.0) as usize).min(self.w));
                    for px in pa..pb {
                        let cx = px as f64 + 0.5;
                        let a = ((cx - xa + 0.5).min(xb - cx + 0.5)).clamp(0.0, 1.0) as f32;
                        let i = py * self.w + px;
                        if a > self.ink[i] {
                            self.ink[i] = a;
                        }
                    }
                }
            }
        }
    }
}

/// Collects flattened glyph contours.
struct Pen {
    contours: Vec<Vec<[f64; 2]>>,
    cur: Vec<[f64; 2]>,
}

impl Pen {
    fn last(&self) -> [f64; 2] {
        *self.cur.last().unwrap_or(&[0.0, 0.0])
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur = vec![[x as f64, y as f64]];
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cur.push([x as f64, y as f64]);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let p0 = self.last();
        for i in 1..=6 {
            let t = i as f64 / 6.0;
            let m = 1.0 - t;
            self.cur.push([m * m * p0[0] + 2.0 * m * t * cx as f64 + t * t * x as f64, m * m * p0[1] + 2.0 * m * t * cy as f64 + t * t * y as f64]);
        }
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let p0 = self.last();
        for i in 1..=8 {
            let t = i as f64 / 8.0;
            let m = 1.0 - t;
            let (a, b, c, d) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
            self.cur.push([a * p0[0] + b * c0x as f64 + c * c1x as f64 + d * x as f64, a * p0[1] + b * c0y as f64 + c * c1y as f64 + d * y as f64]);
        }
    }
    fn close(&mut self) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
    }
}

/// Renders the sheet to a PNG `width_px` wide (white paper, black ink).
pub fn to_png(s: &Sheet, width_px: u32) -> Vec<u8> {
    let ppm = width_px as f64 / s.width; // pixels per mm
    let (w, h) = (width_px as usize, (s.height * ppm).round().max(1.0) as usize);
    let k = ppm * SS;
    let mut cv = Canvas { w: w * SS as usize, h: h * SS as usize, ink: vec![0.0; w * h * (SS as usize).pow(2)] };
    let to_px = |p: [f64; 2]| [p[0] * k, (s.height - p[1]) * k];
    for l in &s.lines {
        let (lw, dash) = pen_style(l.pen);
        let pts: Vec<[f64; 2]> = l.pts.iter().map(|p| to_px(*p)).collect();
        cv.polyline(&pts, (lw * k / 2.0).max(0.55), dash.map(|d| [d[0] * k, d[1] * k]));
    }
    if let Ok(font) = FontRef::new(epaint_default_fonts::UBUNTU_LIGHT) {
        let cmap = font.charmap();
        let glyphs = font.outline_glyphs();
        for t in &s.texts {
            let em = t.size / 0.69 * k;
            let size = Size::new(em as f32);
            let metrics = font.glyph_metrics(size, LocationRef::default());
            let ids: Vec<_> = t.text.chars().map(|c| cmap.map(c).or_else(|| cmap.map('?')).unwrap_or_default()).collect();
            let adv: Vec<f64> = ids.iter().map(|g| metrics.advance_width(*g).unwrap_or(em as f32 * 0.5) as f64).collect();
            let total: f64 = adv.iter().sum();
            let start = match t.anchor {
                Anchor::Start => 0.0,
                Anchor::Middle => -total / 2.0,
                Anchor::End => -total,
            };
            let o = to_px(t.at);
            let (sn, cs) = t.rotate.to_radians().sin_cos();
            let mut x = start;
            for (g, a) in ids.iter().zip(&adv) {
                if let Some(glyph) = glyphs.get(*g) {
                    let mut pen = Pen { contours: vec![], cur: vec![] };
                    if glyph.draw(DrawSettings::unhinted(size, LocationRef::default()), &mut pen).is_ok() {
                        pen.close();
                        // glyph space (y up) -> rotated -> pixels (y down)
                        let cs_px: Vec<Vec<[f64; 2]>> = pen
                            .contours
                            .iter()
                            .map(|c| c.iter().map(|p| {
                                let (gx, gy) = (p[0] + x, p[1]);
                                [o[0] + gx * cs - gy * sn, o[1] - (gx * sn + gy * cs)]
                            }).collect())
                            .collect();
                        cv.fill(&cs_px);
                    }
                }
                x += a;
            }
        }
    }
    // downsample and encode
    let ss = SS as usize;
    let mut rgba = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let mut a = 0.0;
            for dy in 0..ss {
                for dx in 0..ss {
                    a += cv.ink[(y * ss + dy) * cv.w + x * ss + dx];
                }
            }
            let v = (255.0 * (1.0 - a / (ss * ss) as f32)).round() as u8;
            let i = (y * w + x) * 4;
            rgba[i] = v;
            rgba[i + 1] = v;
            rgba[i + 2] = v;
        }
    }
    let mut out = vec![];
    {
        let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut wr) = enc.write_header() {
            let _ = wr.write_image_data(&rgba);
        }
    }
    out
}
