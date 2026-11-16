//! Closed-contour detection: turns sketch lines/arcs/circles into oriented
//! loops and nests them into regions (outer boundary + holes).

use crate::{Entity, Sketch, SketchError};
use serde::Serialize;
use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Segment {
    Line { a: [f64; 2], b: [f64; 2] },
    /// Arc from `a` to `b` around `center`; `ccw` gives traversal direction.
    Arc { center: [f64; 2], a: [f64; 2], b: [f64; 2], ccw: bool },
    Circle { center: [f64; 2], radius: f64, ccw: bool },
    /// Chain of cubic Béziers given by 3n+1 control points (C1 continuous).
    Spline { poles: Vec<[f64; 2]> },
}

/// Bézier control points (3n+1) of the Catmull–Rom spline through `pts`.
/// Closed splines (first point repeated at the end) wrap their tangents.
pub fn spline_poles(pts: &[[f64; 2]], closed: bool) -> Vec<[f64; 2]> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    let at = |i: isize| -> [f64; 2] {
        if closed {
            // pts[0] == pts[n-1]; the ring has n-1 distinct points
            let m = (n - 1) as isize;
            pts[i.rem_euclid(m) as usize]
        } else if i < 0 {
            [2.0 * pts[0][0] - pts[1][0], 2.0 * pts[0][1] - pts[1][1]]
        } else if i as usize >= n {
            [2.0 * pts[n - 1][0] - pts[n - 2][0], 2.0 * pts[n - 1][1] - pts[n - 2][1]]
        } else {
            pts[i as usize]
        }
    };
    let mut out = vec![pts[0]];
    for i in 0..(n - 1) as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let p2 = if closed && i as usize == n - 2 { pts[n - 1] } else { p2 };
        out.push([p1[0] + (p2[0] - p0[0]) / 6.0, p1[1] + (p2[1] - p0[1]) / 6.0]);
        out.push([p2[0] - (p3[0] - p1[0]) / 6.0, p2[1] - (p3[1] - p1[1]) / 6.0]);
        out.push(p2);
    }
    out
}

/// Point on a Bézier chain at global parameter `u` in [0, n].
pub fn bezier_point(poles: &[[f64; 2]], u: f64) -> [f64; 2] {
    let n = (poles.len() - 1) / 3;
    let k = (u.floor() as usize).min(n.saturating_sub(1));
    let t = u - k as f64;
    let p = &poles[3 * k..3 * k + 4];
    let m = 1.0 - t;
    let w = [m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t];
    [
        w[0] * p[0][0] + w[1] * p[1][0] + w[2] * p[2][0] + w[3] * p[3][0],
        w[0] * p[0][1] + w[1] * p[1][1] + w[2] * p[2][1] + w[3] * p[3][1],
    ]
}

const SPLINE_SAMPLES: usize = 24;

impl Segment {
    pub fn start(&self) -> [f64; 2] {
        match self {
            Segment::Line { a, .. } | Segment::Arc { a, .. } => *a,
            Segment::Circle { center, radius, .. } => [center[0] + radius, center[1]],
            Segment::Spline { poles } => poles[0],
        }
    }
    pub fn end(&self) -> [f64; 2] {
        match self {
            Segment::Line { b, .. } | Segment::Arc { b, .. } => *b,
            Segment::Circle { .. } => self.start(),
            Segment::Spline { poles } => poles[poles.len() - 1],
        }
    }
    /// Signed sweep angle of an arc in radians.
    pub fn sweep(&self) -> f64 {
        match self {
            Segment::Arc { center, a, b, ccw } => {
                let ta = (a[1] - center[1]).atan2(a[0] - center[0]);
                let tb = (b[1] - center[1]).atan2(b[0] - center[0]);
                let raw = if *ccw { tb - ta } else { ta - tb };
                let mut s = raw.rem_euclid(TAU);
                if s < 1e-12 {
                    s = TAU;
                }
                if *ccw { s } else { -s }
            }
            Segment::Circle { ccw, .. } => if *ccw { TAU } else { -TAU },
            Segment::Line { .. } | Segment::Spline { .. } => 0.0,
        }
    }
    /// Point halfway along the segment.
    pub fn mid(&self) -> [f64; 2] {
        match self {
            Segment::Line { a, b } => [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0],
            Segment::Arc { center, a, .. } => {
                let r = (a[0] - center[0]).hypot(a[1] - center[1]);
                let t = (a[1] - center[1]).atan2(a[0] - center[0]) + self.sweep() / 2.0;
                [center[0] + r * t.cos(), center[1] + r * t.sin()]
            }
            Segment::Circle { center, radius, .. } => [center[0] - radius, center[1]],
            Segment::Spline { poles } => bezier_point(poles, ((poles.len() - 1) / 3) as f64 / 2.0),
        }
    }
    pub fn reversed(&self) -> Segment {
        match self {
            Segment::Line { a, b } => Segment::Line { a: *b, b: *a },
            Segment::Arc { center, a, b, ccw } => Segment::Arc { center: *center, a: *b, b: *a, ccw: !ccw },
            Segment::Circle { center, radius, ccw } => Segment::Circle { center: *center, radius: *radius, ccw: !ccw },
            Segment::Spline { poles } => Segment::Spline { poles: poles.iter().rev().copied().collect() },
        }
    }
    /// Signed area contribution (Green's theorem, ½∮ x dy − y dx).
    fn area_term(&self) -> f64 {
        match self {
            Segment::Line { a, b } => 0.5 * (a[0] * b[1] - b[0] * a[1]),
            Segment::Arc { center: c, a, b, .. } => {
                let r2 = (a[0] - c[0]).powi(2) + (a[1] - c[1]).powi(2);
                let d = [b[0] - a[0], b[1] - a[1]];
                0.5 * (c[0] * d[1] - c[1] * d[0] + r2 * self.sweep())
            }
            Segment::Circle { radius, .. } => 0.5 * radius * radius * self.sweep(),
            Segment::Spline { .. } => {
                let mut v = vec![];
                self.sample(&mut v, 0);
                v.push(self.end());
                v.windows(2).map(|w| 0.5 * (w[0][0] * w[1][1] - w[1][0] * w[0][1])).sum()
            }
        }
    }
    /// Polyline approximation (excluding the end point).
    pub fn sample(&self, out: &mut Vec<[f64; 2]>, per_rev: usize) {
        match self {
            Segment::Line { a, .. } => out.push(*a),
            Segment::Arc { center, a, .. } => {
                let r = (a[0] - center[0]).hypot(a[1] - center[1]);
                let t0 = (a[1] - center[1]).atan2(a[0] - center[0]);
                let sw = self.sweep();
                let n = ((sw.abs() / TAU) * per_rev as f64).ceil().max(2.0) as usize;
                for i in 0..n {
                    let t = t0 + sw * i as f64 / n as f64;
                    out.push([center[0] + r * t.cos(), center[1] + r * t.sin()]);
                }
            }
            Segment::Spline { poles } => {
                let n = (poles.len() - 1) / 3;
                for k in 0..n * SPLINE_SAMPLES {
                    out.push(bezier_point(poles, k as f64 / SPLINE_SAMPLES as f64));
                }
            }
            Segment::Circle { center, radius, ccw } => {
                for i in 0..per_rev {
                    let t = TAU * i as f64 / per_rev as f64 * if *ccw { 1.0 } else { -1.0 };
                    out.push([center[0] + radius * t.cos(), center[1] + radius * t.sin()]);
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Loop {
    pub segments: Vec<Segment>,
}

impl Loop {
    pub fn signed_area(&self) -> f64 {
        self.segments.iter().map(|s| s.area_term()).sum()
    }
    pub fn area(&self) -> f64 {
        self.signed_area().abs()
    }
    pub fn reverse(&mut self) {
        self.segments.reverse();
        for s in &mut self.segments {
            *s = s.reversed();
        }
    }
    pub fn polyline(&self, per_rev: usize) -> Vec<[f64; 2]> {
        let mut v = vec![];
        for s in &self.segments {
            s.sample(&mut v, per_rev);
        }
        v
    }
    fn contains(&self, p: [f64; 2]) -> bool {
        let poly = self.polyline(256);
        let mut inside = false;
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if x > p[0] {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Region {
    /// Counter-clockwise outer boundary.
    pub outer: Loop,
    /// Clockwise inner boundaries.
    pub holes: Vec<Loop>,
}

impl Region {
    pub fn area(&self) -> f64 {
        self.outer.area() - self.holes.iter().map(|h| h.area()).sum::<f64>()
    }
}

type Graph = (Vec<Loop>, Vec<(usize, usize, Segment)>, Vec<[f64; 2]>);

/// Splits non-construction geometry into closed single-entity loops (circles,
/// closed splines) and graph edges between canonical point nodes.
fn graph(sk: &Sketch) -> Result<Graph, SketchError> {
    // Canonicalize points by position so geometrically coincident points join.
    let mut pos: HashMap<&str, [f64; 2]> = HashMap::new();
    let mut canon: HashMap<&str, usize> = HashMap::new();
    let mut reps: Vec<[f64; 2]> = vec![];
    for e in &sk.entities {
        if let Entity::Point { id, x, y, .. } = e {
            pos.insert(id, [*x, *y]);
            let k = reps.iter().position(|r| (r[0] - x).hypot(r[1] - y) < 1e-6).unwrap_or_else(|| {
                reps.push([*x, *y]);
                reps.len() - 1
            });
            canon.insert(id, k);
        }
    }
    let pt = |id: &str| -> Result<(usize, [f64; 2]), SketchError> {
        Ok((*canon.get(id).ok_or_else(|| SketchError::UnknownEntity(id.into()))?, pos[id]))
    };

    let mut loops: Vec<Loop> = vec![];
    // (start node, end node, segment oriented start->end)
    let mut edges: Vec<(usize, usize, Segment)> = vec![];
    for e in &sk.entities {
        if e.is_construction() {
            continue;
        }
        match e {
            Entity::Line { start, end, .. } => {
                let (i, a) = pt(start)?;
                let (j, b) = pt(end)?;
                if i != j {
                    edges.push((i, j, Segment::Line { a, b }));
                }
            }
            Entity::Arc { center, start, end, .. } => {
                let (_, c) = pt(center)?;
                let (i, a) = pt(start)?;
                let (j, b) = pt(end)?;
                if i == j {
                    let r = (a[0] - c[0]).hypot(a[1] - c[1]);
                    loops.push(Loop { segments: vec![Segment::Circle { center: c, radius: r, ccw: true }] });
                } else {
                    edges.push((i, j, Segment::Arc { center: c, a, b, ccw: true }));
                }
            }
            Entity::Circle { center, radius, .. } => {
                let (_, c) = pt(center)?;
                loops.push(Loop { segments: vec![Segment::Circle { center: c, radius: *radius, ccw: true }] });
            }
            Entity::Spline { points, .. } => {
                if points.len() < 2 {
                    return Err(SketchError::Profile(format!("spline `{}` needs at least two points", e.id())));
                }
                let nodes: Vec<(usize, [f64; 2])> = points.iter().map(|p| pt(p)).collect::<Result<_, _>>()?;
                let xy: Vec<[f64; 2]> = nodes.iter().map(|n| n.1).collect();
                let (i, j) = (nodes[0].0, nodes[nodes.len() - 1].0);
                let closed = i == j && nodes.len() > 3;
                let seg = Segment::Spline { poles: spline_poles(&xy, closed) };
                if closed {
                    loops.push(Loop { segments: vec![seg] });
                } else if i != j {
                    edges.push((i, j, seg));
                }
            }
            Entity::Point { .. } => {}
        }
    }
    Ok((loops, edges, reps))
}

/// Ordered chains of connected non-construction segments (open or closed),
/// e.g. sweep paths. Fails when three or more segments meet at a point.
pub(crate) fn paths(sk: &Sketch) -> Result<Vec<Loop>, SketchError> {
    let (mut loops, edges, reps) = graph(sk)?;
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, (i, j, _)) in edges.iter().enumerate() {
        adj.entry(*i).or_default().push(k);
        adj.entry(*j).or_default().push(k);
    }
    if let Some((n, _)) = adj.iter().find(|(_, es)| es.len() > 2) {
        let p = reps[*n];
        return Err(SketchError::Profile(format!("path branches at ({:.4}, {:.4})", p[0], p[1])));
    }
    let mut used = vec![false; edges.len()];
    // Open chains first (start at a dangling end), then remaining closed rings.
    let mut starts: Vec<usize> = adj.iter().filter(|(_, es)| es.len() == 1).map(|(n, _)| *n).collect();
    starts.sort_unstable();
    starts.extend(edges.iter().map(|e| e.0));
    for s in starts {
        let mut node = s;
        let mut segs = vec![];
        while let Some(k) = adj.get(&node).and_then(|es| es.iter().copied().find(|k| !used[*k])) {
            used[k] = true;
            let (i, j, sg) = &edges[k];
            if *i == node {
                segs.push(sg.clone());
                node = *j;
            } else {
                segs.push(sg.reversed());
                node = *i;
            }
        }
        if !segs.is_empty() {
            loops.push(Loop { segments: segs });
        }
    }
    if loops.is_empty() {
        return Err(SketchError::Profile("sketch has no path geometry".into()));
    }
    Ok(loops)
}

pub(crate) fn regions(sk: &Sketch) -> Result<Vec<Region>, SketchError> {
    let (mut loops, edges, reps) = graph(sk)?;
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, (i, j, _)) in edges.iter().enumerate() {
        adj.entry(*i).or_default().push(k);
        adj.entry(*j).or_default().push(k);
    }
    for (node, es) in &adj {
        if es.len() != 2 {
            let p = reps[*node];
            let kind = if es.len() == 1 { "open contour (dangling endpoint)" } else { "branching contour (more than two segments meet)" };
            return Err(SketchError::Profile(format!("{kind} at ({:.4}, {:.4})", p[0], p[1])));
        }
    }
    let mut used = vec![false; edges.len()];
    for k0 in 0..edges.len() {
        if used[k0] {
            continue;
        }
        let mut segs = vec![];
        let (start, mut node, s) = (edges[k0].0, edges[k0].1, edges[k0].2.clone());
        used[k0] = true;
        segs.push(s);
        while node != start {
            let next = adj[&node].iter().copied().find(|k| !used[*k]);
            let Some(k) = next else {
                return Err(SketchError::Profile("could not close contour".into()));
            };
            used[k] = true;
            let (i, j, s) = &edges[k];
            if *i == node {
                segs.push(s.clone());
                node = *j;
            } else {
                segs.push(s.reversed());
                node = *i;
            }
        }
        loops.push(Loop { segments: segs });
    }

    if loops.is_empty() {
        return Err(SketchError::Profile("sketch has no closed contours".into()));
    }

    // Nesting: parent = smallest loop that contains a sample point of this loop.
    let n = loops.len();
    let areas: Vec<f64> = loops.iter().map(|l| l.area()).collect();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let probe = loops[i].segments[0].mid();
        let mut best: Option<usize> = None;
        for j in 0..n {
            if i != j && areas[j] > areas[i] && loops[j].contains(probe) {
                if best.map_or(true, |b| areas[j] < areas[b]) {
                    best = Some(j);
                }
            }
        }
        parent[i] = best;
    }
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = parent[i] {
            d += 1;
            i = p;
        }
        d
    };
    let mut regions = vec![];
    let mut region_of: HashMap<usize, usize> = HashMap::new();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| depth(i));
    for i in order {
        let mut l = loops[i].clone();
        if depth(i) % 2 == 0 {
            if l.signed_area() < 0.0 {
                l.reverse();
            }
            region_of.insert(i, regions.len());
            regions.push(Region { outer: l, holes: vec![] });
        } else {
            if l.signed_area() > 0.0 {
                l.reverse();
            }
            let p = parent[i].unwrap();
            regions[region_of[&p]].holes.push(l);
        }
    }
    let _ = PI;
    Ok(regions)
}
