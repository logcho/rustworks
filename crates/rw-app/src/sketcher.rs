//! Interactive sketch mode: draw lines, rectangles, circles, arcs and points
//! with the mouse on the sketch plane, with point snapping, automatic
//! horizontal/vertical relations, point dragging and smart dimensions.

use crate::viewport::Camera;
use egui::{Color32, Pos2, Stroke};
use glam::Vec3;
use rw_core::kernel::Plane;
use rw_core::sketch::{Constraint, Entity, Sketch, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Line,
    Rectangle,
    Circle,
    Arc,
    Point,
    Dimension,
}

impl Tool {
    pub const ALL: [(Tool, &'static str, &'static str); 7] = [
        (Tool::Select, "Select", "Select / drag points (S)"),
        (Tool::Line, "Line", "Chain of lines; double-click, Esc or right-click ends (L)"),
        (Tool::Rectangle, "Rect", "Corner rectangle: two clicks (R)"),
        (Tool::Circle, "Circle", "Center, then radius point (C)"),
        (Tool::Arc, "Arc", "Center, start, end (counter-clockwise) (A)"),
        (Tool::Point, "Point", "Sketch point (P)"),
        (Tool::Dimension, "Dimension", "Click a line (length), circle/arc (diameter/radius) or two points (distance) (D)"),
    ];
}

pub struct Sketcher {
    pub feature: String,
    pub tool: Tool,
    clicks: Vec<[f64; 2]>,
    dim_first: Option<String>,
    drag: Option<String>,
    pub hover_point: Option<String>,
    pub message: Option<String>,
}

/// Snap radius in screen pixels.
const SNAP_PX: f32 = 9.0;

impl Sketcher {
    pub fn new(feature: String) -> Sketcher {
        Sketcher { feature, tool: Tool::Line, clicks: vec![], dim_first: None, drag: None, hover_point: None, message: None }
    }

    pub fn cancel(&mut self) {
        self.clicks.clear();
        self.dim_first = None;
        self.drag = None;
    }

    /// Handles input for one frame and paints overlays. Returns an updated
    /// sketch when the user committed an edit.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        rect: egui::Rect,
        cam: &Camera,
        plane: &Plane,
        sketch: &Sketch,
    ) -> Option<Sketch> {
        let painter = ui.painter_at(rect);
        let (w, h) = (rect.width(), rect.height());
        let to_screen = |p: [f64; 2]| -> Option<Pos2> {
            let q = plane.to3d(p);
            cam.project(Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32), w, h).map(|v| rect.min + egui::vec2(v.x, v.y))
        };
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
        let on_plane = hover.and_then(|p| {
            let l = p - rect.min;
            let (o, d) = cam.ray(l.x, l.y, w, h);
            let n = Vec3::from(plane.normal().map(|v| v as f32));
            let po = Vec3::from(plane.origin.map(|v| v as f32));
            let den = d.dot(n);
            if den.abs() < 1e-8 {
                return None;
            }
            let t = (po - o).dot(n) / den;
            let hit = o + d * t;
            Some(plane.to2d([hit.x as f64, hit.y as f64, hit.z as f64]))
        });

        // Points: nearest within snap radius.
        let pts: Vec<(String, [f64; 2])> = sketch
            .entities
            .iter()
            .filter_map(|e| if let Entity::Point { id, x, y, .. } = e { Some((id.clone(), [*x, *y])) } else { None })
            .collect();
        self.hover_point = None;
        if let Some(hp) = hover {
            let mut best = SNAP_PX;
            for (id, p) in &pts {
                if let Some(s) = to_screen(*p) {
                    let d = s.distance(hp);
                    if d < best {
                        best = d;
                        self.hover_point = Some(id.clone());
                    }
                }
            }
        }
        let snapped: Option<[f64; 2]> = match (&self.hover_point, on_plane) {
            (Some(id), _) => pts.iter().find(|p| &p.0 == id).map(|p| p.1),
            (None, Some(p)) => Some(round_nice(p, cam.dist)),
            _ => None,
        };

        // Draw points
        for (id, p) in &pts {
            if let Some(s) = to_screen(*p) {
                let hot = self.hover_point.as_deref() == Some(id.as_str());
                painter.circle_filled(s, if hot { 5.0 } else { 3.0 }, if hot { Color32::from_rgb(255, 140, 0) } else { Color32::from_rgb(20, 60, 160) });
            }
        }

        let mut out: Option<Sketch> = None;
        let clicked = resp.clicked_by(egui::PointerButton::Primary);
        let double = resp.double_clicked();
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.clicked_by(egui::PointerButton::Secondary);
        if esc {
            self.cancel();
        }

        // Tool shortcuts
        ui.input(|i| {
            for (k, t) in [
                (egui::Key::L, Tool::Line),
                (egui::Key::R, Tool::Rectangle),
                (egui::Key::C, Tool::Circle),
                (egui::Key::A, Tool::Arc),
                (egui::Key::P, Tool::Point),
                (egui::Key::D, Tool::Dimension),
                (egui::Key::S, Tool::Select),
            ] {
                if i.key_pressed(k) && !i.modifiers.any() {
                    self.tool = t;
                    self.clicks.clear();
                    self.dim_first = None;
                }
            }
        });
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if typing {
            // keyboard focus is in a text field; ignore stray shortcut changes
        }

        match self.tool {
            Tool::Select => {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    self.drag = self.hover_point.clone();
                }
                if let (Some(id), Some(p)) = (&self.drag, on_plane) {
                    if let Some(s) = to_screen(p) {
                        painter.circle_stroke(s, 6.0, Stroke::new(2.0, Color32::from_rgb(255, 140, 0)));
                    }
                    if resp.drag_stopped() {
                        let mut sk = sketch.clone();
                        if let Some(Entity::Point { x, y, .. }) = sk.entities.iter_mut().find(|e| e.id() == id) {
                            *x = p[0];
                            *y = p[1];
                        }
                        // Keep circle radius etc. — the solver re-satisfies all constraints.
                        out = Some(sk);
                        self.drag = None;
                    }
                }
            }
            Tool::Point => {
                if clicked {
                    if let Some(p) = snapped {
                        let mut sk = sketch.clone();
                        sk.point_at(p[0], p[1], 1e-9);
                        out = Some(sk);
                    }
                }
            }
            Tool::Line => {
                if let (Some(&last), Some(p)) = (self.clicks.last(), snapped) {
                    let (p, hv) = infer_hv(last, p, self.hover_point.is_some());
                    if let (Some(a), Some(b)) = (to_screen(last), to_screen(p)) {
                        painter.line_segment([a, b], Stroke::new(1.5, Color32::from_rgb(0, 110, 255)));
                        let tag = match hv {
                            Some(true) => "H",
                            Some(false) => "V",
                            None => "",
                        };
                        let len = ((p[0] - last[0]).powi(2) + (p[1] - last[1]).powi(2)).sqrt();
                        painter.text(b + egui::vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, format!("{:.2} {tag}", len), egui::FontId::proportional(12.0), Color32::DARK_BLUE);
                    }
                }
                if double && !self.clicks.is_empty() {
                    self.clicks.clear();
                } else if clicked {
                    if let Some(p) = snapped {
                        if let Some(&last) = self.clicks.last() {
                            let (p, hv) = infer_hv(last, p, self.hover_point.is_some());
                            if (p[0] - last[0]).hypot(p[1] - last[1]) > 1e-9 {
                                let mut sk = sketch.clone();
                                let l = sk.add_line(last, p, false);
                                match hv {
                                    Some(true) => sk.constraints.push(Constraint::Horizontal { line: l }),
                                    Some(false) => sk.constraints.push(Constraint::Vertical { line: l }),
                                    None => {}
                                }
                                out = Some(sk);
                                // Closing onto an existing point ends the chain.
                                if self.hover_point.is_some() && self.clicks.len() > 1 {
                                    self.clicks.clear();
                                } else {
                                    self.clicks.push(p);
                                }
                            }
                        } else {
                            self.clicks.push(p);
                        }
                    }
                }
            }
            Tool::Rectangle => {
                if let (Some(&a), Some(b)) = (self.clicks.first(), snapped) {
                    let c = [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]];
                    let s: Vec<Pos2> = c.iter().filter_map(|p| to_screen(*p)).collect();
                    if s.len() == 4 {
                        painter.add(egui::Shape::closed_line(s, Stroke::new(1.5, Color32::from_rgb(0, 110, 255))));
                    }
                    painter.text(
                        hover.unwrap_or_default() + egui::vec2(12.0, -12.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{:.2} × {:.2}", (b[0] - a[0]).abs(), (b[1] - a[1]).abs()),
                        egui::FontId::proportional(12.0),
                        Color32::DARK_BLUE,
                    );
                }
                if clicked {
                    if let Some(p) = snapped {
                        if let Some(&a) = self.clicks.first() {
                            if (p[0] - a[0]).abs() > 1e-9 && (p[1] - a[1]).abs() > 1e-9 {
                                let mut sk = sketch.clone();
                                sk.add_rectangle(a, p);
                                out = Some(sk);
                            }
                            self.clicks.clear();
                        } else {
                            self.clicks.push(p);
                        }
                    }
                }
            }
            Tool::Circle => {
                if let (Some(&c), Some(p)) = (self.clicks.first(), snapped) {
                    let r = (p[0] - c[0]).hypot(p[1] - c[1]);
                    let ring: Vec<Pos2> = (0..64)
                        .filter_map(|i| {
                            let t = i as f64 / 64.0 * std::f64::consts::TAU;
                            to_screen([c[0] + r * t.cos(), c[1] + r * t.sin()])
                        })
                        .collect();
                    painter.add(egui::Shape::closed_line(ring, Stroke::new(1.5, Color32::from_rgb(0, 110, 255))));
                    if let Some(hp) = hover {
                        painter.text(hp + egui::vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, format!("Ø {:.2}", 2.0 * r), egui::FontId::proportional(12.0), Color32::DARK_BLUE);
                    }
                }
                if clicked {
                    if let Some(p) = snapped {
                        if let Some(&c) = self.clicks.first() {
                            let r = (p[0] - c[0]).hypot(p[1] - c[1]);
                            if r > 1e-9 {
                                let mut sk = sketch.clone();
                                sk.add_circle(c, r, false);
                                out = Some(sk);
                            }
                            self.clicks.clear();
                        } else {
                            self.clicks.push(p);
                        }
                    }
                }
            }
            Tool::Arc => {
                if let Some(p) = snapped {
                    if self.clicks.len() == 2 {
                        let (c, s) = (self.clicks[0], self.clicks[1]);
                        let r = (s[0] - c[0]).hypot(s[1] - c[1]);
                        let t0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                        let mut sw = ((p[1] - c[1]).atan2(p[0] - c[0]) - t0).rem_euclid(std::f64::consts::TAU);
                        if sw < 1e-6 {
                            sw = std::f64::consts::TAU;
                        }
                        let pl: Vec<Pos2> = (0..=48)
                            .filter_map(|i| {
                                let t = t0 + sw * i as f64 / 48.0;
                                to_screen([c[0] + r * t.cos(), c[1] + r * t.sin()])
                            })
                            .collect();
                        painter.add(egui::Shape::line(pl, Stroke::new(1.5, Color32::from_rgb(0, 110, 255))));
                    } else if self.clicks.len() == 1 {
                        if let (Some(a), Some(b)) = (to_screen(self.clicks[0]), to_screen(p)) {
                            painter.add(egui::Shape::dashed_line(&[a, b], Stroke::new(1.0, Color32::GRAY), 4.0, 4.0));
                        }
                    }
                    if clicked {
                        self.clicks.push(p);
                        if self.clicks.len() == 3 {
                            let mut sk = sketch.clone();
                            sk.add_arc(self.clicks[0], self.clicks[1], self.clicks[2], false);
                            out = Some(sk);
                            self.clicks.clear();
                        }
                    }
                }
            }
            Tool::Dimension => {
                let hit = hover.and_then(|hp| pick_entity(sketch, hp, &to_screen));
                if let Some(id) = &hit {
                    highlight(&painter, sketch, id, &to_screen);
                }
                if clicked {
                    if let Some(pid) = self.hover_point.clone() {
                        // point-to-point distance needs two clicks
                        match self.dim_first.take() {
                            Some(first) if first != pid => {
                                let (a, b) = (sketch.point_xy(&first).unwrap_or_default(), sketch.point_xy(&pid).unwrap_or_default());
                                let d = (a[0] - b[0]).hypot(a[1] - b[1]);
                                let mut sk = sketch.clone();
                                sk.constraints.push(Constraint::Distance { a: first, b: Some(pid), value: Value::Num(nice(d)) });
                                out = Some(sk);
                            }
                            _ => self.dim_first = Some(pid),
                        }
                    } else if let Some(id) = hit {
                        let mut sk = sketch.clone();
                        match sk.entity(&id).cloned() {
                            Some(Entity::Line { start, end, .. }) => {
                                let (a, b) = (sk.point_xy(&start).unwrap(), sk.point_xy(&end).unwrap());
                                let d = (a[0] - b[0]).hypot(a[1] - b[1]);
                                sk.constraints.push(Constraint::Distance { a: id, b: None, value: Value::Num(nice(d)) });
                            }
                            Some(Entity::Circle { radius, .. }) => {
                                sk.constraints.push(Constraint::Diameter { entity: id, value: Value::Num(nice(2.0 * radius)) });
                            }
                            Some(Entity::Arc { center, start, .. }) => {
                                let (c, s) = (sk.point_xy(&center).unwrap(), sk.point_xy(&start).unwrap());
                                sk.constraints.push(Constraint::Radius { entity: id, value: Value::Num(nice((s[0] - c[0]).hypot(s[1] - c[1]))) });
                            }
                            _ => {}
                        }
                        out = Some(sk);
                        self.message = Some("Dimension added: edit its value in the PropertyManager".into());
                    }
                }
                if let Some(first) = &self.dim_first {
                    if let (Some(a), Some(hp)) = (sketch.point_xy(first).and_then(to_screen), hover) {
                        painter.add(egui::Shape::dashed_line(&[a, hp], Stroke::new(1.0, Color32::from_rgb(255, 140, 0)), 4.0, 4.0));
                    }
                }
            }
        }

        // Pending click markers
        for c in &self.clicks {
            if let Some(s) = to_screen(*c) {
                painter.circle_stroke(s, 4.0, Stroke::new(1.5, Color32::from_rgb(0, 110, 255)));
            }
        }
        // Cursor coordinate readout
        if let (Some(hp), Some(p)) = (hover, snapped) {
            painter.text(
                hp + egui::vec2(14.0, 16.0),
                egui::Align2::LEFT_TOP,
                format!("({}, {})", trim(p[0]), trim(p[1])),
                egui::FontId::monospace(11.0),
                Color32::from_gray(90),
            );
        }
        out
    }

    /// Deletes the entity under the cursor (lines/circles/arcs/points).
    pub fn delete_hovered(&self, ui: &egui::Ui, rect: egui::Rect, cam: &Camera, plane: &Plane, sketch: &Sketch) -> Option<Sketch> {
        let hp = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p))?;
        let (w, h) = (rect.width(), rect.height());
        let to_screen = |p: [f64; 2]| -> Option<Pos2> {
            let q = plane.to3d(p);
            cam.project(Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32), w, h).map(|v| rect.min + egui::vec2(v.x, v.y))
        };
        let id = pick_entity(sketch, hp, &to_screen).or_else(|| self.hover_point.clone())?;
        let mut sk = sketch.clone();
        remove_entity(&mut sk, &id);
        Some(sk)
    }
}

/// Removes an entity, anything built on it, and constraints referencing it.
pub fn remove_entity(sk: &mut Sketch, id: &str) {
    let mut doomed = vec![id.to_string()];
    for e in &sk.entities {
        let refs: Vec<&String> = match e {
            Entity::Line { start, end, .. } => vec![start, end],
            Entity::Circle { center, .. } => vec![center],
            Entity::Arc { center, start, end, .. } => vec![center, start, end],
            Entity::Point { .. } => vec![],
        };
        if refs.iter().any(|r| *r == id) {
            doomed.push(e.id().to_string());
        }
    }
    sk.entities.retain(|e| !doomed.iter().any(|d| d == e.id()));
    sk.constraints.retain(|c| {
        let v = serde_json::to_value(c).unwrap_or_default();
        !v.as_object().is_some_and(|o| o.values().any(|x| x.as_str().is_some_and(|s| doomed.iter().any(|d| d == s))))
    });
    // Drop points no longer used by any curve (except standalone sketch points we just kept).
    let used: Vec<String> = sk
        .entities
        .iter()
        .flat_map(|e| match e {
            Entity::Line { start, end, .. } => vec![start.clone(), end.clone()],
            Entity::Circle { center, .. } => vec![center.clone()],
            Entity::Arc { center, start, end, .. } => vec![center.clone(), start.clone(), end.clone()],
            Entity::Point { .. } => vec![],
        })
        .collect();
    let removed_curve = doomed.len() > 1 || !id.starts_with('p');
    if removed_curve {
        sk.entities.retain(|e| !matches!(e, Entity::Point { id, .. } if !used.contains(id)));
    }
}

/// Snap to a horizontal/vertical alignment with the previous point (within 3°),
/// unless snapping to an existing point.
fn infer_hv(prev: [f64; 2], p: [f64; 2], snapped_to_point: bool) -> ([f64; 2], Option<bool>) {
    if snapped_to_point {
        let dx = (p[0] - prev[0]).abs();
        let dy = (p[1] - prev[1]).abs();
        return (p, if dy < 1e-9 { Some(true) } else if dx < 1e-9 { Some(false) } else { None });
    }
    let ang = (p[1] - prev[1]).atan2(p[0] - prev[0]).to_degrees().abs();
    let near = |a: f64, b: f64| (a - b).abs() < 3.0;
    if near(ang, 0.0) || near(ang, 180.0) {
        ([p[0], prev[1]], Some(true))
    } else if near(ang, 90.0) {
        ([prev[0], p[1]], Some(false))
    } else {
        (p, None)
    }
}

/// Round to a grid that scales with the zoom level (keeps numbers tidy).
fn round_nice(p: [f64; 2], view_dist: f32) -> [f64; 2] {
    let step = if view_dist > 2000.0 { 5.0 } else if view_dist > 400.0 { 1.0 } else if view_dist > 80.0 { 0.5 } else { 0.1 };
    [(p[0] / step).round() * step, (p[1] / step).round() * step]
}

fn nice(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Curve entity under the cursor (screen-space test).
fn pick_entity(sk: &Sketch, hp: Pos2, to_screen: &dyn Fn([f64; 2]) -> Option<Pos2>) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    let mut consider = |d: f32, id: &str| {
        if d < 7.0 && best.as_ref().map_or(true, |b| d < b.0) {
            best = Some((d, id.to_string()));
        }
    };
    for e in &sk.entities {
        match e {
            Entity::Line { id, start, end, .. } => {
                if let (Some(a), Some(b)) = (sk.point_xy(start).and_then(to_screen), sk.point_xy(end).and_then(to_screen)) {
                    consider(seg_dist(hp, a, b), id);
                }
            }
            Entity::Circle { id, center, radius, .. } => {
                if let Some(c) = sk.point_xy(center) {
                    let pts: Vec<Pos2> = (0..=48)
                        .filter_map(|i| {
                            let t = i as f64 / 48.0 * std::f64::consts::TAU;
                            to_screen([c[0] + radius * t.cos(), c[1] + radius * t.sin()])
                        })
                        .collect();
                    let d = pts.windows(2).map(|w| seg_dist(hp, w[0], w[1])).fold(f32::INFINITY, f32::min);
                    consider(d, id);
                }
            }
            Entity::Arc { id, center, start, end, .. } => {
                if let (Some(c), Some(s), Some(en)) = (sk.point_xy(center), sk.point_xy(start), sk.point_xy(end)) {
                    let r = (s[0] - c[0]).hypot(s[1] - c[1]);
                    let t0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                    let mut sw = ((en[1] - c[1]).atan2(en[0] - c[0]) - t0).rem_euclid(std::f64::consts::TAU);
                    if sw < 1e-9 {
                        sw = std::f64::consts::TAU;
                    }
                    let pts: Vec<Pos2> = (0..=32)
                        .filter_map(|i| {
                            let t = t0 + sw * i as f64 / 32.0;
                            to_screen([c[0] + r * t.cos(), c[1] + r * t.sin()])
                        })
                        .collect();
                    let d = pts.windows(2).map(|w| seg_dist(hp, w[0], w[1])).fold(f32::INFINITY, f32::min);
                    consider(d, id);
                }
            }
            Entity::Point { .. } => {}
        }
    }
    best.map(|b| b.1)
}

fn highlight(painter: &egui::Painter, sk: &Sketch, id: &str, to_screen: &dyn Fn([f64; 2]) -> Option<Pos2>) {
    let st = Stroke::new(3.0, Color32::from_rgb(255, 140, 0));
    match sk.entity(id) {
        Some(Entity::Line { start, end, .. }) => {
            if let (Some(a), Some(b)) = (sk.point_xy(start).and_then(to_screen), sk.point_xy(end).and_then(to_screen)) {
                painter.line_segment([a, b], st);
            }
        }
        Some(Entity::Circle { center, radius, .. }) => {
            if let Some(c) = sk.point_xy(center) {
                let pts: Vec<Pos2> = (0..64)
                    .filter_map(|i| {
                        let t = i as f64 / 64.0 * std::f64::consts::TAU;
                        to_screen([c[0] + radius * t.cos(), c[1] + radius * t.sin()])
                    })
                    .collect();
                painter.add(egui::Shape::closed_line(pts, st));
            }
        }
        Some(Entity::Arc { center, start, .. }) => {
            if let (Some(c), Some(s)) = (sk.point_xy(center).and_then(to_screen), sk.point_xy(start).and_then(to_screen)) {
                painter.circle_stroke(c, c.distance(s), Stroke::new(1.0, Color32::from_rgb(255, 140, 0)));
            }
        }
        _ => {}
    }
}
