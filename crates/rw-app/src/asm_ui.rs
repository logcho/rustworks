//! Assembly mode: component tree, mate creation by picking faces, component
//! and mate PropertyManager, interference check. The assembly document lives
//! in an `AsmModel` (undo/redo, autosave, live sync with the MCP server).

use crate::viewport::Viewport;
use egui::{Color32, RichText};
use glam::Vec3;
use rw_core::assembly::{AsmModel, Mate, MateKind, MateRef};
use rw_core::sketch::Value;
use rw_core::{Assembly, Component};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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

const KINDS: [(MateKind, &str); 6] = [
    (MateKind::Coincident, "Coincident"),
    (MateKind::Concentric, "Concentric"),
    (MateKind::Distance, "Distance"),
    (MateKind::Parallel, "Parallel"),
    (MateKind::Perpendicular, "Perpendicular"),
    (MateKind::Angle, "Angle"),
];

/// A face picked in the viewport.
#[derive(Clone, Debug)]
pub struct Pick {
    pub component: String,
    pub name: String,
    /// Point on the face in the component's part coordinates.
    pub local: [f64; 3],
    pub world: Vec3,
}

pub struct AsmUi {
    pub model: AsmModel,
    pub sel_comp: Option<String>,
    pub sel_mate: Option<String>,
    pub picks: Vec<Pick>,
    pub mate_kind: MateKind,
    pub mate_value: f64,
    pub aligned: bool,
    /// (component index, face offset) for each component in the merged mesh.
    owners: Vec<(usize, u32)>,
    pub mesh_dirty: bool,
    pub fit_pending: bool,
    pub interference: Option<Vec<(String, String, f64)>>,
    pub notes: Vec<(String, bool)>,
    /// Part file the user asked to open for editing.
    pub open_part: Option<PathBuf>,
    last_poll: Instant,
    /// Component being dragged: (id, grabbed point in part coords, drag plane point, plane normal).
    pub dragging: Option<(String, [f64; 3], Vec3, Vec3)>,
}

impl AsmUi {
    pub fn new(model: AsmModel) -> AsmUi {
        AsmUi {
            model,
            sel_comp: None,
            sel_mate: None,
            picks: vec![],
            mate_kind: MateKind::Coincident,
            mate_value: 10.0,
            aligned: false,
            owners: vec![],
            mesh_dirty: true,
            fit_pending: true,
            interference: None,
            notes: vec![],
            open_part: None,
            last_poll: Instant::now(),
            dragging: None,
        }
    }

    pub fn create(path: PathBuf) -> anyhow::Result<AsmUi> {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Assembly1").to_string();
        let mut m = AsmModel::new(Assembly::new(&name), Some(path));
        m.save()?;
        Ok(AsmUi::new(m))
    }

    fn note(&mut self, m: impl Into<String>, err: bool) {
        self.notes.push((m.into(), err));
    }

    fn edit(&mut self, f: impl FnOnce(&mut Assembly) -> anyhow::Result<()>) {
        match self.model.edit(f) {
            Ok(()) => {
                self.mesh_dirty = true;
                self.interference = None;
            }
            Err(e) => self.note(format!("{e:#}"), true),
        }
    }

    /// Live sync: reload when the assembly or one of its parts changed on disk.
    pub fn poll(&mut self) {
        if self.last_poll.elapsed().as_millis() < 400 {
            return;
        }
        self.last_poll = Instant::now();
        match self.model.sync_from_disk() {
            Ok(true) => {
                self.mesh_dirty = true;
                self.interference = None;
                self.note("Reloaded external changes (live sync)", false);
            }
            Ok(false) => {}
            Err(e) => self.note(format!("Reload failed: {e:#}"), true),
        }
    }

    /// Uploads the merged, per-component colored mesh to the viewport.
    pub fn sync_viewport(&mut self, vp: &mut Viewport, dev: &egui_wgpu::wgpu::Device) {
        if !self.mesh_dirty {
            return;
        }
        let (mesh, owners) = self.model.out.merged_mesh();
        let mut colors = Vec::with_capacity(mesh.tri_face.len());
        for (k, (ci, _)) in owners.iter().enumerate() {
            let c = &self.model.out.components[*ci];
            let n = c.mesh.as_ref().map_or(0, |m| m.tri_face.len());
            let mut col = PALETTE[k % PALETTE.len()];
            if self.sel_comp.as_deref() == Some(c.id.as_str()) {
                col = [col[0] * 0.75 + 0.05, col[1] * 0.75 + 0.2, col[2] * 0.75 + 0.25];
            }
            colors.extend(std::iter::repeat_n(col, n));
        }
        self.owners = owners;
        let empty = mesh.positions.is_empty();
        vp.set_mesh_colored(dev, if empty { None } else { Some(Arc::new(mesh)) }, &colors);
        vp.selected_face = None;
        if self.fit_pending && !empty {
            vp.fit(None, false);
            self.fit_pending = false;
        }
        self.mesh_dirty = false;
    }

    /// Component under a merged-mesh face, and the world point in its part coordinates.
    fn locate(&self, face: u32, world: Vec3) -> Option<(usize, [f64; 3])> {
        let &(ci, _) = self.owners.iter().rev().find(|(_, off)| *off <= face)?;
        let p = self.model.out.components[ci].placement;
        let w = [world.x as f64, world.y as f64, world.z as f64];
        // inverse rigid transform: R^T (w - t)
        let d = [w[0] - p.t[0], w[1] - p.t[1], w[2] - p.t[2]];
        Some((ci, [0, 1, 2].map(|j| ((p.r[0][j] * d[0] + p.r[1][j] * d[1] + p.r[2][j] * d[2]) * 1e4).round() / 1e4)))
    }

    /// Starts dragging the component under the cursor. Returns false for
    /// grounded components (the view orbits instead).
    pub fn begin_drag(&mut self, face: u32, world: Vec3, eye_dir: Vec3) -> bool {
        let Some((ci, local)) = self.locate(face, world) else { return false };
        let c = &self.model.out.components[ci];
        if c.grounded {
            return false;
        }
        self.sel_comp = Some(c.id.clone());
        self.sel_mate = None;
        self.dragging = Some((c.id.clone(), local, world, eye_dir));
        self.mesh_dirty = true;
        true
    }

    /// Moves the dragged component toward the cursor ray (on a plane facing the camera).
    pub fn drag_to(&mut self, ray_o: Vec3, ray_d: Vec3) {
        let Some((id, local, p0, n)) = self.dragging.clone() else { return };
        let den = ray_d.dot(n);
        if den.abs() < 1e-8 {
            return;
        }
        let t = (p0 - ray_o).dot(n) / den;
        let target = ray_o + ray_d * t;
        if self.model.drag_preview(&id, local, [target.x as f64, target.y as f64, target.z as f64]) {
            self.mesh_dirty = true;
        }
    }

    pub fn end_drag(&mut self) {
        if self.dragging.take().is_some() {
            if let Err(e) = self.model.drag_commit() {
                self.note(format!("{e:#}"), true);
            }
            self.mesh_dirty = true;
            self.interference = None;
        }
    }

    /// Maps a merged-mesh face index + world point to a component pick.
    pub fn pick(&mut self, face: u32, world: Vec3, add: bool) {
        let Some((ci, local)) = self.locate(face, world) else { return };
        let c = &self.model.out.components[ci];
        let pick = Pick { component: c.id.clone(), name: c.name.clone(), local, world };
        if !add {
            self.picks.clear();
        }
        if self.picks.len() >= 2 {
            self.picks.remove(0);
        }
        self.sel_comp = Some(c.id.clone());
        self.sel_mate = None;
        self.picks.push(pick);
        self.mesh_dirty = true;
    }

    fn add_mate(&mut self) {
        if self.picks.len() != 2 {
            return;
        }
        let r = |p: &Pick| MateRef { component: p.component.clone(), face: Some(p.local), plane: None, axis: None, point: None };
        let value = matches!(self.mate_kind, MateKind::Distance | MateKind::Angle).then_some(Value::Num(self.mate_value));
        let m = Mate {
            id: String::new(),
            name: String::new(),
            kind: self.mate_kind,
            a: r(&self.picks[0]),
            b: r(&self.picks[1]),
            value,
            aligned: self.aligned,
            suppressed: false,
        };
        match self.model.add_mate(m) {
            Ok(id) => {
                self.picks.clear();
                self.sel_mate = Some(id);
                self.mesh_dirty = true;
                self.interference = None;
                let st = self.model.out.mates.iter().find(|s| Some(&s.id) == self.sel_mate.as_ref()).cloned();
                if let Some(e) = st.and_then(|s| s.error) {
                    self.note(format!("Mate added but not satisfied: {e}"), true);
                }
            }
            Err(e) => self.note(format!("Mate failed: {e:#}"), true),
        }
    }

    pub fn insert_component(&mut self, part: PathBuf) {
        let base = self.model.base();
        let rel = rw_core::assembly::relative_part_path(&base, &part);
        let stem = part.file_stem().and_then(|s| s.to_str()).unwrap_or("part").to_string();
        // drop new parts beside the existing ones
        let off = self
            .model
            .out
            .components
            .iter()
            .filter_map(|c| c.mesh.as_ref().map(|m| (c, m.mass_props())))
            .map(|(c, mp)| c.placement.point(mp.bbox_max)[0])
            .fold(f64::NEG_INFINITY, f64::max);
        let x = if off.is_finite() { off + 20.0 } else { 0.0 };
        let mut new_id = String::new();
        self.edit(|a| {
            let (id, name) = a.fresh_component(&stem);
            new_id = id.clone();
            a.components.push(Component { id, name, path: rel, position: [x, 0.0, 0.0], rotation: [0.0; 3], fixed: false, suppressed: false });
            Ok(())
        });
        self.sel_comp = Some(new_id);
        self.fit_pending = true;
    }

    pub fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.button(RichText::new("Insert Component").strong()).on_hover_text("Add a part file to the assembly").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("RustWorks part", &["rwpart"]).pick_file() {
                    self.insert_component(p);
                }
            }
            ui.separator();
            ui.label(RichText::new("Mate:").strong());
            for (k, l) in KINDS {
                ui.selectable_value(&mut self.mate_kind, k, l);
            }
            if matches!(self.mate_kind, MateKind::Distance | MateKind::Angle) {
                let unit = if self.mate_kind == MateKind::Angle { "°" } else { " mm" };
                ui.add(egui::DragValue::new(&mut self.mate_value).speed(0.5).suffix(unit));
            }
            if matches!(self.mate_kind, MateKind::Coincident | MateKind::Distance | MateKind::Parallel) {
                ui.checkbox(&mut self.aligned, "Aligned").on_hover_text("Planar faces point the same way instead of face-to-face");
            }
            let ok = self.picks.len() == 2;
            if ui
                .add_enabled(ok, egui::Button::new(RichText::new("Add Mate").strong()))
                .on_hover_text("Click a face, Shift-click a face on another component, then Add Mate")
                .on_disabled_hover_text("Pick two faces: click one, Shift-click another")
                .clicked()
            {
                self.add_mate();
            }
            ui.separator();
            if ui.button("Check Interference").clicked() {
                match self.model.out.interferences() {
                    Ok(v) => {
                        let msg = if v.is_empty() { "No interference".to_string() } else { format!("{} interference(s) found", v.len()) };
                        self.note(msg, !v.is_empty());
                        self.interference = Some(v);
                    }
                    Err(e) => self.note(format!("{e:#}"), true),
                }
            }
            ui.separator();
            if ui.add_enabled(self.model.can_undo(), egui::Button::new("Undo")).clicked() {
                self.undo();
            }
            if ui.add_enabled(self.model.can_redo(), egui::Button::new("Redo")).clicked() {
                self.redo();
            }
        });
    }

    pub fn undo(&mut self) {
        if self.model.undo() {
            self.mesh_dirty = true;
            self.interference = None;
        }
    }

    pub fn redo(&mut self) {
        if self.model.redo() {
            self.mesh_dirty = true;
            self.interference = None;
        }
    }

    pub fn tree(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Assembly:").weak());
            ui.label(RichText::new(&self.model.asm.name).strong().size(16.0));
        });
        let s = &self.model.out.solve;
        let (c, t) = match s.status.as_str() {
            "fully_defined" => (Color32::BLACK, "Fully defined".to_string()),
            "under_defined" => (Color32::from_rgb(0, 102, 204), format!("Under defined ({} DOF)", s.dof)),
            "over_defined" => (Color32::RED, "Over defined / conflicting mates".to_string()),
            _ => (Color32::GRAY, "Empty".to_string()),
        };
        ui.colored_label(c, t);
        ui.separator();
        ui.label(RichText::new("Components").strong());
        let comps = self.model.out.components.clone();
        let mut action: Option<(String, &str)> = None;
        egui::ScrollArea::vertical().id_salt("asmtree").max_height(ui.available_height() * 0.55).show(ui, |ui| {
            for (k, c) in comps.iter().enumerate() {
                let col = PALETTE[k % PALETTE.len()];
                let tag = if c.grounded { "(f)" } else { "(-)" };
                let mut text = RichText::new(format!("{tag} {}", c.name));
                if c.suppressed {
                    text = text.color(Color32::GRAY).italics();
                } else if c.error.is_some() {
                    text = text.color(Color32::from_rgb(200, 30, 30));
                }
                let sel = self.sel_comp.as_deref() == Some(c.id.as_str());
                ui.horizontal(|ui| {
                    let (r, p) = ui.allocate_painter(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    p.rect_filled(r.rect, 2.0, Color32::from_rgb((col[0] * 255.0) as u8, (col[1] * 255.0) as u8, (col[2] * 255.0) as u8));
                    let mut tip = format!("{} · {}\n(f) fixed/grounded, (-) floating", c.id, c.path.display());
                    if let Some(e) = &c.error {
                        tip += &format!("\nError: {e}");
                    }
                    let r = ui.selectable_label(sel, text).on_hover_text(tip);
                    if r.clicked() {
                        self.sel_comp = if sel { None } else { Some(c.id.clone()) };
                        self.sel_mate = None;
                        self.mesh_dirty = true;
                    }
                    if r.double_clicked() {
                        self.open_part = Some(c.path.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button("Open Part").clicked() {
                            action = Some((c.id.clone(), "open"));
                            ui.close();
                        }
                        if ui.button(if c.grounded { "Float" } else { "Fix" }).clicked() {
                            action = Some((c.id.clone(), "fix"));
                            ui.close();
                        }
                        if ui.button(if c.suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                            action = Some((c.id.clone(), "suppress"));
                            ui.close();
                        }
                        if ui.button(RichText::new("Delete (and its mates)").color(Color32::RED)).clicked() {
                            action = Some((c.id.clone(), "delete"));
                            ui.close();
                        }
                    });
                });
            }
            if comps.is_empty() {
                ui.label(RichText::new("Insert Component to add a part.").weak().italics());
            }
        });
        ui.separator();
        ui.label(RichText::new("Mates").strong());
        let mates = self.model.out.mates.clone();
        egui::ScrollArea::vertical().id_salt("matetree").show(ui, |ui| {
            for m in &mates {
                let doc = self.model.asm.mates.iter().find(|x| x.id == m.id);
                let between = doc.map(|d| format!(" ({}, {})", short(&self.model.asm, &d.a.component), short(&self.model.asm, &d.b.component))).unwrap_or_default();
                let mut text = RichText::new(format!("{}{between}", m.name));
                if doc.is_some_and(|d| d.suppressed) {
                    text = text.color(Color32::GRAY).italics();
                } else if !m.ok {
                    text = text.color(Color32::from_rgb(200, 30, 30));
                }
                let sel = self.sel_mate.as_deref() == Some(m.id.as_str());
                let mut r = ui.selectable_label(sel, text);
                if let Some(e) = &m.error {
                    r = r.on_hover_text(e);
                }
                if r.clicked() {
                    self.sel_mate = if sel { None } else { Some(m.id.clone()) };
                    self.sel_comp = None;
                    self.mesh_dirty = true;
                }
                r.context_menu(|ui| {
                    if ui.button(RichText::new("Delete mate").color(Color32::RED)).clicked() {
                        action = Some((m.id.clone(), "delete_mate"));
                        ui.close();
                    }
                });
            }
            if mates.is_empty() {
                ui.label(RichText::new("Pick two faces, choose a mate type, Add Mate.").weak().italics());
            }
        });
        if let Some((id, what)) = action {
            self.act(&id, what);
        }
    }

    fn act(&mut self, id: &str, what: &str) {
        match what {
            "open" => {
                if let Some(c) = self.model.out.components.iter().find(|c| c.id == id) {
                    self.open_part = Some(c.path.clone());
                }
            }
            "fix" => {
                let grounded = self.model.asm.is_grounded(id);
                self.edit(|a| {
                    if grounded {
                        // floating a component that is only implicitly grounded: ground nothing
                        for c in a.components.iter_mut() {
                            if c.id == id {
                                c.fixed = false;
                            }
                        }
                        if !a.components.iter().any(|c| c.fixed) {
                            if let Some(other) = a.components.iter_mut().find(|c| c.id != id && !c.suppressed) {
                                other.fixed = true;
                            }
                        }
                    } else if let Some(c) = a.component_mut(id) {
                        c.fixed = true;
                    }
                    Ok(())
                });
            }
            "suppress" => self.edit(|a| {
                if let Some(c) = a.component_mut(id) {
                    c.suppressed = !c.suppressed;
                }
                Ok(())
            }),
            "delete" => {
                self.edit(|a| {
                    let name = a.component(id).map(|c| c.name.clone()).unwrap_or_default();
                    let hit = |r: &MateRef| r.component == id || r.component == name;
                    a.mates.retain(|m| !hit(&m.a) && !hit(&m.b));
                    a.components.retain(|c| c.id != id);
                    Ok(())
                });
                self.sel_comp = None;
                self.picks.retain(|p| p.component != id);
            }
            "delete_mate" => {
                self.edit(|a| {
                    a.mates.retain(|m| m.id != id);
                    Ok(())
                });
                self.sel_mate = None;
            }
            _ => {}
        }
    }

    pub fn property_manager(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        if let Some(cid) = self.sel_comp.clone() {
            if let Some(c) = self.model.asm.component(&cid).cloned() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&c.name).strong().size(15.0));
                    ui.label(RichText::new("component").weak());
                });
                ui.label(RichText::new(&c.path).weak().small());
                if let Some(e) = self.model.out.components.iter().find(|x| x.id == cid).and_then(|x| x.error.clone()) {
                    ui.colored_label(Color32::from_rgb(200, 30, 30), format!("Error: {e}"));
                }
                ui.separator();
                let mut nc = c.clone();
                let mut changed = false;
                let grounded = self.model.asm.is_grounded(&cid);
                egui::Grid::new("comp").num_columns(2).show(ui, |ui| {
                    ui.label("Position");
                    changed |= crate::app::vec3_edit_speed(ui, &mut nc.position, 0.5);
                    ui.end_row();
                    ui.label("Rotation (°)").on_hover_text("Rotation vector: axis × angle in degrees");
                    changed |= crate::app::vec3_edit_speed(ui, &mut nc.rotation, 1.0);
                    ui.end_row();
                    ui.label("Fixed");
                    let hint = if grounded && !nc.fixed { "(grounded: first component)" } else { "" };
                    changed |= ui.checkbox(&mut nc.fixed, hint).changed();
                    ui.end_row();
                    ui.label("Suppressed");
                    changed |= ui.checkbox(&mut nc.suppressed, "").changed();
                    ui.end_row();
                });
                if !grounded {
                    ui.label(RichText::new("Mated components snap back to their mates after moving.").weak().small());
                }
                if ui.button("Open Part for editing").clicked() {
                    self.open_part = self.model.out.components.iter().find(|x| x.id == cid).map(|x| x.path.clone());
                }
                if changed {
                    self.edit(|a| {
                        if let Some(slot) = a.component_mut(&cid) {
                            *slot = nc;
                        }
                        Ok(())
                    });
                }
                return;
            }
        }
        if let Some(mid) = self.sel_mate.clone() {
            if let Some(m) = self.model.asm.mates.iter().find(|m| m.id == mid).cloned() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&m.name).strong().size(15.0));
                    ui.label(RichText::new(format!("{:?} mate", m.kind)).weak());
                });
                if let Some(e) = self.model.out.mates.iter().find(|s| s.id == mid).and_then(|s| s.error.clone()) {
                    ui.colored_label(Color32::from_rgb(200, 30, 30), format!("Error: {e}"));
                }
                ui.label(format!("{}  ↔  {}", short(&self.model.asm, &m.a.component), short(&self.model.asm, &m.b.component)));
                ui.separator();
                let mut nm = m.clone();
                let mut changed = false;
                egui::Grid::new("mate").num_columns(2).show(ui, |ui| {
                    if let Some(v) = &mut nm.value {
                        ui.label(if m.kind == MateKind::Angle { "Angle (°)" } else { "Distance" });
                        changed |= crate::app::value_edit(ui, egui::Id::new(("mv", &mid)), v, 100.0);
                        ui.end_row();
                    }
                    ui.label("Aligned");
                    changed |= ui.checkbox(&mut nm.aligned, "").changed();
                    ui.end_row();
                    ui.label("Suppressed");
                    changed |= ui.checkbox(&mut nm.suppressed, "").changed();
                    ui.end_row();
                });
                if changed {
                    self.edit(|a| {
                        if let Some(slot) = a.mates.iter_mut().find(|x| x.id == mid) {
                            *slot = nm;
                        }
                        Ok(())
                    });
                }
                if ui.button(RichText::new("Delete mate").color(Color32::RED)).clicked() {
                    self.act(&mid, "delete_mate");
                }
                return;
            }
        }
        ui.label(RichText::new("PropertyManager").strong());
        ui.label(RichText::new("Click a face, then Shift-click a face on another component; choose a mate type and press Add Mate. Drag a part to move it (mated parts slide along their free directions).").weak());
        if !self.picks.is_empty() {
            ui.separator();
            ui.label(RichText::new("Picked faces").strong());
            for (i, p) in self.picks.iter().enumerate() {
                ui.label(format!("{}. {} @ ({:.2}, {:.2}, {:.2})", i + 1, p.name, p.local[0], p.local[1], p.local[2]));
            }
            if ui.small_button("Clear").clicked() {
                self.picks.clear();
            }
        }
        ui.separator();
        let (v, c) = self.model.out.mass();
        ui.label(RichText::new("Mass properties").strong());
        ui.label(format!("Volume: {v:.2} mm³"));
        ui.label(format!("Mass (steel): {:.1} g", v / 1000.0 * 7.85));
        ui.label(format!("Center of mass: ({:.2}, {:.2}, {:.2})", c[0], c[1], c[2]));
        if let Some(list) = &self.interference {
            ui.separator();
            ui.label(RichText::new("Interference").strong());
            if list.is_empty() {
                ui.colored_label(Color32::from_rgb(30, 140, 60), "None: components are clear");
            }
            for (a, b, v) in list {
                ui.colored_label(Color32::from_rgb(200, 30, 30), format!("{a} ↔ {b}: {v:.3} mm³"));
            }
        }
    }

    pub fn status_bar(&self, ui: &mut egui::Ui) {
        let errs = self.model.out.errors();
        if errs.is_empty() {
            ui.colored_label(Color32::from_rgb(30, 140, 60), "Solved OK");
        } else {
            ui.colored_label(Color32::RED, format!("Error: {} problem(s)", errs.len())).on_hover_text(errs.join("\n"));
        }
        ui.label(format!("{:.0} ms", self.model.out.millis));
        ui.label(RichText::new(format!("{} components · {} mates · {} DOF", self.model.asm.components.len(), self.model.asm.mates.len(), self.model.out.solve.dof)).weak());
    }

    /// Markers for picked faces.
    pub fn paint_picks(&self, painter: &egui::Painter, rect: egui::Rect, vp: &Viewport) {
        for (i, p) in self.picks.iter().enumerate() {
            if let Some(s) = vp.cam.project(p.world, rect.width(), rect.height()) {
                let pos = rect.min + egui::vec2(s.x, s.y);
                painter.circle_filled(pos, 6.0, Color32::from_rgb(20, 110, 255));
                painter.text(pos + egui::vec2(9.0, -9.0), egui::Align2::LEFT_BOTTOM, format!("{}", i + 1), egui::FontId::proportional(13.0), Color32::from_rgb(20, 60, 160));
            }
        }
    }
}

fn short(a: &Assembly, id: &str) -> String {
    a.component(id).map(|c| c.name.clone()).unwrap_or_else(|| id.to_string())
}
