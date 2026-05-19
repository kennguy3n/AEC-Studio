//! Window builder.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::opening::{Opening, OpeningKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Fixed,
    Casement,
    Sliding,
    Awning,
}

impl WindowKind {
    pub fn as_sub_kind(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Casement => "casement",
            Self::Sliding => "sliding",
            Self::Awning => "awning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub id: EntityId,
    pub host_wall_id: EntityId,
    pub position_along_wall_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    pub sill_height_mm: f64,
    pub kind: WindowKind,
}

impl Window {
    pub fn into_opening(self) -> Opening {
        Opening {
            id: self.id,
            position_along_wall_mm: self.position_along_wall_mm,
            width_mm: self.width_mm,
            height_mm: self.height_mm,
            sill_height_mm: self.sill_height_mm,
            kind: OpeningKind::Window,
            sub_kind: self.kind.as_sub_kind().into(),
        }
    }
}
