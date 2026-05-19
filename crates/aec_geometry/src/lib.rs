//! Parametric geometry primitives and algorithms used by Design mode.
//!
//! Everything in this crate is **unit-aware** (`f64` millimeters internal)
//! and **serde-friendly** so the bridge can ship geometry across the N-API
//! boundary without an extra type-erased layer.

pub mod camera;
pub mod ceiling;
pub mod door;
pub mod error;
pub mod floor;
pub mod lighting;
pub mod mesh;
pub mod opening;
pub mod room;
pub mod snap;
pub mod spatial_index;
pub mod wall;
pub mod window;

pub use camera::{CameraMode, SavedCamera};
pub use ceiling::Ceiling;
pub use door::{Door, DoorKind};
pub use error::{GeometryError, GeometryResult};
pub use floor::Floor;
pub use lighting::{LightingPreset, LightingPresetId};
pub use mesh::{Mesh, MeshAttribute, Triangle};
pub use opening::Opening;
pub use room::Room;
pub use snap::{snap_to, SnapResult, SnapTarget};
pub use spatial_index::{Bvh, BvhAabb};
pub use wall::Wall;
pub use window::{Window, WindowKind};
