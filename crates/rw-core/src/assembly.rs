//! Assemblies: placed part components positioned by mates.
//!
//! A `.rwasm` file lists components (part files plus a placement) and mates
//! between geometry on them (faces, origin planes/axes, points). Rebuilding
//! regenerates each referenced part (cached by file mtime), resolves mate
//! references to exact face geometry, and solves all component placements at
//! once with a Levenberg–Marquardt least-squares solver (6 DOF per free
//! component), reporting remaining degrees of freedom like the sketch solver.

use crate::doc::StdPlane;
use crate::regen::Regenerator;
use anyhow::{anyhow, bail, Context, Result};
use rw_kernel::{FaceGeom, Mesh, Solid};
use rw_sketch::{Scope, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

pub const ASM_FORMAT: &str = "rustworks-assembly/1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Assembly {
    #[serde(default = "asm_fmt")]
    pub format: String,
    pub name: String,
    /// Global parameters usable in mate values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub mates: Vec<Mate>,
}

fn asm_fmt() -> String {
    ASM_FORMAT.into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Component {
    pub id: String,
    pub name: String,
    /// Part file, relative to the assembly file's folder (or absolute).
    pub path: String,
    /// World position of the part origin (mm).
    #[serde(default)]
    pub position: [f64; 3],
    /// Orientation as a rotation vector: axis × angle in degrees.
    #[serde(default)]
    pub rotation: [f64; 3],
    /// Fixed components are not moved by mates.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MateKind {
    Coincident,
    Concentric,
    Distance,
    Parallel,
    Perpendicular,
    Angle,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AxisName {
    X,
    Y,
    Z,
}

/// Geometry on a component, in the part's own coordinates. Exactly one of
/// `face` (a point on a face), `plane`, `axis` or `point` should be set.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MateRef {
    pub component: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plane: Option<StdPlane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<AxisName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mate {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: MateKind,
    pub a: MateRef,
    pub b: MateRef,
    /// Distance (mm) or angle (deg) for distance/angle mates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Planar faces: keep normals pointing the same way instead of face-to-face.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aligned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

impl Assembly {
    pub fn new(name: &str) -> Assembly {
        Assembly { format: ASM_FORMAT.into(), name: name.into(), parameters: BTreeMap::new(), components: vec![], mates: vec![] }
    }
    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id || c.name == id)
    }
    pub fn component_mut(&mut self, id: &str) -> Option<&mut Component> {
        self.components.iter_mut().find(|c| c.id == id || c.name == id)
    }
    /// Fresh (id, display name) for a component of the given part name, e.g. ("c3", "bracket<2>").
    pub fn fresh_component(&self, part: &str) -> (String, String) {
        let mut n = self.components.len() + 1;
        let id = loop {
            let id = format!("c{n}");
            if self.components.iter().all(|c| c.id != id) {
                break id;
            }
            n += 1;
        };
        let mut k = 1;
        let name = loop {
            let nm = format!("{part}<{k}>");
            if self.components.iter().all(|c| c.name != nm) {
                break nm;
            }
            k += 1;
        };
        (id, name)
    }
    pub fn fresh_mate(&self, kind: MateKind) -> (String, String) {
        let mut n = self.mates.len() + 1;
        let id = loop {
            let id = format!("m{n}");
            if self.mates.iter().all(|m| m.id != id) {
                break id;
            }
            n += 1;
        };
        let base = match kind {
            MateKind::Coincident => "Coincident",
            MateKind::Concentric => "Concentric",
            MateKind::Distance => "Distance",
            MateKind::Parallel => "Parallel",
            MateKind::Perpendicular => "Perpendicular",
            MateKind::Angle => "Angle",
        };
        let mut k = 1;
        let name = loop {
            let nm = format!("{base}{k}");
            if self.mates.iter().all(|m| m.name != nm) {
                break nm;
            }
            k += 1;
        };
        (id, name)
    }
    /// Is `fixed` (explicitly, or implicitly as the first unsuppressed component when none is fixed).
    pub fn is_grounded(&self, id: &str) -> bool {
        let live: Vec<&Component> = self.components.iter().filter(|c| !c.suppressed).collect();
        let any_fixed = live.iter().any(|c| c.fixed);
        live.iter().enumerate().any(|(i, c)| c.id == id && (c.fixed || (!any_fixed && i == 0)))
    }
}

// ---------- small linear algebra ----------

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scl(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: V3) -> V3 {
    scl(a, 1.0 / norm(a).max(1e-300))
}
fn mv(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
fn mm(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

/// Rotation matrix from a rotation vector (radians).
pub fn rotvec_to_matrix(w: V3) -> M3 {
    let th = norm(w);
    if th < 1e-15 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let k = scl(w, 1.0 / th);
    let (s, c) = th.sin_cos();
    let t = 1.0 - c;
    [
        [t * k[0] * k[0] + c, t * k[0] * k[1] - s * k[2], t * k[0] * k[2] + s * k[1]],
        [t * k[0] * k[1] + s * k[2], t * k[1] * k[1] + c, t * k[1] * k[2] - s * k[0]],
        [t * k[0] * k[2] - s * k[1], t * k[1] * k[2] + s * k[0], t * k[2] * k[2] + c],
    ]
}

/// Rotation vector (radians) of a rotation matrix.
pub fn matrix_to_rotvec(r: &M3) -> V3 {
    let tr = r[0][0] + r[1][1] + r[2][2];
    let th = ((tr - 1.0) / 2.0).clamp(-1.0, 1.0).acos();
    if th < 1e-12 {
        return [0.0; 3];
    }
    if std::f64::consts::PI - th < 1e-6 {
        // axis from the symmetric part: R = 2kk^T - I
        let mut k = [((r[0][0] + 1.0) / 2.0).max(0.0).sqrt(), ((r[1][1] + 1.0) / 2.0).max(0.0).sqrt(), ((r[2][2] + 1.0) / 2.0).max(0.0).sqrt()];
        let i = (0..3).max_by(|&a, &b| k[a].total_cmp(&k[b])).unwrap();
        for j in 0..3 {
            if j != i {
                k[j] = (r[i][j] + r[j][i]) / (4.0 * k[i]);
            }
        }
        return scl(unit(k), th);
    }
    let v = [r[2][1] - r[1][2], r[0][2] - r[2][0], r[1][0] - r[0][1]];
    scl(v, th / (2.0 * th.sin()))
}

/// Minimal rotation taking unit vector `a` onto unit vector `b`.
fn rotation_between(a: V3, b: V3) -> M3 {
    let c = dot(a, b).clamp(-1.0, 1.0);
    let ax = cross(a, b);
    if norm(ax) < 1e-12 {
        if c > 0.0 {
            return rotvec_to_matrix([0.0; 3]);
        }
        // half turn about any axis perpendicular to a
        let p = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        return rotvec_to_matrix(scl(unit(cross(a, p)), std::f64::consts::PI));
    }
    rotvec_to_matrix(scl(unit(ax), c.acos()))
}

/// Rigid placement: world = R · local + t.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub r: M3,
    pub t: V3,
}

impl Placement {
    pub fn of(c: &Component) -> Placement {
        Placement { r: rotvec_to_matrix(scl(c.rotation, std::f64::consts::PI / 180.0)), t: c.position }
    }
    pub fn point(&self, p: V3) -> V3 {
        add(mv(&self.r, p), self.t)
    }
    pub fn dir(&self, d: V3) -> V3 {
        mv(&self.r, d)
    }
    /// Row-major 4x4.
    pub fn matrix(&self) -> [[f64; 4]; 4] {
        let mut m = [[0.0; 4]; 4];
        for i in 0..3 {
            m[i][..3].copy_from_slice(&self.r[i]);
            m[i][3] = self.t[i];
        }
        m[3][3] = 1.0;
        m
    }
    fn write(&self, c: &mut Component) {
        let round = |v: f64| {
            let r = (v * 1e6).round() / 1e6;
            if r == 0.0 { 0.0 } else { r }
        };
        c.position = self.t.map(round);
        c.rotation = scl(matrix_to_rotvec(&self.r), 180.0 / std::f64::consts::PI).map(round);
    }
}

/// Mate geometry: what a reference resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MGeom {
    Plane { origin: V3, normal: V3 },
    Axis { origin: V3, dir: V3 },
    Point { at: V3 },
}

impl MGeom {
    fn world(&self, p: &Placement) -> MGeom {
        match *self {
            MGeom::Plane { origin, normal } => MGeom::Plane { origin: p.point(origin), normal: p.dir(normal) },
            MGeom::Axis { origin, dir } => MGeom::Axis { origin: p.point(origin), dir: p.dir(dir) },
            MGeom::Point { at } => MGeom::Point { at: p.point(at) },
        }
    }
}

// ---------- rebuild ----------

struct PartEntry {
    stamp: Option<SystemTime>,
    regen: Regenerator,
    body: Option<Solid>,
    mesh: Option<Arc<Mesh>>,
    volume: f64,
    centroid: V3,
    error: Option<String>,
}

#[derive(Default)]
pub struct AsmRegenerator {
    parts: HashMap<PathBuf, PartEntry>,
}

#[derive(Clone, Debug)]
pub struct ComponentOut {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub placement: Placement,
    pub grounded: bool,
    pub suppressed: bool,
    pub body: Option<Solid>,
    pub mesh: Option<Arc<Mesh>>,
    pub volume: f64,
    pub centroid: V3,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MateStatus {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: MateKind,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AsmSolveReport {
    /// "fully_defined", "under_defined" or "over_defined"
    pub status: String,
    pub converged: bool,
    /// Remaining degrees of freedom of the free components.
    pub dof: usize,
    pub iterations: usize,
    pub max_residual: f64,
}

pub struct AsmOutput {
    pub components: Vec<ComponentOut>,
    pub mates: Vec<MateStatus>,
    pub solve: AsmSolveReport,
    /// Input assembly with solved component placements.
    pub solved: Assembly,
    pub millis: f64,
}

impl AsmOutput {
    pub fn errors(&self) -> Vec<String> {
        let mut v: Vec<String> =
            self.components.iter().filter_map(|c| c.error.as_ref().map(|e| format!("component {} ({}): {e}", c.name, c.id))).collect();
        v.extend(self.mates.iter().filter_map(|m| m.error.as_ref().map(|e| format!("mate {} ({}): {e}", m.name, m.id))));
        v
    }

    pub fn placement(&self, comp: &str) -> Option<Placement> {
        self.components.iter().find(|c| c.id == comp || c.name == comp).map(|c| c.placement)
    }

    /// All visible components as one world-space mesh. Face indices are made
    /// unique by offsetting each component's; `owners` maps triangle → component index.
    pub fn merged_mesh(&self) -> (Mesh, Vec<(usize, u32)>) {
        let mut out = Mesh::default();
        let mut owners = vec![]; // per component: (component index, face offset)
        let mut face_off = 0u32;
        let mut edge_off = 0u32;
        for (ci, c) in self.components.iter().enumerate() {
            let Some(m) = &c.mesh else { continue };
            if c.suppressed {
                continue;
            }
            let p = c.placement;
            let base = out.positions.len() as u32;
            let f32v = |v: V3| v.map(|x| x as f32);
            let f64v = |v: [f32; 3]| v.map(|x| x as f64);
            out.positions.extend(m.positions.iter().map(|&v| f32v(p.point(f64v(v)))));
            out.normals.extend(m.normals.iter().map(|&v| f32v(p.dir(f64v(v)))));
            out.indices.extend(m.indices.iter().map(|i| i + base));
            out.tri_face.extend(m.tri_face.iter().map(|f| f + face_off));
            out.edges.extend(m.edges.iter().map(|pl| pl.iter().map(|&v| f32v(p.point(f64v(v)))).collect()));
            out.edge_ids.extend(m.edge_ids.iter().map(|e| e + edge_off));
            for f in &m.faces {
                let mut f = f.clone();
                f.index += face_off as usize;
                f.centroid = p.point(f.centroid);
                f.normal = p.dir(f.normal);
                out.faces.push(f);
            }
            owners.push((ci, face_off));
            face_off += m.faces.len().max(m.tri_face.iter().copied().max().map_or(0, |x| x as usize + 1)) as u32;
            edge_off += m.edge_ids.iter().copied().max().map_or(0, |x| x + 1);
        }
        (out, owners)
    }

    /// Total volume and mass-weighted centroid.
    pub fn mass(&self) -> (f64, V3) {
        let mut v = 0.0;
        let mut c = [0.0; 3];
        for comp in self.components.iter().filter(|c| !c.suppressed && c.body.is_some()) {
            v += comp.volume;
            c = add(c, scl(comp.placement.point(comp.centroid), comp.volume));
        }
        (v, if v > 0.0 { scl(c, 1.0 / v) } else { c })
    }

    /// World-placed solids of all visible components.
    pub fn placed_solids(&self) -> Result<Vec<(String, Solid)>> {
        self.components
            .iter()
            .filter(|c| !c.suppressed)
            .filter_map(|c| c.body.as_ref().map(|b| (c, b)))
            .map(|(c, b)| Ok((c.name.clone(), rw_kernel::transform_matrix(b, &c.placement.matrix())?)))
            .collect()
    }

    /// Pairs of components whose solids overlap, with the overlap volume.
    pub fn interferences(&self) -> Result<Vec<(String, String, f64)>> {
        let placed = self.placed_solids()?;
        let boxes: Vec<(V3, V3)> = self
            .components
            .iter()
            .filter(|c| !c.suppressed && c.body.is_some())
            .map(|c| {
                let m = c.mesh.as_ref().unwrap();
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for v in &m.positions {
                    let w = c.placement.point(v.map(|x| x as f64));
                    for k in 0..3 {
                        lo[k] = lo[k].min(w[k]);
                        hi[k] = hi[k].max(w[k]);
                    }
                }
                (lo, hi)
            })
            .collect();
        let mut out = vec![];
        for i in 0..placed.len() {
            for j in i + 1..placed.len() {
                let (a, b) = (boxes[i], boxes[j]);
                if (0..3).any(|k| a.1[k] < b.0[k] || b.1[k] < a.0[k]) {
                    continue;
                }
                let v = rw_kernel::overlap_volume(&placed[i].1, &placed[j].1);
                if v > 1e-6 {
                    out.push((placed[i].0.clone(), placed[j].0.clone(), v));
                }
            }
        }
        Ok(out)
    }
}

/// Resolves a component's part file path against the assembly folder.
pub fn part_path(base: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() { pb } else { base.join(pb) }
}

fn stamp(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl AsmRegenerator {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when any cached part file changed on disk since it was built.
    pub fn parts_changed(&self) -> bool {
        self.parts.iter().any(|(p, e)| stamp(p) != e.stamp)
    }

    fn part(&mut self, path: &Path) -> &PartEntry {
        let st = stamp(path);
        let fresh = self.parts.get(path).is_some_and(|e| e.stamp == st && st.is_some());
        if !fresh {
            let mut entry = self.parts.remove(path).unwrap_or(PartEntry {
                stamp: None,
                regen: Regenerator::new(),
                body: None,
                mesh: None,
                volume: 0.0,
                centroid: [0.0; 3],
                error: None,
            });
            entry.stamp = st;
            match crate::model::read_part(path) {
                Ok(part) => {
                    let out = entry.regen.regenerate(&part);
                    let errs = out.errors();
                    entry.error = match (&out.body, errs.is_empty()) {
                        (None, _) => Some("part has no solid body".into()),
                        (Some(_), false) => Some(format!("part has rebuild errors: {}", errs.join("; "))),
                        _ => None,
                    };
                    entry.volume = out.mass.map_or(0.0, |m| m.volume);
                    entry.centroid = out.mass.map_or([0.0; 3], |m| m.centroid);
                    entry.body = out.body;
                    entry.mesh = out.mesh.map(Arc::new);
                }
                Err(e) => {
                    entry.error = Some(format!("{e:#}"));
                    entry.body = None;
                    entry.mesh = None;
                }
            }
            self.parts.insert(path.to_path_buf(), entry);
        }
        &self.parts[path]
    }

    pub fn regenerate(&mut self, asm: &Assembly, base: &Path) -> AsmOutput {
        let t0 = Instant::now();
        let mut comps = vec![];
        for c in &asm.components {
            let path = part_path(base, &c.path);
            let (body, mesh, volume, centroid, error) = if c.suppressed {
                (None, None, 0.0, [0.0; 3], None)
            } else {
                let e = self.part(&path);
                (e.body.clone(), e.mesh.clone(), e.volume, e.centroid, e.error.clone())
            };
            comps.push(ComponentOut {
                id: c.id.clone(),
                name: c.name.clone(),
                path,
                placement: Placement::of(c),
                grounded: asm.is_grounded(&c.id),
                suppressed: c.suppressed,
                body,
                mesh,
                volume,
                centroid,
                error,
            });
        }
        let (mates, solve) = solve(asm, &mut comps, None);
        let mut solved = asm.clone();
        for (c, co) in solved.components.iter_mut().zip(&comps) {
            if !co.grounded && !co.suppressed {
                co.placement.write(c);
            }
        }
        AsmOutput { components: comps, mates, solve, solved, millis: t0.elapsed().as_secs_f64() * 1000.0 }
    }
}

/// Resolves a mate reference to part-local geometry.
pub fn resolve_ref(r: &MateRef, comp: &ComponentOut) -> Result<MGeom> {
    let set = [r.face.is_some(), r.plane.is_some(), r.axis.is_some(), r.point.is_some()].iter().filter(|b| **b).count();
    if set != 1 {
        bail!("reference on `{}` must set exactly one of face, plane, axis, point", r.component);
    }
    if let Some(p) = r.plane {
        let n = match p {
            StdPlane::Front => [0.0, 0.0, 1.0],
            StdPlane::Top => [0.0, 1.0, 0.0],
            StdPlane::Right => [1.0, 0.0, 0.0],
        };
        return Ok(MGeom::Plane { origin: [0.0; 3], normal: n });
    }
    if let Some(a) = r.axis {
        let d = match a {
            AxisName::X => [1.0, 0.0, 0.0],
            AxisName::Y => [0.0, 1.0, 0.0],
            AxisName::Z => [0.0, 0.0, 1.0],
        };
        return Ok(MGeom::Axis { origin: [0.0; 3], dir: d });
    }
    if let Some(p) = r.point {
        return Ok(MGeom::Point { at: p });
    }
    let p = r.face.unwrap();
    let body = comp.body.as_ref().ok_or_else(|| anyhow!("component `{}` has no body", comp.name))?;
    let mesh = comp.mesh.as_ref().ok_or_else(|| anyhow!("component `{}` has no mesh", comp.name))?;
    let (fi, d) = rw_kernel::face_near(mesh, p).ok_or_else(|| anyhow!("component `{}` has no faces", comp.name))?;
    let scale = rw_kernel::scale_of(body);
    if d > (scale * 0.03).max(0.5) {
        bail!("no face of `{}` near ({:.3}, {:.3}, {:.3}) (part coordinates)", comp.name, p[0], p[1], p[2]);
    }
    Ok(match rw_kernel::face_geom(body, fi)? {
        FaceGeom::Plane { origin, normal } => {
            // keep the origin near the picked point so distances read naturally
            let o = sub(p, scl(normal, dot(sub(p, origin), normal)));
            MGeom::Plane { origin: o, normal }
        }
        FaceGeom::Cylinder { origin, axis, .. } | FaceGeom::Cone { apex: origin, axis, .. } => MGeom::Axis { origin, dir: axis },
        FaceGeom::Sphere { center, .. } => MGeom::Point { at: center },
        FaceGeom::Other => bail!("face {fi} of `{}` is a free-form surface; mate to a planar, cylindrical, conical or spherical face", comp.name),
    })
}

/// Residuals of one mate (directional terms scaled by `l` to be comparable with mm).
fn residual(kind: MateKind, aligned: bool, value: f64, ga: MGeom, gb: MGeom, l: f64) -> Result<Vec<f64>> {
    use MGeom::*;
    let sgn = |a: V3, b: V3| if dot(a, b) >= 0.0 { 1.0 } else { -1.0 };
    // direction match: anti-parallel for face-to-face planes unless aligned
    let dir_eq = |a: V3, b: V3, s: f64| -> Vec<f64> { sub(a, scl(b, s)).iter().map(|v| v * l).collect() };
    let line_dist = |p: V3, o: V3, d: V3| -> V3 { cross(sub(p, o), unit(d)) };
    Ok(match (kind, ga, gb) {
        (MateKind::Coincident | MateKind::Distance, Plane { origin: oa, normal: na }, Plane { origin: ob, normal: nb }) => {
            let s = if aligned { 1.0 } else { -1.0 };
            let mut r = dir_eq(na, nb, s);
            let gap = if kind == MateKind::Distance { value } else { 0.0 };
            r.push(dot(sub(ob, oa), na) - gap);
            r
        }
        (MateKind::Coincident | MateKind::Concentric, Axis { origin: oa, dir: da }, Axis { origin: ob, dir: db }) => {
            let mut r = dir_eq(da, db, sgn(da, db));
            r.extend(line_dist(ob, oa, da));
            r
        }
        (MateKind::Coincident | MateKind::Concentric, Point { at: a }, Point { at: b }) => sub(b, a).to_vec(),
        (MateKind::Coincident, Plane { origin, normal }, Point { at }) | (MateKind::Coincident, Point { at }, Plane { origin, normal }) => {
            vec![dot(sub(at, origin), normal)]
        }
        (MateKind::Coincident | MateKind::Concentric, Axis { origin, dir }, Point { at })
        | (MateKind::Coincident | MateKind::Concentric, Point { at }, Axis { origin, dir }) => line_dist(at, origin, dir).to_vec(),
        (MateKind::Coincident, Plane { origin: po, normal }, Axis { origin: ao, dir })
        | (MateKind::Coincident, Axis { origin: ao, dir }, Plane { origin: po, normal }) => {
            vec![dot(dir, normal) * l, dot(sub(ao, po), normal)]
        }
        (MateKind::Distance, Point { at: a }, Point { at: b }) => vec![norm(sub(b, a)) - value],
        (MateKind::Distance, Plane { origin, normal }, Point { at }) | (MateKind::Distance, Point { at }, Plane { origin, normal }) => {
            vec![dot(sub(at, origin), normal).abs() - value]
        }
        (MateKind::Distance, Axis { origin: oa, dir: da }, Axis { origin: ob, dir: db }) => {
            let mut r = dir_eq(da, db, sgn(da, db));
            r.push(norm(line_dist(ob, oa, da)) - value);
            r
        }
        (MateKind::Parallel, a, b) => {
            let (x, y) = (direction(a)?, direction(b)?);
            dir_eq(x, y, if aligned { 1.0 } else { sgn(x, y) })
        }
        (MateKind::Perpendicular, a, b) => vec![dot(direction(a)?, direction(b)?) * l],
        (MateKind::Angle, a, b) => {
            let (x, y) = (direction(a)?, direction(b)?);
            vec![(dot(x, y) - value.to_radians().cos()) * l]
        }
        (k, a, b) => bail!("{k:?} mate is not supported between {} and {}", gname(&a), gname(&b)),
    })
}

fn gname(g: &MGeom) -> &'static str {
    match g {
        MGeom::Plane { .. } => "a plane",
        MGeom::Axis { .. } => "an axis",
        MGeom::Point { .. } => "a point",
    }
}

fn direction(g: MGeom) -> Result<V3> {
    match g {
        MGeom::Plane { normal, .. } => Ok(normal),
        MGeom::Axis { dir, .. } => Ok(dir),
        MGeom::Point { .. } => bail!("points have no direction for parallel/perpendicular/angle mates"),
    }
}

struct Prepared {
    idx: usize,
    kind: MateKind,
    aligned: bool,
    value: f64,
    ca: usize,
    cb: usize,
    ga: MGeom,
    gb: MGeom,
}

fn prepare(asm: &Assembly, comps: &[ComponentOut]) -> (Vec<Prepared>, Vec<MateStatus>) {
    let scope = Scope::new(&asm.parameters);
    let mut ok = vec![];
    let mut status = vec![];
    for (i, m) in asm.mates.iter().enumerate() {
        let mut st = MateStatus { id: m.id.clone(), name: m.name.clone(), kind: m.kind, ok: true, error: None };
        if m.suppressed {
            status.push(st);
            continue;
        }
        let r = (|| -> Result<Prepared> {
            let find = |id: &str| -> Result<usize> {
                comps.iter().position(|c| c.id == id || c.name == id).ok_or_else(|| anyhow!("no component `{id}`"))
            };
            let (ca, cb) = (find(&m.a.component)?, find(&m.b.component)?);
            if ca == cb {
                bail!("a mate needs two different components");
            }
            for c in [ca, cb] {
                if comps[c].suppressed {
                    bail!("component `{}` is suppressed", comps[c].name);
                }
            }
            let ga = resolve_ref(&m.a, &comps[ca])?;
            let gb = resolve_ref(&m.b, &comps[cb])?;
            let value = match (&m.value, m.kind) {
                (Some(v), _) => scope.eval(v).context("evaluating mate value")?,
                (None, MateKind::Distance | MateKind::Angle) => bail!("{:?} mate needs a value", m.kind),
                (None, _) => 0.0,
            };
            // validate the combination once
            residual(m.kind, m.aligned, value, ga, gb, 1.0)?;
            Ok(Prepared { idx: i, kind: m.kind, aligned: m.aligned, value, ca, cb, ga, gb })
        })();
        match r {
            Ok(p) => ok.push(p),
            Err(e) => {
                st.ok = false;
                st.error = Some(format!("{e:#}"));
            }
        }
        status.push(st);
    }
    (ok, status)
}

fn eval_all(ms: &[Prepared], pl: &[Placement], l: f64) -> Vec<Vec<f64>> {
    ms.iter()
        .map(|m| residual(m.kind, m.aligned, m.value, m.ga.world(&pl[m.ca]), m.gb.world(&pl[m.cb]), l).unwrap_or_default())
        .collect()
}

fn flat(r: &[Vec<f64>]) -> Vec<f64> {
    r.iter().flatten().copied().collect()
}

fn sq(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum()
}

/// Applies a 6-vector step (translation, rotation vector) to placement `p`.
fn step(p: &Placement, d: &[f64]) -> Placement {
    let dr = rotvec_to_matrix([d[3], d[4], d[5]]);
    // rotate about the component's current origin
    Placement { r: mm(&dr, &p.r), t: [p.t[0] + d[0], p.t[1] + d[1], p.t[2] + d[2]] }
}

/// Interactive drag: pull `local` (a point on component `comp`, part
/// coordinates) toward the world point `target`, while mates hold.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub comp: usize,
    pub local: V3,
    pub target: V3,
}

/// Weight of the drag pull relative to mate residuals: small, so mates win
/// and the part only slides along its remaining degrees of freedom.
const DRAG_WEIGHT: f64 = 1e-2;

fn solve(asm: &Assembly, comps: &mut [ComponentOut], drag: Option<Drag>) -> (Vec<MateStatus>, AsmSolveReport) {
    let (ms, mut status) = prepare(asm, comps);
    let free: Vec<usize> = (0..comps.len()).filter(|&i| !comps[i].grounded && !comps[i].suppressed && comps[i].body.is_some()).collect();
    let l = comps
        .iter()
        .filter_map(|c| c.body.as_ref())
        .map(rw_kernel::scale_of)
        .fold(1.0f64, f64::max)
        .min(1e4);
    let n = free.len() * 6;
    let mut pl: Vec<Placement> = comps.iter().map(|c| c.placement).collect();
    let tol = 1e-7 * l.max(1.0);
    let mut report = AsmSolveReport { converged: true, ..Default::default() };
    let drag = drag.filter(|d| free.contains(&d.comp));
    let resid = |pl: &[Placement], with_drag: bool| -> Vec<f64> {
        let mut r = flat(&eval_all(&ms, pl, l));
        if let (true, Some(d)) = (with_drag, drag) {
            let w = pl[d.comp].point(d.local);
            r.extend((0..3).map(|k| (w[k] - d.target[k]) * DRAG_WEIGHT));
        }
        r
    };
    let jac_with = |pl: &[Placement], with_drag: bool| -> (Vec<f64>, Vec<Vec<f64>>) {
        let r0 = resid(pl, with_drag);
        let mut cols = vec![];
        for (k, &ci) in free.iter().enumerate() {
            for j in 0..6 {
                let h = if j < 3 { 1e-6 * l } else { 1e-6 };
                let mut d = [0.0; 6];
                d[j] = h;
                let mut p1 = pl.to_vec();
                p1[ci] = step(&pl[ci], &d);
                d[j] = -h;
                let mut p2 = pl.to_vec();
                p2[ci] = step(&pl[ci], &d);
                let (a, b) = (resid(&p1, with_drag), resid(&p2, with_drag));
                cols.push(a.iter().zip(&b).map(|(x, y)| (x - y) / (2.0 * h)).collect());
                let _ = k;
            }
        }
        (r0, cols)
    };
    let jac = |pl: &[Placement]| jac_with(pl, false);
    if n > 0 && (!ms.is_empty() || drag.is_some()) {
        let mut lambda = 1e-3;
        let max_iter = if drag.is_some() { 40 } else { 300 };
        for it in 0..max_iter {
            report.iterations = it;
            let (r, cols) = jac_with(&pl, drag.is_some());
            let f0 = sq(&r);
            if f0.sqrt() < tol {
                break;
            }
            // normal equations
            let mut a = vec![vec![0.0; n]; n];
            let mut g = vec![0.0; n];
            for i in 0..n {
                for j in i..n {
                    let v: f64 = cols[i].iter().zip(&cols[j]).map(|(x, y)| x * y).sum();
                    a[i][j] = v;
                    a[j][i] = v;
                }
                g[i] = -cols[i].iter().zip(&r).map(|(x, y)| x * y).sum::<f64>();
            }
            let mut improved = false;
            for _ in 0..12 {
                let mut m = a.clone();
                for i in 0..n {
                    m[i][i] += lambda * (a[i][i] + 1e-9);
                }
                let Some(dx) = solve_linear(m, g.clone()) else {
                    lambda *= 10.0;
                    continue;
                };
                let mut trial = pl.clone();
                for (k, &ci) in free.iter().enumerate() {
                    trial[ci] = step(&pl[ci], &dx[k * 6..k * 6 + 6]);
                }
                if sq(&resid(&trial, drag.is_some())) < f0 {
                    pl = trial;
                    lambda = (lambda / 3.0).max(1e-12);
                    improved = true;
                    break;
                }
                lambda *= 5.0;
            }
            if !improved {
                break;
            }
        }
    }
    let res = eval_all(&ms, &pl, l);
    report.max_residual = res.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
    report.converged = report.max_residual < tol * 100.0;
    for (m, r) in ms.iter().zip(&res) {
        if r.iter().any(|v| v.abs() > tol * 100.0) {
            let st = &mut status[m.idx];
            st.ok = false;
            st.error = Some("not satisfied (conflicts with other mates or a fixed component)".into());
        }
    }
    // remaining DOF = free variables minus Jacobian rank at the solution
    report.dof = if n == 0 {
        0
    } else if ms.is_empty() {
        n
    } else {
        let (_, cols) = jac(&pl);
        n - rank(&cols)
    };
    report.status = if !report.converged {
        "over_defined"
    } else if report.dof == 0 {
        "fully_defined"
    } else {
        "under_defined"
    }
    .into();
    for (c, p) in comps.iter_mut().zip(&pl) {
        c.placement = *p;
    }
    (status, report)
}

fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-300 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
            if f != 0.0 {
                for k in c..n {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|k| a[i][k] * x[k]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Some(x)
}

/// Numerical rank of the matrix whose columns are `cols`.
fn rank(cols: &[Vec<f64>]) -> usize {
    let n = cols.len();
    let m = cols.first().map_or(0, |c| c.len());
    let mut a: Vec<Vec<f64>> = (0..m).map(|i| (0..n).map(|j| cols[j][i]).collect()).collect();
    let maxv = a.iter().flatten().fold(0.0f64, |x, v| x.max(v.abs()));
    let tol = maxv * 1e-7 + 1e-12;
    let mut rank = 0;
    let mut row = 0;
    for c in 0..n {
        if row >= m {
            break;
        }
        let p = (row..m).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs())).unwrap();
        if a[p][c].abs() <= tol {
            continue;
        }
        a.swap(row, p);
        for r in row + 1..m {
            let f = a[r][c] / a[row][c];
            for k in c..n {
                a[r][k] -= f * a[row][k];
            }
        }
        row += 1;
        rank += 1;
    }
    rank
}

/// Moves component `move_id` so that mate `m` is (approximately) satisfied,
/// starting from current placements: rotates the shortest way to align
/// directions, then translates. Gives the solver a good starting point,
/// like SOLIDWORKS snapping parts together when a mate is added.
pub fn presnap(asm: &mut Assembly, out: &AsmOutput, m: &Mate) -> Result<()> {
    let ca = out.components.iter().find(|c| c.id == m.a.component || c.name == m.a.component).ok_or_else(|| anyhow!("no component `{}`", m.a.component))?;
    let cb = out.components.iter().find(|c| c.id == m.b.component || c.name == m.b.component).ok_or_else(|| anyhow!("no component `{}`", m.b.component))?;
    // move b unless it is grounded
    let (fixed, moving, gf, gm, flip_roles) = if !cb.grounded {
        (ca, cb, resolve_ref(&m.a, ca)?, resolve_ref(&m.b, cb)?, false)
    } else if !ca.grounded {
        (cb, ca, resolve_ref(&m.b, cb)?, resolve_ref(&m.a, ca)?, true)
    } else {
        return Ok(());
    };
    let wf = gf.world(&fixed.placement);
    let mut p = moving.placement;
    let wm = gm.world(&p);
    let value = match &m.value {
        Some(v) => Scope::new(&asm.parameters).eval(v).unwrap_or(0.0),
        None => 0.0,
    };
    // 1. orientation
    let target_dir = |df: V3, dm: V3, opposite: bool| -> V3 {
        if opposite { scl(df, -1.0) } else if dot(df, dm) >= 0.0 { df } else { scl(df, -1.0) }
    };
    let rot = match (m.kind, wf, wm) {
        (MateKind::Coincident | MateKind::Distance, MGeom::Plane { normal: nf, .. }, MGeom::Plane { normal: nm, .. }) => {
            Some(rotation_between(unit(nm), unit(if m.aligned { nf } else { scl(nf, -1.0) })))
        }
        (MateKind::Coincident | MateKind::Concentric | MateKind::Distance | MateKind::Parallel, MGeom::Axis { dir: df, .. }, MGeom::Axis { dir: dm, .. }) => {
            Some(rotation_between(unit(dm), unit(target_dir(df, dm, false))))
        }
        (MateKind::Parallel, a, b) => {
            let (df, dm) = (direction(a)?, direction(b)?);
            Some(rotation_between(unit(dm), unit(target_dir(df, dm, m.aligned))))
        }
        _ => None,
    };
    if let Some(r) = rot {
        // rotate about the moving geometry's anchor so it stays put
        let anchor = match wm {
            MGeom::Plane { origin, .. } | MGeom::Axis { origin, .. } => origin,
            MGeom::Point { at } => at,
        };
        p = Placement { r: mm(&r, &p.r), t: add(mv(&r, sub(p.t, anchor)), anchor) };
    }
    // 2. position
    let wm = gm.world(&p);
    let shift = match (wf, wm) {
        (MGeom::Plane { origin: of, normal: nf }, MGeom::Plane { origin: om, .. }) => {
            let gap = if m.kind == MateKind::Distance { value } else { 0.0 };
            // keep (o_b - o_a)·n_a = gap whichever side moves
            let s = if flip_roles && m.aligned { -1.0 } else { 1.0 };
            Some(scl(nf, gap * s - dot(sub(om, of), nf)))
        }
        (MGeom::Axis { origin: of, dir }, MGeom::Axis { origin: om, .. }) if m.kind != MateKind::Parallel && m.kind != MateKind::Distance => {
            let d = unit(dir);
            let off = sub(om, of);
            Some(scl(sub(off, scl(d, dot(off, d))), -1.0))
        }
        (MGeom::Point { at: a }, MGeom::Point { at: b }) if m.kind != MateKind::Distance => Some(sub(a, b)),
        (MGeom::Plane { origin, normal }, MGeom::Point { at }) if m.kind == MateKind::Coincident => Some(scl(normal, -dot(sub(at, origin), normal))),
        (MGeom::Axis { origin, dir }, MGeom::Point { at }) => {
            let d = unit(dir);
            let off = sub(at, origin);
            Some(scl(sub(off, scl(d, dot(off, d))), -1.0))
        }
        _ => None,
    };
    if let Some(s) = shift {
        p.t = add(p.t, s);
    }
    let id = moving.id.clone();
    let c = asm.component_mut(&id).unwrap();
    p.write(c);
    Ok(())
}

// ---------- model (undo/redo, autosave, live file sync) ----------

pub struct AsmModel {
    pub asm: Assembly,
    pub path: Option<PathBuf>,
    pub out: AsmOutput,
    regen: AsmRegenerator,
    undo: Vec<Assembly>,
    redo: Vec<Assembly>,
    disk_stamp: Option<SystemTime>,
}

impl AsmModel {
    pub fn new(asm: Assembly, path: Option<PathBuf>) -> AsmModel {
        let mut regen = AsmRegenerator::new();
        let base = base_dir(path.as_deref());
        let out = regen.regenerate(&asm, &base);
        let asm = out.solved.clone();
        let disk_stamp = path.as_deref().and_then(stamp);
        AsmModel { asm, path, out, regen, undo: vec![], redo: vec![], disk_stamp }
    }

    pub fn open(path: &Path) -> Result<AsmModel> {
        let asm = read_assembly(path)?;
        Ok(AsmModel::new(asm, Some(path.to_path_buf())))
    }

    pub fn base(&self) -> PathBuf {
        base_dir(self.path.as_deref())
    }

    pub fn rebuild(&mut self) {
        let base = self.base();
        self.out = self.regen.regenerate(&self.asm, &base);
        self.asm = self.out.solved.clone();
    }

    pub fn edit<T>(&mut self, f: impl FnOnce(&mut Assembly) -> Result<T>) -> Result<T> {
        let before = self.asm.clone();
        match f(&mut self.asm) {
            Ok(v) => {
                if self.asm != before {
                    self.undo.push(before);
                    self.redo.clear();
                    self.rebuild();
                    self.save()?;
                }
                Ok(v)
            }
            Err(e) => {
                self.asm = before;
                Err(e)
            }
        }
    }

    /// Adds a mate, first snapping the moving component into place.
    pub fn add_mate(&mut self, mut m: Mate) -> Result<String> {
        if m.id.is_empty() {
            let (id, name) = self.asm.fresh_mate(m.kind);
            m.id = id;
            if m.name.is_empty() {
                m.name = name;
            }
        }
        let snapshot = &self.out;
        let mut asm = self.asm.clone();
        presnap(&mut asm, snapshot, &m)?;
        let id = m.id.clone();
        self.edit(move |a| {
            *a = asm;
            a.mates.push(m);
            Ok(())
        })?;
        Ok(id)
    }

    /// Live drag preview: moves component `comp` so its part-space point
    /// `local` follows `target`, honoring mates. Only the displayed placements
    /// change; call `drag_commit` to store the result as one undo step.
    pub fn drag_preview(&mut self, comp: &str, local: [f64; 3], target: [f64; 3]) -> bool {
        let Some(ci) = self.out.components.iter().position(|c| c.id == comp) else { return false };
        if self.out.components[ci].grounded {
            return false;
        }
        solve(&self.asm, &mut self.out.components, Some(Drag { comp: ci, local, target }));
        // settle exactly onto the mates from the dragged position
        let (mates, report) = solve(&self.asm, &mut self.out.components, None);
        self.out.mates = mates;
        self.out.solve = report;
        true
    }

    /// Stores the dragged placements in the document (then re-solves exactly).
    pub fn drag_commit(&mut self) -> Result<()> {
        let placed: Vec<(String, Placement)> = self.out.components.iter().map(|c| (c.id.clone(), c.placement)).collect();
        self.edit(|a| {
            for (id, p) in &placed {
                if let Some(c) = a.component_mut(id) {
                    p.write(c);
                }
            }
            Ok(())
        })
    }

    pub fn undo(&mut self) -> bool {
        if let Some(a) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.asm, a));
            self.rebuild();
            let _ = self.save();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(a) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.asm, a));
            self.rebuild();
            let _ = self.save();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.path = Some(path.to_path_buf());
        self.save()
    }

    pub fn save(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        write_assembly(&path, &self.asm)?;
        self.disk_stamp = stamp(&path);
        Ok(())
    }

    /// Reloads when the assembly file or any of its part files changed on disk.
    pub fn sync_from_disk(&mut self) -> Result<bool> {
        let mut changed = false;
        if let Some(path) = self.path.clone() {
            let st = stamp(&path);
            if st.is_some() && st != self.disk_stamp {
                let a = read_assembly(&path)?;
                self.disk_stamp = st;
                if a != self.asm {
                    self.undo.push(std::mem::replace(&mut self.asm, a));
                    self.redo.clear();
                    changed = true;
                }
            }
        }
        if changed || self.regen.parts_changed() {
            self.rebuild();
            return Ok(true);
        }
        Ok(false)
    }

    /// Writes a STEP file with every placed component (as one compound).
    pub fn export_step(&self, path: &Path) -> Result<()> {
        let solids: Vec<Solid> = self.out.placed_solids()?.into_iter().map(|s| s.1).collect();
        if solids.is_empty() {
            bail!("assembly has no geometry to export");
        }
        rw_kernel::write_step(&rw_kernel::compound(&solids), path)
    }
}

fn base_dir(path: Option<&Path>) -> PathBuf {
    path.and_then(|p| p.parent()).filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

pub fn read_assembly(path: &Path) -> Result<Assembly> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_assembly(path: &Path, a: &Assembly) -> Result<()> {
    let json = serde_json::to_string_pretty(a)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("rwasm.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Path of `part` relative to the assembly folder when possible.
pub fn relative_part_path(asm_base: &Path, part: &Path) -> String {
    let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (b, p) = (abs(asm_base), abs(part));
    match p.strip_prefix(&b) {
        Ok(r) => r.display().to_string(),
        Err(_) => p.display().to_string(),
    }
}
