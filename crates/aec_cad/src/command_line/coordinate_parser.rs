//! Parse the CAD command-line coordinate notations.
//!
//! Supported syntax:
//! - Absolute Cartesian:  `x,y`
//! - Relative Cartesian:  `@dx,dy`
//! - Relative polar:      `@dist<angle`
//! - Absolute polar:      `dist<angle` (rare, but supported)

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum CoordinateInput {
    Absolute([f64; 2]),
    Relative([f64; 2]),
    AbsolutePolar { distance: f64, angle_deg: f64 },
    RelativePolar { distance: f64, angle_deg: f64 },
}

#[derive(Debug, Error)]
pub enum CoordinateError {
    #[error("empty input")]
    Empty,
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    #[error("syntax error: {0}")]
    Syntax(String),
}

pub fn parse_coordinate(input: &str) -> Result<CoordinateInput, CoordinateError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(CoordinateError::Empty);
    }
    let (relative, body) = if let Some(rest) = s.strip_prefix('@') {
        (true, rest)
    } else {
        (false, s)
    };
    if let Some((dist, angle)) = body.split_once('<') {
        let d: f64 = dist
            .trim()
            .parse()
            .map_err(|_| CoordinateError::InvalidNumber(dist.into()))?;
        let a: f64 = angle
            .trim()
            .parse()
            .map_err(|_| CoordinateError::InvalidNumber(angle.into()))?;
        return Ok(if relative {
            CoordinateInput::RelativePolar {
                distance: d,
                angle_deg: a,
            }
        } else {
            CoordinateInput::AbsolutePolar {
                distance: d,
                angle_deg: a,
            }
        });
    }
    let mut parts = body.split(',');
    let x_s = parts
        .next()
        .ok_or_else(|| CoordinateError::Syntax(input.into()))?
        .trim();
    let y_s = parts
        .next()
        .ok_or_else(|| CoordinateError::Syntax(input.into()))?
        .trim();
    if parts.next().is_some() {
        return Err(CoordinateError::Syntax(format!(
            "too many components in `{input}`"
        )));
    }
    let x: f64 = x_s
        .parse()
        .map_err(|_| CoordinateError::InvalidNumber(x_s.into()))?;
    let y: f64 = y_s
        .parse()
        .map_err(|_| CoordinateError::InvalidNumber(y_s.into()))?;
    Ok(if relative {
        CoordinateInput::Relative([x, y])
    } else {
        CoordinateInput::Absolute([x, y])
    })
}

/// Apply a [`CoordinateInput`] against the current pen position to yield
/// a world-space point.
pub fn resolve(input: &CoordinateInput, current: [f64; 2]) -> [f64; 2] {
    match *input {
        CoordinateInput::Absolute(p) => p,
        CoordinateInput::Relative(d) => [current[0] + d[0], current[1] + d[1]],
        CoordinateInput::AbsolutePolar {
            distance,
            angle_deg,
        } => {
            let a = angle_deg.to_radians();
            [distance * a.cos(), distance * a.sin()]
        }
        CoordinateInput::RelativePolar {
            distance,
            angle_deg,
        } => {
            let a = angle_deg.to_radians();
            [
                current[0] + distance * a.cos(),
                current[1] + distance * a.sin(),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_cartesian() {
        assert_eq!(
            parse_coordinate("3,4").unwrap(),
            CoordinateInput::Absolute([3.0, 4.0])
        );
    }

    #[test]
    fn relative_cartesian() {
        assert_eq!(
            parse_coordinate("@1,2").unwrap(),
            CoordinateInput::Relative([1.0, 2.0])
        );
    }

    #[test]
    fn relative_polar_at_45_deg() {
        let c = parse_coordinate("@10<45").unwrap();
        let resolved = resolve(&c, [0.0, 0.0]);
        assert!((resolved[0] - 10.0_f64 / 2.0_f64.sqrt()).abs() < 1e-9);
        assert!((resolved[1] - 10.0_f64 / 2.0_f64.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn invalid_number_fails() {
        assert!(parse_coordinate("foo,bar").is_err());
    }

    #[test]
    fn empty_input_errors() {
        assert!(parse_coordinate("").is_err());
    }
}
