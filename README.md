# RustWorks

A parametric 3D CAD system in Rust, modeled on the SOLIDWORKS workflow: a sketch → feature history tree, driving dimensions and equations, and an industrial B-rep kernel (OpenCASCADE). It ships as two programs:

- **`rustworks`**: a desktop app with a FeatureManager tree, a PropertyManager, a GPU viewport (wgpu, 4× MSAA) and a background rebuild thread, so the UI stays at 60 fps while the model rebuilds.
- **`rustworks-mcp`**: a [Model Context Protocol](https://modelcontextprotocol.io) server that lets an AI agent (Claude, etc.) design parts. It can also *render the part to an image* so the agent sees what it built.

Both programs read and write the same `.rwpart` file and reload when it changes on disk. If the app has that file open while an agent edits it through MCP, you watch the part update live, and edits made in the app go back to the agent.

## Build & run

```bash
cargo build --release      # first build compiles OpenCASCADE from source (~5 min, needs cmake + a C++ compiler)
./target/release/rustworks examples/flange.rwpart      # desktop app
cargo test --workspace                                  # kernel/solver/regen tests
```

No C++ toolchain? `cargo build --release --no-default-features --features rw-kernel/truck` uses the pure-Rust `truck` kernel instead. That kernel is slower and has no fillet, chamfer, shell, loft or STEP import.

### Use it from Claude Code (MCP)

`.mcp.json` in this repo registers the server. Build with `cargo build --release`, then restart Claude Code in this folder. Example prompt:

> Using rustworks, design a 120 mm pipe flange with a 60 mm hub, a 32 mm bore and 8 counterbored M10 bolt holes on a 90 mm bolt circle. Render it, then export STEP.

For other MCP clients, run `target/release/rustworks-mcp [part.rwpart]` over stdio.

MCP tools: `new_part`, `open_part`, `get_part`, `set_parameters`, `create_sketch`, `edit_sketch`, `get_sketch`, `extrude`, `revolve`, `loft`, `hole`, `fillet`, `chamfer`, `shell`, `pattern_linear`, `pattern_circular`, `mirror`, `import_step`, `edit_feature` (JSON merge patch), `delete_feature`, `suppress_feature`, `undo`, `redo`, `list_faces`, `list_edges`, `render` (PNG, with optional face/edge index labels), `mass_properties`, `export` (STEP/STL/OBJ).

## Architecture

| Crate | Role (SOLIDWORKS analogue) |
|---|---|
| `rw-sketch` | 2D sketch entities, Levenberg–Marquardt constraint solver with DOF analysis (fully / under / over defined), closed-profile and hole detection, equation evaluator (≈ D-Cubed DCM) |
| `rw-kernel` | B-rep modeling on OpenCASCADE 7.8 (via `opencascade-sys`, statically linked): extrude, revolve, loft, booleans, fillet, chamfer, shell, per-face tessellation, exact mass properties, STEP import/export. An optional pure-Rust `truck` backend is also available (≈ Parasolid) |
| `rw-core` | `.rwpart` document, feature tree, incremental regeneration cache, undo/redo, file sync (≈ FeatureManager) |
| `rw-render` | Headless software renderer to PNG for agents |
| `rw-mcp` | MCP server (JSON-RPC 2.0 over stdio) |
| `rw-app` | egui + wgpu desktop application |

Features: Sketch (line, polyline, rectangle, circle, arc, polygon, slot; 19 relation and dimension types), Extrude (blind / mid-plane / through-all, boss / cut), Revolve, Loft, Hole Wizard (simple / counterbore / countersink), Fillet, Chamfer, Shell, Linear Pattern (1–2 directions), Circular Pattern, Mirror (features or the whole body), Import STEP, suppress, reorder, equations.

**Sketching in the app:** double-click a sketch (or click Edit Sketch) to draw with the mouse.
- Tools: Line chains, rectangles, circles, arcs, points, and Smart Dimension. Select mode lets you drag points; the solver keeps every relation.
- Endpoints snap to existing points. Lines within 3° of horizontal or vertical get that relation automatically.
- Right-drag rotates the view. Esc cancels the current tool; Delete removes the entity under the cursor.

**Edge references** (fillet, chamfer) are stored as a point on the edge plus the edge's kind and length. After each rebuild they re-snap to the matched edge, so a fillet keeps following its edge as dimensions change. If a change makes the match ambiguous, the feature reports an error instead of silently grabbing a different edge.

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
- **Edge references are geometric,** not true topological naming. Large changes can make a reference ambiguous; it then reports an error and you reselect the edge.
- **Not built yet:**
  - Modeling: sweep, draft, rib, sheet metal, configurations.
  - Reference geometry: angled planes, axes.
  - Sketching: splines, trim/extend/offset, dimensions displayed in the viewport (they are edited in the PropertyManager).
  - Beyond parts: assemblies, drawings, simulation.
- **Licensing:** OpenCASCADE is LGPL-2.1 (with an exception) and is linked statically. That's fine for this open-source project. If you distribute closed-source binaries, check the LGPL relinking terms or switch to dynamic linking.
