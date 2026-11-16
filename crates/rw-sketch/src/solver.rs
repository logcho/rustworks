//! Damped least-squares (Levenberg–Marquardt) geometric constraint solver.
//!
//! Variables are point coordinates and circle radii. Each constraint becomes one
//! or more scalar residuals. After solving, the rank of the Jacobian gives the
//! remaining degrees of freedom (fully defined vs. under-defined), mirroring the
//! sketch status shown by SOLIDWORKS.

use crate::{Constraint, Entity, Scope, Sketch, SketchError};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolveStatus {
    FullyDefined,
    UnderDefined,
    /// Constraints could not all be satisfied (conflicting / over-defined).
    OverDefined,
}

#[derive(Debug, Clone, Serialize)]
pub struct SolveReport {
    pub status: SolveStatus,
    pub converged: bool,
    pub dof: usize,
    pub redundant: usize,
    pub iterations: usize,
    pub max_residual: f64,
}

#[derive(Clone, Copy)]
enum Rad {
    Circle(usize), // circle index
    Arc(usize, usize), // center point, start point
}

#[derive(Clone, Copy)]
enum C {
    Coinc(usize, usize),
    Horiz(usize, usize),
    Vert(usize, usize),
    Dist(usize, usize, f64),
    HDist(usize, usize, f64),
    VDist(usize, usize, f64),
    Radius(Rad, f64),
    Angle((usize, usize), (usize, usize), f64),
    Parallel((usize, usize), (usize, usize)),
    Perp((usize, usize), (usize, usize)),
    EqLen((usize, usize), (usize, usize)),
    EqRad(Rad, Rad),
    TanLC((usize, usize), usize, Rad),
    TanCC(usize, Rad, usize, Rad, bool),
    OnLine(usize, (usize, usize)),
    /// Signed point-to-line distance.
    LineOffset(usize, (usize, usize), f64),
    OnCircle(usize, usize, Rad),
    Mid(usize, (usize, usize)),
    Sym(usize, usize, (usize, usize)),
    FixX(usize, f64),
    FixY(usize, f64),
}

struct System {
    pts0: Vec<[f64; 2]>,
    pt_var: Vec<Option<usize>>,
    rad0: Vec<f64>,
    rad_var: Vec<usize>,
    cons: Vec<C>,
    nvars: usize,
}

impl System {
    #[inline]
    fn p(&self, i: usize, x: &[f64]) -> [f64; 2] {
        match self.pt_var[i] {
            Some(v) => [x[v], x[v + 1]],
            None => self.pts0[i],
        }
    }
    #[inline]
    fn r(&self, r: Rad, x: &[f64]) -> f64 {
        match r {
            Rad::Circle(c) => x[self.rad_var[c]],
            Rad::Arc(c, s) => dist(self.p(c, x), self.p(s, x)),
        }
    }
    fn residuals(&self, x: &[f64], out: &mut Vec<f64>) {
        out.clear();
        let dir = |l: (usize, usize)| sub(self.p(l.1, x), self.p(l.0, x));
        for c in &self.cons {
            match *c {
                C::Coinc(a, b) => {
                    let d = sub(self.p(a, x), self.p(b, x));
                    out.push(d[0]);
                    out.push(d[1]);
                }
                C::Horiz(a, b) => out.push(self.p(b, x)[1] - self.p(a, x)[1]),
                C::Vert(a, b) => out.push(self.p(b, x)[0] - self.p(a, x)[0]),
                C::Dist(a, b, d) => out.push(dist(self.p(a, x), self.p(b, x)) - d),
                C::HDist(a, b, d) => out.push((self.p(b, x)[0] - self.p(a, x)[0]).abs() - d),
                C::VDist(a, b, d) => out.push((self.p(b, x)[1] - self.p(a, x)[1]).abs() - d),
                C::Radius(r, v) => out.push(self.r(r, x) - v),
                C::Angle(l1, l2, ang) => {
                    let (u, v) = (dir(l1), dir(l2));
                    let a = cross(u, v).atan2(dot(u, v));
                    out.push(wrap(a - ang));
                }
                C::Parallel(l1, l2) => {
                    let (u, v) = (dir(l1), dir(l2));
                    out.push(cross(u, v) / (norm(u) * norm(v)).max(1e-12));
                }
                C::Perp(l1, l2) => {
                    let (u, v) = (dir(l1), dir(l2));
                    out.push(dot(u, v) / (norm(u) * norm(v)).max(1e-12));
                }
                C::EqLen(l1, l2) => out.push(norm(dir(l1)) - norm(dir(l2))),
                C::EqRad(a, b) => out.push(self.r(a, x) - self.r(b, x)),
                C::TanLC(l, c, r) => {
                    out.push(line_dist(self.p(c, x), self.p(l.0, x), self.p(l.1, x)).abs() - self.r(r, x))
                }
                C::TanCC(c1, r1, c2, r2, internal) => {
                    let d = dist(self.p(c1, x), self.p(c2, x));
                    let (a, b) = (self.r(r1, x), self.r(r2, x));
                    out.push(if internal { d - (a - b).abs() } else { d - (a + b) });
                }
                C::OnLine(p, l) => out.push(line_dist(self.p(p, x), self.p(l.0, x), self.p(l.1, x))),
                C::LineOffset(p, l, d) => out.push(line_dist(self.p(p, x), self.p(l.0, x), self.p(l.1, x)) - d),
                C::OnCircle(p, c, r) => out.push(dist(self.p(p, x), self.p(c, x)) - self.r(r, x)),
                C::Mid(p, l) => {
                    let (a, b, m) = (self.p(l.0, x), self.p(l.1, x), self.p(p, x));
                    out.push(m[0] - 0.5 * (a[0] + b[0]));
                    out.push(m[1] - 0.5 * (a[1] + b[1]));
                }
                C::Sym(a, b, l) => {
                    let (pa, pb) = (self.p(a, x), self.p(b, x));
                    let m = [0.5 * (pa[0] + pb[0]), 0.5 * (pa[1] + pb[1])];
                    out.push(line_dist(m, self.p(l.0, x), self.p(l.1, x)));
                    let d = dir(l);
                    out.push(dot(sub(pb, pa), d) / norm(d).max(1e-12));
                }
                C::FixX(p, v) => out.push(self.p(p, x)[0] - v),
                C::FixY(p, v) => out.push(self.p(p, x)[1] - v),
            }
        }
    }
}

fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}
fn dot(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn norm(a: [f64; 2]) -> f64 {
    a[0].hypot(a[1])
}
fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    norm(sub(a, b))
}
/// Signed distance from p to infinite line ab.
fn line_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = sub(b, a);
    cross(d, sub(p, a)) / norm(d).max(1e-12)
}
fn wrap(a: f64) -> f64 {
    let t = std::f64::consts::TAU;
    let mut a = a % t;
    if a > std::f64::consts::PI {
        a -= t;
    }
    if a < -std::f64::consts::PI {
        a += t;
    }
    a
}

fn build(sk: &Sketch, scope: &Scope) -> Result<System, SketchError> {
    let idx = sk.index();
    let mut pt_of: HashMap<&str, usize> = HashMap::new();
    let mut circ_of: HashMap<&str, usize> = HashMap::new();
    let mut pts0 = vec![];
    let mut pt_var = vec![];
    let mut rad0 = vec![];
    let mut rad_var = vec![];
    let mut nvars = 0;
    for e in &sk.entities {
        match e {
            Entity::Point { id, x, y, fixed } => {
                pt_of.insert(id, pts0.len());
                pts0.push([*x, *y]);
                if *fixed {
                    pt_var.push(None);
                } else {
                    pt_var.push(Some(nvars));
                    nvars += 2;
                }
            }
            Entity::Circle { id, radius, .. } => {
                circ_of.insert(id, rad0.len());
                rad0.push(*radius);
                rad_var.push(nvars);
                nvars += 1;
            }
            _ => {}
        }
    }
    let get = |id: &str| -> Result<&Entity, SketchError> {
        idx.get(id).map(|i| &sk.entities[*i]).ok_or_else(|| SketchError::UnknownEntity(id.into()))
    };
    let point = |id: &str| -> Result<usize, SketchError> {
        get(id)?;
        pt_of.get(id).copied().ok_or_else(|| SketchError::WrongType(id.into(), "point"))
    };
    let line = |id: &str| -> Result<(usize, usize), SketchError> {
        match get(id)? {
            Entity::Line { start, end, .. } => Ok((point(start)?, point(end)?)),
            _ => Err(SketchError::WrongType(id.into(), "line")),
        }
    };
    // center + radius reference of a circle or arc
    let round = |id: &str| -> Result<(usize, Rad), SketchError> {
        match get(id)? {
            Entity::Circle { center, .. } => Ok((point(center)?, Rad::Circle(circ_of[id]))),
            Entity::Arc { center, start, .. } => {
                let c = point(center)?;
                Ok((c, Rad::Arc(c, point(start)?)))
            }
            _ => Err(SketchError::WrongType(id.into(), "circle or arc")),
        }
    };
    let num = |v: &crate::Value| -> Result<f64, SketchError> { Ok(scope.eval(v)?) };
    let p0 = |i: usize| pts0[i];
    let r0 = |r: Rad| match r {
        Rad::Circle(c) => rad0[c],
        Rad::Arc(c, s) => dist(pts0[c], pts0[s]),
    };

    let mut cons = vec![];
    // Implicit: arc end lies on arc circle.
    for e in &sk.entities {
        if let Entity::Arc { center, start, end, .. } = e {
            let (c, s, en) = (point(center)?, point(start)?, point(end)?);
            cons.push(C::OnCircle(en, c, Rad::Arc(c, s)));
        }
    }
    for c in &sk.constraints {
        match c {
            Constraint::Coincident { a, b } => {
                let ea = get(a)?;
                let eb = get(b)?;
                match (ea, eb) {
                    (Entity::Point { .. }, Entity::Point { .. }) => cons.push(C::Coinc(point(a)?, point(b)?)),
                    (Entity::Point { .. }, Entity::Line { .. }) => cons.push(C::OnLine(point(a)?, line(b)?)),
                    (Entity::Line { .. }, Entity::Point { .. }) => cons.push(C::OnLine(point(b)?, line(a)?)),
                    (Entity::Point { .. }, _) => {
                        let (cc, r) = round(b)?;
                        cons.push(C::OnCircle(point(a)?, cc, r))
                    }
                    (_, Entity::Point { .. }) => {
                        let (cc, r) = round(a)?;
                        cons.push(C::OnCircle(point(b)?, cc, r))
                    }
                    _ => return Err(SketchError::WrongType(a.clone(), "point")),
                }
            }
            Constraint::Horizontal { line: l } => match get(l)? {
                Entity::Line { .. } => {
                    let (a, b) = line(l)?;
                    cons.push(C::Horiz(a, b))
                }
                _ => return Err(SketchError::WrongType(l.clone(), "line")),
            },
            Constraint::Vertical { line: l } => {
                let (a, b) = line(l)?;
                cons.push(C::Vert(a, b))
            }
            Constraint::Distance { a, b, value } => {
                let v = num(value)?;
                match b {
                    None => match get(a)? {
                        Entity::Line { .. } => {
                            let (p, q) = line(a)?;
                            cons.push(C::Dist(p, q, v))
                        }
                        Entity::Circle { .. } | Entity::Arc { .. } => {
                            let (_, r) = round(a)?;
                            cons.push(C::Radius(r, v / 2.0))
                        }
                        _ => return Err(SketchError::WrongType(a.clone(), "line")),
                    },
                    Some(b) => match (get(a)?, get(b)?) {
                        (Entity::Point { .. }, Entity::Point { .. }) => cons.push(C::Dist(point(a)?, point(b)?, v)),
                        (Entity::Point { .. }, Entity::Line { .. }) | (Entity::Line { .. }, Entity::Point { .. }) => {
                            // point-line distance, keep current side
                            let (p, l) = if pt_of.contains_key(a.as_str()) { (point(a)?, line(b)?) } else { (point(b)?, line(a)?) };
                            let side = line_dist(p0(p), p0(l.0), p0(l.1)).signum();
                            let side = if side == 0.0 { 1.0 } else { side };
                            cons.push(C::LineOffset(p, l, v * side));
                        }
                        (Entity::Line { .. }, Entity::Line { .. }) => {
                            // distance between parallel lines: start of b to line a
                            let la = line(a)?;
                            let lb = line(b)?;
                            let side = line_dist(p0(lb.0), p0(la.0), p0(la.1)).signum();
                            let side = if side == 0.0 { 1.0 } else { side };
                            cons.push(C::Parallel(la, lb));
                            cons.push(C::LineOffset(lb.0, la, v * side));
                        }
                        _ => {
                            // center-to-center for circles/arcs/points
                            let ca = round(a).map(|r| r.0).or_else(|_| point(a))?;
                            let cb = round(b).map(|r| r.0).or_else(|_| point(b))?;
                            cons.push(C::Dist(ca, cb, v))
                        }
                    },
                }
            }
            Constraint::HorizontalDistance { a, b, value } => {
                let pa = round(a).map(|r| r.0).or_else(|_| point(a))?;
                let pb = round(b).map(|r| r.0).or_else(|_| point(b))?;
                cons.push(C::HDist(pa, pb, num(value)?))
            }
            Constraint::VerticalDistance { a, b, value } => {
                let pa = round(a).map(|r| r.0).or_else(|_| point(a))?;
                let pb = round(b).map(|r| r.0).or_else(|_| point(b))?;
                cons.push(C::VDist(pa, pb, num(value)?))
            }
            Constraint::Radius { entity, value } => cons.push(C::Radius(round(entity)?.1, num(value)?)),
            Constraint::Diameter { entity, value } => cons.push(C::Radius(round(entity)?.1, num(value)? / 2.0)),
            Constraint::Angle { a, b, value } => {
                cons.push(C::Angle(line(a)?, line(b)?, num(value)?.to_radians()))
            }
            Constraint::Parallel { a, b } => cons.push(C::Parallel(line(a)?, line(b)?)),
            Constraint::Perpendicular { a, b } => cons.push(C::Perp(line(a)?, line(b)?)),
            Constraint::Equal { a, b } => match (line(a), line(b)) {
                (Ok(la), Ok(lb)) => cons.push(C::EqLen(la, lb)),
                _ => cons.push(C::EqRad(round(a)?.1, round(b)?.1)),
            },
            Constraint::Tangent { a, b } => match (line(a), line(b)) {
                (Ok(l), Err(_)) => {
                    let (c, r) = round(b)?;
                    cons.push(C::TanLC(l, c, r))
                }
                (Err(_), Ok(l)) => {
                    let (c, r) = round(a)?;
                    cons.push(C::TanLC(l, c, r))
                }
                (Err(_), Err(_)) => {
                    let (c1, r1) = round(a)?;
                    let (c2, r2) = round(b)?;
                    let internal = dist(p0(c1), p0(c2)) < r0(r1).max(r0(r2));
                    cons.push(C::TanCC(c1, r1, c2, r2, internal))
                }
                _ => return Err(SketchError::WrongType(b.clone(), "circle or arc")),
            },
            Constraint::Concentric { a, b } => {
                let ca = round(a).map(|r| r.0).or_else(|_| point(a))?;
                let cb = round(b).map(|r| r.0).or_else(|_| point(b))?;
                cons.push(C::Coinc(ca, cb))
            }
            Constraint::PointOnLine { point: p, line: l } => cons.push(C::OnLine(point(p)?, line(l)?)),
            Constraint::PointOnCircle { point: p, circle } => {
                let (c, r) = round(circle)?;
                cons.push(C::OnCircle(point(p)?, c, r))
            }
            Constraint::Midpoint { point: p, line: l } => cons.push(C::Mid(point(p)?, line(l)?)),
            Constraint::Symmetric { a, b, about } => {
                let pa = round(a).map(|r| r.0).or_else(|_| point(a))?;
                let pb = round(b).map(|r| r.0).or_else(|_| point(b))?;
                cons.push(C::Sym(pa, pb, line(about)?))
            }
            Constraint::Fix { point: p, x, y } => {
                let pi = point(p)?;
                let cur = p0(pi);
                cons.push(C::FixX(pi, match x { Some(v) => num(v)?, None => cur[0] }));
                cons.push(C::FixY(pi, match y { Some(v) => num(v)?, None => cur[1] }));
            }
        }
    }
    Ok(System { pts0, pt_var, rad0, rad_var, cons, nvars })
}

pub fn solve(sk: &mut Sketch, scope: &Scope) -> Result<SolveReport, SketchError> {
    let sys = build(sk, scope)?;
    let n = sys.nvars;
    let mut x = vec![0.0; n];
    for (i, v) in sys.pt_var.iter().enumerate() {
        if let Some(v) = v {
            x[*v] = sys.pts0[i][0];
            x[*v + 1] = sys.pts0[i][1];
        }
    }
    for (i, v) in sys.rad_var.iter().enumerate() {
        x[*v] = sys.rad0[i];
    }

    let mut r = Vec::new();
    sys.residuals(&x, &mut r);
    let m = r.len();
    let cost = |r: &[f64]| r.iter().map(|v| v * v).sum::<f64>();
    let mut c0 = cost(&r);
    let mut lambda = 1e-3;
    let mut iters = 0;
    let mut jac = vec![0.0; m * n];
    let mut rp = Vec::with_capacity(m);
    let mut rm = Vec::with_capacity(m);
    let tol = 1e-20;

    while c0 > tol && iters < 200 && n > 0 {
        iters += 1;
        jacobian(&sys, &x, &mut jac, m, n, &mut rp, &mut rm);
        // Normal equations
        let mut a = vec![0.0; n * n];
        let mut g = vec![0.0; n];
        for k in 0..m {
            let row = &jac[k * n..(k + 1) * n];
            let nz: Vec<usize> = (0..n).filter(|&j| row[j] != 0.0).collect();
            for &i in &nz {
                g[i] += row[i] * r[k];
                for &j in &nz {
                    a[i * n + j] += row[i] * row[j];
                }
            }
        }
        let mut improved = false;
        for _ in 0..12 {
            let mut aa = a.clone();
            for i in 0..n {
                aa[i * n + i] += lambda * (a[i * n + i] + 1.0);
            }
            let mut rhs: Vec<f64> = g.iter().map(|v| -v).collect();
            if !lin_solve(&mut aa, &mut rhs, n) {
                lambda *= 10.0;
                continue;
            }
            let xn: Vec<f64> = x.iter().zip(&rhs).map(|(a, b)| a + b).collect();
            let mut rn = Vec::new();
            sys.residuals(&xn, &mut rn);
            let cn = cost(&rn);
            if cn < c0 {
                let step: f64 = rhs.iter().map(|v| v * v).sum::<f64>().sqrt();
                x = xn;
                r = rn;
                let rel = (c0 - cn) / c0.max(1e-300);
                c0 = cn;
                lambda = (lambda / 3.0).max(1e-12);
                improved = true;
                if step < 1e-14 || rel < 1e-16 {
                    iters = 200;
                }
                break;
            }
            lambda *= 4.0;
        }
        if !improved {
            break;
        }
    }

    let max_residual = r.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    let converged = max_residual < 1e-7;

    // Degree-of-freedom analysis from Jacobian rank at the solution.
    let rank = if n > 0 && m > 0 {
        jacobian(&sys, &x, &mut jac, m, n, &mut rp, &mut rm);
        matrix_rank(&mut jac.clone(), m, n)
    } else {
        0
    };
    let dof = n - rank;
    let redundant = m - rank.min(m);
    let status = if !converged {
        SolveStatus::OverDefined
    } else if dof == 0 {
        SolveStatus::FullyDefined
    } else {
        SolveStatus::UnderDefined
    };

    // Write back
    let mut pi = 0;
    let mut ci = 0;
    for e in &mut sk.entities {
        match e {
            Entity::Point { x: px, y: py, .. } => {
                if let Some(v) = sys.pt_var[pi] {
                    *px = clean(x[v]);
                    *py = clean(x[v + 1]);
                }
                pi += 1;
            }
            Entity::Circle { radius, .. } => {
                *radius = clean(x[sys.rad_var[ci]]);
                ci += 1;
            }
            _ => {}
        }
    }
    Ok(SolveReport { status, converged, dof, redundant, iterations: iters, max_residual })
}

/// Snap values within 1e-10 of a "nice" number to remove solver fuzz in saved files.
fn clean(v: f64) -> f64 {
    let r = (v * 1e6).round() / 1e6;
    if (r - v).abs() < 1e-10 { r } else { v }
}

fn jacobian(sys: &System, x: &[f64], jac: &mut [f64], m: usize, n: usize, rp: &mut Vec<f64>, rm: &mut Vec<f64>) {
    let mut xx = x.to_vec();
    for j in 0..n {
        let h = 1e-7 * (1.0 + x[j].abs());
        xx[j] = x[j] + h;
        sys.residuals(&xx, rp);
        xx[j] = x[j] - h;
        sys.residuals(&xx, rm);
        xx[j] = x[j];
        for k in 0..m {
            jac[k * n + j] = (rp[k] - rm[k]) / (2.0 * h);
        }
    }
}

fn lin_solve(a: &mut [f64], b: &mut [f64], n: usize) -> bool {
    for col in 0..n {
        let mut piv = col;
        for r in col + 1..n {
            if a[r * n + col].abs() > a[piv * n + col].abs() {
                piv = r;
            }
        }
        if a[piv * n + col].abs() < 1e-300 {
            return false;
        }
        if piv != col {
            for k in 0..n {
                a.swap(col * n + k, piv * n + k);
            }
            b.swap(col, piv);
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let f = a[r * n + col] / d;
            if f != 0.0 {
                for k in col..n {
                    a[r * n + k] -= f * a[col * n + k];
                }
                b[r] -= f * b[col];
            }
        }
    }
    for col in (0..n).rev() {
        let mut s = b[col];
        for k in col + 1..n {
            s -= a[col * n + k] * b[k];
        }
        b[col] = s / a[col * n + col];
    }
    b.iter().all(|v| v.is_finite())
}

fn matrix_rank(a: &mut [f64], m: usize, n: usize) -> usize {
    let scale = a.iter().fold(0.0f64, |s, v| s.max(v.abs())).max(1.0);
    let eps = 1e-6 * scale;
    let mut rank = 0;
    let mut row = 0;
    for col in 0..n {
        if row >= m {
            break;
        }
        let mut piv = row;
        for r in row + 1..m {
            if a[r * n + col].abs() > a[piv * n + col].abs() {
                piv = r;
            }
        }
        if a[piv * n + col].abs() <= eps {
            continue;
        }
        for k in 0..n {
            a.swap(row * n + k, piv * n + k);
        }
        let d = a[row * n + col];
        for r in row + 1..m {
            let f = a[r * n + col] / d;
            if f != 0.0 {
                for k in col..n {
                    a[r * n + k] -= f * a[row * n + k];
                }
            }
        }
        row += 1;
        rank += 1;
    }
    rank
}
