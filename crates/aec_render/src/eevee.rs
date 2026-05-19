//! EEVEE preview pipeline. Builds a [`BlenderRequest::EeveeRender`] from
//! the supplied scene + preset and forwards it to the [`BlenderWorker`].

use crate::preset::{RenderPreset, RenderQuality};
use crate::scene::RenderScene;
use crate::worker::BlenderRequest;

pub struct EeveePipeline;

impl EeveePipeline {
    /// Build the request envelope for an EEVEE render.
    ///
    /// Force-coerces the preset's quality to [`RenderQuality::Eevee`] so the
    /// worker uses the EEVEE engine even if the caller accidentally passed
    /// a Cycles preset.
    pub fn build_request(
        scene: RenderScene,
        mut preset: RenderPreset,
        output_path: impl Into<String>,
    ) -> BlenderRequest {
        preset.config.quality = RenderQuality::Eevee;
        BlenderRequest::EeveeRender {
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
    fn request_uses_eevee_quality() {
        let req =
            EeveePipeline::build_request(RenderScene::new(), RenderPreset::studio(), "out.png");
        match req {
            BlenderRequest::EeveeRender { preset, .. } => {
                assert_eq!(preset.config.quality, RenderQuality::Eevee);
            }
            _ => panic!("expected EeveeRender"),
        }
    }
}
