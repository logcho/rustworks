#!/usr/bin/env python3
"""End-to-end smoke test of the RustWorks MCP server (used by CI).

Drives `rustworks-mcp` over stdio: models a part, mates two copies of it in
an assembly, makes a drawing, and exports STEP/STL/PDF. Exits non-zero on
any failure.

    python3 scripts/mcp_smoke.py [path/to/rustworks-mcp]
"""
import base64
import json
import math
import os
import subprocess
import sys
import tempfile

exe = sys.argv[1] if len(sys.argv) > 1 else os.path.join("target", "release", "rustworks-mcp")
exe = os.path.abspath(exe + (".exe" if os.name == "nt" and not exe.endswith(".exe") else ""))
work = tempfile.mkdtemp(prefix="rustworks-smoke-")
srv = subprocess.Popen([exe], cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
failures = []
nid = 0


def rpc(method, params):
    global nid
    nid += 1
    srv.stdin.write(json.dumps({"jsonrpc": "2.0", "id": nid, "method": method, "params": params}) + "\n")
    srv.stdin.flush()
    line = srv.stdout.readline()
    if not line:
        raise SystemExit("server closed stdout")
    return json.loads(line)


def call(tool, **args):
    r = rpc("tools/call", {"name": tool, "arguments": args})["result"]
    text = next(c["text"] for c in r["content"] if c["type"] == "text")
    images = [c for c in r["content"] if c["type"] == "image"]
    if r["isError"]:
        failures.append(f"{tool}: {text[:300]}")
    try:
        data = json.loads(text)
    except ValueError:
        data = {}
    if isinstance(data, dict) and data.get("rebuild_errors"):
        failures.append(f"{tool}: {data['rebuild_errors']}")
    for img in images:
        if not base64.b64decode(img["data"]).startswith(b"\x89PNG"):
            failures.append(f"{tool}: image is not a PNG")
    return data, images


def check(cond, what):
    if not cond:
        failures.append(what)


init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}})
check(init["result"]["serverInfo"]["name"] == "rustworks", "initialize")
tools = rpc("tools/list", {})["result"]["tools"]
check(len(tools) >= 46, f"expected 46+ tools, got {len(tools)}")

# Part: 40 x 10 x 30 block with a Ø10 through hole, filleted vertical edges
call("new_part", name="block", path="block.rwpart")
call("set_parameters", parameters={"w": 40})
d, _ = call("create_sketch", plane="top", geometry=[{"type": "center_rectangle", "center": [0, 0], "width": 40, "height": 30}, {"type": "circle", "center": [0, 0], "radius": 5}])
call("extrude", sketch=d["sketch"], depth=10)
e, _ = call("list_edges", parallel_to="y")
corners = [x["index"] for x in e.get("edges", []) if abs(x["mid"][0]) > 15]  # skip the hole's seam line
check(len(corners) == 4, f"four vertical corner edges, got {len(corners)}")
call("fillet", edge_indices=corners, radius=2)
m, _ = call("mass_properties")
expected = (40 * 30 - math.pi * 25 - 4 * (4 - math.pi)) * 10
check(abs(m.get("volume_mm3", 0) - expected) < 1e-2, f"volume {m.get('volume_mm3')} vs {expected:.3f}")
_, imgs = call("render", view="isometric")
check(len(imgs) == 1, "render returns an image")
call("export", format="step", path="block.step")
call("export", format="stl", path="block.stl")
check(open(os.path.join(work, "block.step")).read().startswith("ISO-10303-21"), "STEP file")

# Assembly: two blocks stacked face-to-face with concentric holes
call("new_assembly", name="stack", path="stack.rwasm")
call("add_component", part="block.rwpart")
call("add_component", part="block.rwpart", position=[80, 0, 0])
f, _ = call("list_faces", component="c1")
faces = f.get("faces_in_part_coordinates", [])
top = next((x["index"] for x in faces if x["kind"] == "plane" and x["normal"][1] > 0.99), None)
bottom = next((x["index"] for x in faces if x["kind"] == "plane" and x["normal"][1] < -0.99), None)
hole = next((x["index"] for x in faces if (x.get("surface") or {}).get("kind") == "cylinder" and abs(x["surface"]["radius"] - 5) < 1e-6), None)
check(None not in (top, bottom, hole), "found top, bottom and hole faces")
call("add_mate", type="concentric", a={"component": "c1", "face": hole}, b={"component": "c2", "face": hole})
s, _ = call("add_mate", type="coincident", a={"component": "c1", "face": top}, b={"component": "c2", "face": bottom})
check(s.get("solve", {}).get("dof") == 1, f"one DOF left (spin), got {s.get('solve')}")
pos = next((c["position"] for c in s.get("components", []) if c["id"] == "c2"), None)
check(pos is not None and abs(pos[0]) < 1e-4 and abs(pos[1] - 10) < 1e-4 and abs(pos[2]) < 1e-4, f"c2 stacked on c1: {pos}")
i, _ = call("check_interference")
check(i.get("clear") is True, f"no interference: {i}")
call("export", format="step", path="stack.step")

# Drawing of the assembly
r, imgs = call("create_drawing", path="stack.rwdrw", material="Steel")
check(len(imgs) == 1 and len(r.get("views", [])) == 4, "drawing preview with four views")
for fmt in ["pdf", "svg", "dxf"]:
    call("export_drawing", format=fmt)
check(open(os.path.join(work, "stack.pdf"), "rb").read().startswith(b"%PDF"), "PDF file")

# SOLIDWORKS: translate a bridge dump (no SOLIDWORKS needed) with its feature history
repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sw, _ = call("import_solidworks", path=os.path.join(repo, "examples", "solidworks", "bracket.swdump.json"), out_dir=os.path.join(work, "sw"))
rep = sw.get("report", {})
check(rep.get("mode") == "history" and rep.get("features_translated") == 5, f"SOLIDWORKS dump imported with history: {rep}")
check(abs((rep.get("rustworks_volume_mm3") or 0) - (rep.get("solidworks_volume_mm3") or 1)) < 1e-3, "SOLIDWORKS volume matches")
p, _ = call("get_part")
check(any(f.get("name") == "Cut-Extrude1" for f in p.get("document", {}).get("features", [])), "imported part is the active document")

srv.stdin.close()
srv.wait(timeout=60)
if failures:
    print("FAILED:")
    for f in failures:
        print("  -", f)
    sys.exit(1)
print(f"MCP smoke test passed ({len(tools)} tools) in {work}")
