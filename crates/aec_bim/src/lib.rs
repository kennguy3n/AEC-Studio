//! BIM core: spatial hierarchy, IFC classification, property sets, cache,
//! schedules, validation, diff, drawing generation.
//!
//! The Rust side owns the *graph* (project → site → building → level → space
//! → elements). The IFC byte-level read/write happens out-of-process in the
//! IfcOpenShell worker (see `workers/ifc/`).

pub mod boq;
pub mod cache;
pub mod classification;
pub mod diff;
pub mod drawing_gen;
pub mod properties;
pub mod relations;
pub mod schedules;
pub mod spatial;
pub mod validation;

pub use boq::{boq_for_project, BoqLine, BoqRegion, BoqReport};
pub use cache::{BimCache, CachedElement};
pub use classification::{
    ClassificationAssignment, ClassificationSource, ClassificationStore, IfcClass,
};
pub use diff::{diff_projects, ElementDelta, ProjectDiff, PropertyDelta};
pub use drawing_gen::{
    generate_elevation, generate_plan, generate_section, DrawingResult, ElementGeometry,
    SectionPlane,
};
pub use properties::{
    standard_pset_keys, standard_pset_name_for_class, ElementProperties, PropertySet,
    PropertyStore, PropertyValue, QuantitySet,
};
pub use relations::{AggregateRelation, ContainmentRelation, RelationStore};
pub use schedules::{
    DoorScheduleEntry, MaterialScheduleEntry, RoomScheduleEntry, ScheduleColumn, ScheduleRow,
    ScheduleSheet, WindowScheduleEntry,
};
pub use spatial::{Building, Level, Project, RemovedSubtree, Site, Space, SpatialNode};
pub use validation::{validate_project, ValidationFinding, ValidationReport, ValidationSeverity};
