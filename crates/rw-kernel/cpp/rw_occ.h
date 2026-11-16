// Small C++ shim for OpenCASCADE operations that `opencascade-sys` does not
// bind: drafts, B-spline sketch edges, swept solids, face geometry queries,
// compounds and general rigid transforms.
#pragma once
#include "rust/cxx.h"
#include <TopoDS_Edge.hxx>
#include <TopoDS_Shape.hxx>
#include <TopoDS_Wire.hxx>
#include <memory>

std::unique_ptr<TopoDS_Edge> rw_bezier_edge(rust::Slice<const double> poles);
std::unique_ptr<TopoDS_Shape> rw_draft(const TopoDS_Shape &s, rust::Slice<const int32_t> faces,
                                       rust::Slice<const double> pull, double angle_rad,
                                       rust::Slice<const double> neutral);
std::unique_ptr<TopoDS_Shape> rw_sweep(const TopoDS_Wire &spine, const TopoDS_Wire &profile);
int32_t rw_face_geom(const TopoDS_Shape &s, int32_t face, rust::Slice<double> out);
std::unique_ptr<TopoDS_Shape> rw_compound_new();
void rw_compound_add(TopoDS_Shape &c, const TopoDS_Shape &s);
std::unique_ptr<TopoDS_Shape> rw_transform(const TopoDS_Shape &s, rust::Slice<const double> m);
