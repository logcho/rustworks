//! Sketch editing tools shared by the GUI and the MCP server: trim, sketch
//! fillet (corner rounding) and offset.

use crate::profile::spline_poles;
use crate::{Constraint, Entity, Sketch, SketchError, Value};
use std::f64::consts::TAU;

type P = [f64; 2];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1]]
}
fn mul(a: P, s: f64) -> P {
    [a[0] * s, a[1] * s]
}
fn dot(a: P, b: P) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: P, b: P) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn len(a: P) -> f64 {
    a[0].hypot(a[1])
}
fn unit(a: P) -> P {
    mul(a, 1.0 / len(a).max(1e-300))
}
fn ang(c: P, p: P) -> f64 {
    (p[1] - c[1]).atan2(p[0] - c[0])
}

fn err(m: impl Into<String>) -> SketchError {
    SketchError::Profile(m.into())
}

/// Resolved geometry of one sketch entity.
#[derive(Clone, Debug)]
enum G {
    Line { a: P, b: P },
    /// CCW arc from angle t0 sweeping `sw` (> 0).
    Arc { c: P, r: f64, t0: f64, sw: f64 },
    Circle { c: P, r: f64 },
    /// Polyline approximation of a spline.
    Poly(Vec<P>),
}

impl G {
    fn at(&self, t: f64) -> P {
        match self {
            G::Line { a, b } => add(*a, mul(sub(*b, *a), t)),
            G::Arc { c, r, t0, .. } => [c[0] + r * (t0 + t).cos(), c[1] + r * (t0 + t).sin()],
            G::Circle { c, r } => [c[0] + r * t.cos(), c[1] + r * t.sin()],
            G::Poly(_) => unreachable!(),
        }
    }
    /// Parameter of the point on this curve nearest to `p`.
    fn param(&self, p: P) -> f64 {
        match self {
            G::Line { a, b } => dot(sub(p, *a), sub(*b, *a)) / dot(sub(*b, *a), sub(*b, *a)).max(1e-300),
            G::Arc { c, t0, sw, .. } => {
                let t = (ang(*c, p) - t0).rem_euclid(TAU);
                // closest end when outside the arc
                if t > *sw && t > (*sw + TAU) / 2.0 { t - TAU } else { t }
            }
            G::Circle { c, .. } => ang(*c, p).rem_euclid(TAU),
            G::Poly(_) => 0.0,
        }
    }
    fn range(&self) -> (f64, f64) {
        match self {
            G::Line { .. } => (0.0, 1.0),
            G::Arc { sw, .. } => (0.0, *sw),
            G::Circle { .. } => (0.0, TAU),
            G::Poly(_) => (0.0, 1.0),
        }
    }
    /// Points where `self` meets `other` (within both curves' extents).
    fn meet(&self, other: &G) -> Vec<P> {
        let pieces = |g: &G| -> Vec<G> {
            match g {
                G::Poly(v) => v.windows(2).map(|w| G::Line { a: w[0], b: w[1] }).collect(),
                g => vec![g.clone()],
            }
        };
        let mut out = vec![];
        for a in pieces(self) {
            for b in pieces(other) {
                for p in raw_meet(&a, &b) {
                    if a.contains(p) && b.contains(p) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
    fn contains(&self, p: P) -> bool {
        let eps = 1e-7;
        match self {
            G::Line { .. } => {
                let t = self.param(p);
                t >= -eps && t <= 1.0 + eps
            }
            G::Arc { sw, .. } => {
                let t = self.param(p);
                t >= -eps && t <= sw + eps
            }
            _ => true,
        }
    }
}

/// Intersections of the full underlying lines/circles.
fn raw_meet(a: &G, b: &G) -> Vec<P> {
    let circ = |g: &G| match g {
        G::Arc { c, r, .. } | G::Circle { c, r } => Some((*c, *r)),
        _ => None,
    };
    match (a, b) {
        (G::Line { a: p, b: q }, G::Line { a: r, b: s }) => {
            let d1 = sub(*q, *p);
            let d2 = sub(*s, *r);
            let den = cross(d1, d2);
            if den.abs() < 1e-14 * len(d1) * len(d2) {
                return vec![];
            }
            let t = cross(sub(*r, *p), d2) / den;
            vec![add(*p, mul(d1, t))]
        }
        (G::Line { a: p, b: q }, _) | (_, G::Line { a: p, b: q }) => {
            let (c, r) = circ(if matches!(a, G::Line { .. }) { b } else { a }).unwrap();
            let d = sub(*q, *p);
            let f = sub(*p, c);
            let (qa, qb, qc) = (dot(d, d), 2.0 * dot(f, d), dot(f, f) - r * r);
            let disc = qb * qb - 4.0 * qa * qc;
            if disc < -1e-12 * qa.max(1.0) {
                return vec![];
            }
            let sq = disc.max(0.0).sqrt();
            let mut v = vec![add(*p, mul(d, (-qb - sq) / (2.0 * qa)))];
            if sq > 1e-12 {
                v.push(add(*p, mul(d, (-qb + sq) / (2.0 * qa))));
            }
            v
        }
        _ => {
            let ((c1, r1), (c2, r2)) = (circ(a).unwrap(), circ(b).unwrap());
            let d = len(sub(c2, c1));
            if d < 1e-12 || d > r1 + r2 + 1e-9 || d < (r1 - r2).abs() - 1e-9 {
                return vec![];
            }
            let x = (d * d + r1 * r1 - r2 * r2) / (2.0 * d);
            let h = (r1 * r1 - x * x).max(0.0).sqrt();
            let u = unit(sub(c2, c1));
            let m = add(c1, mul(u, x));
            let n = [-u[1], u[0]];
            if h < 1e-12 { vec![m] } else { vec![add(m, mul(n, h)), sub(m, mul(n, h))] }
        }
    }
}

impl Sketch {
    fn xy(&self, id: &str) -> Result<P, SketchError> {
        self.point_xy(id).ok_or_else(|| SketchError::UnknownEntity(id.into()))
    }

    fn geom(&self, e: &Entity) -> Result<Option<G>, SketchError> {
        Ok(Some(match e {
            Entity::Point { .. } => return Ok(None),
            Entity::Line { start, end, .. } => G::Line { a: self.xy(start)?, b: self.xy(end)? },
            Entity::Circle { center, radius, .. } => G::Circle { c: self.xy(center)?, r: *radius },
            Entity::Arc { center, start, end, .. } => {
                let (c, s, e) = (self.xy(center)?, self.xy(start)?, self.xy(end)?);
                let t0 = ang(c, s);
                let mut sw = (ang(c, e) - t0).rem_euclid(TAU);
                if sw < 1e-12 {
                    sw = TAU;
                }
                G::Arc { c, r: len(sub(s, c)), t0, sw }
            }
            Entity::Spline { points, .. } => {
                let xy: Vec<P> = points.iter().map(|p| self.xy(p)).collect::<Result<_, _>>()?;
                let closed = points.len() > 3 && points.first() == points.last();
                let poles = spline_poles(&xy, closed);
                let n = (poles.len() - 1) / 3;
                G::Poly((0..=n * 16).map(|k| crate::profile::bezier_point(&poles, k as f64 / 16.0)).collect())
            }
        }))
    }

    fn new_point(&mut self, p: P) -> String {
        let id = self.fresh_id("p");
        self.entities.push(Entity::Point { id: id.clone(), x: p[0], y: p[1], fixed: false });
        id
    }

    fn entity_mut(&mut self, id: &str) -> Option<&mut Entity> {
        self.entities.iter_mut().find(|e| e.id() == id)
    }

    /// Power trim: removes the piece of `entity` around `pick` that lies between
    /// its nearest intersections with other geometry (or the whole entity if
    /// nothing crosses it). New endpoints stay attached to the cutting curves.
    /// Returns ids of entities created for the kept pieces.
    pub fn trim(&mut self, entity: &str, pick: P) -> Result<Vec<String>, SketchError> {
        let target = self.entity(entity).cloned().ok_or_else(|| SketchError::UnknownEntity(entity.into()))?;
        if matches!(target, Entity::Spline { .. }) {
            return Err(err("trimming splines is not supported; delete the spline or edit its points"));
        }
        let g = self.geom(&target)?.ok_or_else(|| err(format!("`{entity}` is a point; nothing to trim")))?;
        // (param, cutter id) of every crossing
        let mut cuts: Vec<(f64, String)> = vec![];
        for e in &self.entities {
            if e.id() == entity {
                continue;
            }
            if let Some(o) = self.geom(e)? {
                for p in g.meet(&o) {
                    cuts.push((g.param(p), e.id().to_string()));
                }
            }
        }
        let (lo_r, hi_r) = g.range();
        let tol = 1e-7 * (hi_r - lo_r).max(1.0);
        let circle = matches!(g, G::Circle { .. });
        if !circle {
            cuts.retain(|c| c.0 > lo_r + tol && c.0 < hi_r - tol);
        }
        cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
        cuts.dedup_by(|a, b| (a.0 - b.0).abs() < tol);
        let tp = g.param(pick);
        let pin = |sk: &mut Sketch, p: P, cutter: &str| -> String {
            let id = sk.new_point(p);
            let c = match sk.entity(cutter) {
                Some(Entity::Line { .. }) => Some(Constraint::PointOnLine { point: id.clone(), line: cutter.into() }),
                Some(Entity::Circle { .. } | Entity::Arc { .. }) => Some(Constraint::PointOnCircle { point: id.clone(), circle: cutter.into() }),
                _ => None,
            };
            sk.constraints.extend(c);
            id
        };
        // Constraints that no longer hold for a shortened entity.
        let size_dependent = |c: &Constraint| {
            matches!(
                c,
                Constraint::Distance { .. }
                    | Constraint::HorizontalDistance { .. }
                    | Constraint::VerticalDistance { .. }
                    | Constraint::Equal { .. }
                    | Constraint::Midpoint { .. }
                    | Constraint::Symmetric { .. }
            )
        };
        if circle {
            if cuts.len() < 2 {
                self.remove(&[entity.to_string()]);
                return Ok(vec![]);
            }
            // cyclic neighbours of the pick angle
            let tp = tp.rem_euclid(TAU);
            let hi = cuts.iter().find(|c| c.0 > tp).unwrap_or(&cuts[0]).clone();
            let lo = cuts.iter().rev().find(|c| c.0 < tp).unwrap_or(&cuts[cuts.len() - 1]).clone();
            let (Entity::Circle { center, construction, .. }, G::Circle { .. }) = (&target, &g) else { unreachable!() };
            let s = pin(self, g.at(hi.0), &hi.1);
            let e = pin(self, g.at(lo.0), &lo.1);
            let (center, construction) = (center.clone(), *construction);
            *self.entity_mut(entity).unwrap() = Entity::Arc { id: entity.into(), center, start: s, end: e, construction };
            return Ok(vec![]);
        }
        let lo = cuts.iter().rev().find(|c| c.0 < tp).cloned();
        let hi = cuts.iter().find(|c| c.0 > tp).cloned();
        if lo.is_none() && hi.is_none() {
            self.remove(&[entity.to_string()]);
            return Ok(vec![]);
        }
        self.constraints.retain(|c| !(size_dependent(c) && c.refs().iter().any(|r| *r == entity)));
        let mut created = vec![];
        let (old_start, old_end) = match &target {
            Entity::Line { start, end, .. } | Entity::Arc { start, end, .. } => (start.clone(), end.clone()),
            _ => unreachable!(),
        };
        // piece 1 keeps the id: [start, lo]; piece 2 is new: [hi, end]
        let p_lo = lo.as_ref().map(|c| pin(self, g.at(c.0), &c.1));
        let p_hi = hi.as_ref().map(|c| pin(self, g.at(c.0), &c.1));
        match (p_lo, p_hi) {
            (Some(a), Some(b)) => {
                match self.entity_mut(entity).unwrap() {
                    Entity::Line { end, .. } | Entity::Arc { end, .. } => *end = a,
                    _ => {}
                }
                let nid = self.fresh_id(if matches!(target, Entity::Line { .. }) { "l" } else { "a" });
                let piece = match &target {
                    Entity::Line { construction, .. } => Entity::Line { id: nid.clone(), start: b, end: old_end.clone(), construction: *construction },
                    Entity::Arc { center, construction, .. } => {
                        Entity::Arc { id: nid.clone(), center: center.clone(), start: b, end: old_end.clone(), construction: *construction }
                    }
                    _ => unreachable!(),
                };
                self.entities.push(piece);
                // keep orientation relations on the new piece too
                let copies: Vec<Constraint> = self
                    .constraints
                    .iter()
                    .filter_map(|c| match c {
                        Constraint::Horizontal { line } if line == entity => Some(Constraint::Horizontal { line: nid.clone() }),
                        Constraint::Vertical { line } if line == entity => Some(Constraint::Vertical { line: nid.clone() }),
                        Constraint::Concentric { a, b } if a == entity => Some(Constraint::Concentric { a: nid.clone(), b: b.clone() }),
                        _ => None,
                    })
                    .collect();
                self.constraints.extend(copies);
                if let Entity::Arc { .. } = &target {
                    self.constraints.push(Constraint::Equal { a: entity.into(), b: nid.clone() });
                }
                created.push(nid);
            }
            (Some(a), None) => match self.entity_mut(entity).unwrap() {
                Entity::Line { end, .. } | Entity::Arc { end, .. } => *end = a,
                _ => {}
            },
            (None, Some(b)) => match self.entity_mut(entity).unwrap() {
                Entity::Line { start, .. } | Entity::Arc { start, .. } => *start = b,
                _ => {}
            },
            (None, None) => unreachable!(),
        }
        self.prune_points(&[old_start, old_end]);
        Ok(created)
    }

    /// Rounds the corner where exactly two lines meet at `point` with a tangent
    /// arc of `radius`. Returns the new arc's id.
    pub fn fillet_corner(&mut self, point: &str, radius: f64) -> Result<String, SketchError> {
        if radius <= 0.0 {
            return Err(err("fillet radius must be positive"));
        }
        let p = self.xy(point)?;
        let lines: Vec<(String, String)> = self
            .entities
            .iter()
            .filter_map(|e| match e {
                Entity::Line { id, start, end, construction: false } if start == point => Some((id.clone(), end.clone())),
                Entity::Line { id, start, end, construction: false } if end == point => Some((id.clone(), start.clone())),
                _ => None,
            })
            .collect();
        if lines.len() != 2 {
            return Err(err(format!("sketch fillet needs exactly two lines meeting at `{point}` (found {})", lines.len())));
        }
        let (a1, a2) = (self.xy(&lines[0].1)?, self.xy(&lines[1].1)?);
        let (u1, u2) = (unit(sub(a1, p)), unit(sub(a2, p)));
        let phi = dot(u1, u2).clamp(-1.0, 1.0).acos();
        if phi < 1e-6 || (std::f64::consts::PI - phi) < 1e-6 {
            return Err(err("the lines are collinear; nothing to round"));
        }
        let d = radius / (phi / 2.0).tan();
        if d >= len(sub(a1, p)) - 1e-9 || d >= len(sub(a2, p)) - 1e-9 {
            return Err(err(format!("radius {radius} is too large for these lines (needs {d:.4} of each line)")));
        }
        let (t1, t2) = (add(p, mul(u1, d)), add(p, mul(u2, d)));
        let c = add(p, mul(unit(add(u1, u2)), radius / (phi / 2.0).sin()));
        let (k1, k2) = (self.new_point(t1), self.new_point(t2));
        for ((lid, _), k) in lines.iter().zip([&k1, &k2]) {
            if let Some(Entity::Line { start, end, .. }) = self.entity_mut(lid) {
                if start == point {
                    *start = k.clone();
                } else {
                    *end = k.clone();
                }
            }
        }
        // the sharp corner and anything pinned to it (incl. line lengths) go away
        let gone = [point.to_string(), lines[0].0.clone(), lines[1].0.clone()];
        self.constraints.retain(|cn| match cn {
            Constraint::Distance { a, b: None, .. } => !gone[1..].contains(a),
            cn => !cn.refs().iter().any(|r| *r == point),
        });
        let _ = gone;
        let cid = self.new_point(c);
        // CCW from the tangent point on the "right" to the one on the "left"
        let (s, e) = if cross(sub(t1, c), sub(t2, c)) > 0.0 { (k1, k2) } else { (k2, k1) };
        let aid = self.fresh_id("a");
        self.entities.push(Entity::Arc { id: aid.clone(), center: cid, start: s, end: e, construction: false });
        self.constraints.push(Constraint::Tangent { a: lines[0].0.clone(), b: aid.clone() });
        self.constraints.push(Constraint::Tangent { a: lines[1].0.clone(), b: aid.clone() });
        self.constraints.push(Constraint::Radius { entity: aid.clone(), value: Value::Num(radius) });
        self.prune_points(&[point.to_string()]);
        Ok(aid)
    }

    /// Offsets a connected chain of lines/arcs (or single circles/splines) by
    /// `distance`. For closed chains a positive distance grows the outline;
    /// for open chains it offsets to the left of the drawing direction.
    /// Returns ids of the created entities.
    pub fn offset(&mut self, entities: &[String], distance: f64) -> Result<Vec<String>, SketchError> {
        if distance == 0.0 {
            return Err(err("offset distance must be non-zero"));
        }
        let mut items: Vec<(Entity, G)> = vec![];
        for id in entities {
            let e = self.entity(id).cloned().ok_or_else(|| SketchError::UnknownEntity(id.clone()))?;
            if let Some(g) = self.geom(&e)? {
                items.push((e, g));
            }
        }
        if items.is_empty() {
            return Err(err("select lines, arcs, circles or splines to offset"));
        }
        let mut created = vec![];
        // standalone closed curves
        let mut chainable = vec![];
        for (e, g) in items {
            match (&e, &g) {
                (Entity::Circle { .. }, G::Circle { c, r }) => {
                    let nr = r + distance;
                    if nr <= 0.0 {
                        return Err(err("offset would collapse the circle"));
                    }
                    created.push(self.add_circle(*c, nr, false));
                }
                (Entity::Spline { points, .. }, G::Poly(_)) => {
                    let xy: Vec<P> = points.iter().map(|p| self.xy(p)).collect::<Result<_, _>>()?;
                    let closed = points.len() > 3 && points.first() == points.last();
                    let poles = spline_poles(&xy, closed);
                    let n = (poles.len() - 1) / 3;
                    let sign = if closed && signed_area(&xy) < 0.0 { 1.0 } else { -1.0 };
                    let sign = if closed { sign } else { 1.0 };
                    let pts: Vec<P> = (0..xy.len())
                        .map(|i| {
                            let u = (i as f64).min(n as f64);
                            let du = 1e-4;
                            let tg = unit(sub(
                                crate::profile::bezier_point(&poles, (u + du).min(n as f64)),
                                crate::profile::bezier_point(&poles, (u - du).max(0.0)),
                            ));
                            add(xy[i], mul([-tg[1], tg[0]], distance * sign))
                        })
                        .collect();
                    created.push(self.add_spline(&pts, false));
                }
                _ => chainable.push((e, g)),
            }
        }
        if chainable.is_empty() {
            return Ok(created);
        }
        // Order the lines/arcs into a chain by shared endpoints.
        let ends = |g: &G| -> (P, P) {
            match g {
                G::Line { a, b } => (*a, *b),
                G::Arc { .. } => (g.at(0.0), g.at(g.range().1)),
                _ => unreachable!(),
            }
        };
        let near = |a: P, b: P| len(sub(a, b)) < 1e-6;
        let mut chain: Vec<(G, bool)> = vec![]; // (geometry, reversed)
        let mut pool: Vec<G> = chainable.into_iter().map(|x| x.1).collect();
        // start from a piece whose start is not another piece's end (open chain), else any
        let start_idx = (0..pool.len())
            .find(|&i| {
                let (s, _) = ends(&pool[i]);
                !pool.iter().enumerate().any(|(j, g)| j != i && (near(ends(g).1, s) || near(ends(g).0, s)))
            })
            .or_else(|| {
                (0..pool.len()).find(|&i| {
                    let (_, e) = ends(&pool[i]);
                    !pool.iter().enumerate().any(|(j, g)| j != i && (near(ends(g).1, e) || near(ends(g).0, e)))
                })
            });
        let (first, rev0) = match start_idx {
            Some(i) => {
                let (s, _) = ends(&pool[i]);
                let open_at_start = !pool.iter().enumerate().any(|(j, g)| j != i && (near(ends(g).1, s) || near(ends(g).0, s)));
                (pool.remove(i), !open_at_start)
            }
            None => (pool.remove(0), false),
        };
        chain.push((first, rev0));
        while !pool.is_empty() {
            let (g, r) = chain.last().unwrap();
            let tail = if *r { ends(g).0 } else { ends(g).1 };
            let Some(k) = pool.iter().position(|g| near(ends(g).0, tail) || near(ends(g).1, tail)) else {
                return Err(err("offset entities must form one connected chain"));
            };
            let g = pool.remove(k);
            let rev = !near(ends(&g).0, tail);
            chain.push((g, rev));
        }
        let head = if chain[0].1 { ends(&chain[0].0).1 } else { ends(&chain[0].0).0 };
        let (lg, lr) = chain.last().unwrap();
        let tail = if *lr { ends(lg).0 } else { ends(lg).1 };
        let closed = chain.len() > 1 && near(head, tail);
        // traversal-ordered polyline to find orientation
        let mut side = 1.0;
        if closed {
            let mut poly = vec![];
            for (g, r) in &chain {
                let (lo, hi) = g.range();
                for k in 0..16 {
                    let t = lo + (hi - lo) * k as f64 / 16.0;
                    poly.push(g.at(if *r { hi - (t - lo) } else { t }));
                }
            }
            // left of a CCW loop is inside; positive distance should grow it
            side = if signed_area(&poly) > 0.0 { -1.0 } else { 1.0 };
        }
        let dl = distance * side; // offset to the left by dl
        // offset pieces in traversal order: (start, end, geometry)
        enum O {
            L(P, P),
            A { c: P, r: f64, s: P, e: P, ccw: bool },
        }
        let mut offs: Vec<O> = vec![];
        for (g, r) in &chain {
            match g {
                G::Line { a, b } => {
                    let (s, e) = if *r { (*b, *a) } else { (*a, *b) };
                    let n = unit(sub(e, s));
                    let nl = [-n[1], n[0]];
                    offs.push(O::L(add(s, mul(nl, dl)), add(e, mul(nl, dl))));
                }
                G::Arc { c, r: rad, .. } => {
                    let ccw = !*r;
                    // left of a CCW arc points to the center
                    let nr = if ccw { rad - dl } else { rad + dl };
                    if nr <= 1e-9 {
                        return Err(err("offset distance collapses an arc"));
                    }
                    let (s0, e0) = ends(g);
                    let (s, e) = if *r { (e0, s0) } else { (s0, e0) };
                    let k = nr / rad;
                    offs.push(O::A { c: *c, r: nr, s: add(*c, mul(sub(s, *c), k)), e: add(*c, mul(sub(e, *c), k)), ccw });
                }
                _ => unreachable!(),
            }
        }
        // Join neighbours: tangent joints already meet; otherwise intersect.
        let n = offs.len();
        let joints = if closed { n } else { n - 1 };
        for j in 0..joints {
            let (i, k) = (j, (j + 1) % n);
            let end_i = match &offs[i] {
                O::L(_, e) => *e,
                O::A { e, .. } => *e,
            };
            let start_k = match &offs[k] {
                O::L(s, _) => *s,
                O::A { s, .. } => *s,
            };
            if near(end_i, start_k) {
                continue;
            }
            let gi = match &offs[i] {
                O::L(s, e) => G::Line { a: *s, b: *e },
                O::A { c, r, .. } => G::Circle { c: *c, r: *r },
            };
            let gk = match &offs[k] {
                O::L(s, e) => G::Line { a: *s, b: *e },
                O::A { c, r, .. } => G::Circle { c: *c, r: *r },
            };
            let mid = mul(add(end_i, start_k), 0.5);
            let cand = raw_meet(&gi, &gk);
            let Some(x) = cand.into_iter().min_by(|a, b| len(sub(*a, mid)).total_cmp(&len(sub(*b, mid)))) else {
                return Err(err("offset pieces do not meet; try a smaller distance"));
            };
            let set_end = |o: &mut O, p: P| match o {
                O::L(_, e) => *e = p,
                O::A { e, .. } => *e = p,
            };
            let set_start = |o: &mut O, p: P| match o {
                O::L(s, _) => *s = p,
                O::A { s, .. } => *s = p,
            };
            set_end(&mut offs[i], x);
            set_start(&mut offs[k], x);
        }
        for o in &offs {
            match o {
                O::L(s, e) => {
                    if len(sub(*e, *s)) < 1e-9 {
                        return Err(err("offset collapses a line; try a smaller distance"));
                    }
                    created.push(self.add_line(*s, *e, false));
                }
                O::A { c, s, e, ccw, .. } => {
                    let (a, b) = if *ccw { (*s, *e) } else { (*e, *s) };
                    created.push(self.add_arc(*c, a, b, false));
                }
            }
        }
        Ok(created)
    }
}

fn signed_area(poly: &[P]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| cross(poly[i], poly[(i + 1) % n])).sum::<f64>() / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;
    use std::collections::BTreeMap;

    fn solve(s: &mut Sketch) {
        let r = s.solve(&Scope::new(&BTreeMap::new())).unwrap();
        assert!(r.converged, "{r:?}");
    }

    #[test]
    fn trim_cross() {
        // horizontal line crossed by a vertical one: trim the right half
        let mut s = Sketch::default();
        let h = s.add_line([-10.0, 0.0], [10.0, 0.0], false);
        s.add_line([0.0, -5.0], [0.0, 5.0], false);
        s.trim(&h, [6.0, 0.0]).unwrap();
        let Some(Entity::Line { start, end, .. }) = s.entity(&h).cloned() else { panic!() };
        let (a, b) = (s.point_xy(&start).unwrap(), s.point_xy(&end).unwrap());
        assert!((a[0] + 10.0).abs() < 1e-9 && b[0].abs() < 1e-9, "{a:?} {b:?}");
        solve(&mut s);
        // middle trim leaves two pieces
        let mut s = Sketch::default();
        let h = s.add_line([-10.0, 0.0], [10.0, 0.0], false);
        s.add_line([-3.0, -5.0], [-3.0, 5.0], false);
        s.add_line([3.0, -5.0], [3.0, 5.0], false);
        let made = s.trim(&h, [0.0, 0.0]).unwrap();
        assert_eq!(made.len(), 1);
        solve(&mut s);
    }

    #[test]
    fn trim_circle_to_arc() {
        let mut s = Sketch::default();
        let c = s.add_circle([0.0, 0.0], 5.0, false);
        s.add_line([-10.0, 0.0], [10.0, 0.0], false);
        s.trim(&c, [0.0, -5.0]).unwrap();
        assert!(matches!(s.entity(&c), Some(Entity::Arc { .. })));
        solve(&mut s);
        // upper half arc + the line → trim the line ends to get a D-shape region
        let l = s.entities.iter().find(|e| matches!(e, Entity::Line { .. })).unwrap().id().to_string();
        s.trim(&l, [8.0, 0.0]).unwrap();
        s.trim(&l, [-8.0, 0.0]).unwrap();
        let regs = s.regions().unwrap();
        assert_eq!(regs.len(), 1);
        assert!((regs[0].area() - std::f64::consts::PI * 12.5).abs() < 1e-6, "{}", regs[0].area());
    }

    #[test]
    fn fillet_rectangle_corner() {
        let mut s = Sketch::default();
        let ls = s.add_rectangle([0.0, 0.0], [20.0, 10.0]);
        let corner = match s.entity(&ls[1]).unwrap() {
            Entity::Line { start, .. } => start.clone(), // (20,0)
            _ => unreachable!(),
        };
        let arc = s.fillet_corner(&corner, 3.0).unwrap();
        solve(&mut s);
        let regs = s.regions().unwrap();
        let expected = 200.0 - (9.0 - std::f64::consts::PI * 9.0 / 4.0);
        assert!((regs[0].area() - expected).abs() < 1e-6, "{} vs {expected}", regs[0].area());
        assert!(s.entity(&arc).is_some());
    }

    #[test]
    fn offset_rectangle_and_slot() {
        let mut s = Sketch::default();
        let ls = s.add_rectangle([0.0, 0.0], [20.0, 10.0]);
        let made = s.offset(&ls, 2.0).unwrap();
        assert_eq!(made.len(), 4);
        let regs = s.regions().unwrap();
        assert_eq!(regs.len(), 1);
        assert!((regs[0].outer.area() - 24.0 * 14.0).abs() < 1e-6, "{}", regs[0].outer.area());
        assert_eq!(regs[0].holes.len(), 1);
        // slot offset inward keeps arcs tangent
        let mut s = Sketch::default();
        let ids = s.add_slot([0.0, 0.0], [10.0, 0.0], 4.0);
        s.offset(&ids, -1.0).unwrap();
        let regs = s.regions().unwrap();
        let inner = 10.0 * 6.0 + std::f64::consts::PI * 9.0;
        assert!((regs[0].holes[0].area() - inner).abs() < 1e-6, "{}", regs[0].holes[0].area());
    }

    #[test]
    fn spline_profile_and_path() {
        let mut s = Sketch::default();
        s.add_spline(&[[0.0, 0.0], [10.0, 5.0], [20.0, 0.0]], false);
        s.add_line([20.0, 0.0], [0.0, 0.0], false);
        let regs = s.regions().unwrap();
        assert_eq!(regs.len(), 1);
        assert!(regs[0].area() > 50.0 && regs[0].area() < 100.0, "{}", regs[0].area());
        let mut p = Sketch::default();
        p.add_line([0.0, 0.0], [0.0, 10.0], false);
        p.add_arc([5.0, 10.0], [0.0, 10.0], [5.0, 15.0], false);
        let paths = p.paths().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].segments.len(), 2);
    }
}
