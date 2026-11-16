//! Geometry kernel facade. The default backend is OpenCASCADE (OCCT 7.8,
//! statically built); a pure-Rust `truck` backend is available with
//! `--no-default-features --features truck`. Everything above this crate
//! (feature tree, GUI, MCP) is backend-agnostic.

mod mesh;
pub use mesh::{FaceInfo, MassProps, Mesh};

#[cfg(feature = "occt")]
mod occ;
#[cfg(feature = "occt")]
mod occ_ext;
#[cfg(feature = "occt")]
pub use occ::*;

#[cfg(all(feature = "truck", not(feature = "occt")))]
mod truck_impl;
#[cfg(all(feature = "truck", not(feature = "occt")))]
pub use truck_impl::*;

#[cfg(not(any(feature = "occt", feature = "truck")))]
compile_error!("enable a kernel backend: feature `occt` (default) or `truck`");

use anyhow::Result;
use serde::Serialize;

pub const BACKEND: &str = if cfg!(feature = "occt") { "OpenCASCADE 7.8" } else { "truck" };

/// A sketch plane: origin plus orthonormal in-plane axes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Plane {
    pub origin: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
}

fn v_norm(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn v_cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn v_dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn v_scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

impl Plane {
    pub fn new(origin: [f64; 3], normal: [f64; 3], xdir: Option<[f64; 3]>) -> Result<Plane> {
        let l = v_norm(normal);
        if l < 1e-10 {
            anyhow::bail!("plane normal is zero");
        }
        let n = v_scale(normal, 1.0 / l);
        // Stable default x axis: like the standard planes for axis-aligned normals.
        let guess = xdir.unwrap_or(if n[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { v_cross([0.0, 1.0, 0.0], n) });
        let gx = [guess[0] - n[0] * v_dot(guess, n), guess[1] - n[1] * v_dot(guess, n), guess[2] - n[2] * v_dot(guess, n)];
        let gl = v_norm(gx);
        if gl < 1e-10 {
            anyhow::bail!("plane x direction is parallel to its normal");
        }
        let x = v_scale(gx, 1.0 / gl);
        Ok(Plane { origin, x, y: v_cross(n, x) })
    }
    pub fn normal(&self) -> [f64; 3] {
        v_cross(self.x, self.y)
    }
    pub fn to3d(&self, p: [f64; 2]) -> [f64; 3] {
        let o = self.origin;
        [o[0] + self.x[0] * p[0] + self.y[0] * p[1], o[1] + self.x[1] * p[0] + self.y[1] * p[1], o[2] + self.x[2] * p[0] + self.y[2] * p[1]]
    }
    pub fn offset(&self, d: f64) -> Plane {
        let n = self.normal();
        Plane { origin: [self.origin[0] + n[0] * d, self.origin[1] + n[1] * d, self.origin[2] + n[2] * d], ..*self }
    }
    pub fn dir3d(&self, d: [f64; 2]) -> [f64; 3] {
        [self.x[0] * d[0] + self.y[0] * d[1], self.x[1] * d[0] + self.y[1] * d[1], self.x[2] * d[0] + self.y[2] * d[1]]
    }
    /// 3D point → plane coordinates.
    pub fn to2d(&self, p: [f64; 3]) -> [f64; 2] {
        let d = [p[0] - self.origin[0], p[1] - self.origin[1], p[2] - self.origin[2]];
        [v_dot(d, self.x), v_dot(d, self.y)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

/// Outcome of a robust union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnionKind {
    Fused,
    FusedWithOverlap,
    MergedTouching,
}

/// Rigid transforms used by patterns and mirrors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Xform {
    Translate([f64; 3]),
    Rotate { origin: [f64; 3], axis: [f64; 3], deg: f64 },
    Mirror { origin: [f64; 3], normal: [f64; 3] },
}

impl Xform {
    /// Row-major 4x4 matrix.
    pub fn matrix(&self) -> [[f64; 4]; 4] {
        match *self {
            Xform::Translate(d) => translation(d),
            Xform::Rotate { origin, axis, deg } => rotation(origin, axis, deg),
            Xform::Mirror { origin, normal } => reflection(origin, normal),
        }
    }
}

/// Analytic geometry of a B-rep face (used by assembly mates).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FaceGeom {
    Plane { origin: [f64; 3], normal: [f64; 3] },
    Cylinder { origin: [f64; 3], axis: [f64; 3], radius: f64 },
    Cone { apex: [f64; 3], axis: [f64; 3], half_angle: f64 },
    Sphere { center: [f64; 3], radius: f64 },
    Other,
}

/// A projected edge in drawing coordinates (mm), from hidden-line removal.
#[derive(Clone, Debug)]
pub struct Edge2 {
    pub pts: Vec<[f64; 2]>,
    /// "line", "circle" or "curve"
    pub kind: &'static str,
}

/// Orthographic view axes: direction toward the viewer and the drawing's +X.
/// Drawing +Y is `dir × x`.
#[derive(Clone, Copy, Debug)]
pub struct ViewAxes {
    pub dir: [f64; 3],
    pub x: [f64; 3],
}

#[derive(Clone, Debug, Serialize)]
pub struct EdgeInfo {
    pub index: usize,
    /// "line", "circle" or "curve"
    pub kind: &'static str,
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub mid: [f64; 3],
    pub length: f64,
    #[serde(skip)]
    pub polyline: Vec<[f32; 3]>,
}

impl EdgeInfo {
    pub fn from_polyline(index: usize, kind: &'static str, pl: Vec<[f32; 3]>) -> EdgeInfo {
        let f = |p: [f32; 3]| p.map(|v| v as f64);
        let len: f64 = pl.windows(2).map(|w| dist(f(w[0]), f(w[1]))).sum();
        let mid = f(pl[pl.len() / 2]);
        EdgeInfo { index, kind, start: f(pl[0]), end: f(pl[pl.len() - 1]), mid, length: len, polyline: pl }
    }

    /// Distance from `p` to this edge.
    pub fn distance_to(&self, p: [f64; 3]) -> f64 {
        self.polyline
            .windows(2)
            .map(|w| point_seg_dist(p, w[0].map(|v| v as f64), w[1].map(|v| v as f64)))
            .fold(f64::INFINITY, f64::min)
    }
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn point_seg_dist(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let l2 = v_dot(ab, ab);
    let t = if l2 > 0.0 { (v_dot(ap, ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t])
}

/// Distance from a point to a triangle.
pub fn point_tri_dist(p: [f64; 3], t: [[f64; 3]; 3]) -> f64 {
    let [a, b, c] = t;
    let sub = |x: [f64; 3], y: [f64; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
    let n = v_cross(sub(b, a), sub(c, a));
    let nn = v_dot(n, n);
    if nn > 1e-30 {
        let d = v_dot(sub(p, a), n) / nn;
        let q = [p[0] - n[0] * d, p[1] - n[1] * d, p[2] - n[2] * d];
        let inside = [(a, b), (b, c), (c, a)].iter().all(|(u, v)| v_dot(n, v_cross(sub(*v, *u), sub(q, *u))) >= 0.0);
        if inside {
            return dist(p, q);
        }
    }
    point_seg_dist(p, a, b).min(point_seg_dist(p, b, c)).min(point_seg_dist(p, c, a))
}

/// Index of the mesh face nearest to point `p`.
pub fn face_near(mesh: &Mesh, p: [f64; 3]) -> Option<(usize, f64)> {
    let mut best: std::collections::HashMap<u32, f64> = Default::default();
    for (t, tri) in mesh.indices.chunks(3).enumerate() {
        let d = point_tri_dist(p, [tri[0], tri[1], tri[2]].map(|i| mesh.positions[i as usize].map(|v| v as f64)));
        let e = best.entry(mesh.tri_face[t]).or_insert(f64::INFINITY);
        *e = e.min(d);
    }
    best.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).map(|(f, d)| (f as usize, d))
}

/// Index of the edge nearest to point `p` and its distance.
pub fn edge_near(edges: &[EdgeInfo], p: [f64; 3]) -> Option<(usize, f64)> {
    edges.iter().map(|e| (e.index, e.distance_to(p))).min_by(|a, b| a.1.total_cmp(&b.1))
}

pub fn translation(d: [f64; 3]) -> [[f64; 4]; 4] {
    [[1.0, 0.0, 0.0, d[0]], [0.0, 1.0, 0.0, d[1]], [0.0, 0.0, 1.0, d[2]], [0.0, 0.0, 0.0, 1.0]]
}

/// Rotation about an arbitrary axis through `origin` (Rodrigues).
pub fn rotation(origin: [f64; 3], axis: [f64; 3], deg: f64) -> [[f64; 4]; 4] {
    let a = v_scale(axis, 1.0 / v_norm(axis).max(1e-30));
    let (s, c) = deg.to_radians().sin_cos();
    let t = 1.0 - c;
    let r = [
        [t * a[0] * a[0] + c, t * a[0] * a[1] - s * a[2], t * a[0] * a[2] + s * a[1]],
        [t * a[0] * a[1] + s * a[2], t * a[1] * a[1] + c, t * a[1] * a[2] - s * a[0]],
        [t * a[0] * a[2] - s * a[1], t * a[1] * a[2] + s * a[0], t * a[2] * a[2] + c],
    ];
    let o = origin;
    let mut m = [[0.0; 4]; 4];
    for i in 0..3 {
        m[i][..3].copy_from_slice(&r[i]);
        m[i][3] = o[i] - (r[i][0] * o[0] + r[i][1] * o[1] + r[i][2] * o[2]);
    }
    m[3][3] = 1.0;
    m
}

/// Reflection across a plane through `origin` with `normal`.
pub fn reflection(origin: [f64; 3], normal: [f64; 3]) -> [[f64; 4]; 4] {
    let n = v_scale(normal, 1.0 / v_norm(normal).max(1e-30));
    let d = v_dot(n, origin);
    let mut m = [[0.0; 4]; 4];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = if i == j { 1.0 } else { 0.0 } - 2.0 * n[i] * n[j];
        }
        m[i][3] = 2.0 * d * n[i];
    }
    m[3][3] = 1.0;
    m
}

/// STEP as a string (via a temp file; OCCT only writes files).
pub fn step_string(s: &Solid) -> Result<String> {
    let p = std::env::temp_dir().join(format!("rustworks-{}-{:?}.step", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
    write_step(s, &p)?;
    let txt = std::fs::read_to_string(&p)?;
    let _ = std::fs::remove_file(&p);
    Ok(txt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rw_sketch::Sketch;

    fn mp(s: &Solid) -> MassProps {
        let m = tessellate(s, 0.01, true);
        mass_props(s, &m)
    }

    fn plate_with_hole() -> Solid {
        let mut sk = Sketch::default();
        sk.add_rectangle([0.0, 0.0], [40.0, 20.0]);
        sk.add_circle([20.0, 10.0], 5.0, false);
        let pl = Plane::new([0.0; 3], [0.0, 0.0, 1.0], Some([1.0, 0.0, 0.0])).unwrap();
        extrude(&region_face(&pl, &sk.regions().unwrap()[0]).unwrap(), [0.0, 0.0, 5.0]).unwrap()
    }

    fn close(v: f64, e: f64, rel: f64) {
        assert!((v - e).abs() / e.abs() < rel, "{v} vs expected {e}");
    }

    #[test]
    fn volume_of_plate() {
        let s = plate_with_hole();
        close(mp(&s).volume, 40.0 * 20.0 * 5.0 - std::f64::consts::PI * 25.0 * 5.0, 3e-3);
        assert!(!tessellate(&s, 0.05, true).edges.is_empty());
    }

    #[test]
    fn boolean_cut_and_pattern() {
        let s = plate_with_hole();
        let mut sk = Sketch::default();
        sk.add_circle([5.0, 5.0], 2.0, false);
        let pl = Plane::new([0.0, 0.0, 6.0], [0.0, 0.0, 1.0], None).unwrap();
        let tool = extrude(&region_face(&pl, &sk.regions().unwrap()[0]).unwrap(), [0.0, 0.0, -10.0]).unwrap();
        let mut body = boolean(&s, &tool, BoolOp::Subtract, 45.0).unwrap();
        let t2 = transform(&tool, &Xform::Translate([30.0, 10.0, 0.0]));
        body = boolean(&body, &t2, BoolOp::Subtract, 45.0).unwrap();
        close(mp(&body).volume, 40.0 * 20.0 * 5.0 - std::f64::consts::PI * (25.0 + 8.0) * 5.0, 3e-3);
        assert!(step_string(&body).unwrap().contains("ISO-10303-21"));
    }

    #[test]
    fn revolve_and_mirror() {
        let mut sk = Sketch::default();
        sk.add_rectangle([5.0, 0.0], [10.0, 20.0]);
        let pl = Plane::new([0.0; 3], [0.0, 0.0, 1.0], Some([1.0, 0.0, 0.0])).unwrap();
        let tube = revolve(&region_face(&pl, &sk.regions().unwrap()[0]).unwrap(), [0.0; 3], [0.0, 1.0, 0.0], 360.0).unwrap();
        let e = std::f64::consts::PI * 75.0 * 20.0;
        close(mp(&tube).volume, e, 3e-3);
        let m = transform(&tube, &Xform::Mirror { origin: [0.0, -1.0, 0.0], normal: [0.0, 1.0, 0.0] });
        close(mp(&m).volume, e, 3e-3);
        let b = mp(&m);
        assert!(b.bbox_max[1] < -1.9 && b.bbox_min[1] > -22.1, "mirrored to y in [-22,-2]: {:?}", (b.bbox_min, b.bbox_max));
    }

    #[cfg(feature = "occt")]
    #[test]
    fn fillet_chamfer_shell() {
        let mut sk = Sketch::default();
        sk.add_rectangle([0.0, 0.0], [20.0, 20.0]);
        let pl = Plane::new([0.0; 3], [0.0, 0.0, 1.0], Some([1.0, 0.0, 0.0])).unwrap();
        let cube = extrude(&region_face(&pl, &sk.regions().unwrap()[0]).unwrap(), [0.0, 0.0, 20.0]).unwrap();
        let es = edges(&cube);
        assert_eq!(es.len(), 12);
        let (top_edge, _) = edge_near(&es, [10.0, 0.0, 20.0]).unwrap();
        let f = fillet(&cube, &[top_edge], 2.0).unwrap();
        // removed material along a 20mm edge: (r² - πr²/4) * L
        close(mp(&f).volume, 8000.0 - (4.0 - std::f64::consts::PI) * 20.0, 1e-4);
        let c = chamfer(&cube, &[top_edge], 2.0).unwrap();
        close(mp(&c).volume, 8000.0 - 2.0 * 20.0, 1e-4);
        let m = tessellate(&cube, 0.05, true);
        let (top, _) = face_near(&m, [10.0, 10.0, 20.0]).unwrap();
        let sh = shell(&cube, &[top], 1.0).unwrap();
        close(mp(&sh).volume, 8000.0 - 18.0 * 18.0 * 19.0, 1e-4);
        // coplanar union: stacked same-footprint block (failed with truck)
        let pl2 = Plane::new([0.0, 0.0, 20.0], [0.0, 0.0, 1.0], Some([1.0, 0.0, 0.0])).unwrap();
        let top_block = extrude(&region_face(&pl2, &sk.regions().unwrap()[0]).unwrap(), [0.0, 0.0, 5.0]).unwrap();
        let u = boolean(&cube, &top_block, BoolOp::Union, 30.0).unwrap();
        close(mp(&u).volume, 10000.0, 1e-6);
        assert_eq!(tessellate(&u, 0.05, true).faces.len(), 6, "coplanar faces merged");
    }

    #[cfg(feature = "occt")]
    #[test]
    fn sweep_draft_facegeom() {
        // 2x2 square swept along an L path (line + quarter arc + line) in the XY plane
        let mut path = Sketch::default();
        path.add_line([0.0, 0.0], [0.0, 20.0], false);
        path.add_arc([10.0, 20.0], [10.0, 30.0], [0.0, 20.0], false);
        let front = Plane::new([0.0; 3], [0.0, 0.0, 1.0], Some([1.0, 0.0, 0.0])).unwrap();
        let p = &path.paths().unwrap()[0];
        // profile on the XZ plane at the path start (normal +Y)
        let mut prof = Sketch::default();
        prof.add_circle([0.0, 0.0], 1.0, false);
        let top = Plane::new([0.0; 3], [0.0, 1.0, 0.0], Some([1.0, 0.0, 0.0])).unwrap();
        let r = &prof.regions().unwrap()[0];
        let s = sweep(&top, r, &front, p).unwrap();
        let len = 20.0 + std::f64::consts::PI * 10.0 / 2.0;
        close(mp(&s).volume, std::f64::consts::PI * len, 1e-3);
        // spline profile extrudes
        let mut sp = Sketch::default();
        sp.add_spline(&[[0.0, 0.0], [10.0, 6.0], [20.0, 0.0]], false);
        sp.add_line([20.0, 0.0], [0.0, 0.0], false);
        let reg = &sp.regions().unwrap()[0];
        let e = extrude(&region_face(&front, reg).unwrap(), [0.0, 0.0, 2.0]).unwrap();
        close(mp(&e).volume, reg.area() * 2.0, 2e-3);
        // draft the four side faces of a block by 5 degrees about its base
        let mut sk = Sketch::default();
        sk.add_rectangle([0.0, 0.0], [20.0, 20.0]);
        let block = extrude(&region_face(&top, &sk.regions().unwrap()[0]).unwrap(), [0.0, 10.0, 0.0]).unwrap();
        let m = tessellate(&block, 0.05, true);
        let sides: Vec<usize> = m.faces.iter().filter(|f| f.normal[1].abs() < 0.5).map(|f| f.index).collect();
        assert_eq!(sides.len(), 4);
        let d = draft(&block, &sides, [0.0, 1.0, 0.0], 5.0, [0.0; 3], [0.0, 1.0, 0.0]).unwrap();
        let v = mp(&d).volume;
        assert!(v < 4000.0 && v > 3000.0, "drafted volume {v}");
        match face_geom(&block, sides[0]).unwrap() {
            FaceGeom::Plane { normal, .. } => assert!(normal[1].abs() < 1e-9),
            g => panic!("{g:?}"),
        }
        let cyl = extrude(&region_face(&front, &prof.regions().unwrap()[0]).unwrap(), [0.0, 0.0, 5.0]).unwrap();
        let cm = tessellate(&cyl, 0.05, true);
        let side = cm.faces.iter().find(|f| f.kind == "curved").unwrap().index;
        match face_geom(&cyl, side).unwrap() {
            FaceGeom::Cylinder { radius, axis, .. } => {
                assert!((radius - 1.0).abs() < 1e-9 && axis[2].abs() > 0.999)
            }
            g => panic!("{g:?}"),
        }
        let both = compound(&[cyl.clone(), transform_matrix(&cyl, &translation([10.0, 0.0, 0.0])).unwrap()]);
        close(mp(&both).volume, 2.0 * std::f64::consts::PI * 5.0, 1e-6);
        assert!(overlap_volume(&cyl, &both) > 15.0);
    }

    #[cfg(feature = "occt")]
    #[test]
    fn hidden_line_views() {
        // 40 x 20 x 10 plate (x, y, z) with a through hole along z
        let s = plate_with_hole();
        let bb = |es: &[Edge2]| {
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for e in es {
                for p in &e.pts {
                    for k in 0..2 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
            }
            (lo, hi)
        };
        // looking down -Z: the hole is a visible circle
        let (vis, hid) = hlr(&s, ViewAxes { dir: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0] }).unwrap();
        let (lo, hi) = bb(&vis);
        assert!((lo[0]).abs() < 1e-6 && (hi[0] - 40.0).abs() < 1e-6 && (hi[1] - 20.0).abs() < 1e-6, "{lo:?} {hi:?}");
        assert!(vis.iter().any(|e| e.kind == "circle"));
        let _ = hid; // bottom edges lie exactly under visible ones; drawings filter those out
        // looking along -Y (side): the hole shows only as hidden lines
        let (vis, hid) = hlr(&s, ViewAxes { dir: [0.0, 1.0, 0.0], x: [1.0, 0.0, 0.0] }).unwrap();
        let (lo, hi) = bb(&vis);
        assert!((hi[0] - lo[0] - 40.0).abs() < 1e-6 && (hi[1] - lo[1] - 5.0).abs() < 1e-6, "{lo:?} {hi:?}");
        assert!(hid.len() >= 2, "hole silhouettes are hidden: {}", hid.len());
    }
}
