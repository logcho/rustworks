//! Drawing mode: shows a `.rwdrw` sheet (rebuilt in the background whenever
//! the drawing or its source model changes), with view/sheet controls and
//! PDF/SVG/DXF/PNG export.

use egui::{Color32, Pos2, RichText, Stroke, Vec2};
use rw_drawing::{Anchor, Drawing, Pen, Projection, Sheet, SheetSize, ViewDef, ViewKind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Instant, SystemTime};

const SHEETS: [(SheetSize, &str); 7] = [
    (SheetSize::A4, "A4"),
    (SheetSize::A3, "A3"),
    (SheetSize::A2, "A2"),
    (SheetSize::A1, "A1"),
    (SheetSize::A0, "A0"),
    (SheetSize::Letter, "Letter"),
    (SheetSize::Tabloid, "Tabloid"),
];
const SCALES: [f64; 13] = [10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.4, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01];
const KINDS: [ViewKind; 7] = [ViewKind::Front, ViewKind::Top, ViewKind::Right, ViewKind::Left, ViewKind::Back, ViewKind::Bottom, ViewKind::Iso];

pub struct DrawingUi {
    pub path: PathBuf,
    pub doc: Drawing,
    sheet: Option<Sheet>,
    error: Option<String>,
    building: Option<Receiver<Result<Sheet, String>>>,
    rebuild: bool,
    stamps: Vec<Option<SystemTime>>,
    last_poll: Instant,
    /// Screen pixels per sheet millimetre, and pan offset of the sheet origin.
    zoom: f32,
    pan: Vec2,
    fit: bool,
    sel_view: Option<usize>,
    pub notes: Vec<(String, bool)>,
}

impl DrawingUi {
    pub fn open(path: PathBuf) -> anyhow::Result<DrawingUi> {
        let doc = rw_drawing::read_drawing(&path)?;
        let mut d = DrawingUi {
            path,
            doc,
            sheet: None,
            error: None,
            building: None,
            rebuild: true,
            stamps: vec![],
            last_poll: Instant::now(),
            zoom: 2.0,
            pan: Vec2::ZERO,
            fit: true,
            sel_view: None,
            notes: vec![],
        };
        d.stamps = d.watch_stamps();
        Ok(d)
    }

    /// Creates a drawing of `source` (a saved part or assembly) at `path`.
    pub fn create(path: PathBuf, source: &Path) -> anyhow::Result<DrawingUi> {
        let base = path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        let rel = rw_core::assembly::relative_part_path(&base, source);
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("DRAWING").to_uppercase();
        rw_drawing::write_drawing(&path, &Drawing::new(&name, &rel))?;
        DrawingUi::open(path)
    }

    fn source(&self) -> PathBuf {
        rw_drawing::source_path(Some(&self.path), &self.doc)
    }

    /// Files whose changes require a rebuild: the drawing, its source and,
    /// for assemblies, every referenced part.
    fn watch_stamps(&self) -> Vec<Option<SystemTime>> {
        let stamp = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let src = self.source();
        let mut v = vec![stamp(&self.path), stamp(&src)];
        if src.extension().is_some_and(|e| e == "rwasm") {
            if let Ok(a) = rw_core::assembly::read_assembly(&src) {
                let base = src.parent().map(Path::to_path_buf).unwrap_or_default();
                for c in &a.components {
                    v.push(stamp(&rw_core::assembly::part_path(&base, &c.path)));
                }
            }
        }
        v
    }

    fn save(&mut self) {
        match rw_drawing::write_drawing(&self.path, &self.doc) {
            Ok(()) => {
                self.stamps = self.watch_stamps();
                self.rebuild = true;
            }
            Err(e) => self.notes.push((format!("Save failed: {e:#}"), true)),
        }
    }

    /// Live sync + background rebuilds. Call every frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed().as_millis() > 500 {
            self.last_poll = Instant::now();
            let st = self.watch_stamps();
            if st != self.stamps {
                self.stamps = st;
                if let Ok(d) = rw_drawing::read_drawing(&self.path) {
                    self.doc = d;
                }
                self.rebuild = true;
            }
        }
        if let Some(rx) = &self.building {
            if let Ok(r) = rx.try_recv() {
                self.building = None;
                match r {
                    Ok(s) => {
                        for w in &s.warnings {
                            self.notes.push((w.clone(), true));
                        }
                        self.sheet = Some(s);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        if self.rebuild && self.building.is_none() {
            self.rebuild = false;
            let (tx, rx) = channel();
            let (doc, path, ctx) = (self.doc.clone(), self.path.clone(), ctx.clone());
            std::thread::spawn(move || {
                let r = rw_drawing::build(&doc, Some(&path)).map_err(|e| format!("{e:#}"));
                let _ = tx.send(r);
                ctx.request_repaint();
            });
            self.building = Some(rx);
        }
    }

    fn export(&mut self, ext: &str) {
        let Some(sheet) = &self.sheet else { return };
        let name = format!("{}.{ext}", self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("drawing"));
        if let Some(p) = rfd::FileDialog::new().set_file_name(name).add_filter(ext, &[ext]).save_file() {
            match rw_drawing::export(sheet, &p) {
                Ok(()) => self.notes.push((format!("Exported {}", p.display()), false)),
                Err(e) => self.notes.push((format!("Export failed: {e:#}"), true)),
            }
        }
    }

    pub fn command_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("Export:").strong());
            for ext in ["pdf", "svg", "dxf", "png"] {
                if ui.add_enabled(self.sheet.is_some(), egui::Button::new(ext.to_uppercase())).clicked() {
                    self.export(ext);
                }
            }
            ui.separator();
            let mut changed = false;
            ui.label("Sheet");
            egui::ComboBox::from_id_salt("sheet").selected_text(SHEETS.iter().find(|s| s.0 == self.doc.sheet).map_or("", |s| s.1)).show_ui(ui, |ui| {
                for (s, l) in SHEETS {
                    changed |= ui.selectable_value(&mut self.doc.sheet, s, l).changed();
                }
            });
            ui.label("Scale");
            let cur = self.doc.scale.map_or("Auto".to_string(), rw_drawing::scale_text);
            egui::ComboBox::from_id_salt("scale").selected_text(cur).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.doc.scale, None, "Auto").changed();
                for s in SCALES {
                    changed |= ui.selectable_value(&mut self.doc.scale, Some(s), rw_drawing::scale_text(s)).changed();
                }
            });
            egui::ComboBox::from_id_salt("proj")
                .selected_text(if self.doc.projection == Projection::ThirdAngle { "Third angle" } else { "First angle" })
                .show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut self.doc.projection, Projection::ThirdAngle, "Third angle (ANSI)").changed();
                    changed |= ui.selectable_value(&mut self.doc.projection, Projection::FirstAngle, "First angle (ISO)").changed();
                });
            changed |= ui.checkbox(&mut self.doc.dimensions, "Dimensions").changed();
            let mut hidden = self.doc.views.iter().filter(|v| v.kind != ViewKind::Iso).all(|v| v.hidden_lines.unwrap_or(true));
            if ui.checkbox(&mut hidden, "Hidden lines").changed() {
                for v in self.doc.views.iter_mut().filter(|v| v.kind != ViewKind::Iso) {
                    v.hidden_lines = Some(hidden);
                }
                changed = true;
            }
            ui.separator();
            if ui.button("Fit").clicked() {
                self.fit = true;
            }
            if changed {
                self.save();
            }
        });
    }

    pub fn tree(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Drawing:").weak());
            ui.label(RichText::new(&self.doc.name).strong().size(16.0));
        });
        ui.label(RichText::new(format!("of {}", self.doc.source)).weak().small());
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(200, 30, 30), e);
        }
        ui.separator();
        ui.label(RichText::new("Views").strong());
        let mut changed = false;
        for k in KINDS {
            let pos = self.doc.views.iter().position(|v| v.kind == k);
            let mut on = pos.is_some();
            ui.horizontal(|ui| {
                if ui.checkbox(&mut on, "").changed() {
                    if on {
                        self.doc.views.push(ViewDef::new(k));
                    } else if let Some(i) = pos {
                        self.doc.views.remove(i);
                        self.sel_view = None;
                    }
                    changed = true;
                }
                let sel = pos.is_some() && self.sel_view == pos;
                if ui.selectable_label(sel, k.label()).clicked() {
                    self.sel_view = if sel { None } else { pos };
                }
            });
        }
        if changed {
            self.save();
        }
    }

    pub fn property_manager(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let mut changed = false;
        if let Some(i) = self.sel_view.filter(|i| *i < self.doc.views.len()) {
            let v = &mut self.doc.views[i];
            ui.label(RichText::new(format!("{} view", v.kind.label())).strong().size(15.0));
            ui.separator();
            egui::Grid::new("view").num_columns(2).show(ui, |ui| {
                ui.label("Hidden lines");
                let mut h = v.hidden_lines.unwrap_or(v.kind != ViewKind::Iso);
                if ui.checkbox(&mut h, "").changed() {
                    v.hidden_lines = Some(h);
                    changed = true;
                }
                ui.end_row();
                ui.label("Scale factor").on_hover_text("Relative to the sheet scale (1 = same)");
                let mut f = v.scale.unwrap_or(1.0);
                if ui.add(egui::DragValue::new(&mut f).speed(0.01).range(0.01..=10.0)).changed() {
                    v.scale = if (f - 1.0).abs() < 1e-9 { None } else { Some(f) };
                    changed = true;
                }
                ui.end_row();
                ui.label("Shift (mm)");
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(&mut v.offset[0]).speed(0.5).prefix("x ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut v.offset[1]).speed(0.5).prefix("y ")).changed();
                });
                ui.end_row();
            });
            ui.separator();
        }
        ui.label(RichText::new("Title block").strong());
        let t = &mut self.doc.title;
        egui::Grid::new("title").num_columns(2).show(ui, |ui| {
            for (label, slot) in [("Title", &mut t.title), ("Drawn by", &mut t.drawn_by), ("Material", &mut t.material)] {
                ui.label(label);
                let mut s = slot.clone().unwrap_or_default();
                let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(150.0));
                if r.lost_focus() && s != slot.clone().unwrap_or_default() {
                    *slot = if s.trim().is_empty() { None } else { Some(s) };
                    changed = true;
                }
                ui.end_row();
            }
            ui.label("Drawing no.");
            let mut s = self.doc.name.clone();
            if ui.add(egui::TextEdit::singleline(&mut s).desired_width(150.0)).lost_focus() && s != self.doc.name && !s.trim().is_empty() {
                self.doc.name = s;
                changed = true;
            }
            ui.end_row();
        });
        ui.label("Notes");
        let mut n = self.doc.title.notes.clone().unwrap_or_default();
        if ui.add(egui::TextEdit::multiline(&mut n).desired_rows(3).desired_width(240.0)).lost_focus() && n != self.doc.title.notes.clone().unwrap_or_default() {
            self.doc.title.notes = if n.trim().is_empty() { None } else { Some(n) };
            changed = true;
        }
        if let Some(s) = &self.sheet {
            ui.separator();
            ui.label(format!("Scale {} · {:.0} × {:.0} mm", rw_drawing::scale_text(s.scale), s.width, s.height));
        }
        if changed {
            self.save();
        }
    }

    pub fn status_bar(&self, ui: &mut egui::Ui) {
        if self.building.is_some() {
            ui.colored_label(Color32::from_rgb(0, 102, 204), "Building views…");
        } else if self.error.is_some() {
            ui.colored_label(Color32::RED, "Drawing error");
        } else {
            ui.colored_label(Color32::from_rgb(30, 140, 60), "Up to date");
        }
        ui.label(RichText::new(format!("live-sync: {}", self.path.display())).weak());
    }

    /// The sheet canvas: drag to pan, scroll to zoom, click a view to select it.
    pub fn canvas(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_rgb(120, 128, 140));
        let Some(sheet) = &self.sheet else {
            let msg = self.error.clone().unwrap_or_else(|| "Building views…".into());
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), Color32::WHITE);
            return;
        };
        if self.fit {
            self.zoom = ((rect.width() - 40.0) / sheet.width as f32).min((rect.height() - 40.0) / sheet.height as f32).max(0.1);
            self.pan = Vec2::ZERO;
            self.fit = false;
        }
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let origin = rect.center() + self.pan - egui::vec2(sheet.width as f32, -(sheet.height as f32)) * self.zoom / 2.0;
        if resp.hovered() && scroll.abs() > 0.0 {
            // zoom about the cursor
            let f = (scroll * 0.0025).exp();
            if let Some(h) = ui.input(|i| i.pointer.hover_pos()) {
                let before = h - origin;
                self.pan += before - before * f;
            }
            self.zoom = (self.zoom * f).clamp(0.05, 60.0);
        }
        let z = self.zoom;
        let origin = rect.center() + self.pan - egui::vec2(sheet.width as f32, -(sheet.height as f32)) * z / 2.0;
        let to = |p: [f64; 2]| Pos2::new(origin.x + p[0] as f32 * z, origin.y - p[1] as f32 * z);
        let paper = egui::Rect::from_min_max(to([0.0, sheet.height]), to([sheet.width, 0.0]));
        painter.rect_filled(paper.translate(egui::vec2(4.0, 4.0)), 0.0, Color32::from_black_alpha(60));
        painter.rect_filled(paper, 0.0, Color32::WHITE);
        // selected view highlight
        if let Some(v) = self.sel_view.and_then(|i| self.doc.views.get(i)).and_then(|d| sheet.views.iter().find(|v| v.kind == d.kind)) {
            let r = egui::Rect::from_min_max(to([v.min[0], v.max[1]]), to([v.max[0], v.min[1]])).expand(4.0);
            painter.rect_filled(r, 2.0, Color32::from_rgba_unmultiplied(0, 110, 255, 18));
            painter.rect_stroke(r, 2.0, Stroke::new(1.0, Color32::from_rgb(0, 110, 255)), egui::StrokeKind::Outside);
        }
        for l in &sheet.lines {
            let (w, dash) = match l.pen {
                Pen::Visible => (0.5, None),
                Pen::Hidden => (0.3, Some((3.0, 1.5))),
                Pen::Thin => (0.18, None),
                Pen::Border => (0.7, None),
            };
            let st = Stroke::new((w * z).max(0.6), Color32::BLACK);
            let pts: Vec<Pos2> = l.pts.iter().map(|p| to(*p)).collect();
            match dash {
                Some((on, off)) => {
                    painter.extend(egui::Shape::dashed_line(&pts, st, on * z, off * z));
                }
                None => {
                    painter.add(egui::Shape::line(pts, st));
                }
            }
        }
        for t in &sheet.texts {
            let em = (t.size / 0.7) as f32 * z;
            if em < 2.0 {
                continue;
            }
            let galley = painter.layout_no_wrap(t.text.clone(), egui::FontId::proportional(em), Color32::BLACK);
            let w = galley.size().x;
            let dx = match t.anchor {
                Anchor::Start => 0.0,
                Anchor::Middle => -w / 2.0,
                Anchor::End => -w,
            };
            let ang = -(t.rotate as f32).to_radians();
            let base = to(t.at);
            // galley origin is its top-left; move from baseline up by ~0.8 em
            let off = egui::vec2(dx, -em * 0.8);
            let (s, c) = ang.sin_cos();
            let rot = egui::vec2(off.x * c - off.y * s, off.x * s + off.y * c);
            painter.add(egui::epaint::TextShape::new(base + rot, galley, Color32::BLACK).with_angle(ang));
        }
        if resp.clicked() {
            if let Some(h) = resp.interact_pointer_pos() {
                let p = [((h.x - origin.x) / z) as f64, ((origin.y - h.y) / z) as f64];
                let hit = sheet.views.iter().find(|v| p[0] >= v.min[0] && p[0] <= v.max[0] && p[1] >= v.min[1] && p[1] <= v.max[1]);
                self.sel_view = hit.and_then(|v| self.doc.views.iter().position(|d| d.kind == v.kind));
            }
        }
        if resp.double_clicked() {
            self.fit = true;
        }
        if self.building.is_some() {
            painter.text(rect.right_top() + egui::vec2(-12.0, 14.0), egui::Align2::RIGHT_CENTER, "Updating…", egui::FontId::proportional(14.0), Color32::WHITE);
        }
    }
}
