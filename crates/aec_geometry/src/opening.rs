//! Opening (door or window) hosted on a wall.

use serde::{Deserialize, Serialize};

use aec_core::types::EntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpeningKind {
    Door,
    Window,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    pub id: EntityId,
    pub position_along_wall_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    /// Distance from the floor to the bottom of the opening (0 for doors).
    pub sill_height_mm: f64,
    pub kind: OpeningKind,
    /// "single_swing" | "double_swing" | "sliding" | "pocket" | "fixed" | "casement" | "awning"
    pub sub_kind: String,
}

impl Opening {
    pub fn end_along_wall_mm(&self) -> f64 {
        self.position_along_wall_mm + self.width_mm
    }
}
