//! Blocks — reusable named groups of CAD primitives.

pub mod block;
pub mod block_attribute;
pub mod block_instance;
pub mod block_library;
pub mod dynamic_block;

pub use block::Block;
pub use block_attribute::{AttributeKind, BlockAttribute};
pub use block_instance::BlockInstance;
pub use block_library::{BlockLibrary, BlockScope};
pub use dynamic_block::{DynamicBlockSpec, DynamicParameter};
