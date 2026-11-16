//! Tests with dumps shaped like the bridge's output (SI units, SOLIDWORKS
//! enums, row-convention transforms), so the translation is checked without
//! SOLIDWORKS.

use super::*;
use dump::{Component, Dim, Dump, EdgeFeature, Feature, Mate, MateEntity, Point, Segment};
use rw_core::{kernel, sketch, AsmModel, FeatureKind, Regenerator};
use std::f64::consts::PI;

/// SOLIDWORKS Top plane (sketch x → +X, sketch y → −Z), offset `h` metres up.
fn top(h: f64) -> Vec<f64> {
    vec![1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 0.0, h, 0.0, 1.0]
}

fn pt(id: &str, x: f64, y: f64) -> Point {
    Point { id: id.into(), x, y }
}

fn line(id: &str, a: &str, b: &str) -> Segment {
    Segment { id: id.into(), kind: "line".into(), start: Some(a.into()), end: Some(b.into()), ..Default::default() }
}

fn circle(id: &str, c: &str, r: f64) -> Segment {
    Segment { id: id.into(), kind: "circle".into(), center: Some(c.into()), radius: Some(r), ..Default::default() }
}

fn feat(name: &str, kind: &str) -> Feature {
    Feature { name: name.into(), kind: kind.into(), ..Default::default() }
}

/// 60 × 40 × 10 mm plate with a Ø12 hole (sketch), a Ø8 through-all cut at
/// x = 20 and one 2 mm vertical-edge fillet, as SOLIDWORKS would report it.
fn bracket_dump(cut_x: f64) -> Dump {
    let mut s1 = feat("Sketch1", "ProfileFeature");
    s1.sketch = Some(dump::Sketch {
        to_model: top(0.0),
        points: vec![pt("1-1", -0.03, -0.02), pt("1-2", 0.03, -0.02), pt("1-3", 0.03, 0.02), pt("1-4", -0.03, 0.02), pt("1-5", 0.0, 0.0)],
        segments: vec![line("0-1", "1-1", "1-2"), line("0-2", "1-2", "1-3"), line("0-3", "1-3", "1-4"), line("0-4", "1-4", "1-1"), circle("0-5", "1-5", 0.006)],
        dims: vec![Dim { kind: 2, value: 0.06, entities: vec!["0-1".into()] }, Dim { kind: 6, value: 0.012, entities: vec!["0-5".into()] }],
    });
    let mut e1 = feat("Boss-Extrude1", "Extrusion");
    e1.profile = Some("Sketch1".into());
    e1.extrude = Some(dump::Extrude { end1: 0, depth1: 0.01, ..Default::default() });
    let mut s2 = feat("Sketch2", "ProfileFeature");
    s2.sketch = Some(dump::Sketch { to_model: top(0.01), points: vec![pt("1-9", cut_x, 0.0)], segments: vec![circle("0-9", "1-9", 0.004)], dims: vec![] });
    // through-all along the sketch normal (+Y) cuts nothing: the translator must flip it
    let mut c1 = feat("Cut-Extrude1", "Cut");
    c1.profile = Some("Sketch2".into());
    c1.extrude = Some(dump::Extrude { end1: 1, ..Default::default() });
    let mut f1 = feat("Fillet1", "Fillet");
    f1.fillet = Some(EdgeFeature { size: 0.002, edges: vec![[0.03, 0.005, 0.02]], ..Default::default() });
    let mut features = vec![feat("Comments", "CommentsFolder"), feat("Front Plane", "RefPlane"), feat("Origin", "OriginProfileFeature")];
    features.extend([s1, e1, s2, c1, f1]);
    let vol = 60.0 * 40.0 * 10.0 - PI * 36.0 * 10.0 - PI * 16.0 * 10.0 - (4.0 - PI) * 10.0;
    let com_x = -(PI * 16.0 * 10.0 * 20.0) / vol;
    Dump {
        format: dump::DUMP_FORMAT.into(),
        kind: "part".into(),
        name: "bracket".into(),
        volume_m3: Some(vol * 1e-9),
        center_of_mass_m: Some([com_x * 1e-3, 0.005, 0.0]),
        features,
        ..Default::default()
    }
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustworks-sw-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn part_history_is_rebuilt_and_verified() {
    let dir = tmp("part");
    let d = bracket_dump(0.02);
    let (rep, conv) = translate_part(&d, &dir, &dir).unwrap();
    assert_eq!(rep.mode, "history", "{:?}", rep.notes);
    assert_eq!(rep.features_translated, 5);
    assert_eq!(conv, Some(Convention::Rows));
    let part = rw_core::model::read_part(&rep.file).unwrap();
    // feature names and driving dimensions survive
    assert!(part.feature("Boss-Extrude1").is_some() && part.feature("Fillet1").is_some());
    let FeatureKind::Sketch(sk) = &part.feature("Sketch1").unwrap().kind else { panic!() };
    assert!(sk.sketch.constraints.iter().any(|c| matches!(c, sketch::Constraint::Diameter { value, .. } if *value == sketch::Value::Num(12.0))));
    assert!(sk.sketch.constraints.iter().filter(|c| matches!(c, sketch::Constraint::Horizontal { .. })).count() == 2);
    let FeatureKind::Extrude(cut) = &part.feature("Cut-Extrude1").unwrap().kind else { panic!() };
    assert!(cut.reverse, "cut direction flipped into the material");
    // the result is editable: change the plate length dimension and rebuild
    let mut p2 = part.clone();
    if let FeatureKind::Sketch(sf) = &mut p2.feature_mut("Sketch1").unwrap().kind {
        for c in sf.sketch.constraints.iter_mut() {
            if let sketch::Constraint::Distance { value, .. } = c {
                *value = sketch::Value::Num(80.0);
            }
        }
    }
    let out = Regenerator::new().regenerate(&p2);
    let mp = out.mass.unwrap();
    assert!((mp.bbox_max[0] - mp.bbox_min[0] - 80.0).abs() < 1e-6, "{mp:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mismatch_or_unsupported_falls_back_to_step() {
    let dir = tmp("fallback");
    // a STEP file standing in for SOLIDWORKS' export of the same part
    let (good, _) = translate_part(&bracket_dump(0.02), &dir, &dir).unwrap();
    let part = rw_core::model::read_part(&good.file).unwrap();
    let body = Regenerator::new().regenerate(&part).body.unwrap();
    kernel::write_step(&body, &dir.join("bracket.step")).unwrap();

    // 1. history rebuilds but does not match SOLIDWORKS (cut on the wrong side)
    let mut d = bracket_dump(-0.02);
    d.center_of_mass_m = bracket_dump(0.02).center_of_mass_m;
    d.step = Some("bracket.step".into());
    let (rep, _) = translate_part(&d, &dir, &dir).unwrap();
    assert_eq!(rep.mode, "geometry");
    assert!(rep.notes[0].contains("centre of mass"), "{:?}", rep.notes);
    assert!((rep.rustworks_volume_mm3.unwrap() - rep.solidworks_volume_mm3.unwrap()).abs() < 1e-3);

    // 2. an unsupported feature type
    let mut d = bracket_dump(0.02);
    d.step = Some("bracket.step".into());
    d.features.push(feat("Dome1", "Dome"));
    let (rep, _) = translate_part(&d, &dir, &dir).unwrap();
    assert_eq!(rep.mode, "geometry");
    assert!(rep.notes[0].contains("Dome1") && rep.notes[0].contains("not supported"), "{:?}", rep.notes);

    // 3. unsupported and no STEP: a clear error
    let mut d = bracket_dump(0.02);
    d.features.push(feat("Dome1", "Dome"));
    let err = translate_part(&d, &dir, &dir.join("x")).unwrap_err().to_string();
    assert!(err.contains("no STEP"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assembly_with_placements_and_mates() {
    let dir = tmp("asm");
    std::fs::write(dir.join("bracket.swdump.json"), serde_json::to_string(&bracket_dump(0.02)).unwrap()).unwrap();
    let ident = |t: [f64; 3]| vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2], 1.0];
    let comp = |name: &str, transform: Vec<f64>, fixed: bool| Component {
        name: name.into(),
        path: "C:\\parts\\bracket.SLDPRT".into(),
        dump: Some("bracket.swdump.json".into()),
        transform,
        fixed,
        suppressed: false,
    };
    // c3: rotated 90° about +Y (row convention: rows are the images of x, y, z)
    let rot_y = vec![0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.2, 0.0, 0.0, 1.0];
    let ent = |c: &str, p: [f64; 3]| MateEntity { component: c.into(), ref_type: 2, point: Some(p), ..Default::default() };
    let d = Dump {
        format: dump::DUMP_FORMAT.into(),
        kind: "assembly".into(),
        name: "stack".into(),
        components: vec![comp("bracket-1", ident([0.0; 3]), true), comp("bracket-2", ident([0.0, 0.01, 0.0]), false), comp("bracket-3", rot_y, false)],
        mates: vec![
            // top face of bracket-1 against the bottom face of bracket-2 (assembly coordinates)
            Mate { name: "Coincident1".into(), kind: 0, align: 1, entities: vec![ent("bracket-1", [-0.02, 0.01, 0.01]), ent("bracket-2", [-0.02, 0.01, 0.01])], ..Default::default() },
            // the Ø12 holes, given in part coordinates (the bridge may report either space)
            Mate { name: "Concentric1".into(), kind: 1, align: 2, entities: vec![ent("bracket-1", [0.006, 0.005, 0.0]), ent("bracket-2", [0.006, 0.005, 0.0])], ..Default::default() },
            Mate { name: "Gear1".into(), kind: 10, entities: vec![], ..Default::default() },
        ],
        ..Default::default()
    };
    std::fs::write(dir.join("stack.swdump.json"), serde_json::to_string(&d).unwrap()).unwrap();
    let rep = match import(&dir.join("stack.swdump.json"), &dir.join("out")).unwrap() {
        ImportReport::Assembly(a) => a,
        r => panic!("{r:?}"),
    };
    assert_eq!(rep.components, 3);
    assert_eq!(rep.parts.len(), 1, "one part file shared by three components");
    assert_eq!(rep.parts[0].mode, "history");
    assert_eq!(rep.mates_translated, 2);
    assert!(rep.notes.iter().any(|n| n.contains("Gear1")), "{:?}", rep.notes);
    assert!(!rep.notes.iter().any(|n| n.contains("after import")), "{:?}", rep.notes);
    let m = AsmModel::open(&rep.file).unwrap();
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    let c2 = m.asm.component("bracket-2").unwrap();
    assert!((c2.position[1] - 10.0).abs() < 1e-6, "{:?}", c2.position);
    let c3 = m.asm.component("bracket-3").unwrap();
    assert!((c3.rotation[1] - 90.0).abs() < 1e-6 && (c3.position[0] - 200.0).abs() < 1e-6, "{:?} {:?}", c3.rotation, c3.position);
    // bracket-2 can only spin about the shared hole axis
    assert_eq!(m.out.solve.dof, 1 + 6, "{:?}", m.out.solve);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_files_need_solidworks_here() {
    let err = import(Path::new("bracket.SLDPRT"), &std::env::temp_dir()).unwrap_err().to_string();
    if cfg!(windows) {
        assert!(err.contains("bridge") || err.contains("python") || err.contains("SOLIDWORKS"), "{err}");
    } else {
        assert!(err.contains("Windows") && err.contains(".swdump.json"), "{err}");
    }
    assert!(import(Path::new("x.slddrw"), &std::env::temp_dir()).unwrap_err().to_string().contains("drawings"));
}

