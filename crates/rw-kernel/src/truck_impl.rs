//! Pure-Rust `truck` backend (enable with `--no-default-features --features truck`).
//! No fillet/chamfer/shell/STEP import; booleans are slower and less robust
//! than the OpenCASCADE backend. Kept for builds without a C++ toolchain.

use crate::{BoolOp, EdgeInfo, FaceGeom, FaceInfo, MassProps, Mesh, Plane, UnionKind, Xform};
use anyhow::{anyhow, bail, Result};
use rw_sketch::{Loop, Region, Segment};
use std::collections::HashSet;
use truck_meshalgo::prelude::*;
use truck_modeling::*;

pub use truck_modeling::{Face, Solid};

fn p3(a: [f64; 3]) -> Point3 {
    Point3::new(a[0], a[1], a[2])
}

fn loop_wire(plane: &Plane, lp: &Loop) -> Wire {
    let mut edges: Vec<Edge> = vec![];
    // Split full circles into two arcs for robust topology.
    let mut segs: Vec<Segment> = vec![];
    for s in &lp.segments {
        if let Segment::Circle { center, radius, ccw } = s {
            // Seam angle derived from the circle itself: deterministic, but avoids
            // systematically aligned seams (e.g. every hole on the X axis lining
            // up with the parent disk's seam), which trips truck's booleans.
            let t = seam_angle(plane.to3d(*center), *radius);
            let (st, ct) = t.sin_cos();
            let a = [center[0] + radius * ct, center[1] + radius * st];
            let b = [center[0] - radius * ct, center[1] - radius * st];
            segs.push(Segment::Arc { center: *center, a, b, ccw: *ccw });
            segs.push(Segment::Arc { center: *center, a: b, b: a, ccw: *ccw });
        } else if let Segment::Spline { .. } = s {
            // truck: approximate splines by short line segments
            let mut pts = vec![];
            s.sample(&mut pts, 0);
            pts.push(s.end());
            for w in pts.windows(2) {
                segs.push(Segment::Line { a: w[0], b: w[1] });
            }
        } else {
            segs.push(s.clone());
        }
    }
    let verts: Vec<Vertex> = segs.iter().map(|s| builder::vertex(p3(plane.to3d(s.start())))).collect();
    let n = segs.len();
    for (i, s) in segs.iter().enumerate() {
        let (v0, v1) = (&verts[i], &verts[(i + 1) % n]);
        let e = match s {
            Segment::Line { .. } => builder::line(v0, v1),
            Segment::Arc { .. } => builder::circle_arc(v0, v1, p3(plane.to3d(s.mid()))),
            Segment::Circle { .. } | Segment::Spline { .. } => unreachable!(),
        };
        edges.push(e);
    }
    edges.into_iter().collect()
}

fn seam_angle(c: [f64; 3], r: f64) -> f64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in [c[0], c[1], c[2], r] {
        for byte in ((v * 1e6).round() as i64).to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    (h % 3600) as f64 / 3600.0 * std::f64::consts::TAU
}

/// Planar face for a region (outer loop with holes).
pub fn region_face(plane: &Plane, region: &Region) -> Result<Face> {
    let mut wires = vec![loop_wire(plane, &region.outer)];
    for h in &region.holes {
        wires.push(loop_wire(plane, h));
    }
    builder::try_attach_plane(&wires).map_err(|e| anyhow!("could not build face from profile: {e:?}"))
}

/// Ensures the solid has outward-facing orientation (positive volume).
fn orient(mut s: Solid) -> Solid {
    let m = tessellate(&s, 0.5, false);
    if m.mass_props().volume < 0.0 {
        s.not();
    }
    s
}

/// Linear sweep of a face along `dir`.
pub fn extrude(face: &Face, dir: [f64; 3]) -> Result<Solid> {
    Ok(orient(builder::tsweep(face, Vector3::from(dir))))
}

/// Rotational sweep of a face about an axis. Angles ≥ 360° give a full revolve.
pub fn revolve(face: &Face, origin: [f64; 3], axis: [f64; 3], angle_deg: f64) -> Result<Solid> {
    let ang = if angle_deg.abs() >= 359.9999 { 7.0 } else { angle_deg.to_radians() };
    Ok(orient(builder::rsweep(face, p3(origin), Vector3::from(axis).normalize(), Rad(ang))))
}

/// Robust boolean: retries with a few intersection tolerances since truck's
/// intersection curves are tolerance-sensitive.
pub fn boolean(a: &Solid, b: &Solid, op: BoolOp, scale: f64) -> Result<Solid> {
    let base = (scale * 2e-3).clamp(2e-3, 0.5);
    let mut b2 = b.clone();
    if op == BoolOp::Subtract {
        b2.not();
    }
    for f in [1.0, 2.0, 0.5, 4.0] {
        let tol = base * f;
        let t = std::time::Instant::now();
        // truck can panic on degenerate intersections; isolate it.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match op {
            BoolOp::Union => truck_shapeops::or(a, &b2, tol),
            BoolOp::Subtract | BoolOp::Intersect => truck_shapeops::and(a, &b2, tol),
        }))
        .ok()
        .flatten();
        if std::env::var("RW_DEBUG").is_ok() {
            eprintln!("  boolean {op:?} tol={tol:.4} ok={} {:.0}ms", r.is_some(), t.elapsed().as_secs_f64() * 1e3);
        }
        if let Some(s) = r {
            if s.boundaries().iter().any(|sh| !sh.is_empty()) {
                return Ok(s);
            }
        }
    }
    // Last resort (subtractions only): nudge the tool by ~1e-5 of the part size. This breaks exact
    // seam/vertex alignments that trip truck's intersection code, at the cost of
    // a sub-micron positional change on typical parts.
    // Unions are not nudged: a nudged coplanar union yields sliver topology;
    // `union_robust` handles those with an overlapping tool instead.
    let nudges: &[[f64; 3]] = if op == BoolOp::Subtract {
        &[[1.0, 0.618, 0.382], [-0.382, 1.0, -0.618], [0.618, -0.382, 1.0]]
    } else {
        &[]
    };
    for dir in nudges {
        let d = scale * 1e-5;
        let mut nudged = transform_m(b, crate::translation([dir[0] * d, dir[1] * d, dir[2] * d]));
        if op == BoolOp::Subtract {
            nudged.not();
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match op {
            BoolOp::Union => truck_shapeops::or(a, &nudged, base),
            BoolOp::Subtract | BoolOp::Intersect => truck_shapeops::and(a, &nudged, base),
        }))
        .ok()
        .flatten();
        if std::env::var("RW_DEBUG").is_ok() {
            eprintln!("  boolean {op:?} nudged ok={}", r.is_some());
        }
        if let Some(s) = r {
            if s.boundaries().iter().any(|sh| !sh.is_empty()) {
                return Ok(s);
            }
        }
    }
    bail!("boolean {op:?} failed (geometry may be tangent or coplanar; try offsetting slightly)")
}

/// Union that copes with truck's weakness on coplanar contact:
/// 1. exact union; 2. union with `alt` (the same tool extended a hair into the
/// existing material); 3. if the bodies merely touch or are disjoint, merge
/// their shells into one multi-body solid.
pub fn union_robust(a: &Solid, b: &Solid, alt: Option<&Solid>, scale: f64) -> Result<(Solid, UnionKind)> {
    if let Ok(s) = boolean(a, b, BoolOp::Union, scale) {
        return Ok((s, UnionKind::Fused));
    }
    if let Some(alt) = alt {
        if let Ok(s) = boolean(a, alt, BoolOp::Union, scale) {
            return Ok((s, UnionKind::FusedWithOverlap));
        }
    }
    if touching_or_disjoint(a, b, scale) {
        let mut shells: Vec<Shell> = a.boundaries().clone();
        shells.extend(b.boundaries().iter().cloned());
        return Ok((Solid::new(shells), UnionKind::MergedTouching));
    }
    bail!("boolean union failed (bodies overlap with coincident faces)")
}

fn bbox(s: &Solid, scale: f64) -> ([f64; 3], [f64; 3]) {
    let mp = tessellate(s, (scale / 100.0).max(1e-3), false).mass_props();
    (mp.bbox_min, mp.bbox_max)
}

/// True if the bounding boxes are disjoint or overlap with zero thickness.
fn touching_or_disjoint(a: &Solid, b: &Solid, scale: f64) -> bool {
    let (alo, ahi) = bbox(a, scale);
    let (blo, bhi) = bbox(b, scale);
    let tol = (scale * 1e-6).max(1e-7);
    let mut thinnest = f64::INFINITY;
    for k in 0..3 {
        let t = ahi[k].min(bhi[k]) - alo[k].max(blo[k]);
        thinnest = thinnest.min(t);
    }
    thinnest <= tol
}

/// Applies a row-major 4x4 affine transform.
pub fn transform(s: &Solid, x: &Xform) -> Solid {
    transform_m(s, x.matrix())
}

fn transform_m(s: &Solid, m: [[f64; 4]; 4]) -> Solid {
    // cgmath matrices are column-major.
    let mat = Matrix4::new(
        m[0][0], m[1][0], m[2][0], m[3][0],
        m[0][1], m[1][1], m[2][1], m[3][1],
        m[0][2], m[1][2], m[2][2], m[3][2],
        m[0][3], m[1][3], m[2][3], m[3][3],
    );
    let det = mat.determinant();
    let mut out = builder::transformed(s, mat);
    if det < 0.0 {
        out.not();
    }
    out
}

/// Approximate size of the solid (bounding-box diagonal of its vertices).
pub fn scale_of(s: &Solid) -> f64 {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for sh in s.boundaries() {
        for v in sh.vertex_iter() {
            let p = v.point();
            for (i, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[i] = lo[i].min(c);
                hi[i] = hi[i].max(c);
            }
        }
    }
    if !lo[0].is_finite() {
        return 1.0;
    }
    ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt().max(1e-6)
}

/// Farthest signed distance of the solid's surface from `origin` along `dir`
/// (unit), plus a small safety margin. Used to size "through all" tools tightly,
/// which keeps booleans fast.
pub fn extent_along(s: &Solid, origin: [f64; 3], dir: [f64; 3]) -> f64 {
    let sc = scale_of(s);
    let tol = (sc / 40.0).max(1e-3);
    let m = tessellate(s, tol, false);
    let mut best = f64::NEG_INFINITY;
    for p in &m.positions {
        let d = (p[0] as f64 - origin[0]) * dir[0] + (p[1] as f64 - origin[1]) * dir[1] + (p[2] as f64 - origin[2]) * dir[2];
        best = best.max(d);
    }
    if !best.is_finite() {
        return sc;
    }
    best + 2.0 * tol + sc * 0.01
}

/// Tessellates `solid` with chordal tolerance `tol`.
/// Tessellates `solid` with chordal tolerance `tol`. With `with_edges`,
/// also extracts B-rep edges and per-face info.
pub fn tessellate(solid: &Solid, tol: f64, with_edges: bool) -> Mesh {
    let meshed = solid.triangulation(tol);
    // The fast tessellator occasionally fails on trimmed faces produced by
    // booleans; fall back to the robust (projecting) tessellator for those.
    let failed = meshed.boundaries().iter().flat_map(|s| s.face_iter()).any(|f| f.surface().is_none());
    let robust = if failed {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| solid.robust_triangulation(tol))).ok()
    } else {
        None
    };
    let robust_faces: Vec<Option<PolygonMesh>> = robust
        .as_ref()
        .map(|r| r.boundaries().iter().flat_map(|s| s.face_iter()).map(|f| f.surface()).collect())
        .unwrap_or_default();
    let mut m = Mesh::default();
    let mut face_idx = 0u32;
    for shell in meshed.boundaries() {
        for face in shell.face_iter() {
            let surf = face.surface().or_else(|| robust_faces.get(face_idx as usize).cloned().flatten());
            let Some(mut pm) = surf else {
                if std::env::var("RW_DEBUG").is_ok() {
                    eprintln!("face {face_idx}: tessellation failed");
                }
                face_idx += 1;
                continue;
            };
            if !face.orientation() {
                pm.invert();
            }
            let base = m.positions.len() as u32;
            let pos = pm.positions();
            let nor = pm.normals();
            // Expand to unique (pos, normal) vertices per face.
            let mut verts: std::collections::HashMap<(usize, Option<usize>), u32> = Default::default();
            let mut push = |m: &mut Mesh, v: &StandardVertex| -> u32 {
                *verts.entry((v.pos, v.nor)).or_insert_with(|| {
                    let p = pos[v.pos];
                    m.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                    let n = v.nor.filter(|&i| i < nor.len()).map(|i| nor[i]).unwrap_or(Vector3::new(0.0, 0.0, 0.0));
                    m.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                    (m.positions.len() - 1) as u32
                })
            };
            let mut area = 0.0;
            let mut cen = Vector3::new(0.0, 0.0, 0.0);
            let mut nsum = Vector3::new(0.0, 0.0, 0.0);
            for tri in pm.faces().triangle_iter() {
                let ids = [push(&mut m, &tri[0]), push(&mut m, &tri[1]), push(&mut m, &tri[2])];
                let [a, b, c] = [pos[tri[0].pos], pos[tri[1].pos], pos[tri[2].pos]];
                let cr = (b - a).cross(c - a);
                let ar = cr.magnitude() * 0.5;
                area += ar;
                cen += (a.to_vec() + b.to_vec() + c.to_vec()) / 3.0 * ar;
                nsum += cr * 0.5;
                m.indices.extend_from_slice(&ids);
                m.tri_face.push(face_idx);
            }
            let _ = base;
            // Fill missing normals with face normals.
            if with_edges {
                let planar = {
                    let n0 = nsum.normalize();
                    nor.iter().all(|n| n.normalize().dot(n0) > 1.0 - 1e-6)
                };
                let c = if area > 0.0 { cen / area } else { cen };
                let n = if nsum.magnitude2() > 0.0 { nsum.normalize() } else { nsum };
                m.faces.push(FaceInfo {
                    index: face_idx as usize,
                    kind: if planar { "plane" } else { "curved" },
                    area,
                    centroid: [c.x, c.y, c.z],
                    normal: [n.x, n.y, n.z],
                });
            }
            face_idx += 1;
        }
    }
    m.fix_normals();
    if with_edges {
        let mut seen = HashSet::new();
        for shell in meshed.boundaries() {
            for edge in shell.edge_iter() {
                if !seen.insert(edge.id()) {
                    continue;
                }
                let pl: Vec<[f32; 3]> = edge.curve().iter().map(|p| [p.x as f32, p.y as f32, p.z as f32]).collect();
                if pl.len() >= 2 {
                    m.edge_ids.push(m.edges.len() as u32);
                    m.edges.push(pl);
                }
            }
        }
    }
    m
}


pub fn mass_props(_s: &Solid, mesh: &Mesh) -> MassProps {
    mesh.mass_props()
}

pub fn write_step(s: &Solid, path: &std::path::Path) -> Result<()> {
    let compressed = s.compress();
    let txt = truck_stepio::out::CompleteStepDisplay::new(truck_stepio::out::StepModel::from(&compressed), Default::default()).to_string();
    std::fs::write(path, txt)?;
    Ok(())
}

pub fn read_step(_path: &std::path::Path) -> Result<Solid> {
    bail!("STEP import requires the OpenCASCADE backend")
}

pub fn fillet(_s: &Solid, _edges: &[usize], _r: f64) -> Result<Solid> {
    bail!("fillet requires the OpenCASCADE backend")
}

pub fn chamfer(_s: &Solid, _edges: &[usize], _d: f64) -> Result<Solid> {
    bail!("chamfer requires the OpenCASCADE backend")
}

pub fn shell(_s: &Solid, _faces: &[usize], _t: f64) -> Result<Solid> {
    bail!("shell requires the OpenCASCADE backend")
}

pub fn edges(s: &Solid) -> Vec<EdgeInfo> {
    let m = tessellate(s, (scale_of(s) / 500.0).max(1e-3), true);
    m.edges.iter().zip(&m.edge_ids).map(|(pl, id)| EdgeInfo::from_polyline(*id as usize, "curve", pl.clone())).collect()
}

pub fn loft(_sections: &[(Plane, Loop)]) -> Result<Solid> {
    bail!("loft requires the OpenCASCADE backend")
}

pub fn sweep(_pp: &Plane, _r: &Region, _sp: &Plane, _path: &Loop) -> Result<Solid> {
    bail!("sweep requires the OpenCASCADE backend")
}

pub fn draft(_s: &Solid, _f: &[usize], _pull: [f64; 3], _a: f64, _o: [f64; 3], _n: [f64; 3]) -> Result<Solid> {
    bail!("draft requires the OpenCASCADE backend")
}

pub fn face_geom(_s: &Solid, _fi: usize) -> Result<FaceGeom> {
    Ok(FaceGeom::Other)
}

pub fn compound(_solids: &[Solid]) -> Solid {
    panic!("assemblies require the OpenCASCADE backend")
}

pub fn transform_matrix(_s: &Solid, _m: &[[f64; 4]; 4]) -> Result<Solid> {
    bail!("assemblies require the OpenCASCADE backend")
}

pub fn overlap_volume(_a: &Solid, _b: &Solid) -> f64 {
    0.0
}
