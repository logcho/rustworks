//! MCP tool definitions (JSON Schemas shown to the AI client).

use serde_json::{json, Value};

const PLANE_DESC: &str = "Sketch plane. One of: \"front\" (XY, normal +Z), \"top\" (XZ, normal +Y), \"right\" (YZ, normal +X); \
{\"base\":\"top\",\"offset\":10} for an offset plane; {\"face\": N} to sketch on planar face N of the current body (see list_faces / render with face_labels); \
or {\"origin\":[x,y,z],\"normal\":[x,y,z],\"x_dir\":[x,y,z]}. World is Y-up, units mm.";

const SHAPES_DESC: &str = "Geometry to add, in sketch-plane 2D coordinates (mm). Shapes: \
{\"type\":\"line\",\"from\":[x,y],\"to\":[x,y]}, \
{\"type\":\"polyline\",\"points\":[[x,y],...],\"closed\":true}, \
{\"type\":\"rectangle\",\"corner1\":[x,y],\"corner2\":[x,y]}, \
{\"type\":\"center_rectangle\",\"center\":[x,y],\"width\":w,\"height\":h}, \
{\"type\":\"circle\",\"center\":[x,y],\"radius\":r}, \
{\"type\":\"arc\",\"center\":[x,y],\"start\":[x,y],\"end\":[x,y]} (counter-clockwise start→end), \
{\"type\":\"polygon\",\"center\":[x,y],\"radius\":r,\"sides\":n,\"rotation\":deg}, \
{\"type\":\"slot\",\"from\":[x,y],\"to\":[x,y],\"radius\":r}, \
{\"type\":\"point\",\"at\":[x,y]}. \
Any shape may set \"construction\":true (lines/circles/arcs) to exclude it from profiles. Endpoints at identical coordinates are merged automatically, so closed outlines form closed profiles. \
Nested closed loops become holes. The response lists the entity ids created for each shape (points p*, lines l*, circles c*, arcs a*).";

const CONSTRAINTS_DESC: &str = "Constraints referencing entity ids. Types: \
coincident{a,b}, horizontal{line}, vertical{line}, distance{a,b?,value} (line length if b omitted; point-point; point-line; line-line), \
horizontal_distance{a,b,value}, vertical_distance{a,b,value}, radius{entity,value}, diameter{entity,value}, angle{a,b,value(deg)}, \
parallel{a,b}, perpendicular{a,b}, equal{a,b}, tangent{a,b}, concentric{a,b}, point_on_line{point,line}, point_on_circle{point,circle}, \
midpoint{point,line}, symmetric{a,b,about}, fix{point,x?,y?}. \
Every `value` may be a number or an expression string using part parameters, e.g. \"width/2 + 3\".";

fn op_schema() -> Value {
    json!({"type":"string","enum":["boss","cut"],"description":"boss adds material, cut removes it. Default boss."})
}

fn dim() -> Value {
    json!({"description":"Number (mm or deg) or expression string referencing parameters, e.g. \"thickness*2\"","anyOf":[{"type":"number"},{"type":"string"}]})
}

pub fn list() -> Value {
    let v3 = json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3});
    let plane = json!({"description": PLANE_DESC});
    json!([
        {
            "name": "new_part",
            "description": "Create a new empty part and make it the active document. It is saved to `path` (default ./<name>.rwpart) and autosaved after every change, so the RustWorks desktop app showing that file updates live.",
            "inputSchema": {"type":"object","properties":{
                "name":{"type":"string"},
                "path":{"type":"string","description":"File path for the .rwpart document"}
            },"required":["name"]}
        },
        {
            "name": "open_part",
            "description": "Open an existing .rwpart file as the active document.",
            "inputSchema": {"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
        },
        {
            "name": "get_part",
            "description": "Return the full part document (parameters + feature tree JSON) and the rebuild status of every feature.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "set_parameters",
            "description": "Create/update global design parameters (like SOLIDWORKS equations). Values are numbers or expressions over other parameters. Use null to delete one. Any dimension in the model can reference them by name.",
            "inputSchema": {"type":"object","properties":{"parameters":{"type":"object","additionalProperties":{"anyOf":[{"type":"number"},{"type":"string"},{"type":"null"}]}}},"required":["parameters"]}
        },
        {
            "name": "create_sketch",
            "description": format!("Create a 2D sketch feature with geometry and constraints in one call, then solve it. Returns entity ids and solve status (fully/under/over defined, remaining degrees of freedom) plus the plane's 2D→3D mapping. {SHAPES_DESC}"),
            "inputSchema": {"type":"object","properties":{
                "plane": plane,
                "name":{"type":"string"},
                "geometry":{"type":"array","items":{"type":"object"},"description":SHAPES_DESC},
                "constraints":{"type":"array","items":{"type":"object"},"description":CONSTRAINTS_DESC}
            },"required":["plane","geometry"]}
        },
        {
            "name": "edit_sketch",
            "description": "Modify an existing sketch: add geometry/constraints, remove entities or constraints, or move points. Triggers a rebuild of the sketch and everything after it.",
            "inputSchema": {"type":"object","properties":{
                "sketch":{"type":"string","description":"Sketch feature id or name"},
                "add_geometry":{"type":"array","items":{"type":"object"},"description":SHAPES_DESC},
                "add_constraints":{"type":"array","items":{"type":"object"},"description":CONSTRAINTS_DESC},
                "remove_entities":{"type":"array","items":{"type":"string"},"description":"Entity ids to delete (dependent lines/constraints are removed too)"},
                "remove_constraints":{"type":"array","items":{"type":"integer"},"description":"Constraint indices (from get_sketch) to delete"},
                "move_points":{"type":"object","additionalProperties":{"type":"array","items":{"type":"number"}},"description":"{point_id:[x,y]} new positions before solving"},
                "plane": plane
            },"required":["sketch"]}
        },
        {
            "name": "get_sketch",
            "description": "Return a sketch's entities with solved coordinates, its constraints (with indices), its plane and its solve status.",
            "inputSchema": {"type":"object","properties":{"sketch":{"type":"string"}},"required":["sketch"]}
        },
        {
            "name": "extrude",
            "description": "Extrude a sketch's closed profiles along the sketch normal (Extruded Boss/Base or Extruded Cut).",
            "inputSchema": {"type":"object","properties":{
                "sketch":{"type":"string"},
                "depth": dim(),
                "end":{"type":"string","enum":["blind","mid_plane","through_all"],"description":"Default blind. mid_plane extrudes depth/2 each side. through_all ignores depth."},
                "reverse":{"type":"boolean","description":"Extrude opposite to the sketch normal"},
                "op": op_schema(),
                "name":{"type":"string"}
            },"required":["sketch"]}
        },
        {
            "name": "revolve",
            "description": "Revolve a sketch's closed profiles about an axis (Revolved Boss/Cut). The profile must not cross the axis (touching it is fine).",
            "inputSchema": {"type":"object","properties":{
                "sketch":{"type":"string"},
                "axis":{"description":"A sketch line id, \"x_axis\" / \"y_axis\" (the sketch's own axes), or {\"origin\":[x,y],\"direction\":[dx,dy]} in sketch coordinates"},
                "angle": dim(),
                "op": op_schema(),
                "name":{"type":"string"}
            },"required":["sketch","axis"]}
        },
        {
            "name": "hole",
            "description": "Hole Wizard: drill simple, counterbored or countersunk holes into the body from a plane/face, opposite to its normal. Omit depth for through-all.",
            "inputSchema": {"type":"object","properties":{
                "plane": plane,
                "points":{"type":"array","items":{"type":"array","items":{"type":"number"}},"description":"Hole centers in plane 2D coordinates"},
                "diameter": dim(),
                "depth": dim(),
                "counterbore":{"type":"object","properties":{"diameter":dim(),"depth":dim()}},
                "countersink":{"type":"object","properties":{"diameter":dim(),"angle":dim()}},
                "name":{"type":"string"}
            },"required":["plane","points","diameter"]}
        },
        {
            "name": "pattern_linear",
            "description": "Linear pattern of features (bosses, cuts, holes, other patterns) in one or two directions. Count includes the original.",
            "inputSchema": {"type":"object","properties":{
                "features":{"type":"array","items":{"type":"string"}},
                "direction": v3, "spacing": dim(), "count": dim(),
                "direction2": v3, "spacing2": dim(), "count2": dim(),
                "name":{"type":"string"}
            },"required":["features","direction","spacing","count"]}
        },
        {
            "name": "pattern_circular",
            "description": "Circular pattern of features about an axis. Count includes the original; angle 360 (default) spaces instances equally.",
            "inputSchema": {"type":"object","properties":{
                "features":{"type":"array","items":{"type":"string"}},
                "axis_origin": v3, "axis_direction": v3,
                "count": dim(), "angle": dim(),
                "name":{"type":"string"}
            },"required":["features","count"]}
        },
        {
            "name": "mirror",
            "description": "Mirror features across a plane, or the whole body when `features` is empty/omitted.",
            "inputSchema": {"type":"object","properties":{
                "features":{"type":"array","items":{"type":"string"}},
                "plane": plane,
                "name":{"type":"string"}
            },"required":["plane"]}
        },
        {
            "name": "list_edges",
            "description": "List the body's edges: index, kind (line/circle/curve), start/end/mid points and length. Filter to keep the list short. Use an edge's mid point or index with fillet/chamfer.",
            "inputSchema": {"type":"object","properties":{
                "kind":{"type":"string","enum":["line","circle","curve"]},
                "parallel_to":{"type":"string","enum":["x","y","z"],"description":"Only straight edges along this axis"},
                "near":{"type":"array","items":{"type":"number"},"description":"Only edges within `radius` of this point"},
                "radius":{"type":"number"},
                "limit":{"type":"integer"}
            }}
        },
        {
            "name": "fillet",
            "description": "Round edges with a constant radius. Select edges by points on/near them (e.g. mid points from list_edges), by edge indices, or all_edges. References re-snap to the edges after every rebuild, so they survive dimension changes.",
            "inputSchema": {"type":"object","properties":{
                "edges":{"type":"array","items":{"type":"array","items":{"type":"number"}},"description":"Points near the edges"},
                "edge_indices":{"type":"array","items":{"type":"integer"}},
                "all_edges":{"type":"boolean"},
                "radius": dim(),
                "name":{"type":"string"}
            },"required":["radius"]}
        },
        {
            "name": "chamfer",
            "description": "Bevel edges with an equal-distance chamfer. Edge selection like fillet.",
            "inputSchema": {"type":"object","properties":{
                "edges":{"type":"array","items":{"type":"array","items":{"type":"number"}}},
                "edge_indices":{"type":"array","items":{"type":"integer"}},
                "all_edges":{"type":"boolean"},
                "distance": dim(),
                "name":{"type":"string"}
            },"required":["distance"]}
        },
        {
            "name": "shell",
            "description": "Hollow the body to a wall thickness (inward). Faces to remove (leave open) are given by indices (see list_faces / render face_labels) or points on them.",
            "inputSchema": {"type":"object","properties":{
                "faces":{"type":"array","items":{"type":"integer"}},
                "face_points":{"type":"array","items":{"type":"array","items":{"type":"number"}}},
                "thickness": dim(),
                "name":{"type":"string"}
            },"required":["thickness"]}
        },
        {
            "name": "loft",
            "description": "Loft a solid through the profiles of two or more sketches (in order). Each sketch must contain exactly one closed profile without holes, typically on parallel offset planes.",
            "inputSchema": {"type":"object","properties":{
                "sketches":{"type":"array","items":{"type":"string"}},
                "op": op_schema(),
                "name":{"type":"string"}
            },"required":["sketches"]}
        },
        {
            "name": "import_step",
            "description": "Import a STEP file's solid geometry as a feature (boss adds it to the body, cut subtracts it).",
            "inputSchema": {"type":"object","properties":{
                "path":{"type":"string"},
                "op": op_schema(),
                "name":{"type":"string"}
            },"required":["path"]}
        },
        {
            "name": "edit_feature",
            "description": "Change any feature's definition with a JSON merge patch against its JSON form (see get_part), e.g. {\"depth\": 25} or {\"op\":\"cut\",\"reverse\":true} or {\"name\":\"Base\"}. Downstream features rebuild automatically.",
            "inputSchema": {"type":"object","properties":{
                "feature":{"type":"string"},
                "changes":{"type":"object"}
            },"required":["feature","changes"]}
        },
        {
            "name": "delete_feature",
            "description": "Delete a feature. Fails if other features depend on it unless cascade=true (which deletes the dependents too).",
            "inputSchema": {"type":"object","properties":{"feature":{"type":"string"},"cascade":{"type":"boolean"}},"required":["feature"]}
        },
        {
            "name": "suppress_feature",
            "description": "Suppress (skip) or unsuppress a feature without deleting it.",
            "inputSchema": {"type":"object","properties":{"feature":{"type":"string"},"suppressed":{"type":"boolean"}},"required":["feature","suppressed"]}
        },
        {
            "name": "undo",
            "description": "Undo the last change.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "redo",
            "description": "Redo the last undone change.",
            "inputSchema": {"type":"object","properties":{}}
        },
        {
            "name": "list_faces",
            "description": "List the faces of the current body: index, kind (plane/curved), outward normal, centroid and area. Use a planar face index as {\"face\":N} for sketch/hole planes.",
            "inputSchema": {"type":"object","properties":{"planar_only":{"type":"boolean"}}}
        },
        {
            "name": "render",
            "description": "Render the current part to a PNG image so you can see it: shaded body, black B-rep edges, blue sketches. face_labels draws face indices (yellow tags) to help pick faces.",
            "inputSchema": {"type":"object","properties":{
                "view":{"type":"string","enum":["isometric","isometric_back","front","back","top","bottom","right","left"]},
                "width":{"type":"integer"},"height":{"type":"integer"},
                "edges":{"type":"boolean"},
                "face_labels":{"type":"boolean"},
                "edge_labels":{"type":"boolean","description":"Draw edge indices (cyan tags) for fillet/chamfer selection"},
                "sketches":{"type":"boolean","description":"Overlay sketch geometry (default true)"}
            }}
        },
        {
            "name": "mass_properties",
            "description": "Volume (mm³), surface area (mm²), center of mass, bounding box and mass for a given density (g/cm³, default 7.85 steel).",
            "inputSchema": {"type":"object","properties":{"density":{"type":"number"}}}
        },
        {
            "name": "export",
            "description": "Export the current body: step (exact B-rep, for other CAD), stl (3D printing), obj (mesh).",
            "inputSchema": {"type":"object","properties":{
                "format":{"type":"string","enum":["step","stl","obj"]},
                "path":{"type":"string"}
            },"required":["format","path"]}
        }
    ])
}
