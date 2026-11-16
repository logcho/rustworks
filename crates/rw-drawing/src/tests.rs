use super::*;
use rw_core::sketch::Sketch;
use rw_core::*;

/// 60 x 20 x 40 (x, y, z) block with two Ø8 through holes along Y.
fn block() -> Part {
    let mut p = Part::new("block");
    let mut sk = Sketch::default();
    sk.add_rectangle([-30.0, -20.0], [30.0, 20.0]);
    sk.add_circle([-15.0, 0.0], 4.0, false);
    sk.add_circle([15.0, 0.0], 4.0, false);
    let s = p.fresh_ids("Sketch");
    p.features.push(Feature { id: s.0.clone(), name: s.1, suppressed: false, kind: FeatureKind::Sketch(SketchFeature { plane: PlaneRef::Named(StdPlane::Top), sketch: sk }) });
    let e = p.fresh_ids("Extrude");
    p.features.push(Feature {
        id: e.0,
        name: e.1,
        suppressed: false,
        kind: FeatureKind::Extrude(Extrude { sketch: s.0, depth: 20.0.into(), end: EndCondition::Blind, reverse: false, op: Op::Boss }),
    });
    p
}

#[test]
fn three_view_sheet_and_exports() {
    let dir = std::env::temp_dir().join(format!("rustworks-drw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    rw_core::model::write_part(&dir.join("block.rwpart"), &block()).unwrap();
    let d = Drawing::new("BLOCK-001", "block.rwpart");
    let file = dir.join("block.rwdrw");
    write_drawing(&file, &d).unwrap();
    let back = read_drawing(&file).unwrap();
    assert_eq!(back, d);
    let sheet = build(&back, Some(&file)).unwrap();
    assert!(sheet.warnings.is_empty(), "{:?}", sheet.warnings);
    assert_eq!(sheet.views.len(), 4);
    // A3 fits this 60 mm part at 2:1
    assert_eq!(sheet.scale, 2.0, "scale {}", sheet.scale);
    let v = |k: ViewKind| sheet.views.iter().find(|v| v.kind == k).unwrap();
    let (front, top, right) = (v(ViewKind::Front), v(ViewKind::Top), v(ViewKind::Right));
    // sizes on paper = model size × scale
    let size = |v: &ViewInfo| [v.max[0] - v.min[0], v.max[1] - v.min[1]];
    assert!((size(front)[0] - 120.0).abs() < 1e-6 && (size(front)[1] - 40.0).abs() < 1e-6, "{:?}", size(front));
    assert!((size(top)[1] - 80.0).abs() < 1e-6, "{:?}", size(top));
    assert!((size(right)[0] - 80.0).abs() < 1e-6, "{:?}", size(right));
    // third angle: top above front, right to the right, aligned
    assert!(top.min[1] > front.max[1] && (top.min[0] - front.min[0]).abs() < 1e-6);
    assert!(right.min[0] > front.max[0] && (right.min[1] - front.min[1]).abs() < 1e-6);
    // the holes are hidden in the front view, circles (with a callout) from the top
    assert!(front.hidden_edges >= 4, "front hidden {}", front.hidden_edges);
    let texts: Vec<&str> = sheet.texts.iter().map(|t| t.text.as_str()).collect();
    assert!(texts.contains(&"2× Ø8"), "{texts:?}");
    assert!(texts.contains(&"60") && texts.contains(&"20") && texts.contains(&"40"), "{texts:?}");
    assert!(texts.contains(&"2:1"));
    // exports
    for ext in ["svg", "dxf", "pdf", "png"] {
        let p = dir.join(format!("block.{ext}"));
        export(&sheet, &p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.len() > 1000, "{ext} too small");
    }
    let svg = std::fs::read_to_string(dir.join("block.svg")).unwrap();
    assert!(svg.contains("stroke-dasharray") && svg.contains("2× Ø8"));
    let dxf = std::fs::read_to_string(dir.join("block.dxf")).unwrap();
    assert!(dxf.contains("HIDDEN") && dxf.contains("%%c8") && dxf.trim_end().ends_with("EOF"));
    let pdf = std::fs::read(dir.join("block.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
    let png = std::fs::read(dir.join("block.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // first-angle swaps sides
    let mut d1 = d.clone();
    d1.projection = Projection::FirstAngle;
    let s1 = build(&d1, Some(&file)).unwrap();
    let f1 = s1.views.iter().find(|v| v.kind == ViewKind::Front).unwrap();
    let t1 = s1.views.iter().find(|v| v.kind == ViewKind::Top).unwrap();
    assert!(t1.max[1] < f1.min[1], "first angle: top view below front");
    let _ = std::fs::remove_dir_all(&dir);
}
