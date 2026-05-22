//! LTYPE (linetype) table record codec.
//!
//! Each linetype describes a pattern of dashes, spaces and optional
//! embedded text/shapes.  The existing [`crate::layers::Linetype`]
//! captures the dash-pattern subset; we round-trip that and preserve
//! everything else opaquely.

use crate::layers::{Linetype, LinetypeElement};

#[derive(Debug, Clone, PartialEq)]
pub struct LinetypeRecord {
    pub name: String,
    pub description: String,
    pub pattern_length: f64,
    pub elements: Vec<LinetypeElement>,
}

impl LinetypeRecord {
    pub fn into_linetype(self) -> Linetype {
        Linetype {
            name: self.name,
            description: self.description,
            elements: self.elements,
        }
    }

    pub fn from_linetype(l: &Linetype) -> Self {
        Self {
            name: l.name.clone(),
            description: l.description.clone(),
            pattern_length: l.pattern_length(),
            elements: l.elements.clone(),
        }
    }
}
