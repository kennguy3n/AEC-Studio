//! Cycles final-render pipeline.

use crate::preset::{RenderPreset, RenderQuality};
use crate::scene::RenderScene;
use crate::worker::BlenderRequest;

pub struct CyclesPipeline;

impl CyclesPipeline {
    /// Build the request envelope for a Cycles render. If the supplied
    /// preset's quality is `Eevee`, upgrade it to `Standard` before sending
    /// — Cycles wouldn't honour the EEVEE-specific knobs anyway.
    pub fn build_request(
        scene: RenderScene,
        mut preset: RenderPreset,
        output_path: impl Into<String>,
    ) -> BlenderRequest {
        if preset.config.quality == RenderQuality::Eevee {
            preset.config.quality = RenderQuality::Standard;
        }
        BlenderRequest::CyclesRender {
            scene: Box::new(scene),
            preset: Box::new(preset),
            output_path: output_path.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_pipeline_upgrades_eevee_preset() {
        let req = CyclesPipeline::build_request(
            RenderScene::new(),
            RenderPreset::eevee_preview(),
            "out.png",
        );
        match req {
            BlenderRequest::CyclesRender { preset, .. } => {
                assert_eq!(preset.config.quality, RenderQuality::Standard);
            }
            _ => panic!("expected CyclesRender"),
        }
    }

    #[test]
    fn cycles_pipeline_keeps_standard_preset_intact() {
        let req =
            CyclesPipeline::build_request(RenderScene::new(), RenderPreset::high(), "out.png");
        match req {
            BlenderRequest::CyclesRender { preset, .. } => {
                assert_eq!(preset.config.quality, RenderQuality::High);
                assert_eq!(preset.config.samples, 256);
            }
            _ => panic!("expected CyclesRender"),
        }
    }
}
