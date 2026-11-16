//! Headless software renderer: shaded mesh + B-rep edges + sketches to PNG.
//! Used by the MCP server so an AI agent can *see* the part it is building,
//! with optional face-index labels for picking faces to sketch on.

use rw_kernel::Mesh;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Isometric,
    Front,
    Back,
    Top,
    Bottom,
    Right,
    Left,
    /// Isometric from below/behind.
    IsometricBack,
}

impl View {
    /// (eye direction from target, up vector), Y-up like SOLIDWORKS.
    pub fn basis(self) -> ([f64; 3], [f64; 3]) {
        match self {
            View::Isometric => ([1.0, 0.8, 1.0], [0.0, 1.0, 0.0]),
            View::IsometricBack => ([-1.0, -0.8, -1.0], [0.0, 1.0, 0.0]),
            View::Front => ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
            View::Back => ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
            View::Top => ([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
            View::Bottom => ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
            View::Right => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            View::Left => ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        }
    }
}

pub struct RenderOptions<'a> {
    pub width: u32,
    pub height: u32,
    pub view: View,
    pub edges: bool,
    pub face_labels: bool,
    pub edge_labels: bool,
    /// Sketch polylines: (points, construction)
    pub sketches: &'a [(Vec<[f32; 3]>, bool)],
    /// Optional per-triangle base colors (e.g. one per assembly component).
    pub tri_colors: &'a [[f32; 3]],
}

struct Cam {
    right: [f64; 3],
    up: [f64; 3],
    fwd: [f64; 3],
    center: [f64; 3],
    scale: f64,
    w: f64,
    h: f64,
}

impl Cam {
    /// -> (screen x, screen y, depth: larger = closer)
    fn project(&self, p: [f32; 3]) -> [f64; 3] {
        let d = [p[0] as f64 - self.center[0], p[1] as f64 - self.center[1], p[2] as f64 - self.center[2]];
        let x = dot(d, self.right);
        let y = dot(d, self.up);
        let z = dot(d, self.fwd);
        [self.w / 2.0 + x * self.scale, self.h / 2.0 - y * self.scale, z]
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = dot(a, a).sqrt().max(1e-30);
    [a[0] / l, a[1] / l, a[2] / l]
}

struct Canvas {
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
    z: Vec<f64>,
}

impl Canvas {
    fn plot(&mut self, x: i64, y: i64, z: f64, c: [f32; 3], ztest: bool) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let i = y as usize * self.w + x as usize;
        if !ztest || z >= self.z[i] {
            self.rgb[i] = c;
        }
    }

    fn line(&mut self, a: [f64; 3], b: [f64; 3], c: [f32; 3], thick: i64, bias: f64, ztest: bool) {
        let n = ((b[0] - a[0]).abs().max((b[1] - a[1]).abs()).ceil() as usize).max(1);
        for i in 0..=n {
            let t = i as f64 / n as f64;
            let x = a[0] + (b[0] - a[0]) * t;
            let y = a[1] + (b[1] - a[1]) * t;
            let z = a[2] + (b[2] - a[2]) * t + bias;
            for dy in 0..thick {
                for dx in 0..thick {
                    self.plot(x as i64 + dx - thick / 2, y as i64 + dy - thick / 2, z, c, ztest);
                }
            }
        }
    }
}

/// Renders to RGBA8 pixels (width*height*4).
pub fn render_rgba(mesh: Option<&Mesh>, o: &RenderOptions) -> Vec<u8> {
    const SS: usize = 2; // supersampling factor
    let (w, h) = (o.width as usize * SS, o.height as usize * SS);
    let (eye, up0) = o.view.basis();
    let fwd = norm(eye); // toward viewer
    let right = norm(cross(up0, fwd));
    let up = cross(fwd, right);

    // Fit bounds of everything visible.
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut grow = |p: [f32; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] as f64);
            hi[k] = hi[k].max(p[k] as f64);
        }
    };
    if let Some(m) = mesh {
        m.positions.iter().for_each(|p| grow(*p));
    }
    for (pl, _) in o.sketches {
        pl.iter().for_each(|p| grow(*p));
    }
    if !lo[0].is_finite() {
        lo = [-50.0; 3];
        hi = [50.0; 3];
    }
    let center = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    // Projected extent of the bbox corners.
    let mut ext = [0.0f64; 2];
    for i in 0..8 {
        let c = [
            if i & 1 == 0 { lo[0] } else { hi[0] } - center[0],
            if i & 2 == 0 { lo[1] } else { hi[1] } - center[1],
            if i & 4 == 0 { lo[2] } else { hi[2] } - center[2],
        ];
        ext[0] = ext[0].max(dot(c, right).abs());
        ext[1] = ext[1].max(dot(c, up).abs());
    }
    let scale = 0.86 * (w as f64 / (2.0 * ext[0].max(1e-6))).min(h as f64 / (2.0 * ext[1].max(1e-6)));
    let cam = Cam { right, up, fwd, center, scale, w: w as f64, h: h as f64 };

    // Background: soft vertical gradient.
    let mut cv = Canvas { w, h, rgb: vec![[0.0; 3]; w * h], z: vec![f64::NEG_INFINITY; w * h] };
    for y in 0..h {
        let t = y as f32 / h as f32;
        let c = [0.98 - 0.16 * t, 0.985 - 0.12 * t, 1.0 - 0.06 * t];
        for x in 0..w {
            cv.rgb[y * w + x] = c;
        }
    }

    let light = norm([fwd[0] + 0.3 * up[0] - 0.2 * right[0], fwd[1] + 0.3 * up[1] - 0.2 * right[1], fwd[2] + 0.3 * up[2] - 0.2 * right[2]]);
    let base = [0.70f32, 0.75, 0.82];
    if let Some(m) = mesh {
        let sp: Vec<[f64; 3]> = m.positions.iter().map(|p| cam.project(*p)).collect();
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let base = o.tri_colors.get(ti).copied().unwrap_or(base);
            let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let (a, b, c) = (sp[ia], sp[ib], sp[ic]);
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area.abs() < 1e-12 {
                continue;
            }
            let x0 = a[0].min(b[0]).min(c[0]).floor().max(0.0) as i64;
            let x1 = a[0].max(b[0]).max(c[0]).ceil().min(w as f64 - 1.0) as i64;
            let y0 = a[1].min(b[1]).min(c[1]).floor().max(0.0) as i64;
            let y1 = a[1].max(b[1]).max(c[1]).ceil().min(h as f64 - 1.0) as i64;
            let na = m.normals[ia].map(|v| v as f64);
            let nb = m.normals[ib].map(|v| v as f64);
            let nc = m.normals[ic].map(|v| v as f64);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let w0 = ((b[0] - px) * (c[1] - py) - (b[1] - py) * (c[0] - px)) / area;
                    let w1 = ((c[0] - px) * (a[1] - py) - (c[1] - py) * (a[0] - px)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                        continue;
                    }
                    let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                    let i = y as usize * w + x as usize;
                    if z <= cv.z[i] {
                        continue;
                    }
                    cv.z[i] = z;
                    let n = norm([
                        w0 * na[0] + w1 * nb[0] + w2 * nc[0],
                        w0 * na[1] + w1 * nb[1] + w2 * nc[1],
                        w0 * na[2] + w1 * nb[2] + w2 * nc[2],
                    ]);
                    let facing = dot(n, fwd);
                    let n = if facing < 0.0 { [-n[0], -n[1], -n[2]] } else { n };
                    let diff = dot(n, light).max(0.0) as f32;
                    let half = norm([light[0] + fwd[0], light[1] + fwd[1], light[2] + fwd[2]]);
                    let spec = (dot(n, half).max(0.0) as f32).powf(40.0) * 0.25;
                    let k = 0.38 + 0.62 * diff;
                    cv.rgb[i] = [base[0] * k + spec, base[1] * k + spec, base[2] * k + spec];
                }
            }
        }
        if o.edges {
            let bias = 1e-3 * (hi[0] - lo[0] + hi[1] - lo[1] + hi[2] - lo[2]).max(1e-6);
            for pl in &m.edges {
                for s in pl.windows(2) {
                    cv.line(cam.project(s[0]), cam.project(s[1]), [0.08, 0.08, 0.1], SS as i64, bias, true);
                }
            }
        }
    }
    for (pl, construction) in o.sketches {
        let c = if *construction { [0.55, 0.55, 0.6] } else { [0.1, 0.35, 0.95] };
        for (k, s) in pl.windows(2).enumerate() {
            if *construction && k % 2 == 1 {
                continue;
            }
            cv.line(cam.project(s[0]), cam.project(s[1]), c, SS as i64, 0.0, false);
        }
    }
    if let (Some(m), true) = (mesh, o.face_labels) {
        // Anchor each label on the face's largest visible triangle (depth-tested),
        // so hidden faces are not labelled.
        let sp: Vec<[f64; 3]> = m.positions.iter().map(|p| cam.project(*p)).collect();
        let mut best: Vec<Option<(f64, [f64; 2])>> = vec![None; m.faces.len().max(1) + m.tri_face.iter().map(|f| *f as usize + 1).max().unwrap_or(0)];
        let zr = 1e-3 * (hi[0] - lo[0] + hi[1] - lo[1] + hi[2] - lo[2]).max(1e-6);
        for (ti, t) in m.indices.chunks(3).enumerate() {
            let (a, b, c) = (sp[t[0] as usize], sp[t[1] as usize], sp[t[2] as usize]);
            let area = ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * 0.5;
            let cx = (a[0] + b[0] + c[0]) / 3.0;
            let cy = (a[1] + b[1] + c[1]) / 3.0;
            let cz = (a[2] + b[2] + c[2]) / 3.0;
            let (xi, yi) = (cx as i64, cy as i64);
            if xi < 0 || yi < 0 || xi >= w as i64 || yi >= h as i64 {
                continue;
            }
            if cz + zr * 5.0 < cv.z[yi as usize * w + xi as usize] {
                continue; // occluded
            }
            let fi = m.tri_face[ti] as usize;
            if best[fi].map_or(true, |(ar, _)| area > ar) {
                best[fi] = Some((area, [cx, cy]));
            }
        }
        for f in &m.faces {
            if let Some(Some((area, p))) = best.get(f.index) {
                if *area > 4.0 * (SS * SS) as f64 {
                    draw_label(&mut cv, p[0] as i64, p[1] as i64, &f.index.to_string(), SS as i64 * 2);
                }
            }
        }
    }
    if let (Some(m), true) = (mesh, o.edge_labels) {
        let zr = 2e-3 * (hi[0] - lo[0] + hi[1] - lo[1] + hi[2] - lo[2]).max(1e-6);
        for (pl, id) in m.edges.iter().zip(&m.edge_ids) {
            let mid = pl[pl.len() / 2];
            let p = cam.project(mid);
            let (x, y) = (p[0] as i64, p[1] as i64);
            if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                continue;
            }
            // visible if nothing is clearly in front of the edge midpoint
            let zb = cv.z[y as usize * w + x as usize];
            if zb.is_finite() && p[2] + zr * 10.0 < zb {
                continue;
            }
            draw_tag(&mut cv, x, y, &id.to_string(), SS as i64 * 2, [0.55, 0.95, 1.0]);
        }
    }
    draw_triad(&mut cv, &cam, SS as i64);

    // Downsample
    let (ow, oh) = (o.width as usize, o.height as usize);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = [0.0f32; 3];
            for dy in 0..SS {
                for dx in 0..SS {
                    let c = cv.rgb[(y * SS + dy) * w + x * SS + dx];
                    for k in 0..3 {
                        acc[k] += c[k];
                    }
                }
            }
            let i = (y * ow + x) * 4;
            for k in 0..3 {
                out[i + k] = ((acc[k] / (SS * SS) as f32).clamp(0.0, 1.0) * 255.0) as u8;
            }
            out[i + 3] = 255;
        }
    }
    out
}

pub fn render_png(mesh: Option<&Mesh>, o: &RenderOptions) -> Vec<u8> {
    let rgba = render_rgba(mesh, o);
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, o.width, o.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().expect("png header");
        wr.write_image_data(&rgba).expect("png data");
    }
    buf
}

fn draw_triad(cv: &mut Canvas, cam: &Cam, s: i64) {
    let o = [60.0 * s as f64 / 2.0 + 10.0, cv.h as f64 - 60.0 * s as f64 / 2.0 - 10.0];
    let len = 28.0 * s as f64;
    let axes = [([1.0, 0.0, 0.0], [0.85, 0.1, 0.1], "X"), ([0.0, 1.0, 0.0], [0.1, 0.6, 0.1], "Y"), ([0.0, 0.0, 1.0], [0.1, 0.2, 0.9], "Z")];
    for (a, c, l) in axes {
        let e = [o[0] + dot(a, cam.right) * len, o[1] - dot(a, cam.up) * len];
        cv.line([o[0], o[1], 0.0], [e[0], e[1], 0.0], c, s, 0.0, false);
        draw_text(cv, e[0] as i64 + 2 * s, e[1] as i64 - 3 * s, l, s, c);
    }
}

fn draw_label(cv: &mut Canvas, x: i64, y: i64, txt: &str, s: i64) {
    draw_tag(cv, x, y, txt, s, [1.0, 0.92, 0.3]);
}

fn draw_tag(cv: &mut Canvas, x: i64, y: i64, txt: &str, s: i64, bg: [f32; 3]) {
    let wpx = txt.len() as i64 * 4 * s + s;
    let hpx = 7 * s;
    for yy in 0..hpx {
        for xx in 0..wpx {
            cv.plot(x - wpx / 2 + xx, y - hpx / 2 + yy, 0.0, bg, false);
        }
    }
    draw_text(cv, x - wpx / 2 + s, y - hpx / 2 + s, txt, s, [0.1, 0.1, 0.1]);
}

/// 3x5 bitmap font for digits and X/Y/Z.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        _ => [0; 5],
    }
}

fn draw_text(cv: &mut Canvas, x: i64, y: i64, txt: &str, s: i64, c: [f32; 3]) {
    for (i, ch) in txt.chars().enumerate() {
        let g = glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    for dy in 0..s {
                        for dx in 0..s {
                            cv.plot(x + (i as i64 * 4 + col) * s + dx, y + row as i64 * s + dy, 0.0, c, false);
                        }
                    }
                }
            }
        }
    }
}
