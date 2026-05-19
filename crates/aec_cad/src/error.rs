use thiserror::Error;

pub type CadResult<T> = std::result::Result<T, CadError>;

#[derive(Debug, Error)]
pub enum CadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected end of DXF input")]
    UnexpectedEof,
    #[error("DXF parse error at code/value `{code}`/`{value}`: {message}")]
    Parse {
        code: i32,
        value: String,
        message: String,
    },
    #[error("invalid DXF group code `{0}`")]
    InvalidGroupCode(i32),
    #[error("invalid layer name `{0}`")]
    InvalidLayerName(String),
}
