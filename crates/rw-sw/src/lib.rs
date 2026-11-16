//! SOLIDWORKS interoperability.
//!
//! Native SOLIDWORKS files are closed, so RustWorks reads them through
//! SOLIDWORKS itself: `tools/solidworks/rw_sw_bridge.py` runs on a Windows PC
//! with SOLIDWORKS, uses the official API to dump each document's feature
//! tree, sketches, dimensions, placements and mates (plus a STEP copy), and
//! this crate translates the dump into RustWorks documents:
//!
//! * parts are rebuilt as parametric feature trees when every feature maps,
//!   verified against SOLIDWORKS' mass properties; otherwise they import as
//!   exact STEP geometry, with a report of what could not be carried over;
//! * assemblies become `.rwasm` files with each part, its placement and the
//!   mates RustWorks supports.
//!
//! Export writes STEP and asks SOLIDWORKS (through the bridge) to save it as
//! a native `.sldprt` / `.sldasm` (geometry, without RustWorks' history).

pub mod dump;
mod translate;

pub use translate::{read_dump, translate_assembly, translate_part, AssemblyReport, Convention, PartReport};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ImportReport {
    Part(PartReport),
    Assembly(AssemblyReport),
}

impl ImportReport {
    /// The RustWorks document that was created (.rwpart or .rwasm).
    pub fn file(&self) -> &Path {
        match self {
            ImportReport::Part(p) => &p.file,
            ImportReport::Assembly(a) => &a.file,
        }
    }
}

/// Locates the bridge script and a Python interpreter.
///
/// Script: `RUSTWORKS_SW_BRIDGE`, else `solidworks/rw_sw_bridge.py` next to the
/// executable (release archives), else `tools/solidworks/` in a source checkout.
/// Python: `RUSTWORKS_PYTHON`, else `python` (Windows) / `python3`.
pub fn bridge() -> Result<(PathBuf, PathBuf)> {
    let python = std::env::var_os("RUSTWORKS_PYTHON").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
    if let Some(p) = std::env::var_os("RUSTWORKS_SW_BRIDGE") {
        return Ok((python, PathBuf::from(p)));
    }
    let exe = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
    for cand in [exe.join("solidworks/rw_sw_bridge.py"), exe.join("../../tools/solidworks/rw_sw_bridge.py"), PathBuf::from("tools/solidworks/rw_sw_bridge.py")] {
        if cand.exists() {
            return Ok((python, cand));
        }
    }
    bail!("SOLIDWORKS bridge script not found; set RUSTWORKS_SW_BRIDGE to the path of rw_sw_bridge.py")
}

fn need_solidworks() -> Result<()> {
    if !cfg!(windows) {
        bail!(
            "opening native SOLIDWORKS files needs SOLIDWORKS, which runs on Windows. On the SOLIDWORKS PC run \
             `python rw_sw_bridge.py dump <file> <folder>` (or RustWorks there), then import the resulting .swdump.json here"
        );
    }
    Ok(())
}

fn run_bridge(args: &[&Path], cmd: &str) -> Result<serde_json::Value> {
    need_solidworks()?;
    let (python, script) = bridge()?;
    let out = Command::new(&python)
        .arg(&script)
        .arg(cmd)
        .args(args)
        .output()
        .with_context(|| format!("running {} {}", python.display(), script.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or("");
    let v: serde_json::Value = serde_json::from_str(last).map_err(|_| {
        anyhow!("SOLIDWORKS bridge failed: {}{}", String::from_utf8_lossy(&out.stderr).trim(), stdout.trim())
    })?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        bail!("SOLIDWORKS bridge: {}", v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error"));
    }
    Ok(v)
}

/// Imports a `.sldprt` / `.sldasm` (via SOLIDWORKS on this machine) or a
/// `.swdump.json` produced by the bridge elsewhere. Files go to `out_dir`.
pub fn import(path: &Path, out_dir: &Path) -> Result<ImportReport> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let dump_path = if name.ends_with(".swdump.json") {
        path.to_path_buf()
    } else if name.ends_with(".sldprt") || name.ends_with(".sldasm") {
        let raw = out_dir.join("solidworks-dump");
        std::fs::create_dir_all(&raw)?;
        let v = run_bridge(&[path, &raw], "dump")?;
        PathBuf::from(v["dump"].as_str().ok_or_else(|| anyhow!("bridge returned no dump"))?)
    } else if name.ends_with(".slddrw") {
        bail!("SOLIDWORKS drawings (.slddrw) cannot be imported; import the part or assembly and create a RustWorks drawing from it")
    } else {
        bail!("not a SOLIDWORKS file: {}", path.display());
    };
    let dump = read_dump(&dump_path)?;
    let dir = dump_path.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(match dump.kind.as_str() {
        "part" => ImportReport::Part(translate_part(&dump, &dir, out_dir)?.0),
        "assembly" => ImportReport::Assembly(translate_assembly(&dump, &dir, out_dir)?),
        other => bail!("unknown dump kind `{other}`"),
    })
}

/// Saves a STEP file as a native SOLIDWORKS document (via SOLIDWORKS on this machine).
pub fn export_step(step: &Path, target: &Path) -> Result<PathBuf> {
    let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext != "sldprt" && ext != "sldasm" {
        bail!("SOLIDWORKS target must be .sldprt or .sldasm");
    }
    run_bridge(&[step, target], "export")?;
    Ok(target.to_path_buf())
}

#[cfg(test)]
mod tests;
