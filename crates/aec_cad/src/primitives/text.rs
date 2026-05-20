//! Text and MText primitives.

use serde::{Deserialize, Serialize};

use crate::primitives::traits::{
    Affine2, Bbox, Drawable, Selectable, SnapKind, SnapPoint, Snappable, Transformable,
};

/// Horizontal text alignment (DXF group 72).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HAlign {
    Left,
    Center,
    Right,
    Aligned,
    Middle,
    Fit,
}

/// Vertical text alignment (DXF group 73).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VAlign {
    Baseline,
    Bottom,
    Middle,
    Top,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub layer: String,
    pub position: [f64; 2],
    pub height: f64,
    pub rotation_deg: f64,
    pub style: String,
    pub content: String,
    pub h_align: HAlign,
    pub v_align: VAlign,
    #[serde(default)]
    pub width_factor: f64,
    #[serde(default)]
    pub oblique_angle_deg: f64,
}

impl Text {
    pub fn new(
        layer: impl Into<String>,
        position: [f64; 2],
        height: f64,
        content: impl Into<String>,
    ) -> Self {
        Self {
            layer: layer.into(),
            position,
            height,
            rotation_deg: 0.0,
            style: "STANDARD".into(),
            content: content.into(),
            h_align: HAlign::Left,
            v_align: VAlign::Baseline,
            width_factor: 1.0,
            oblique_angle_deg: 0.0,
        }
    }

    pub fn rendered_width(&self) -> f64 {
        // Approximation: 0.6 * height * width_factor per character.
        let w = if self.width_factor <= 0.0 {
            1.0
        } else {
            self.width_factor
        };
        0.6 * self.height * w * (self.content.chars().count() as f64)
    }
}

impl Drawable for Text {
    fn bbox(&self) -> Bbox {
        let width = self.rendered_width();
        let height = self.height;
        let (sin_t, cos_t) = self.rotation_deg.to_radians().sin_cos();
        let corners = [[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]];
        let mut bbox = Bbox::from_point(self.position);
        for corner in corners {
            let px = self.position[0] + corner[0] * cos_t - corner[1] * sin_t;
            let py = self.position[1] + corner[0] * sin_t + corner[1] * cos_t;
            bbox.extend([px, py]);
        }
        bbox
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for Text {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let b = self.bbox();
        if b.contains(point) {
            return 0.0;
        }
        let dx = if point[0] < b.min[0] {
            b.min[0] - point[0]
        } else if point[0] > b.max[0] {
            point[0] - b.max[0]
        } else {
            0.0
        };
        let dy = if point[1] < b.min[1] {
            b.min[1] - point[1]
        } else if point[1] > b.max[1] {
            point[1] - b.max[1]
        } else {
            0.0
        };
        dx * dx + dy * dy
    }

    fn inside(&self, window: &Bbox) -> bool {
        let b = self.bbox();
        window.contains(b.min) && window.contains(b.max)
    }
}

impl Snappable for Text {
    fn snap_points(&self) -> Vec<SnapPoint> {
        vec![SnapPoint {
            kind: SnapKind::Insertion,
            at: self.position,
        }]
    }
}

impl Transformable for Text {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            position: t.apply(self.position),
            height: self.height * (t.scale[0].abs() + t.scale[1].abs()) * 0.5,
            rotation_deg: self.rotation_deg + t.rotation_deg,
            style: self.style.clone(),
            content: self.content.clone(),
            h_align: self.h_align,
            v_align: self.v_align,
            width_factor: self.width_factor,
            oblique_angle_deg: self.oblique_angle_deg,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MText {
    pub layer: String,
    pub position: [f64; 2],
    pub height: f64,
    pub width: f64,
    pub rotation_deg: f64,
    pub content: String,
    pub style: String,
    pub line_spacing: f64,
}

impl MText {
    pub fn new(
        layer: impl Into<String>,
        position: [f64; 2],
        height: f64,
        width: f64,
        content: impl Into<String>,
    ) -> Self {
        Self {
            layer: layer.into(),
            position,
            height,
            width,
            rotation_deg: 0.0,
            content: content.into(),
            style: "STANDARD".into(),
            line_spacing: 1.0,
        }
    }

    pub fn line_count(&self) -> usize {
        self.content.split('\n').count().max(1)
    }

    pub fn rendered_height(&self) -> f64 {
        self.line_count() as f64 * self.height * self.line_spacing
    }
}

impl Drawable for MText {
    fn bbox(&self) -> Bbox {
        let width = self.width;
        let height = self.rendered_height();
        let (sin_t, cos_t) = self.rotation_deg.to_radians().sin_cos();
        let mut bbox = Bbox::from_point(self.position);
        for corner in [[0.0, 0.0], [width, 0.0], [width, height], [0.0, height]] {
            let px = self.position[0] + corner[0] * cos_t - corner[1] * sin_t;
            let py = self.position[1] + corner[0] * sin_t + corner[1] * cos_t;
            bbox.extend([px, py]);
        }
        bbox
    }

    fn layer(&self) -> &str {
        &self.layer
    }
}

impl Selectable for MText {
    fn distance2(&self, point: [f64; 2]) -> f64 {
        let b = self.bbox();
        if b.contains(point) {
            return 0.0;
        }
        let dx = (point[0] - b.min[0].max(point[0].min(b.max[0]))).abs();
        let dy = (point[1] - b.min[1].max(point[1].min(b.max[1]))).abs();
        dx * dx + dy * dy
    }

    fn inside(&self, window: &Bbox) -> bool {
        let b = self.bbox();
        window.contains(b.min) && window.contains(b.max)
    }
}

impl Snappable for MText {
    fn snap_points(&self) -> Vec<SnapPoint> {
        vec![SnapPoint {
            kind: SnapKind::Insertion,
            at: self.position,
        }]
    }
}

impl Transformable for MText {
    fn transformed(&self, t: &Affine2) -> Self {
        Self {
            layer: self.layer.clone(),
            position: t.apply(self.position),
            height: self.height * (t.scale[0].abs() + t.scale[1].abs()) * 0.5,
            width: self.width * t.scale[0].abs(),
            rotation_deg: self.rotation_deg + t.rotation_deg,
            content: self.content.clone(),
            style: self.style.clone(),
            line_spacing: self.line_spacing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_bbox_includes_insertion() {
        let t = Text::new("0", [0.0, 0.0], 1.0, "Hello");
        let b = t.bbox();
        assert!(b.contains([0.0, 0.0]));
    }

    #[test]
    fn mtext_line_count_from_newlines() {
        let m = MText::new("0", [0.0, 0.0], 1.0, 10.0, "line1\nline2\nline3");
        assert_eq!(m.line_count(), 3);
    }

    #[test]
    fn text_transform_rotation() {
        let t = Text::new("0", [0.0, 0.0], 1.0, "X");
        let m = Affine2::rotation_about([0.0, 0.0], 90.0);
        let tt = t.transformed(&m);
        assert!((tt.rotation_deg - 90.0).abs() < 1e-9);
    }
}
