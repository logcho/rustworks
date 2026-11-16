//! SOLIDWORKS import/export tools (see crates/rw-sw).

use crate::{abs, opt, req, Reply, Server};
use anyhow::{anyhow, bail, Result};
use rw_core::{AsmModel, Model};
use serde_json::{json, Value as J};
use std::path::{Path, PathBuf};

impl Server {
    pub(crate) fn solidworks_call(&mut self, name: &str, a: &J) -> Option<Result<Reply>> {
        match name {
            "import_solidworks" => Some(self.import_solidworks(a)),
            "export_solidworks" => Some(self.export_solidworks(a)),
            _ => None,
        }
    }

    fn import_solidworks(&mut self, a: &J) -> Result<Reply> {
        let path: PathBuf = req(a, "path")?;
        let out: PathBuf = opt(a, "out_dir")?.unwrap_or_else(|| default_out_dir(&path));
        let rep = rw_sw::import(&path, &out)?;
        let file = rep.file().to_path_buf();
        if file.extension().is_some_and(|e| e == "rwasm") {
            self.asm = Some(AsmModel::open(&file)?);
            self.focus_asm = true;
        } else {
            self.model = Some(Model::open(&file)?);
            self.focus_asm = false;
        }
        Ok(Reply::Text(crate::round_json(json!({"imported": abs(&file), "report": rep}))))
    }

    fn export_solidworks(&mut self, a: &J) -> Result<Reply> {
        let target: PathBuf = req(a, "path")?;
        let tmp = std::env::temp_dir().join(format!("rustworks-sw-export-{}.step", std::process::id()));
        let to_asm = target.extension().is_some_and(|e| e.eq_ignore_ascii_case("sldasm"));
        if to_asm {
            let m = self.asm.as_mut().ok_or_else(|| anyhow!("no assembly open to export as .sldasm"))?;
            m.export_step(&tmp)?;
        } else {
            let m = self.model.as_mut().ok_or_else(|| anyhow!("no part open to export as .sldprt"))?;
            let body = m.out.body.as_ref().ok_or_else(|| anyhow!("the part has no body"))?;
            rw_core::kernel::write_step(body, &tmp)?;
        }
        let r = rw_sw::export_step(&tmp, &target);
        let _ = std::fs::remove_file(&tmp);
        let saved = r?;
        Ok(Reply::Text(json!({"saved": abs(&saved), "note": "saved as SOLIDWORKS geometry (an imported body); RustWorks' feature history is not transferred"})))
    }
}

/// `<folder of the source>/<name>-rustworks/`.
pub fn default_out_dir(src: &Path) -> PathBuf {
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("solidworks");
    let stem = name.split('.').next().unwrap_or("solidworks");
    src.parent().unwrap_or(Path::new(".")).join(format!("{stem}-rustworks"))
}

/// Command line: `rustworks-mcp sw-import <file> [out_dir]` / `sw-export <model> <target>`.
pub fn cli(args: &[String]) -> Result<bool> {
    match args.first().map(String::as_str) {
        Some("sw-import") => {
            let src = PathBuf::from(args.get(1).ok_or_else(|| anyhow!("usage: rustworks-mcp sw-import <file.sldprt|.sldasm|.swdump.json> [out_dir]"))?);
            let out = args.get(2).map(PathBuf::from).unwrap_or_else(|| default_out_dir(&src));
            let rep = rw_sw::import(&src, &out)?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            Ok(true)
        }
        Some("sw-export") => {
            let (src, target) = match (args.get(1), args.get(2)) {
                (Some(a), Some(b)) => (PathBuf::from(a), PathBuf::from(b)),
                _ => bail!("usage: rustworks-mcp sw-export <model.rwpart|.rwasm> <target.sldprt|.sldasm>"),
            };
            let tmp = std::env::temp_dir().join(format!("rustworks-sw-export-{}.step", std::process::id()));
            if src.extension().is_some_and(|e| e == "rwasm") {
                AsmModel::open(&src)?.export_step(&tmp)?;
            } else {
                let m = Model::open(&src)?;
                rw_core::kernel::write_step(m.out.body.as_ref().ok_or_else(|| anyhow!("part has no body"))?, &tmp)?;
            }
            let r = rw_sw::export_step(&tmp, &target);
            let _ = std::fs::remove_file(&tmp);
            println!("saved {}", r?.display());
            Ok(true)
        }
        _ => Ok(false),
    }
}
