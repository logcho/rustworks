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
