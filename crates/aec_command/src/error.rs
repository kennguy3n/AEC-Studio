//! Errors raised by the command engine.

use thiserror::Error;

pub type CommandResult<T> = std::result::Result<T, CommandError>;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("entity `{0}` not found")]
    EntityNotFound(String),

    #[error("entity `{0}` already exists")]
    EntityAlreadyExists(String),

    #[error(
        "scope mismatch: command in scope {expected:?} cannot run while engine is in {actual:?}"
    )]
    ScopeMismatch { expected: String, actual: String },

    #[error("nothing to undo")]
    NothingToUndo,

    #[error("nothing to redo")]
    NothingToRedo,

    #[error("invalid arguments for command `{tool}`: {reason}")]
    InvalidArguments { tool: String, reason: String },

    #[error("journal corrupt: {0}")]
    JournalCorrupt(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
