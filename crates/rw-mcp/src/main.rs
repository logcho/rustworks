//! RustWorks MCP server: exposes parametric part modeling to AI agents over
//! the Model Context Protocol (JSON-RPC 2.0, newline-delimited, stdio).

#![recursion_limit = "512"]

mod asm;
mod drawing;
mod tools;

use anyhow::{anyhow, bail, Context, Result};
use rw_core::sketch::{Constraint, Entity, Sketch};
use rw_core::assembly::AsmModel;
use rw_core::*;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value as J};
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL: &str = "2025-06-18";

struct Server {
    model: Option<Model>,
    asm: Option<AsmModel>,
    /// The assembly (not the part) is the active document for render/export/etc.
    focus_asm: bool,
    /// Active drawing file.
    drawing: Option<PathBuf>,
}

enum Reply {
    Text(J),
    TextAndImage(J, Vec<u8>),
}

fn main() -> Result<()> {
    // Positional arg: optional part file to open at startup.
    let mut server = Server { model: None, asm: None, focus_asm: false, drawing: None };
    if let Some(p) = std::env::args().nth(1) {
        let path = PathBuf::from(p);
        if path.extension().is_some_and(|e| e == "rwasm") {
            server.asm = Some(if path.exists() { AsmModel::open(&path)? } else {
                let mut m = AsmModel::new(Assembly::new(path.file_stem().and_then(|s| s.to_str()).unwrap_or("assembly")), Some(path.clone()));
                m.save()?;
                m
            });
            server.focus_asm = true;
        } else {
        server.model = Some(if path.exists() {
            Model::open(&path)?
        } else {
            let mut m = Model::new(Part::new(path.file_stem().and_then(|s| s.to_str()).unwrap_or("part")));
            m.save_as(&path)?;
            m
        });
        }
    }
    let stdin = std::io::stdin();
    let mut stdout = protocol_stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: J = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let r = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":format!("parse error: {e}")}});
                writeln!(stdout, "{r}")?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(resp) = server.handle(&msg) {
            writeln!(stdout, "{resp}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

impl Server {
    fn handle(&mut self, msg: &J) -> Option<J> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?; // notifications get no response
        let result: std::result::Result<J, (i64, String)> = match method {
            "initialize" => {
                let ver = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL);
                Ok(json!({
                    "protocolVersion": ver,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "rustworks", "title": "RustWorks CAD", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::list()})),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call(&name, &args) {
                    Ok(Reply::Text(v)) => json!({"content":[{"type":"text","text":pretty(&v)}],"isError":false}),
                    Ok(Reply::TextAndImage(v, png)) => json!({"content":[
                        {"type":"image","data":base64(&png),"mimeType":"image/png"},
                        {"type":"text","text":pretty(&v)}
                    ],"isError":false}),
                    Err(e) => json!({"content":[{"type":"text","text":format!("Error: {e:#}")}],"isError":true}),
                })
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err((code, m)) => json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":m}}),
        })
    }

    fn model(&mut self) -> Result<&mut Model> {
        if self.model.is_none() {
            let path = PathBuf::from("part.rwpart");
            let mut m = if path.exists() { Model::open(&path)? } else { Model::new(Part::new("part")) };
            m.save_as(&path)?;
            self.model = Some(m);
        }
        let m = self.model.as_mut().unwrap();
        m.sync_from_disk()?; // pick up edits made in the GUI
        Ok(m)
    }

    fn call(&mut self, name: &str, a: &J) -> Result<Reply> {
        if let Some(r) = self.asm_call(name, a) {
            return r;
        }
        if let Some(r) = self.drawing_call(name, a) {
            return r;
        }
        match name {
            "new_part" => {
                let nm: String = req(a, "name")?;
                let path: PathBuf = opt(a, "path")?.unwrap_or_else(|| PathBuf::from(format!("{nm}.rwpart")));
                let mut m = Model::new(Part::new(&nm));
                m.save_as(&path)?;
                self.model = Some(m);
                self.focus_asm = false;
                Ok(Reply::Text(json!({"created": nm, "path": abs(&path)})))
            }
            "open_part" => {
                let path: PathBuf = req(a, "path")?;
                let m = Model::open(&path)?;
                self.model = Some(m);
                self.focus_asm = false;
                let m = self.model.as_mut().unwrap();
                Ok(Reply::Text(summary(m, json!({"opened": abs(&path)}))))
            }
            "get_part" => {
                let m = self.model()?;
                Ok(Reply::Text(json!({
                    "path": m.path.as_ref().map(|p| abs(p)),
                    "document": m.part,
                    "status": m.out.status,
                    "volume": m.out.mass.map(|x| x.volume),
                })))
            }
            "set_parameters" => {
                let ps: Map<String, J> = req(a, "parameters")?;
                let m = self.model()?;
                m.edit(|p| {
                    for (k, v) in ps {
                        if v.is_null() {
                            p.parameters.remove(&k);
                        } else {
                            p.parameters.insert(k, serde_json::from_value(v)?);
                        }
                    }
                    Ok(())
                })?;
                let params = m.part.parameters.clone();
                Ok(Reply::Text(summary(m, json!({"parameters": params}))))
            }
            "create_sketch" => {
                let shapes: Vec<J> = req(a, "geometry")?;
                let cons: Vec<Constraint> = opt(a, "constraints")?.unwrap_or_default();
                let name: Option<String> = opt(a, "name")?;
                let m = self.model()?;
                let plane = plane_arg(m, a.get("plane").ok_or_else(|| anyhow!("missing `plane`"))?)?;
                let mut sk = Sketch::default();
                let created = add_shapes(&mut sk, &shapes)?;
                sk.constraints.extend(cons);
                let id = m.edit(|p| {
                    let (id, auto) = p.fresh_ids("Sketch");
                    p.features.push(Feature {
                        id: id.clone(),
                        name: name.unwrap_or(auto),
                        suppressed: false,
                        kind: FeatureKind::Sketch(SketchFeature { plane, sketch: sk }),
                    });
                    Ok(id)
                })?;
                let info = sketch_info(m, &id)?;
                Ok(Reply::Text(summary(m, json!({"sketch": id, "created_entities": created, "sketch_state": info}))))
            }
            "edit_sketch" => {
                let sid: String = req(a, "sketch")?;
                let shapes: Vec<J> = opt(a, "add_geometry")?.unwrap_or_default();
                let cons: Vec<Constraint> = opt(a, "add_constraints")?.unwrap_or_default();
                let rm_e: Vec<String> = opt(a, "remove_entities")?.unwrap_or_default();
                let mut rm_c: Vec<usize> = opt(a, "remove_constraints")?.unwrap_or_default();
                let moves: Map<String, J> = opt(a, "move_points")?.unwrap_or_default();
                let trims: Vec<J> = opt(a, "trim")?.unwrap_or_default();
                let fillets: Vec<J> = opt(a, "fillet_corners")?.unwrap_or_default();
                let offsets: Vec<J> = opt(a, "offset")?.unwrap_or_default();
                let m = self.model()?;
                let new_plane = match a.get("plane") {
                    Some(p) => Some(plane_arg(m, p)?),
                    None => None,
                };
                let mut created = J::Null;
                let fid = m.edit(|p| {
                    let f = p.feature_mut(&sid).ok_or_else(|| anyhow!("no feature `{sid}`"))?;
                    let FeatureKind::Sketch(sf) = &mut f.kind else { bail!("`{sid}` is not a sketch") };
                    if let Some(pl) = new_plane {
                        sf.plane = pl;
                    }
                    let sk = &mut sf.sketch;
                    rm_c.sort_unstable();
                    for i in rm_c.into_iter().rev() {
                        if i < sk.constraints.len() {
                            sk.constraints.remove(i);
                        }
                    }
                    sk.remove(&rm_e);
                    for (pid, xy) in &moves {
                        let xy: [f64; 2] = serde_json::from_value(xy.clone())?;
                        match sk.entities.iter_mut().find(|e| e.id() == pid) {
                            Some(Entity::Point { x, y, .. }) => {
                                *x = xy[0];
                                *y = xy[1];
                            }
                            _ => bail!("`{pid}` is not a point"),
                        }
                    }
                    created = add_shapes(sk, &shapes)?;
                    sk.constraints.extend(cons);
                    let mut ops = vec![];
                    for t in &trims {
                        let e: String = req(t, "entity")?;
                        let at: [f64; 2] = req(t, "at")?;
                        let made = sk.trim(&e, at).with_context(|| format!("trim `{e}`"))?;
                        ops.push(json!({"trim": e, "new_entities": made}));
                    }
                    for fc in &fillets {
                        let pnt: String = req(fc, "point")?;
                        let r: f64 = req(fc, "radius")?;
                        let arc = sk.fillet_corner(&pnt, r).with_context(|| format!("fillet at `{pnt}`"))?;
                        ops.push(json!({"fillet": pnt, "arc": arc}));
                    }
                    for o in &offsets {
                        let es: Vec<String> = req(o, "entities")?;
                        let d: f64 = req(o, "distance")?;
                        let made = sk.offset(&es, d).context("offset")?;
                        ops.push(json!({"offset": es, "new_entities": made}));
                    }
                    if !ops.is_empty() {
                        created = json!({"shapes": created, "operations": ops});
                    }
                    Ok(f.id.clone())
                })?;
                let info = sketch_info(m, &fid)?;
                Ok(Reply::Text(summary(m, json!({"sketch": fid, "created_entities": created, "sketch_state": info}))))
            }
            "get_sketch" => {
                let sid: String = req(a, "sketch")?;
                let m = self.model()?;
                let fid = m.part.feature(&sid).ok_or_else(|| anyhow!("no feature `{sid}`"))?.id.clone();
                Ok(Reply::Text(sketch_info(m, &fid)?))
            }
            "extrude" => {
                let e = Extrude {
                    sketch: self.resolve_id(&req::<String>(a, "sketch")?)?,
                    depth: opt(a, "depth")?.unwrap_or(rw_core::sketch::Value::Num(10.0)),
                    end: opt(a, "end")?.unwrap_or_default(),
                    reverse: opt(a, "reverse")?.unwrap_or(false),
                    op: opt(a, "op")?.unwrap_or_default(),
                };
                let base = if e.op == Op::Cut { "Cut-Extrude" } else { "Extrude" };
                self.add_feature(a, base, FeatureKind::Extrude(e))
            }
            "revolve" => {
                let r = Revolve {
                    sketch: self.resolve_id(&req::<String>(a, "sketch")?)?,
                    axis: req(a, "axis")?,
                    angle: opt(a, "angle")?.unwrap_or(rw_core::sketch::Value::Num(360.0)),
                    op: opt(a, "op")?.unwrap_or_default(),
                };
                let base = if r.op == Op::Cut { "Cut-Revolve" } else { "Revolve" };
                self.add_feature(a, base, FeatureKind::Revolve(r))
            }
            "hole" => {
                let plane = {
                    let m = self.model()?;
                    plane_arg(m, a.get("plane").ok_or_else(|| anyhow!("missing `plane`"))?)?
                };
                let h = Hole {
                    plane,
                    points: req(a, "points")?,
                    diameter: req(a, "diameter")?,
                    depth: opt(a, "depth")?,
                    counterbore: opt(a, "counterbore")?,
                    countersink: opt(a, "countersink")?,
                };
                self.add_feature(a, "Hole", FeatureKind::Hole(h))
            }
            "pattern_linear" => {
                let p = LinearPattern {
                    features: self.resolve_ids(&req::<Vec<String>>(a, "features")?)?,
                    direction: req(a, "direction")?,
                    spacing: req(a, "spacing")?,
                    count: req(a, "count")?,
                    direction2: opt(a, "direction2")?,
                    spacing2: opt(a, "spacing2")?,
                    count2: opt(a, "count2")?,
                };
                self.add_feature(a, "LPattern", FeatureKind::LinearPattern(p))
            }
            "pattern_circular" => {
                let p = CircularPattern {
                    features: self.resolve_ids(&req::<Vec<String>>(a, "features")?)?,
                    axis_origin: opt(a, "axis_origin")?.unwrap_or([0.0; 3]),
                    axis_direction: opt(a, "axis_direction")?.unwrap_or([0.0, 1.0, 0.0]),
                    count: req(a, "count")?,
                    angle: opt(a, "angle")?.unwrap_or(rw_core::sketch::Value::Num(360.0)),
                };
                self.add_feature(a, "CirPattern", FeatureKind::CircularPattern(p))
            }
            "mirror" => {
                let plane = {
                    let m = self.model()?;
                    plane_arg(m, a.get("plane").ok_or_else(|| anyhow!("missing `plane`"))?)?
                };
                let features = self.resolve_ids(&opt::<Vec<String>>(a, "features")?.unwrap_or_default())?;
                self.add_feature(a, "Mirror", FeatureKind::Mirror(Mirror { features, plane }))
            }
            "plane" => {
                let base = match a.get("base") {
                    Some(b) => Some(plane_arg(self.model()?, b)?),
                    None => None,
                };
                let rp = RefPlane {
                    base,
                    offset: opt(a, "offset")?.unwrap_or_default(),
                    angle: opt(a, "angle")?,
                    points: opt(a, "points")?,
                    flip: opt(a, "flip")?.unwrap_or(false),
                };
                if rp.base.is_none() && rp.points.is_none() {
                    bail!("give `base` (+ offset/angle) or three `points`");
                }
                self.add_feature(a, "Plane", FeatureKind::Plane(rp))
            }
            "sweep" => {
                let sw = Sweep {
                    profile: self.resolve_id(&req::<String>(a, "profile")?)?,
                    path: self.resolve_id(&req::<String>(a, "path")?)?,
                    op: opt(a, "op")?.unwrap_or_default(),
                };
                let base = if sw.op == Op::Cut { "Cut-Sweep" } else { "Sweep" };
                self.add_feature(a, base, FeatureKind::Sweep(sw))
            }
            "draft" => {
                let mut pts: Vec<[f64; 3]> = opt(a, "face_points")?.unwrap_or_default();
                let faces: Vec<usize> = opt(a, "faces")?.unwrap_or_default();
                let m = self.model()?;
                if !faces.is_empty() {
                    let mesh = m.out.mesh.as_ref().ok_or_else(|| anyhow!("part has no body yet"))?;
                    for fi in faces {
                        pts.push(face_point(mesh, fi).ok_or_else(|| anyhow!("no face {fi}"))?);
                    }
                }
                if pts.is_empty() {
                    bail!("select faces to draft with `faces` or `face_points`");
                }
                let neutral = plane_arg(m, a.get("neutral_plane").ok_or_else(|| anyhow!("missing `neutral_plane`"))?)?;
                let d = Draft { faces: pts, neutral, angle: req(a, "angle")?, reverse: opt(a, "reverse")?.unwrap_or(false) };
                self.add_feature(a, "Draft", FeatureKind::Draft(d))
            }
            "list_edges" => {
                let kind: Option<String> = opt(a, "kind")?;
                let axis: Option<String> = opt(a, "parallel_to")?;
                let near: Option<[f64; 3]> = opt(a, "near")?;
                let radius: f64 = opt(a, "radius")?.unwrap_or(5.0);
                let limit: usize = opt(a, "limit")?.unwrap_or(100);
                let m = self.model()?;
                let body = m.out.body.as_ref().ok_or_else(|| anyhow!("part has no body yet"))?;
                let all = rw_core::kernel::edges(body);
                let total = all.len();
                let list: Vec<_> = all
                    .into_iter()
                    .filter(|e| kind.as_deref().is_none_or(|k| e.kind == k))
                    .filter(|e| {
                        axis.as_deref().is_none_or(|ax| {
                            let d = [e.end[0] - e.start[0], e.end[1] - e.start[1], e.end[2] - e.start[2]];
                            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-12);
                            let k = match ax { "x" => 0, "y" => 1, _ => 2 };
                            e.kind == "line" && (d[k].abs() / l) > 0.9999
                        })
                    })
                    .filter(|e| near.is_none_or(|p| e.distance_to(p) <= radius))
                    .take(limit)
                    .collect();
                Ok(Reply::Text(round_json(json!({"total_edges": total, "shown": list.len(), "edges": list}))))
            }
            "fillet" | "chamfer" => {
                let mut pts: Vec<[f64; 3]> = opt(a, "edges")?.unwrap_or_default();
                let idx: Vec<usize> = opt(a, "edge_indices")?.unwrap_or_default();
                let all: bool = opt(a, "all_edges")?.unwrap_or(false);
                if !idx.is_empty() {
                    let m = self.model()?;
                    let body = m.out.body.as_ref().ok_or_else(|| anyhow!("part has no body yet"))?;
                    let es = rw_core::kernel::edges(body);
                    for i in idx {
                        let e = es.iter().find(|e| e.index == i).ok_or_else(|| anyhow!("no edge {i}"))?;
                        pts.push(e.mid);
                    }
                }
                if pts.is_empty() && !all {
                    bail!("select edges with `edges` (points), `edge_indices`, or all_edges=true");
                }
                if name == "fillet" {
                    let fl = Fillet { edges: pts.into_iter().map(EdgeRef::Point).collect(), all_edges: all, radius: req(a, "radius")? };
                    self.add_feature(a, "Fillet", FeatureKind::Fillet(fl))
                } else {
                    let c = Chamfer { edges: pts.into_iter().map(EdgeRef::Point).collect(), all_edges: all, distance: req(a, "distance")? };
                    self.add_feature(a, "Chamfer", FeatureKind::Chamfer(c))
                }
            }
            "shell" => {
                let mut pts: Vec<[f64; 3]> = opt(a, "face_points")?.unwrap_or_default();
                let faces: Vec<usize> = opt(a, "faces")?.unwrap_or_default();
                if !faces.is_empty() {
                    let m = self.model()?;
                    let mesh = m.out.mesh.as_ref().ok_or_else(|| anyhow!("part has no body yet"))?;
                    for fi in faces {
                        pts.push(face_point(mesh, fi).ok_or_else(|| anyhow!("no face {fi}"))?);
                    }
                }
                let sh = Shell { open_faces: pts, thickness: req(a, "thickness")? };
                self.add_feature(a, "Shell", FeatureKind::Shell(sh))
            }
            "loft" => {
                let l = Loft { sketches: self.resolve_ids(&req::<Vec<String>>(a, "sketches")?)?, op: opt(a, "op")?.unwrap_or_default() };
                self.add_feature(a, "Loft", FeatureKind::Loft(l))
            }
            "import_step" => {
                let path: PathBuf = req(a, "path")?;
                let im = Import { path: abs(&path), op: opt(a, "op")?.unwrap_or_default() };
                self.add_feature(a, "Import", FeatureKind::Import(im))
            }
            "edit_feature" => {
                let fid: String = req(a, "feature")?;
                let changes: J = req(a, "changes")?;
                let m = self.model()?;
                let id = m.edit(|p| {
                    let f = p.feature_mut(&fid).ok_or_else(|| anyhow!("no feature `{fid}`"))?;
                    let mut v = serde_json::to_value(&*f)?;
                    merge_patch(&mut v, &changes);
                    v["id"] = J::String(f.id.clone());
                    *f = serde_json::from_value(v).context("patched feature is invalid")?;
                    Ok(f.id.clone())
                })?;
                let def = m.part.feature(&id).cloned();
                Ok(Reply::Text(summary(m, json!({"feature": def}))))
            }
            "delete_feature" => {
                let fid: String = req(a, "feature")?;
                let cascade: bool = opt(a, "cascade")?.unwrap_or(false);
                let m = self.model()?;
                let removed = m.edit(|p| {
                    let id = p.feature(&fid).ok_or_else(|| anyhow!("no feature `{fid}`"))?.id.clone();
                    let mut doomed = vec![id.clone()];
                    let mut i = 0;
                    while i < doomed.len() {
                        for d in p.dependents(&doomed[i]) {
                            if !doomed.contains(&d) {
                                doomed.push(d);
                            }
                        }
                        i += 1;
                    }
                    if doomed.len() > 1 && !cascade {
                        bail!("features {:?} depend on `{id}`; pass cascade=true to delete them too", &doomed[1..]);
                    }
                    p.features.retain(|f| !doomed.contains(&f.id));
                    Ok(doomed)
                })?;
                Ok(Reply::Text(summary(m, json!({"deleted": removed}))))
            }
            "suppress_feature" => {
                let fid: String = req(a, "feature")?;
                let s: bool = req(a, "suppressed")?;
                let m = self.model()?;
                m.edit(|p| {
                    p.feature_mut(&fid).ok_or_else(|| anyhow!("no feature `{fid}`"))?.suppressed = s;
                    Ok(())
                })?;
                Ok(Reply::Text(summary(m, json!({"feature": fid, "suppressed": s}))))
            }
            "undo" => {
                let m = self.model()?;
                let ok = m.undo();
                Ok(Reply::Text(summary(m, json!({"undone": ok}))))
            }
            "redo" => {
                let m = self.model()?;
                let ok = m.redo();
                Ok(Reply::Text(summary(m, json!({"redone": ok}))))
            }
            "list_faces" => {
                let planar: bool = opt(a, "planar_only")?.unwrap_or(false);
                let m = self.model()?;
                let faces: Vec<_> = m
                    .out
                    .mesh
                    .as_ref()
                    .map(|me| me.faces.iter().filter(|f| !planar || f.kind == "plane").cloned().collect())
                    .unwrap_or_default();
                Ok(Reply::Text(json!({"faces": round_json(serde_json::to_value(faces)?)})))
            }
            "render" => {
                let view: rw_render::View = opt(a, "view")?.unwrap_or_default();
                let w: u32 = opt(a, "width")?.unwrap_or(800).clamp(64, 2048);
                let h: u32 = opt(a, "height")?.unwrap_or(600).clamp(64, 2048);
                let edges: bool = opt(a, "edges")?.unwrap_or(true);
                let labels: bool = opt(a, "face_labels")?.unwrap_or(false);
                let edge_labels: bool = opt(a, "edge_labels")?.unwrap_or(false);
                let show_sk: bool = opt(a, "sketches")?.unwrap_or(true);
                let m = self.model()?;
                // Show sketches that are not yet consumed by a feature (like SOLIDWORKS hides used sketches).
                let consumed: Vec<String> = m.part.features.iter().flat_map(|f| f.kind.references()).collect();
                let mut lines: Vec<_> = if show_sk {
                    m.out.sketches.iter().filter(|s| !consumed.contains(&s.feature)).flat_map(|s| s.polylines()).collect()
                } else {
                    vec![]
                };
                let half = m.out.mass.map(|mp| {
                    let d = [0, 1, 2].map(|k| mp.bbox_max[k] - mp.bbox_min[k]);
                    (d[0].max(d[1]).max(d[2]) * 0.6).max(10.0)
                }).unwrap_or(40.0);
                for (_, pl) in &m.out.ref_planes {
                    lines.push((plane_outline(pl, half), true));
                }
                let png = rw_render::render_png(
                    m.out.mesh.as_ref(),
                    &rw_render::RenderOptions { width: w, height: h, view, edges, face_labels: labels, edge_labels, sketches: &lines, tri_colors: &[] },
                );
                Ok(Reply::TextAndImage(summary(m, json!({"view": format!("{view:?}")})), png))
            }
            "mass_properties" => {
                let density: f64 = opt(a, "density")?.unwrap_or(7.85);
                let m = self.model()?;
                let mp = m.out.mass.ok_or_else(|| anyhow!("part has no body yet"))?;
                Ok(Reply::Text(round_json(json!({
                    "volume_mm3": mp.volume,
                    "surface_area_mm2": mp.surface_area,
                    "mass_g": mp.volume / 1000.0 * density,
                    "density_g_cm3": density,
                    "center_of_mass": mp.centroid,
                    "bbox_min": mp.bbox_min,
                    "bbox_max": mp.bbox_max,
                    "size": [mp.bbox_max[0]-mp.bbox_min[0], mp.bbox_max[1]-mp.bbox_min[1], mp.bbox_max[2]-mp.bbox_min[2]],
                }))))
            }
            "export" => {
                let fmt: String = req(a, "format")?;
                let path: PathBuf = req(a, "path")?;
                let m = self.model()?;
                export(m, &fmt, &path)?;
                Ok(Reply::Text(json!({"exported": abs(&path), "format": fmt})))
            }
            _ => bail!("unknown tool `{name}`"),
        }
    }

    fn resolve_id(&mut self, id: &str) -> Result<String> {
        let m = self.model()?;
        Ok(m.part.feature(id).ok_or_else(|| anyhow!("no feature `{id}`"))?.id.clone())
    }

    fn resolve_ids(&mut self, ids: &[String]) -> Result<Vec<String>> {
        ids.iter().map(|i| self.resolve_id(i)).collect()
    }

    fn add_feature(&mut self, a: &J, base: &str, kind: FeatureKind) -> Result<Reply> {
        let name: Option<String> = opt(a, "name")?;
        let m = self.model()?;
        let id = m.edit(|p| {
            let (id, auto) = p.fresh_ids(base);
            p.features.push(Feature { id: id.clone(), name: name.unwrap_or(auto), suppressed: false, kind });
            Ok(id)
        })?;
        let st = m.out.status.iter().find(|s| s.id == id).cloned();
        Ok(Reply::Text(summary(m, json!({"feature": id, "feature_status": st}))))
    }
}

const INSTRUCTIONS: &str = "RustWorks is a parametric 3D CAD system (like SOLIDWORKS) on the OpenCASCADE kernel. Part workflow: new_part → create_sketch on a plane \
(front/top/right, offset, a reference plane from the `plane` tool, or {face:N}) with geometry (lines, arcs, circles, splines, ...) + constraints; refine sketches with \
edit_sketch (trim, fillet_corners, offset) → extrude/revolve/loft/sweep (boss or cut) → hole/patterns/mirror → fillet/chamfer (pick edges with list_edges \
or render edge_labels) → draft/shell → render to look at it → edit_feature / set_parameters to iterate → export (step/stl). \
Drawings: create_drawing from the active part or assembly (standard views, auto scale, overall dimensions, hole callouts, title block) → \
look at the returned image → edit_drawing (views, scale, sheet, title) → export_drawing (pdf/svg/dxf/png). \
Assembly workflow: build each part file, then new_assembly → add_component (first one is grounded) → add_mate between faces (list_faces with component, or \
render) → check_interference → render/export. Units are mm, world is Y-up. Define key dimensions as parameters and reference them by name in expressions \
so the design stays editable. After significant changes call render (face_labels:true when you need a face index). Every response includes rebuild \
errors/warnings; fix errors before continuing.";

fn req<T: DeserializeOwned>(a: &J, k: &str) -> Result<T> {
    let v = a.get(k).ok_or_else(|| anyhow!("missing argument `{k}`"))?;
    serde_json::from_value(v.clone()).with_context(|| format!("invalid argument `{k}`"))
}

fn opt<T: DeserializeOwned>(a: &J, k: &str) -> Result<Option<T>> {
    match a.get(k) {
        None | Some(J::Null) => Ok(None),
        Some(v) => Ok(Some(serde_json::from_value(v.clone()).with_context(|| format!("invalid argument `{k}`"))?)),
    }
}

fn abs(p: &std::path::Path) -> String {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).display().to_string()
}

fn pretty(v: &J) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Rounds floats to 4 decimals to keep responses compact for the model.
fn round_json(v: J) -> J {
    match v {
        J::Number(n) if n.is_f64() => {
            let f = (n.as_f64().unwrap() * 1e4).round() / 1e4;
            json!(f)
        }
        J::Array(a) => J::Array(a.into_iter().map(round_json).collect()),
        J::Object(o) => J::Object(o.into_iter().map(|(k, v)| (k, round_json(v))).collect()),
        x => x,
    }
}

/// Standard response tail: rebuild time, errors, warnings, body stats.
fn summary(m: &Model, mut v: J) -> J {
    let errors = m.out.errors();
    let warnings: Vec<String> =
        m.out.status.iter().filter_map(|s| s.warning.as_ref().map(|w| format!("{} ({}): {w}", s.name, s.id))).collect();
    let o = v.as_object_mut().unwrap();
    o.insert("rebuild_ms".into(), json!((m.out.millis * 10.0).round() / 10.0));
    if !errors.is_empty() {
        o.insert("rebuild_errors".into(), json!(errors));
    }
    if !warnings.is_empty() {
        o.insert("warnings".into(), json!(warnings));
    }
    o.insert(
        "features".into(),
        json!(m.out.status.iter().map(|s| format!("{} {} [{}]{}", s.id, s.name, s.kind, if s.suppressed { " (suppressed)" } else if !s.ok { " ERROR" } else { "" })).collect::<Vec<_>>()),
    );
    if let Some(mp) = m.out.mass {
        o.insert(
            "body".into(),
            round_json(json!({"volume_mm3": mp.volume, "bbox_min": mp.bbox_min, "bbox_max": mp.bbox_max,
                "faces": m.out.mesh.as_ref().map(|x| x.faces.len())})),
        );
    }
    v
}

fn sketch_info(m: &Model, fid: &str) -> Result<J> {
    let solved = m.out.sketches.iter().find(|s| s.feature == fid);
    let f = m.part.feature(fid).ok_or_else(|| anyhow!("no feature `{fid}`"))?;
    let FeatureKind::Sketch(sf) = &f.kind else { bail!("`{fid}` is not a sketch") };
    let constraints: Vec<J> =
        sf.sketch.constraints.iter().enumerate().map(|(i, c)| json!({"index": i, "constraint": c})).collect();
    Ok(round_json(json!({
        "id": f.id,
        "name": f.name,
        "entities": sf.sketch.entities,
        "constraints": constraints,
        "solve": solved.and_then(|s| s.report.clone()),
        "profiles": solved.map(|s| s.regions.iter().map(|r| json!({"area": r.area(), "holes": r.holes.len()})).collect::<Vec<_>>()),
        "profile_error": solved.and_then(|s| s.profile_error.clone()),
        "plane": solved.map(|s| json!({"origin": s.plane.origin, "x_axis": s.plane.x, "y_axis": s.plane.y, "normal": s.plane.normal()})),
    })))
}

/// Square outline of a reference plane, centered on its origin.
pub fn plane_outline(pl: &rw_core::kernel::Plane, half: f64) -> Vec<[f32; 3]> {
    [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0], [-1.0, -1.0]]
        .iter()
        .map(|c: &[f64; 2]| pl.to3d([c[0] * half, c[1] * half]).map(|v| v as f32))
        .collect()
}

fn plane_arg(m: &Model, v: &J) -> Result<PlaneRef> {
    if let Some(s) = v.as_str() {
        if !matches!(s, "front" | "top" | "right") {
            let f = m.part.feature(s).ok_or_else(|| anyhow!("no plane `{s}` (use front/top/right or a reference plane id/name)"))?;
            if !matches!(f.kind, FeatureKind::Plane(_)) {
                bail!("`{s}` is not a reference plane");
            }
            return Ok(PlaneRef::Feature(f.id.clone()));
        }
    }
    if let Some(n) = v.get("face") {
        let idx = n.as_u64().ok_or_else(|| anyhow!("face must be an integer"))? as usize;
        let mesh = m.out.mesh.as_ref().ok_or_else(|| anyhow!("no body yet; cannot sketch on a face"))?;
        let f = mesh.faces.iter().find(|f| f.index == idx).ok_or_else(|| anyhow!("no face {idx}"))?;
        return plane_from_face(f).ok_or_else(|| anyhow!("face {idx} is curved; pick a planar face"));
    }
    Ok(serde_json::from_value(v.clone()).context("invalid plane")?)
}

fn pt(v: &J, k: &str) -> Result<[f64; 2]> {
    serde_json::from_value(v.get(k).cloned().ok_or_else(|| anyhow!("shape missing `{k}`"))?)
        .with_context(|| format!("shape field `{k}` must be [x, y]"))
}

fn f(v: &J, k: &str) -> Result<f64> {
    v.get(k).and_then(|x| x.as_f64()).ok_or_else(|| anyhow!("shape missing number `{k}`"))
}

/// Adds shapes to a sketch; returns the ids created per shape.
fn add_shapes(sk: &mut Sketch, shapes: &[J]) -> Result<J> {
    let mut out = vec![];
    for (i, s) in shapes.iter().enumerate() {
        let ty = s.get("type").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("shape {i} missing `type`"))?;
        let cons = s.get("construction").and_then(|c| c.as_bool()).unwrap_or(false);
        let before: Vec<String> = sk.entities.iter().map(|e| e.id().to_string()).collect();
        match ty {
            "line" => {
                sk.add_line(pt(s, "from")?, pt(s, "to")?, cons);
            }
            "polyline" => {
                let pts: Vec<[f64; 2]> = serde_json::from_value(s.get("points").cloned().unwrap_or(J::Null))?;
                let closed = s.get("closed").and_then(|c| c.as_bool()).unwrap_or(true);
                if pts.len() < 2 {
                    bail!("polyline needs at least 2 points");
                }
                let n = if closed { pts.len() } else { pts.len() - 1 };
                for k in 0..n {
                    sk.add_line(pts[k], pts[(k + 1) % pts.len()], cons);
                }
            }
            "rectangle" => {
                sk.add_rectangle(pt(s, "corner1")?, pt(s, "corner2")?);
            }
            "center_rectangle" => {
                let c = pt(s, "center")?;
                let (w, h) = (f(s, "width")?, f(s, "height")?);
                sk.add_rectangle([c[0] - w / 2.0, c[1] - h / 2.0], [c[0] + w / 2.0, c[1] + h / 2.0]);
            }
            "circle" => {
                sk.add_circle(pt(s, "center")?, f(s, "radius")?, cons);
            }
            "arc" => {
                sk.add_arc(pt(s, "center")?, pt(s, "start")?, pt(s, "end")?, cons);
            }
            "polygon" => {
                let sides = s.get("sides").and_then(|x| x.as_u64()).unwrap_or(6) as usize;
                let rot = s.get("rotation").and_then(|x| x.as_f64()).unwrap_or(0.0);
                sk.add_polygon(pt(s, "center")?, f(s, "radius")?, sides, rot);
            }
            "slot" => {
                sk.add_slot(pt(s, "from")?, pt(s, "to")?, f(s, "radius")?);
            }
            "spline" => {
                let mut pts: Vec<[f64; 2]> = serde_json::from_value(s.get("points").cloned().unwrap_or(J::Null))?;
                if pts.len() < 2 {
                    bail!("spline needs at least 2 points");
                }
                if s.get("closed").and_then(|c| c.as_bool()).unwrap_or(false) && pts.first() != pts.last() {
                    pts.push(pts[0]);
                }
                sk.add_spline(&pts, cons);
            }
            "point" => {
                let p = pt(s, "at")?;
                sk.point_at(p[0], p[1], 1e-9);
            }
            other => bail!("unknown shape type `{other}`"),
        }
        let new: Vec<String> = sk.entities.iter().map(|e| e.id().to_string()).filter(|id| !before.contains(id)).collect();
        out.push(json!({"shape": i, "type": ty, "ids": new}));
    }
    Ok(J::Array(out))
}

pub fn merge_patch(target: &mut J, patch: &J) {
    match patch {
        J::Object(p) => {
            if !target.is_object() {
                *target = json!({});
            }
            let t = target.as_object_mut().unwrap();
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(J::Null), v);
                }
            }
        }
        _ => *target = patch.clone(),
    }
}

pub fn export(m: &Model, fmt: &str, path: &std::path::Path) -> Result<()> {
    let body = m.out.body.as_ref().ok_or_else(|| anyhow!("part has no body to export"))?;
    match fmt {
        "step" | "stp" => rw_core::kernel::write_step(body, path)?,
        "stl" | "obj" => {
            // Export at a finer tolerance than the display mesh.
            let tol = (rw_core::kernel::scale_of(body) / 4000.0).clamp(0.001, 0.2);
            let mesh = rw_core::kernel::tessellate(body, tol, false);
            let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
            if fmt == "stl" { mesh.write_stl(&mut w)? } else { mesh.write_obj(&mut w)? }
        }
        _ => bail!("unsupported format `{fmt}` (use step, stl or obj)"),
    }
    Ok(())
}

/// The MCP protocol owns stdout, but the C++ kernel (OpenCASCADE) prints
/// progress banners to stdout too. Keep a private handle to the real stdout
/// for JSON-RPC and point fd 1 at stderr so library chatter can't corrupt it.
#[cfg(unix)]
fn protocol_stdout() -> std::io::BufWriter<std::fs::File> {
    use std::os::fd::FromRawFd;
    unsafe {
        let fd = libc::dup(1);
        if fd >= 0 && libc::dup2(2, 1) >= 0 {
            return std::io::BufWriter::new(std::fs::File::from_raw_fd(fd));
        }
        std::io::BufWriter::new(std::fs::File::from_raw_fd(1))
    }
}

#[cfg(windows)]
fn protocol_stdout() -> std::io::BufWriter<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    unsafe {
        let fd = libc::dup(1);
        if fd >= 0 && libc::dup2(2, 1) >= 0 {
            let h = libc::get_osfhandle(fd);
            if h != -1 {
                return std::io::BufWriter::new(std::fs::File::from_raw_handle(h as _));
            }
        }
        let h = libc::get_osfhandle(1);
        std::io::BufWriter::new(std::fs::File::from_raw_handle(h as _))
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// A point that lies on face `fi` (centroid of its largest triangle).
fn face_point(mesh: &rw_core::kernel::Mesh, fi: usize) -> Option<[f64; 3]> {
    let mut best: Option<(f64, [f64; 3])> = None;
    for (t, tri) in mesh.indices.chunks(3).enumerate() {
        if mesh.tri_face[t] as usize != fi {
            continue;
        }
        let p = [tri[0], tri[1], tri[2]].map(|i| mesh.positions[i as usize].map(|v| v as f64));
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        let c = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let area = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
        if best.is_none_or(|b| area > b.0) {
            let cen = [0, 1, 2].map(|k| ((p[0][k] + p[1][k] + p[2][k]) / 3.0 * 1e4).round() / 1e4);
            best = Some((area, cen));
        }
    }
    best.map(|b| b.1)
}
