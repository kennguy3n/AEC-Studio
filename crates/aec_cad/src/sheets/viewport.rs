//! Sheet viewport — a clipped window into model space at a given scale.

use serde::{Deserialize, Serialize};

use crate::primitives::Bbox;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetViewport {
    pub name: String,
    /// Region of paper (mm) where the viewport is drawn.
    pub paper_origin: [f64; 2],
    pub paper_size: [f64; 2],
    /// Region of model space (drawing units) the viewport shows.
    pub model_center: [f64; 2],
    /// Linear scale: 1 model unit → `scale` paper mm. Common values:
    /// `1/50.0` for 1:50, `1/100.0` for 1:100, etc.
    pub scale: f64,
    pub rotation_deg: f64,
    /// Layers frozen in this viewport (in addition to globally frozen).
    pub frozen_layers: Vec<String>,
}

impl SheetViewport {
    pub fn new(name: impl Into<String>, paper_origin: [f64; 2], paper_size: [f64; 2]) -> Self {
        Self {
            name: name.into(),
            paper_origin,
            paper_size,
            model_center: [0.0, 0.0],
            scale: 1.0 / 100.0,
            rotation_deg: 0.0,
            frozen_layers: Vec::new(),
        }
    }

    /// Convert a model-space point to paper-space (mm).
    pub fn model_to_paper(&self, p: [f64; 2]) -> [f64; 2] {
        let dx = p[0] - self.model_center[0];
        let dy = p[1] - self.model_center[1];
        let s = self.scale;
        let (sin_t, cos_t) = self.rotation_deg.to_radians().sin_cos();
        let rx = dx * cos_t - dy * sin_t;
        let ry = dx * sin_t + dy * cos_t;
        [
            self.paper_origin[0] + self.paper_size[0] * 0.5 + rx * s,
            self.paper_origin[1] + self.paper_size[1] * 0.5 + ry * s,
        ]
    }

    /// The model-space bounding box this viewport sees.
    pub fn model_bbox(&self) -> Bbox {
        let half_w = self.paper_size[0] * 0.5 / self.scale;
        let half_h = self.paper_size[1] * 0.5 / self.scale;
        Bbox {
            min: [self.model_center[0] - half_w, self.model_center[1] - half_h],
            max: [self.model_center[0] + half_w, self.model_center[1] + half_h],
        }
    }

    /// Clip a model-space point: returns Some(paper-space) if visible.
    pub fn clip_to_paper(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let bbox = self.model_bbox();
        if bbox.contains(p) {
            Some(self.model_to_paper(p))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_maps_to_viewport_center() {
        let vp = SheetViewport::new("V1", [10.0, 10.0], [100.0, 50.0]);
        let p = vp.model_to_paper([0.0, 0.0]);
        assert!((p[0] - 60.0).abs() < 1e-6);
        assert!((p[1] - 35.0).abs() < 1e-6);
    }

    #[test]
    fn model_bbox_uses_scale() {
        let mut vp = SheetViewport::new("V1", [0.0, 0.0], [100.0, 100.0]);
        vp.scale = 1.0 / 50.0;
        let bb = vp.model_bbox();
        // 100 mm * 50 = 5000 model units across the viewport.
        assert!((bb.max[0] - bb.min[0] - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn clip_returns_none_when_outside() {
        let vp = SheetViewport::new("V1", [0.0, 0.0], [10.0, 10.0]);
        assert!(vp.clip_to_paper([1e6, 1e6]).is_none());
    }
}
