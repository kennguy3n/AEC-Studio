//! Door builder. Re-exports the `OpeningKind::Door` constructor with the
//! correct `sub_kind` tag so callers don't fat-finger the string.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

use crate::opening::{Opening, OpeningKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoorKind {
    SingleSwing,
    DoubleSwing,
    Sliding,
    Pocket,
}

impl DoorKind {
    pub fn as_sub_kind(self) -> &'static str {
        match self {
            Self::SingleSwing => "single_swing",
            Self::DoubleSwing => "double_swing",
            Self::Sliding => "sliding",
            Self::Pocket => "pocket",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Door {
    pub id: EntityId,
    pub host_wall_id: EntityId,
    pub position_along_wall_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    pub kind: DoorKind,
}

impl Door {
    pub fn into_opening(self) -> Opening {
        Opening {
            id: self.id,
            position_along_wall_mm: self.position_along_wall_mm,
            width_mm: self.width_mm,
            height_mm: self.height_mm,
            sill_height_mm: 0.0,
            kind: OpeningKind::Door,
            sub_kind: self.kind.as_sub_kind().into(),
        }
    }
}
