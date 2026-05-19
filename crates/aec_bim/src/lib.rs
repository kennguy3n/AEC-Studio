//! BIM core: spatial hierarchy, IFC classification, property sets, cache.
//!
//! The Rust side owns the *graph* (project → site → building → level → space
//! → elements). The IFC byte-level read/write happens out-of-process in the
//! IfcOpenShell worker (see `workers/ifc/`).

pub mod cache;
pub mod classification;
pub mod properties;
pub mod spatial;

pub use cache::{BimCache, CachedElement};
pub use classification::IfcClass;
pub use properties::{PropertySet, PropertyValue, QuantitySet};
pub use spatial::{Building, Level, Project, Site, Space, SpatialNode};
