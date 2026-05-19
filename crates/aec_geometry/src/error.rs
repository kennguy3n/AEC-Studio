use thiserror::Error;

pub type GeometryResult<T> = std::result::Result<T, GeometryError>;

#[derive(Debug, Error)]
pub enum GeometryError {
    #[error("degenerate geometry: {0}")]
    Degenerate(String),

    #[error("opening `{opening_id}` does not fit in wall `{wall_id}` (wall length {wall_len_mm}mm, opening end {end_mm}mm)")]
    OpeningOutOfBounds {
        opening_id: String,
        wall_id: String,
        wall_len_mm: f64,
        end_mm: f64,
    },

    #[error("polygon is not closed or has fewer than 3 vertices")]
    InvalidPolygon,
}
