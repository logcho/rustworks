# RustWorks

[![CI](https://github.com/logcho/rustworks/actions/workflows/ci.yml/badge.svg)](https://github.com/logcho/rustworks/actions/workflows/ci.yml)

A parametric 3D CAD system in Rust, modeled on the SOLIDWORKS workflow: a sketch → feature history tree, driving dimensions and equations, and an industrial B-rep kernel (OpenCASCADE). It ships as two programs:

- **`rustworks`**: a desktop app with a FeatureManager tree, a PropertyManager, a GPU viewport (wgpu, 4× MSAA) and a background rebuild thread, so the UI stays at 60 fps while the model rebuilds.
- **`rustworks-mcp`**: a [Model Context Protocol](https://modelcontextprotocol.io) server that lets an AI agent (Claude, etc.) design parts and assemblies. It can also *render the model to an image* so the agent sees what it built.

Both programs read and write the same `.rwpart` / `.rwasm` / `.rwdrw` files and reload when they change on disk. If the app has a file open while an agent edits it through MCP, you watch the model update live, and edits made in the app go back to the agent.

## Build & run

Prebuilt binaries for macOS (Apple Silicon), Linux (x86_64) and Windows (x86_64) are attached to each [GitHub release](https://github.com/logcho/rustworks/releases). They contain `rustworks` (desktop app), `rustworks-mcp` (MCP server) and the examples; the macOS archive also has `RustWorks.app`. The binaries are unsigned, so macOS asks you to confirm the first launch (right-click › Open).

To build from source you need Rust (stable) plus CMake and a C++17 compiler for OpenCASCADE:

| OS | Prerequisites |
|---|---|
| macOS | Xcode Command Line Tools, `brew install cmake` |
| Linux | `build-essential cmake` (Debian/Ubuntu). X11 and Wayland libraries are loaded at runtime. |
| Windows | Visual Studio Build Tools (C++ workload), CMake |

```bash
cargo build --release      # first build compiles OpenCASCADE from source (5–15 min)
./target/release/rustworks examples/flange.rwpart      # desktop app (a part)
./target/release/rustworks examples/assembly/demo.rwasm  # an assembly
./target/release/rustworks examples/assembly/base.rwdrw  # a 2D drawing
cargo test --workspace                                  # unit and integration tests
python3 scripts/mcp_smoke.py                            # end-to-end MCP check
```

No C++ toolchain? `cargo build --release --no-default-features --features rw-kernel/truck` uses the pure-Rust `truck` kernel instead. That kernel is slower and has no fillet, chamfer, shell, loft, sweep, draft, STEP import, assemblies or drawings.

**CI** (`.github/workflows/ci.yml`) builds and tests on Linux, macOS and Windows on every push and pull request, runs the MCP smoke test, and checks the pure-Rust kernel. The OpenCASCADE build is cached between runs. **Releases** (`release.yml`): push a tag such as `v0.2.0` and the workflow builds, smoke-tests and packages all three platforms (`scripts/package.sh`) and publishes them as a GitHub release.

### Use it from Claude Code (MCP)

`.mcp.json` in this repo registers the server. Build with `cargo build --release`, then restart Claude Code in this folder. Example prompt:

> Using rustworks, design a 120 mm pipe flange with a 60 mm hub, a 32 mm bore and 8 counterbored M10 bolt holes on a 90 mm bolt circle. Render it, then export STEP.

For other MCP clients, run `target/release/rustworks-mcp [part.rwpart]` over stdio.

MCP tools (46):
- Parts: `new_part`, `open_part`, `get_part`, `set_parameters`, `create_sketch`, `edit_sketch` (incl. `trim`, `fillet_corners`, `offset`), `get_sketch`, `plane`, `extrude`, `revolve`, `loft`, `sweep`, `hole`, `fillet`, `chamfer`, `draft`, `shell`, `pattern_linear`, `pattern_circular`, `mirror`, `import_step`, `edit_feature` (JSON merge patch), `delete_feature`, `suppress_feature`, `list_faces`, `list_edges`.
- Assemblies: `new_assembly`, `open_assembly`, `get_assembly`, `add_component`, `edit_component`, `remove_component`, `add_mate`, `edit_mate`, `delete_mate`, `check_interference`.
- Drawings: `create_drawing`, `open_drawing`, `edit_drawing`, `render_drawing`, `export_drawing` (PDF/SVG/DXF/PNG).
- Both: `undo`, `redo`, `render` (PNG; face/edge index labels for parts, per-component colors for assemblies), `mass_properties`, `export` (STEP/STL/OBJ).

## Architecture

| Crate | Role (SOLIDWORKS analogue) |
|---|---|
| `rw-sketch` | 2D sketch entities (incl. splines), Levenberg–Marquardt constraint solver with DOF analysis (fully / under / over defined), closed-profile, hole and path detection, trim / offset / sketch fillet, equation evaluator (≈ D-Cubed DCM) |
| `rw-kernel` | B-rep modeling on OpenCASCADE 7.8 (via `opencascade-sys`, statically linked, plus a small C++ shim in `cpp/` for drafts, B-spline edges, sweeps, face geometry and compounds): extrude, revolve, loft, sweep, booleans, fillet, chamfer, shell, draft, per-face tessellation, exact mass properties, STEP import/export. An optional pure-Rust `truck` backend is also available (≈ Parasolid) |
| `rw-core` | `.rwpart` document, feature tree, incremental regeneration cache, undo/redo, file sync (≈ FeatureManager); `.rwasm` assemblies with a mate solver (≈ SOLIDWORKS assemblies) |
| `rw-render` | Headless software renderer to PNG for agents |
| `rw-drawing` | 2D drawings: exact hidden-line views (OpenCASCADE HLR), layout, dimensions, title block; SVG/DXF/PDF/PNG |
| `rw-mcp` | MCP server (JSON-RPC 2.0 over stdio) |
| `rw-app` | egui + wgpu desktop application |

Part features: Sketch (line, polyline, rectangle, circle, arc, polygon, slot, spline; 19 relation and dimension types; trim, offset, sketch fillet), Reference Plane (offset, tilted, or through 3 points), Extrude (blind / mid-plane / through-all, boss / cut), Revolve, Loft, Sweep (along lines/arcs/splines), Hole Wizard (simple / counterbore / countersink), Fillet, Chamfer, Draft, Shell, Linear Pattern (1–2 directions), Circular Pattern, Mirror (features or the whole body), Import STEP, suppress, reorder, equations.

**Sketching in the app:** double-click a sketch (or click Edit Sketch) to draw with the mouse.
- Tools: Line chains, rectangles, circles, arcs, splines, points, and Smart Dimension. Select mode lets you drag points; the solver keeps every relation.
- Driving dimensions are drawn in the viewport; click a value to edit it in place (numbers or parameter expressions).
- Editing: **Trim** (click the piece to remove), **Offset** (click a curve; its connected chain is offset, Shift for the other side), **Fillet** (click a corner point). The radius and distance fields appear in the command bar.
- Endpoints snap to existing points. Lines within 3° of horizontal or vertical get that relation automatically.
- Right-drag rotates the view. Esc cancels the current tool; Delete removes the entity under the cursor.

**Assemblies** (`.rwasm`): File › New/Open Assembly. Insert Component adds a part file; the first component (or any marked Fixed) is grounded. Click a face, Shift-click a face on another component, pick a mate type (coincident, concentric, distance, parallel, perpendicular, angle) and press Add Mate: the moving part snaps into place, then all mates are solved together. Planar faces mate as planes, cylinders and cones as their axes, spheres as their centers. The tree shows each mate's status and the remaining degrees of freedom; conflicting mates turn red. Drag a part with the left mouse button to move it: mated parts slide only along their remaining degrees of freedom (a pinned part spins about its pin), and the grounded part stays put. Check Interference reports overlapping components with the overlap volume. Double-click a component to edit its part; the assembly picks up the change automatically.

**Drawings** (`.rwdrw`): File › New Drawing makes a drawing of the active part or assembly. It shows front, top and right views in third-angle layout (first-angle optional) plus an isometric. Visible edges are solid and hidden edges dashed, computed exactly by hidden-line removal. The largest standard scale that fits the sheet (A4–A0, Letter, Tabloid) is chosen automatically, and the isometric snaps to its own standard scale. Overall dimensions, diameter callouts for round holes and bosses (grouped, e.g. "2× Ø8"), view labels and a title block (title, drawing number, scale, material, projection, date) are added automatically. Views can be added, removed, rescaled and shifted. The drawing rebuilds whenever its part or assembly changes and exports to PDF, SVG, DXF (layers VISIBLE / HIDDEN / DIM / BORDER / TEXT) and PNG.

**Edge references** (fillet, chamfer) are stored as a point on the edge plus the edge's kind, length and position within the part's bounding box. After each rebuild they re-snap to the matched edge, so a fillet keeps following its edge as dimensions change (for example the top edge of a pin stays "the top edge" when the pin gets longer). If a change makes the match ambiguous, the feature reports an error instead of silently grabbing a different edge.

### Performance
Measured on the example parts (release build, Apple Silicon):

| | truck kernel | OpenCASCADE |
|---|---|---|
| Flange, full rebuild (11 features, 8 counterbored holes) | 8.5 s | **0.11 s** |
| Bracket, full rebuild | 3–4.6 s | **0.10 s** |
| Bracket, incremental edit of the last feature | 143 ms | **2.6 ms** |

- **Incremental rebuild:** each feature's result is cached under a hash of everything before it. Editing feature *N* only replays *N…*.
- **No UI blocking:** rebuilds run on a worker thread. Rapid edits are coalesced, so only the latest state is built.
- **Tight tool sizing:** through-all tools are sized to the body, because truck's boolean time grows with tool size and tolerance.

## Known limitations

- **Shell after fillets:** OCCT cannot open a face whose edges are tangent to fillets. RustWorks detects this and asks you to move the Shell before those fillets, which is also the standard SOLIDWORKS practice.
- **Edge and face references are geometric,** not true topological naming. Large changes can make a reference ambiguous; it then reports an error and you reselect the edge or face.
- **Sweeps** use the profile where it is drawn (no automatic placement at the path start) and do not twist or scale along the path. Splines cannot be trimmed.
- **Assemblies:** no mechanical mates (gear, cam, slot), no sub-assemblies, no in-context part editing, and no exploded views. Dragging moves parts but cannot rotate an unmated part in place; type a rotation in the PropertyManager.
- **Not built yet:**
  - Modeling: rib, wrap, variable-radius fillets, threads, sheet metal, multi-body parts, configurations.
  - Reference geometry: axes and points as features.
  - Sketching: extend, text, dimension leader/arrow graphics (values are shown as labels), feature dimensions (e.g. extrude depth) in the viewport.
  - Drawings: only overall dimensions and diameter callouts are automatic; no manual dimensions, section/detail views, GD&T, bills of materials or balloons.
  - Simulation.
- **Licensing:** OpenCASCADE is LGPL-2.1 (with an exception) and is linked statically. That's fine for this open-source project, whose source is public, so release binaries can be rebuilt and relinked. If you distribute closed-source binaries, check the LGPL relinking terms or switch to dynamic linking.
