#include "rw-kernel/cpp/rw_occ.h"

#include <BRepAdaptor_Surface.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepOffsetAPI_DraftAngle.hxx>
#include <BRepOffsetAPI_MakePipeShell.hxx>
#include <BRepGProp.hxx>
#include <BRep_Builder.hxx>
#include <GProp_GProps.hxx>
#include <Geom_BSplineCurve.hxx>
#include <Standard_Failure.hxx>
#include <TColStd_Array1OfInteger.hxx>
#include <TColStd_Array1OfReal.hxx>
#include <TColgp_Array1OfPnt.hxx>
#include <TopExp.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Compound.hxx>
#include <TopoDS_Face.hxx>
#include <gp_Cone.hxx>
#include <gp_Cylinder.hxx>
#include <gp_Pln.hxx>
#include <gp_Sphere.hxx>
#include <gp_Trsf.hxx>
#include <stdexcept>
#include <string>

namespace {
[[noreturn]] void fail(const std::string &msg) { throw std::runtime_error(msg); }

std::string occ_msg(const Standard_Failure &e) {
  const char *m = e.GetMessageString();
  return (m && *m) ? std::string(m) : std::string(e.DynamicType()->Name());
}
} // namespace

// Piecewise cubic Bezier chain (3n+1 poles, xyz flattened) as one C1 B-spline edge.
std::unique_ptr<TopoDS_Edge> rw_bezier_edge(rust::Slice<const double> p) {
  int np = int(p.size() / 3);
  if (np < 4 || (np - 1) % 3 != 0) fail("spline needs 3n+1 control points");
  int nseg = (np - 1) / 3;
  try {
    TColgp_Array1OfPnt poles(1, np);
    for (int i = 0; i < np; i++) poles.SetValue(i + 1, gp_Pnt(p[3 * i], p[3 * i + 1], p[3 * i + 2]));
    TColStd_Array1OfReal knots(1, nseg + 1);
    TColStd_Array1OfInteger mults(1, nseg + 1);
    for (int i = 0; i <= nseg; i++) {
      knots.SetValue(i + 1, double(i));
      mults.SetValue(i + 1, (i == 0 || i == nseg) ? 4 : 3);
    }
    Handle(Geom_BSplineCurve) c = new Geom_BSplineCurve(poles, knots, mults, 3);
    BRepBuilderAPI_MakeEdge mk(c);
    if (!mk.IsDone()) fail("could not build spline edge");
    return std::make_unique<TopoDS_Edge>(mk.Edge());
  } catch (const Standard_Failure &e) {
    fail("spline: " + occ_msg(e));
  }
}

std::unique_ptr<TopoDS_Shape> rw_draft(const TopoDS_Shape &s, rust::Slice<const int32_t> faces,
                                       rust::Slice<const double> pull, double angle,
                                       rust::Slice<const double> nt) {
  if (pull.size() != 3 || nt.size() != 6) fail("draft: bad direction/plane");
  try {
    TopTools_IndexedMapOfShape map;
    TopExp::MapShapes(s, TopAbs_FACE, map);
    BRepOffsetAPI_DraftAngle d(s);
    gp_Dir dir(pull[0], pull[1], pull[2]);
    gp_Pln pln(gp_Pnt(nt[0], nt[1], nt[2]), gp_Dir(nt[3], nt[4], nt[5]));
    for (int32_t i : faces) {
      if (i < 0 || i >= map.Extent()) fail("draft: face " + std::to_string(i) + " does not exist");
      d.Add(TopoDS::Face(map(i + 1)), dir, angle, pln);
      if (!d.AddDone())
        fail("draft cannot tilt face " + std::to_string(i) +
             " (pick side faces that cross the neutral plane, not faces parallel to it)");
    }
    d.Build();
    if (!d.IsDone()) fail("draft failed (angle too large for the neighbouring faces?)");
    return std::make_unique<TopoDS_Shape>(d.Shape());
  } catch (const Standard_Failure &e) {
    fail("draft: " + occ_msg(e));
  }
}

std::unique_ptr<TopoDS_Shape> rw_sweep(const TopoDS_Wire &spine, const TopoDS_Wire &profile) {
  try {
    BRepOffsetAPI_MakePipeShell mk(spine);
    mk.SetMode(false); // corrected Frenet: no twisting along planar paths
    mk.SetTransitionMode(BRepBuilderAPI_RoundCorner);
    mk.Add(profile, false, false);
    mk.Build();
    if (!mk.IsDone()) fail("sweep failed (does the path self-intersect or turn tighter than the profile?)");
    if (!mk.MakeSolid()) fail("sweep could not close the swept shell into a solid");
    TopoDS_Shape out = mk.Shape();
    GProp_GProps g;
    BRepGProp::VolumeProperties(out, g);
    if (g.Mass() < 0) out.Reverse(); // profile orientation can yield an inside-out solid
    return std::make_unique<TopoDS_Shape>(out);
  } catch (const Standard_Failure &e) {
    fail("sweep: " + occ_msg(e));
  }
}

// Writes analytic face geometry into `out` (>= 7 doubles) and returns its kind:
// 0 other, 1 plane (origin, outward normal), 2 cylinder (axis origin, axis dir, radius),
// 3 cone (apex, axis dir, half-angle), 4 sphere (center, -, radius).
int32_t rw_face_geom(const TopoDS_Shape &s, int32_t face, rust::Slice<double> out) {
  if (out.size() < 7) fail("face_geom: output too small");
  TopTools_IndexedMapOfShape map;
  TopExp::MapShapes(s, TopAbs_FACE, map);
  if (face < 0 || face >= map.Extent()) fail("face " + std::to_string(face) + " does not exist");
  const TopoDS_Face &f = TopoDS::Face(map(face + 1));
  try {
    BRepAdaptor_Surface a(f, true);
    auto put = [&](const gp_Pnt &p, const gp_Dir &d, double v) {
      out[0] = p.X(); out[1] = p.Y(); out[2] = p.Z();
      out[3] = d.X(); out[4] = d.Y(); out[5] = d.Z();
      out[6] = v;
    };
    switch (a.GetType()) {
    case GeomAbs_Plane: {
      gp_Pln pl = a.Plane();
      gp_Dir n = pl.Axis().Direction();
      if (f.Orientation() == TopAbs_REVERSED) n.Reverse();
      put(pl.Location(), n, 0.0);
      return 1;
    }
    case GeomAbs_Cylinder: {
      gp_Cylinder c = a.Cylinder();
      put(c.Location(), c.Axis().Direction(), c.Radius());
      return 2;
    }
    case GeomAbs_Cone: {
      gp_Cone c = a.Cone();
      put(c.Apex(), c.Axis().Direction(), c.SemiAngle());
      return 3;
    }
    case GeomAbs_Sphere: {
      gp_Sphere sp = a.Sphere();
      put(sp.Location(), gp_Dir(0, 0, 1), sp.Radius());
      return 4;
    }
    default:
      return 0;
    }
  } catch (const Standard_Failure &e) {
    fail("face_geom: " + occ_msg(e));
  }
}

std::unique_ptr<TopoDS_Shape> rw_compound_new() {
  TopoDS_Compound c;
  BRep_Builder b;
  b.MakeCompound(c);
  return std::make_unique<TopoDS_Shape>(c);
}

void rw_compound_add(TopoDS_Shape &c, const TopoDS_Shape &s) {
  BRep_Builder b;
  b.Add(c, s);
}

// Rigid transform from a row-major 3x4 matrix (rotation | translation).
std::unique_ptr<TopoDS_Shape> rw_transform(const TopoDS_Shape &s, rust::Slice<const double> m) {
  if (m.size() != 12) fail("transform needs 12 values");
  try {
    gp_Trsf t;
    t.SetValues(m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9], m[10], m[11]);
    BRepBuilderAPI_Transform tr(s, t, true);
    return std::make_unique<TopoDS_Shape>(tr.Shape());
  } catch (const Standard_Failure &e) {
    fail("transform: " + occ_msg(e));
  }
}
