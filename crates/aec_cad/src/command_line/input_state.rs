//! State machine for the active multi-step command.
//!
//! The CAD command line is a small state machine: a [`CommandSession`]
//! holds the in-flight command kind, the step counter, and the
//! accumulated inputs (points, distances, selected entity IDs).

use serde::{Deserialize, Serialize};

use crate::command_line::coordinate_parser::{parse_coordinate, resolve, CoordinateError};
use crate::command_line::parser::CommandKind;
use crate::command_line::prompt::step_prompt;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionInput {
    Point([f64; 2]),
    Number(f64),
    Selection(Vec<u64>),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandSession {
    pub kind: CommandKind,
    pub step: usize,
    pub inputs: Vec<SessionInput>,
    pub current_pen: [f64; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub enum AdvanceResult {
    /// The session needs more input. The string is the next prompt.
    NeedMore(String),
    /// The session is complete; the inputs are now final.
    Complete,
    /// The user cancelled.
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("coordinate parse error: {0}")]
    Coordinate(#[from] CoordinateError),
    #[error("expected a number, got `{0}`")]
    BadNumber(String),
}

impl CommandSession {
    pub fn start(kind: CommandKind) -> Self {
        Self {
            kind,
            step: 0,
            inputs: Vec::new(),
            current_pen: [0.0, 0.0],
        }
    }

    pub fn prompt(&self) -> Option<String> {
        step_prompt(self.kind, self.step)
    }

    /// Feed a single user input string. The string is interpreted based on
    /// the current command + step (point vs distance vs etc).
    pub fn advance(&mut self, input: &str) -> Result<AdvanceResult, SessionError> {
        let trimmed = input.trim();
        if trimmed.eq_ignore_ascii_case("cancel")
            || trimmed.eq_ignore_ascii_case("escape")
            || trimmed == "\x1b"
        {
            return Ok(AdvanceResult::Cancelled);
        }
        if trimmed.is_empty() {
            // Empty input is "accept default / finish" for variadic commands.
            return Ok(self.maybe_complete());
        }
        match (self.kind, self.step) {
            (CommandKind::Line | CommandKind::Polyline, _) => {
                if trimmed.eq_ignore_ascii_case("close") || trimmed.eq_ignore_ascii_case("c") {
                    self.inputs.push(SessionInput::Text("close".into()));
                    return Ok(AdvanceResult::Complete);
                }
                if trimmed.eq_ignore_ascii_case("undo") || trimmed.eq_ignore_ascii_case("u") {
                    self.inputs.pop();
                    if self.step > 0 {
                        self.step -= 1;
                    }
                    return Ok(AdvanceResult::NeedMore(self.prompt().unwrap_or_default()));
                }
                let coord = parse_coordinate(trimmed)?;
                let point = resolve(&coord, self.current_pen);
                self.current_pen = point;
                self.inputs.push(SessionInput::Point(point));
                self.step += 1;
                if self.is_done() {
                    Ok(AdvanceResult::Complete)
                } else {
                    Ok(AdvanceResult::NeedMore(self.prompt().unwrap_or_default()))
                }
            }
            (CommandKind::Circle, 0) => {
                let coord = parse_coordinate(trimmed)?;
                let p = resolve(&coord, self.current_pen);
                self.current_pen = p;
                self.inputs.push(SessionInput::Point(p));
                self.step += 1;
                Ok(AdvanceResult::NeedMore(self.prompt().unwrap_or_default()))
            }
            (CommandKind::Circle, 1) => {
                let v: f64 = trimmed
                    .parse()
                    .map_err(|_| SessionError::BadNumber(trimmed.into()))?;
                self.inputs.push(SessionInput::Number(v));
                Ok(AdvanceResult::Complete)
            }
            (CommandKind::Arc, step) if step < 3 => {
                let coord = parse_coordinate(trimmed)?;
                let p = resolve(&coord, self.current_pen);
                self.current_pen = p;
                self.inputs.push(SessionInput::Point(p));
                self.step += 1;
                if self.step >= 3 {
                    Ok(AdvanceResult::Complete)
                } else {
                    Ok(AdvanceResult::NeedMore(self.prompt().unwrap_or_default()))
                }
            }
            (CommandKind::Move | CommandKind::Copy, step) if step == 1 || step == 2 => {
                let coord = parse_coordinate(trimmed)?;
                let p = resolve(&coord, self.current_pen);
                self.current_pen = p;
                self.inputs.push(SessionInput::Point(p));
                self.step += 1;
                if self.step >= 3 {
                    Ok(AdvanceResult::Complete)
                } else {
                    Ok(AdvanceResult::NeedMore(self.prompt().unwrap_or_default()))
                }
            }
            _ => {
                self.inputs.push(SessionInput::Text(trimmed.into()));
                self.step += 1;
                Ok(self.maybe_complete())
            }
        }
    }

    fn maybe_complete(&mut self) -> AdvanceResult {
        if self.is_done() {
            AdvanceResult::Complete
        } else if let Some(p) = self.prompt() {
            AdvanceResult::NeedMore(p)
        } else {
            AdvanceResult::Complete
        }
    }

    fn is_done(&self) -> bool {
        match self.kind {
            CommandKind::Circle => self.step >= 2,
            CommandKind::Arc => self.step >= 3,
            CommandKind::Move | CommandKind::Copy => self.step >= 3,
            CommandKind::Erase | CommandKind::Undo | CommandKind::Redo => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_two_clicks_does_not_complete() {
        let mut s = CommandSession::start(CommandKind::Line);
        assert!(matches!(
            s.advance("0,0").unwrap(),
            AdvanceResult::NeedMore(_)
        ));
        assert!(matches!(
            s.advance("10,0").unwrap(),
            AdvanceResult::NeedMore(_)
        ));
        // Close finishes.
        assert_eq!(s.advance("close").unwrap(), AdvanceResult::Complete);
    }

    #[test]
    fn circle_center_then_radius_completes() {
        let mut s = CommandSession::start(CommandKind::Circle);
        s.advance("0,0").unwrap();
        let r = s.advance("5").unwrap();
        assert_eq!(r, AdvanceResult::Complete);
        // First input is a point, second is a number.
        assert!(matches!(s.inputs[0], SessionInput::Point(_)));
        assert!(matches!(s.inputs[1], SessionInput::Number(_)));
    }

    #[test]
    fn relative_polar_uses_pen_position() {
        let mut s = CommandSession::start(CommandKind::Line);
        s.advance("10,10").unwrap();
        s.advance("@5<0").unwrap();
        if let SessionInput::Point(p) = s.inputs[1] {
            assert!((p[0] - 15.0).abs() < 1e-9);
            assert!((p[1] - 10.0).abs() < 1e-9);
        }
    }

    #[test]
    fn cancel_returns_cancelled() {
        let mut s = CommandSession::start(CommandKind::Line);
        assert_eq!(s.advance("cancel").unwrap(), AdvanceResult::Cancelled);
    }
}
