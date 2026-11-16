//! Feature-tree regeneration with incremental caching.
//!
//! Each feature's resulting state is cached under a hash of (parameters +
//! all features up to and including it). Editing feature N therefore only
//! rebuilds features N.., which keeps interactive edits fast on long trees.

use crate::doc::*;
use anyhow::{anyhow, bail, Context, Result};
use rw_kernel::{BoolOp, MassProps, Mesh, Plane, Solid, Xform};
use rw_sketch::{Region, Scope, Sketch, SolveReport, Value};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

#[derive(Clone)]
struct Tool {
    solids: Vec<Solid>,
    /// Per-solid alternate tool extended slightly into existing material,
    /// used when an exact union fails on coplanar contact.
    alts: Vec<Option<Solid>>,
    op: Op,
}

impl Tool {
    fn plain(solids: Vec<Solid>, op: Op) -> Tool {
        let alts = vec![None; solids.len()];
        Tool { solids, alts, op }
    }
}

#[derive(Clone, Default)]
struct State {
    body: Option<Solid>,
    tools: HashMap<String, Tool>,
    sketches: HashMap<String, SolvedSketch>,
    /// Re-snapped edge reference points per feature (written back to the doc).
    snaps: HashMap<String, Vec<EdgeRef>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SolvedSketch {
    pub feature: String,
    pub plane: Plane,
    pub sketch: Sketch,
    pub report: Option<SolveReport>,
    #[serde(skip)]
    pub regions: Vec<Region>,
    pub profile_error: Option<String>,
}

impl SolvedSketch {
    /// 3D polylines of the sketch geometry: (points, is_construction).
    pub fn polylines(&self) -> Vec<(Vec<[f32; 3]>, bool)> {
        use rw_sketch::Entity;
        let pt = |id: &str| self.sketch.point_xy(id).unwrap_or([0.0, 0.0]);
        let to3 = |p: [f64; 2]| self.plane.to3d(p).map(|v| v as f32);
        let mut out = vec![];
        for e in &self.sketch.entities {
            match e {
                Entity::Line { start, end, construction, .. } => out.push((vec![to3(pt(start)), to3(pt(end))], *construction)),
                Entity::Circle { center, radius, construction, .. } => {
                    let c = pt(center);
                    let v = (0..=64)
                        .map(|i| {
                            let t = i as f64 / 64.0 * std::f64::consts::TAU;
                            to3([c[0] + radius * t.cos(), c[1] + radius * t.sin()])
                        })
                        .collect();
                    out.push((v, *construction));
                }
                Entity::Arc { center, start, end, construction, .. } => {
                    let (c, s, e) = (pt(center), pt(start), pt(end));
                    let r = (s[0] - c[0]).hypot(s[1] - c[1]);
                    let t0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                    let mut sw = ((e[1] - c[1]).atan2(e[0] - c[0]) - t0).rem_euclid(std::f64::consts::TAU);
                    if sw < 1e-9 {
                        sw = std::f64::consts::TAU;
                    }
                    let v = (0..=48)
                        .map(|i| {
                            let t = t0 + sw * i as f64 / 48.0;
                            to3([c[0] + r * t.cos(), c[1] + r * t.sin()])
                        })
                        .collect();
                    out.push((v, *construction));
                }
                Entity::Point { .. } => {}
            }
        }
        out
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FeatureStatus {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub suppressed: bool,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sketch: Option<SolveReport>,
}

pub struct RegenOutput {
    pub body: Option<Solid>,
    pub mesh: Option<Mesh>,
    pub mass: Option<MassProps>,
    pub status: Vec<FeatureStatus>,
    /// Solved sketches in feature order.
    pub sketches: Vec<SolvedSketch>,
    /// The input part with sketch geometry updated to solved positions.
    pub solved: Part,
    pub millis: f64,
    pub cache_hits: usize,
}

impl RegenOutput {
    pub fn errors(&self) -> Vec<String> {
        self.status
            .iter()
            .filter_map(|s| s.error.as_ref().map(|e| format!("{} ({}): {}", s.name, s.id, e)))
            .collect()
    }
}

#[derive(Default)]
pub struct Regenerator {
    cache: Vec<(u64, State, FeatureStatus)>,
}

pub fn resolve_plane(p: &PlaneRef, scope: &Scope) -> Result<Plane> {
    let std = |s: &StdPlane| match s {
        StdPlane::Front => Plane { origin: [0.0; 3], x: [1.0, 0.0, 0.0], y: [0.0, 1.0, 0.0] },
        StdPlane::Top => Plane { origin: [0.0; 3], x: [1.0, 0.0, 0.0], y: [0.0, 0.0, -1.0] },
        StdPlane::Right => Plane { origin: [0.0; 3], x: [0.0, 0.0, -1.0], y: [0.0, 1.0, 0.0] },
    };
    Ok(match p {
        PlaneRef::Named(s) => std(s),
        PlaneRef::Offset { base, offset } => std(base).offset(scope.eval(offset)?),
        PlaneRef::Custom { origin, normal, x_dir } => Plane::new(*origin, *normal, *x_dir)?,
    })
}

impl Regenerator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    pub fn regenerate(&mut self, part: &Part) -> RegenOutput {
        let t0 = Instant::now();
        let mut solved = part.clone();
        let scope_params = part.parameters.clone();
        let scope = Scope::new(&scope_params);

        let mut h = DefaultHasher::new();
        serde_json::to_string(&part.parameters).unwrap_or_default().hash(&mut h);
        let mut state = State::default();
        let mut status = vec![];
        let mut hits = 0;
        let mut new_cache = Vec::with_capacity(part.features.len());
        let mut cache_valid = true;

        for (i, f) in part.features.iter().enumerate() {
            serde_json::to_string(f).unwrap_or_default().hash(&mut h);
            let key = h.finish();
            if cache_valid {
                if let Some((k, st, fs)) = self.cache.get(i) {
                    if *k == key {
                        state = st.clone();
                        if let Some(sk) = state.sketches.get(&f.id) {
                            if let FeatureKind::Sketch(sf) = &mut solved.features[i].kind {
                                sf.sketch = sk.sketch.clone();
                            }
                        }
                        status.push(fs.clone());
                        new_cache.push((key, state.clone(), fs.clone()));
                        hits += 1;
                        continue;
                    }
                }
                cache_valid = false;
            }
            let mut fs = FeatureStatus {
                id: f.id.clone(),
                name: f.name.clone(),
                kind: f.kind.type_name(),
                suppressed: f.suppressed,
                ok: true,
                error: None,
                warning: None,
                sketch: None,
            };
            if !f.suppressed {
                // Features are transactional: a failure leaves the body as it was.
                let before = state.body.clone();
                if let Err(e) = apply(&mut state, f, &scope, &mut fs) {
                    state.body = before;
                    fs.ok = false;
                    fs.error = Some(format!("{e:#}"));
                }
                if let (FeatureKind::Sketch(sf), Some(sk)) = (&mut solved.features[i].kind, state.sketches.get(&f.id)) {
                    sf.sketch = sk.sketch.clone();
                }
                if let Some(pts) = state.snaps.remove(&f.id) {
                    match &mut solved.features[i].kind {
                        FeatureKind::Fillet(x) => x.edges = pts,
                        FeatureKind::Chamfer(x) => x.edges = pts,
                        _ => {}
                    }
                }
            }
            status.push(fs.clone());
            new_cache.push((key, state.clone(), fs));
        }

        // Re-key the cache against the *solved* document (which is what gets
        // saved), so the next regenerate of the saved doc is a full cache hit.
        let mut h2 = DefaultHasher::new();
        serde_json::to_string(&solved.parameters).unwrap_or_default().hash(&mut h2);
        for (i, f) in solved.features.iter().enumerate() {
            serde_json::to_string(f).unwrap_or_default().hash(&mut h2);
            new_cache[i].0 = h2.finish();
        }
        self.cache = new_cache;

        let (mesh, mass) = match &state.body {
            Some(b) => {
                let tol = (rw_kernel::scale_of(b) / 1500.0).clamp(0.002, 1.0);
                let m = rw_kernel::tessellate(b, tol, true);
                let mp = rw_kernel::mass_props(b, &m);
                (Some(m), Some(mp))
            }
            None => (None, None),
        };
        let sketches = part.features.iter().filter_map(|f| state.sketches.get(&f.id).cloned()).collect();
        RegenOutput {
            body: state.body,
            mesh,
            mass,
            status,
            sketches,
            solved,
            millis: t0.elapsed().as_secs_f64() * 1000.0,
            cache_hits: hits,
        }
    }
}

fn num(scope: &Scope, v: &Value, what: &str) -> Result<f64> {
    scope.eval(v).with_context(|| format!("evaluating {what}"))
}

fn combine(state: &mut State, tool: &Tool, fs: &mut FeatureStatus) -> Result<()> {
    for (i, s) in tool.solids.iter().enumerate() {
        match (&state.body, tool.op) {
            (None, Op::Boss) => state.body = Some(s.clone()),
            (None, Op::Cut) => bail!("nothing to cut: create a boss feature first"),
            (Some(b), Op::Boss) => {
                let scale = rw_kernel::scale_of(b);
                let alt = tool.alts.get(i).and_then(|a| a.as_ref());
                let (body, kind) = rw_kernel::union_robust(b, s, alt, scale)?;
                if kind == rw_kernel::UnionKind::MergedTouching {
                    fs.warning = Some("bodies only touch along a face; kept as a multi-body solid".into());
                }
                state.body = Some(body);
            }
            (Some(b), Op::Cut) => {
                let scale = rw_kernel::scale_of(b);
                state.body = Some(rw_kernel::boolean(b, s, BoolOp::Subtract, scale)?);
            }
        }
    }
    Ok(())
}

fn body_scale(state: &State) -> f64 {
    state.body.as_ref().map(rw_kernel::scale_of).unwrap_or(100.0)
}

fn sketch_of<'a>(state: &'a State, id: &str) -> Result<&'a SolvedSketch> {
    let sk = state.sketches.get(id).ok_or_else(|| anyhow!("sketch `{id}` not found (or it failed / is suppressed)"))?;
    if let Some(e) = &sk.profile_error {
        bail!("sketch `{id}` has no usable profile: {e}");
    }
    Ok(sk)
}

fn apply(state: &mut State, f: &Feature, scope: &Scope, fs: &mut FeatureStatus) -> Result<()> {
    match &f.kind {
        FeatureKind::Sketch(sf) => {
            let plane = resolve_plane(&sf.plane, scope)?;
            let mut sk = sf.sketch.clone();
            let report = sk.solve(scope)?;
            fs.sketch = Some(report.clone());
            let (regions, profile_error) = match sk.regions() {
                Ok(r) => (r, None),
                Err(e) => (vec![], Some(e.to_string())),
            };
            state.sketches.insert(
                f.id.clone(),
                SolvedSketch { feature: f.id.clone(), plane, sketch: sk, report: Some(report.clone()), regions, profile_error },
            );
            if !report.converged {
                bail!("sketch constraints conflict (over-defined); max residual {:.3e}", report.max_residual);
            }
        }
        FeatureKind::Extrude(e) => {
            let sk = sketch_of(state, &e.sketch)?.clone();
            let mut d = num(scope, &e.depth, "depth")?;
            let n = sk.plane.normal();
            let sign = if e.reverse { -1.0 } else { 1.0 };
            let mut start = 0.0;
            match e.end {
                EndCondition::Blind => {
                    if d <= 0.0 {
                        bail!("depth must be positive");
                    }
                }
                EndCondition::MidPlane => {
                    if d <= 0.0 {
                        bail!("depth must be positive");
                    }
                    start = -d / 2.0;
                }
                EndCondition::ThroughAll => {
                    let body = state.body.as_ref().ok_or_else(|| anyhow!("through all needs an existing body"))?;
                    let dir = [n[0] * sign, n[1] * sign, n[2] * sign];
                    d = rw_kernel::extent_along(body, sk.plane.origin, dir);
                    if d <= 0.0 {
                        bail!("through all: no material in the extrude direction (try reverse)");
                    }
                }
            }
            if e.op == Op::Cut && e.end != EndCondition::MidPlane {
                // Start the cut tool slightly behind the sketch plane so a sketch on a
                // face does not leave a coplanar (numerically fragile) overlap.
                let eps = (body_scale(state) * 1e-3).max(1e-3);
                start -= eps;
                d += eps;
            }
            let plane = sk.plane.offset(start * sign);
            let dir = [n[0] * d * sign, n[1] * d * sign, n[2] * d * sign];
            // Alternate boss tool sunk slightly into existing material.
            let delta = (body_scale(state) * 1e-4).max(1e-4);
            let alt_plane = sk.plane.offset((start - delta) * sign);
            let alt_dir = [n[0] * (d + delta) * sign, n[1] * (d + delta) * sign, n[2] * (d + delta) * sign];
            let want_alt = e.op == Op::Boss && state.body.is_some() && e.end == EndCondition::Blind;
            let mut solids = vec![];
            let mut alts = vec![];
            for r in &sk.regions {
                let face = rw_kernel::region_face(&plane, r)?;
                solids.push(rw_kernel::extrude(&face, dir)?);
                alts.push(if want_alt {
                    Some(rw_kernel::extrude(&rw_kernel::region_face(&alt_plane, r)?, alt_dir)?)
                } else {
                    None
                });
            }
            let tool = Tool { solids, alts, op: e.op };
            combine(state, &tool, fs)?;
            state.tools.insert(f.id.clone(), tool);
        }
        FeatureKind::Revolve(r) => {
            let sk = sketch_of(state, &r.sketch)?.clone();
            let ang = num(scope, &r.angle, "angle")?;
            let (o2, d2) = match &r.axis {
                AxisRef::Entity(id) if id == "x_axis" => ([0.0, 0.0], [1.0, 0.0]),
                AxisRef::Entity(id) if id == "y_axis" => ([0.0, 0.0], [0.0, 1.0]),
                AxisRef::Entity(id) => match sk.sketch.entity(id) {
                    Some(rw_sketch::Entity::Line { start, end, .. }) => {
                        let a = sk.sketch.point_xy(start).unwrap();
                        let b = sk.sketch.point_xy(end).unwrap();
                        (a, [b[0] - a[0], b[1] - a[1]])
                    }
                    _ => bail!("revolve axis `{id}` is not a line in sketch `{}`", r.sketch),
                },
                AxisRef::Explicit { origin, direction } => (*origin, *direction),
            };
            let origin = sk.plane.to3d(o2);
            let axis = sk.plane.dir3d(d2);
            let mut solids = vec![];
            for reg in &sk.regions {
                let face = rw_kernel::region_face(&sk.plane, reg)?;
                solids.push(rw_kernel::revolve(&face, origin, axis, ang)?);
            }
            let tool = Tool::plain(solids, r.op);
            combine(state, &tool, fs)?;
            state.tools.insert(f.id.clone(), tool);
        }
        FeatureKind::Hole(h) => {
            let plane = resolve_plane(&h.plane, scope)?;
            let r = num(scope, &h.diameter, "diameter")? / 2.0;
            if r <= 0.0 {
                bail!("hole diameter must be positive");
            }
            let n = plane.normal();
            let depth = match &h.depth {
                Some(v) => num(scope, v, "depth")?,
                None => {
                    let body = state.body.as_ref().ok_or_else(|| anyhow!("nothing to drill into"))?;
                    // Deepest point among all hole locations.
                    h.points
                        .iter()
                        .map(|p| rw_kernel::extent_along(body, plane.to3d(*p), [-n[0], -n[1], -n[2]]))
                        .fold(f64::NEG_INFINITY, f64::max)
                        .max(1e-3)
                }
            };
            let eps = (body_scale(state) * 1e-3).max(1e-3);
            // Hole tools are built from extruded cylinders: truck's booleans are
            // much more reliable on those than on revolved surfaces.
            let cyl = |c2: [f64; 2], rad: f64, z0: f64, z1: f64| -> Result<Solid> {
                let mut sk = Sketch::default();
                sk.add_circle(c2, rad, false);
                let regs = sk.regions().map_err(|e| anyhow!("hole profile: {e}"))?;
                let face = rw_kernel::region_face(&plane.offset(-z0), &regs[0])?;
                rw_kernel::extrude(&face, [-n[0] * (z1 - z0), -n[1] * (z1 - z0), -n[2] * (z1 - z0)])
            };
            let mut solids = vec![];
            for p in &h.points {
                let mut main_top = -eps;
                if let Some(cb) = &h.counterbore {
                    let cr = num(scope, &cb.diameter, "counterbore diameter")? / 2.0;
                    let cd = num(scope, &cb.depth, "counterbore depth")?;
                    if cr <= r || cd <= 0.0 {
                        bail!("counterbore must be wider than the hole and have positive depth");
                    }
                    solids.push(cyl(*p, cr, -eps, cd)?);
                    // Start the main bore inside the counterbore void (no coplanar faces).
                    main_top = cd * 0.5;
                } else if let Some(cs) = &h.countersink {
                    let cr = num(scope, &cs.diameter, "countersink diameter")? / 2.0;
                    let ca = num(scope, &cs.angle, "countersink angle")?.to_radians() / 2.0;
                    if cr <= r || ca <= 0.0 {
                        bail!("countersink must be wider than the hole");
                    }
                    let cd = (cr - r) / ca.tan();
                    // Cone ring (does not touch the axis); the bore clears the middle.
                    let ri = r * 0.5;
                    let prof = [[ri, -eps], [cr + eps * ca.tan(), -eps], [r, cd], [ri, cd]];
                    let c = plane.to3d(*p);
                    let sec = Plane { origin: c, x: plane.x, y: [-n[0], -n[1], -n[2]] };
                    let mut sk = Sketch::default();
                    for i in 0..4 {
                        sk.add_line(prof[i], prof[(i + 1) % 4], false);
                    }
                    let regs = sk.regions().map_err(|e| anyhow!("countersink profile: {e}"))?;
                    let face = rw_kernel::region_face(&sec, &regs[0])?;
                    solids.push(rw_kernel::revolve(&face, c, sec.y, 360.0)?);
                    main_top = cd * 0.5;
                }
                let bottom = if h.depth.is_some() { depth } else { depth + eps };
                solids.push(cyl(*p, r, main_top, bottom)?);
            }
            let tool = Tool::plain(solids, Op::Cut);
            combine(state, &tool, fs)?;
            state.tools.insert(f.id.clone(), tool);
        }
        FeatureKind::LinearPattern(p) => {
            let count = num(scope, &p.count, "count")?.round().max(1.0) as usize;
            let spacing = num(scope, &p.spacing, "spacing")?;
            let dir = normalize(p.direction)?;
            let (count2, spacing2, dir2) = match &p.direction2 {
                Some(d2) => (
                    p.count2.as_ref().map(|c| num(scope, c, "count2")).transpose()?.unwrap_or(1.0).round().max(1.0) as usize,
                    p.spacing2.as_ref().map(|s| num(scope, s, "spacing2")).transpose()?.unwrap_or(spacing),
                    normalize(*d2)?,
                ),
                None => (1, 0.0, [0.0; 3]),
            };
            let mut xforms = vec![];
            for j in 0..count2 {
                for i in 0..count {
                    if i == 0 && j == 0 {
                        continue;
                    }
                    let (a, b) = (i as f64 * spacing, j as f64 * spacing2);
                    xforms.push(Xform::Translate([
                        dir[0] * a + dir2[0] * b,
                        dir[1] * a + dir2[1] * b,
                        dir[2] * a + dir2[2] * b,
                    ]));
                }
            }
            instance(state, f, fs, &p.features, &xforms)?;
        }
        FeatureKind::CircularPattern(p) => {
            let count = num(scope, &p.count, "count")?.round().max(1.0) as usize;
            let total = num(scope, &p.angle, "angle")?;
            let step = if (total.abs() - 360.0).abs() < 1e-9 { total / count as f64 } else { total / (count.max(2) - 1) as f64 };
            let xforms: Vec<_> =
                (1..count).map(|i| Xform::Rotate { origin: p.axis_origin, axis: p.axis_direction, deg: step * i as f64 }).collect();
            instance(state, f, fs, &p.features, &xforms)?;
        }
        FeatureKind::Fillet(fl) => {
            let body = state.body.clone().ok_or_else(|| anyhow!("nothing to fillet"))?;
            let r = num(scope, &fl.radius, "radius")?;
            let idx = pick_edges(state, f, &body, &fl.edges, fl.all_edges)?;
            state.body = Some(rw_kernel::fillet(&body, &idx, r)?);
        }
        FeatureKind::Chamfer(c) => {
            let body = state.body.clone().ok_or_else(|| anyhow!("nothing to chamfer"))?;
            let d = num(scope, &c.distance, "distance")?;
            let idx = pick_edges(state, f, &body, &c.edges, c.all_edges)?;
            state.body = Some(rw_kernel::chamfer(&body, &idx, d)?);
        }
        FeatureKind::Shell(sh) => {
            let body = state.body.clone().ok_or_else(|| anyhow!("nothing to shell"))?;
            let t = num(scope, &sh.thickness, "thickness")?;
            let scale = rw_kernel::scale_of(&body);
            let mesh = rw_kernel::tessellate(&body, (scale / 300.0).max(0.01), false);
            let mut faces = vec![];
            for p in &sh.open_faces {
                let (fi, d) = rw_kernel::face_near(&mesh, *p).ok_or_else(|| anyhow!("body has no faces"))?;
                if d > (scale * 0.03).max(0.5) {
                    bail!("no face near ({:.3}, {:.3}, {:.3})", p[0], p[1], p[2]);
                }
                if !faces.contains(&fi) {
                    faces.push(fi);
                }
            }
            state.body = Some(rw_kernel::shell(&body, &faces, t)?);
        }
        FeatureKind::Loft(l) => {
            if l.sketches.len() < 2 {
                bail!("loft needs at least two sketches");
            }
            let mut sections = vec![];
            for sid in &l.sketches {
                let sk = sketch_of(state, sid)?;
                if sk.regions.len() != 1 || !sk.regions[0].holes.is_empty() {
                    bail!("loft sketch `{sid}` must contain exactly one closed profile without holes");
                }
                sections.push((sk.plane, sk.regions[0].outer.clone()));
            }
            let tool = Tool::plain(vec![rw_kernel::loft(&sections)?], l.op);
            combine(state, &tool, fs)?;
            state.tools.insert(f.id.clone(), tool);
        }
        FeatureKind::Import(im) => {
            let solid = rw_kernel::read_step(std::path::Path::new(&im.path))?;
            let tool = Tool::plain(vec![solid], im.op);
            combine(state, &tool, fs)?;
            state.tools.insert(f.id.clone(), tool);
        }
        FeatureKind::Mirror(m) => {
            let plane = resolve_plane(&m.plane, scope)?;
            let x = Xform::Mirror { origin: plane.origin, normal: plane.normal() };
            if m.features.is_empty() {
                let body = state.body.clone().ok_or_else(|| anyhow!("nothing to mirror"))?;
                let tool = Tool::plain(vec![rw_kernel::transform(&body, &x)], Op::Boss);
                combine(state, &tool, fs)?;
                state.tools.insert(f.id.clone(), tool);
            } else {
                instance(state, f, fs, &m.features, &[x])?;
            }
        }
    }
    Ok(())
}

/// Resolves edge references to kernel edge indices.
///
/// Each candidate edge is scored by its distance to the reference point plus
/// a signature mismatch (kind and length). The best edge is accepted when it
/// is essentially exact, or clearly better than the runner-up; otherwise the
/// reference is reported as ambiguous rather than silently picking an edge.
fn pick_edges(state: &mut State, f: &Feature, body: &Solid, refs: &[EdgeRef], all: bool) -> Result<Vec<usize>> {
    let es = rw_kernel::edges(body);
    if all {
        return Ok(es.iter().map(|e| e.index).collect());
    }
    if refs.is_empty() {
        bail!("no edges selected");
    }
    let scale = rw_kernel::scale_of(body);
    let exact = (scale * 0.01).max(0.05);
    let mut idx = vec![];
    let mut snapped = vec![];
    for r in refs {
        let p = r.at();
        let (kind, length) = match r {
            EdgeRef::Tagged { kind, length, .. } => (kind.clone(), *length),
            EdgeRef::Point(_) => (None, None),
        };
        let mut ds: Vec<(usize, f64)> = es
            .iter()
            .map(|e| {
                let mut score = e.distance_to(p);
                if kind.as_deref().is_some_and(|k| k != e.kind) {
                    score += scale;
                }
                if let Some(l) = length {
                    score += (e.length - l).abs();
                }
                (e.index, score)
            })
            .collect();
        ds.sort_by(|a, b| a.1.total_cmp(&b.1));
        let (i, d) = *ds.first().ok_or_else(|| anyhow!("body has no edges"))?;
        let d2 = ds.get(1).map(|x| x.1).unwrap_or(f64::INFINITY);
        let unambiguous = d < 0.4 * d2 && d < 0.25 * scale;
        if d > exact && !unambiguous {
            bail!(
                "edge reference ({:.3}, {:.3}, {:.3}) is ambiguous after the change (best match {d:.3}, next {d2:.3}); reselect the edge",
                p[0], p[1], p[2]
            );
        }
        let e = es.iter().find(|e| e.index == i).unwrap();
        snapped.push(EdgeRef::Tagged {
            at: e.mid.map(|v| (v * 1e4).round() / 1e4),
            kind: Some(e.kind.to_string()),
            length: Some((e.length * 1e4).round() / 1e4),
        });
        if !idx.contains(&i) {
            idx.push(i);
        }
    }
    state.snaps.insert(f.id.clone(), snapped);
    Ok(idx)
}

fn normalize(v: [f64; 3]) -> Result<[f64; 3]> {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-12 {
        bail!("direction vector is zero");
    }
    Ok([v[0] / l, v[1] / l, v[2] / l])
}

/// Applies transformed copies of the referenced features' tool bodies.
fn instance(state: &mut State, f: &Feature, fs: &mut FeatureStatus, features: &[String], xforms: &[Xform]) -> Result<()> {
    if features.is_empty() {
        bail!("no features selected to pattern");
    }
    let mut recorded: Option<Tool> = None;
    for fid in features {
        let src = state
            .tools
            .get(fid)
            .cloned()
            .ok_or_else(|| anyhow!("feature `{fid}` has no geometry to pattern (sketch, failed or suppressed?)"))?;
        let mut copy = Tool { solids: vec![], alts: vec![], op: src.op };
        for x in xforms {
            for (s, a) in src.solids.iter().zip(&src.alts) {
                copy.solids.push(rw_kernel::transform(s, x));
                copy.alts.push(a.as_ref().map(|a| rw_kernel::transform(a, x)));
            }
        }
        combine(state, &copy, fs)?;
        // Record as a tool so patterns can themselves be mirrored/patterned.
        match &mut recorded {
            None => recorded = Some(copy),
            Some(r) if r.op == copy.op => {
                r.solids.extend(copy.solids);
                r.alts.extend(copy.alts);
            }
            _ => {}
        }
    }
    if let Some(r) = recorded {
        state.tools.insert(f.id.clone(), r);
    }
    Ok(())
}

/// Sketch plane on a planar face. The origin is the projection of the world
/// origin onto the face, so sketch coordinates line up with the standard
/// planes (a +Y face behaves like the Top plane, offset).
pub fn plane_from_face(f: &rw_kernel::FaceInfo) -> Option<PlaneRef> {
    if f.kind != "plane" {
        return None;
    }
    let snap = |a: [f64; 3]| a.map(|x| if (x - x.round()).abs() < 1e-6 { x.round() } else { (x * 1e6).round() / 1e6 });
    let n = f.normal;
    let d = n[0] * f.centroid[0] + n[1] * f.centroid[1] + n[2] * f.centroid[2];
    Some(PlaneRef::Custom { origin: snap([n[0] * d, n[1] * d, n[2] * d]), normal: snap(n), x_dir: None })
}
