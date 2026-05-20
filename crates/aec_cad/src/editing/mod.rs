//! Editing tools — move, copy, rotate, scale, mirror, offset, trim,
//! extend, fillet, chamfer, stretch.
//!
//! Every tool is a pure function on a [`Primitive`] (or pair of them)
//! that returns the modified primitive(s). The upstream command engine
//! wraps the result in a typed command and handles undo/redo.

pub mod chamfer_tool;
pub mod copy_tool;
pub mod extend_tool;
pub mod fillet_tool;
pub mod mirror_tool;
pub mod move_tool;
pub mod offset_tool;
pub mod rotate_tool;
pub mod scale_tool;
pub mod stretch_tool;
pub mod trim_tool;

pub use chamfer_tool::ChamferTool;
pub use copy_tool::CopyTool;
pub use extend_tool::ExtendTool;
pub use fillet_tool::FilletTool;
pub use mirror_tool::MirrorTool;
pub use move_tool::MoveTool;
pub use offset_tool::OffsetTool;
pub use rotate_tool::RotateTool;
pub use scale_tool::ScaleTool;
pub use stretch_tool::StretchTool;
pub use trim_tool::TrimTool;
