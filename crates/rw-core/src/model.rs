//! An open part: document + regeneration cache + undo/redo + file sync.
//! Shared by the desktop app and the MCP server. Both sides save to the same
//! `.rwpart` file and reload when it changes on disk, which gives live
//! two-way sync between an AI agent and the GUI.

use crate::doc::Part;
use crate::regen::{RegenOutput, Regenerator};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct Model {
    pub part: Part,
    pub path: Option<PathBuf>,
    pub out: RegenOutput,
    regen: Regenerator,
    undo: Vec<Part>,
    redo: Vec<Part>,
    disk_stamp: Option<SystemTime>,
}

impl Model {
    pub fn new(part: Part) -> Model {
        let mut regen = Regenerator::new();
        let out = regen.regenerate(&part);
        let part = out.solved.clone();
        Model { part, path: None, out, regen, undo: vec![], redo: vec![], disk_stamp: None }
    }

    pub fn open(path: &Path) -> Result<Model> {
        let part = read_part(path)?;
        let mut m = Model::new(part);
        m.path = Some(path.to_path_buf());
        m.disk_stamp = stamp(path);
        Ok(m)
    }

    pub fn rebuild(&mut self) {
        self.out = self.regen.regenerate(&self.part);
        self.part = self.out.solved.clone();
    }

    /// Applies an edit transactionally: on error the part is restored. On
    /// success the change is pushed to the undo stack, rebuilt and autosaved.
    pub fn edit<T>(&mut self, f: impl FnOnce(&mut Part) -> Result<T>) -> Result<T> {
        let before = self.part.clone();
        match f(&mut self.part) {
            Ok(v) => {
                if self.part != before {
                    self.undo.push(before);
                    if self.undo.len() > 200 {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                    self.rebuild();
                    self.autosave()?;
                }
                Ok(v)
            }
            Err(e) => {
                self.part = before;
                Err(e)
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        if let Some(p) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.part, p));
            self.rebuild();
            let _ = self.autosave();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(p) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.part, p));
            self.rebuild();
            let _ = self.autosave();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Replace the whole document (e.g. "new part"), keeping undo history.
    pub fn replace(&mut self, part: Part) {
        self.undo.push(std::mem::replace(&mut self.part, part));
        self.redo.clear();
        self.regen.clear();
        self.rebuild();
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.path = Some(path.to_path_buf());
        self.save()
    }

    pub fn save(&mut self) -> Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        write_part(&path, &self.part)?;
        self.disk_stamp = stamp(&path);
        Ok(())
    }

    fn autosave(&mut self) -> Result<()> {
        if self.path.is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// If the file changed on disk (another process edited it), reload it.
    /// Returns true when a reload happened.
    pub fn sync_from_disk(&mut self) -> Result<bool> {
        let Some(path) = self.path.clone() else { return Ok(false) };
        let st = stamp(&path);
        if st.is_none() || st == self.disk_stamp {
            return Ok(false);
        }
        let part = read_part(&path)?;
        self.disk_stamp = st;
        if part != self.part {
            self.undo.push(std::mem::replace(&mut self.part, part));
            self.redo.clear();
            self.rebuild();
            return Ok(true);
        }
        Ok(false)
    }
}

fn stamp(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn read_part(path: &Path) -> Result<Part> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
}

/// Atomic write (temp file + rename) so readers never see a partial file.
pub fn write_part(path: &Path, part: &Part) -> Result<()> {
    let json = serde_json::to_string_pretty(part)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("rwpart.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
