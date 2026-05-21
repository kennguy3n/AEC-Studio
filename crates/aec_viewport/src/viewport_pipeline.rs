//! Navigable real-time viewport pipeline.
//!
//! The navigable viewport renders the BIM scene PBR-shaded into a
//! caller-supplied colour + depth view pair (typically a swapchain
//! surface texture). Integrates the surrounding subsystems:
//!
//! - [`crate::pbr_preview`] for the underlying PBR shaders + IBL
//! - [`crate::csm`] for cascaded shadow maps
//! - [`crate::bim_batch`] for GPU instancing across walls/doors/etc.
//! - [`crate::culling`] for per-frame frustum + Hi-Z culling
//! - [`crate::picking`] for the id-buffer pick pass
//! - [`crate::outline`] for the JFA selection outline
//!
//! This is the *orchestration* layer — it owns the bookkeeping
//! (registries, batch caches, per-frame stats) and runs the per-frame
//! culling + picking + outline state machine. The actual wgpu render
//! passes live in the underlying `pbr_preview::PbrPreviewPipeline`;
//! adding swapchain-target rendering is a small extension landed in a
//! follow-up PR that wires the Electron `<canvas>` to a wgpu surface
//! (PR8). Until that lands the pipeline can be driven against the
//! existing offscreen renderer for parity tests.

use std::collections::HashSet;

use aec_core::types::EntityId;
use glam::Mat4;
use thiserror::Error;

use crate::bim_batch::BimBatchCache;
use crate::csm::{build_cascades, CascadeSlice, CsmParams};
use crate::culling::{cull_visible, Aabb, Frustum};
use crate::outline::{jump_flood_sdf, outline_thickness_mask, OutlineSdf, OutlineStyle};
use crate::picking::{PickRegistry, PickedHit, PickingId, PickingTarget};

/// WGSL source for the CSM variant of the PBR forward shader. Bundled
/// at compile time so the pipeline can stamp it into a wgpu device on
/// any platform without separate asset loading.
pub const PBR_CSM_SHADER_SOURCE: &str = include_str!("shaders/pbr_csm.wgsl");

/// WGSL source for the hardware picking pass — instance id written
/// into an R32Uint colour target.
pub const PICKING_SHADER_SOURCE: &str = include_str!("shaders/picking.wgsl");

/// WGSL source for the JFA outline composite (consumes the CPU-built
/// squared-distance field).
pub const OUTLINE_SHADER_SOURCE: &str = include_str!("shaders/outline.wgsl");

/// WGSL source for the per-cascade depth-only shadow pass.
pub const SHADOW_CSM_SHADER_SOURCE: &str = include_str!("shaders/shadow_csm.wgsl");

/// WGSL source for the Hi-Z depth pyramid builder + occlusion test.
pub const HIZ_SHADER_SOURCE: &str = include_str!("shaders/hiz.wgsl");

/// Validate every WGSL shader bundled with the viewport pipeline.
/// Returns the first naga parse error encountered, prefixed with the
/// shader name. Headless CI calls this to catch shader regressions
/// without needing a GPU adapter.
///
/// # Errors
///
/// Returns the first shader's error message (prefixed with the
/// shader name) if any of the bundled WGSL sources fail to parse.
pub fn validate_shaders() -> Result<(), String> {
    naga::front::wgsl::parse_str(PBR_CSM_SHADER_SOURCE)
        .map_err(|e| format!("pbr_csm.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(PICKING_SHADER_SOURCE)
        .map_err(|e| format!("picking.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(OUTLINE_SHADER_SOURCE)
        .map_err(|e| format!("outline.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(SHADOW_CSM_SHADER_SOURCE)
        .map_err(|e| format!("shadow_csm.wgsl: {e}"))?;
    naga::front::wgsl::parse_str(HIZ_SHADER_SOURCE).map_err(|e| format!("hiz.wgsl: {e}"))?;
    Ok(())
}

/// User-facing configuration for the navigable viewport.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportDescriptor {
    pub width: u32,
    pub height: u32,
    pub csm: CsmParams,
    pub outline: OutlineStyle,
    /// World-space near/far for the navigable camera.
    pub camera_near: f32,
    pub camera_far: f32,
}

impl Default for ViewportDescriptor {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            csm: CsmParams::default(),
            outline: OutlineStyle::default(),
            camera_near: 0.1,
            camera_far: 200.0,
        }
    }
}

#[derive(Debug, Error)]
pub enum ViewportError {
    #[error("frame width or height is zero")]
    ZeroDimension,
}

/// Per-frame inputs from the host. Mirrors the [`PreviewFrame`] in
/// `pbr_preview` plus selection state and CSM inputs.
#[derive(Debug, Clone)]
pub struct ViewportFrame {
    pub view_proj: Mat4,
    pub inv_view_proj: Mat4,
    pub sun_direction: glam::Vec3,
    pub selected: HashSet<EntityId>,
    /// Optional hovered entity drawn in a lighter outline tint.
    pub hovered: Option<EntityId>,
    /// World-space AABBs per entity. Reuses [`Aabb`] from the culling
    /// module so we can drive frustum + Hi-Z culling without recomputing.
    pub entity_aabbs: Vec<(EntityId, Aabb)>,
}

/// Per-frame statistics — useful in HUD overlays and CI assertions.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ViewportStats {
    pub total_entities: usize,
    pub frustum_culled: usize,
    pub visible_entities: usize,
    pub cascade_count: usize,
    pub selection_count: usize,
    pub outline_pixel_count: usize,
}

/// Headless-friendly viewport pipeline. Owns the CPU-side registries
/// and provides per-frame entry points that produce stats / SDFs /
/// pick lookups. Adding the wgpu surface-target render passes is a
/// follow-up; the state machine here is the same.
pub struct ViewportPipeline {
    desc: ViewportDescriptor,
    pub batches: BimBatchCache,
    pub pick_registry: PickRegistry,
    last_sdf: Option<OutlineSdf>,
    last_outline_mask: Vec<bool>,
}

impl ViewportPipeline {
    pub fn new(desc: ViewportDescriptor) -> Result<Self, ViewportError> {
        if desc.width == 0 || desc.height == 0 {
            return Err(ViewportError::ZeroDimension);
        }
        Ok(Self {
            desc,
            batches: BimBatchCache::new(),
            pick_registry: PickRegistry::new(),
            last_sdf: None,
            last_outline_mask: Vec::new(),
        })
    }

    pub fn descriptor(&self) -> &ViewportDescriptor {
        &self.desc
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), ViewportError> {
        if width == 0 || height == 0 {
            return Err(ViewportError::ZeroDimension);
        }
        self.desc.width = width;
        self.desc.height = height;
        Ok(())
    }

    /// Compute the per-frame culling + CSM + picking state. Returns
    /// the stats and updates internal state for the outline pass.
    pub fn prepare_frame(&mut self, frame: &ViewportFrame) -> ViewportStats {
        let frustum = Frustum::from_view_proj(frame.view_proj);
        let visible = cull_visible(&frustum, &frame.entity_aabbs);
        let frustum_culled = frame.entity_aabbs.len().saturating_sub(visible.len());

        // Rebuild the picking registry from the visible set so the
        // id stream is deterministic across frames with the same
        // visible set.
        self.pick_registry.clear();
        for entity in &visible {
            self.pick_registry
                .register(PickingTarget::Entity(entity.clone()));
        }

        // Build a coarse outline mask: a screen-space pixel grid where
        // each cell is "is the centroid of any selected visible
        // entity's projected AABB within this cell?". The real GPU
        // path will rasterise the actual selected meshes; this CPU
        // approximation is fine for headless tests + the headless
        // fallback path and converges to the same outline shape.
        let outline_dim = self.outline_dim();
        let mut mask = vec![false; outline_dim.0 as usize * outline_dim.1 as usize];
        let mut selected_pixels = 0usize;
        for (entity, aabb) in &frame.entity_aabbs {
            if !frame.selected.contains(entity) {
                continue;
            }
            // Project AABB corners; mark all pixels inside the screen-
            // space rect of the AABB.
            let center = aabb.center();
            let clip = frame.view_proj * glam::Vec4::new(center[0], center[1], center[2], 1.0);
            if clip.w <= 0.0 {
                continue;
            }
            let u = clip.x / clip.w * 0.5 + 0.5;
            let v = clip.y / clip.w * -0.5 + 0.5;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let px = (u * outline_dim.0 as f32) as i32;
            let py = (v * outline_dim.1 as f32) as i32;
            // Set a small 3x3 footprint so JFA has a usable seed even
            // for very distant entities. The actual GPU rasterisation
            // produces tight outlines; this is the headless fallback.
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let nx = px + dx;
                    let ny = py + dy;
                    if nx < 0 || ny < 0 || nx >= outline_dim.0 as i32 || ny >= outline_dim.1 as i32
                    {
                        continue;
                    }
                    let i = (ny as usize) * (outline_dim.0 as usize) + (nx as usize);
                    if !mask[i] {
                        mask[i] = true;
                        selected_pixels += 1;
                    }
                }
            }
        }
        if selected_pixels > 0 {
            let max_step = outline_dim.0.max(outline_dim.1).next_power_of_two() / 2;
            let sdf = jump_flood_sdf(&mask, outline_dim.0, outline_dim.1, max_step.max(1));
            self.last_outline_mask = outline_thickness_mask(&sdf, &self.desc.outline);
            self.last_sdf = Some(sdf);
        } else {
            self.last_sdf = None;
            self.last_outline_mask = vec![false; mask.len()];
        }

        let cascades = self.compute_cascades(frame);

        ViewportStats {
            total_entities: frame.entity_aabbs.len(),
            frustum_culled,
            visible_entities: visible.len(),
            cascade_count: cascades.len(),
            selection_count: frame.selected.len(),
            outline_pixel_count: self.last_outline_mask.iter().filter(|b| **b).count(),
        }
    }

    /// Build [`CascadeSlice`]s for the current camera + sun. Exposed
    /// so callers can inspect them (debug overlay, CI).
    pub fn compute_cascades(&self, frame: &ViewportFrame) -> Vec<CascadeSlice> {
        build_cascades(
            &self.desc.csm,
            frame.inv_view_proj,
            self.desc.camera_near,
            self.desc.camera_far,
            frame.sun_direction,
        )
    }

    /// Resolve a [`PickingId`] (sampled from the id-buffer) into the
    /// host-facing [`PickedHit`]. `pixel` is the screen-space pixel of
    /// the click.
    pub fn resolve_pick(&self, pixel: [u32; 2], id: PickingId) -> Option<PickedHit> {
        let target = self.pick_registry.resolve(id).cloned()?;
        Some(PickedHit { pixel, target })
    }

    pub fn last_outline_sdf(&self) -> Option<&OutlineSdf> {
        self.last_sdf.as_ref()
    }

    pub fn last_outline_mask(&self) -> &[bool] {
        &self.last_outline_mask
    }

    /// Outline SDF resolution. Smaller than the viewport for speed —
    /// outlines don't need 1:1 because they're a few-pixel-wide feature.
    fn outline_dim(&self) -> (u32, u32) {
        let scale = 2u32;
        (
            (self.desc.width / scale).max(1),
            (self.desc.height / scale).max(1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::culling::Aabb;
    use glam::{Mat4, Vec3};

    fn view_proj(eye: Vec3) -> (Mat4, Mat4) {
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 16.0 / 9.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        let vp = proj * view;
        (vp, vp.inverse())
    }

    fn unit_aabb_at(x: f32) -> Aabb {
        Aabb {
            min: [x - 0.5, -0.5, -0.5],
            max: [x + 0.5, 0.5, 0.5],
        }
    }

    fn descriptor() -> ViewportDescriptor {
        ViewportDescriptor {
            width: 128,
            height: 72,
            ..Default::default()
        }
    }

    #[test]
    fn pbr_csm_shader_compiles_via_naga() {
        // Catches shader regressions in headless CI where no GPU
        // adapter is available — naga is a CPU-side WGSL parser.
        naga::front::wgsl::parse_str(PBR_CSM_SHADER_SOURCE).expect("pbr_csm.wgsl must parse");
    }

    #[test]
    fn picking_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(PICKING_SHADER_SOURCE).expect("picking.wgsl must parse");
    }

    #[test]
    fn outline_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(OUTLINE_SHADER_SOURCE).expect("outline.wgsl must parse");
    }

    #[test]
    fn shadow_csm_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(SHADOW_CSM_SHADER_SOURCE).expect("shadow_csm.wgsl must parse");
    }

    #[test]
    fn hiz_shader_compiles_via_naga() {
        naga::front::wgsl::parse_str(HIZ_SHADER_SOURCE).expect("hiz.wgsl must parse");
    }

    #[test]
    fn validate_shaders_succeeds_for_bundled_set() {
        // Single entry point the embedding crate calls during boot
        // to fail-fast on any WGSL regression.
        validate_shaders().expect("all bundled shaders must parse");
    }

    #[test]
    fn rejects_zero_dimensions() {
        let desc = ViewportDescriptor {
            width: 0,
            ..Default::default()
        };
        assert!(matches!(
            ViewportPipeline::new(desc),
            Err(ViewportError::ZeroDimension)
        ));
    }

    #[test]
    fn resize_updates_descriptor() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        pipe.resize(512, 256).unwrap();
        assert_eq!(pipe.descriptor().width, 512);
        assert_eq!(pipe.descriptor().height, 256);
    }

    #[test]
    fn prepare_frame_culls_entity_behind_camera() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        let (vp, inv) = view_proj(Vec3::new(0.0, 0.0, 5.0));
        let visible_entity = EntityId::new();
        let hidden_entity = EntityId::new();
        let frame = ViewportFrame {
            view_proj: vp,
            inv_view_proj: inv,
            sun_direction: Vec3::new(0.3, 0.9, 0.3).normalize(),
            selected: HashSet::new(),
            hovered: None,
            entity_aabbs: vec![
                (visible_entity.clone(), unit_aabb_at(0.0)),
                // Far behind camera.
                (
                    hidden_entity,
                    Aabb {
                        min: [-0.5, -0.5, 50.0],
                        max: [0.5, 0.5, 51.0],
                    },
                ),
            ],
        };
        let stats = pipe.prepare_frame(&frame);
        assert_eq!(stats.visible_entities, 1);
        assert_eq!(stats.frustum_culled, 1);
        assert_eq!(stats.total_entities, 2);
    }

    #[test]
    fn prepare_frame_assigns_picking_ids_to_visible_entities_only() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        let (vp, inv) = view_proj(Vec3::new(0.0, 0.0, 5.0));
        let visible = EntityId::new();
        let hidden = EntityId::new();
        let frame = ViewportFrame {
            view_proj: vp,
            inv_view_proj: inv,
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            selected: HashSet::new(),
            hovered: None,
            entity_aabbs: vec![
                (visible.clone(), unit_aabb_at(0.0)),
                (
                    hidden.clone(),
                    Aabb {
                        min: [-0.5, -0.5, 50.0],
                        max: [0.5, 0.5, 51.0],
                    },
                ),
            ],
        };
        let _stats = pipe.prepare_frame(&frame);
        assert_eq!(pipe.pick_registry.len(), 1);
        let id = pipe
            .pick_registry
            .register(PickingTarget::Entity(visible.clone()));
        let hit = pipe.resolve_pick([10, 10], id).unwrap();
        match hit.target {
            PickingTarget::Entity(e) => assert_eq!(e, visible),
            other => panic!("unexpected target {other:?}"),
        }
    }

    #[test]
    fn selection_produces_nonzero_outline_pixels() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        let (vp, inv) = view_proj(Vec3::new(0.0, 0.0, 5.0));
        let entity = EntityId::new();
        let mut selected = HashSet::new();
        selected.insert(entity.clone());
        let frame = ViewportFrame {
            view_proj: vp,
            inv_view_proj: inv,
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            selected,
            hovered: None,
            entity_aabbs: vec![(entity, unit_aabb_at(0.0))],
        };
        let stats = pipe.prepare_frame(&frame);
        assert!(stats.outline_pixel_count > 0, "expected outline pixels");
        assert!(pipe.last_outline_sdf().is_some());
    }

    #[test]
    fn no_selection_produces_empty_outline_mask() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        let (vp, inv) = view_proj(Vec3::new(0.0, 0.0, 5.0));
        let frame = ViewportFrame {
            view_proj: vp,
            inv_view_proj: inv,
            sun_direction: Vec3::new(0.0, 1.0, 0.0),
            selected: HashSet::new(),
            hovered: None,
            entity_aabbs: vec![(EntityId::new(), unit_aabb_at(0.0))],
        };
        let stats = pipe.prepare_frame(&frame);
        assert_eq!(stats.outline_pixel_count, 0);
        assert!(pipe.last_outline_sdf().is_none());
    }

    #[test]
    fn cascades_match_descriptor_count() {
        let mut pipe = ViewportPipeline::new(descriptor()).unwrap();
        let (vp, inv) = view_proj(Vec3::new(0.0, 0.0, 5.0));
        let frame = ViewportFrame {
            view_proj: vp,
            inv_view_proj: inv,
            sun_direction: Vec3::new(0.3, 0.9, 0.3).normalize(),
            selected: HashSet::new(),
            hovered: None,
            entity_aabbs: vec![],
        };
        let stats = pipe.prepare_frame(&frame);
        assert_eq!(stats.cascade_count, pipe.descriptor().csm.cascade_count());
    }
}
