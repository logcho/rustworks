//! OpenCASCADE (OCCT 7.8) backend, written directly against `opencascade-sys`.
//!
//! OCCT is the industrial B-rep kernel behind FreeCAD and many commercial
//! tools: robust booleans (including coplanar contact), fillets, chamfers,
//! shelling, exact mass properties and STEP import/export.

use crate::{BoolOp, EdgeInfo, FaceInfo, MassProps, Mesh, Plane, UnionKind, Xform};
use anyhow::{bail, Result};
use cxx::UniquePtr;
use opencascade_sys as ffi;
use rw_sketch::{Loop, Region, Segment};

use ffi::top_abs::{TopAbs_Orientation, TopAbs_ShapeEnum};
use ffi::topo_ds::TopoDS_Shape;

/// A B-rep shape (solid or compound of solids).
pub struct Solid {
    inner: UniquePtr<TopoDS_Shape>,
}

impl Clone for Solid {
    fn clone(&self) -> Self {
        // TopoDS_Shape copies are cheap handles sharing the same geometry.
        Solid { inner: ffi::topo_ds::TopoDS_Shape_to_owned(&self.inner) }
    }
}

impl std::fmt::Debug for Solid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Solid(occt)")
    }
}

pub struct Face {
    inner: UniquePtr<ffi::topo_ds::TopoDS_Face>,
}

fn own(s: &TopoDS_Shape) -> Solid {
    Solid { inner: ffi::topo_ds::TopoDS_Shape_to_owned(s) }
}
fn pnt(p: [f64; 3]) -> UniquePtr<ffi::gp::gp_Pnt> {
    ffi::gp::new_point(p[0], p[1], p[2])
}
fn dir(d: [f64; 3]) -> UniquePtr<ffi::gp::gp_Dir> {
    ffi::gp::gp_Dir_new(d[0], d[1], d[2])
}
fn progress() -> UniquePtr<ffi::message::Message_ProgressRange> {
    ffi::message::Message_ProgressRange_new()
}

fn edge_line(a: [f64; 3], b: [f64; 3]) -> UniquePtr<ffi::topo_ds::TopoDS_Edge> {
    let mut mk = ffi::b_rep_builder_api::BRepBuilderAPI_MakeEdge_gp_Pnt_gp_Pnt(&pnt(a), &pnt(b));
    ffi::topo_ds::TopoDS_Edge_to_owned(mk.pin_mut().Edge())
}

fn edge_arc(a: [f64; 3], m: [f64; 3], b: [f64; 3]) -> UniquePtr<ffi::topo_ds::TopoDS_Edge> {
    let arc = ffi::gc::GC_MakeArcOfCircle_point_point_point(&pnt(a), &pnt(m), &pnt(b));
    let trimmed = ffi::gc::GC_MakeArcOfCircle_Value(&arc);
    let curve = ffi::geom::new_HandleGeomCurve_from_HandleGeom_TrimmedCurve(&trimmed);
    let mut mk = ffi::b_rep_builder_api::BRepBuilderAPI_MakeEdge_HandleGeomCurve(&curve);
    ffi::topo_ds::TopoDS_Edge_to_owned(mk.pin_mut().Edge())
}

fn loop_wire(plane: &Plane, lp: &Loop) -> Result<UniquePtr<ffi::topo_ds::TopoDS_Wire>> {
    let mut segs: Vec<Segment> = vec![];
    for s in &lp.segments {
        if let Segment::Circle { center, radius, ccw } = s {
            // Two half arcs; orientation follows `ccw` (holes are clockwise).
            let a = [center[0] + radius, center[1]];
            let b = [center[0] - radius, center[1]];
            segs.push(Segment::Arc { center: *center, a, b, ccw: *ccw });
            segs.push(Segment::Arc { center: *center, a: b, b: a, ccw: *ccw });
        } else {
            segs.push(s.clone());
        }
    }
    let mut mk = ffi::b_rep_builder_api::BRepBuilderAPI_MakeWire_new();
    for s in &segs {
        let (a, b) = (plane.to3d(s.start()), plane.to3d(s.end()));
        let e = match s {
            Segment::Line { .. } => edge_line(a, b),
            Segment::Arc { .. } => edge_arc(a, plane.to3d(s.mid()), b),
            Segment::Circle { .. } => unreachable!(),
        };
        mk.pin_mut().add_edge(&e);
    }
    if !mk.IsDone() {
        bail!("profile loop does not form a connected wire");
    }
    Ok(ffi::topo_ds::TopoDS_Wire_to_owned(mk.pin_mut().Wire()))
}

/// Planar face for a region (outer loop with holes).
pub fn region_face(plane: &Plane, region: &Region) -> Result<Face> {
    let outer = loop_wire(plane, &region.outer)?;
    let mut mk = ffi::b_rep_builder_api::BRepBuilderAPI_MakeFace_wire(&outer, true);
    for h in &region.holes {
        let w = loop_wire(plane, h)?;
        mk.pin_mut().add_wire(&w);
    }
    if !mk.IsDone() {
        bail!("could not build a planar face from the profile");
    }
    Ok(Face { inner: ffi::topo_ds::TopoDS_Face_to_owned(mk.Face()) })
}

fn face_shape(f: &Face) -> &TopoDS_Shape {
    ffi::topo_ds::cast_face_to_shape(&f.inner)
}

/// Exact signed volume.
fn volume(s: &TopoDS_Shape) -> f64 {
    let mut props = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::VolumeProperties(s, props.pin_mut(), false, false, false);
    props.Mass()
}

/// Linear sweep of a face along `dir`.
pub fn extrude(face: &Face, d: [f64; 3]) -> Result<Solid> {
    let v = ffi::gp::new_vec(d[0], d[1], d[2]);
    let mut mk = ffi::b_rep_prim_api::BRepPrimAPI_MakePrism_new(face_shape(face), &v, false, true);
    if !mk.IsDone() {
        bail!("extrude failed");
    }
    Ok(own(mk.pin_mut().Shape()))
}

/// Rotational sweep of a face. Angles ≥ 360° give a full revolve.
pub fn revolve(face: &Face, origin: [f64; 3], axis: [f64; 3], angle_deg: f64) -> Result<Solid> {
    let ax = ffi::gp::gp_Ax1_new(&pnt(origin), &dir(axis));
    let ang = if angle_deg.abs() >= 359.9999 { std::f64::consts::TAU } else { angle_deg.to_radians() };
    let mut mk = ffi::b_rep_prim_api::BRepPrimAPI_MakeRevol_new(face_shape(face), &ax, ang, false);
    if !mk.IsDone() {
        bail!("revolve failed (does the profile cross the axis?)");
    }
    Ok(own(mk.pin_mut().Shape()))
}

/// Merges coplanar/co-cylindrical faces and collinear edges left by booleans.
fn unify(s: &TopoDS_Shape) -> Solid {
    let mut u = ffi::shape_upgrade::UnifySameDomain_new(s, true, true, true);
    u.pin_mut().build();
    own(u.shape())
}

fn is_empty(s: &TopoDS_Shape) -> bool {
    let ex = ffi::top_exp::TopExp_Explorer_new(s, TopAbs_ShapeEnum::TopAbs_FACE);
    !ex.More()
}

pub fn boolean(a: &Solid, b: &Solid, op: BoolOp, _scale: f64) -> Result<Solid> {
    let out = match op {
        BoolOp::Union => {
            let mut f = ffi::b_rep_algo_api::BRepAlgoAPI_Fuse_new(&a.inner, &b.inner);
            if !f.IsDone() {
                bail!("boolean union failed");
            }
            unify(f.pin_mut().Shape())
        }
        BoolOp::Subtract => {
            let mut c = ffi::b_rep_algo_api::BRepAlgoAPI_Cut_new(&a.inner, &b.inner);
            if !c.IsDone() {
                bail!("boolean cut failed");
            }
            unify(c.pin_mut().Shape())
        }
        BoolOp::Intersect => {
            let mut c = ffi::b_rep_algo_api::BRepAlgoAPI_Common_new(&a.inner, &b.inner);
            if !c.IsDone() {
                bail!("boolean intersect failed");
            }
            unify(c.pin_mut().Shape())
        }
    };
    if op != BoolOp::Subtract && is_empty(&out.inner) {
        bail!("boolean {op:?} produced an empty result");
    }
    Ok(out)
}

/// OCCT fuses coplanar contact natively, so no overlap tricks are needed.
pub fn union_robust(a: &Solid, b: &Solid, _alt: Option<&Solid>, scale: f64) -> Result<(Solid, UnionKind)> {
    Ok((boolean(a, b, BoolOp::Union, scale)?, UnionKind::Fused))
}

fn apply_trsf(s: &TopoDS_Shape, t: &ffi::gp::gp_Trsf) -> Solid {
    let mut mk = ffi::b_rep_builder_api::BRepBuilderAPI_Transform_new(s, t, true);
    own(mk.pin_mut().Shape())
}

pub fn transform(s: &Solid, x: &Xform) -> Solid {
    match *x {
        Xform::Translate(d) => {
            let mut t = ffi::gp::new_transform();
            t.pin_mut().set_translation_vec(&ffi::gp::new_vec(d[0], d[1], d[2]));
            apply_trsf(&s.inner, &t)
        }
        Xform::Rotate { origin, axis, deg } => {
            let mut t = ffi::gp::new_transform();
            t.pin_mut().SetRotation(&ffi::gp::gp_Ax1_new(&pnt(origin), &dir(axis)), deg.to_radians());
            apply_trsf(&s.inner, &t)
        }
        Xform::Mirror { origin, normal } => {
            // Plane reflection = point reflection through `origin` followed by a
            // half turn about the plane normal (both are rigid gp_Trsf ops).
            let mut p = ffi::gp::new_transform();
            p.pin_mut().SetScale(&pnt(origin), -1.0);
            let s1 = apply_trsf(&s.inner, &p);
            let mut r = ffi::gp::new_transform();
            r.pin_mut().SetRotation(&ffi::gp::gp_Ax1_new(&pnt(origin), &dir(normal)), std::f64::consts::PI);
            apply_trsf(&s1.inner, &r)
        }
    }
}

fn indexed(s: &TopoDS_Shape, kind: TopAbs_ShapeEnum) -> UniquePtr<ffi::top_tools::TopTools_IndexedMapOfShape> {
    let mut map = ffi::top_tools::new_indexed_map_of_shape();
    ffi::top_exp::TopExp::MapShapes(s, kind, map.pin_mut());
    map
}

/// Bounding box from the shape's vertices and edge samples.
fn bbox(s: &Solid) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for e in edge_polylines(s, 0.2) {
        for p in e.1 {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k] as f64);
                hi[k] = hi[k].max(p[k] as f64);
            }
        }
    }
    (lo, hi)
}

pub fn scale_of(s: &Solid) -> f64 {
    let (lo, hi) = bbox(s);
    if !lo[0].is_finite() {
        return 1.0;
    }
    ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt().max(1e-6)
}

pub fn extent_along(s: &Solid, origin: [f64; 3], d: [f64; 3]) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for (_, pl, _) in edge_polylines(s, 0.1) {
        for p in pl {
            let v = (p[0] as f64 - origin[0]) * d[0] + (p[1] as f64 - origin[1]) * d[1] + (p[2] as f64 - origin[2]) * d[2];
            best = best.max(v);
        }
    }
    if !best.is_finite() {
        return scale_of(s);
    }
    best + scale_of(s) * 0.01 + 1e-3
}

/// (edge index, polyline) for every unique edge with a 3D curve.
fn edge_polylines(s: &Solid, angular: f64) -> Vec<(usize, Vec<[f32; 3]>, ffi::geom_abs::GeomAbs_CurveType)> {
    let map = indexed(&s.inner, TopAbs_ShapeEnum::TopAbs_EDGE);
    let mut out = vec![];
    for i in 1..=map.Extent() {
        let edge = ffi::topo_ds::TopoDS::Edge(map.FindKey(i));
        let (mut f, mut l) = (0.0, 0.0);
        let curve = ffi::b_rep::BRep_Tool_Curve(edge, &mut f, &mut l);
        if curve.IsNull() {
            continue; // degenerated edge (e.g. apex of a revolve)
        }
        let ad = ffi::b_rep_adaptor::BRepAdaptor_Curve_new(edge);
        let kind = ad.GetType();
        let td = ffi::gc_pnts::TangentialDeflection_new(&ad, angular, 0.01);
        let n = td.NbPoints();
        let mut pl = Vec::with_capacity(n as usize);
        for k in 1..=n {
            let p = ffi::gc_pnts::GCPnts_TangentialDeflection_Value(&td, k);
            pl.push([p.X() as f32, p.Y() as f32, p.Z() as f32]);
        }
        if pl.len() >= 2 {
            out.push(((i - 1) as usize, pl, kind));
        }
    }
    out
}

/// All unique edges of the solid, with geometry summaries for selection.
pub fn edges(s: &Solid) -> Vec<EdgeInfo> {
    edge_polylines(s, 0.1)
        .into_iter()
        .map(|(index, pl, kind)| {
            let len: f64 = pl
                .windows(2)
                .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt() as f64)
                .sum();
            // point halfway along the polyline
            let mut acc = 0.0;
            let mut mid = pl[0];
            for w in pl.windows(2) {
                let d = ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2) + (w[1][2] - w[0][2]).powi(2)).sqrt() as f64;
                if acc + d >= len / 2.0 {
                    let t = ((len / 2.0 - acc) / d.max(1e-12)) as f32;
                    mid = [w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t, w[0][2] + (w[1][2] - w[0][2]) * t];
                    break;
                }
                acc += d;
            }
            let k = match kind {
                ffi::geom_abs::GeomAbs_CurveType::GeomAbs_Line => "line",
                ffi::geom_abs::GeomAbs_CurveType::GeomAbs_Circle => "circle",
                _ => "curve",
            };
            EdgeInfo {
                index,
                kind: k,
                start: pl[0].map(|v| v as f64),
                end: pl[pl.len() - 1].map(|v| v as f64),
                mid: mid.map(|v| v as f64),
                length: len,
                polyline: pl,
            }
        })
        .collect()
}

fn edge_by_index(s: &Solid, idx: &[usize]) -> Result<Vec<UniquePtr<ffi::topo_ds::TopoDS_Edge>>> {
    let map = indexed(&s.inner, TopAbs_ShapeEnum::TopAbs_EDGE);
    idx.iter()
        .map(|&i| {
            if i as i32 >= map.Extent() {
                bail!("edge {i} does not exist");
            }
            Ok(ffi::topo_ds::TopoDS_Edge_to_owned(ffi::topo_ds::TopoDS::Edge(map.FindKey(i as i32 + 1))))
        })
        .collect()
}

pub fn fillet(s: &Solid, edge_idx: &[usize], radius: f64) -> Result<Solid> {
    if radius <= 0.0 {
        bail!("fillet radius must be positive");
    }
    let es = edge_by_index(s, edge_idx)?;
    let mut mk = ffi::b_rep_fillet_api::BRepFilletAPI_MakeFillet_new(&s.inner);
    for e in &es {
        mk.pin_mut().add_edge(radius, e);
    }
    mk.pin_mut().Build(&progress());
    if !mk.IsDone() {
        bail!("fillet failed (radius too large for the adjacent faces?)");
    }
    Ok(own(mk.pin_mut().Shape()))
}

pub fn chamfer(s: &Solid, edge_idx: &[usize], distance: f64) -> Result<Solid> {
    if distance <= 0.0 {
        bail!("chamfer distance must be positive");
    }
    let es = edge_by_index(s, edge_idx)?;
    let mut mk = ffi::b_rep_fillet_api::BRepFilletAPI_MakeChamfer_new(&s.inner);
    for e in &es {
        mk.pin_mut().add_edge(distance, e);
    }
    mk.pin_mut().Build(&progress());
    if !mk.IsDone() {
        bail!("chamfer failed (distance too large?)");
    }
    Ok(own(mk.pin_mut().Shape()))
}

/// Hollow the solid with wall `thickness`, opening the given faces.
pub fn shell(s: &Solid, open_faces: &[usize], thickness: f64) -> Result<Solid> {
    if thickness <= 0.0 {
        bail!("shell thickness must be positive");
    }
    let map = indexed(&s.inner, TopAbs_ShapeEnum::TopAbs_FACE);
    let mut list = ffi::top_tools::new_list_of_shape();
    for &i in open_faces {
        if i as i32 >= map.Extent() {
            bail!("face {i} does not exist");
        }
        list.pin_mut().Append(map.FindKey(i as i32 + 1));
    }
    let mut mk = ffi::b_rep_offset_api::BRepOffsetAPI_MakeThickSolid_new();
    mk.pin_mut().MakeThickSolidByJoin(
        &s.inner,
        &list,
        -thickness,
        1e-4,
        ffi::b_rep_offset_api::BRepOffset_Mode::BRepOffset_Skin,
        false,
        false,
        ffi::geom_abs::GeomAbs_JoinType::GeomAbs_Arc,
        false,
        &progress(),
    );
    let out = own(mk.pin_mut().Shape());
    if is_empty(&out.inner) || volume(&out.inner).abs() < 1e-9 {
        bail!("shell failed (wall too thick for the part's features?)");
    }
    // Validate: every opened face must actually be gone. OCCT can return a
    // malformed result when an opened face is tangent to fillets.
    let before = tessellate(s, (scale_of(s) / 300.0).max(0.01), false);
    let after = tessellate(&out, (scale_of(s) / 300.0).max(0.01), false);
    for &fi in open_faces {
        if let Some(p) = point_on_face(&before, fi) {
            if crate::face_near(&after, p).is_some_and(|(_, d)| d < thickness * 0.25) {
                bail!("shell could not open the face: faces tangent to it (e.g. fillets) confuse the kernel; move the Shell before those fillets in the feature tree");
            }
        }
    }
    Ok(out)
}

/// Triangulates the solid. Each triangle remembers its B-rep face index.
pub fn tessellate(s: &Solid, tol: f64, with_edges: bool) -> Mesh {
    let _mesher = ffi::b_rep_mesh::IncrementalMesh_new(&s.inner, tol.max(1e-4));
    let faces = indexed(&s.inner, TopAbs_ShapeEnum::TopAbs_FACE);
    let mut m = Mesh::default();
    for fi in 1..=faces.Extent() {
        let face = ffi::topo_ds::TopoDS::Face(faces.FindKey(fi));
        let mut loc = ffi::top_loc::Location_new();
        let h = ffi::b_rep::BRep_Tool_Triangulation(face, loc.pin_mut());
        let Ok(tri) = ffi::poly::Handle_Poly_Triangulation_Get(&h) else { continue };
        let trsf = ffi::top_loc::TopLoc_Location_Transformation(&loc);
        let base = m.positions.len() as u32;
        let n = tri.NbNodes();
        let mut pts = Vec::with_capacity(n as usize);
        for i in 1..=n {
            let mut p = ffi::poly::Poly_Triangulation_Node(tri, i);
            p.pin_mut().Transform(&trsf);
            pts.push([p.X(), p.Y(), p.Z()]);
        }
        let reversed = face.Orientation() == TopAbs_Orientation::TopAbs_REVERSED;
        let mut nrm = vec![[0.0f64; 3]; pts.len()];
        let mut area = 0.0;
        let mut cen = [0.0; 3];
        let mut nsum = [0.0; 3];
        let fidx = (fi - 1) as u32;
        for t in 1..=tri.NbTriangles() {
            let tr = tri.Triangle(t);
            let mut ids = [tr.Value(1) - 1, tr.Value(2) - 1, tr.Value(3) - 1].map(|v| v as usize);
            if reversed {
                ids.swap(1, 2);
            }
            let [a, b, c] = [pts[ids[0]], pts[ids[1]], pts[ids[2]]];
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cr = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let ar = 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
            area += ar;
            for k in 0..3 {
                cen[k] += (a[k] + b[k] + c[k]) / 3.0 * ar;
                nsum[k] += cr[k] * 0.5;
                for &id in &ids {
                    nrm[id][k] += cr[k];
                }
            }
            m.indices.extend(ids.iter().map(|&i| base + i as u32));
            m.tri_face.push(fidx);
        }
        for (p, nv) in pts.iter().zip(&nrm) {
            let l = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt().max(1e-30);
            m.positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            m.normals.push([(nv[0] / l) as f32, (nv[1] / l) as f32, (nv[2] / l) as f32]);
        }
        if with_edges {
            let nl = (nsum[0] * nsum[0] + nsum[1] * nsum[1] + nsum[2] * nsum[2]).sqrt().max(1e-30);
            let n0 = [nsum[0] / nl, nsum[1] / nl, nsum[2] / nl];
            let planar = nrm.iter().all(|nv| {
                let l = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt().max(1e-30);
                (nv[0] * n0[0] + nv[1] * n0[1] + nv[2] * n0[2]) / l > 1.0 - 1e-6
            });
            let c = if area > 0.0 { cen.map(|v| v / area) } else { cen };
            m.faces.push(FaceInfo { index: fidx as usize, kind: if planar { "plane" } else { "curved" }, area, centroid: c, normal: n0 });
        }
    }
    if with_edges {
        m.edges = edge_polylines(s, 0.08).into_iter().map(|e| e.1).collect();
        m.edge_ids = edge_polylines(s, 0.08).into_iter().map(|e| e.0 as u32).collect();
    }
    m
}

/// Exact mass properties from the B-rep (bbox from a tessellation).
pub fn mass_props(s: &Solid, mesh: &Mesh) -> MassProps {
    let mut vp = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::VolumeProperties(&s.inner, vp.pin_mut(), false, false, false);
    let mut sp = ffi::g_prop::GProps_new();
    ffi::b_rep_g_prop::BRepGProp::SurfaceProperties(&s.inner, sp.pin_mut(), false, false);
    let c = ffi::g_prop::GProp_GProps_CentreOfMass(&vp);
    let approx = mesh.mass_props();
    MassProps {
        volume: vp.Mass(),
        surface_area: sp.Mass(),
        centroid: [c.X(), c.Y(), c.Z()],
        bbox_min: approx.bbox_min,
        bbox_max: approx.bbox_max,
    }
}

pub fn write_step(s: &Solid, path: &std::path::Path) -> Result<()> {
    let mut w = ffi::step_control::STEPControl_Writer_new();
    let st = ffi::step_control::transfer_shape(w.pin_mut(), &s.inner);
    if st != ffi::if_select::IFSelect_ReturnStatus::IFSelect_RetDone {
        bail!("STEP transfer failed");
    }
    let st = ffi::step_control::write_step(w.pin_mut(), path.display().to_string());
    if st != ffi::if_select::IFSelect_ReturnStatus::IFSelect_RetDone {
        bail!("writing STEP file failed");
    }
    Ok(())
}

pub fn read_step(path: &std::path::Path) -> Result<Solid> {
    if !path.exists() {
        bail!("{} not found", path.display());
    }
    let mut r = ffi::step_control::STEPControl_Reader_new();
    let st = ffi::step_control::read_step(r.pin_mut(), path.display().to_string());
    if st != ffi::if_select::IFSelect_ReturnStatus::IFSelect_RetDone {
        bail!("could not read STEP file {}", path.display());
    }
    r.pin_mut().TransferRoots(&progress());
    let s = ffi::step_control::one_shape_step(&r);
    if s.IsNull() || is_empty(&s) {
        bail!("STEP file contains no solid geometry");
    }
    Ok(Solid { inner: s })
}


/// Solid lofted through closed section loops (in order).
pub fn loft(sections: &[(Plane, Loop)]) -> Result<Solid> {
    let mut mk = ffi::b_rep_offset_api::BRepOffsetAPI_ThruSections_new(true);
    for (pl, lp) in sections {
        let w = loop_wire(pl, lp)?;
        mk.pin_mut().AddWire(&w);
    }
    mk.pin_mut().CheckCompatibility(false);
    mk.pin_mut().Build(&progress());
    if !mk.IsDone() {
        bail!("loft failed (are the profiles compatible?)");
    }
    Ok(own(mk.pin_mut().Shape()))
}

/// A point on face `fi` (centroid of its largest triangle).
fn point_on_face(m: &Mesh, fi: usize) -> Option<[f64; 3]> {
    let mut best: Option<(f64, [f64; 3])> = None;
    for (t, tri) in m.indices.chunks(3).enumerate() {
        if m.tri_face[t] as usize != fi {
            continue;
        }
        let p = [tri[0], tri[1], tri[2]].map(|i| m.positions[i as usize].map(|v| v as f64));
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let c = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let a = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        if best.is_none_or(|b| a > b.0) {
            best = Some((a, [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0)));
        }
    }
    best.map(|b| b.1)
}
