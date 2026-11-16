use crate::asm_ui::AsmUi;
use crate::viewport::{LineVertex, Viewport};
use crate::worker::{Rebuilt, Request, Response, Worker};
use egui::{Color32, RichText};
use glam::Vec3;
use rw_core::kernel::{MassProps, Mesh, Plane};
use rw_core::model::{read_part, write_part};
use rw_core::sketch::{Constraint, Entity, Scope, SolveStatus, Value};
use rw_core::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

const ACCENT: Color32 = Color32::from_rgb(0, 102, 204);

pub struct App {
    part: Part,
    path: Option<PathBuf>,
    disk_stamp: Option<SystemTime>,
    undo: Vec<Part>,
    redo: Vec<Part>,
    generation: u64,
    worker: Worker,
    vp: Option<Viewport>,
    status: Vec<FeatureStatus>,
    sketches: Vec<SolvedSketch>,
    mass: Option<MassProps>,
    millis: f64,
    cache_hits: usize,
    selected: Option<String>,
    picked_point: Option<Vec3>,
    toast: Option<(String, Instant, bool)>,
    shape: ShapeForm,
    new_param: (String, String),
    dim_form: DimForm,
    last_poll: Instant,
    last_frame: Instant,
    overlay_dirty: bool,
    first_mesh: bool,
    /// Active interactive sketch session.
    sketcher: Option<crate::sketcher::Sketcher>,
    /// Selected model edges: (kernel edge index, point on edge).
    sel_edges: Vec<(u32, [f64; 3])>,
    hover_edge: Option<u32>,
    /// Extra features selected with Cmd/Ctrl-click (e.g. loft sections).
    multi: Vec<String>,
    /// Reference planes from the last rebuild (feature id, plane).
    ref_planes: Vec<(String, Plane)>,
    /// Last part mesh (restored when leaving assembly mode).
    part_mesh: Option<Arc<Mesh>>,
    /// Open assembly, and whether it is the active window.
    asm: Option<AsmUi>,
    asm_active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ShapeKind {
    Rectangle,
    Circle,
    Line,
    Polygon,
    Slot,
    Arc,
}

struct ShapeForm {
    kind: ShapeKind,
    a: [f64; 2],
    b: [f64; 2],
    r: f64,
    sides: usize,
    construction: bool,
}

struct DimForm {
    kind: usize,
    a: String,
    b: String,
    value: String,
}

const DIM_KINDS: [&str; 12] = [
    "distance", "horizontal_distance", "vertical_distance", "radius", "diameter", "angle", "horizontal", "vertical",
    "parallel", "perpendicular", "equal", "tangent",
];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> App {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let ctx = cc.egui_ctx.clone();
        let worker = Worker::spawn(move || ctx.request_repaint());
        let vp = cc.wgpu_render_state.as_ref().map(Viewport::new);
        let remembered: Option<PathBuf> =
            cc.storage.and_then(|s| s.get_string("last_file")).map(PathBuf::from).filter(|p| p.exists());
        let mut app = App {
            part: Part::new("Part1"),
            path: None,
            disk_stamp: None,
            undo: vec![],
            redo: vec![],
            generation: 0,
            worker,
            vp,
            status: vec![],
            sketches: vec![],
            mass: None,
            millis: 0.0,
            cache_hits: 0,
            selected: None,
            picked_point: None,
            toast: None,
            shape: ShapeForm { kind: ShapeKind::Rectangle, a: [-20.0, -10.0], b: [20.0, 10.0], r: 5.0, sides: 6, construction: false },
            new_param: (String::new(), String::new()),
            dim_form: DimForm { kind: 0, a: String::new(), b: String::new(), value: "10".into() },
            last_poll: Instant::now(),
            last_frame: Instant::now(),
            overlay_dirty: true,
            first_mesh: true,
            sketcher: None,
            sel_edges: vec![],
            hover_edge: None,
            multi: vec![],
            ref_planes: vec![],
            part_mesh: None,
            asm: None,
            asm_active: false,
        };
        match open.or(remembered) {
            Some(p) if p.extension().is_some_and(|e| e == "rwasm") => {
                app.regen();
                app.open_assembly(p);
            }
            Some(p) if p.exists() => app.open(p),
            Some(p) => {
                app.part = Part::new(p.file_stem().and_then(|s| s.to_str()).unwrap_or("Part1"));
                app.path = Some(p);
                app.save();
                app.regen();
            }
            None => app.regen(),
        }
        app
    }

    // ---------- document ops ----------

    fn regen(&mut self) {
        self.generation += 1;
        self.worker.send(Request::Regen(self.generation, self.part.clone()));
    }

    fn commit(&mut self, f: impl FnOnce(&mut Part)) {
        let before = self.part.clone();
        f(&mut self.part);
        if self.part != before {
            self.undo.push(before);
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.save();
            self.regen();
            self.overlay_dirty = true;
        }
    }

    fn undo(&mut self) {
        if let Some(p) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.part, p));
            self.save();
            self.regen();
        }
    }

    fn redo(&mut self) {
        if let Some(p) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.part, p));
            self.save();
            self.regen();
        }
    }

    fn save(&mut self) {
        if let Some(path) = &self.path {
            match write_part(path, &self.part) {
                Ok(()) => self.disk_stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok(),
                Err(e) => self.notify(format!("Save failed: {e:#}"), true),
            }
        }
    }

    fn open(&mut self, p: PathBuf) {
        match read_part(&p) {
            Ok(part) => {
                self.part = part;
                self.disk_stamp = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
                self.path = Some(p);
                self.undo.clear();
                self.redo.clear();
                self.selected = None;
                self.first_mesh = true;
                self.regen();
            }
            Err(e) => self.notify(format!("Open failed: {e:#}"), true),
        }
    }

    /// Live sync: reload when another process (e.g. the MCP server) changed the file.
    fn poll_disk(&mut self) {
        if self.last_poll.elapsed().as_millis() < 400 {
            return;
        }
        self.last_poll = Instant::now();
        let Some(path) = self.path.clone() else { return };
        let st = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if st.is_some() && st != self.disk_stamp {
            self.disk_stamp = st;
            if let Ok(part) = read_part(&path) {
                if part != self.part {
                    self.undo.push(std::mem::replace(&mut self.part, part));
                    self.redo.clear();
                    self.notify("Reloaded external changes (live sync)".into(), false);
                    self.regen();
                }
            }
        }
    }

    fn notify(&mut self, msg: String, err: bool) {
        self.toast = Some((msg, Instant::now(), err));
    }

    fn apply_rebuilt(&mut self, r: Box<Rebuilt>, rs: Option<&egui_wgpu::RenderState>) {
        if r.generation != self.generation {
            return; // stale
        }
        self.worker.busy = false;
        // Adopt solved sketch coordinates without creating an undo step.
        self.part = r.solved;
        self.status = r.status;
        self.sketches = r.sketches;
        self.mass = r.mass;
        self.millis = r.millis;
        self.cache_hits = r.cache_hits;
        self.ref_planes = r.ref_planes;
        self.part_mesh = r.mesh.clone();
        if self.asm_active {
            return;
        }
        if let (Some(vp), Some(rs)) = (self.vp.as_mut(), rs) {
            vp.set_mesh(&rs.device, r.mesh);
            if self.first_mesh && vp.mesh().is_some() {
                vp.fit(None, false);
                self.first_mesh = false;
            }
        }
        self.overlay_dirty = true;
    }

    fn selected_feature(&self) -> Option<&Feature> {
        self.selected.as_ref().and_then(|id| self.part.feature(id))
    }

    fn status_of(&self, id: &str) -> Option<&FeatureStatus> {
        self.status.iter().find(|s| s.id == id)
    }

    fn add_feature(&mut self, base: &str, kind: FeatureKind) {
        let mut new_id = String::new();
        self.commit(|p| {
            let (id, name) = p.fresh_ids(base);
            new_id = id.clone();
            p.features.push(Feature { id, name, suppressed: false, kind });
        });
        self.selected = Some(new_id);
        self.overlay_dirty = true;
    }

    fn selected_face_plane(&self) -> Option<PlaneRef> {
        let f = self.vp.as_ref()?.selected_face?;
        let m = self.vp.as_ref()?.mesh()?;
        let info = m.faces.iter().find(|x| x.index as u32 == f)?;
        plane_from_face(info)
    }

    /// Resolves any plane reference, including reference-plane features.
    fn plane_of(&self, p: &PlaneRef) -> Option<Plane> {
        let mut map: std::collections::HashMap<String, Plane> = self.ref_planes.iter().cloned().collect();
        for (id, pl) in &self.ref_planes {
            if let Some(f) = self.part.feature(id) {
                map.insert(f.name.clone(), *pl);
            }
        }
        resolve_plane_in(p, &Scope::new(&self.part.parameters), &map).ok()
    }

    /// (id, name) of every reference plane feature.
    fn ref_plane_list(&self) -> Vec<(String, String)> {
        self.part.features.iter().filter(|f| matches!(f.kind, FeatureKind::Plane(_))).map(|f| (f.id.clone(), f.name.clone())).collect()
    }

    // ---------- assembly mode ----------

    fn open_assembly(&mut self, p: PathBuf) {
        match rw_core::assembly::AsmModel::open(&p) {
            Ok(m) => self.activate_assembly(AsmUi::new(m)),
            Err(e) => self.notify(format!("Open assembly failed: {e:#}"), true),
        }
    }

    fn activate_assembly(&mut self, a: AsmUi) {
        self.asm = Some(a);
        self.set_asm_active(true);
    }

    fn set_asm_active(&mut self, on: bool) {
        self.asm_active = on && self.asm.is_some();
        self.sketcher = None;
        self.sel_edges.clear();
        if let Some(vp) = self.vp.as_mut() {
            vp.selected_face = None;
        }
        if let Some(a) = self.asm.as_mut() {
            a.mesh_dirty = true;
            a.fit_pending = true;
        }
        self.first_mesh = true;
        self.overlay_dirty = true;
        if !self.asm_active {
            // restore the part mesh
            self.regen();
        }
    }

    // ---------- UI ----------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New Part").clicked() {
                    self.undo.push(std::mem::replace(&mut self.part, Part::new("Part1")));
                    self.path = None;
                    self.selected = None;
                    self.first_mesh = true;
                    self.regen();
                }
                if ui.button("Open…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("RustWorks part", &["rwpart"]).pick_file() {
                        self.open(p);
                    }
                }
                if ui.button("Save").clicked() {
                    self.save_or_save_as();
                }
                if ui.button("Save As…").clicked() {
                    self.save_as();
                }
                ui.separator();
                if ui.button("New Assembly…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("Assembly1.rwasm").add_filter("RustWorks assembly", &["rwasm"]).save_file() {
                        match AsmUi::create(p) {
                            Ok(a) => self.activate_assembly(a),
                            Err(e) => self.notify(format!("Could not create assembly: {e:#}"), true),
                        }
                    }
                }
                if ui.button("Open Assembly…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("RustWorks assembly", &["rwasm"]).pick_file() {
                        self.open_assembly(p);
                    }
                }
                if self.asm_active && ui.button("Export Assembly STEP…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("assembly.step").add_filter("step", &["step"]).save_file() {
                        let r = self.asm.as_ref().unwrap().model.export_step(&p);
                        match r {
                            Ok(()) => self.notify(format!("Exported {}", p.display()), false),
                            Err(e) => self.notify(format!("Export failed: {e:#}"), true),
                        }
                    }
                }
                ui.separator();
                if ui.button("Import STEP…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("STEP", &["step", "stp"]).pick_file() {
                        self.add_feature("Import", FeatureKind::Import(Import { path: p.display().to_string(), op: Op::Boss }));
                    }
                }
                ui.separator();
                for (label, fmt) in [("Export STEP…", "step"), ("Export STL…", "stl"), ("Export OBJ…", "obj")] {
                    if ui.button(label).clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_file_name(format!("{}.{fmt}", self.part.name))
                            .add_filter(fmt, &[fmt])
                            .save_file()
                        {
                            self.worker.send(Request::Export(fmt.into(), p));
                        }
                    }
                }
            });
            if self.asm.is_some() {
                ui.separator();
                let mut want: Option<bool> = None;
                if ui.selectable_label(!self.asm_active, "Part").on_hover_text("Edit the open part").clicked() {
                    want = Some(false);
                }
                let an = self.asm.as_ref().map(|a| a.model.asm.name.clone()).unwrap_or_default();
                if ui.selectable_label(self.asm_active, format!("Assembly: {an}")).clicked() {
                    want = Some(true);
                }
                if let Some(w) = want {
                    if w != self.asm_active {
                        self.set_asm_active(w);
                    }
                }
                ui.separator();
            }
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo  Cmd+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo  Shift+Cmd+Z")).clicked() {
                    self.redo();
                }
            });
            ui.menu_button("View", |ui| {
                if let Some(vp) = self.vp.as_mut() {
                    ui.checkbox(&mut vp.show_edges, "Show edges");
                    let mut persp = !vp.cam.ortho;
                    if ui.checkbox(&mut persp, "Perspective").changed() {
                        vp.cam.ortho = !persp;
                    }
                    if ui.button("Zoom to fit (F)").clicked() {
                        vp.fit(None, true);
                    }
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label("RustWorks — parametric CAD in Rust");
                ui.label("Orbit: left-drag · Pan: right/middle-drag or Shift+drag · Zoom: scroll");
                ui.label("AI design: connect the rustworks-mcp server; this window live-syncs with the same .rwpart file.");
            });
        });
    }

    fn save_or_save_as(&mut self) {
        if self.path.is_some() {
            self.save();
            self.notify("Saved".into(), false);
        } else {
            self.save_as();
        }
    }

    fn save_as(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_file_name(format!("{}.rwpart", self.part.name))
            .add_filter("RustWorks part", &["rwpart"])
            .save_file()
        {
            self.path = Some(p);
            self.save();
            self.notify("Saved".into(), false);
        }
    }

    fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let sel = self.selected_feature().cloned();
            let sel_sketch = sel.as_ref().filter(|f| matches!(f.kind, FeatureKind::Sketch(_))).map(|f| f.id.clone());
            let sel_solid = sel
                .as_ref()
                .filter(|f| !matches!(f.kind, FeatureKind::Sketch(_)))
                .map(|f| f.id.clone());
            let face_plane = self.selected_face_plane();

            let editing = self.sketcher.is_some();
            if editing {
                if ui.button(RichText::new("Exit Sketch").strong().color(ACCENT)).clicked() {
                    self.sketcher = None;
                    self.overlay_dirty = true;
                }
                if let Some(sk) = self.sketcher.as_mut() {
                    for (t, label, tip) in crate::sketcher::Tool::ALL {
                        if ui.selectable_label(sk.tool == t, label).on_hover_text(tip).clicked() {
                            sk.tool = t;
                            sk.cancel();
                        }
                    }
                    match sk.tool {
                        crate::sketcher::Tool::Fillet => {
                            ui.add(egui::DragValue::new(&mut sk.fillet_radius).speed(0.1).range(0.001..=1e6).prefix("R "));
                        }
                        crate::sketcher::Tool::Offset => {
                            ui.add(egui::DragValue::new(&mut sk.offset_dist).speed(0.1).range(0.001..=1e6).prefix("d "));
                        }
                        _ => {}
                    }
                }
                ui.separator();
            } else if let Some(id) = sel_sketch.clone() {
                if ui.button(RichText::new("Edit Sketch").strong()).on_hover_text("Draw with the mouse (or double-click the sketch in the tree)").clicked() {
                    self.enter_sketch(&id);
                }
            }
            ui.menu_button(RichText::new("Sketch").strong(), |ui| {
                for (label, plane) in [
                    ("Front Plane", PlaneRef::Named(StdPlane::Front)),
                    ("Top Plane", PlaneRef::Named(StdPlane::Top)),
                    ("Right Plane", PlaneRef::Named(StdPlane::Right)),
                ] {
                    if ui.button(label).clicked() {
                        self.new_sketch(plane);
                    }
                }
                for (id, name) in self.ref_plane_list() {
                    if ui.button(format!("{name} (reference plane)")).clicked() {
                        self.new_sketch(PlaneRef::Feature(id));
                    }
                }
                if ui.add_enabled(face_plane.is_some(), egui::Button::new("On Selected Face")).clicked() {
                    self.new_sketch(face_plane.clone().unwrap());
                }
            });
            let sel_plane_feat = sel.as_ref().filter(|f| matches!(f.kind, FeatureKind::Plane(_))).map(|f| f.id.clone());
            if ui
                .button("Plane")
                .on_hover_text("Reference plane: offset from the selected face, selected reference plane, or the Top plane")
                .clicked()
            {
                let base = face_plane.clone().or(sel_plane_feat.clone().map(PlaneRef::Feature)).unwrap_or(PlaneRef::Named(StdPlane::Top));
                self.add_feature("Plane", FeatureKind::Plane(RefPlane { base: Some(base), offset: 10.0.into(), angle: None, points: None, flip: false }));
            }
            ui.separator();
            let tool = |ui: &mut egui::Ui, label: &str, enabled: bool, tip: &str| {
                ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text(tip).clicked()
            };
            if tool(ui, "Extrude", sel_sketch.is_some(), "Extruded Boss/Base from the selected sketch") {
                let s = sel_sketch.clone().unwrap();
                self.add_feature("Extrude", FeatureKind::Extrude(Extrude { sketch: s, depth: 10.0.into(), end: EndCondition::Blind, reverse: false, op: Op::Boss }));
            }
            if tool(ui, "Cut", sel_sketch.is_some(), "Extruded Cut (through all) from the selected sketch") {
                let s = sel_sketch.clone().unwrap();
                self.add_feature("Cut-Extrude", FeatureKind::Extrude(Extrude { sketch: s, depth: 10.0.into(), end: EndCondition::ThroughAll, reverse: true, op: Op::Cut }));
            }
            let two_sketches: Vec<String> = {
                let mut v: Vec<String> = self
                    .selected
                    .iter()
                    .chain(self.multi.iter())
                    .filter(|id| matches!(self.part.feature(id).map(|f| &f.kind), Some(FeatureKind::Sketch(_))))
                    .cloned()
                    .collect();
                v.dedup();
                v
            };
            if tool(ui, "Sweep", two_sketches.len() == 2, "Sweep a profile sketch along a path sketch (select one, Cmd/Ctrl-click the other)") {
                // the sketch with a closed profile is the profile; the other is the path
                let has_profile = |id: &String| self.sketches.iter().any(|s| &s.feature == id && s.profile_error.is_none() && !s.regions.is_empty());
                let (profile, path) = if has_profile(&two_sketches[0]) && !has_profile(&two_sketches[1]) {
                    (two_sketches[0].clone(), two_sketches[1].clone())
                } else {
                    (two_sketches[1].clone(), two_sketches[0].clone())
                };
                self.add_feature("Sweep", FeatureKind::Sweep(Sweep { profile, path, op: Op::Boss }));
                self.multi.clear();
            }
            if tool(ui, "Revolve", sel_sketch.is_some(), "Revolved Boss about the sketch Y axis") {
                let s = sel_sketch.clone().unwrap();
                self.add_feature("Revolve", FeatureKind::Revolve(Revolve { sketch: s, axis: AxisRef::Entity("y_axis".into()), angle: 360.0.into(), op: Op::Boss }));
            }
            let hole_ok = face_plane.is_some() && self.picked_point.is_some();
            if tool(ui, "Hole", hole_ok, "Hole Wizard at the clicked point of the selected planar face") {
                if let (Some(plane), Some(p)) = (face_plane.clone(), self.picked_point) {
                    if let Some(pl) = self.plane_of(&plane) {
                        let d = [p.x as f64 - pl.origin[0], p.y as f64 - pl.origin[1], p.z as f64 - pl.origin[2]];
                        let uv = [round3(dot3(d, pl.x)), round3(dot3(d, pl.y))];
                        self.add_feature("Hole", FeatureKind::Hole(Hole { plane, points: vec![uv], diameter: 6.0.into(), depth: None, counterbore: None, countersink: None }));
                    }
                }
            }
            ui.separator();
            if tool(ui, "Linear Pattern", sel_solid.is_some(), "Linear pattern of the selected feature") {
                let f = sel_solid.clone().unwrap();
                self.add_feature("LPattern", FeatureKind::LinearPattern(LinearPattern { features: vec![f], direction: [1.0, 0.0, 0.0], spacing: 20.0.into(), count: 3.0.into(), direction2: None, spacing2: None, count2: None }));
            }
            if tool(ui, "Circular Pattern", sel_solid.is_some(), "Circular pattern of the selected feature about the Y axis") {
                let f = sel_solid.clone().unwrap();
                self.add_feature("CirPattern", FeatureKind::CircularPattern(CircularPattern { features: vec![f], axis_origin: [0.0; 3], axis_direction: [0.0, 1.0, 0.0], count: 6.0.into(), angle: 360.0.into() }));
            }
            if tool(ui, "Mirror", self.mass.is_some(), "Mirror the selected feature (or the whole body) about the Right plane") {
                let features = sel_solid.clone().map(|f| vec![f]).unwrap_or_default();
                self.add_feature("Mirror", FeatureKind::Mirror(Mirror { features, plane: PlaneRef::Named(StdPlane::Right) }));
            }
            ui.separator();
            let n_edges = self.sel_edges.len();
            if tool(ui, "Fillet", n_edges > 0, "Round the selected edges (click edges in the viewport; Shift adds)") {
                let edges = self.sel_edges.iter().map(|e| EdgeRef::Point(e.1)).collect();
                self.add_feature("Fillet", FeatureKind::Fillet(Fillet { edges, all_edges: false, radius: 2.0.into() }));
                self.sel_edges.clear();
            }
            if tool(ui, "Chamfer", n_edges > 0, "Bevel the selected edges") {
                let edges = self.sel_edges.iter().map(|e| EdgeRef::Point(e.1)).collect();
                self.add_feature("Chamfer", FeatureKind::Chamfer(Chamfer { edges, all_edges: false, distance: 1.0.into() }));
                self.sel_edges.clear();
            }
            let shell_ok = self.picked_point.is_some() && self.vp.as_ref().is_some_and(|v| v.selected_face.is_some());
            if tool(ui, "Shell", shell_ok, "Hollow the part, removing the selected face") {
                let p = self.picked_point.unwrap();
                self.add_feature("Shell", FeatureKind::Shell(Shell { open_faces: vec![[p.x as f64, p.y as f64, p.z as f64].map(round3)], thickness: 2.0.into() }));
            }
            if tool(ui, "Draft", shell_ok, "Taper the selected face 3° about the Top plane (edit the neutral plane in the PropertyManager)") {
                let p = self.picked_point.unwrap();
                self.add_feature(
                    "Draft",
                    FeatureKind::Draft(Draft { faces: vec![[p.x as f64, p.y as f64, p.z as f64].map(round3)], neutral: PlaneRef::Named(StdPlane::Top), angle: 3.0.into(), reverse: false }),
                );
            }
            let loft_sk: Vec<String> = self
                .multi
                .iter()
                .chain(self.selected.iter())
                .filter(|id| matches!(self.part.feature(id).map(|f| &f.kind), Some(FeatureKind::Sketch(_))))
                .cloned()
                .collect();
            let mut loft_sk_sorted = loft_sk.clone();
            loft_sk_sorted.sort_by_key(|id| self.part.feature_index(id));
            loft_sk_sorted.dedup();
            if tool(ui, "Loft", loft_sk_sorted.len() >= 2, "Loft through the selected sketches (Cmd/Ctrl-click sketches in the tree)") {
                self.add_feature("Loft", FeatureKind::Loft(Loft { sketches: loft_sk_sorted, op: Op::Boss }));
                self.multi.clear();
            }
            ui.separator();
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Undo (Cmd+Z)").clicked() {
                self.undo();
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Redo")).on_hover_text("Redo (Shift+Cmd+Z)").clicked() {
                self.redo();
            }
        });
    }

    fn new_sketch(&mut self, plane: PlaneRef) {
        self.add_feature("Sketch", FeatureKind::Sketch(SketchFeature { plane, sketch: Default::default() }));
        if let Some(id) = self.selected.clone() {
            self.enter_sketch(&id);
        }
    }

    /// Enters interactive sketch mode and turns the view normal to the sketch.
    fn enter_sketch(&mut self, id: &str) {
        let Some(FeatureKind::Sketch(sf)) = self.part.feature(id).map(|f| f.kind.clone()) else { return };
        if let Some(pl) = self.plane_of(&sf.plane) {
            if let Some(vp) = self.vp.as_mut() {
                let n = pl.normal();
                vp.view_to(Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32), Vec3::new(pl.y[0] as f32, pl.y[1] as f32, pl.y[2] as f32));
            }
        }
        self.selected = Some(id.to_string());
        self.sketcher = Some(crate::sketcher::Sketcher::new(id.to_string()));
        self.overlay_dirty = true;
    }

    fn feature_tree(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Part:").weak());
            ui.label(RichText::new(&self.part.name).strong().size(16.0));
        });
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new("Σ Equations / Parameters").strong()).default_open(true).show(ui, |ui| {
            let scope_params = self.part.parameters.clone();
            let scope = Scope::new(&scope_params);
            let mut change: Option<(String, Option<Value>)> = None;
            egui::Grid::new("params").num_columns(3).striped(true).show(ui, |ui| {
                for (k, v) in &scope_params {
                    ui.label(k);
                    let mut vv = v.clone();
                    if value_edit(ui, egui::Id::new(("param", k)), &mut vv, 90.0) {
                        change = Some((k.clone(), Some(vv)));
                    }
                    let ev = scope.eval(v);
                    match ev {
                        Ok(x) => ui.label(RichText::new(format!("= {}", fmt_num(x))).weak()),
                        Err(e) => ui.label(RichText::new("!").color(Color32::RED)).on_hover_text(e.to_string()),
                    };
                    if ui.small_button("x").on_hover_text("Delete parameter").clicked() {
                        change = Some((k.clone(), None));
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_param.0).hint_text("name").desired_width(70.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_param.1).hint_text("value / expr").desired_width(80.0));
                if ui.button("+").clicked() && !self.new_param.0.trim().is_empty() {
                    let v = parse_value(&self.new_param.1);
                    change = Some((self.new_param.0.trim().to_string(), Some(v)));
                    self.new_param = (String::new(), String::new());
                }
            });
            if let Some((k, v)) = change {
                self.commit(|p| match v {
                    Some(v) => {
                        p.parameters.insert(k, v);
                    }
                    None => {
                        p.parameters.remove(&k);
                    }
                });
            }
        });
        ui.separator();
        ui.label(RichText::new("FeatureManager").strong());
        ui.label(RichText::new("Front Plane · Top Plane · Right Plane · Origin").weak().small());
        ui.add_space(2.0);
        let features = self.part.features.clone();
        let mut action: Option<(String, &'static str)> = None;
        let mut enter: Option<String> = None;
        egui::ScrollArea::vertical().id_salt("tree").auto_shrink([false, true]).show(ui, |ui| {
            for f in &features {
                let st = self.status_of(&f.id);
                let icon = match &f.kind {
                    FeatureKind::Sketch(_) => "S",
                    FeatureKind::Extrude(e) if e.op == Op::Cut => "C",
                    FeatureKind::Extrude(_) => "E",
                    FeatureKind::Revolve(_) => "R",
                    FeatureKind::Hole(_) => "H",
                    FeatureKind::LinearPattern(_) | FeatureKind::CircularPattern(_) => "P",
                    FeatureKind::Mirror(_) => "M",
                    FeatureKind::Fillet(_) => "F",
                    FeatureKind::Chamfer(_) => "Ch",
                    FeatureKind::Shell(_) => "Sh",
                    FeatureKind::Loft(_) => "L",
                    FeatureKind::Import(_) => "I",
                    FeatureKind::Plane(_) => "Pl",
                    FeatureKind::Sweep(_) => "Sw",
                    FeatureKind::Draft(_) => "D",
                };
                let mut text = RichText::new(format!("[{icon}]  {}", f.name));
                let mut tip = format!("{} · id {}", f.kind.type_name(), f.id);
                if f.suppressed {
                    text = text.color(Color32::GRAY).italics();
                    tip += "\nSuppressed";
                } else if let Some(st) = st {
                    if let Some(e) = &st.error {
                        text = text.color(Color32::from_rgb(200, 30, 30));
                        tip += &format!("\nError: {e}");
                    } else if let Some(w) = &st.warning {
                        text = text.color(Color32::from_rgb(190, 120, 0));
                        tip += &format!("\nWarning: {w}");
                    }
                    if let Some(rep) = &st.sketch {
                        tip += &format!("\n{}", solve_text(rep.status, rep.dof));
                    }
                }
                let is_sel = self.selected.as_deref() == Some(f.id.as_str()) || self.multi.contains(&f.id);
                let r = ui.selectable_label(is_sel, text).on_hover_text(tip);
                if r.double_clicked() && matches!(f.kind, FeatureKind::Sketch(_)) {
                    enter = Some(f.id.clone());
                } else if r.clicked() {
                    if ui.input(|i| i.modifiers.command) {
                        if let Some(pos) = self.multi.iter().position(|x| x == &f.id) {
                            self.multi.remove(pos);
                        } else {
                            self.multi.push(f.id.clone());
                        }
                    } else {
                        self.multi.clear();
                        self.selected = if is_sel { None } else { Some(f.id.clone()) };
                    }
                    self.overlay_dirty = true;
                }
                r.context_menu(|ui| {
                    if ui.button(if f.suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                        action = Some((f.id.clone(), "suppress"));
                        ui.close();
                    }
                    if ui.button("Move up").clicked() {
                        action = Some((f.id.clone(), "up"));
                        ui.close();
                    }
                    if ui.button("Move down").clicked() {
                        action = Some((f.id.clone(), "down"));
                        ui.close();
                    }
                    if ui.button(RichText::new("Delete (with dependents)").color(Color32::RED)).clicked() {
                        action = Some((f.id.clone(), "delete"));
                        ui.close();
                    }
                });
            }
            if features.is_empty() {
                ui.label(RichText::new("Start with Sketch -> pick a plane.").weak().italics());
            }
        });
        if let Some((id, what)) = action {
            self.feature_action(&id, what);
        }
        if let Some(id) = enter {
            self.enter_sketch(&id);
        }
    }

    fn feature_action(&mut self, id: &str, what: &str) {
        match what {
            "suppress" => self.commit(|p| {
                if let Some(f) = p.feature_mut(id) {
                    f.suppressed = !f.suppressed;
                }
            }),
            "up" | "down" => self.commit(|p| {
                if let Some(i) = p.feature_index(id) {
                    let j = if what == "up" { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < p.features.len()) };
                    if let Some(j) = j {
                        p.features.swap(i, j);
                    }
                }
            }),
            "delete" => {
                self.commit(|p| {
                    let mut doomed = vec![id.to_string()];
                    let mut i = 0;
                    while i < doomed.len() {
                        for d in p.dependents(&doomed[i]) {
                            if !doomed.contains(&d) {
                                doomed.push(d);
                            }
                        }
                        i += 1;
                    }
                    p.features.retain(|f| !doomed.contains(&f.id));
                });
                self.selected = None;
                self.overlay_dirty = true;
            }
            _ => {}
        }
    }

    fn property_manager(&mut self, ui: &mut egui::Ui) {
        let Some(f) = self.selected_feature().cloned() else {
            ui.add_space(6.0);
            ui.label(RichText::new("PropertyManager").strong());
            ui.label(RichText::new("Select a feature in the tree to edit it.").weak());
            if let (Some(vp), Some(mp)) = (self.vp.as_ref(), self.mass) {
                if let Some(face) = vp.selected_face.and_then(|i| vp.mesh().and_then(|m| m.faces.iter().find(|f| f.index as u32 == i))) {
                    ui.separator();
                    ui.label(RichText::new(format!("Face {}", face.index)).strong());
                    ui.label(format!("{} · area {:.2} mm²", face.kind, face.area));
                    ui.label(format!("normal ({:.3}, {:.3}, {:.3})", face.normal[0], face.normal[1], face.normal[2]));
                    ui.label(RichText::new("Use Sketch -> On Selected Face, or Hole at the clicked point.").weak().small());
                }
                ui.separator();
                ui.label(RichText::new("Mass properties").strong());
                ui.label(format!("Volume: {:.2} mm³", mp.volume));
                ui.label(format!("Surface: {:.2} mm²", mp.surface_area));
                ui.label(format!("Mass (steel): {:.1} g", mp.volume / 1000.0 * 7.85));
                ui.label(format!(
                    "Size: {:.2} × {:.2} × {:.2}",
                    mp.bbox_max[0] - mp.bbox_min[0],
                    mp.bbox_max[1] - mp.bbox_min[1],
                    mp.bbox_max[2] - mp.bbox_min[2]
                ));
            }
            return;
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} ", f.name)).strong().size(15.0));
            ui.label(RichText::new(f.kind.type_name()).weak());
        });
        if let Some(st) = self.status_of(&f.id) {
            if let Some(e) = &st.error {
                ui.colored_label(Color32::from_rgb(200, 30, 30), format!("Error: {e}"));
            }
            if let Some(w) = &st.warning {
                ui.colored_label(Color32::from_rgb(190, 120, 0), format!("Warning: {w}"));
            }
        }
        ui.separator();
        let mut nf = f.clone();
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(&mut nf.name).desired_width(140.0));
            if r.lost_focus() && nf.name != f.name && !nf.name.trim().is_empty() {
                changed = true;
            }
        });
        let fid = f.id.clone();
        let ref_planes = self.ref_plane_list();
        let sketch_ids: Vec<(String, String)> = self
            .part
            .features
            .iter()
            .filter(|x| matches!(x.kind, FeatureKind::Sketch(_)))
            .map(|x| (x.id.clone(), x.name.clone()))
            .collect();
        match &mut nf.kind {
            FeatureKind::Sketch(sf) => {
                changed |= self.sketch_editor(ui, &fid, sf);
            }
            FeatureKind::Extrude(e) => {
                changed |= sketch_combo(ui, &mut e.sketch, &sketch_ids);
                egui::Grid::new("ext").num_columns(2).show(ui, |ui| {
                    ui.label("Operation");
                    changed |= op_combo(ui, &mut e.op);
                    ui.end_row();
                    ui.label("End condition");
                    egui::ComboBox::from_id_salt("end")
                        .selected_text(match e.end {
                            EndCondition::Blind => "Blind",
                            EndCondition::MidPlane => "Mid Plane",
                            EndCondition::ThroughAll => "Through All",
                        })
                        .show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut e.end, EndCondition::Blind, "Blind").changed();
                            changed |= ui.selectable_value(&mut e.end, EndCondition::MidPlane, "Mid Plane").changed();
                            changed |= ui.selectable_value(&mut e.end, EndCondition::ThroughAll, "Through All").changed();
                        });
                    ui.end_row();
                    if e.end != EndCondition::ThroughAll {
                        ui.label("Depth");
                        changed |= value_edit(ui, egui::Id::new(("depth", &fid)), &mut e.depth, 100.0);
                        ui.end_row();
                    }
                    ui.label("Reverse");
                    changed |= ui.checkbox(&mut e.reverse, "").changed();
                    ui.end_row();
                });
            }
            FeatureKind::Revolve(r) => {
                changed |= sketch_combo(ui, &mut r.sketch, &sketch_ids);
                egui::Grid::new("rev").num_columns(2).show(ui, |ui| {
                    ui.label("Operation");
                    changed |= op_combo(ui, &mut r.op);
                    ui.end_row();
                    ui.label("Angle (°)");
                    changed |= value_edit(ui, egui::Id::new(("angle", &fid)), &mut r.angle, 100.0);
                    ui.end_row();
                    ui.label("Axis");
                    let mut txt = match &r.axis {
                        AxisRef::Entity(s) => s.clone(),
                        AxisRef::Explicit { .. } => "custom".into(),
                    };
                    let resp = ui.add(egui::TextEdit::singleline(&mut txt).desired_width(100.0)).on_hover_text("x_axis, y_axis or a sketch line id");
                    if resp.lost_focus() && txt != "custom" {
                        r.axis = AxisRef::Entity(txt);
                        changed = true;
                    }
                    ui.end_row();
                });
            }
            FeatureKind::Hole(h) => {
                egui::Grid::new("hole").num_columns(2).show(ui, |ui| {
                    ui.label("Diameter");
                    changed |= value_edit(ui, egui::Id::new(("hd", &fid)), &mut h.diameter, 100.0);
                    ui.end_row();
                    ui.label("Through all");
                    let mut thru = h.depth.is_none();
                    if ui.checkbox(&mut thru, "").changed() {
                        h.depth = if thru { None } else { Some(10.0.into()) };
                        changed = true;
                    }
                    ui.end_row();
                    if let Some(d) = &mut h.depth {
                        ui.label("Depth");
                        changed |= value_edit(ui, egui::Id::new(("hdep", &fid)), d, 100.0);
                        ui.end_row();
                    }
                    ui.label("Counterbore");
                    let mut cb = h.counterbore.is_some();
                    if ui.checkbox(&mut cb, "").changed() {
                        h.counterbore = cb.then(|| Counterbore { diameter: 10.0.into(), depth: 3.0.into() });
                        if cb {
                            h.countersink = None;
                        }
                        changed = true;
                    }
                    ui.end_row();
                    if let Some(c) = &mut h.counterbore {
                        ui.label("  C'bore Ø");
                        changed |= value_edit(ui, egui::Id::new(("cbd", &fid)), &mut c.diameter, 100.0);
                        ui.end_row();
                        ui.label("  C'bore depth");
                        changed |= value_edit(ui, egui::Id::new(("cbh", &fid)), &mut c.depth, 100.0);
                        ui.end_row();
                    }
                });
                ui.label(RichText::new(format!("{} location(s)", h.points.len())).weak());
                for (i, p) in h.points.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("#{}", i + 1));
                        changed |= ui.add(egui::DragValue::new(&mut p[0]).speed(0.5).prefix("x ")).changed();
                        changed |= ui.add(egui::DragValue::new(&mut p[1]).speed(0.5).prefix("y ")).changed();
                    });
                }
                if ui.small_button("+ location").clicked() {
                    let last = h.points.last().copied().unwrap_or([0.0, 0.0]);
                    h.points.push([last[0] + 10.0, last[1]]);
                    changed = true;
                }
            }
            FeatureKind::LinearPattern(p) => {
                egui::Grid::new("lp").num_columns(2).show(ui, |ui| {
                    ui.label("Direction");
                    changed |= vec3_edit(ui, &mut p.direction);
                    ui.end_row();
                    ui.label("Spacing");
                    changed |= value_edit(ui, egui::Id::new(("sp", &fid)), &mut p.spacing, 100.0);
                    ui.end_row();
                    ui.label("Count");
                    changed |= value_edit(ui, egui::Id::new(("ct", &fid)), &mut p.count, 100.0);
                    ui.end_row();
                    ui.label("Direction 2");
                    let mut two = p.direction2.is_some();
                    if ui.checkbox(&mut two, "").changed() {
                        p.direction2 = two.then_some([0.0, 0.0, 1.0]);
                        p.spacing2 = two.then(|| p.spacing.clone());
                        p.count2 = two.then(|| 2.0.into());
                        changed = true;
                    }
                    ui.end_row();
                    if let (Some(d), Some(s), Some(c)) = (&mut p.direction2, &mut p.spacing2, &mut p.count2) {
                        ui.label("  Dir 2");
                        changed |= vec3_edit(ui, d);
                        ui.end_row();
                        ui.label("  Spacing 2");
                        changed |= value_edit(ui, egui::Id::new(("sp2", &fid)), s, 100.0);
                        ui.end_row();
                        ui.label("  Count 2");
                        changed |= value_edit(ui, egui::Id::new(("ct2", &fid)), c, 100.0);
                        ui.end_row();
                    }
                });
            }
            FeatureKind::CircularPattern(p) => {
                egui::Grid::new("cp").num_columns(2).show(ui, |ui| {
                    ui.label("Axis origin");
                    changed |= vec3_edit(ui, &mut p.axis_origin);
                    ui.end_row();
                    ui.label("Axis direction");
                    changed |= vec3_edit(ui, &mut p.axis_direction);
                    ui.end_row();
                    ui.label("Count");
                    changed |= value_edit(ui, egui::Id::new(("cc", &fid)), &mut p.count, 100.0);
                    ui.end_row();
                    ui.label("Total angle");
                    changed |= value_edit(ui, egui::Id::new(("ca", &fid)), &mut p.angle, 100.0);
                    ui.end_row();
                });
            }
            FeatureKind::Fillet(fl) => {
                egui::Grid::new("fil").num_columns(2).show(ui, |ui| {
                    ui.label("Radius");
                    changed |= value_edit(ui, egui::Id::new(("fr", &fid)), &mut fl.radius, 100.0);
                    ui.end_row();
                    ui.label("All edges");
                    changed |= ui.checkbox(&mut fl.all_edges, "").changed();
                    ui.end_row();
                });
                ui.label(RichText::new(format!("{} edge reference(s)", fl.edges.len())).weak());
                if !self.sel_edges.is_empty() && ui.button(format!("Use {} selected edge(s)", self.sel_edges.len())).clicked() {
                    fl.edges = self.sel_edges.iter().map(|e| EdgeRef::Point(e.1)).collect();
                    changed = true;
                }
            }
            FeatureKind::Chamfer(c) => {
                egui::Grid::new("cha").num_columns(2).show(ui, |ui| {
                    ui.label("Distance");
                    changed |= value_edit(ui, egui::Id::new(("cd", &fid)), &mut c.distance, 100.0);
                    ui.end_row();
                    ui.label("All edges");
                    changed |= ui.checkbox(&mut c.all_edges, "").changed();
                    ui.end_row();
                });
                ui.label(RichText::new(format!("{} edge reference(s)", c.edges.len())).weak());
                if !self.sel_edges.is_empty() && ui.button(format!("Use {} selected edge(s)", self.sel_edges.len())).clicked() {
                    c.edges = self.sel_edges.iter().map(|e| EdgeRef::Point(e.1)).collect();
                    changed = true;
                }
            }
            FeatureKind::Shell(sh) => {
                ui.horizontal(|ui| {
                    ui.label("Thickness");
                    changed |= value_edit(ui, egui::Id::new(("st", &fid)), &mut sh.thickness, 100.0);
                });
                ui.label(RichText::new(format!("{} open face(s)", sh.open_faces.len())).weak());
                if let Some(p) = self.picked_point {
                    if ui.button("Add selected face as open").clicked() {
                        sh.open_faces.push([p.x as f64, p.y as f64, p.z as f64].map(round3));
                        changed = true;
                    }
                }
            }
            FeatureKind::Loft(l) => {
                changed |= op_combo(ui, &mut l.op);
                ui.label(RichText::new(format!("Sections: {}", l.sketches.join(" -> "))).weak());
            }
            FeatureKind::Import(im) => {
                changed |= op_combo(ui, &mut im.op);
                ui.label(RichText::new(&im.path).weak().small());
            }
            FeatureKind::Plane(rp) => {
                if let Some(base) = &mut rp.base {
                    ui.label("Base plane");
                    changed |= plane_combo(ui, base, &ref_planes);
                }
                egui::Grid::new("rplane").num_columns(2).show(ui, |ui| {
                    ui.label("Offset");
                    changed |= value_edit(ui, egui::Id::new(("rpo", &fid)), &mut rp.offset, 100.0);
                    ui.end_row();
                    ui.label("Tilt (°)").on_hover_text("Rotate about the base plane's X axis");
                    let mut ang = rp.angle.clone().unwrap_or_default();
                    if value_edit(ui, egui::Id::new(("rpa", &fid)), &mut ang, 100.0) {
                        rp.angle = if ang == Value::Num(0.0) { None } else { Some(ang) };
                        changed = true;
                    }
                    ui.end_row();
                    ui.label("Flip normal");
                    changed |= ui.checkbox(&mut rp.flip, "").changed();
                    ui.end_row();
                });
                if ui.button("Sketch on this plane").clicked() {
                    self.new_sketch(PlaneRef::Feature(fid.clone()));
                }
            }
            FeatureKind::Sweep(sw) => {
                egui::Grid::new("sweep").num_columns(2).show(ui, |ui| {
                    ui.label("Operation");
                    changed |= op_combo(ui, &mut sw.op);
                    ui.end_row();
                });
                ui.push_id("prof", |ui| {
                    ui.label("Profile");
                    changed |= sketch_combo(ui, &mut sw.profile, &sketch_ids);
                });
                ui.push_id("path", |ui| {
                    ui.label("Path");
                    changed |= sketch_combo(ui, &mut sw.path, &sketch_ids);
                });
                ui.label(RichText::new("The profile is swept from where it is drawn; place it at the start of the path.").weak().small());
            }
            FeatureKind::Draft(d) => {
                egui::Grid::new("draft").num_columns(2).show(ui, |ui| {
                    ui.label("Angle (°)");
                    changed |= value_edit(ui, egui::Id::new(("da", &fid)), &mut d.angle, 100.0);
                    ui.end_row();
                    ui.label("Reverse");
                    changed |= ui.checkbox(&mut d.reverse, "").changed();
                    ui.end_row();
                });
                ui.label("Neutral plane");
                changed |= plane_combo(ui, &mut d.neutral, &ref_planes);
                ui.label(RichText::new(format!("{} face(s)", d.faces.len())).weak());
                if let Some(p) = self.picked_point {
                    if ui.button("Add selected face").clicked() {
                        d.faces.push([p.x as f64, p.y as f64, p.z as f64].map(round3));
                        changed = true;
                    }
                }
            }
            FeatureKind::Mirror(m) => {
                ui.label("Mirror plane");
                changed |= plane_combo(ui, &mut m.plane, &ref_planes);
                ui.label(RichText::new(if m.features.is_empty() { "Mirroring the whole body".into() } else { format!("Features: {}", m.features.join(", ")) }).weak());
            }
        }
        if let FeatureKind::LinearPattern(LinearPattern { features, .. })
        | FeatureKind::CircularPattern(CircularPattern { features, .. }) = &nf.kind
        {
            ui.label(RichText::new(format!("Patterned: {}", features.join(", "))).weak());
        }
        if changed {
            self.commit(|p| {
                if let Some(slot) = p.feature_mut(&fid) {
                    *slot = nf;
                }
            });
            self.overlay_dirty = true;
        }
    }

    /// Sketch PropertyManager: solve status, driving dimensions, shapes, relations.
    fn sketch_editor(&mut self, ui: &mut egui::Ui, fid: &str, sf: &mut SketchFeature) -> bool {
        let mut changed = false;
        ui.label("Plane");
        let refs = self.ref_plane_list();
        changed |= plane_combo(ui, &mut sf.plane, &refs);
        if let Some(st) = self.status_of(fid).and_then(|s| s.sketch.clone()) {
            let (c, t) = match st.status {
                SolveStatus::FullyDefined => (Color32::BLACK, "Fully defined".to_string()),
                SolveStatus::UnderDefined => (ACCENT, format!("Under defined ({} DOF)", st.dof)),
                SolveStatus::OverDefined => (Color32::RED, "Over defined / conflicting".to_string()),
            };
            ui.colored_label(c, RichText::new(t).strong());
        }
        if let Some(e) = self.sketches.iter().find(|s| s.feature == fid).and_then(|s| s.profile_error.clone()) {
            ui.colored_label(Color32::from_rgb(190, 120, 0), format!("Profile: {e}"));
        }
        ui.separator();
        ui.label(RichText::new("Dimensions & relations").strong());
        let mut remove: Option<usize> = None;
        egui::Grid::new("dims").num_columns(3).striped(true).show(ui, |ui| {
            for (i, c) in sf.sketch.constraints.iter_mut().enumerate() {
                let (label, val) = constraint_label(c);
                ui.label(label);
                match val {
                    Some(v) => changed |= value_edit(ui, egui::Id::new(("dim", fid, i)), v, 80.0),
                    None => {
                        ui.label("");
                    }
                }
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            sf.sketch.constraints.remove(i);
            changed = true;
        }
        egui::CollapsingHeader::new("+ Add dimension / relation").show(ui, |ui| {
            let df = &mut self.dim_form;
            egui::ComboBox::from_id_salt("dimkind").selected_text(DIM_KINDS[df.kind]).show_ui(ui, |ui| {
                for (i, k) in DIM_KINDS.iter().enumerate() {
                    ui.selectable_value(&mut df.kind, i, *k);
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut df.a).hint_text("entity a").desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut df.b).hint_text("entity b").desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut df.value).hint_text("value").desired_width(60.0));
            });
            if ui.button("Add").clicked() {
                let mut j = serde_json::json!({"type": DIM_KINDS[df.kind]});
                let k = DIM_KINDS[df.kind];
                let v = serde_json::to_value(parse_value(&df.value)).unwrap();
                match k {
                    "radius" | "diameter" => {
                        j["entity"] = df.a.clone().into();
                        j["value"] = v;
                    }
                    "horizontal" | "vertical" => j["line"] = df.a.clone().into(),
                    "distance" => {
                        j["a"] = df.a.clone().into();
                        if !df.b.is_empty() {
                            j["b"] = df.b.clone().into();
                        }
                        j["value"] = v;
                    }
                    "parallel" | "perpendicular" | "equal" | "tangent" => {
                        j["a"] = df.a.clone().into();
                        j["b"] = df.b.clone().into();
                    }
                    _ => {
                        j["a"] = df.a.clone().into();
                        j["b"] = df.b.clone().into();
                        j["value"] = v;
                    }
                }
                match serde_json::from_value::<Constraint>(j) {
                    Ok(c) => {
                        sf.sketch.constraints.push(c);
                        changed = true;
                    }
                    Err(e) => self.toast = Some((format!("Invalid relation: {e}"), Instant::now(), true)),
                }
            }
        });
        ui.separator();
        ui.label(RichText::new("Add geometry").strong());
        let s = &mut self.shape;
        ui.horizontal_wrapped(|ui| {
            for (k, l) in [
                (ShapeKind::Rectangle, "Rect"),
                (ShapeKind::Circle, "Circle"),
                (ShapeKind::Line, "Line"),
                (ShapeKind::Polygon, "Polygon"),
                (ShapeKind::Slot, "Slot"),
                (ShapeKind::Arc, "Arc"),
            ] {
                ui.selectable_value(&mut s.kind, k, l);
            }
        });
        let pt = |ui: &mut egui::Ui, label: &str, p: &mut [f64; 2]| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::DragValue::new(&mut p[0]).speed(0.5).prefix("x "));
                ui.add(egui::DragValue::new(&mut p[1]).speed(0.5).prefix("y "));
            });
        };
        match s.kind {
            ShapeKind::Rectangle => {
                pt(ui, "Corner 1", &mut s.a);
                pt(ui, "Corner 2", &mut s.b);
            }
            ShapeKind::Line => {
                pt(ui, "From", &mut s.a);
                pt(ui, "To", &mut s.b);
                ui.checkbox(&mut s.construction, "Construction");
            }
            ShapeKind::Circle | ShapeKind::Polygon => {
                pt(ui, "Center", &mut s.a);
                ui.add(egui::DragValue::new(&mut s.r).speed(0.2).prefix("r "));
                if s.kind == ShapeKind::Polygon {
                    ui.add(egui::DragValue::new(&mut s.sides).range(3..=64).prefix("sides "));
                }
            }
            ShapeKind::Slot => {
                pt(ui, "Center 1", &mut s.a);
                pt(ui, "Center 2", &mut s.b);
                ui.add(egui::DragValue::new(&mut s.r).speed(0.2).prefix("r "));
            }
            ShapeKind::Arc => {
                pt(ui, "Center", &mut s.a);
                pt(ui, "Start", &mut s.b);
                ui.add(egui::DragValue::new(&mut s.r).speed(1.0).prefix("sweep° "));
            }
        }
        if ui.button(RichText::new("Add to sketch").strong()).clicked() {
            let sk = &mut sf.sketch;
            match s.kind {
                ShapeKind::Rectangle => {
                    sk.add_rectangle(s.a, s.b);
                }
                ShapeKind::Line => {
                    sk.add_line(s.a, s.b, s.construction);
                }
                ShapeKind::Circle => {
                    sk.add_circle(s.a, s.r.max(1e-3), false);
                }
                ShapeKind::Polygon => {
                    sk.add_polygon(s.a, s.r.max(1e-3), s.sides, 90.0);
                }
                ShapeKind::Slot => {
                    sk.add_slot(s.a, s.b, s.r.max(1e-3));
                }
                ShapeKind::Arc => {
                    let r = ((s.b[0] - s.a[0]).powi(2) + (s.b[1] - s.a[1]).powi(2)).sqrt();
                    let t0 = (s.b[1] - s.a[1]).atan2(s.b[0] - s.a[0]) + s.r.to_radians();
                    sk.add_arc(s.a, s.b, [s.a[0] + r * t0.cos(), s.a[1] + r * t0.sin()], false);
                }
            }
            changed = true;
        }
        egui::CollapsingHeader::new(format!("Entities ({})", sf.sketch.entities.len())).show(ui, |ui| {
            for e in &sf.sketch.entities {
                let t = match e {
                    Entity::Point { id, x, y, .. } => format!("{id}  ({}, {})", fmt_num(*x), fmt_num(*y)),
                    Entity::Line { id, start, end, construction } => format!("{id}  line {start}->{end}{}", if *construction { " (constr.)" } else { "" }),
                    Entity::Circle { id, center, radius, .. } => format!("{id}  circle @{center} r={}", fmt_num(*radius)),
                    Entity::Arc { id, center, start, end, .. } => format!("{id}  arc @{center} {start}->{end}"),
                    Entity::Spline { id, points, .. } => format!("{id}  spline {}", points.join("->")),
                };
                ui.label(RichText::new(t).monospace().small());
            }
        });
        changed
    }

    fn rebuild_overlay(&mut self, rs: &egui_wgpu::RenderState) {
        if self.vp.is_none() {
            return;
        }
        let consumed: Vec<String> = self.part.features.iter().flat_map(|f| f.kind.references()).collect();
        let mut lines: Vec<LineVertex> = vec![];
        // While editing a sketch, draw it live from the document.
        let editing: Option<SolvedSketch> = self.sketcher.as_ref().and_then(|sk| {
            let f = self.part.feature(&sk.feature)?;
            let FeatureKind::Sketch(sf) = &f.kind else { return None };
            let plane = self.plane_of(&sf.plane)?;
            Some(SolvedSketch { feature: f.id.clone(), plane, sketch: sf.sketch.clone(), report: None, regions: vec![], profile_error: None })
        });
        let editing_id = editing.as_ref().map(|e| e.feature.clone());
        for s in self.sketches.iter().filter(|s| Some(&s.feature) != editing_id.as_ref()).chain(editing.iter()) {
            let sel = self.selected.as_deref() == Some(s.feature.as_str()) || editing_id.as_deref() == Some(s.feature.as_str());
            if consumed.contains(&s.feature) && !sel {
                continue;
            }
            for (pl, construction) in s.polylines() {
                let c = if *&construction {
                    [0.45, 0.45, 0.5, 1.0]
                } else if sel {
                    [0.0, 0.45, 1.0, 1.0]
                } else {
                    [0.15, 0.3, 0.75, 1.0]
                };
                for (k, w) in pl.windows(2).enumerate() {
                    if construction && k % 2 == 1 {
                        continue;
                    }
                    lines.push(LineVertex { pos: w[0], color: c });
                    lines.push(LineVertex { pos: w[1], color: c });
                }
            }
        }
        // Reference planes (square outlines; selected one highlighted)
        if !self.asm_active {
            let half = self
                .mass
                .map(|m| {
                    let d = [m.bbox_max[0] - m.bbox_min[0], m.bbox_max[1] - m.bbox_min[1], m.bbox_max[2] - m.bbox_min[2]];
                    d[0].max(d[1]).max(d[2]) * 0.6
                })
                .unwrap_or(40.0)
                .max(10.0);
            for (id, pl) in &self.ref_planes {
                let sel = self.selected.as_deref() == Some(id.as_str());
                let c = if sel { [0.0, 0.45, 1.0, 1.0] } else { [0.85, 0.55, 0.1, 0.9] };
                let corners: Vec<[f32; 3]> = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]]
                    .iter()
                    .map(|q: &[f64; 2]| pl.to3d([q[0] * half, q[1] * half]).map(|v| v as f32))
                    .collect();
                for k in 0..4 {
                    lines.push(LineVertex { pos: corners[k], color: c });
                    lines.push(LineVertex { pos: corners[(k + 1) % 4], color: c });
                }
            }
        }
        // Origin axes
        let len = self.mass.map(|m| {
            let d = [m.bbox_max[0] - m.bbox_min[0], m.bbox_max[1] - m.bbox_min[1], m.bbox_max[2] - m.bbox_min[2]];
            ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 0.15) as f32
        });
        let len = len.unwrap_or(15.0).max(5.0);
        for (dir, c) in [([1.0, 0.0, 0.0], [0.85, 0.15, 0.15, 1.0]), ([0.0, 1.0, 0.0], [0.15, 0.6, 0.15, 1.0]), ([0.0, 0.0, 1.0], [0.15, 0.25, 0.9, 1.0])] {
            lines.push(LineVertex { pos: [0.0; 3], color: c });
            lines.push(LineVertex { pos: [dir[0] * len, dir[1] * len, dir[2] * len], color: c });
        }
        if let Some(vp) = self.vp.as_mut() {
            vp.set_overlay(&rs.device, &lines);
        }
        self.overlay_dirty = false;
    }

    fn viewport(&mut self, ui: &mut egui::Ui, rs: Option<&egui_wgpu::RenderState>) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        // Background gradient (SOLIDWORKS-like)
        let mut mesh = egui::Mesh::default();
        let top = Color32::from_rgb(250, 251, 253);
        let bot = Color32::from_rgb(196, 210, 228);
        mesh.colored_vertex(rect.left_top(), top);
        mesh.colored_vertex(rect.right_top(), top);
        mesh.colored_vertex(rect.right_bottom(), bot);
        mesh.colored_vertex(rect.left_bottom(), bot);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));

        let Some(rs) = rs else {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "GPU unavailable", egui::FontId::proportional(18.0), Color32::RED);
            return;
        };
        if self.overlay_dirty {
            self.rebuild_overlay(rs);
        }
        let found = self.sketcher.as_ref().and_then(|sk| {
            self.part.feature(&sk.feature).and_then(|f| match &f.kind {
                FeatureKind::Sketch(sf) => Some((sf.sketch.clone(), self.plane_of(&sf.plane)?)),
                _ => None,
            })
        });
        let asm_mode = self.asm_active;
        if asm_mode {
            if let (Some(a), Some(vp)) = (self.asm.as_mut(), self.vp.as_mut()) {
                a.sync_viewport(vp, &rs.device);
            }
        }
        let Some(vp) = self.vp.as_mut() else { return };
        let ppp = ui.ctx().pixels_per_point();
        let (w, h) = (rect.width(), rect.height());

        // Input
        let input = ui.input(|i| (i.pointer.hover_pos(), i.smooth_scroll_delta.y, i.modifiers.shift, i.pointer.primary_down(), i.pointer.secondary_down(), i.pointer.middle_down()));
        let (hover, scroll, shift, _p, sec, mid) = input;
        let sketching = self.sketcher.is_some();
        if resp.dragged() {
            let d = resp.drag_delta();
            if sketching {
                // left button belongs to the sketch tools
                if mid || (shift && sec) {
                    vp.cam.pan(d.x, d.y, h);
                } else if sec {
                    vp.cam.orbit(d.x, d.y);
                }
            } else if sec || mid || (shift && resp.dragged_by(egui::PointerButton::Primary)) {
                vp.cam.pan(d.x, d.y, h);
            } else if resp.dragged_by(egui::PointerButton::Primary) {
                vp.cam.orbit(d.x, d.y);
            }
        }
        let local = hover.filter(|p| rect.contains(*p)).map(|p| p - rect.min);
        let hit = local.and_then(|l| vp.pick(l.x, l.y, w, h));
        vp.hovered = if resp.dragged() { None } else { hit.map(|h| h.0) };
        if resp.hovered() && scroll.abs() > 0.0 {
            let toward = hit.map(|h| h.1).or_else(|| {
                local.map(|l| {
                    let (o, d) = vp.cam.ray(l.x, l.y, w, h);
                    // point on the plane through the target facing the camera
                    let n = vp.cam.eye_dir();
                    let t = (vp.cam.target - o).dot(n) / d.dot(n);
                    o + d * t
                })
            });
            vp.cam.zoom((-scroll * 0.0025).exp(), toward);
        }
        // Edge hover (screen-space distance to projected B-rep edges).
        self.hover_edge = None;
        if !sketching && !asm_mode && !resp.dragged() {
            if let (Some(m), Some(l)) = (vp.mesh(), local) {
                let mut best = 6.0f32;
                for (pl, id) in m.edges.iter().zip(&m.edge_ids) {
                    let sp: Vec<glam::Vec2> = pl.iter().filter_map(|p| vp.cam.project(Vec3::from(*p), w, h)).collect();
                    for s in sp.windows(2) {
                        let d = seg_dist2(glam::Vec2::new(l.x, l.y), s[0], s[1]);
                        if d < best {
                            best = d;
                            self.hover_edge = Some(*id);
                        }
                    }
                }
            }
        }
        if resp.clicked() && asm_mode {
            if let Some(a) = self.asm.as_mut() {
                match hit {
                    Some((f, p)) => a.pick(f, p, shift),
                    None if !shift => {
                        a.picks.clear();
                        a.sel_comp = None;
                        a.sel_mate = None;
                        a.mesh_dirty = true;
                    }
                    None => {}
                }
            }
        } else if resp.clicked() && !sketching {
            if let Some(e) = self.hover_edge {
                let mid = vp
                    .mesh()
                    .and_then(|m| m.edges.iter().zip(&m.edge_ids).find(|(_, id)| **id == e).map(|(pl, _)| pl[pl.len() / 2]))
                    .unwrap_or_default();
                if !shift {
                    self.sel_edges.clear();
                }
                if let Some(i) = self.sel_edges.iter().position(|x| x.0 == e) {
                    self.sel_edges.remove(i);
                } else {
                    self.sel_edges.push((e, mid.map(|v| v as f64)));
                }
                vp.selected_face = None;
            } else {
                vp.selected_face = hit.map(|h| h.0);
                self.picked_point = hit.map(|h| h.1);
                if !shift {
                    self.sel_edges.clear();
                }
            }
        }
        if resp.double_clicked() && !sketching {
            vp.fit(None, true);
        }
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.05);
        if vp.cam.tick(dt) {
            ui.ctx().request_repaint();
        }

        // Render 3D into texture and composite.
        let size = [(w * ppp).round() as u32, (h * ppp).round() as u32];
        if let Some(tex) = vp.render(rs, size) {
            painter.image(tex, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }

        // Selected / hovered edges
        if let Some(m) = vp.mesh() {
            for (pl, id) in m.edges.iter().zip(&m.edge_ids) {
                let sel = self.sel_edges.iter().any(|e| e.0 == *id);
                let hot = self.hover_edge == Some(*id);
                if !sel && !hot {
                    continue;
                }
                let pts: Vec<egui::Pos2> = pl.iter().filter_map(|p| vp.cam.project(Vec3::from(*p), w, h)).map(|v| rect.min + egui::vec2(v.x, v.y)).collect();
                let c = if sel { Color32::from_rgb(20, 110, 255) } else { Color32::from_rgb(255, 150, 20) };
                painter.add(egui::Shape::line(pts, egui::Stroke::new(3.0, c)));
            }
        }

        if let (true, Some(a)) = (asm_mode, self.asm.as_ref()) {
            a.paint_picks(&painter, rect, vp);
        }
        // Interactive sketching
        let mut sketch_edit: Option<(String, rw_core::sketch::Sketch)> = None;
        if let Some(sk) = self.sketcher.as_mut() {
            if let Some((sketch, plane)) = found {
                let del = ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
                if del {
                    if let Some(n) = sk.delete_hovered(ui, rect, &vp.cam, &plane, &sketch) {
                        sketch_edit = Some((sk.feature.clone(), n));
                    }
                } else if let Some(n) = sk.frame(ui, &resp, rect, &vp.cam, &plane, &sketch) {
                    sketch_edit = Some((sk.feature.clone(), n));
                }
            }
        }

        // Heads-up view toolbar
        let bar = egui::Rect::from_min_size(rect.center_top() + egui::vec2(-190.0, 8.0), egui::vec2(380.0, 28.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 215))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(6, 2))
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    let views: [(&str, Vec3, Vec3); 6] = [
                        ("Iso", Vec3::new(1.0, 0.8, 1.0), Vec3::Y),
                        ("Front", Vec3::Z, Vec3::Y),
                        ("Back", -Vec3::Z, Vec3::Y),
                        ("Top", Vec3::Y, -Vec3::Z),
                        ("Right", Vec3::X, Vec3::Y),
                        ("Left", -Vec3::X, Vec3::Y),
                    ];
                    for (l, eye, up) in views {
                        if ui.small_button(l).clicked() {
                            vp.view_to(eye, up);
                        }
                    }
                    if ui.small_button("Fit").clicked() {
                        vp.fit(None, true);
                    }
                    ui.toggle_value(&mut vp.show_edges, "Edges");
                });
            });

        // Orientation triad
        let o = rect.left_bottom() + egui::vec2(45.0, -45.0);
        for (dir, c, l) in [(Vec3::X, Color32::from_rgb(210, 40, 40), "X"), (Vec3::Y, Color32::from_rgb(30, 150, 30), "Y"), (Vec3::Z, Color32::from_rgb(40, 70, 220), "Z")] {
            let v = vp.cam.rot.inverse() * dir;
            let e = o + egui::vec2(v.x, -v.y) * 30.0;
            painter.line_segment([o, e], egui::Stroke::new(2.0, c));
            painter.text(e + egui::vec2(v.x, -v.y) * 8.0, egui::Align2::CENTER_CENTER, l, egui::FontId::proportional(12.0), c);
        }
        if self.worker.busy {
            painter.text(rect.right_top() + egui::vec2(-12.0, 14.0), egui::Align2::RIGHT_CENTER, "Rebuilding…", egui::FontId::proportional(14.0), ACCENT);
        }
        if let Some(f) = vp.hovered {
            if let Some(p) = hover {
                painter.text(p + egui::vec2(14.0, 14.0), egui::Align2::LEFT_TOP, format!("Face {f}"), egui::FontId::proportional(11.0), Color32::DARK_GRAY);
            }
        }
        if let Some(sk) = &self.sketcher {
            painter.text(rect.left_top() + egui::vec2(12.0, 14.0), egui::Align2::LEFT_CENTER, format!("SKETCH MODE · {:?} tool · right-drag rotate · Esc cancels · Del deletes", sk.tool), egui::FontId::proportional(13.0), ACCENT);
        }
        if let Some(m) = self.sketcher.as_mut().and_then(|s| s.message.take()) {
            self.toast = Some((m, Instant::now(), false));
        }
        if let Some((fid, nsk)) = sketch_edit {
            self.commit(|p| {
                if let Some(Feature { kind: FeatureKind::Sketch(sf), .. }) = p.feature_mut(&fid) {
                    sf.sketch = nsk;
                }
            });
        }
        if asm_mode {
            if self.asm.as_ref().is_some_and(|a| a.model.asm.components.is_empty()) {
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Insert Component to add parts to the assembly", egui::FontId::proportional(16.0), Color32::from_gray(110));
            }
        } else if self.part.features.is_empty() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Sketch -> choose a plane to begin\nor connect an AI via the RustWorks MCP server", egui::FontId::proportional(16.0), Color32::from_gray(110));
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let errs = self.status.iter().filter(|s| s.error.is_some()).count();
            if errs > 0 {
                ui.colored_label(Color32::RED, format!("Error: {errs} rebuild error(s)"));
            } else {
                ui.colored_label(Color32::from_rgb(30, 140, 60), "Rebuilt OK");
            }
            ui.label(format!("{:.0} ms", self.millis));
            ui.label(RichText::new(format!("cache {}/{}", self.cache_hits, self.part.features.len())).weak());
            if let Some(m) = self.vp.as_ref().and_then(|v| v.mesh()) {
                ui.label(RichText::new(format!("{} tris · {} faces", m.triangle_count(), m.faces.len())).weak());
            }
            if let Some(mp) = self.mass {
                ui.label(format!("V = {:.1} mm³", mp.volume));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &self.path {
                    Some(p) => ui.label(RichText::new(format!("live-sync: {}", p.display())).weak()),
                    None => ui.label(RichText::new("unsaved — File › Save As to enable live sync").weak()),
                };
                ui.label(RichText::new("mm · Y-up").weak());
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let rs = frame.wgpu_render_state().cloned();
        // Results from the regen worker
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
                Response::Regen(r) => self.apply_rebuilt(r, rs.as_ref()),
                Response::Exported(Ok(p)) => self.notify(format!("Exported {}", p.display()), false),
                Response::Exported(Err(e)) => self.notify(format!("Export failed: {e}"), true),
            }
        }
        self.poll_disk();
        if let Some(a) = self.asm.as_mut() {
            if self.asm_active {
                a.poll();
            }
            for (m, e) in std::mem::take(&mut a.notes) {
                self.toast = Some((m, Instant::now(), e));
            }
            if let Some(p) = a.open_part.take() {
                self.open(p);
                self.set_asm_active(false);
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));

        // Shortcuts
        let (undo, redo, save, fit, del) = ui.input_mut(|i| {
            let redo = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                redo || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::Delete),
            )
        });
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        let asm_mode = self.asm_active;
        if undo {
            match (asm_mode, self.asm.as_mut()) {
                (true, Some(a)) => a.undo(),
                _ => self.undo(),
            }
        }
        if redo {
            match (asm_mode, self.asm.as_mut()) {
                (true, Some(a)) => a.redo(),
                _ => self.redo(),
            }
        }
        if save {
            self.save_or_save_as();
        }
        if !typing {
            if fit {
                if let Some(vp) = self.vp.as_mut() {
                    vp.fit(None, true);
                }
            }
            if del && !asm_mode {
                if let Some(id) = self.selected.clone() {
                    self.feature_action(&id, "delete");
                }
            }
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        if let (true, Some(a)) = (asm_mode, self.asm.as_mut()) {
            egui::Panel::top("commands")
                .frame(egui::Frame::new().fill(Color32::from_rgb(240, 236, 228)).inner_margin(egui::Margin::symmetric(8, 5)))
                .show(ui, |ui| a.command_bar(ui));
            egui::Panel::bottom("status").show(ui, |ui| ui.horizontal(|ui| a.status_bar(ui)));
            egui::Panel::left("tree").resizable(true).default_size(250.0).show(ui, |ui| a.tree(ui));
            egui::Panel::right("props").resizable(true).default_size(290.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| a.property_manager(ui));
            });
        } else {
            egui::Panel::top("commands")
                .frame(egui::Frame::new().fill(Color32::from_rgb(236, 240, 246)).inner_margin(egui::Margin::symmetric(8, 5)))
                .show(ui, |ui| self.command_bar(ui));
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
            egui::Panel::left("tree").resizable(true).default_size(250.0).show(ui, |ui| self.feature_tree(ui));
            egui::Panel::right("props").resizable(true).default_size(290.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| self.property_manager(ui));
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| self.viewport(ui, rs.as_ref()));

        if let Some((msg, t, err)) = &self.toast {
            if t.elapsed().as_secs_f32() < 4.0 {
                let ctx = ui.ctx().clone();
                egui::Area::new(egui::Id::new("toast")).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -36.0)).show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.colored_label(if *err { Color32::RED } else { Color32::from_rgb(30, 120, 50) }, msg);
                    });
                });
            } else {
                self.toast = None;
            }
        }
        self.last_frame = Instant::now();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let (true, Some(p)) = (self.asm_active, self.asm.as_ref().and_then(|a| a.model.path.clone())) {
            storage.set_string("last_file", p.display().to_string());
        } else if let Some(p) = &self.path {
            storage.set_string("last_file", p.display().to_string());
        }
    }
}

// ---------- widgets & helpers ----------

/// Numeric-or-expression field. Commits on Enter / focus loss. Returns true if changed.
pub(crate) fn value_edit(ui: &mut egui::Ui, id: egui::Id, v: &mut Value, width: f32) -> bool {
    let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| v.to_string());
    let r = ui.add(egui::TextEdit::singleline(&mut text).id(id).desired_width(width));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
        false
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
        if r.lost_focus() {
            let nv = parse_value(&text);
            if nv != *v {
                *v = nv;
                return true;
            }
        }
        false
    }
}

fn parse_value(s: &str) -> Value {
    let s = s.trim();
    match s.parse::<f64>() {
        Ok(n) => Value::Num(n),
        Err(_) if s.is_empty() => Value::Num(0.0),
        Err(_) => Value::Expr(s.to_string()),
    }
}

fn vec3_edit(ui: &mut egui::Ui, v: &mut [f64; 3]) -> bool {
    vec3_edit_speed(ui, v, 0.05)
}

pub(crate) fn vec3_edit_speed(ui: &mut egui::Ui, v: &mut [f64; 3], speed: f64) -> bool {
    let mut c = false;
    ui.horizontal(|ui| {
        for (k, l) in ["x ", "y ", "z "].iter().enumerate() {
            c |= ui.add(egui::DragValue::new(&mut v[k]).speed(speed).prefix(*l)).changed();
        }
    });
    c
}

fn op_combo(ui: &mut egui::Ui, op: &mut Op) -> bool {
    let mut c = false;
    egui::ComboBox::from_id_salt("op").selected_text(if *op == Op::Boss { "Boss (add)" } else { "Cut (remove)" }).show_ui(ui, |ui| {
        c |= ui.selectable_value(op, Op::Boss, "Boss (add)").changed();
        c |= ui.selectable_value(op, Op::Cut, "Cut (remove)").changed();
    });
    c
}

fn sketch_combo(ui: &mut egui::Ui, cur: &mut String, sketches: &[(String, String)]) -> bool {
    let mut c = false;
    let name = sketches.iter().find(|s| &s.0 == cur).map(|s| s.1.clone()).unwrap_or_else(|| cur.clone());
    ui.horizontal(|ui| {
        ui.label("Sketch");
        egui::ComboBox::from_id_salt("sk").selected_text(name).show_ui(ui, |ui| {
            for (id, nm) in sketches {
                c |= ui.selectable_value(cur, id.clone(), nm).changed();
            }
        });
    });
    c
}

fn plane_combo(ui: &mut egui::Ui, plane: &mut PlaneRef, refs: &[(String, String)]) -> bool {
    let label = match plane {
        PlaneRef::Feature(id) => refs.iter().find(|r| &r.0 == id).map(|r| r.1.clone()).unwrap_or_else(|| id.clone()),
        PlaneRef::Named(StdPlane::Front) => "Front".to_string(),
        PlaneRef::Named(StdPlane::Top) => "Top".to_string(),
        PlaneRef::Named(StdPlane::Right) => "Right".to_string(),
        PlaneRef::Offset { base, offset } => format!("{base:?} + {offset}"),
        PlaneRef::Custom { normal, .. } => format!("Face plane n=({:.2},{:.2},{:.2})", normal[0], normal[1], normal[2]),
    };
    let mut c = false;
    egui::ComboBox::from_id_salt("plane").selected_text(label).show_ui(ui, |ui| {
        for (l, p) in [("Front", StdPlane::Front), ("Top", StdPlane::Top), ("Right", StdPlane::Right)] {
            if ui.selectable_label(false, l).clicked() {
                *plane = PlaneRef::Named(p);
                c = true;
            }
        }
        for (id, name) in refs {
            if ui.selectable_label(false, name).clicked() {
                *plane = PlaneRef::Feature(id.clone());
                c = true;
            }
        }
    });
    if let PlaneRef::Offset { offset, .. } = plane {
        ui.horizontal(|ui| {
            ui.label("Offset");
            c |= value_edit(ui, egui::Id::new("plane-offset"), offset, 80.0);
        });
    } else if let PlaneRef::Named(base) = plane {
        if ui.small_button("Make offset plane").clicked() {
            *plane = PlaneRef::Offset { base: *base, offset: 10.0.into() };
            c = true;
        }
    }
    c
}

fn constraint_label(c: &mut Constraint) -> (String, Option<&mut Value>) {
    match c {
        Constraint::Distance { a, b, value } => (format!("dist {a}{}", b.as_ref().map(|b| format!("–{b}")).unwrap_or_default()), Some(value)),
        Constraint::HorizontalDistance { a, b, value } => (format!("h-dist {a}–{b}"), Some(value)),
        Constraint::VerticalDistance { a, b, value } => (format!("v-dist {a}–{b}"), Some(value)),
        Constraint::Radius { entity, value } => (format!("R {entity}"), Some(value)),
        Constraint::Diameter { entity, value } => (format!("Ø {entity}"), Some(value)),
        Constraint::Angle { a, b, value } => (format!("angle {a}–{b}"), Some(value)),
        Constraint::Fix { point, .. } => (format!("fix {point}"), None),
        other => {
            let v = serde_json::to_value(&*other).unwrap_or_default();
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("?").to_string();
            let args: Vec<String> = v
                .as_object()
                .map(|o| o.iter().filter(|(k, _)| *k != "type").filter_map(|(_, v)| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            (format!("{ty} {}", args.join(", ")), None)
        }
    }
}

fn solve_text(s: SolveStatus, dof: usize) -> String {
    match s {
        SolveStatus::FullyDefined => "Fully defined".into(),
        SolveStatus::UnderDefined => format!("Under defined ({dof} DOF)"),
        SolveStatus::OverDefined => "Over defined".into(),
    }
}

fn fmt_num(x: f64) -> String {
    let s = format!("{:.4}", x);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn seg_dist2(p: glam::Vec2, a: glam::Vec2, b: glam::Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[allow(dead_code)]
fn _assert_send(_: Arc<Mesh>) {}
