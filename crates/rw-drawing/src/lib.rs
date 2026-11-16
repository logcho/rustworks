//! 2D engineering drawings generated from parts and assemblies.
//!
//! A `.rwdrw` document names a source model and the views to show. Building
//! it runs exact hidden-line removal (OpenCASCADE HLR) per view, lays the
//! views out in third- or first-angle projection at a standard scale, adds
//! overall dimensions, hole callouts, view labels, a border and a title
//! block, and produces a `Sheet` of vector lines and text in millimetres.
//! The sheet exports to SVG, DXF, PDF and PNG.

mod export;
mod raster;

pub use export::{to_dxf, to_pdf, to_svg};
pub use raster::to_png;

use anyhow::{anyhow, bail, Context, Result};
use rw_core::kernel::{self, Edge2, Solid, ViewAxes};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DRW_FORMAT: &str = "rustworks-drawing/1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Drawing {
    #[serde(default = "fmt")]
    pub format: String,
    pub name: String,
    /// Part (.rwpart) or assembly (.rwasm), relative to the drawing file's folder.
    pub source: String,
    #[serde(default)]
    pub sheet: SheetSize,
    /// Drawing scale (paper mm per model mm); automatic standard scale when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default)]
    pub projection: Projection,
    #[serde(default = "default_views")]
    pub views: Vec<ViewDef>,
    /// Automatic overall dimensions and hole callouts.
    #[serde(default = "yes")]
    pub dimensions: bool,
    #[serde(default)]
    pub title: TitleBlock,
}

fn fmt() -> String {
    DRW_FORMAT.into()
}
fn yes() -> bool {
    true
}
fn default_views() -> Vec<ViewDef> {
    [ViewKind::Front, ViewKind::Top, ViewKind::Right, ViewKind::Iso].into_iter().map(ViewDef::new).collect()
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SheetSize {
    A4,
    #[default]
    A3,
    A2,
    A1,
    A0,
    Letter,
    Tabloid,
}

impl SheetSize {
    /// Landscape width × height in mm.
    pub fn mm(self) -> (f64, f64) {
        match self {
            SheetSize::A4 => (297.0, 210.0),
            SheetSize::A3 => (420.0, 297.0),
            SheetSize::A2 => (594.0, 420.0),
            SheetSize::A1 => (841.0, 594.0),
            SheetSize::A0 => (1189.0, 841.0),
            SheetSize::Letter => (279.4, 215.9),
            SheetSize::Tabloid => (431.8, 279.4),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    /// ANSI: top view above the front view, right view to its right.
    #[default]
    ThirdAngle,
    /// ISO: top view below the front view, right view to its left.
    FirstAngle,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Front,
    Top,
    Right,
    Left,
    Back,
    Bottom,
    Iso,
}

impl ViewKind {
    pub fn label(self) -> &'static str {
        match self {
            ViewKind::Front => "FRONT",
            ViewKind::Top => "TOP",
            ViewKind::Right => "RIGHT",
            ViewKind::Left => "LEFT",
            ViewKind::Back => "BACK",
            ViewKind::Bottom => "BOTTOM",
            ViewKind::Iso => "ISOMETRIC",
        }
    }
    /// Direction toward the viewer and drawing +X (world is Y-up).
    pub fn axes(self) -> ViewAxes {
        let s = 1.0 / 3f64.sqrt();
        let h = 1.0 / 2f64.sqrt();
        let (dir, x) = match self {
            ViewKind::Front => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
            ViewKind::Back => ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0]),
            ViewKind::Top => ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
            ViewKind::Bottom => ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
            ViewKind::Right => ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            ViewKind::Left => ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ViewKind::Iso => ([s, s, s], [h, 0.0, -h]),
        };
        ViewAxes { dir, x }
    }
    /// Grid cell (column, row) relative to the front view.
    fn cell(self, p: Projection) -> (i32, i32) {
        let f = if p == Projection::FirstAngle { -1 } else { 1 };
        match self {
            ViewKind::Front => (0, 0),
            ViewKind::Top => (0, f),
            ViewKind::Bottom => (0, -f),
            ViewKind::Right => (f, 0),
            ViewKind::Left => (-f, 0),
            ViewKind::Back => (2 * f, 0),
            ViewKind::Iso => (1, 1),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ViewDef {
    #[serde(rename = "type")]
    pub kind: ViewKind,
    /// Show hidden edges as dashed lines (default: yes for orthographic views, no for isometric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_lines: Option<bool>,
    /// Manual shift of the view on the sheet (mm).
    #[serde(default, skip_serializing_if = "is_zero2")]
    pub offset: [f64; 2],
    /// Per-view scale factor relative to the sheet scale (e.g. 0.5 for a smaller isometric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

fn is_zero2(v: &[f64; 2]) -> bool {
    v[0] == 0.0 && v[1] == 0.0
}

impl ViewDef {
    pub fn new(kind: ViewKind) -> ViewDef {
        ViewDef { kind, hidden_lines: None, offset: [0.0; 2], scale: None }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TitleBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawn_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Drawing {
    pub fn new(name: &str, source: &str) -> Drawing {
        Drawing {
            format: fmt(),
            name: name.into(),
            source: source.into(),
            sheet: SheetSize::default(),
            scale: None,
            projection: Projection::default(),
            views: default_views(),
            dimensions: true,
            title: TitleBlock::default(),
        }
    }
}

// ---------- sheet (output) ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pen {
    /// Visible model edges.
    Visible,
    /// Hidden model edges (dashed).
    Hidden,
    /// Dimension, extension and leader lines.
    Thin,
    /// Sheet border and title block frame.
    Border,
}

#[derive(Clone, Debug)]
pub struct SLine {
    pub pts: Vec<[f64; 2]>,
    pub pen: Pen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Clone, Debug)]
pub struct SText {
    /// Baseline anchor point (mm, y up).
    pub at: [f64; 2],
    /// Character height (mm).
    pub size: f64,
    pub text: String,
    pub anchor: Anchor,
    /// Counter-clockwise rotation in degrees.
    pub rotate: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ViewInfo {
    pub kind: ViewKind,
    /// Paper bounding box (mm): min, max.
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub scale: f64,
    pub visible_edges: usize,
    pub hidden_edges: usize,
}

/// A laid-out drawing sheet, in millimetres with the origin at the bottom-left.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub width: f64,
    pub height: f64,
    pub lines: Vec<SLine>,
    pub texts: Vec<SText>,
    pub views: Vec<ViewInfo>,
    pub scale: f64,
    /// Warnings such as views overlapping the title block.
    pub warnings: Vec<String>,
}

// ---------- building ----------

/// Loads the source model as one shape (assemblies as a compound of placed parts).
pub fn load_source(path: &Path) -> Result<(Solid, String)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rwasm" => {
            let m = rw_core::AsmModel::open(path)?;
            let errs = m.out.errors();
            if !errs.is_empty() {
                bail!("assembly has errors: {}", errs.join("; "));
            }
            let solids: Vec<Solid> = m.out.placed_solids()?.into_iter().map(|s| s.1).collect();
            if solids.is_empty() {
                bail!("assembly has no geometry");
            }
            Ok((kernel::compound(&solids), m.asm.name.clone()))
        }
        "rwpart" => {
            let part = rw_core::model::read_part(path)?;
            let out = rw_core::Regenerator::new().regenerate(&part);
            let body = out.body.ok_or_else(|| anyhow!("part `{}` has no solid body", part.name))?;
            Ok((body, part.name))
        }
        _ => bail!("drawing source must be a .rwpart or .rwasm file (got {})", path.display()),
    }
}

pub fn source_path(drawing_file: Option<&Path>, d: &Drawing) -> PathBuf {
    let p = PathBuf::from(&d.source);
    if p.is_absolute() {
        return p;
    }
    let base = drawing_file.and_then(|f| f.parent()).filter(|b| !b.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    base.join(p)
}

struct Projected {
    def: ViewDef,
    vis: Vec<Edge2>,
    hid: Vec<Edge2>,
    lo: [f64; 2],
    hi: [f64; 2],
}

const STD_SCALES: [f64; 15] = [20.0, 10.0, 5.0, 4.0, 2.0, 1.0, 0.5, 0.4, 0.25, 0.2, 0.1, 0.05, 0.02, 0.01, 0.005];
const MARGIN: f64 = 10.0;
const TITLE_W: f64 = 180.0;
const TITLE_H: f64 = 36.0;
const GAP: f64 = 22.0;

pub fn scale_text(s: f64) -> String {
    if s >= 1.0 {
        format!("{}:1", trim(s))
    } else {
        format!("1:{}", trim(1.0 / s))
    }
}

pub fn trim(v: f64) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Builds the sheet for drawing `d` from an already-loaded source shape.
pub fn build_sheet(d: &Drawing, shape: &Solid, model_name: &str, source_label: &str) -> Result<Sheet> {
    if d.views.is_empty() {
        bail!("the drawing has no views");
    }
    let (w, h) = d.sheet.mm();
    // 1. project every view (model units, view plane)
    let mut views: Vec<Projected> = vec![];
    for v in &d.views {
        let (vis, hid) = kernel::hlr(shape, v.kind.axes()).with_context(|| format!("{} view", v.kind.label()))?;
        let show_hidden = v.hidden_lines.unwrap_or(v.kind != ViewKind::Iso);
        let hid = if show_hidden { drop_covered(hid, &vis) } else { vec![] };
        let (lo, hi) = bbox(vis.iter().chain(hid.iter()));
        if !lo[0].is_finite() {
            bail!("{} view is empty", v.kind.label());
        }
        views.push(Projected { def: v.clone(), vis, hid, lo, hi });
    }
    // 2. grid of cells: column widths and row heights (model units). An
    // isometric without an explicit scale does not drive the layout when its
    // cell is shared with orthographic views; it is shrunk to fit instead.
    let cells: Vec<(i32, i32)> = views.iter().map(|v| v.def.kind.cell(d.projection)).collect();
    let ortho_in = |col: Option<i32>, row: Option<i32>| {
        views.iter().zip(&cells).any(|(v, c)| v.def.kind != ViewKind::Iso && col.is_none_or(|x| c.0 == x) && row.is_none_or(|y| c.1 == y))
    };
    let floating: Vec<bool> = views
        .iter()
        .zip(&cells)
        .map(|(v, c)| v.def.kind == ViewKind::Iso && v.def.scale.is_none() && ortho_in(Some(c.0), None) && ortho_in(None, Some(c.1)))
        .collect();
    let mut factors: Vec<f64> = views.iter().map(|v| v.def.scale.unwrap_or(1.0)).collect();
    let cols: Vec<i32> = sorted_unique(cells.iter().map(|c| c.0));
    let rows: Vec<i32> = sorted_unique(cells.iter().map(|c| c.1));
    let col_w = |c: i32, f: &[f64]| (0..views.len()).filter(|&i| cells[i].0 == c && !floating[i]).map(|i| (views[i].hi[0] - views[i].lo[0]) * f[i]).fold(0.0, f64::max);
    let row_h = |r: i32, f: &[f64]| (0..views.len()).filter(|&i| cells[i].1 == r && !floating[i]).map(|i| (views[i].hi[1] - views[i].lo[1]) * f[i]).fold(0.0, f64::max);
    for i in (0..views.len()).filter(|&i| floating[i]) {
        let (cw, ch) = (col_w(cells[i].0, &factors), row_h(cells[i].1, &factors));
        let (vw, vh) = (views[i].hi[0] - views[i].lo[0], views[i].hi[1] - views[i].lo[1]);
        // a little larger than the cell is fine: gaps absorb it
        factors[i] = (cw * 1.15 / vw.max(1e-9)).min(ch * 1.15 / vh.max(1e-9)).min(1.0);
    }
    // floating views do not affect cell sizes, so a snapshot suffices
    let cell_factors = factors.clone();
    let col_w = |c: i32| col_w(c, &cell_factors);
    let row_h = |r: i32| row_h(r, &cell_factors);
    let sum_w: f64 = cols.iter().map(|c| col_w(*c)).sum();
    let sum_h: f64 = rows.iter().map(|r| row_h(*r)).sum();
    // drawing area: inside the border, above the title block band
    let area_lo = [MARGIN + GAP * 0.6, MARGIN + TITLE_H + 24.0];
    let area_hi = [w - MARGIN - GAP * 0.3, h - MARGIN - GAP * 0.3];
    let gaps_w = GAP * (cols.len() as f64 - 1.0) + GAP * 0.5;
    let gaps_h = GAP * (rows.len() as f64 - 1.0) + GAP * 0.5;
    let fit = ((area_hi[0] - area_lo[0] - gaps_w) / sum_w.max(1e-9)).min((area_hi[1] - area_lo[1] - gaps_h) / sum_h.max(1e-9));
    let scale = match d.scale {
        Some(s) if s > 0.0 => s,
        Some(_) => bail!("scale must be positive"),
        None => STD_SCALES.iter().copied().find(|s| *s <= fit).unwrap_or(*STD_SCALES.last().unwrap()),
    };
    // floating isometric views use the largest standard scale that fits their slot
    for i in (0..views.len()).filter(|&i| floating[i]) {
        let want = scale * factors[i];
        let std = STD_SCALES.iter().copied().find(|s| *s <= want * 1.0001).unwrap_or(want);
        factors[i] = std / scale;
    }
    let factor = |v: &Projected| factors[views.iter().position(|x| std::ptr::eq(x, v)).unwrap()];
    // 3. cell centres on paper, layout centred in the drawing area
    let tot_w = sum_w * scale + GAP * (cols.len() as f64 - 1.0);
    let tot_h = sum_h * scale + GAP * (rows.len() as f64 - 1.0);
    let mut x = (area_lo[0] + area_hi[0]) / 2.0 - tot_w / 2.0;
    let mut col_cx = std::collections::HashMap::new();
    for c in &cols {
        let cw = col_w(*c) * scale;
        col_cx.insert(*c, x + cw / 2.0);
        x += cw + GAP;
    }
    let mut y = (area_lo[1] + area_hi[1]) / 2.0 - tot_h / 2.0;
    let mut row_cy = std::collections::HashMap::new();
    for r in &rows {
        let rh = row_h(*r) * scale;
        row_cy.insert(*r, y + rh / 2.0);
        y += rh + GAP;
    }
    let mut sheet = Sheet { width: w, height: h, lines: vec![], texts: vec![], views: vec![], scale, warnings: vec![] };
    // 4. place views
    for (v, cell) in views.iter().zip(&cells) {
        let s = scale * factor(v);
        let c = [col_cx[&cell.0] + v.def.offset[0], row_cy[&cell.1] + v.def.offset[1]];
        let mid = [(v.lo[0] + v.hi[0]) / 2.0, (v.lo[1] + v.hi[1]) / 2.0];
        let map = |p: [f64; 2]| [c[0] + (p[0] - mid[0]) * s, c[1] + (p[1] - mid[1]) * s];
        for e in &v.hid {
            sheet.lines.push(SLine { pts: e.pts.iter().map(|p| map(*p)).collect(), pen: Pen::Hidden });
        }
        for e in &v.vis {
            sheet.lines.push(SLine { pts: e.pts.iter().map(|p| map(*p)).collect(), pen: Pen::Visible });
        }
        let (plo, phi) = (map(v.lo), map(v.hi));
        let mut label = v.def.kind.label().to_string();
        if (factor(v) - 1.0).abs() > 1e-9 {
            label += &format!("  SCALE {}", scale_text(s));
        }
        let label_y = plo[1] - if d.dimensions && v.def.kind != ViewKind::Iso { 17.0 } else { 7.0 };
        sheet.texts.push(SText { at: [(plo[0] + phi[0]) / 2.0, label_y], size: 3.0, text: label, anchor: Anchor::Middle, rotate: 0.0 });
        if d.dimensions && v.def.kind != ViewKind::Iso {
            dimension_view(&mut sheet, v, &map, s);
        }
        if phi[0] > w - MARGIN - TITLE_W && plo[1] < MARGIN + TITLE_H {
            sheet.warnings.push(format!("{} view overlaps the title block; set a smaller scale or move the view", v.def.kind.label()));
        }
        if plo[0] < MARGIN || plo[1] < MARGIN || phi[0] > w - MARGIN || phi[1] > h - MARGIN {
            sheet.warnings.push(format!("{} view runs off the sheet; use a larger sheet or a smaller scale", v.def.kind.label()));
        }
        sheet.views.push(ViewInfo { kind: v.def.kind, min: plo, max: phi, scale: s, visible_edges: v.vis.len(), hidden_edges: v.hid.len() });
    }
    frame(&mut sheet, d, model_name, source_label);
    Ok(sheet)
}

/// Builds the sheet for a drawing file (loads the source model).
pub fn build(d: &Drawing, drawing_file: Option<&Path>) -> Result<Sheet> {
    let src = source_path(drawing_file, d);
    let (shape, name) = load_source(&src).with_context(|| format!("loading {}", src.display()))?;
    build_sheet(d, &shape, &name, &d.source)
}

/// Centre and radius of the circle through three points.
fn circumcircle(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> Option<([f64; 2], f64)> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a[0] * a[0] + a[1] * a[1], b[0] * b[0] + b[1] * b[1], c[0] * c[0] + c[1] * c[1]);
    let x = (a2 * (b[1] - c[1]) + b2 * (c[1] - a[1]) + c2 * (a[1] - b[1])) / d;
    let y = (a2 * (c[0] - b[0]) + b2 * (a[0] - c[0]) + c2 * (b[0] - a[0])) / d;
    Some(([x, y], (a[0] - x).hypot(a[1] - y)))
}

fn sorted_unique(it: impl Iterator<Item = i32>) -> Vec<i32> {
    let mut v: Vec<i32> = it.collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn bbox<'a>(es: impl Iterator<Item = &'a Edge2>) -> ([f64; 2], [f64; 2]) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for e in es {
        for p in &e.pts {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    (lo, hi)
}

fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if l2 > 0.0 { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p[0] - a[0] - ab[0] * t).hypot(p[1] - a[1] - ab[1] * t)
}

/// Removes hidden edges that lie exactly under visible ones (e.g. a back
/// face's outline directly behind the front face's).
fn drop_covered(hid: Vec<Edge2>, vis: &[Edge2]) -> Vec<Edge2> {
    let (lo, hi) = bbox(vis.iter());
    let eps = ((hi[0] - lo[0]).hypot(hi[1] - lo[1]) * 2e-4).max(1e-4);
    let segs: Vec<([f64; 2], [f64; 2])> = vis.iter().flat_map(|e| e.pts.windows(2).map(|w| (w[0], w[1]))).collect();
    hid.into_iter()
        .filter(|e| {
            // sample along the edge, including segment midpoints
            let samples = e.pts.iter().copied().chain(e.pts.windows(2).map(|w| [(w[0][0] + w[1][0]) / 2.0, (w[0][1] + w[1][1]) / 2.0]));
            !samples.into_iter().all(|p| segs.iter().any(|(a, b)| seg_dist(p, *a, *b) <= eps))
        })
        .collect()
}

// ---------- dimensions ----------

const DIM_TEXT: f64 = 3.0;
const ARROW: f64 = 2.5;

fn arrow(sheet: &mut Sheet, tip: [f64; 2], toward: [f64; 2]) {
    let d = [toward[0] - tip[0], toward[1] - tip[1]];
    let l = d[0].hypot(d[1]).max(1e-9);
    let u = [d[0] / l, d[1] / l];
    let n = [-u[1], u[0]];
    let base = [tip[0] + u[0] * ARROW, tip[1] + u[1] * ARROW];
    let a = [base[0] + n[0] * ARROW * 0.33, base[1] + n[1] * ARROW * 0.33];
    let b = [base[0] - n[0] * ARROW * 0.33, base[1] - n[1] * ARROW * 0.33];
    sheet.lines.push(SLine { pts: vec![a, tip, b, a], pen: Pen::Thin });
}

/// Linear dimension between paper points `a` and `b`, drawn at `offset` mm
/// from them along the normal `n` (unit), labelled `text`.
fn linear_dim(sheet: &mut Sheet, a: [f64; 2], b: [f64; 2], n: [f64; 2], offset: f64, text: String) {
    let pa = [a[0] + n[0] * offset, a[1] + n[1] * offset];
    let pb = [b[0] + n[0] * offset, b[1] + n[1] * offset];
    // extension lines with a small gap at the model and a short overshoot
    for (p, q) in [(a, pa), (b, pb)] {
        sheet.lines.push(SLine { pts: vec![[p[0] + n[0] * 1.5, p[1] + n[1] * 1.5], [q[0] + n[0] * 1.5, q[1] + n[1] * 1.5]], pen: Pen::Thin });
    }
    sheet.lines.push(SLine { pts: vec![pa, pb], pen: Pen::Thin });
    arrow(sheet, pa, pb);
    arrow(sheet, pb, pa);
    let mid = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
    let ang = (pb[1] - pa[1]).atan2(pb[0] - pa[0]).to_degrees();
    let rot = if ang > 90.0 || ang <= -90.0 { ang + 180.0 } else { ang };
    // text sits just above the dimension line in reading orientation
    let up = [-(rot.to_radians().sin()), rot.to_radians().cos()];
    let _ = n;
    sheet.texts.push(SText { at: [mid[0] + up[0] * 1.0, mid[1] + up[1] * 1.0], size: DIM_TEXT, text, anchor: Anchor::Middle, rotate: rot });
}

/// Overall width/height dimensions and diameter callouts for one view.
fn dimension_view(sheet: &mut Sheet, v: &Projected, map: &dyn Fn([f64; 2]) -> [f64; 2], s: f64) {
    let (lo, hi) = (v.lo, v.hi);
    let (plo, phi) = (map(lo), map(hi));
    let wv = hi[0] - lo[0];
    let hv = hi[1] - lo[1];
    let side_dims = matches!(v.def.kind, ViewKind::Front | ViewKind::Back | ViewKind::Top | ViewKind::Bottom);
    if wv > 1e-6 {
        linear_dim(sheet, [plo[0], plo[1]], [phi[0], plo[1]], [0.0, -1.0], 8.0, trim(wv));
    }
    if hv > 1e-6 && side_dims {
        linear_dim(sheet, [plo[0], plo[1]], [plo[0], phi[1]], [-1.0, 0.0], 8.0, trim(hv));
    }
    // diameter callouts: arcs reassembled by centre/radius into full circles,
    // then grouped by size (a circle is often split into two half arcs)
    let mut arcs: Vec<([f64; 2], f64, f64)> = vec![]; // (centre, radius, swept angle)
    for e in v.vis.iter().filter(|e| e.kind == "circle" && e.pts.len() >= 3) {
        let (p, q, r) = (e.pts[0], e.pts[e.pts.len() / 2], e.pts[e.pts.len() - 1]);
        let Some((c, rad)) = circumcircle(p, q, r).or_else(|| {
            // closed edge: first == last; use first, quarter and mid points
            circumcircle(p, e.pts[e.pts.len() / 4], q)
        }) else {
            continue;
        };
        let mut sweep = 0.0;
        for w in e.pts.windows(2) {
            let a0 = (w[0][1] - c[1]).atan2(w[0][0] - c[0]);
            let a1 = (w[1][1] - c[1]).atan2(w[1][0] - c[0]);
            let mut d = a1 - a0;
            if d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            } else if d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            sweep += d.abs();
        }
        let tol = rad * 1e-3 + 1e-6;
        match arcs.iter_mut().find(|x| (x.0[0] - c[0]).hypot(x.0[1] - c[1]) < tol && (x.1 - rad).abs() < tol) {
            Some(x) => x.2 += sweep,
            None => arcs.push((c, rad, sweep)),
        }
    }
    let mut groups: Vec<(f64, [f64; 2], usize)> = vec![]; // (diameter, first centre, count)
    for (c, rad, sweep) in arcs {
        if sweep < std::f64::consts::TAU * 0.99 {
            continue; // partial arcs (fillets, slots) are not holes
        }
        let key = (rad * 2.0 * 100.0).round() / 100.0;
        match groups.iter_mut().find(|g| (g.0 - key).abs() < 1e-6) {
            Some(g) => g.2 += 1,
            None => groups.push((key, c, 1)),
        }
    }
    groups.sort_by(|a, b| b.0.total_cmp(&a.0));
    // Each callout gets its own leader direction (so concentric circles do
    // not share a path) and a short leader ending in a horizontal shoulder.
    const DIRS: [f64; 6] = [45.0, 135.0, 22.0, 158.0, 68.0, 112.0];
    let (sw, sh) = (sheet.width, sheet.height);
    for (k, (dia, c, n)) in groups.iter().take(6).enumerate() {
        let pc = map(*c);
        let r = dia / 2.0 * s;
        let a = DIRS[k % DIRS.len()].to_radians();
        let dir = [a.cos(), a.sin()];
        let on = [pc[0] + dir[0] * r, pc[1] + dir[1] * r];
        let len = 9.0 + 5.0 * k as f64;
        let mut elbow = [on[0] + dir[0] * len, on[1] + dir[1] * len];
        elbow[1] = elbow[1].min(sh - MARGIN - 4.0);
        let right = dir[0] >= 0.0;
        let text = if *n > 1 { format!("{n}× Ø{}", trim(*dia)) } else { format!("Ø{}", trim(*dia)) };
        let tw = text.chars().count() as f64 * DIM_TEXT * 0.62;
        let mut tail = [elbow[0] + if right { 3.0 } else { -3.0 }, elbow[1]];
        // keep the label inside the border
        if right && tail[0] + tw > sw - MARGIN - 2.0 {
            tail[0] = sw - MARGIN - 2.0 - tw;
        }
        if !right && tail[0] - tw < MARGIN + 2.0 {
            tail[0] = MARGIN + 2.0 + tw;
        }
        sheet.lines.push(SLine { pts: vec![on, elbow, tail], pen: Pen::Thin });
        arrow(sheet, on, elbow);
        sheet.texts.push(SText {
            at: [tail[0] + if right { 0.8 } else { -0.8 }, tail[1] + 0.8],
            size: DIM_TEXT,
            text,
            anchor: if right { Anchor::Start } else { Anchor::End },
            rotate: 0.0,
        });
    }
}

// ---------- border and title block ----------

fn rect(sheet: &mut Sheet, lo: [f64; 2], hi: [f64; 2], pen: Pen) {
    sheet.lines.push(SLine { pts: vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]], lo], pen });
}

fn frame(sheet: &mut Sheet, d: &Drawing, model_name: &str, source: &str) {
    let (w, h) = (sheet.width, sheet.height);
    rect(sheet, [MARGIN, MARGIN], [w - MARGIN, h - MARGIN], Pen::Border);
    let x0 = w - MARGIN - TITLE_W;
    let y0 = MARGIN;
    rect(sheet, [x0, y0], [w - MARGIN, y0 + TITLE_H], Pen::Border);
    // rows: title (top, tall), then two rows of fields
    let r1 = y0 + 12.0;
    let r2 = y0 + 24.0;
    sheet.lines.push(SLine { pts: vec![[x0, r2], [w - MARGIN, r2]], pen: Pen::Thin });
    sheet.lines.push(SLine { pts: vec![[x0, r1], [w - MARGIN, r1]], pen: Pen::Thin });
    let cols = [x0, x0 + 60.0, x0 + 105.0, x0 + 140.0, w - MARGIN];
    for c in &cols[1..4] {
        sheet.lines.push(SLine { pts: vec![[*c, y0], [*c, r2]], pen: Pen::Thin });
    }
    let title = d.title.title.clone().unwrap_or_else(|| model_name.to_uppercase());
    sheet.texts.push(SText { at: [x0 + 3.0, r2 + 4.0], size: 5.0, text: title, anchor: Anchor::Start, rotate: 0.0 });
    sheet.texts.push(SText { at: [w - MARGIN - 3.0, r2 + 4.0], size: 2.5, text: d.name.clone(), anchor: Anchor::End, rotate: 0.0 });
    let field = |sheet: &mut Sheet, x: f64, y: f64, label: &str, value: String| {
        sheet.texts.push(SText { at: [x + 1.5, y + 8.2], size: 1.8, text: label.to_string(), anchor: Anchor::Start, rotate: 0.0 });
        sheet.texts.push(SText { at: [x + 1.5, y + 2.5], size: 3.0, text: value, anchor: Anchor::Start, rotate: 0.0 });
    };
    let proj = if d.projection == Projection::ThirdAngle { "THIRD ANGLE" } else { "FIRST ANGLE" };
    field(sheet, cols[0], r1, "SOURCE", short(source, 30));
    field(sheet, cols[1], r1, "SCALE", scale_text(sheet.scale));
    field(sheet, cols[2], r1, "UNITS", "mm".into());
    field(sheet, cols[3], r1, "SHEET", format!("{:?}", d.sheet).to_uppercase());
    field(sheet, cols[0], y0, "DRAWN BY", d.title.drawn_by.clone().unwrap_or_else(|| "RustWorks".into()));
    field(sheet, cols[1], y0, "MATERIAL", d.title.material.clone().unwrap_or_else(|| "-".into()));
    field(sheet, cols[2], y0, "PROJECTION", proj.into());
    field(sheet, cols[3], y0, "DATE", today());
    if let Some(n) = &d.title.notes {
        for (i, line) in n.lines().enumerate() {
            sheet.texts.push(SText { at: [MARGIN + 4.0, MARGIN + 4.0 + 5.0 * (n.lines().count() - 1 - i) as f64], size: 3.0, text: line.to_string(), anchor: Anchor::Start, rotate: 0.0 });
        }
    }
}

fn short(s: &str, n: usize) -> String {
    let name = Path::new(s).file_name().and_then(|f| f.to_str()).unwrap_or(s);
    if name.chars().count() <= n { name.to_string() } else { format!("{}…", name.chars().take(n - 1).collect::<String>()) }
}

/// Current UTC date as YYYY-MM-DD (no external time crate).
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = secs.div_euclid(86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- files ----------

pub fn read_drawing(path: &Path) -> Result<Drawing> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_drawing(path: &Path, d: &Drawing) -> Result<()> {
    let json = serde_json::to_string_pretty(d)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("rwdrw.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Writes the sheet in the format given by the file extension (svg, dxf, pdf, png).
pub fn export(sheet: &Sheet, path: &Path) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let bytes = match ext.as_str() {
        "svg" => to_svg(sheet).into_bytes(),
        "dxf" => to_dxf(sheet).into_bytes(),
        "pdf" => to_pdf(sheet),
        "png" => to_png(sheet, 2400),
        _ => bail!("unsupported drawing format `.{ext}` (use svg, dxf, pdf or png)"),
    };
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests;
