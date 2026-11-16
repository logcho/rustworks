// Compiles the small C++ shim (cpp/rw_occ.cpp) against the OpenCASCADE
// headers that `opencascade-sys` (feature `builtin`) installs into
// target/OCCT. Only needed for the default `occt` backend.

fn main() {
    if std::env::var_os("CARGO_FEATURE_OCCT").is_none() {
        return;
    }
    // occt-sys installs OCCT at <target>/OCCT, i.e. four levels above any OUT_DIR.
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let occt = std::env::var("RW_OCCT_INCLUDE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| out.join("../../../../OCCT/include"));
    // opencascade-sys builds OCCT in its own build script, which Cargo may run
    // concurrently with this one on a clean build: wait for the install.
    let ready = occt.join("BRepOffsetAPI_DraftAngle.hxx");
    let t0 = std::time::Instant::now();
    while !ready.exists() {
        if t0.elapsed().as_secs() > 3600 {
            panic!("OpenCASCADE headers not found at {} (set RW_OCCT_INCLUDE)", occt.display());
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    cxx_build::bridge("src/occ_ext.rs")
        .file("cpp/rw_occ.cpp")
        .include(&occt)
        .std("c++17")
        .define("_USE_MATH_DEFINES", "TRUE")
        .flag_if_supported("-w")
        .compile("rw-occ-shim");
    // Hidden-line removal lives in TKHLR, which opencascade-sys does not link.
    println!("cargo:rustc-link-search=native={}", occt.join("../lib").display());
    println!("cargo:rustc-link-lib=static=TKHLR");
    println!("cargo:rerun-if-changed=src/occ_ext.rs");
    println!("cargo:rerun-if-changed=cpp/rw_occ.cpp");
    println!("cargo:rerun-if-changed=cpp/rw_occ.h");
    println!("cargo:rerun-if-env-changed=RW_OCCT_INCLUDE");
}
