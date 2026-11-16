//! Drawing tools: 2D engineering drawings of the active part or assembly.

use crate::{abs, merge_patch, opt, req, Reply, Server};
use anyhow::{anyhow, bail, Context, Result};
use rw_drawing::{Drawing, ViewDef, ViewKind};
use serde_json::{json, Value as J};
use std::path::{Path, PathBuf};

impl Server {
    pub(crate) fn drawing_call(&mut self, name: &str, a: &J) -> Option<Result<Reply>> {
        match name {
            "create_drawing" | "open_drawing" | "edit_drawing" | "render_drawing" | "export_drawing" => Some(self.drawing_tool(name, a)),
            _ => None,
        }
    }

    fn drawing_tool(&mut self, name: &str, a: &J) -> Result<Reply> {
        match name {
            "create_drawing" => {
                let source: PathBuf = match opt::<PathBuf>(a, "source")? {
                    Some(p) => p,
                    None => self.active_source()?,
                };
                if !source.exists() {
                    bail!("source {} not found", source.display());
                }
                let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or("drawing").to_string();
                let path: PathBuf = opt(a, "path")?.unwrap_or_else(|| PathBuf::from(format!("{stem}.rwdrw")));
                let base = path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                std::fs::create_dir_all(&base).ok();
                let rel = rw_core::assembly::relative_part_path(&base, &source);
                let mut d = Drawing::new(&opt::<String>(a, "name")?.unwrap_or_else(|| stem.to_uppercase()), &rel);
                apply_options(&mut d, a)?;
                rw_drawing::write_drawing(&path, &d)?;
                self.drawing = Some(path.clone());
                self.render_current(a, json!({"created": abs(&path), "source": rel}))
            }
            "open_drawing" => {
                let path: PathBuf = req(a, "path")?;
                rw_drawing::read_drawing(&path)?;
                self.drawing = Some(path.clone());
                self.render_current(a, json!({"opened": abs(&path)}))
            }
            "edit_drawing" => {
                let path = self.drawing_path()?;
                let mut d = rw_drawing::read_drawing(&path)?;
                if let Some(ch) = a.get("changes") {
                    let mut v = serde_json::to_value(&d)?;
                    merge_patch(&mut v, ch);
                    // allow plain view names in patches
                    if let Some(vs) = v.get_mut("views").and_then(|x| x.as_array_mut()) {
                        for x in vs.iter_mut() {
                            if let Some(s) = x.as_str() {
                                *x = json!({"type": s});
                            }
                        }
                    }
                    d = serde_json::from_value(v).context("patched drawing is invalid")?;
                }
                apply_options(&mut d, a)?;
                rw_drawing::write_drawing(&path, &d)?;
                self.render_current(a, json!({"edited": abs(&path), "drawing": d}))
            }
            "render_drawing" => self.render_current(a, json!({})),
            "export_drawing" => {
                let fmt: String = req(a, "format")?;
                let out: PathBuf = match opt::<PathBuf>(a, "path")? {
                    Some(p) => p,
                    None => self.drawing_path()?.with_extension(&fmt),
                };
                let path = self.drawing_path()?;
                let d = rw_drawing::read_drawing(&path)?;
                let sheet = rw_drawing::build(&d, Some(&path))?;
                let out = if out.extension().is_none() { out.with_extension(&fmt) } else { out };
                if out.extension().and_then(|e| e.to_str()) != Some(fmt.as_str()) {
                    bail!("path extension does not match format `{fmt}`");
                }
                rw_drawing::export(&sheet, &out)?;
                Ok(Reply::Text(json!({"exported": abs(&out), "format": fmt, "warnings": sheet.warnings})))
            }
            _ => unreachable!(),
        }
    }

    fn drawing_path(&self) -> Result<PathBuf> {
        self.drawing.clone().ok_or_else(|| anyhow!("no drawing open: call create_drawing or open_drawing first"))
    }

    /// The file of the active part or assembly (saved first so the drawing sees the latest state).
    fn active_source(&mut self) -> Result<PathBuf> {
        if self.focus_asm {
            if let Some(m) = self.asm.as_mut() {
                m.save()?;
                return m.path.clone().ok_or_else(|| anyhow!("the assembly has no file"));
            }
        }
        let m = self.model.as_mut().ok_or_else(|| anyhow!("no part or assembly open; pass `source`"))?;
        m.save()?;
        m.path.clone().ok_or_else(|| anyhow!("the part has no file"))
    }

    fn render_current(&mut self, a: &J, mut info: J) -> Result<Reply> {
        let path = self.drawing_path()?;
        let d = rw_drawing::read_drawing(&path)?;
        let sheet = rw_drawing::build(&d, Some(&path))?;
        let width: u32 = opt(a, "width")?.unwrap_or(1600).clamp(400, 4000);
        let png = rw_drawing::to_png(&sheet, width);
        let o = info.as_object_mut().unwrap();
        o.insert("sheet".into(), json!(format!("{:?} {}x{} mm", d.sheet, sheet.width, sheet.height)));
        o.insert("scale".into(), json!(rw_drawing::scale_text(sheet.scale)));
        o.insert(
            "views".into(),
            crate::round_json(json!(sheet
                .views
                .iter()
                .map(|v| json!({"view": v.kind, "scale": rw_drawing::scale_text(v.scale), "min": v.min, "max": v.max, "visible_edges": v.visible_edges, "hidden_edges": v.hidden_edges}))
                .collect::<Vec<_>>())),
        );
        if !sheet.warnings.is_empty() {
            o.insert("warnings".into(), json!(sheet.warnings));
        }
        Ok(Reply::TextAndImage(info, png))
    }
}

/// Applies the convenience arguments shared by create_drawing / edit_drawing.
fn apply_options(d: &mut Drawing, a: &J) -> Result<()> {
    if let Some(v) = opt(a, "sheet")? {
        d.sheet = v;
    }
    if let Some(v) = a.get("scale") {
        d.scale = if v.is_null() || v.as_str() == Some("auto") { None } else { Some(parse_scale(v)?) };
    }
    if let Some(v) = opt(a, "projection")? {
        d.projection = v;
    }
    if let Some(v) = opt(a, "dimensions")? {
        d.dimensions = v;
    }
    if let Some(vs) = a.get("views").and_then(|x| x.as_array()) {
        let hidden: Option<bool> = opt(a, "hidden_lines")?;
        d.views = vs
            .iter()
            .map(|x| -> Result<ViewDef> {
                let mut vd = if let Some(s) = x.as_str() {
                    ViewDef::new(serde_json::from_value::<ViewKind>(json!(s)).with_context(|| format!("unknown view `{s}`"))?)
                } else {
                    serde_json::from_value(x.clone()).context("invalid view")?
                };
                if vd.hidden_lines.is_none() && vd.kind != ViewKind::Iso {
                    vd.hidden_lines = hidden;
                }
                Ok(vd)
            })
            .collect::<Result<_>>()?;
    } else if let Some(h) = opt::<bool>(a, "hidden_lines")? {
        for v in d.views.iter_mut().filter(|v| v.kind != ViewKind::Iso) {
            v.hidden_lines = Some(h);
        }
    }
    for (k, slot) in [("title", &mut d.title.title), ("drawn_by", &mut d.title.drawn_by), ("material", &mut d.title.material), ("notes", &mut d.title.notes)] {
        if let Some(v) = opt::<String>(a, k)? {
            *slot = Some(v);
        }
    }
    Ok(())
}

/// Scale as a number (paper/model) or "1:2" / "2:1".
fn parse_scale(v: &J) -> Result<f64> {
    if let Some(x) = v.as_f64() {
        return Ok(x);
    }
    let s = v.as_str().ok_or_else(|| anyhow!("scale must be a number or \"1:2\""))?;
    let (p, q) = s.split_once(':').ok_or_else(|| anyhow!("scale must look like 1:2"))?;
    let (p, q): (f64, f64) = (p.trim().parse()?, q.trim().parse()?);
    if p <= 0.0 || q <= 0.0 {
        bail!("scale must be positive");
    }
    Ok(p / q)
}
