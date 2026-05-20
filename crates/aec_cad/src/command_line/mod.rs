//! Command-line parser + state machine for keyboard-driven CAD input.

pub mod coordinate_parser;
pub mod input_state;
pub mod parser;
pub mod prompt;

pub use coordinate_parser::{parse_coordinate, resolve, CoordinateError, CoordinateInput};
pub use input_state::{AdvanceResult, CommandSession, SessionError, SessionInput};
pub use parser::{CommandKind, CommandParser, ParsedCommand};
pub use prompt::{build_prompt, step_prompt};
