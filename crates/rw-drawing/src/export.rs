//! Vector exporters: SVG, DXF (R12) and PDF. Coordinates are millimetres
//! with the origin at the sheet's bottom-left.

use crate::{Anchor, Pen, Sheet};
use std::fmt::Write;

/// Line width (mm) and dash pattern (mm) per pen.
pub(crate) fn pen_style(p: Pen) -> (f64, Option<[f64; 2]>) {
    match p {
        Pen::Visible => (0.5, None),
        Pen::Hidden => (0.3, Some([3.0, 1.5])),
        Pen::Thin => (0.18, None),
        Pen::Border => (0.7, None),
    }
}

fn esc_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn to_svg(s: &Sheet) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}mm" height="{h}mm" viewBox="0 0 {w} {h}">"#,
        w = s.width,
        h = s.height
    );
    let _ = writeln!(o, r##"<rect x="0" y="0" width="{}" height="{}" fill="#ffffff"/>"##, s.width, s.height);
    let _ = writeln!(o, r#"<g fill="none" stroke="black" stroke-linecap="round" stroke-linejoin="round">"#);
    for l in &s.lines {
        if l.pts.len() < 2 {
            continue;
        }
        let (w, dash) = pen_style(l.pen);
        let pts: Vec<String> = l.pts.iter().map(|p| format!("{:.3},{:.3}", p[0], s.height - p[1])).collect();
        let dash = dash.map(|d| format!(r#" stroke-dasharray="{} {}""#, d[0], d[1])).unwrap_or_default();
        let _ = writeln!(o, r#"<polyline points="{}" stroke-width="{w}"{dash}/>"#, pts.join(" "));
    }
    let _ = writeln!(o, "</g>");
    let _ = writeln!(o, r#"<g font-family="Helvetica, Arial, sans-serif" fill="black">"#);
    for t in &s.texts {
        let (x, y) = (t.at[0], s.height - t.at[1]);
        let anchor = match t.anchor {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        };
        let rot = if t.rotate.abs() > 1e-9 { format!(r#" transform="rotate({:.3} {x:.3} {y:.3})""#, -t.rotate) } else { String::new() };
        let _ = writeln!(o, r#"<text x="{x:.3}" y="{y:.3}" font-size="{:.3}" text-anchor="{anchor}"{rot}>{}</text>"#, t.size / 0.72, esc_xml(&t.text));
    }
    let _ = writeln!(o, "</g>\n</svg>");
    o
}

/// DXF text: degree/diameter use AutoCAD control codes.
fn dxf_text(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'Ø' => "%%c".to_string(),
            '°' => "%%d".to_string(),
            '×' => "x".to_string(),
            '…' => "...".to_string(),
            c if c.is_ascii() => c.to_string(),
            _ => "?".to_string(),
        })
        .collect()
}

pub fn to_dxf(s: &Sheet) -> String {
    let mut o = String::new();
    let mut g = |code: i32, v: &str| {
        let _ = writeln!(o, "{code}\n{v}");
    };
    g(0, "SECTION");
    g(2, "HEADER");
    g(9, "$ACADVER");
    g(1, "AC1009");
    g(9, "$MEASUREMENT");
    g(70, "1");
    g(9, "$EXTMIN");
    g(10, "0.0");
    g(20, "0.0");
    g(9, "$EXTMAX");
    g(10, &format!("{:.3}", s.width));
    g(20, &format!("{:.3}", s.height));
    g(0, "ENDSEC");
    g(0, "SECTION");
    g(2, "TABLES");
    g(0, "TABLE");
    g(2, "LTYPE");
    g(70, "2");
    for (name, desc, pattern) in [("CONTINUOUS", "Solid line", vec![]), ("DASHED", "__ __ __", vec![3.0, -1.5])] {
        g(0, "LTYPE");
        g(2, name);
        g(70, "0");
        g(3, desc);
        g(72, "65");
        g(73, &pattern.len().to_string());
        g(40, &format!("{:.3}", pattern.iter().map(|x: &f64| x.abs()).sum::<f64>()));
        for p in pattern {
            g(49, &format!("{p:.3}"));
        }
    }
    g(0, "ENDTAB");
    g(0, "TABLE");
    g(2, "LAYER");
    g(70, "5");
    for (name, color, lt) in [("VISIBLE", "7", "CONTINUOUS"), ("HIDDEN", "8", "DASHED"), ("DIM", "3", "CONTINUOUS"), ("BORDER", "7", "CONTINUOUS"), ("TEXT", "7", "CONTINUOUS")] {
        g(0, "LAYER");
        g(2, name);
        g(70, "0");
        g(62, color);
        g(6, lt);
    }
    g(0, "ENDTAB");
    g(0, "ENDSEC");
    g(0, "SECTION");
    g(2, "ENTITIES");
    for l in &s.lines {
        let layer = match l.pen {
            Pen::Visible => "VISIBLE",
            Pen::Hidden => "HIDDEN",
            Pen::Thin => "DIM",
            Pen::Border => "BORDER",
        };
        for w in l.pts.windows(2) {
            g(0, "LINE");
            g(8, layer);
            g(10, &format!("{:.4}", w[0][0]));
            g(20, &format!("{:.4}", w[0][1]));
            g(30, "0.0");
            g(11, &format!("{:.4}", w[1][0]));
            g(21, &format!("{:.4}", w[1][1]));
            g(31, "0.0");
        }
    }
    for t in &s.texts {
        g(0, "TEXT");
        g(8, "TEXT");
        g(10, &format!("{:.4}", t.at[0]));
        g(20, &format!("{:.4}", t.at[1]));
        g(30, "0.0");
        g(40, &format!("{:.3}", t.size));
        g(1, &dxf_text(&t.text));
        if t.rotate.abs() > 1e-9 {
            g(50, &format!("{:.3}", t.rotate));
        }
        let h = match t.anchor {
            Anchor::Start => 0,
            Anchor::Middle => 1,
            Anchor::End => 2,
        };
        if h != 0 {
            g(72, &h.to_string());
            g(11, &format!("{:.4}", t.at[0]));
            g(21, &format!("{:.4}", t.at[1]));
            g(31, "0.0");
        }
    }
    g(0, "ENDSEC");
    g(0, "EOF");
    o
}

/// Latin-1 / WinAnsi bytes for a PDF string literal.
fn pdf_text(s: &str) -> Vec<u8> {
    let mut v = vec![];
    for c in s.chars() {
        let b: u8 = match c {
            '(' | ')' | '\\' => {
                v.push(b'\\');
                c as u8
            }
            'Ø' => 0xD8,
            '×' => 0xD7,
            '°' => 0xB0,
            '…' => 0x85,
            c if (c as u32) < 0x80 => c as u8,
            _ => b'?',
        };
        v.push(b);
    }
    v
}

/// Approximate Helvetica advance width (em units) for anchoring.
fn helvetica_width(s: &str) -> f64 {
    s.chars()
        .map(|c| match c {
            'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' => 0.25,
            'f' | 't' | 'r' | 'I' | ' ' | '(' | ')' | '-' => 0.32,
            'm' | 'M' | 'W' | 'w' => 0.85,
            'A'..='Z' | 'Ø' => 0.68,
            '0'..='9' => 0.556,
            _ => 0.52,
        })
        .sum()
}

pub fn to_pdf(s: &Sheet) -> Vec<u8> {
    let k = 72.0 / 25.4;
    let mut c: Vec<u8> = vec![];
    let _ = write!(Wr(&mut c), "{k:.6} 0 0 {k:.6} 0 0 cm 1 J 1 j 0 G\n");
    for l in &s.lines {
        if l.pts.len() < 2 {
            continue;
        }
        let (w, dash) = pen_style(l.pen);
        let _ = write!(Wr(&mut c), "{w} w ");
        match dash {
            Some(d) => {
                let _ = write!(Wr(&mut c), "[{} {}] 0 d ", d[0], d[1]);
            }
            None => {
                let _ = write!(Wr(&mut c), "[] 0 d ");
            }
        }
        let _ = write!(Wr(&mut c), "{:.3} {:.3} m ", l.pts[0][0], l.pts[0][1]);
        for p in &l.pts[1..] {
            let _ = write!(Wr(&mut c), "{:.3} {:.3} l ", p[0], p[1]);
        }
        c.extend_from_slice(b"S\n");
    }
    for t in &s.texts {
        let em = t.size / 0.72;
        let wdt = helvetica_width(&t.text) * em;
        let shift = match t.anchor {
            Anchor::Start => 0.0,
            Anchor::Middle => -wdt / 2.0,
            Anchor::End => -wdt,
        };
        let (sn, cs) = t.rotate.to_radians().sin_cos();
        let x = t.at[0] + shift * cs;
        let y = t.at[1] + shift * sn;
        let _ = write!(Wr(&mut c), "BT /F1 {em:.3} Tf {cs:.6} {sn:.6} {:.6} {cs:.6} {x:.3} {y:.3} Tm (", -sn);
        c.extend(pdf_text(&t.text));
        c.extend_from_slice(b") Tj ET\n");
    }
    let (pw, ph) = (s.width * k, s.height * k);
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs = vec![];
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>").into_bytes(),
        {
            let mut v = format!("<< /Length {} >>\nstream\n", c.len()).into_bytes();
            v.extend_from_slice(&c);
            v.extend_from_slice(b"\nendstream");
            v
        },
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).into_bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).into_bytes());
    out
}

/// `fmt::Write` adapter for a byte buffer.
struct Wr<'a>(&'a mut Vec<u8>);
impl std::fmt::Write for Wr<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}
