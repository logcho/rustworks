//! SOLIDWORKS dump → RustWorks part / assembly.
//!
//! Parts are rebuilt from their feature history when every feature can be
//! mapped. The rebuilt part is then checked against SOLIDWORKS' own mass
//! properties (volume and centre of mass). If anything is unsupported, fails
//! to rebuild or does not match, the part falls back to the exact STEP
//! geometry SOLIDWORKS exported, and the report says why.

use crate::dump::{self, Dump};
use anyhow::{anyhow, bail, Context, Result};
use rw_core::assembly::{self, Assembly, Component, Mate, MateKind, MateRef};
use rw_core::sketch::{Constraint, Entity, Sketch, Value};
use rw_core::*;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const M: f64 = 1000.0; // metres → mm

/// How `MathTransform.ArrayData`'s 3×3 block maps axes. SOLIDWORKS documents a
/// row-vector convention; both are tried and the result is verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Convention {
    Rows,
    Columns,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PartReport {
    pub name: String,
    pub file: PathBuf,
    /// "history" (parametric features) or "geometry" (STEP import).
    pub mode: String,
    pub features_translated: usize,
    /// Why history could not be used, unsupported features, dropped details.
    pub notes: Vec<String>,
    pub solidworks_volume_mm3: Option<f64>,
    pub rustworks_volume_mm3: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AssemblyReport {
    pub name: String,
    pub file: PathBuf,
    pub parts: Vec<PartReport>,
    pub components: usize,
    pub mates_translated: usize,
    pub notes: Vec<String>,
}

fn rot(a: &[f64], c: Convention) -> Result<[[f64; 3]; 3]> {
    if a.len() < 12 {
        bail!("transform has {} values, expected 13", a.len());
    }
    // r[i] = image of basis axis i
    Ok(match c {
        Convention::Rows => [[a[0], a[1], a[2]], [a[3], a[4], a[5]], [a[6], a[7], a[8]]],
        Convention::Columns => [[a[0], a[3], a[6]], [a[1], a[4], a[7]], [a[2], a[5], a[8]]],
    })
}

fn sketch_plane(to_model: &[f64], c: Convention) -> Result<kernel::Plane> {
    let r = rot(to_model, c)?;
    let o = [to_model[9] * M, to_model[10] * M, to_model[11] * M];
    let n = [
        r[0][1] * r[1][2] - r[0][2] * r[1][1],
        r[0][2] * r[1][0] - r[0][0] * r[1][2],
        r[0][0] * r[1][1] - r[0][1] * r[1][0],
    ];
    kernel::Plane::new(o, n, Some(r[0]))
}

fn plane_ref(p: &kernel::Plane) -> PlaneRef {
    let r = |v: [f64; 3]| v.map(|x| (x * 1e9).round() / 1e9);
    PlaneRef::Custom { origin: r(p.origin), normal: r(p.normal()), x_dir: Some(r(p.x)) }
}

fn mm3(p: [f64; 3]) -> [f64; 3] {
    p.map(|v| v * M)
}

/// Feature types that carry no geometry.
fn ignorable(kind: &str) -> bool {
    kind.ends_with("Folder")
        || matches!(
            kind,
            "RefPlane" | "RefAxis" | "RefPoint" | "CoordSys" | "OriginProfileFeature" | "AmbientLight" | "DirectionLight"
                | "PointLight" | "SpotLight" | "DetailCabinet" | "Attribute" | "MateGroup" | "Comments" | "SketchBlockDef"
                | "MaterialFolder" | "EqnFolder" | "HistoryFolder" | "SelectionSetFolder" | "SensorFolder"
        )
}

struct Builder<'a> {
    conv: Convention,
    part: Part,
    regen: Regenerator,
    /// SOLIDWORKS feature name → RustWorks feature id
    ids: HashMap<String, String>,
    /// sketch feature id → (SOLIDWORKS entity id → RustWorks entity id)
    sketch_ids: HashMap<String, HashMap<String, String>>,
    sketch_planes: HashMap<String, kernel::Plane>,
    notes: &'a mut Vec<String>,
}

impl Builder<'_> {
    fn push(&mut self, sw_name: &str, base: &str, kind: FeatureKind) -> String {
        let (id, _) = self.part.fresh_ids(base);
        self.part.features.push(Feature { id: id.clone(), name: sw_name.to_string(), suppressed: false, kind });
        self.ids.insert(sw_name.to_string(), id.clone());
        id
    }

    /// Rebuilds and returns (ok, volume) of the last feature.
    fn check_last(&mut self) -> (bool, f64, Option<String>) {
        let out = self.regen.regenerate(&self.part);
        let st = out.status.last().cloned();
        let ok = st.as_ref().is_some_and(|s| s.ok);
        (ok, out.mass.map_or(0.0, |m| m.volume), st.and_then(|s| s.error))
    }

    fn volume(&mut self) -> f64 {
        self.regen.regenerate(&self.part).mass.map_or(0.0, |m| m.volume)
    }

    fn sketch(&mut self, f: &dump::Feature, sk: &dump::Sketch) -> Result<()> {
        let plane = sketch_plane(&sk.to_model, self.conv)?;
        let (s, map) = convert_sketch(sk)?;
        let id = self.push(&f.name, "Sketch", FeatureKind::Sketch(SketchFeature { plane: plane_ref(&plane), sketch: s }));
        self.sketch_ids.insert(id.clone(), map);
        self.sketch_planes.insert(id, plane);
        Ok(())
    }

    fn profile(&self, f: &dump::Feature) -> Result<String> {
        let name = f.profile.as_ref().ok_or_else(|| anyhow!("no profile sketch"))?;
        self.ids.get(name).cloned().ok_or_else(|| anyhow!("profile sketch `{name}` was not translated"))
    }

    fn extrude(&mut self, f: &dump::Feature, e: &dump::Extrude) -> Result<()> {
        if e.thin || e.draft {
            bail!("thin and drafted extrudes are not supported");
        }
        let sketch = self.profile(f)?;
        let op = if f.kind == "Cut" { Op::Cut } else { Op::Boss };
        let end = |c: i32| -> Result<EndCondition> {
            Ok(match c {
                0 => EndCondition::Blind,
                1 | 9 => EndCondition::ThroughAll,
                6 => EndCondition::MidPlane,
                other => bail!("extrude end condition {other} (up to vertex/surface/next/body) is not supported"),
            })
        };
        let e1 = end(e.end1)?;
        let base = if op == Op::Cut { "Cut-Extrude" } else { "Extrude" };
        let mk = |end: EndCondition, depth: f64, reverse: bool| {
            FeatureKind::Extrude(Extrude { sketch: sketch.clone(), depth: Value::Num(round(depth * M)), end, reverse, op })
        };
        let before = self.volume();
        let id = self.push(&f.name, base, mk(e1, e.depth1, e.reverse));
        if op == Op::Cut {
            // SOLIDWORKS' default cut direction depends on context: try both
            // and keep the one that removes more material.
            let flip = |b: &mut Self| {
                if let Some(Feature { kind: FeatureKind::Extrude(x), .. }) = b.part.features.last_mut() {
                    x.reverse = !x.reverse;
                }
            };
            let (ok_a, v_a, _) = self.check_last();
            flip(self);
            let (ok_b, v_b, err) = self.check_last();
            let removed = |ok: bool, v: f64| if ok { before - v } else { f64::NEG_INFINITY };
            let (ra, rb) = (removed(ok_a, v_a), removed(ok_b, v_b));
            if ra > rb {
                flip(self);
            }
            if ra.max(rb) <= 1e-6 {
                bail!("cut removes no material in either direction{}", err.map(|e| format!(": {e}")).unwrap_or_default());
            }
        }
        if e.both && e1 != EndCondition::MidPlane {
            let e2 = end(e.end2.unwrap_or(0))?;
            let reverse = if let Some(Feature { kind: FeatureKind::Extrude(x), .. }) = self.part.features.iter().find(|x| x.id == id) { !x.reverse } else { true };
            let name2 = format!("{} (direction 2)", f.name);
            self.push(&name2, base, mk(e2, e.depth2.unwrap_or(e.depth1), reverse));
            self.ids.insert(f.name.clone(), id); // patterns refer to the first
        }
        Ok(())
    }

    fn revolve(&mut self, f: &dump::Feature, r: &dump::Revolve) -> Result<()> {
        if r.thin {
            bail!("thin revolves are not supported");
        }
        let deg = r.angle.to_degrees();
        if r.kind != 0 && deg < 359.999 {
            bail!("mid-plane / two-direction revolves are not supported");
        }
        let sketch = self.profile(f)?;
        let axis = if let Some(seg) = &r.axis_segment {
            let id = self.sketch_ids.get(&sketch).and_then(|m| m.get(seg)).ok_or_else(|| anyhow!("revolve axis is not in the profile sketch"))?;
            AxisRef::Entity(id.clone())
        } else if let Some([a, b]) = r.axis_points {
            let pl = self.sketch_planes[&sketch];
            let (p, q) = (pl.to2d(mm3(a)), pl.to2d(mm3(b)));
            AxisRef::Explicit { origin: p, direction: [q[0] - p[0], q[1] - p[1]] }
        } else {
            bail!("revolve axis missing");
        };
        let op = if f.kind == "RevCut" { Op::Cut } else { Op::Boss };
        self.push(&f.name, if op == Op::Cut { "Cut-Revolve" } else { "Revolve" }, FeatureKind::Revolve(Revolve { sketch, axis, angle: Value::Num(round(deg)), op }));
        Ok(())
    }

    fn edges(e: &dump::EdgeFeature) -> Result<Vec<EdgeRef>> {
        if e.variable {
            bail!("variable-size fillets are not supported");
        }
        if e.edges.is_empty() {
            bail!("only edge selections are supported (face/feature selections are not)");
        }
        Ok(e.edges.iter().map(|p| EdgeRef::Point(mm3(*p).map(|v| (v * 1e4).round() / 1e4))).collect())
    }

    fn fillet(&mut self, f: &dump::Feature, e: &dump::EdgeFeature) -> Result<()> {
        let edges = Self::edges(e)?;
        self.push(&f.name, "Fillet", FeatureKind::Fillet(Fillet { edges, all_edges: false, radius: Value::Num(round(e.size * M)) }));
        Ok(())
    }

    fn chamfer(&mut self, f: &dump::Feature, e: &dump::EdgeFeature) -> Result<()> {
        let equal = e.size2.is_none_or(|d| (d - e.size).abs() < 1e-9) && e.angle.is_none_or(|a| (a - std::f64::consts::FRAC_PI_4).abs() < 1e-6);
        if !equal {
            bail!("only equal-distance chamfers are supported");
        }
        let edges = Self::edges(e)?;
        self.push(&f.name, "Chamfer", FeatureKind::Chamfer(Chamfer { edges, all_edges: false, distance: Value::Num(round(e.size * M)) }));
        Ok(())
    }

    fn shell(&mut self, f: &dump::Feature, s: &dump::ShellData) -> Result<()> {
        if s.outward {
            bail!("outward shells are not supported");
        }
        let open_faces = s.faces.iter().map(|p| mm3(*p)).collect();
        self.push(&f.name, "Shell", FeatureKind::Shell(Shell { open_faces, thickness: Value::Num(round(s.thickness * M)) }));
        Ok(())
    }

    fn hole(&mut self, f: &dump::Feature, h: &dump::HoleData) -> Result<()> {
        let plane = sketch_plane(&h.sketch.to_model, self.conv)?;
        // hole centres: sketch points not used as segment ends
        let used: Vec<&String> = h.sketch.segments.iter().flat_map(|s| [s.start.as_ref(), s.end.as_ref(), s.center.as_ref()].into_iter().flatten()).collect();
        let points: Vec<[f64; 2]> = h.sketch.points.iter().filter(|p| !used.contains(&&p.id)).map(|p| [round(p.x * M), round(p.y * M)]).collect();
        if points.is_empty() {
            bail!("hole has no centre points");
        }
        let (counterbore, countersink) = match h.hole_type {
            0 => {
                let (d, t) = (h.cbore_diameter.ok_or_else(|| anyhow!("counterbore size missing"))?, h.cbore_depth.unwrap_or(0.0));
                (Some(Counterbore { diameter: Value::Num(round(d * M)), depth: Value::Num(round(t * M)) }), None)
            }
            1 => {
                let d = h.csink_diameter.ok_or_else(|| anyhow!("countersink size missing"))?;
                (None, Some(Countersink { diameter: Value::Num(round(d * M)), angle: Value::Num(round(h.csink_angle.unwrap_or(std::f64::consts::FRAC_PI_2).to_degrees())) }))
            }
            _ => (None, None),
        };
        let depth = match h.end {
            0 => Some(Value::Num(round(h.depth * M))),
            1 | 9 => None,
            other => bail!("hole end condition {other} is not supported"),
        };
        self.push(&f.name, "Hole", FeatureKind::Hole(Hole { plane: plane_ref(&plane), points, diameter: Value::Num(round(h.diameter * M)), depth, counterbore, countersink }));
        Ok(())
    }

    fn feature_ids(&self, names: &[String]) -> Result<Vec<String>> {
        names.iter().map(|n| self.ids.get(n).cloned().ok_or_else(|| anyhow!("patterned feature `{n}` was not translated"))).collect()
    }

    fn linear_pattern(&mut self, f: &dump::Feature, p: &dump::LinearPatternData) -> Result<()> {
        if p.skipped > 0 {
            bail!("patterns with skipped instances are not supported");
        }
        let features = self.feature_ids(&p.features)?;
        let lp = LinearPattern {
            features,
            direction: p.dir1,
            spacing: Value::Num(round(p.spacing1 * M)),
            count: Value::Num(p.count1 as f64),
            direction2: p.dir2,
            spacing2: p.spacing2.map(|s| Value::Num(round(s * M))),
            count2: p.count2.map(|c| Value::Num(c as f64)),
        };
        self.push(&f.name, "LPattern", FeatureKind::LinearPattern(lp));
        Ok(())
    }

    fn circular_pattern(&mut self, f: &dump::Feature, p: &dump::CircularPatternData) -> Result<()> {
        if p.skipped > 0 {
            bail!("patterns with skipped instances are not supported");
        }
        let features = self.feature_ids(&p.features)?;
        let total = if p.equal { p.angle.to_degrees() } else { p.angle.to_degrees() * (p.count.max(2) - 1) as f64 };
        let cp = CircularPattern { features, axis_origin: mm3(p.axis_origin), axis_direction: p.axis_dir, count: Value::Num(p.count as f64), angle: Value::Num(round(total)) };
        self.push(&f.name, "CirPattern", FeatureKind::CircularPattern(cp));
        Ok(())
    }

    fn mirror(&mut self, f: &dump::Feature, m: &dump::MirrorData) -> Result<()> {
        let features = if m.body { vec![] } else { self.feature_ids(&m.features)? };
        let plane = PlaneRef::Custom { origin: mm3(m.origin), normal: m.normal, x_dir: None };
        self.push(&f.name, "Mirror", FeatureKind::Mirror(Mirror { features, plane }));
        Ok(())
    }

    fn feature(&mut self, f: &dump::Feature) -> Result<bool> {
        if let Some(e) = &f.error {
            bail!("SOLIDWORKS bridge could not read it ({e})");
        }
        let need = |o: bool| if o { Ok(()) } else { Err(anyhow!("definition missing from the dump")) };
        match f.kind.as_str() {
            "ProfileFeature" => {
                need(f.sketch.is_some())?;
                self.sketch(f, f.sketch.as_ref().unwrap())?
            }
            "Extrusion" | "Cut" | "Boss" | "ICE" => {
                need(f.extrude.is_some())?;
                self.extrude(f, f.extrude.as_ref().unwrap())?
            }
            "Revolution" | "RevCut" => {
                need(f.revolve.is_some())?;
                self.revolve(f, f.revolve.as_ref().unwrap())?
            }
            "Fillet" => {
                need(f.fillet.is_some())?;
                self.fillet(f, f.fillet.as_ref().unwrap())?
            }
            "Chamfer" => {
                need(f.chamfer.is_some())?;
                self.chamfer(f, f.chamfer.as_ref().unwrap())?
            }
            "Shell" => {
                need(f.shell.is_some())?;
                self.shell(f, f.shell.as_ref().unwrap())?
            }
            "HoleWzd" => {
                need(f.hole.is_some())?;
                self.hole(f, f.hole.as_ref().unwrap())?
            }
            "LPattern" => {
                need(f.linear_pattern.is_some())?;
                self.linear_pattern(f, f.linear_pattern.as_ref().unwrap())?
            }
            "CirPattern" => {
                need(f.circular_pattern.is_some())?;
                self.circular_pattern(f, f.circular_pattern.as_ref().unwrap())?
            }
            "MirrorPattern" | "MirrorSolid" => {
                need(f.mirror.is_some())?;
                self.mirror(f, f.mirror.as_ref().unwrap())?
            }
            k if ignorable(k) => return Ok(false),
            k => bail!("feature type `{k}` is not supported"),
        }
        Ok(true)
    }
}

fn round(v: f64) -> f64 {
    let r = (v * 1e6).round() / 1e6;
    if (r - v).abs() < 1e-9 { r } else { v }
}

/// SOLIDWORKS sketch → RustWorks sketch (+ id map). Geometry keeps its solved
/// positions; driving dimensions and horizontal/vertical lines are re-created.
fn convert_sketch(sk: &dump::Sketch) -> Result<(Sketch, HashMap<String, String>)> {
    let mut s = Sketch::default();
    let mut map: HashMap<String, String> = HashMap::new();
    for (i, p) in sk.points.iter().enumerate() {
        let id = format!("p{}", i + 1);
        s.entities.push(Entity::Point { id: id.clone(), x: round(p.x * M), y: round(p.y * M), fixed: false });
        map.insert(p.id.clone(), id);
    }
    let pid = |map: &HashMap<String, String>, o: &Option<String>| -> Result<String> {
        let k = o.as_ref().ok_or_else(|| anyhow!("segment endpoint missing"))?;
        map.get(k).cloned().ok_or_else(|| anyhow!("unknown sketch point {k}"))
    };
    let mut lines = vec![];
    for (i, g) in sk.segments.iter().enumerate() {
        let e = match g.kind.as_str() {
            "line" => {
                let id = format!("l{}", i + 1);
                lines.push(id.clone());
                Entity::Line { id, start: pid(&map, &g.start)?, end: pid(&map, &g.end)?, construction: g.construction }
            }
            "arc" => {
                let (mut a, mut b) = (pid(&map, &g.start)?, pid(&map, &g.end)?);
                if g.dir.unwrap_or(1) < 0 {
                    std::mem::swap(&mut a, &mut b); // RustWorks arcs run counter-clockwise
                }
                Entity::Arc { id: format!("a{}", i + 1), center: pid(&map, &g.center)?, start: a, end: b, construction: g.construction }
            }
            "circle" => Entity::Circle {
                id: format!("c{}", i + 1),
                center: pid(&map, &g.center)?,
                radius: round(g.radius.ok_or_else(|| anyhow!("circle radius missing"))? * M),
                construction: g.construction,
            },
            "spline" => Entity::Spline {
                id: format!("s{}", i + 1),
                points: g.points.iter().map(|p| map.get(p).cloned().ok_or_else(|| anyhow!("unknown spline point {p}"))).collect::<Result<_>>()?,
                construction: g.construction,
            },
            other if g.construction => {
                let _ = other;
                continue; // construction ellipses etc. do not affect the shape
            }
            other => bail!("sketch entity `{other}` (ellipse, parabola, text…) is not supported"),
        };
        map.insert(g.id.clone(), e.id().to_string());
        s.entities.push(e);
    }
    // horizontal / vertical lines
    for l in &lines {
        if let Some(Entity::Line { start, end, .. }) = s.entity(l).cloned() {
            let (a, b) = (s.point_xy(&start).unwrap(), s.point_xy(&end).unwrap());
            if (a[1] - b[1]).abs() < 1e-7 {
                s.constraints.push(Constraint::Horizontal { line: l.clone() });
            } else if (a[0] - b[0]).abs() < 1e-7 {
                s.constraints.push(Constraint::Vertical { line: l.clone() });
            }
        }
    }
    // driving dimensions (swDimensionType_e)
    for d in &sk.dims {
        let ents: Vec<String> = d.entities.iter().filter_map(|e| map.get(e).cloned()).collect();
        let is = |id: &str, k: char| id.starts_with(k);
        let mm = Value::Num(round(d.value * M));
        let c = match (d.kind, ents.as_slice()) {
            (2, [l]) if is(l, 'l') => Some(Constraint::Distance { a: l.clone(), b: None, value: mm }),
            (2, [a, b]) => Some(Constraint::Distance { a: a.clone(), b: Some(b.clone()), value: mm }),
            (11, [a, b]) if is(a, 'p') && is(b, 'p') => Some(Constraint::HorizontalDistance { a: a.clone(), b: b.clone(), value: mm }),
            (12, [a, b]) if is(a, 'p') && is(b, 'p') => Some(Constraint::VerticalDistance { a: a.clone(), b: b.clone(), value: mm }),
            (5, [e]) if is(e, 'a') || is(e, 'c') => Some(Constraint::Radius { entity: e.clone(), value: mm }),
            (6, [e]) if is(e, 'a') || is(e, 'c') => Some(Constraint::Diameter { entity: e.clone(), value: mm }),
            (3, [a, b]) if is(a, 'l') && is(b, 'l') => {
                // keep the measured (unsigned) angle between the lines as found
                Some(Constraint::Angle { a: a.clone(), b: b.clone(), value: Value::Num(round(signed_angle(&s, a, b).unwrap_or(d.value.to_degrees()))) })
            }
            _ => None,
        };
        s.constraints.extend(c);
    }
    Ok((s, map))
}

/// Current counter-clockwise angle (deg) from line a to line b.
fn signed_angle(s: &Sketch, a: &str, b: &str) -> Option<f64> {
    let dir = |l: &str| -> Option<[f64; 2]> {
        match s.entity(l)? {
            Entity::Line { start, end, .. } => {
                let (p, q) = (s.point_xy(start)?, s.point_xy(end)?);
                Some([q[0] - p[0], q[1] - p[1]])
            }
            _ => None,
        }
    };
    let (u, v) = (dir(a)?, dir(b)?);
    Some((u[0] * v[1] - u[1] * v[0]).atan2(u[0] * v[0] + u[1] * v[1]).to_degrees())
}

fn step_path(d: &Dump, dump_dir: &Path) -> Option<PathBuf> {
    let s = d.step.as_ref()?;
    let p = PathBuf::from(s);
    let p = if p.is_absolute() { p } else { dump_dir.join(p) };
    p.exists().then(|| std::fs::canonicalize(&p).unwrap_or(p))
}

/// Tries the parametric translation with one transform convention.
fn try_history(d: &Dump, conv: Convention, notes: &mut Vec<String>) -> Result<(Part, usize)> {
    let mut b = Builder {
        conv,
        part: Part::new(&d.name),
        regen: Regenerator::new(),
        ids: HashMap::new(),
        sketch_ids: HashMap::new(),
        sketch_planes: HashMap::new(),
        notes,
    };
    let mut n = 0;
    for f in &d.features {
        if f.suppressed {
            if !ignorable(&f.kind) {
                b.notes.push(format!("{} ({}) is suppressed in SOLIDWORKS and was left out", f.name, f.kind));
            }
            continue;
        }
        if b.feature(f).with_context(|| format!("{} ({})", f.name, f.kind))? {
            n += 1;
        }
    }
    let out = b.regen.regenerate(&b.part);
    let errs = out.errors();
    if !errs.is_empty() {
        bail!("rebuild errors: {}", errs.join("; "));
    }
    Ok((out.solved, n))
}

/// Volume (mm³) and centre of mass (mm) of a part.
fn mass_of(p: &Part) -> Option<(f64, [f64; 3])> {
    let out = Regenerator::new().regenerate(p);
    out.mass.map(|m| (m.volume, m.centroid))
}

/// Whether RustWorks' result matches SOLIDWORKS' mass properties.
fn matches(d: &Dump, vol: f64, com: [f64; 3]) -> Result<(), String> {
    if let Some(v) = d.volume_m3 {
        let sw = v * 1e9;
        if (vol - sw).abs() > sw.abs() * 2e-3 + 1e-3 {
            return Err(format!("volume {vol:.3} mm³ differs from SOLIDWORKS' {sw:.3} mm³"));
        }
        if let Some(c) = d.center_of_mass_m {
            let c = mm3(c);
            let size = sw.abs().cbrt().max(1.0);
            let dist = ((com[0] - c[0]).powi(2) + (com[1] - c[1]).powi(2) + (com[2] - c[2]).powi(2)).sqrt();
            if dist > size * 5e-3 + 1e-3 {
                return Err(format!("centre of mass is {dist:.3} mm away from SOLIDWORKS' (mirrored or rotated feature?)"));
            }
        }
    }
    Ok(())
}

/// Translates a part dump; writes `<out_dir>/<name>.rwpart`.
pub fn translate_part(d: &Dump, dump_dir: &Path, out_dir: &Path) -> Result<(PartReport, Option<Convention>)> {
    if d.kind != "part" {
        bail!("`{}` is a SOLIDWORKS {}, not a part", d.name, d.kind);
    }
    std::fs::create_dir_all(out_dir)?;
    let file = out_dir.join(format!("{}.rwpart", d.name));
    let mut rep = PartReport { name: d.name.clone(), file: file.clone(), solidworks_volume_mm3: d.volume_m3.map(|v| v * 1e9), ..Default::default() };
    rep.notes.extend(d.warnings.iter().map(|w| format!("bridge: {w}")));
    // 1. parametric history, verified against SOLIDWORKS
    let mut first_err: Option<String> = None;
    for conv in [Convention::Rows, Convention::Columns] {
        let mut notes = vec![];
        match try_history(d, conv, &mut notes) {
            Ok((part, n)) => {
                let Some((vol, com)) = mass_of(&part) else {
                    first_err.get_or_insert("rebuilt part has no solid body".into());
                    continue;
                };
                match matches(d, vol, com) {
                    Ok(()) => {
                        model::write_part(&file, &part)?;
                        rep.mode = "history".into();
                        rep.features_translated = n;
                        rep.rustworks_volume_mm3 = Some(vol);
                        rep.notes.extend(notes);
                        if d.volume_m3.is_none() {
                            rep.notes.push("SOLIDWORKS mass properties unavailable: the rebuilt part could not be verified".into());
                        }
                        return Ok((rep, Some(conv)));
                    }
                    Err(e) => {
                        first_err.get_or_insert(e);
                    }
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let unsupported = msg.contains("not supported") || msg.contains("could not read");
                first_err.get_or_insert(msg);
                if unsupported {
                    break; // the other convention will not help
                }
            }
        }
    }
    // 2. exact geometry from STEP
    let step = step_path(d, dump_dir).ok_or_else(|| anyhow!("history could not be translated ({}) and the dump has no STEP file", first_err.clone().unwrap_or_default()))?;
    let mut part = Part::new(&d.name);
    part.features.push(Feature { id: "f1".into(), name: format!("{} (SOLIDWORKS geometry)", d.name), suppressed: false, kind: FeatureKind::Import(Import { path: step.display().to_string(), op: Op::Boss }) });
    let (vol, _) = mass_of(&part).ok_or_else(|| anyhow!("STEP file {} has no solid geometry", step.display()))?;
    model::write_part(&file, &part)?;
    rep.mode = "geometry".into();
    rep.rustworks_volume_mm3 = Some(vol);
    rep.notes.insert(0, format!("imported as exact geometry without feature history: {}", first_err.unwrap_or_default()));
    if let Err(e) = matches(d, vol, [0.0; 3]).or_else(|e| if e.contains("centre") { Ok(()) } else { Err(e) }) {
        rep.notes.push(format!("check: {e}"));
    }
    Ok((rep, None))
}

/// Reads a dump file.
pub fn read_dump(path: &Path) -> Result<Dump> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let d: Dump = serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
    if !d.format.is_empty() && d.format != dump::DUMP_FORMAT {
        bail!("unsupported dump format `{}`", d.format);
    }
    Ok(d)
}

/// Rigid placement (rotation vector in degrees, position in mm) from a SOLIDWORKS transform.
fn placement(t: &[f64], conv: Convention) -> Result<([f64; 3], [f64; 3], [[f64; 3]; 3])> {
    let r = rot(t, conv)?; // r[i] = image of axis i
    let m = [[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]]; // column form
    let rv = rw_core::assembly::matrix_to_rotvec(&m).map(|v| (v.to_degrees() * 1e6).round() / 1e6);
    let pos = [t[9] * M, t[10] * M, t[11] * M].map(|v| (v * 1e6).round() / 1e6);
    Ok((pos, rv, m))
}

/// Translates an assembly dump (and the part dumps it references).
pub fn translate_assembly(d: &Dump, dump_dir: &Path, out_dir: &Path) -> Result<AssemblyReport> {
    if d.kind != "assembly" {
        bail!("`{}` is not a SOLIDWORKS assembly", d.name);
    }
    std::fs::create_dir_all(out_dir)?;
    let file = out_dir.join(format!("{}.rwasm", d.name));
    let mut rep = AssemblyReport { name: d.name.clone(), file: file.clone(), ..Default::default() };
    rep.notes.extend(d.warnings.iter().map(|w| format!("bridge: {w}")));
    // parts (each unique dump once)
    let mut part_files: HashMap<String, PathBuf> = HashMap::new();
    let mut conv = Convention::Rows;
    for c in &d.components {
        let Some(dp) = &c.dump else {
            rep.notes.push(format!("component {} has no part dump (could not be opened in SOLIDWORKS); skipped", c.name));
            continue;
        };
        if part_files.contains_key(dp) {
            continue;
        }
        let pd = read_dump(&dump_dir.join(dp))?;
        let (pr, pc) = translate_part(&pd, dump_dir, out_dir)?;
        if let Some(pc) = pc {
            conv = pc;
        }
        part_files.insert(dp.clone(), pr.file.clone());
        rep.parts.push(pr);
    }
    let mut asm = Assembly::new(&d.name);
    let mut transforms: HashMap<String, [[f64; 3]; 3]> = HashMap::new();
    let mut origins: HashMap<String, [f64; 3]> = HashMap::new();
    for (i, c) in d.components.iter().enumerate() {
        let Some(pf) = c.dump.as_ref().and_then(|dp| part_files.get(dp)) else { continue };
        let (position, rotation, m) = placement(&c.transform, conv)?;
        let id = format!("c{}", i + 1);
        transforms.insert(c.name.clone(), m);
        origins.insert(c.name.clone(), position);
        asm.components.push(Component {
            id: id.clone(),
            name: c.name.clone(),
            path: pf.file_name().unwrap().to_string_lossy().into_owned(),
            position,
            rotation,
            fixed: c.fixed,
            suppressed: c.suppressed,
            color: None,
        });
    }
    rep.components = asm.components.len();
    // mates: references become points on faces in part coordinates
    let mut bodies: HashMap<String, Option<kernel::Mesh>> = HashMap::new();
    for m in &d.mates {
        match translate_mate(m, &asm, &transforms, &origins, &part_files, d, &mut bodies) {
            Ok(mut mate) => {
                mate.id = format!("m{}", asm.mates.len() + 1);
                asm.mates.push(mate);
                rep.mates_translated += 1;
            }
            Err(e) => rep.notes.push(format!("mate {} not translated: {e:#}", m.name)),
        }
    }
    assembly::write_assembly(&file, &asm)?;
    // load once to report solve problems
    let model = AsmModel::open(&file)?;
    for e in model.out.errors() {
        rep.notes.push(format!("after import: {e}"));
    }
    Ok(rep)
}

fn translate_mate(
    m: &dump::Mate,
    asm: &Assembly,
    transforms: &HashMap<String, [[f64; 3]; 3]>,
    origins: &HashMap<String, [f64; 3]>,
    part_files: &HashMap<String, PathBuf>,
    d: &Dump,
    bodies: &mut HashMap<String, Option<kernel::Mesh>>,
) -> Result<Mate> {
    let kind = match m.kind {
        0 => MateKind::Coincident,
        1 => MateKind::Concentric,
        2 => MateKind::Perpendicular,
        3 => MateKind::Parallel,
        5 => MateKind::Distance,
        6 => MateKind::Angle,
        other => bail!("mate type {other} (tangent, gear, cam, width, …) is not supported"),
    };
    if m.entities.len() != 2 {
        bail!("{} references (expected 2)", m.entities.len());
    }
    let mut refs = vec![];
    for e in &m.entities {
        let comp = asm.component(&e.component).ok_or_else(|| anyhow!("component {} not imported", e.component))?;
        let mut r = MateRef { component: comp.id.clone(), face: None, plane: None, axis: None, point: None };
        if e.ref_type == 4 {
            let name = e.ref_name.clone().unwrap_or_default();
            r.plane = Some(match name.as_str() {
                "Front Plane" | "Front" => StdPlane::Front,
                "Top Plane" | "Top" => StdPlane::Top,
                "Right Plane" | "Right" => StdPlane::Right,
                other => bail!("reference plane `{other}` is not one of the standard planes"),
            });
        } else if e.ref_type == 5 {
            bail!("mates to reference axes are not supported");
        } else {
            let p = e.point.or_else(|| (e.params.len() >= 3).then(|| [e.params[0], e.params[1], e.params[2]])).ok_or_else(|| anyhow!("reference has no location"))?;
            let p = mm3(p);
            // the point may be in assembly or part space: use whichever lies on the part
            let rm = transforms[&e.component];
            let o = origins[&e.component];
            let dlt = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
            let local = [0, 1, 2].map(|j| rm[0][j] * dlt[0] + rm[1][j] * dlt[1] + rm[2][j] * dlt[2]);
            let src = d.components.iter().find(|c| c.name == e.component).and_then(|c| c.dump.clone()).unwrap_or_default();
            let mesh = bodies.entry(src.clone()).or_insert_with(|| {
                let pf = part_files.get(&src)?;
                let part = model::read_part(pf).ok()?;
                Regenerator::new().regenerate(&part).mesh
            });
            let mesh = mesh.as_ref().ok_or_else(|| anyhow!("component {} has no geometry", e.component))?;
            let dist = |q: [f64; 3]| kernel::face_near(mesh, q).map_or(f64::INFINITY, |x| x.1);
            let (best, dbest) = [local, p].into_iter().map(|q| (q, dist(q))).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            if dbest > 0.5 {
                bail!("could not locate the referenced face on {} ({dbest:.2} mm away)", e.component);
            }
            r.face = Some(best.map(|v| (v * 1e4).round() / 1e4));
        }
        refs.push(r);
    }
    let value = match kind {
        MateKind::Distance => Some(Value::Num(round(m.value.ok_or_else(|| anyhow!("distance missing"))? * M))),
        MateKind::Angle => Some(Value::Num(round(m.value.ok_or_else(|| anyhow!("angle missing"))?.to_degrees()))),
        _ => None,
    };
    let b = refs.pop().unwrap();
    let a = refs.pop().unwrap();
    Ok(Mate { id: String::new(), name: m.name.clone(), kind, a, b, value, aligned: m.align == 0, suppressed: m.suppressed })
}

