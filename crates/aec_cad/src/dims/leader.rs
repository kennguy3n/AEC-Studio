//! Leader (and multileader) — text + arrow + connector segments.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leader {
    pub layer: String,
    pub style: String,
    /// Arrow tip + intermediate kink + text attachment.
    pub vertices: Vec<[f64; 2]>,
    pub text: String,
    pub text_height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Multileader {
    pub layer: String,
    pub style: String,
    pub leaders: Vec<Vec<[f64; 2]>>,
    pub text: String,
    pub text_height: f64,
    pub landing_length: f64,
}

impl Leader {
    pub fn total_length(&self) -> f64 {
        let mut len = 0.0;
        for win in self.vertices.windows(2) {
            let dx = win[1][0] - win[0][0];
            let dy = win[1][1] - win[0][1];
            len += (dx * dx + dy * dy).sqrt();
        }
        len
    }
}

impl Multileader {
    pub fn arrow_count(&self) -> usize {
        self.leaders.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_length_two_segments() {
        let l = Leader {
            layer: "0".into(),
            style: "STANDARD".into(),
            vertices: vec![[0.0, 0.0], [3.0, 0.0], [3.0, 4.0]],
            text: "Note".into(),
            text_height: 2.5,
        };
        assert!((l.total_length() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn multileader_arrow_count() {
        let m = Multileader {
            layer: "0".into(),
            style: "STANDARD".into(),
            leaders: vec![vec![[0.0, 0.0]], vec![[1.0, 1.0]]],
            text: "Note".into(),
            text_height: 2.5,
            landing_length: 2.0,
        };
        assert_eq!(m.arrow_count(), 2);
    }
}
