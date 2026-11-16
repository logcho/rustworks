//! Assembly tools: components placed by mates, plus assembly-aware versions of
//! render / mass_properties / export / undo / redo / list_faces.

use crate::{abs, face_point, merge_patch, opt, req, round_json, Reply, Server};
use anyhow::{anyhow, bail, Context, Result};
use rw_core::assembly::{relative_part_path, AsmModel, AxisName, ComponentOut, MateRef};
use rw_core::{Assembly, Component, Mate, MateKind, StdPlane};
use serde_json::{json, Value as J};
use std::path::PathBuf;

/// Distinct, readable component colors for renders.
pub const PALETTE: [[f32; 3]; 8] = [
    [0.70, 0.75, 0.82],
    [0.86, 0.66, 0.42],
    [0.55, 0.78, 0.60],
    [0.80, 0.58, 0.62],
    [0.62, 0.66, 0.88],
    [0.85, 0.80, 0.50],
    [0.56, 0.80, 0.82],
    [0.76, 0.64, 0.84],
];

impl Server {
    fn asm(&mut self) -> Result<&mut AsmModel> {
        let m = self.asm.as_mut().ok_or_else(|| anyhow!("no assembly open: call new_assembly or open_assembly first"))?;
        m.sync_from_disk()?;
        Ok(m)
    }

    /// Handles assembly tools; returns None for tools it does not own.
    pub(crate) fn asm_call(&mut self, name: &str, a: &J) -> Option<Result<Reply>> {
        let focus_tools = ["undo", "redo", "render", "mass_properties", "export"];
        let owned = matches!(
            name,
            "new_assembly"
                | "open_assembly"
                | "get_assembly"
                | "add_component"
                | "edit_component"
                | "remove_component"
                | "add_mate"
                | "edit_mate"
                | "delete_mate"
                | "check_interference"
        ) || (focus_tools.contains(&name) && self.target_asm(a))
            || (name == "list_faces" && a.get("component").is_some_and(|c| !c.is_null()));
        if !owned {
            return None;
        }
        Some(self.asm_tool(name, a))
    }

    fn target_asm(&self, a: &J) -> bool {
        match a.get("target").and_then(|t| t.as_str()) {
            Some("assembly") => true,
            Some("part") => false,
            _ => self.focus_asm && self.asm.is_some(),
        }
    }

    fn asm_tool(&mut self, name: &str, a: &J) -> Result<Reply> {
        match name {
            "new_assembly" => {
                let nm: String = req(a, "name")?;
                let path: PathBuf = opt(a, "path")?.unwrap_or_else(|| PathBuf::from(format!("{nm}.rwasm")));
                let mut m = AsmModel::new(Assembly::new(&nm), Some(path.clone()));
                m.save()?;
                self.asm = Some(m);
                self.focus_asm = true;
                Ok(Reply::Text(json!({"created": nm, "path": abs(&path)})))
            }
            "open_assembly" => {
                let path: PathBuf = req(a, "path")?;
                self.asm = Some(AsmModel::open(&path)?);
                self.focus_asm = true;
                let m = self.asm.as_ref().unwrap();
                Ok(Reply::Text(asm_summary(m, json!({"opened": abs(&path)}))))
            }
            "get_assembly" => {
                let m = self.asm()?;
                Ok(Reply::Text(asm_summary(m, json!({"path": m.path.as_ref().map(|p| abs(p)), "document": m.asm}))))
            }
            "add_component" => {
                let part: PathBuf = req(a, "part")?;
                if !part.exists() {
                    bail!("part file {} not found", part.display());
                }
                let position: [f64; 3] = opt(a, "position")?.unwrap_or([0.0; 3]);
                let rotation: [f64; 3] = opt(a, "rotation")?.unwrap_or([0.0; 3]);
                let fixed: bool = opt(a, "fixed")?.unwrap_or(false);
                let name: Option<String> = opt(a, "name")?;
                self.focus_asm = true;
                let m = self.asm()?;
                let rel = relative_part_path(&m.base(), &part);
                let stem = part.file_stem().and_then(|s| s.to_str()).unwrap_or("part").to_string();
                let id = m.edit(|asm| {
                    let (id, auto) = asm.fresh_component(&stem);
                    asm.components.push(Component { id: id.clone(), name: name.unwrap_or(auto), path: rel, position, rotation, fixed, suppressed: false });
                    Ok(id)
                })?;
                let faces = m.out.components.iter().find(|c| c.id == id).and_then(|c| c.mesh.as_ref()).map(|me| me.faces.len());
                Ok(Reply::Text(asm_summary(m, json!({"component": id, "faces": faces}))))
            }
            "edit_component" => {
                let cid: String = req(a, "component")?;
                let pos: Option<[f64; 3]> = opt(a, "position")?;
                let rot: Option<[f64; 3]> = opt(a, "rotation")?;
                let fixed: Option<bool> = opt(a, "fixed")?;
                let sup: Option<bool> = opt(a, "suppressed")?;
                let nm: Option<String> = opt(a, "name")?;
                let part: Option<PathBuf> = opt(a, "part")?;
                let m = self.asm()?;
                let base = m.base();
                m.edit(|asm| {
                    let c = asm.component_mut(&cid).ok_or_else(|| anyhow!("no component `{cid}`"))?;
                    if let Some(p) = pos {
                        c.position = p;
                    }
                    if let Some(r) = rot {
                        c.rotation = r;
                    }
                    if let Some(f) = fixed {
                        c.fixed = f;
                    }
                    if let Some(s) = sup {
                        c.suppressed = s;
                    }
                    if let Some(n) = nm {
                        c.name = n;
                    }
                    if let Some(p) = &part {
                        if !p.exists() {
                            bail!("part file {} not found", p.display());
                        }
                        c.path = relative_part_path(&base, p);
                    }
                    Ok(())
                })?;
                Ok(Reply::Text(asm_summary(m, json!({"component": cid}))))
            }
            "remove_component" => {
                let cid: String = req(a, "component")?;
                let m = self.asm()?;
                let removed = m.edit(|asm| {
                    let c = asm.component(&cid).ok_or_else(|| anyhow!("no component `{cid}`"))?;
                    let (id, name) = (c.id.clone(), c.name.clone());
                    let hit = |r: &MateRef| r.component == id || r.component == name;
                    let mates: Vec<String> = asm.mates.iter().filter(|mt| hit(&mt.a) || hit(&mt.b)).map(|mt| mt.id.clone()).collect();
                    asm.mates.retain(|mt| !mates.contains(&mt.id));
                    asm.components.retain(|c| c.id != id);
                    Ok(json!({"component": id, "mates_removed": mates}))
                })?;
                Ok(Reply::Text(asm_summary(m, json!({"removed": removed}))))
            }
            "add_mate" => {
                let kind: MateKind = req(a, "type")?;
                let m = self.asm()?;
                let ra = mate_ref(m, a.get("a").ok_or_else(|| anyhow!("missing `a`"))?)?;
                let rb = mate_ref(m, a.get("b").ok_or_else(|| anyhow!("missing `b`"))?)?;
                let mate = Mate {
                    id: String::new(),
                    name: opt(a, "name")?.unwrap_or_default(),
                    kind,
                    a: ra,
                    b: rb,
                    value: opt(a, "value")?,
                    aligned: opt(a, "aligned")?.unwrap_or(false),
                    suppressed: false,
                };
                let id = m.add_mate(mate)?;
                Ok(Reply::Text(asm_summary(m, json!({"mate": id}))))
            }
            "edit_mate" => {
                let mid: String = req(a, "mate")?;
                let changes: J = req(a, "changes")?;
                let m = self.asm()?;
                m.edit(|asm| {
                    let mt = asm.mates.iter_mut().find(|x| x.id == mid || x.name == mid).ok_or_else(|| anyhow!("no mate `{mid}`"))?;
                    let mut v = serde_json::to_value(&*mt)?;
                    merge_patch(&mut v, &changes);
                    v["id"] = J::String(mt.id.clone());
                    *mt = serde_json::from_value(v).context("patched mate is invalid")?;
                    Ok(())
                })?;
                Ok(Reply::Text(asm_summary(m, json!({"mate": mid}))))
            }
            "delete_mate" => {
                let mid: String = req(a, "mate")?;
                let m = self.asm()?;
                m.edit(|asm| {
                    let n = asm.mates.len();
                    asm.mates.retain(|x| x.id != mid && x.name != mid);
                    if asm.mates.len() == n {
                        bail!("no mate `{mid}`");
                    }
                    Ok(())
                })?;
                Ok(Reply::Text(asm_summary(m, json!({"deleted": mid}))))
            }
            "check_interference" => {
                let m = self.asm()?;
                let hits = m.out.interferences()?;
                Ok(Reply::Text(round_json(json!({
                    "interferences": hits.iter().map(|(x, y, v)| json!({"a": x, "b": y, "volume_mm3": v})).collect::<Vec<_>>(),
                    "clear": hits.is_empty(),
                }))))
            }
            "list_faces" => {
                let cid: String = req(a, "component")?;
                let planar: bool = opt(a, "planar_only")?.unwrap_or(false);
                let m = self.asm()?;
                let c = find(m, &cid)?;
                let mesh = c.mesh.as_ref().ok_or_else(|| anyhow!("component `{cid}` has no geometry"))?;
                let body = c.body.as_ref().unwrap();
                let mut out = vec![];
                for f in mesh.faces.iter().filter(|f| !planar || f.kind == "plane") {
                    let geom = rw_core::kernel::face_geom(body, f.index).ok();
                    out.push(json!({
                        "index": f.index,
                        "kind": f.kind,
                        "area": f.area,
                        "point": face_point(mesh, f.index),
                        "normal": f.normal,
                        "world_point": face_point(mesh, f.index).map(|p| c.placement.point(p)),
                        "world_normal": c.placement.dir(f.normal),
                        "surface": geom,
                    }));
                }
                Ok(Reply::Text(round_json(json!({"component": c.id, "faces_in_part_coordinates": out}))))
            }
            "undo" => {
                let m = self.asm()?;
                let ok = m.undo();
                Ok(Reply::Text(asm_summary(m, json!({"undone": ok}))))
            }
            "redo" => {
                let m = self.asm()?;
                let ok = m.redo();
                Ok(Reply::Text(asm_summary(m, json!({"redone": ok}))))
            }
            "render" => {
                let view: rw_render::View = opt(a, "view")?.unwrap_or_default();
                let w: u32 = opt(a, "width")?.unwrap_or(800).clamp(64, 2048);
                let h: u32 = opt(a, "height")?.unwrap_or(600).clamp(64, 2048);
                let edges: bool = opt(a, "edges")?.unwrap_or(true);
                let m = self.asm()?;
                let (mesh, _) = m.out.merged_mesh();
                let mut colors = vec![];
                let mut k = 0;
                let mut legend = vec![];
                for c in m.out.components.iter().filter(|c| !c.suppressed) {
                    if let Some(me) = &c.mesh {
                        let col = PALETTE[k % PALETTE.len()];
                        colors.extend(std::iter::repeat_n(col, me.tri_face.len()));
                        legend.push(json!({"component": c.id, "name": c.name, "color_rgb": col.map(|v| (v * 255.0).round() as u8)}));
                        k += 1;
                    }
                }
                let png = rw_render::render_png(
                    Some(&mesh),
                    &rw_render::RenderOptions { width: w, height: h, view, edges, face_labels: false, edge_labels: false, sketches: &[], tri_colors: &colors },
                );
                Ok(Reply::TextAndImage(asm_summary(m, json!({"view": format!("{view:?}"), "legend": legend})), png))
            }
            "mass_properties" => {
                let density: f64 = opt(a, "density")?.unwrap_or(7.85);
                let m = self.asm()?;
                let (v, c) = m.out.mass();
                let per: Vec<J> = m.out.components.iter().filter(|c| !c.suppressed).map(|c| json!({"component": c.id, "volume_mm3": c.volume})).collect();
                Ok(Reply::Text(round_json(json!({
                    "volume_mm3": v, "mass_g": v / 1000.0 * density, "density_g_cm3": density, "center_of_mass": c, "components": per
                }))))
            }
            "export" => {
                let fmt: String = req(a, "format")?;
                let path: PathBuf = req(a, "path")?;
                let m = self.asm()?;
                match fmt.as_str() {
                    "step" | "stp" => m.export_step(&path)?,
                    "stl" | "obj" => {
                        let (mesh, _) = m.out.merged_mesh();
                        let mut w = std::io::BufWriter::new(std::fs::File::create(&path)?);
                        if fmt == "stl" { mesh.write_stl(&mut w)? } else { mesh.write_obj(&mut w)? }
                    }
                    _ => bail!("unsupported format `{fmt}` (use step, stl or obj)"),
                }
                Ok(Reply::Text(json!({"exported": abs(&path), "format": fmt, "target": "assembly"})))
            }
            _ => unreachable!(),
        }
    }
}

fn find<'a>(m: &'a AsmModel, cid: &str) -> Result<&'a ComponentOut> {
    m.out.components.iter().find(|c| c.id == cid || c.name == cid).ok_or_else(|| anyhow!("no component `{cid}`"))
}

/// Parses a mate reference: {component, face: N | face_point: [x,y,z] | plane | axis | point}.
fn mate_ref(m: &AsmModel, v: &J) -> Result<MateRef> {
    let cid: String = req(v, "component")?;
    let c = find(m, &cid)?;
    let mut r = MateRef { component: c.id.clone(), face: None, plane: None, axis: None, point: None };
    if let Some(f) = v.get("face").filter(|f| !f.is_null()) {
        if let Some(i) = f.as_u64() {
            let mesh = c.mesh.as_ref().ok_or_else(|| anyhow!("component `{cid}` has no geometry"))?;
            r.face = Some(face_point(mesh, i as usize).ok_or_else(|| anyhow!("component `{cid}` has no face {i}"))?);
        } else {
            r.face = Some(serde_json::from_value(f.clone()).context("`face` must be a face index or [x,y,z]")?);
        }
    }
    if let Some(p) = v.get("face_point").filter(|f| !f.is_null()) {
        r.face = Some(serde_json::from_value(p.clone()).context("`face_point` must be [x,y,z]")?);
    }
    r.plane = opt::<StdPlane>(v, "plane")?;
    r.axis = opt::<AxisName>(v, "axis")?;
    r.point = opt(v, "point")?;
    let n = [r.face.is_some(), r.plane.is_some(), r.axis.is_some(), r.point.is_some()].iter().filter(|b| **b).count();
    if n != 1 {
        bail!("mate reference on `{cid}` needs exactly one of face, face_point, plane, axis, point");
    }
    Ok(r)
}

fn asm_summary(m: &AsmModel, mut v: J) -> J {
    let o = v.as_object_mut().unwrap();
    o.insert("rebuild_ms".into(), json!((m.out.millis * 10.0).round() / 10.0));
    let errors = m.out.errors();
    if !errors.is_empty() {
        o.insert("rebuild_errors".into(), json!(errors));
    }
    o.insert("solve".into(), round_json(serde_json::to_value(&m.out.solve).unwrap_or_default()));
    o.insert(
        "components".into(),
        round_json(json!(m
            .out
            .components
            .iter()
            .zip(&m.asm.components)
            .map(|(c, d)| json!({
                "id": c.id, "name": c.name, "part": d.path,
                "position": d.position, "rotation": d.rotation,
                "grounded": c.grounded, "suppressed": c.suppressed,
            }))
            .collect::<Vec<_>>())),
    );
    o.insert(
        "mates".into(),
        json!(m
            .out
            .mates
            .iter()
            .map(|s| format!("{} {} [{:?}]{}", s.id, s.name, s.kind, if s.ok { "" } else { " ERROR" }))
            .collect::<Vec<_>>()),
    );
    v
}
