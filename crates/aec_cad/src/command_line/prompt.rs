//! Context-sensitive prompts shown after each step of a multi-step
//! command.

use crate::command_line::parser::CommandKind;

/// Build a prompt string in AutoCAD style: `Prompt [Opt1/Opt2]`.
pub fn build_prompt(prompt: &str, options: &[&str]) -> String {
    if options.is_empty() {
        format!("{prompt}: ")
    } else {
        let opts = options.join("/");
        format!("{prompt} [{opts}]: ")
    }
}

/// Prompt for step `n` of command `kind`. Returns `None` when the
/// command is complete.
pub fn step_prompt(kind: CommandKind, step: usize) -> Option<String> {
    match (kind, step) {
        (CommandKind::Line, 0) => Some(build_prompt("Specify first point", &[])),
        (CommandKind::Line, _) => Some(build_prompt("Specify next point or", &["Close", "Undo"])),
        (CommandKind::Polyline, 0) => Some(build_prompt("Specify start point", &[])),
        (CommandKind::Polyline, _) => Some(build_prompt(
            "Specify next point or",
            &["Arc", "Close", "Undo"],
        )),
        (CommandKind::Circle, 0) => Some(build_prompt(
            "Specify center point or",
            &["3P", "2P", "Ttr"],
        )),
        (CommandKind::Circle, 1) => Some(build_prompt("Specify radius or", &["Diameter"])),
        (CommandKind::Arc, 0) => Some(build_prompt("Specify start point of arc", &["Center"])),
        (CommandKind::Arc, 1) => Some(build_prompt("Specify second point of arc", &[])),
        (CommandKind::Arc, 2) => Some(build_prompt("Specify end point of arc", &[])),
        (CommandKind::Offset, 0) => Some(build_prompt("Specify offset distance or", &["Through"])),
        (CommandKind::Offset, 1) => Some(build_prompt("Select object to offset", &["Exit"])),
        (CommandKind::Offset, 2) => Some(build_prompt(
            "Specify point on side to offset or",
            &["Exit"],
        )),
        (CommandKind::Copy, 0) => Some(build_prompt("Select objects to copy", &[])),
        (CommandKind::Copy, 1) => Some(build_prompt("Specify base point", &["Displacement"])),
        (CommandKind::Copy, 2) => Some(build_prompt("Specify second point or", &["Array", "Exit"])),
        (CommandKind::Move, 0) => Some(build_prompt("Select objects to move", &[])),
        (CommandKind::Move, 1) => Some(build_prompt("Specify base point", &[])),
        (CommandKind::Move, 2) => Some(build_prompt("Specify destination point", &[])),
        (CommandKind::Mirror, 0) => Some(build_prompt("Select objects to mirror", &[])),
        (CommandKind::Mirror, 1) => Some(build_prompt("Specify first point of mirror line", &[])),
        (CommandKind::Mirror, 2) => Some(build_prompt("Specify second point of mirror line", &[])),
        (CommandKind::Mirror, 3) => Some(build_prompt("Erase source objects?", &["Yes", "No"])),
        (CommandKind::Rotate, 0) => Some(build_prompt("Select objects to rotate", &[])),
        (CommandKind::Rotate, 1) => Some(build_prompt("Specify base point", &[])),
        (CommandKind::Rotate, 2) => Some(build_prompt("Specify rotation angle or", &["Reference"])),
        (CommandKind::Scale, 0) => Some(build_prompt("Select objects to scale", &[])),
        (CommandKind::Scale, 1) => Some(build_prompt("Specify base point", &[])),
        (CommandKind::Scale, 2) => Some(build_prompt("Specify scale factor or", &["Reference"])),
        (CommandKind::Trim, 0) => Some(build_prompt("Select cutting edges", &[])),
        (CommandKind::Trim, _) => Some(build_prompt("Select object to trim", &["Exit"])),
        (CommandKind::Extend, 0) => Some(build_prompt("Select boundary edges", &[])),
        (CommandKind::Extend, _) => Some(build_prompt("Select object to extend", &["Exit"])),
        (CommandKind::Fillet, 0) => Some(build_prompt(
            "Specify fillet radius or",
            &["Polyline", "Trim"],
        )),
        (CommandKind::Fillet, 1) => Some(build_prompt("Select first object", &[])),
        (CommandKind::Fillet, 2) => Some(build_prompt("Select second object", &[])),
        (CommandKind::Chamfer, 0) => Some(build_prompt(
            "Specify first chamfer distance",
            &["Distance", "Angle"],
        )),
        (CommandKind::Chamfer, 1) => Some(build_prompt("Specify second chamfer distance", &[])),
        (CommandKind::Chamfer, 2) => Some(build_prompt("Select first line", &[])),
        (CommandKind::Chamfer, 3) => Some(build_prompt("Select second line", &[])),
        (CommandKind::Erase, 0) => Some(build_prompt("Select objects to erase", &[])),
        (CommandKind::Layer, 0) => Some(build_prompt(
            "Layer action",
            &[
                "New", "Rename", "Freeze", "Thaw", "Lock", "Unlock", "Current",
            ],
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_first_prompt() {
        assert!(step_prompt(CommandKind::Line, 0)
            .unwrap()
            .contains("first point"));
    }

    #[test]
    fn line_continuation_offers_close_and_undo() {
        let p = step_prompt(CommandKind::Line, 1).unwrap();
        assert!(p.contains("Close"));
        assert!(p.contains("Undo"));
    }

    #[test]
    fn circle_radius_step() {
        assert!(step_prompt(CommandKind::Circle, 1)
            .unwrap()
            .contains("radius"));
    }
}
