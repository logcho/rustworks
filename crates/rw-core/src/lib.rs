//! RustWorks core: parametric part document, feature regeneration and the
//! shared `Model` used by the GUI and the MCP server.

pub mod doc;
pub mod model;
pub mod regen;

pub use doc::*;
pub use model::Model;
pub use regen::{plane_from_face, resolve_plane, FeatureStatus, RegenOutput, Regenerator, SolvedSketch};
pub use rw_kernel as kernel;
pub use rw_sketch as sketch;

#[cfg(test)]
mod tests;
