//! Hardware profiler + resource governor.

pub mod policy;
pub mod profiler;
pub mod report;
pub mod scheduler;
pub mod tier;
pub mod ui_report;

pub use policy::{AiModelTier, AiPolicy, GovernorPolicy, RenderPolicy};
pub use profiler::{CpuProfile, GpuProfile, HardwareProfile, HardwareProfiler};
pub use report::ProfileReport;
pub use scheduler::{BackoffReason, GovernorScheduler, ScheduleVerdict, SchedulerError};
pub use tier::HardwareTier;
pub use ui_report::{GovernorState, ThermalState};
