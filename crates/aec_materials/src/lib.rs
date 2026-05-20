//! PBR material model + library + finish-swap engine.

pub mod library;
pub mod material;
pub mod mood_board;
pub mod swap;

pub use library::{MaterialLibrary, MaterialLibraryError, MaterialQuery};
pub use material::{PbrMaterial, TextureRef};
pub use mood_board::{
    generate_mood_board, MoodBoard, MoodBoardPage, MoodSwatch, PaletteEntry, MAX_SWATCHES_PER_PAGE,
};
pub use swap::{FinishSwap, SwapPreview};
