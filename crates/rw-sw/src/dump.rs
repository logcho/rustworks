//! The `.swdump.json` format written by `tools/solidworks/rw_sw_bridge.py`.
//!
//! The bridge copies values out of the SOLIDWORKS API as they are: lengths in
//! metres, angles in radians, enum values as SOLIDWORKS integers, transforms
//! as `MathTransform.ArrayData`. All interpretation happens in Rust
//! (`translate.rs`), where it can be tested without SOLIDWORKS.

use serde::{Deserialize, Serialize};

pub const DUMP_FORMAT: &str = "rustworks-swdump/1";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Dump {
    #[serde(default)]
    pub format: String,
    /// "part" or "assembly"
    pub kind: String,
    /// Original SOLIDWORKS file.
    #[serde(default)]
    pub source: String,
    pub name: String,
    /// STEP export of the whole document, relative to the dump file.
    #[serde(default)]
    pub step: Option<String>,
    /// SOLIDWORKS mass-properties volume (m³), used to verify translations.
    #[serde(default)]
    pub volume_m3: Option<f64>,
    /// SOLIDWORKS centre of mass (m), to catch mirrored or rotated translations.
    #[serde(default)]
    pub center_of_mass_m: Option<[f64; 3]>,
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub mates: Vec<Mate>,
    /// Problems the bridge hit while reading the document.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Feature {
    pub name: String,
    /// `IFeature.GetTypeName2`, e.g. "ProfileFeature", "Extrusion", "Cut".
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub suppressed: bool,
    /// Bridge-side error reading this feature's definition.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub sketch: Option<Sketch>,
    /// Name of the sketch a feature consumes (its first ProfileFeature sub-feature).
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub extrude: Option<Extrude>,
    #[serde(default)]
    pub revolve: Option<Revolve>,
    #[serde(default)]
    pub fillet: Option<EdgeFeature>,
    #[serde(default)]
    pub chamfer: Option<EdgeFeature>,
    #[serde(default)]
    pub shell: Option<ShellData>,
    #[serde(default)]
    pub hole: Option<HoleData>,
    #[serde(default)]
    pub linear_pattern: Option<LinearPatternData>,
    #[serde(default)]
    pub circular_pattern: Option<CircularPatternData>,
    #[serde(default)]
    pub mirror: Option<MirrorData>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sketch {
    /// Sketch→model `MathTransform.ArrayData` (13 values: 3×3 rotation, translation in m, scale).
    pub to_model: Vec<f64>,
    #[serde(default)]
    pub points: Vec<Point>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub dims: Vec<Dim>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Point {
    /// `GetID()` joined with "-".
    pub id: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    /// "line", "arc", "circle", "spline", or another SOLIDWORKS segment type (unsupported).
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub construction: bool,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub center: Option<String>,
    /// Arc direction from `GetRotationDir`: 1 counter-clockwise, -1 clockwise.
    #[serde(default)]
    pub dir: Option<i32>,
    /// Circle radius (m).
    #[serde(default)]
    pub radius: Option<f64>,
    /// Spline points (ids).
    #[serde(default)]
    pub points: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Dim {
    /// `IDisplayDimension.Type2` (swDimensionType_e).
    #[serde(rename = "type")]
    pub kind: i32,
    /// `SystemValue`: metres or radians.
    pub value: f64,
    /// Attached sketch entity ids (points or segments).
    #[serde(default)]
    pub entities: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Extrude {
    /// swEndConditions_e of direction 1 (0 blind, 1 through all, 6 mid plane, ...).
    pub end1: i32,
    #[serde(default)]
    pub depth1: f64,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub both: bool,
    #[serde(default)]
    pub end2: Option<i32>,
    #[serde(default)]
    pub depth2: Option<f64>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub thin: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Revolve {
    /// Total angle (rad).
    pub angle: f64,
    /// Revolve type (0 one direction, 1 mid plane, 2 two directions).
    #[serde(default)]
    pub kind: i32,
    /// Axis as a sketch segment id of the profile sketch…
    #[serde(default)]
    pub axis_segment: Option<String>,
    /// …or two points on it in model coordinates (m).
    #[serde(default)]
    pub axis_points: Option<[[f64; 3]; 2]>,
    #[serde(default)]
    pub thin: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EdgeFeature {
    /// Fillet radius or chamfer distance (m).
    pub size: f64,
    /// Chamfer: second distance or angle (rad) when not equal-distance.
    #[serde(default)]
    pub chamfer_type: Option<i32>,
    #[serde(default)]
    pub size2: Option<f64>,
    #[serde(default)]
    pub angle: Option<f64>,
    /// Points on each edge (model coordinates, m).
    #[serde(default)]
    pub edges: Vec<[f64; 3]>,
    /// Faces whose edges are filleted (points on them), if selected by face.
    #[serde(default)]
    pub faces: Vec<[f64; 3]>,
    /// Variable radius, setback, etc. (unsupported).
    #[serde(default)]
    pub variable: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShellData {
    pub thickness: f64,
    #[serde(default)]
    pub outward: bool,
    #[serde(default)]
    pub faces: Vec<[f64; 3]>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HoleData {
    /// Hole-position sketch (its points are the hole centres).
    pub sketch: Sketch,
    /// swWzdHoleTypes_e.
    pub hole_type: i32,
    pub diameter: f64,
    /// swEndConditions_e (0 blind, 1 through all).
    pub end: i32,
    #[serde(default)]
    pub depth: f64,
    #[serde(default)]
    pub cbore_diameter: Option<f64>,
    #[serde(default)]
    pub cbore_depth: Option<f64>,
    #[serde(default)]
    pub csink_diameter: Option<f64>,
    #[serde(default)]
    pub csink_angle: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LinearPatternData {
    pub features: Vec<String>,
    /// Unit direction (model).
    pub dir1: [f64; 3],
    pub spacing1: f64,
    pub count1: u32,
    #[serde(default)]
    pub dir2: Option<[f64; 3]>,
    #[serde(default)]
    pub spacing2: Option<f64>,
    #[serde(default)]
    pub count2: Option<u32>,
    #[serde(default)]
    pub skipped: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CircularPatternData {
    pub features: Vec<String>,
    pub axis_origin: [f64; 3],
    pub axis_dir: [f64; 3],
    pub count: u32,
    /// Spacing (rad) between instances, or total angle when `equal`.
    pub angle: f64,
    #[serde(default)]
    pub equal: bool,
    #[serde(default)]
    pub skipped: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MirrorData {
    /// Features to mirror (empty with `body` = mirror bodies).
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub body: bool,
    pub origin: [f64; 3],
    pub normal: [f64; 3],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Component {
    /// `Name2`, e.g. "bracket-1".
    pub name: String,
    pub path: String,
    /// Dump of the component's part, relative to this dump.
    #[serde(default)]
    pub dump: Option<String>,
    /// `Transform2.ArrayData` (component → assembly).
    pub transform: Vec<f64>,
    #[serde(default)]
    pub fixed: bool,
    #[serde(default)]
    pub suppressed: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Mate {
    pub name: String,
    /// swMateType_e (0 coincident, 1 concentric, 2 perpendicular, 3 parallel, 5 distance, 6 angle, ...).
    #[serde(rename = "type")]
    pub kind: i32,
    /// swMateAlign_e (0 aligned, 1 anti-aligned, 2 closest).
    #[serde(default)]
    pub align: i32,
    /// Distance (m) or angle (rad).
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub suppressed: bool,
    #[serde(default)]
    pub entities: Vec<MateEntity>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MateEntity {
    pub component: String,
    /// swSelectType_e of the reference (2 face, 1 edge, 3 vertex, 4 datum plane, 5 datum axis, ...).
    #[serde(default)]
    pub ref_type: i32,
    /// Datum plane/axis name, e.g. "Front Plane".
    #[serde(default)]
    pub ref_name: Option<String>,
    /// A point on the referenced face (m); coordinate space not guaranteed (assembly or part).
    #[serde(default)]
    pub point: Option<[f64; 3]>,
    /// `MateEntity2.EntityParams`.
    #[serde(default)]
    pub params: Vec<f64>,
}
