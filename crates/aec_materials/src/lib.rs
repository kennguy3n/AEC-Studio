//! PBR material model + library + finish-swap engine.

pub mod library;
pub mod material;
pub mod swap;

pub use library::{MaterialLibrary, MaterialLibraryError, MaterialQuery};
pub use material::{PbrMaterial, TextureRef};
pub use swap::{FinishSwap, SwapPreview};
