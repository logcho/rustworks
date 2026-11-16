//! cxx bridge to the C++ shim in `cpp/rw_occ.cpp` (OCCT calls that
//! `opencascade-sys` does not expose).

#[cxx::bridge]
pub(crate) mod ext {
    unsafe extern "C++" {
        include!("rw-kernel/cpp/rw_occ.h");

        type TopoDS_Shape = opencascade_sys::topo_ds::TopoDS_Shape;
        type TopoDS_Edge = opencascade_sys::topo_ds::TopoDS_Edge;
        type TopoDS_Wire = opencascade_sys::topo_ds::TopoDS_Wire;

        fn rw_bezier_edge(poles: &[f64]) -> Result<UniquePtr<TopoDS_Edge>>;
        fn rw_draft(s: &TopoDS_Shape, faces: &[i32], pull: &[f64], angle_rad: f64, neutral: &[f64]) -> Result<UniquePtr<TopoDS_Shape>>;
        fn rw_sweep(spine: &TopoDS_Wire, profile: &TopoDS_Wire) -> Result<UniquePtr<TopoDS_Shape>>;
        fn rw_face_geom(s: &TopoDS_Shape, face: i32, out: &mut [f64]) -> Result<i32>;
        fn rw_compound_new() -> UniquePtr<TopoDS_Shape>;
        fn rw_compound_add(c: Pin<&mut TopoDS_Shape>, s: &TopoDS_Shape);
        fn rw_transform(s: &TopoDS_Shape, m: &[f64]) -> Result<UniquePtr<TopoDS_Shape>>;
    }
}
