//! Common entity data header shared by every modern (R13+) DWG entity.
//!
//! Every entity in the OBJECTS section starts with a fixed header
//! (handles, type code, EED, common entity flags) before the per-type
//! payload kicks in. The header layout differs slightly per version:
//! R2007+ uses T strings while R13–R2004 uses TV, and R2010+ added a
//! few flag bits. We capture the version-agnostic shape here.

use crate::dwg::bits::reader::HandleRef;

#[derive(Debug, Clone, PartialEq)]
pub struct CommonEntityHeader {
    /// Handle of this entity (the handle the object map points to).
    pub handle: u64,
    /// Owner handle (parent dictionary, block-header, or model space).
    pub owner_handle: HandleRef,
    /// Reactor handles attached to this entity.
    pub reactors: Vec<HandleRef>,
    /// XDictionary handle (R2000+).
    pub x_dictionary: Option<HandleRef>,
    /// True if the entity is contained in `*Paper_Space`, false for
    /// `*Model_Space`. The model-space block-header is the default
    /// owner if neither flag is set.
    pub paper_space: bool,
    /// Layer handle reference.  Decoded layer-name lookup happens
    /// when the OBJECTS section finishes parsing.
    pub layer: HandleRef,
    /// Linetype handle (None means "BYLAYER").
    pub linetype: Option<HandleRef>,
    /// Color encoding (ACI or extended).
    pub color: i32,
    /// Linetype scale (CELTSCALE).
    pub linetype_scale: f64,
    /// True if the entity is invisible (bit 0 of the visibility flag).
    pub invisible: bool,
}

impl Default for CommonEntityHeader {
    fn default() -> Self {
        Self {
            handle: 0,
            owner_handle: HandleRef { code: 0, value: 0 },
            reactors: Vec::new(),
            x_dictionary: None,
            paper_space: false,
            layer: HandleRef { code: 0, value: 0 },
            linetype: None,
            color: 256, // BYLAYER
            linetype_scale: 1.0,
            invisible: false,
        }
    }
}
