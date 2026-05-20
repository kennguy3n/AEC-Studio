//! Parse the user-typed CAD command alias to its canonical name + args.
//!
//! The aliases mirror AutoCAD / QCAD conventions (`L` = LINE, `PL` =
//! POLYLINE, etc.). The first whitespace-separated token is the alias;
//! remaining tokens are passed back to the caller as raw strings.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommandKind {
    Line,
    Polyline,
    Circle,
    Arc,
    Offset,
    Copy,
    Move,
    Mirror,
    Rotate,
    Scale,
    Trim,
    Extend,
    Fillet,
    Chamfer,
    Erase,
    Undo,
    Redo,
    Distance,
    Area,
    Zoom,
    Pan,
    Layer,
    Save,
    Open,
    Help,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommand {
    pub kind: CommandKind,
    pub args: Vec<String>,
    pub original: String,
}

#[derive(Debug, Clone)]
pub struct CommandParser {
    aliases: HashMap<String, CommandKind>,
}

impl Default for CommandParser {
    fn default() -> Self {
        let mut aliases = HashMap::new();
        for (k, v) in [
            ("L", CommandKind::Line),
            ("LINE", CommandKind::Line),
            ("PL", CommandKind::Polyline),
            ("PLINE", CommandKind::Polyline),
            ("POLYLINE", CommandKind::Polyline),
            ("C", CommandKind::Circle),
            ("CIRCLE", CommandKind::Circle),
            ("A", CommandKind::Arc),
            ("ARC", CommandKind::Arc),
            ("O", CommandKind::Offset),
            ("OFFSET", CommandKind::Offset),
            ("CO", CommandKind::Copy),
            ("COPY", CommandKind::Copy),
            ("CP", CommandKind::Copy),
            ("MO", CommandKind::Move),
            ("M", CommandKind::Move),
            ("MOVE", CommandKind::Move),
            ("MI", CommandKind::Mirror),
            ("MIRROR", CommandKind::Mirror),
            ("RO", CommandKind::Rotate),
            ("ROTATE", CommandKind::Rotate),
            ("SC", CommandKind::Scale),
            ("SCALE", CommandKind::Scale),
            ("TR", CommandKind::Trim),
            ("TRIM", CommandKind::Trim),
            ("EX", CommandKind::Extend),
            ("EXTEND", CommandKind::Extend),
            ("F", CommandKind::Fillet),
            ("FILLET", CommandKind::Fillet),
            ("CH", CommandKind::Chamfer),
            ("CHAMFER", CommandKind::Chamfer),
            ("E", CommandKind::Erase),
            ("ERASE", CommandKind::Erase),
            ("DEL", CommandKind::Erase),
            ("U", CommandKind::Undo),
            ("UNDO", CommandKind::Undo),
            ("REDO", CommandKind::Redo),
            ("DI", CommandKind::Distance),
            ("DIST", CommandKind::Distance),
            ("AR", CommandKind::Area),
            ("AREA", CommandKind::Area),
            ("Z", CommandKind::Zoom),
            ("ZOOM", CommandKind::Zoom),
            ("P", CommandKind::Pan),
            ("PAN", CommandKind::Pan),
            ("LA", CommandKind::Layer),
            ("LAYER", CommandKind::Layer),
            ("SAVE", CommandKind::Save),
            ("OPEN", CommandKind::Open),
            ("?", CommandKind::Help),
            ("HELP", CommandKind::Help),
        ] {
            aliases.insert(k.to_ascii_uppercase(), v);
        }
        Self { aliases }
    }
}

impl CommandParser {
    pub fn parse(&self, input: &str) -> Option<ParsedCommand> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut tokens = trimmed.split_whitespace();
        let head = tokens.next()?.to_ascii_uppercase();
        let kind = *self.aliases.get(&head)?;
        let args = tokens.map(str::to_string).collect();
        Some(ParsedCommand {
            kind,
            args,
            original: trimmed.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_alias() {
        let p = CommandParser::default();
        let c = p.parse("L 0,0 10,0").unwrap();
        assert_eq!(c.kind, CommandKind::Line);
        assert_eq!(c.args, vec!["0,0", "10,0"]);
    }

    #[test]
    fn parses_full_name() {
        let p = CommandParser::default();
        let c = p.parse("FILLET").unwrap();
        assert_eq!(c.kind, CommandKind::Fillet);
    }

    #[test]
    fn case_insensitive() {
        let p = CommandParser::default();
        let c = p.parse("circle 0,0 5").unwrap();
        assert_eq!(c.kind, CommandKind::Circle);
    }

    #[test]
    fn unknown_alias_returns_none() {
        let p = CommandParser::default();
        assert!(p.parse("XYZZY").is_none());
    }

    #[test]
    fn empty_returns_none() {
        let p = CommandParser::default();
        assert!(p.parse("   ").is_none());
    }
}
