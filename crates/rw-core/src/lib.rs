//! RustWorks core: parametric part document, feature regeneration and the
//! shared `Model` used by the GUI and the MCP server.

pub mod assembly;
pub mod doc;
pub mod model;
pub mod regen;

pub use doc::*;
pub use assembly::{AsmModel, Assembly, Component, Mate, MateKind, MateRef};
pub use model::Model;
pub use regen::{faces_near, plane_from_face, resolve_plane, resolve_plane_in, FeatureStatus, RegenOutput, Regenerator, SolvedSketch};
pub use rw_kernel as kernel;
pub use rw_sketch as sketch;

#[cfg(test)]
mod tests;
