//! The part document: parameters plus an ordered feature history tree.
//! Serialized as JSON (`.rwpart`) so humans, the GUI and AI agents (via MCP)
//! can all read and edit it.

use rw_sketch::{Sketch, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: &str = "rustworks-part/1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Part {
    #[serde(default = "fmt_default")]
    pub format: String,
    pub name: String,
    #[serde(default = "units_default")]
    pub units: String,
    /// Global design parameters ("equations"), referenced by name in any dimension.
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub features: Vec<Feature>,
}

fn fmt_default() -> String {
    FORMAT.into()
}
fn units_default() -> String {
    "mm".into()
}

impl Part {
    pub fn new(name: &str) -> Part {
        Part { format: FORMAT.into(), name: name.into(), units: "mm".into(), parameters: BTreeMap::new(), features: vec![] }
    }

    pub fn feature(&self, id: &str) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id || f.name == id)
    }

    pub fn feature_mut(&mut self, id: &str) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id || f.name == id)
    }

    pub fn feature_index(&self, id: &str) -> Option<usize> {
        self.features.iter().position(|f| f.id == id || f.name == id)
    }

    /// New unique feature id + display name, e.g. ("f3", "Extrude2").
    pub fn fresh_ids(&self, base: &str) -> (String, String) {
        let mut n = self.features.len() + 1;
        let id = loop {
            let id = format!("f{n}");
            if self.features.iter().all(|f| f.id != id) {
                break id;
            }
            n += 1;
        };
        let mut k = 1;
        let name = loop {
            let nm = format!("{base}{k}");
            if self.features.iter().all(|f| f.name != nm) {
                break nm;
            }
            k += 1;
        };
        (id, name)
    }

    /// Ids of features that reference `id` (directly).
    pub fn dependents(&self, id: &str) -> Vec<String> {
        self.features
            .iter()
            .filter(|f| f.kind.references().iter().any(|r| r == id))
            .map(|f| f.id.clone())
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Feature {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
    #[serde(flatten)]
    pub kind: FeatureKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FeatureKind {
    Sketch(SketchFeature),
    Extrude(Extrude),
    Revolve(Revolve),
    Hole(Hole),
    LinearPattern(LinearPattern),
    CircularPattern(CircularPattern),
    Mirror(Mirror),
    Fillet(Fillet),
    Chamfer(Chamfer),
    Shell(Shell),
    Loft(Loft),
    Import(Import),
    /// Reference plane (construction geometry other features can sketch on).
    Plane(RefPlane),
    Sweep(Sweep),
    Draft(Draft),
}

impl FeatureKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            FeatureKind::Sketch(_) => "sketch",
            FeatureKind::Extrude(_) => "extrude",
            FeatureKind::Revolve(_) => "revolve",
            FeatureKind::Hole(_) => "hole",
            FeatureKind::LinearPattern(_) => "linear_pattern",
            FeatureKind::CircularPattern(_) => "circular_pattern",
            FeatureKind::Mirror(_) => "mirror",
            FeatureKind::Fillet(_) => "fillet",
            FeatureKind::Chamfer(_) => "chamfer",
            FeatureKind::Shell(_) => "shell",
            FeatureKind::Loft(_) => "loft",
            FeatureKind::Import(_) => "import",
            FeatureKind::Plane(_) => "plane",
            FeatureKind::Sweep(_) => "sweep",
            FeatureKind::Draft(_) => "draft",
        }
    }
    pub fn references(&self) -> Vec<String> {
        match self {
            FeatureKind::Extrude(e) => vec![e.sketch.clone()],
            FeatureKind::Revolve(r) => vec![r.sketch.clone()],
            FeatureKind::LinearPattern(p) => p.features.clone(),
            FeatureKind::CircularPattern(p) => p.features.clone(),
            FeatureKind::Loft(l) => l.sketches.clone(),
            FeatureKind::Sweep(s) => vec![s.profile.clone(), s.path.clone()],
            FeatureKind::Sketch(s) => s.plane.feature().into_iter().collect(),
            FeatureKind::Hole(h) => h.plane.feature().into_iter().collect(),
            FeatureKind::Mirror(m) => m.features.iter().cloned().chain(m.plane.feature()).collect(),
            FeatureKind::Plane(p) => p.base.as_ref().and_then(|b| b.feature()).into_iter().collect(),
            FeatureKind::Draft(d) => d.neutral.feature().into_iter().collect(),
            _ => vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StdPlane {
    /// XY plane, normal +Z
    Front,
    /// XZ plane, normal +Y
    Top,
    /// YZ plane, normal +X
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PlaneRef {
    Named(StdPlane),
    Offset {
        base: StdPlane,
        offset: Value,
    },
    Custom {
        origin: [f64; 3],
        normal: [f64; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x_dir: Option<[f64; 3]>,
    },
    /// A reference-plane feature, by id.
    Feature(String),
}

impl PlaneRef {
    /// The reference-plane feature this plane depends on, if any.
    pub fn feature(&self) -> Option<String> {
        match self {
            PlaneRef::Feature(id) => Some(id.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SketchFeature {
    pub plane: PlaneRef,
    #[serde(flatten)]
    pub sketch: Sketch,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Add material (Boss/Base)
    #[default]
    Boss,
    /// Remove material
    Cut,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EndCondition {
    #[default]
    Blind,
    MidPlane,
    ThroughAll,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Extrude {
    pub sketch: String,
    #[serde(default)]
    pub depth: Value,
    #[serde(default, skip_serializing_if = "is_default")]
    pub end: EndCondition,
    #[serde(default, skip_serializing_if = "is_default")]
    pub reverse: bool,
    #[serde(default)]
    pub op: Op,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AxisRef {
    /// Id of a sketch line, or "x_axis" / "y_axis" for the sketch axes.
    Entity(String),
    Explicit { origin: [f64; 2], direction: [f64; 2] },
}

fn full_turn() -> Value {
    Value::Num(360.0)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Revolve {
    pub sketch: String,
    pub axis: AxisRef,
    #[serde(default = "full_turn")]
    pub angle: Value,
    #[serde(default)]
    pub op: Op,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Counterbore {
    pub diameter: Value,
    pub depth: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Countersink {
    pub diameter: Value,
    #[serde(default = "csink_angle")]
    pub angle: Value,
}
fn csink_angle() -> Value {
    Value::Num(90.0)
}

/// Hole Wizard–style hole(s): drilled into the material opposite the plane normal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hole {
    pub plane: PlaneRef,
    /// Hole centers in plane coordinates.
    pub points: Vec<[f64; 2]>,
    pub diameter: Value,
    /// Depth; omitted means through all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterbore: Option<Counterbore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countersink: Option<Countersink>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LinearPattern {
    pub features: Vec<String>,
    pub direction: [f64; 3],
    pub spacing: Value,
    pub count: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction2: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing2: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count2: Option<Value>,
}

fn y_axis() -> [f64; 3] {
    [0.0, 1.0, 0.0]
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CircularPattern {
    pub features: Vec<String>,
    #[serde(default)]
    pub axis_origin: [f64; 3],
    #[serde(default = "y_axis")]
    pub axis_direction: [f64; 3],
    pub count: Value,
    /// Total angle spanned; 360 spaces instances equally around the axis.
    #[serde(default = "full_turn")]
    pub angle: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mirror {
    /// Features to mirror; empty mirrors the whole body.
    #[serde(default)]
    pub features: Vec<String>,
    pub plane: PlaneRef,
}

/// Reference to a body edge: a point on/near it, optionally with the edge's
/// signature (kind + length). Plain points are accepted as input; after every
/// rebuild references are rewritten with the matched edge's midpoint and
/// signature, so they keep following the edge when dimensions change.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EdgeRef {
    Point([f64; 3]),
    Tagged {
        at: [f64; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<f64>,
        /// Position within the body's bounding box (0..1 per axis), used to
        /// tell identical edges apart after the part is resized.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rel: Option<[f64; 3]>,
    },
}

impl EdgeRef {
    pub fn at(&self) -> [f64; 3] {
        match self {
            EdgeRef::Point(p) => *p,
            EdgeRef::Tagged { at, .. } => *at,
        }
    }
}

impl From<[f64; 3]> for EdgeRef {
    fn from(p: [f64; 3]) -> Self {
        EdgeRef::Point(p)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Fillet {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<EdgeRef>,
    /// Fillet every edge of the body.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_edges: bool,
    pub radius: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Chamfer {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<EdgeRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_edges: bool,
    pub distance: Value,
}

/// Hollows the body; faces nearest to `open_faces` points are removed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Shell {
    #[serde(default)]
    pub open_faces: Vec<[f64; 3]>,
    pub thickness: Value,
}

/// Lofted boss/cut through the (single, hole-free) profiles of 2+ sketches, in order.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Loft {
    pub sketches: Vec<String>,
    #[serde(default)]
    pub op: Op,
}

/// Imported STEP geometry, added (boss) or subtracted (cut).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Import {
    pub path: String,
    #[serde(default)]
    pub op: Op,
}

/// Reference plane: `base` offset along its normal and optionally tilted by
/// `angle` degrees about the base plane's in-plane X axis, or a plane through
/// three points.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RefPlane {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<PlaneRef>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<[[f64; 3]; 3]>,
    /// Reverse the normal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip: bool,
}

fn is_zero(v: &Value) -> bool {
    *v == Value::Num(0.0)
}

/// Swept boss/cut: the profile sketch's regions moved along the path sketch's
/// (single) chain of lines/arcs/splines.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sweep {
    pub profile: String,
    pub path: String,
    #[serde(default)]
    pub op: Op,
}

/// Draft: tilts the faces nearest to `faces` points by `angle` degrees,
/// hinged at the neutral plane, opening along its normal (or opposite when
/// `reverse`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Draft {
    pub faces: Vec<[f64; 3]>,
    pub neutral: PlaneRef,
    pub angle: Value,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
}
