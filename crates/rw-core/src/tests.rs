use crate::*;
use rw_sketch::{Constraint, Sketch, Value};
use std::f64::consts::PI;

fn sketch_feature(part: &mut Part, plane: PlaneRef, sk: Sketch) -> String {
    let (id, name) = part.fresh_ids("Sketch");
    part.features.push(Feature { id: id.clone(), name, suppressed: false, kind: FeatureKind::Sketch(SketchFeature { plane, sketch: sk }) });
    id
}

fn push(part: &mut Part, base: &str, kind: FeatureKind) -> String {
    let (id, name) = part.fresh_ids(base);
    part.features.push(Feature { id: id.clone(), name, suppressed: false, kind });
    id
}

fn extrude(sketch: &str, depth: f64, end: EndCondition, op: Op) -> FeatureKind {
    FeatureKind::Extrude(Extrude { sketch: sketch.into(), depth: depth.into(), end, reverse: false, op })
}

fn bracket() -> Part {
    let mut p = Part::new("bracket");
    p.parameters.insert("width".into(), Value::Num(80.0));
    let mut sk = Sketch::default();
    let l = sk.add_rectangle([-40.0, -25.0], [40.0, 25.0]);
    sk.constraints.push(Constraint::Distance { a: l[0].clone(), b: None, value: Value::Expr("width".into()) });
    let s1 = sketch_feature(&mut p, PlaneRef::Named(StdPlane::Top), sk);
    push(&mut p, "Extrude", extrude(&s1, 10.0, EndCondition::Blind, Op::Boss));

    let mut sk = Sketch::default();
    sk.add_circle([0.0, 0.0], 15.0, false);
    let s2 = sketch_feature(&mut p, PlaneRef::Offset { base: StdPlane::Top, offset: 10.0.into() }, sk);
    push(&mut p, "Extrude", extrude(&s2, 20.0, EndCondition::Blind, Op::Boss));

    let mut sk = Sketch::default();
    sk.add_circle([0.0, 0.0], 8.0, false);
    let s3 = sketch_feature(&mut p, PlaneRef::Offset { base: StdPlane::Top, offset: 30.0.into() }, sk);
    let mut cut = extrude(&s3, 0.0, EndCondition::ThroughAll, Op::Cut);
    if let FeatureKind::Extrude(e) = &mut cut {
        e.reverse = true;
    }
    push(&mut p, "Cut-Extrude", cut);

    let h = push(
        &mut p,
        "Hole",
        FeatureKind::Hole(Hole {
            plane: PlaneRef::Offset { base: StdPlane::Top, offset: 10.0.into() },
            points: vec![[-30.0, -17.5]],
            diameter: 6.0.into(),
            depth: None,
            counterbore: Some(Counterbore { diameter: 10.0.into(), depth: 3.0.into() }),
            countersink: None,
        }),
    );
    push(
        &mut p,
        "LPattern",
        FeatureKind::LinearPattern(LinearPattern {
            features: vec![h],
            direction: [1.0, 0.0, 0.0],
            spacing: 60.0.into(),
            count: 2.0.into(),
            direction2: Some([0.0, 0.0, -1.0]),
            spacing2: Some(35.0.into()),
            count2: Some(2.0.into()),
        }),
    );
    p
}

fn assert_vol(out: &RegenOutput, expect: f64, rel: f64) {
    assert!(out.errors().is_empty(), "errors: {:?}", out.errors());
    let v = out.mass.unwrap().volume;
    assert!((v - expect).abs() / expect < rel, "volume {v} vs expected {expect}");
}

#[test]
fn bracket_regenerates_with_correct_volume() {
    let part = bracket();
    let mut m = Model::new(part);
    let hole = PI * 25.0 * 3.0 + PI * 9.0 * 7.0;
    let expect = 40000.0 + PI * 225.0 * 20.0 - PI * 64.0 * 30.0 - 4.0 * hole;
    assert_vol(&m.out, expect, 5e-3);
    println!("full regen: {:.1} ms, {} tris", m.out.millis, m.out.mesh.as_ref().unwrap().triangle_count());

    // Parametric change: only features from the first sketch on rebuild... the
    // width param affects sketch 1, so everything rebuilds. Changing the last
    // feature should hit the cache for all earlier ones.
    m.edit(|p| {
        if let FeatureKind::LinearPattern(lp) = &mut p.feature_mut("LPattern1").unwrap().kind {
            lp.count = 1.0.into();
            lp.count2 = Some(1.0.into());
        }
        Ok(())
    })
    .unwrap();
    let n = m.part.features.len();
    assert_eq!(m.out.cache_hits, n - 1, "incremental regen should reuse all but the last feature");
    assert_vol(&m.out, expect + 3.0 * hole, 5e-3);
    println!("incremental regen: {:.1} ms", m.out.millis);

    // Parameter-driven change through the equation.
    m.edit(|p| {
        p.parameters.insert("width".into(), Value::Num(100.0));
        Ok(())
    })
    .unwrap();
    assert_vol(&m.out, expect + 3.0 * hole + 20.0 * 50.0 * 10.0, 5e-3);
    assert!(m.undo());
    assert_vol(&m.out, expect + 3.0 * hole, 5e-3);
}

#[test]
fn revolve_touching_axis_and_mirror() {
    let mut p = Part::new("shaft");
    let mut sk = Sketch::default();
    let pts = [[0.0, 0.0], [10.0, 0.0], [10.0, 30.0], [5.0, 30.0], [5.0, 50.0], [0.0, 50.0]];
    for i in 0..pts.len() {
        sk.add_line(pts[i], pts[(i + 1) % pts.len()], false);
    }
    let s = sketch_feature(&mut p, PlaneRef::Named(StdPlane::Front), sk);
    push(
        &mut p,
        "Revolve",
        FeatureKind::Revolve(Revolve { sketch: s, axis: AxisRef::Entity("y_axis".into()), angle: 360.0.into(), op: Op::Boss }),
    );
    let out = Regenerator::new().regenerate(&p);
    let expect = PI * 100.0 * 30.0 + PI * 25.0 * 20.0;
    assert_vol(&out, expect, 5e-3);

    push(&mut p, "Mirror", FeatureKind::Mirror(Mirror { features: vec![], plane: PlaneRef::Named(StdPlane::Top) }));
    let out = Regenerator::new().regenerate(&p);
    assert_vol(&out, 2.0 * expect, 5e-3);
}

#[test]
fn errors_are_reported_per_feature_and_roundtrip() {
    let mut p = bracket();
    push(&mut p, "Extrude", extrude("nope", 5.0, EndCondition::Blind, Op::Boss));
    let out = Regenerator::new().regenerate(&p);
    assert_eq!(out.errors().len(), 1);
    assert!(out.body.is_some(), "earlier features still produce a body");
    let json = serde_json::to_string_pretty(&p).unwrap();
    let back: Part = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

#[test]
#[ignore]
fn debug_prefix_volumes() {
    let p = bracket();
    for n in 1..=p.features.len() {
        let mut q = p.clone();
        q.features.truncate(n);
        let t = std::time::Instant::now();
        let out = Regenerator::new().regenerate(&q);
        println!("{} {:?} vol={:?} warn={:?} {:.0}ms", n, q.features[n - 1].name, out.mass.map(|m| m.volume), out.status[n - 1].warning, t.elapsed().as_secs_f64()*1e3);
    }
}

#[test]
fn live_sync_between_two_models() {
    let dir = std::env::temp_dir().join(format!("rw-sync-{}", std::process::id()));
    let path = dir.join("p.rwpart");
    let mut a = Model::new(bracket());
    a.save_as(&path).unwrap();
    let mut b = Model::open(&path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    a.edit(|p| {
        p.parameters.insert("width".into(), Value::Num(90.0));
        Ok(())
    })
    .unwrap();
    assert!(b.sync_from_disk().unwrap(), "b should pick up a's edit");
    assert_eq!(b.part.parameters["width"], Value::Num(90.0));
    assert!(!b.sync_from_disk().unwrap(), "no change -> no reload");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fillet_shell_chamfer_with_resnapping_refs() {
    let mut p = Part::new("box");
    p.parameters.insert("w".into(), Value::Num(40.0));
    let mut sk = Sketch::default();
    let l = sk.add_rectangle([-20.0, -20.0], [20.0, 20.0]);
    sk.constraints.push(Constraint::Distance { a: l[0].clone(), b: None, value: Value::Expr("w".into()) });
    sk.constraints.push(Constraint::Distance { a: l[1].clone(), b: None, value: Value::Expr("w".into()) });
    let s = sketch_feature(&mut p, PlaneRef::Named(StdPlane::Top), sk);
    push(&mut p, "Extrude", extrude(&s, 30.0, EndCondition::Blind, Op::Boss));
    // vertical corner edge at (20, *, -20) (top-plane sketch y maps to -z)
    push(&mut p, "Fillet", FeatureKind::Fillet(Fillet { edges: vec![[20.0, 15.0, -20.0].into()], all_edges: false, radius: 5.0.into() }));
    push(&mut p, "Shell", FeatureKind::Shell(Shell { open_faces: vec![[0.0, 30.0, 0.0]], thickness: 2.0.into() }));
    let mut m = Model::new(p);
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    let v = m.out.mass.unwrap().volume;
    assert!(v > 5000.0 && v < 48000.0 * 0.5, "shelled volume {v}");
    // snapped reference = edge midpoint
    let FeatureKind::Fillet(fl) = &m.part.feature("Fillet1").unwrap().kind else { panic!() };
    assert!((fl.edges[0].at()[1] - 15.0).abs() < 1e-6, "{:?}", fl.edges);
    // Grow the box: the fillet reference must follow the moved edge.
    m.edit(|p| {
        p.parameters.insert("w".into(), Value::Num(44.0));
        Ok(())
    })
    .unwrap();
    assert!(m.out.errors().is_empty(), "after resize: {:?}", m.out.errors());
    let FeatureKind::Fillet(fl) = &m.part.feature("Fillet1").unwrap().kind else { panic!() };
    assert!((fl.edges[0].at()[0] - 22.0).abs() < 1e-3, "reference re-snapped: {:?}", fl.edges);
    // Chamfer the open rim's outer edge.
    m.edit(|p| {
        p.features.push(Feature { id: "fc".into(), name: "Chamfer1".into(), suppressed: false,
            kind: FeatureKind::Chamfer(Chamfer { edges: vec![[0.0, 30.0, -22.0].into()], all_edges: false, distance: 0.5.into() }) });
        Ok(())
    })
    .unwrap();
    assert!(m.out.errors().is_empty(), "chamfer: {:?}", m.out.errors());
}

#[test]
fn loft_and_step_roundtrip() {
    let mut p = Part::new("loft");
    let mut a = Sketch::default();
    a.add_rectangle([-20.0, -20.0], [20.0, 20.0]);
    let s1 = sketch_feature(&mut p, PlaneRef::Named(StdPlane::Top), a);
    let mut b = Sketch::default();
    b.add_rectangle([-10.0, -10.0], [10.0, 10.0]);
    let s2 = sketch_feature(&mut p, PlaneRef::Offset { base: StdPlane::Top, offset: 30.0.into() }, b);
    push(&mut p, "Loft", FeatureKind::Loft(Loft { sketches: vec![s1, s2], op: Op::Boss }));
    let out = Regenerator::new().regenerate(&p);
    // frustum of a square pyramid: h/3 (A1 + A2 + sqrt(A1 A2))
    let expect = 30.0 / 3.0 * (1600.0 + 400.0 + 800.0);
    assert_vol(&out, expect, 1e-4);

    let path = std::env::temp_dir().join(format!("rw-loft-{}.step", std::process::id()));
    rw_kernel::write_step(out.body.as_ref().unwrap(), &path).unwrap();
    let mut q = Part::new("imported");
    push(&mut q, "Import", FeatureKind::Import(Import { path: path.display().to_string(), op: Op::Boss }));
    let out2 = Regenerator::new().regenerate(&q);
    assert_vol(&out2, expect, 1e-4);
    let _ = std::fs::remove_file(path);
}

#[test]
fn ref_plane_sweep_draft() {
    let mut p = Part::new("rp");
    let pl = push(&mut p, "Plane", FeatureKind::Plane(RefPlane { base: Some(PlaneRef::Named(StdPlane::Top)), offset: 5.0.into(), angle: None, points: None, flip: false }));
    let mut sk = Sketch::default();
    sk.add_rectangle([-10.0, -10.0], [10.0, 10.0]);
    let s = sketch_feature(&mut p, PlaneRef::Feature(pl.clone()), sk);
    push(&mut p, "Extrude", extrude(&s, 10.0, EndCondition::Blind, Op::Boss));
    let mut r = Regenerator::new();
    let out = r.regenerate(&p);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let mp = out.mass.unwrap();
    assert!((mp.volume - 4000.0).abs() < 1e-6 && (mp.bbox_min[1] - 5.0).abs() < 1e-3, "{mp:?}");
    assert_eq!(out.ref_planes.len(), 1);
    // draft the +X side face 5° hinged at the reference plane, pulling +Y
    push(&mut p, "Draft", FeatureKind::Draft(Draft { faces: vec![[10.0, 10.0, 0.0]], neutral: PlaneRef::Feature(pl.clone()), angle: 5.0.into(), reverse: false }));
    let out = r.regenerate(&p);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let v = out.mass.unwrap().volume;
    let wedge = 0.5 * 10.0 * 10.0 * 5f64.to_radians().tan() * 20.0;
    assert!((v - (4000.0 - wedge)).abs() < 1e-3 || (v - (4000.0 + wedge)).abs() < 1e-3, "drafted volume {v}, wedge {wedge}");
    // the plane cannot be deleted while used
    assert_eq!(p.dependents(&pl).len(), 2);

    // sweep a r=1 circle along a spline path
    let mut q = Part::new("sw");
    let mut path = Sketch::default();
    path.add_spline(&[[0.0, 0.0], [4.0, 10.0], [0.0, 20.0], [-4.0, 30.0]], false);
    let ps = sketch_feature(&mut q, PlaneRef::Named(StdPlane::Front), path);
    let mut prof = Sketch::default();
    prof.add_circle([0.0, 0.0], 1.0, false);
    let pr = sketch_feature(&mut q, PlaneRef::Named(StdPlane::Top), prof);
    push(&mut q, "Sweep", FeatureKind::Sweep(Sweep { profile: pr, path: ps, op: Op::Boss }));
    let out = Regenerator::new().regenerate(&q);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let v = out.mass.unwrap().volume;
    assert!(v > PI * 30.0 && v < PI * 40.0, "swept volume {v}");
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rustworks-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn assembly_mates_dof_interference() {
    use crate::assembly::*;
    let dir = tmpdir("asm");
    // plate 40x40x10 with a Ø10 through hole at the center
    let mut plate = Part::new("plate");
    let mut sk = Sketch::default();
    sk.add_rectangle([-20.0, -20.0], [20.0, 20.0]);
    let s = sketch_feature(&mut plate, PlaneRef::Named(StdPlane::Top), sk);
    push(&mut plate, "Extrude", extrude(&s, 10.0, EndCondition::Blind, Op::Boss));
    push(
        &mut plate,
        "Hole",
        FeatureKind::Hole(Hole {
            plane: PlaneRef::Offset { base: StdPlane::Top, offset: 10.0.into() },
            points: vec![[0.0, 0.0]],
            diameter: 10.0.into(),
            depth: None,
            counterbore: None,
            countersink: None,
        }),
    );
    model::write_part(&dir.join("plate.rwpart"), &plate).unwrap();
    // pin Ø12 x 30
    let mut pin = Part::new("pin");
    let mut sk = Sketch::default();
    sk.add_circle([0.0, 0.0], 6.0, false);
    let s = sketch_feature(&mut pin, PlaneRef::Named(StdPlane::Top), sk);
    push(&mut pin, "Extrude", extrude(&s, 30.0, EndCondition::Blind, Op::Boss));
    model::write_part(&dir.join("pin.rwpart"), &pin).unwrap();

    let mut a = Assembly::new("demo");
    let comp = |id: &str, path: &str, pos: [f64; 3], rot: [f64; 3]| Component {
        id: id.into(),
        name: format!("{id}<1>"),
        path: path.into(),
        position: pos,
        rotation: rot,
        fixed: false,
        suppressed: false,
        color: None,
    };
    a.components.push(comp("c1", "plate.rwpart", [0.0; 3], [0.0; 3]));
    a.components.push(comp("c2", "pin.rwpart", [50.0, 5.0, 20.0], [30.0, 0.0, 10.0]));
    let path = dir.join("demo.rwasm");
    let mut m = AsmModel::new(a, Some(path.clone()));
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    assert_eq!(m.out.solve.dof, 6);
    let r = |c: &str, f: [f64; 3]| MateRef { component: c.into(), face: Some(f), plane: None, axis: None, point: None };
    let mate = |kind, a, b| Mate { id: String::new(), name: String::new(), kind, a, b, value: None, aligned: false, suppressed: false };
    m.add_mate(mate(MateKind::Concentric, r("c1", [5.0, 5.0, 0.0]), r("c2", [6.0, 15.0, 0.0]))).unwrap();
    let top = m.add_mate(mate(MateKind::Coincident, r("c1", [15.0, 10.0, 15.0]), r("c2", [3.0, 0.0, 0.0]))).unwrap();
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    assert_eq!(m.out.solve.status, "under_defined", "{:?}", m.out.solve);
    assert_eq!(m.out.solve.dof, 1, "only spin about the pin axis remains");
    let p = m.out.placement("c2").unwrap();
    let o = p.point([0.0; 3]);
    assert!(o[0].abs() < 1e-5 && (o[1] - 10.0).abs() < 1e-5 && o[2].abs() < 1e-5, "pin origin {o:?}");
    let axis = p.dir([0.0, 1.0, 0.0]);
    assert!(axis[1] > 0.99999, "pin points up {axis:?}");
    assert!(m.out.interferences().unwrap().is_empty());
    // dragging the pin sideways can only spin it about its axis
    assert!(m.drag_preview("c2", [6.0, 15.0, 0.0], [50.0, 100.0, 0.0]));
    let p = m.out.placement("c2").unwrap();
    let o = p.point([0.0; 3]);
    assert!(o[0].abs() < 1e-3 && (o[1] - 10.0).abs() < 1e-3 && o[2].abs() < 1e-3, "pin stays mated {o:?}");
    let g = p.point([6.0, 15.0, 0.0]);
    assert!(g[0] > 5.99 && g[2].abs() < 1e-2, "grabbed point turned toward the target {g:?}");
    m.drag_commit().unwrap();
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    let g2 = m.out.placement("c2").unwrap().point([6.0, 15.0, 0.0]);
    assert!((g2[0] - g[0]).abs() < 1e-3, "committed {g2:?}");
    // the grounded plate does not move
    assert!(!m.drag_preview("c1", [0.0; 3], [100.0, 0.0, 0.0]));
    // push the pin through the plate: bottoms flush (aligned normals) → overlaps the Ø10 hole wall
    m.edit(|asm| {
        let mt = asm.mates.iter_mut().find(|x| x.id == top).unwrap();
        mt.a.face = Some([15.0, 0.0, 15.0]);
        mt.aligned = true;
        Ok(())
    })
    .unwrap();
    assert!(m.out.errors().is_empty(), "{:?}", m.out.errors());
    let o = m.out.placement("c2").unwrap().point([0.0; 3]);
    assert!(o[1].abs() < 1e-5, "pin bottom flush with plate bottom {o:?}");
    let hits = m.out.interferences().unwrap();
    assert_eq!(hits.len(), 1);
    assert!((hits[0].2 - PI * 11.0 * 10.0).abs() < 1e-3, "{hits:?}");
    // persistence + reload
    let back = assembly::read_assembly(&path).unwrap();
    assert_eq!(back.mates.len(), 2);
    let m2 = AsmModel::open(&path).unwrap();
    assert!(m2.out.errors().is_empty());
    let step = dir.join("demo.step");
    m2.export_step(&step).unwrap();
    assert!(std::fs::read_to_string(&step).unwrap().contains("ISO-10303-21"));
    let (v, _) = m2.out.mass();
    assert!((v - (16000.0 - PI * 250.0 + PI * 36.0 * 30.0)).abs() < 1e-3, "{v}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pattern_of_mixed_pattern_keeps_cuts() {
    // plate with a boss+hole cell, patterned (A), then A patterned again (B)
    let mut p = Part::new("grid");
    let mut sk = Sketch::default();
    sk.add_rectangle([0.0, 0.0], [100.0, 40.0]);
    let s = sketch_feature(&mut p, PlaneRef::Named(StdPlane::Top), sk);
    push(&mut p, "Extrude", extrude(&s, 2.0, EndCondition::Blind, Op::Boss));
    let mut sk = Sketch::default();
    sk.add_rectangle([4.0, 4.0], [8.0, 8.0]);
    let s = sketch_feature(&mut p, PlaneRef::Offset { base: StdPlane::Top, offset: 2.0.into() }, sk);
    let boss = push(&mut p, "Extrude", extrude(&s, 1.0, EndCondition::Blind, Op::Boss));
    let hole = push(
        &mut p,
        "Hole",
        FeatureKind::Hole(Hole { plane: PlaneRef::Offset { base: StdPlane::Top, offset: 3.0.into() }, points: vec![[15.0, 6.0]], diameter: 4.0.into(), depth: None, counterbore: None, countersink: None }),
    );
    let a = push(
        &mut p,
        "LPattern",
        FeatureKind::LinearPattern(LinearPattern { features: vec![boss, hole], direction: [1.0, 0.0, 0.0], spacing: 20.0.into(), count: 3.0.into(), direction2: None, spacing2: None, count2: None }),
    );
    push(
        &mut p,
        "LPattern",
        FeatureKind::LinearPattern(LinearPattern { features: vec![a], direction: [0.0, 0.0, -1.0], spacing: 20.0.into(), count: 2.0.into(), direction2: None, spacing2: None, count2: None }),
    );
    let out = Regenerator::new().regenerate(&p);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    // 1 seed + 2 (A) + 2 (B copies of A's instances) = 5 cells of each
    let hole_v = PI * 4.0 * 3.0; // Ø4 through plate+nothing: holes are in the 2 mm plate at x=15 (no boss there)
    let expected = 100.0 * 40.0 * 2.0 + 5.0 * 16.0 - 5.0 * PI * 4.0 * 2.0;
    let _ = hole_v;
    let v = out.mass.unwrap().volume;
    assert!((v - expected).abs() < 1e-3, "volume {v} vs {expected} (holes missing from the second pattern?)");
}
