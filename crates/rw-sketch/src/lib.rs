//! 2D parametric sketches: entities, geometric/dimensional constraints, a
//! Levenberg–Marquardt solver with degree-of-freedom analysis, and closed
//! profile (region) extraction for feature creation.

pub mod expr;
pub mod profile;
mod solver;
mod tools;

pub use expr::{ExprError, Scope, Value};
pub use profile::{spline_poles, Loop, Region, Segment};
pub use solver::{SolveReport, SolveStatus};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entity {
    Point {
        id: String,
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "is_false")]
        fixed: bool,
    },
    Line {
        id: String,
        start: String,
        end: String,
        #[serde(default, skip_serializing_if = "is_false")]
        construction: bool,
    },
    Circle {
        id: String,
        center: String,
        radius: f64,
        #[serde(default, skip_serializing_if = "is_false")]
        construction: bool,
    },
    /// Counter-clockwise arc from `start` to `end` around `center`.
    Arc {
        id: String,
        center: String,
        start: String,
        end: String,
        #[serde(default, skip_serializing_if = "is_false")]
        construction: bool,
    },
    /// Smooth curve through the listed points (Catmull–Rom, exactly cubic
    /// B-spline in the kernel). Closed when the first and last point coincide.
    Spline {
        id: String,
        points: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        construction: bool,
    },
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

impl Entity {
    pub fn id(&self) -> &str {
        match self {
            Entity::Point { id, .. }
            | Entity::Line { id, .. }
            | Entity::Circle { id, .. }
            | Entity::Arc { id, .. }
            | Entity::Spline { id, .. } => id,
        }
    }
    pub fn is_construction(&self) -> bool {
        match self {
            Entity::Point { .. } => true,
            Entity::Line { construction, .. }
            | Entity::Circle { construction, .. }
            | Entity::Arc { construction, .. }
            | Entity::Spline { construction, .. } => *construction,
        }
    }
    /// Ids of the points this entity is built on.
    pub fn point_refs(&self) -> Vec<&String> {
        match self {
            Entity::Point { .. } => vec![],
            Entity::Line { start, end, .. } => vec![start, end],
            Entity::Circle { center, .. } => vec![center],
            Entity::Arc { center, start, end, .. } => vec![center, start, end],
            Entity::Spline { points, .. } => points.iter().collect(),
        }
    }
}

impl Constraint {
    /// Entity ids referenced by this constraint.
    pub fn refs(&self) -> Vec<&String> {
        use Constraint::*;
        match self {
            Coincident { a, b } | Parallel { a, b } | Perpendicular { a, b } | Equal { a, b } | Tangent { a, b } | Concentric { a, b } => vec![a, b],
            HorizontalDistance { a, b, .. } | VerticalDistance { a, b, .. } | Angle { a, b, .. } => vec![a, b],
            Horizontal { line } | Vertical { line } => vec![line],
            Distance { a, b, .. } => std::iter::once(a).chain(b.iter()).collect(),
            Radius { entity, .. } | Diameter { entity, .. } => vec![entity],
            PointOnLine { point, line } | Midpoint { point, line } => vec![point, line],
            PointOnCircle { point, circle } => vec![point, circle],
            Symmetric { a, b, about } => vec![a, b, about],
            Fix { point, .. } => vec![point],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    Coincident { a: String, b: String },
    Horizontal { line: String },
    Vertical { line: String },
    /// Point-to-point distance (or line length when `b` omitted and `a` is a line).
    Distance { a: String, #[serde(default)] b: Option<String>, value: Value },
    HorizontalDistance { a: String, b: String, value: Value },
    VerticalDistance { a: String, b: String, value: Value },
    Radius { entity: String, value: Value },
    Diameter { entity: String, value: Value },
    /// Angle from line `a` to line `b`, in degrees (counter-clockwise).
    Angle { a: String, b: String, value: Value },
    Parallel { a: String, b: String },
    Perpendicular { a: String, b: String },
    /// Equal length (lines) or equal radius (circles/arcs).
    Equal { a: String, b: String },
    /// Line tangent to circle/arc, or circle/arc tangent to circle/arc (external).
    Tangent { a: String, b: String },
    Concentric { a: String, b: String },
    PointOnLine { point: String, line: String },
    PointOnCircle { point: String, circle: String },
    Midpoint { point: String, line: String },
    /// `a` and `b` symmetric about line `about`.
    Symmetric { a: String, b: String, about: String },
    /// Lock a point at its current (or given) position.
    Fix { point: String, #[serde(default)] x: Option<Value>, #[serde(default)] y: Option<Value> },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Sketch {
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum SketchError {
    #[error("unknown sketch entity `{0}`")]
    UnknownEntity(String),
    #[error("entity `{0}` has wrong type for this constraint (expected {1})")]
    WrongType(String, &'static str),
    #[error(transparent)]
    Expr(#[from] ExprError),
    #[error("profile error: {0}")]
    Profile(String),
}

impl Sketch {
    pub fn entity(&self, id: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id() == id)
    }

    pub fn point_xy(&self, id: &str) -> Option<[f64; 2]> {
        match self.entity(id)? {
            Entity::Point { x, y, .. } => Some([*x, *y]),
            _ => None,
        }
    }

    /// Generates a fresh id with the given prefix (`p`, `l`, `c`, `a`).
    pub fn fresh_id(&self, prefix: &str) -> String {
        let mut n = self.entities.len() + 1;
        loop {
            let id = format!("{prefix}{n}");
            if self.entity(&id).is_none() {
                return id;
            }
            n += 1;
        }
    }

    /// Returns an existing point within `tol` of (x, y) or creates one.
    /// Auto-merging makes endpoints of consecutive segments shared, so closed
    /// shapes drawn by coordinates form closed loops automatically.
    pub fn point_at(&mut self, x: f64, y: f64, tol: f64) -> String {
        for e in &self.entities {
            if let Entity::Point { id, x: px, y: py, .. } = e {
                if (px - x).hypot(py - y) <= tol {
                    return id.clone();
                }
            }
        }
        let id = self.fresh_id("p");
        self.entities.push(Entity::Point { id: id.clone(), x, y, fixed: false });
        id
    }

    pub fn add_line(&mut self, a: [f64; 2], b: [f64; 2], construction: bool) -> String {
        let s = self.point_at(a[0], a[1], 1e-9);
        let e = self.point_at(b[0], b[1], 1e-9);
        let id = self.fresh_id("l");
        self.entities.push(Entity::Line { id: id.clone(), start: s, end: e, construction });
        id
    }

    pub fn add_circle(&mut self, c: [f64; 2], r: f64, construction: bool) -> String {
        let cp = self.point_at(c[0], c[1], 1e-9);
        let id = self.fresh_id("c");
        self.entities.push(Entity::Circle { id: id.clone(), center: cp, radius: r, construction });
        id
    }

    /// CCW arc from `start` to `end` about `center`. The end point is snapped
    /// onto the circle defined by center/start.
    pub fn add_arc(&mut self, c: [f64; 2], s: [f64; 2], e: [f64; 2], construction: bool) -> String {
        let r = (s[0] - c[0]).hypot(s[1] - c[1]);
        let ang = (e[1] - c[1]).atan2(e[0] - c[0]);
        let e = [c[0] + r * ang.cos(), c[1] + r * ang.sin()];
        let cp = self.point_at(c[0], c[1], 1e-9);
        let sp = self.point_at(s[0], s[1], 1e-9);
        let ep = self.point_at(e[0], e[1], 1e-9);
        let id = self.fresh_id("a");
        self.entities.push(Entity::Arc { id: id.clone(), center: cp, start: sp, end: ep, construction });
        id
    }

    /// Spline through `pts`; closed when the first and last points coincide.
    pub fn add_spline(&mut self, pts: &[[f64; 2]], construction: bool) -> String {
        let ids: Vec<String> = pts.iter().map(|p| self.point_at(p[0], p[1], 1e-9)).collect();
        let id = self.fresh_id("s");
        self.entities.push(Entity::Spline { id: id.clone(), points: ids, construction });
        id
    }

    /// Removes entities plus everything built on them (entities using a removed
    /// point, constraints referencing any removed id) and endpoints left unused.
    pub fn remove(&mut self, ids: &[String]) {
        let mut doomed: Vec<String> = ids.to_vec();
        for e in &self.entities {
            if e.point_refs().iter().any(|r| doomed.contains(r)) {
                doomed.push(e.id().to_string());
            }
        }
        let loose: Vec<String> = self
            .entities
            .iter()
            .filter(|e| doomed.iter().any(|d| d == e.id()))
            .flat_map(|e| e.point_refs().into_iter().cloned())
            .collect();
        self.entities.retain(|e| !doomed.iter().any(|d| d == e.id()));
        self.constraints.retain(|c| !c.refs().iter().any(|r| doomed.contains(r)));
        self.prune_points(&loose);
    }

    /// Deletes those `candidates` points that no entity or constraint uses any more.
    pub(crate) fn prune_points(&mut self, candidates: &[String]) {
        let used: std::collections::HashSet<String> = self
            .entities
            .iter()
            .flat_map(|e| e.point_refs().into_iter().cloned())
            .chain(self.constraints.iter().flat_map(|c| c.refs().into_iter().cloned()))
            .collect();
        self.entities.retain(|e| match e {
            Entity::Point { id, .. } => used.contains(id) || !candidates.contains(id),
            _ => true,
        });
    }

    /// Axis-aligned rectangle with horizontal/vertical constraints. Returns the 4 line ids
    /// (bottom, right, top, left).
    pub fn add_rectangle(&mut self, a: [f64; 2], b: [f64; 2]) -> Vec<String> {
        let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
        let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
        let pts = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let ids: Vec<String> = (0..4).map(|i| self.add_line(pts[i], pts[(i + 1) % 4], false)).collect();
        self.constraints.push(Constraint::Horizontal { line: ids[0].clone() });
        self.constraints.push(Constraint::Horizontal { line: ids[2].clone() });
        self.constraints.push(Constraint::Vertical { line: ids[1].clone() });
        self.constraints.push(Constraint::Vertical { line: ids[3].clone() });
        ids
    }

    /// Regular polygon inscribed in a circle of radius `r`.
    pub fn add_polygon(&mut self, c: [f64; 2], r: f64, sides: usize, rot_deg: f64) -> Vec<String> {
        let sides = sides.max(3);
        let pts: Vec<[f64; 2]> = (0..sides)
            .map(|i| {
                let t = rot_deg.to_radians() + i as f64 * std::f64::consts::TAU / sides as f64;
                [c[0] + r * t.cos(), c[1] + r * t.sin()]
            })
            .collect();
        let ids: Vec<String> = (0..sides).map(|i| self.add_line(pts[i], pts[(i + 1) % sides], false)).collect();
        for i in 1..sides {
            self.constraints.push(Constraint::Equal { a: ids[0].clone(), b: ids[i].clone() });
        }
        ids
    }

    /// Slot (stadium) between two centers with given radius.
    pub fn add_slot(&mut self, a: [f64; 2], b: [f64; 2], r: f64) -> Vec<String> {
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = d[0].hypot(d[1]).max(1e-12);
        let n = [-d[1] / len * r, d[0] / len * r];
        let p1 = [a[0] - n[0], a[1] - n[1]];
        let p2 = [b[0] - n[0], b[1] - n[1]];
        let p3 = [b[0] + n[0], b[1] + n[1]];
        let p4 = [a[0] + n[0], a[1] + n[1]];
        let l1 = self.add_line(p1, p2, false);
        let a1 = self.add_arc(b, p2, p3, false);
        let l2 = self.add_line(p3, p4, false);
        let a2 = self.add_arc(a, p4, p1, false);
        self.constraints.push(Constraint::Tangent { a: l1.clone(), b: a1.clone() });
        self.constraints.push(Constraint::Tangent { a: l2.clone(), b: a2.clone() });
        self.constraints.push(Constraint::Equal { a: a1.clone(), b: a2.clone() });
        vec![l1, a1, l2, a2]
    }

    /// Solve all constraints, updating entity positions in place.
    pub fn solve(&mut self, scope: &Scope) -> Result<SolveReport, SketchError> {
        solver::solve(self, scope)
    }

    /// Extract closed regions (outer loop + holes) from non-construction geometry.
    pub fn regions(&self) -> Result<Vec<Region>, SketchError> {
        profile::regions(self)
    }

    /// Connected chains of non-construction geometry (open or closed), in
    /// drawing order — used as sweep paths.
    pub fn paths(&self) -> Result<Vec<Loop>, SketchError> {
        profile::paths(self)
    }

    pub(crate) fn index(&self) -> HashMap<&str, usize> {
        self.entities.iter().enumerate().map(|(i, e)| (e.id(), i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn rectangle_fully_defined() {
        let mut s = Sketch::default();
        let l = s.add_rectangle([0.0, 0.0], [9.0, 4.0]);
        let p0 = match s.entity(&l[0]).unwrap() {
            Entity::Line { start, .. } => start.clone(),
            _ => unreachable!(),
        };
        s.constraints.push(Constraint::Fix { point: p0, x: Some(0.0.into()), y: Some(0.0.into()) });
        s.constraints.push(Constraint::Distance { a: l[0].clone(), b: None, value: Value::Expr("w".into()) });
        s.constraints.push(Constraint::Distance { a: l[1].clone(), b: None, value: 5.0.into() });
        let mut p = BTreeMap::new();
        p.insert("w".to_string(), Value::Num(10.0));
        let r = s.solve(&Scope::new(&p)).unwrap();
        assert_eq!(r.status, SolveStatus::FullyDefined, "{r:?}");
        let regs = s.regions().unwrap();
        assert_eq!(regs.len(), 1);
        assert!((regs[0].outer.area() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn tangent_and_holes() {
        let mut s = Sketch::default();
        s.add_rectangle([-20.0, -10.0], [20.0, 10.0]);
        let c = s.add_circle([0.0, 0.0], 3.0, false);
        s.constraints.push(Constraint::Diameter { entity: c, value: 8.0.into() });
        s.add_slot([-12.0, 0.0], [-8.0, 0.0], 1.5);
        let r = s.solve(&Scope::new(&BTreeMap::new())).unwrap();
        assert!(r.converged, "{r:?}");
        let regs = s.regions().unwrap();
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].holes.len(), 2);
        let expected = 800.0 - std::f64::consts::PI * 16.0 - (4.0 * 3.0 + std::f64::consts::PI * 2.25);
        assert!((regs[0].area() - expected).abs() < 1e-2, "{}", regs[0].area());
    }
}
