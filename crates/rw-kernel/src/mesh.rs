//! Display/analysis mesh produced from a B-rep solid.

use serde::Serialize;
use std::io::Write;

/// Triangle mesh with per-vertex normals, per-triangle B-rep face ids and
/// polyline B-rep edges (for silhouette/edge rendering).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// B-rep face index for each triangle.
    pub tri_face: Vec<u32>,
    pub edges: Vec<Vec<[f32; 3]>>,
    /// Kernel edge index for each entry of `edges` (for edge selection).
    pub edge_ids: Vec<u32>,
    pub faces: Vec<FaceInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FaceInfo {
    pub index: usize,
    /// "plane" or "curved"
    pub kind: &'static str,
    pub area: f64,
    pub centroid: [f64; 3],
    /// Outward normal (average for curved faces).
    pub normal: [f64; 3],
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct MassProps {
    pub volume: f64,
    pub surface_area: f64,
    pub centroid: [f64; 3],
    pub bbox_min: [f64; 3],
    pub bbox_max: [f64; 3],
}

impl Mesh {
    /// Replaces zero normals by area-weighted triangle normals.
    #[cfg_attr(not(feature = "truck"), allow(dead_code))]
    pub(crate) fn fix_normals(&mut self) {
        let bad: Vec<bool> = self.normals.iter().map(|n| n[0] * n[0] + n[1] * n[1] + n[2] * n[2] < 1e-12).collect();
        if !bad.iter().any(|b| *b) {
            return;
        }
        for t in self.indices.chunks(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let (pa, pb, pc) = (self.positions[a], self.positions[b], self.positions[c]);
            let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            for i in [a, b, c] {
                if bad[i] {
                    for k in 0..3 {
                        self.normals[i][k] += n[k];
                    }
                }
            }
        }
        for (i, n) in self.normals.iter_mut().enumerate() {
            if bad[i] {
                let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-20);
                *n = [n[0] / l, n[1] / l, n[2] / l];
            }
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn mass_props(&self) -> MassProps {
        let mut vol = 0.0;
        let mut area = 0.0;
        let mut c = [0.0; 3];
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in &self.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k] as f64);
                hi[k] = hi[k].max(p[k] as f64);
            }
        }
        for t in self.indices.chunks(3) {
            let a = self.positions[t[0] as usize].map(|v| v as f64);
            let b = self.positions[t[1] as usize].map(|v| v as f64);
            let cc = self.positions[t[2] as usize].map(|v| v as f64);
            let v = (a[0] * (b[1] * cc[2] - b[2] * cc[1]) - a[1] * (b[0] * cc[2] - b[2] * cc[0])
                + a[2] * (b[0] * cc[1] - b[1] * cc[0]))
                / 6.0;
            vol += v;
            for k in 0..3 {
                c[k] += v * (a[k] + b[k] + cc[k]) / 4.0;
            }
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [cc[0] - a[0], cc[1] - a[1], cc[2] - a[2]];
            let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
            area += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        }
        if vol.abs() > 1e-300 {
            for k in c.iter_mut() {
                *k /= vol;
            }
        }
        if !lo[0].is_finite() {
            lo = [0.0; 3];
            hi = [0.0; 3];
        }
        MassProps { volume: vol, surface_area: area, centroid: c, bbox_min: lo, bbox_max: hi }
    }

    pub fn write_stl(&self, w: &mut impl Write) -> std::io::Result<()> {
        let mut header = [0u8; 80];
        let tag = b"RustWorks binary STL";
        header[..tag.len()].copy_from_slice(tag);
        w.write_all(&header)?;
        w.write_all(&(self.triangle_count() as u32).to_le_bytes())?;
        for t in self.indices.chunks(3) {
            let [a, b, c] = [self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]];
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let mut n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-30);
            n = n.map(|x| x / l);
            for vtx in [n, a, b, c] {
                for f in vtx {
                    w.write_all(&f.to_le_bytes())?;
                }
            }
            w.write_all(&[0, 0])?;
        }
        Ok(())
    }

    pub fn write_obj(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "# RustWorks OBJ export")?;
        for p in &self.positions {
            writeln!(w, "v {} {} {}", p[0], p[1], p[2])?;
        }
        for n in &self.normals {
            writeln!(w, "vn {} {} {}", n[0], n[1], n[2])?;
        }
        for t in self.indices.chunks(3) {
            let (a, b, c) = (t[0] + 1, t[1] + 1, t[2] + 1);
            writeln!(w, "f {a}//{a} {b}//{b} {c}//{c}")?;
        }
        Ok(())
    }
}
