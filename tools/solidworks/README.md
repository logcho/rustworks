# RustWorks ↔ SOLIDWORKS bridge

SOLIDWORKS' native files (`.sldprt`, `.sldasm`) are a closed format, so
RustWorks reads and writes them **through SOLIDWORKS itself**, using the
official SOLIDWORKS API. This folder holds the small Python program that does
that. It must run on a Windows PC with SOLIDWORKS installed.

## Setup (on the SOLIDWORKS PC)

1. Install Python 3.8+ from python.org (tick "Add python.exe to PATH").
2. `pip install pywin32`
3. That's it. RustWorks finds `rw_sw_bridge.py` automatically when it sits in
   the `solidworks` folder next to the RustWorks executables (as in the release
   archives). Otherwise set `RUSTWORKS_SW_BRIDGE` to its full path, and
   `RUSTWORKS_PYTHON` if Python isn't on the PATH.

## Use

* **In RustWorks** (on the SOLIDWORKS PC): File › Import SOLIDWORKS… /
  Export to SOLIDWORKS…, or ask Claude (`import_solidworks`,
  `export_solidworks`), or from a terminal:

  ```
  rustworks-mcp sw-import C:\Parts\bracket.SLDPRT
  rustworks-mcp sw-export bracket.rwpart C:\Parts\bracket-copy.SLDPRT
  ```

* **On a Mac/Linux machine:** run the bridge on the SOLIDWORKS PC and copy the
  output folder over, then import the `.swdump.json` file there:

  ```
  python rw_sw_bridge.py dump C:\Parts\bracket.SLDPRT C:\Temp\bracket-dump
  ```

## What carries over

| SOLIDWORKS | RustWorks |
|---|---|
| Sketches (lines, arcs, circles, splines; construction geometry) | Sketches with the same geometry, horizontal/vertical relations and driving dimensions (length, distance, radius, diameter, angle) |
| Extruded boss/cut (blind, through all, mid plane, two directions) | Extrude |
| Revolved boss/cut | Revolve |
| Hole Wizard (simple, counterbore, countersink, tapped as drill size) | Hole |
| Fillet (constant radius, edges), Chamfer (equal distance), Shell | Fillet, Chamfer, Shell |
| Linear/circular pattern, Mirror | Patterns, Mirror |
| Assembly components and placements | Assembly components (sub-assemblies flattened) |
| Coincident, concentric, distance, parallel, perpendicular, angle mates | Mates |

Every part rebuilt from history is checked against SOLIDWORKS' mass
properties (volume and centre of mass). If a part uses anything else (drafts,
sweeps, lofts, ribs, variable fillets, configurations, …), fails to rebuild, or
doesn't match, it is imported as **exact geometry** from the STEP file the
bridge also writes, and `import-report.json` explains why.

Exporting sends RustWorks geometry to SOLIDWORKS as an imported body: the
shape is exact, but SOLIDWORKS does not get RustWorks' feature history.
Drawings (`.slddrw`) are not converted; make a RustWorks drawing from the
imported part instead.

## Status

The translation side is tested in RustWorks' test suite with sample dumps
(see `examples/solidworks/`). The bridge itself needs SOLIDWORKS to run and has
not yet been run against a real installation: if a file fails to import,
please share the bridge's error output and the `.swdump.json` it wrote.
