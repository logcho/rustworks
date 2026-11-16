#!/usr/bin/env python3
"""RustWorks <-> SOLIDWORKS bridge.

Runs on a Windows PC with SOLIDWORKS installed and drives it through the
official SOLIDWORKS API (COM). It does no interpretation: it copies raw API
data into a `.swdump.json` file (lengths in metres, angles in radians,
SOLIDWORKS enum values) plus a STEP export of each document. RustWorks turns
the dump into a parametric part/assembly (or falls back to the exact STEP
geometry) - see crates/rw-sw.

Usage (on the SOLIDWORKS machine):
    python rw_sw_bridge.py dump   <file.sldprt|file.sldasm> <out_dir>
    python rw_sw_bridge.py export <file.step> <file.sldprt|file.sldasm>

Requires: Python 3.8+ and pywin32 (`pip install pywin32`).
The last line printed is a JSON status object: {"ok": true, ...} or {"ok": false, "error": ...}.
"""
import json
import os
import sys
import traceback

try:
    import pythoncom
    import win32com.client
    from win32com.client import VARIANT
except ImportError:  # pragma: no cover - only meaningful on Windows
    pythoncom = None
    win32com = None

DUMP_FORMAT = "rustworks-swdump/1"

# SOLIDWORKS enum values (swDocumentTypes_e, swOpenDocOptions_e, swSaveAsOptions_e)
SW_DOC_PART, SW_DOC_ASSEMBLY = 1, 2
SW_OPEN_SILENT, SW_OPEN_READONLY = 1, 2
SW_SAVEAS_SILENT = 1
# swSketchSegments_e
SEG_LINE, SEG_ARC, SEG_ELLIPSE, SEG_SPLINE = 0, 1, 2, 3


# ---------------------------------------------------------------- COM helpers

def byref_long():
    return VARIANT(pythoncom.VT_BYREF | pythoncom.VT_I4, 0)


def null_dispatch():
    return VARIANT(pythoncom.VT_DISPATCH, None)


def cast(obj, iface):
    """QueryInterface to a typed SOLIDWORKS interface when makepy support is available."""
    try:
        return win32com.client.CastTo(obj, iface)
    except Exception:
        return obj


def attr(obj, *names, args=()):
    """First of `names` that works on obj: property value or method result."""
    last = None
    for n in names:
        try:
            v = getattr(obj, n)
        except Exception as e:  # noqa: BLE001
            last = e
            continue
        try:
            return v(*args) if callable(v) else v
        except Exception as e:  # noqa: BLE001
            last = e
    raise AttributeError(f"none of {names} worked: {last}")


def maybe(fn, default=None):
    try:
        return fn()
    except Exception:  # noqa: BLE001
        return default


def seq(v):
    """COM arrays arrive as tuples, None or a single object."""
    if v is None:
        return []
    if isinstance(v, (list, tuple)):
        return list(v)
    return [v]


def ident(o):
    ids = seq(attr(o, "GetID"))
    return "-".join(str(int(i)) for i in ids)


def first(v):
    """Typed wrappers return (value, out1, ...) tuples for ByRef calls."""
    return v[0] if isinstance(v, tuple) else v


# ---------------------------------------------------------------- SOLIDWORKS

def connect():
    if win32com is None:
        raise RuntimeError("pywin32 is not installed (pip install pywin32), or this is not Windows")
    try:
        sw = win32com.client.gencache.EnsureDispatch("SldWorks.Application")
    except Exception:  # noqa: BLE001 - fall back to late binding
        sw = win32com.client.Dispatch("SldWorks.Application")
    maybe(lambda: setattr(sw, "Visible", True))
    return sw


def open_doc(sw, path):
    ext = os.path.splitext(path)[1].lower()
    doc_type = {".sldprt": SW_DOC_PART, ".sldasm": SW_DOC_ASSEMBLY}.get(ext)
    if doc_type is None:
        raise RuntimeError(f"not a SOLIDWORKS part or assembly: {path}")
    opts = SW_OPEN_SILENT | SW_OPEN_READONLY
    attempts = [
        lambda: sw.OpenDoc6(path, doc_type, opts, "", byref_long(), byref_long()),
        lambda: sw.OpenDoc6(path, doc_type, opts, ""),
        lambda: sw.OpenDoc6(path, doc_type, opts, "", 0, 0),
    ]
    err = None
    for a in attempts:
        try:
            m = first(a())
            if m is not None:
                return m, doc_type
        except Exception as e:  # noqa: BLE001
            err = e
    raise RuntimeError(f"SOLIDWORKS could not open {path}: {err}")


def save_as(model, path):
    ext = model.Extension
    attempts = [
        lambda: ext.SaveAs(path, 0, SW_SAVEAS_SILENT, null_dispatch(), byref_long(), byref_long()),
        lambda: ext.SaveAs(path, 0, SW_SAVEAS_SILENT, None, 0, 0),
        lambda: model.SaveAs3(path, 0, SW_SAVEAS_SILENT),
    ]
    err = None
    for a in attempts:
        try:
            r = first(a())
            if r is False:
                continue
            if os.path.exists(path):
                return
        except Exception as e:  # noqa: BLE001
            err = e
    raise RuntimeError(f"SOLIDWORKS could not save {path}: {err}")


def mass_props(model):
    """(volume m³, centre of mass m) from SOLIDWORKS mass properties."""
    for fn in (lambda: model.Extension.CreateMassProperty2(), lambda: model.Extension.CreateMassProperty()):
        mp = maybe(fn)
        if mp is not None:
            v = maybe(lambda: float(mp.Volume))
            if v is not None:
                c = maybe(lambda: [float(x) for x in seq(mp.CenterOfMass)][:3])
                return v, c
    return None, None


def transform_array(xf):
    return [float(v) for v in seq(xf.ArrayData)]


def edge_point(edge):
    """A point on an edge (curve midpoint), model coordinates (m)."""
    edge = cast(edge, "IEdge")
    try:
        p = edge.GetCurveParams3()
        u = (p.UMinValue + p.UMaxValue) / 2.0
        pt = seq(edge.Evaluate2(u, 0))
        return [float(pt[0]), float(pt[1]), float(pt[2])]
    except Exception:  # noqa: BLE001
        a = seq(cast(edge.GetStartVertex(), "IVertex").GetPoint())
        b = seq(cast(edge.GetEndVertex(), "IVertex").GetPoint())
        return [(a[i] + b[i]) / 2.0 for i in range(3)]


def face_point(face):
    """A point on a face: the closest face point to its bounding-box centre."""
    face = cast(face, "IFace2")
    box = seq(face.GetBox())
    c = [(box[i] + box[i + 3]) / 2.0 for i in range(3)]
    p = seq(face.GetClosestPointOn(c[0], c[1], c[2]))
    return [float(p[0]), float(p[1]), float(p[2])]


def line_direction(entity):
    """Unit direction of a straight edge, reference axis or sketch line."""
    e = entity
    for iface in ("IRefAxis",):
        ra = maybe(lambda: cast(e, iface))
        params = maybe(lambda: seq(ra.GetRefAxisParams()))
        if params and len(params) >= 6:
            d = [params[3] - params[0], params[4] - params[1], params[5] - params[2]]
            return unit(d), params[:3]
    ed = cast(e, "IEdge")
    a = seq(cast(ed.GetStartVertex(), "IVertex").GetPoint())
    b = seq(cast(ed.GetEndVertex(), "IVertex").GetPoint())
    return unit([b[i] - a[i] for i in range(3)]), a[:3]


def axis_of(entity):
    """(origin, unit direction) of a circular edge, cylindrical face, reference axis or straight edge."""
    ed = maybe(lambda: cast(entity, "IEdge"))
    curve = maybe(lambda: ed.GetCurve())
    if curve is not None and maybe(lambda: curve.IsCircle()):
        cp = seq(curve.CircleParams)
        return cp[:3], unit(cp[3:6])
    face = maybe(lambda: cast(entity, "IFace2"))
    surf = maybe(lambda: face.GetSurface())
    if surf is not None and maybe(lambda: surf.IsCylinder()):
        cp = seq(surf.CylinderParams)
        return cp[:3], unit(cp[3:6])
    d, o = line_direction(entity)
    return o, d


def unit(v):
    n = sum(x * x for x in v) ** 0.5 or 1.0
    return [x / n for x in v]


def plane_of(entity):
    """(origin, normal) of a reference plane or planar face (m)."""
    rp = maybe(lambda: cast(entity, "IRefPlane"))
    xf = maybe(lambda: rp.Transform)
    if xf is not None:
        a = transform_array(xf)
        # row-vector convention: third row is the plane normal
        return a[9:12], [a[6], a[7], a[8]]
    face = cast(entity, "IFace2")
    surf = face.GetSurface()
    pp = seq(surf.PlaneParams)
    n = unit(pp[0:3])
    if maybe(lambda: face.FaceInSurfaceSense()) is False:
        n = [-x for x in n]
    return pp[3:6], n


# ---------------------------------------------------------------- features

def sub_sketch(feat):
    f = maybe(lambda: feat.GetFirstSubFeature())
    while f is not None:
        if maybe(lambda: f.GetTypeName2()) in ("ProfileFeature", "3DProfileFeature"):
            return f
        f = maybe(lambda: f.GetNextSubFeature())
    return None


def read_sketch(feat):
    sk = cast(feat.GetSpecificFeature2(), "ISketch")
    to_model = transform_array(sk.ModelToSketchTransform.Inverse())
    points = []
    for p in seq(sk.GetSketchPoints2()):
        p = cast(p, "ISketchPoint")
        points.append({"id": ident(p), "x": float(p.X), "y": float(p.Y)})
    segments = []
    for s in seq(sk.GetSketchSegments()):
        s = cast(s, "ISketchSegment")
        kind = int(s.GetType())
        seg = {"id": ident(s), "construction": bool(maybe(lambda: s.ConstructionGeometry, False))}
        if kind == SEG_LINE:
            ln = cast(s, "ISketchLine")
            seg.update(type="line", start=ident(ln.GetStartPoint2()), end=ident(ln.GetEndPoint2()))
        elif kind == SEG_ARC:
            arc = cast(s, "ISketchArc")
            if maybe(lambda: bool(arc.IsCircle()), False):
                seg.update(type="circle", center=ident(arc.GetCenterPoint2()), radius=float(arc.GetRadius()))
            else:
                seg.update(type="arc", center=ident(arc.GetCenterPoint2()), start=ident(arc.GetStartPoint2()),
                           end=ident(arc.GetEndPoint2()), dir=int(arc.GetRotationDir()))
        elif kind == SEG_SPLINE:
            sp = cast(s, "ISketchSpline")
            seg.update(type="spline", points=[ident(p) for p in seq(sp.GetPoints2())])
        else:
            seg.update(type=f"sw{kind}")
        segments.append(seg)
    dims = []
    dd = maybe(lambda: feat.GetFirstDisplayDimension())
    while dd is not None:
        dd = cast(dd, "IDisplayDimension")
        try:
            dim = cast(dd.GetDimension2(0), "IDimension")
            value = maybe(lambda: float(first(dim.GetSystemValue3(1, None))[0]))
            if value is None:
                value = float(dim.SystemValue)
            ann = cast(dd.GetAnnotation(), "IAnnotation")
            ents = [ident(e) for e in seq(ann.GetAttachedEntities3())]
            dims.append({"type": int(dd.Type2), "value": value, "entities": ents})
        except Exception as e:  # noqa: BLE001
            dims.append({"type": -1, "value": 0.0, "entities": [], "error": str(e)})
        dd = maybe(lambda: feat.GetNextDisplayDimension(dd))
    return {"to_model": to_model, "points": points, "segments": segments, "dims": dims}


def with_selections(model, data, fn):
    maybe(lambda: data.AccessSelections(model, None))
    try:
        return fn()
    finally:
        maybe(lambda: data.ReleaseSelectionAccess())


def read_feature(model, feat, kind):
    out = {}
    if kind in ("ProfileFeature",):
        out["sketch"] = read_sketch(feat)
        return out
    sub = sub_sketch(feat)
    if sub is not None:
        out["profile"] = sub.Name
    data = maybe(lambda: feat.GetDefinition())
    if kind in ("Extrusion", "Cut", "Boss", "ICE"):
        d = cast(data, "IExtrudeFeatureData2")
        both = bool(maybe(lambda: d.BothDirections, False))
        out["extrude"] = {
            "end1": int(d.GetEndCondition(True)),
            "depth1": float(d.GetDepth(True)),
            "reverse": bool(maybe(lambda: d.ReverseDirection, False)),
            "both": both,
            "end2": maybe(lambda: int(d.GetEndCondition(False))) if both else None,
            "depth2": maybe(lambda: float(d.GetDepth(False))) if both else None,
            "draft": bool(maybe(lambda: d.GetDraftWhileExtruding(True), False)),
            "thin": bool(maybe(lambda: d.IsThinFeature(), False)),
        }
    elif kind in ("Revolution", "RevCut"):
        d = cast(data, "IRevolveFeatureData2")
        rev = {"angle": float(d.GetRevolutionAngle(True)), "kind": int(maybe(lambda: d.Type, 0)),
               "thin": bool(maybe(lambda: d.IsThinFeature(), False))}

        def axis():
            ax = d.Axis
            s = maybe(lambda: cast(ax, "ISketchSegment"))
            sid = maybe(lambda: ident(s))
            if sid:
                rev["axis_segment"] = sid
            else:
                dvec, o = line_direction(ax)
                rev["axis_points"] = [o, [o[i] + dvec[i] for i in range(3)]]
        with_selections(model, d, axis)
        out["revolve"] = rev
    elif kind == "Fillet":
        d = cast(data, "ISimpleFilletFeatureData2")

        def read():
            edges = [edge_point(e) for e in seq(maybe(lambda: attr(d, "Edges", "GetEdges")))]
            faces = [face_point(f) for f in seq(maybe(lambda: attr(d, "Faces", "GetFaces")))]
            return {"size": float(d.DefaultRadius), "edges": edges, "faces": faces,
                    "variable": int(maybe(lambda: d.Type, 0)) != 0}
        out["fillet"] = with_selections(model, d, read)
    elif kind == "Chamfer":
        d = cast(data, "IChamferFeatureData2")

        def read():
            edges = [edge_point(e) for e in seq(maybe(lambda: attr(d, "Edges", "GetEdges")))]
            return {"size": float(maybe(lambda: d.GetEdgeChamferDistance(0), None) or attr(d, "EdgeChamferDistance", "Distance")),
                    "chamfer_type": maybe(lambda: int(d.Type)),
                    "size2": maybe(lambda: float(d.GetEdgeChamferDistance(1))),
                    "angle": maybe(lambda: float(d.EdgeChamferAngle)),
                    "edges": edges}
        out["chamfer"] = with_selections(model, d, read)
    elif kind == "Shell":
        d = cast(data, "IShellFeatureData")

        def read():
            faces = [face_point(f) for f in seq(maybe(lambda: attr(d, "FacesRemoved", "GetFacesRemoved", "Faces")))]
            return {"thickness": float(d.Thickness), "outward": bool(maybe(lambda: d.Outward, False)), "faces": faces}
        out["shell"] = with_selections(model, d, read)
    elif kind == "HoleWzd":
        d = cast(data, "IWizardHoleFeatureData2")
        if sub is None:
            raise RuntimeError("hole has no position sketch")
        hole_type = int(d.Type)
        dia = maybe(lambda: float(d.TapDrillDiameter)) if hole_type in (3, 4) else None
        out["hole"] = {
            "sketch": read_sketch(sub),
            "hole_type": hole_type,
            "diameter": dia or float(attr(d, "HoleDiameter", "Diameter")),
            "end": int(d.EndCondition),
            "depth": float(maybe(lambda: d.HoleDepth, 0.0) or 0.0),
            "cbore_diameter": maybe(lambda: float(d.CounterBoreDiameter)),
            "cbore_depth": maybe(lambda: float(d.CounterBoreDepth)),
            "csink_diameter": maybe(lambda: float(d.CounterSinkDiameter)),
            "csink_angle": maybe(lambda: float(d.CounterSinkAngle)),
        }
        out.pop("profile", None)
    elif kind == "LPattern":
        d = cast(data, "ILinearPatternFeatureData")

        def read():
            d1, _ = line_direction(d.D1Axis)
            if maybe(lambda: d.D1ReverseDirection, False):
                d1 = [-x for x in d1]
            lp = {"features": [f.Name for f in seq(d.PatternFeatureArray)], "dir1": d1,
                  "spacing1": float(d.D1Spacing), "count1": int(d.D1TotalInstances),
                  "skipped": len(seq(maybe(lambda: d.SkippedItemArray)))}
            n2 = int(maybe(lambda: d.D2TotalInstances, 1) or 1)
            if n2 > 1:
                d2, _ = line_direction(d.D2Axis)
                if maybe(lambda: d.D2ReverseDirection, False):
                    d2 = [-x for x in d2]
                lp.update(dir2=d2, spacing2=float(d.D2Spacing), count2=n2)
            return lp
        out["linear_pattern"] = with_selections(model, d, read)
    elif kind == "CirPattern":
        d = cast(data, "ICircularPatternFeatureData")

        def read():
            o, a = axis_of(d.Axis)
            if maybe(lambda: d.ReverseDirection, False):
                a = [-x for x in a]
            return {"features": [f.Name for f in seq(d.PatternFeatureArray)], "axis_origin": o, "axis_dir": a,
                    "count": int(d.TotalInstances), "angle": float(d.Spacing), "equal": bool(d.EqualSpacing),
                    "skipped": len(seq(maybe(lambda: d.SkippedItemArray)))}
        out["circular_pattern"] = with_selections(model, d, read)
    elif kind in ("MirrorPattern", "MirrorSolid"):
        d = cast(data, "IMirrorPatternFeatureData")

        def read():
            o, n = plane_of(d.Plane)
            feats = [f.Name for f in seq(maybe(lambda: d.PatternFeatureArray))]
            return {"features": feats, "body": kind == "MirrorSolid" or not feats, "origin": o, "normal": n}
        out["mirror"] = with_selections(model, d, read)
    return out


def dump_part(sw, model, path, out_dir, name):
    features = []
    warnings = []
    f = model.FirstFeature()
    while f is not None:
        kind = maybe(lambda: f.GetTypeName2(), "?")
        entry = {"name": f.Name, "type": kind, "suppressed": bool(maybe(lambda: f.IsSuppressed(), False))}
        try:
            entry.update(read_feature(model, f, kind))
        except Exception as e:  # noqa: BLE001
            entry["error"] = f"{type(e).__name__}: {e}"
        features.append(entry)
        f = maybe(lambda: f.GetNextFeature())
    step = f"{name}.step"
    try:
        save_as(model, os.path.join(out_dir, step))
    except Exception as e:  # noqa: BLE001
        warnings.append(str(e))
        step = None
    vol, com = mass_props(model)
    return {"format": DUMP_FORMAT, "kind": "part", "source": path, "name": name, "step": step,
            "volume_m3": vol, "center_of_mass_m": com, "features": features, "warnings": warnings}


def dump_assembly(sw, model, path, out_dir, name, done):
    comps = []
    warnings = []
    asm = cast(model, "IAssemblyDoc")
    for c in seq(asm.GetComponents(False)):
        c = cast(c, "IComponent2")
        cpath = maybe(lambda: c.GetPathName(), "")
        if not cpath.lower().endswith(".sldprt"):
            continue  # sub-assemblies: their parts are listed too (flattened)
        stem = os.path.splitext(os.path.basename(cpath))[0]
        if cpath not in done:
            done[cpath] = None
            try:
                pm, _ = open_doc(sw, cpath)
                d = dump_part(sw, pm, cpath, out_dir, stem)
                write_json(os.path.join(out_dir, f"{stem}.swdump.json"), d)
                done[cpath] = f"{stem}.swdump.json"
            except Exception as e:  # noqa: BLE001
                warnings.append(f"{cpath}: {e}")
        comps.append({"name": c.Name2, "path": cpath, "dump": done.get(cpath),
                      "transform": transform_array(c.Transform2),
                      "fixed": bool(maybe(lambda: c.IsFixed(), False)),
                      "suppressed": bool(maybe(lambda: c.IsSuppressed(), False))})
    mates = []
    f = model.FirstFeature()
    while f is not None:
        if maybe(lambda: f.GetTypeName2()) == "MateGroup":
            m = maybe(lambda: f.GetFirstSubFeature())
            while m is not None:
                try:
                    mates.append(read_mate(m))
                except Exception as e:  # noqa: BLE001
                    warnings.append(f"mate {maybe(lambda: m.Name)}: {e}")
                m = maybe(lambda: m.GetNextSubFeature())
        f = maybe(lambda: f.GetNextFeature())
    step = f"{name}.step"
    try:
        save_as(model, os.path.join(out_dir, step))
    except Exception as e:  # noqa: BLE001
        warnings.append(str(e))
        step = None
    vol, com = mass_props(model)
    return {"format": DUMP_FORMAT, "kind": "assembly", "source": path, "name": name, "step": step,
            "volume_m3": vol, "center_of_mass_m": com, "components": comps, "mates": mates, "warnings": warnings}


def read_mate(feat):
    mate = cast(feat.GetSpecificFeature2(), "IMate2")
    out = {"name": feat.Name, "type": int(mate.Type), "align": int(maybe(lambda: mate.Alignment, 2)),
           "suppressed": bool(maybe(lambda: feat.IsSuppressed(), False)), "entities": []}
    dd = maybe(lambda: mate.DisplayDimension)
    if dd is not None:
        out["value"] = maybe(lambda: float(cast(dd.GetDimension2(0), "IDimension").SystemValue))
    for i in range(int(mate.GetMateEntityCount())):
        me = cast(mate.MateEntity(i), "IMateEntity2")
        comp = cast(me.ReferenceComponent, "IComponent2")
        ent = {"component": comp.Name2, "ref_type": int(me.ReferenceType2),
               "params": [float(v) for v in seq(maybe(lambda: me.EntityParams))]}
        ref = maybe(lambda: me.Reference)
        if ent["ref_type"] in (4, 5):  # datum plane / axis
            ent["ref_name"] = maybe(lambda: cast(ref, "IFeature").Name)
        elif ref is not None:
            ent["point"] = maybe(lambda: face_point(ref))
        out["entities"].append(ent)
    return out


def write_json(path, data):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=1)


# ---------------------------------------------------------------- commands

def cmd_dump(src, out_dir):
    src = os.path.abspath(src)
    os.makedirs(out_dir, exist_ok=True)
    sw = connect()
    model, doc_type = open_doc(sw, src)
    name = os.path.splitext(os.path.basename(src))[0]
    if doc_type == SW_DOC_PART:
        data = dump_part(sw, model, src, out_dir, name)
    else:
        data = dump_assembly(sw, model, src, out_dir, name, {})
    out = os.path.join(out_dir, f"{name}.swdump.json")
    write_json(out, data)
    return {"ok": True, "dump": os.path.abspath(out)}


def cmd_export(step, target):
    step, target = os.path.abspath(step), os.path.abspath(target)
    sw = connect()
    err = byref_long()
    model = None
    for a in (lambda: sw.LoadFile4(step, "r", None, err), lambda: sw.LoadFile4(step, "r", None),
              lambda: sw.LoadFile2(step, "r")):
        model = maybe(lambda: first(a()))
        if model is not None:
            break
    if model is None:
        raise RuntimeError(f"SOLIDWORKS could not import {step}")
    save_as(model, target)
    maybe(lambda: sw.CloseDoc(model.GetTitle()))
    return {"ok": True, "saved": target}


def main(argv):
    if len(argv) != 4 or argv[1] not in ("dump", "export"):
        print(__doc__)
        return 2
    try:
        res = cmd_dump(argv[2], argv[3]) if argv[1] == "dump" else cmd_export(argv[2], argv[3])
    except Exception as e:  # noqa: BLE001
        traceback.print_exc(file=sys.stderr)
        res = {"ok": False, "error": f"{type(e).__name__}: {e}"}
    print(json.dumps(res))
    return 0 if res.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
