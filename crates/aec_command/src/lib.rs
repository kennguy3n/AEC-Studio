//! `aec_command` — typed command engine, undo/redo journal, and command
//! structs for every user- and AI-initiated state mutation.
//!
//! Every state change in AEC Studio flows through this crate. Commands carry
//! a `scope`, an `actor`, a `tool` name, typed `arguments`, the resulting
//! `diff`, and an `audit` envelope with BLAKE3 hash chaining.
//!
//! AI tool-call outputs reuse the same command structs via
//! [`crate::engine::CommandEngine::propose`]: AI never bypasses the diff
//! engine or the audit log.

pub mod ai_apply;
pub mod audit;
pub mod commands;
pub mod engine;
pub mod error;
pub mod journal;
pub mod template_apply;

pub use ai_apply::{diff_to_commands, ApplyConversion, ApplyDefaults, SkippedOperation};
pub use template_apply::{
    template_to_commands, template_to_commands_as, CameraDefaults, InstantiationOutcome,
    RoomInstantiation, SkippedRoom,
};

pub use audit::{AuditEnvelope, AuditHashChain};
pub use commands::{Command, CommandKind, EntityDelta, ProjectGraph};
pub use engine::{CommandEngine, CommandResult as ExecutedCommand, ExecutionMode};
pub use error::{CommandError, CommandResult};
pub use journal::{JournalEntry, UndoRedoJournal};
