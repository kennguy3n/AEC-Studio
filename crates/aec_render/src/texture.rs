//! CPU-side texture atlas for the native path tracer and the PBR
//! preview rasteriser.
//!
//! Each [`PbrMaterial`] in `aec_materials` may reference up to four
//! texture maps (albedo, normal, metallic/roughness, emissive). At
//! scene-build time `FinalRenderPipeline` resolves those `TextureRef`
//! handles to concrete on-disk image blobs and registers them with a
//! [`TextureAtlas`]. The atlas stores decoded `[f32; 4]` linear pixel
//! data plus a MIP-map chain (box-filter downsample) so the renderer
//! can perform bilinear sampling at any LOD without re-decoding on
//! every ray hit.
//!
//! The atlas is intentionally CPU-only — the GPU path tracer falls
//! back to CPU rendering whenever the scene uses textures (the wgpu
//! bindless-texture-array path is documented in `gpu_trace.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glam::{Vec3, Vec4};
use image::{DynamicImage, GenericImageView};

/// Whether an on-disk 8-bit texture is sRGB-encoded (the standard for
/// **colour** textures — albedo, emissive) or already linear (the
/// standard for **data** textures — normal maps, metallic/roughness,
/// AO, displacement, height fields).
///
/// The path tracer's BSDF assumes linear-space inputs (textures and
/// flat fields alike); applying the sRGB → linear transfer to a data
/// texture would systematically tilt the decoded values — a mid-grey
/// `128/255 = 0.502` normal-map texel would decode to `0.214` and
/// then map to a tangent-space normal of `-0.572` instead of the
/// flat `±0.0` the artist authored. Likewise a `0.5` roughness
/// channel in a glTF MR texture would be reported as `0.214`.
///
/// Float (HDR) textures (`.hdr`, `.exr`, `Rgb32F`, `Rgba32F`) are
/// **always** treated as already-linear regardless of this setting:
/// the Radiance / OpenEXR formats encode linear-light data by
/// definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorSpace {
    /// 8-bit values are sRGB-encoded; apply the standard sRGB → linear
    /// transfer at decode time. This is the default and the correct
    /// setting for albedo / base-colour and emissive maps.
    #[default]
    Srgb,
    /// 8-bit values are already linear (data textures). Each channel
    /// is decoded as `byte / 255.0` with no gamma applied.
    Linear,
}

/// Opaque handle into a [`TextureAtlas`]. Returned by
/// [`TextureAtlas::register_image`] / [`TextureAtlas::load_path`] and
/// consumed by [`bilinear_sample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(u32);

impl TextureId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Texture errors returned by [`TextureAtlas::load_path`] and friends.
#[derive(Debug, thiserror::Error)]
pub enum TextureError {
    #[error("texture file not found: {0}")]
    NotFound(PathBuf),
    #[error("texture decode failed for {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("texture {id} not in atlas")]
    UnknownId { id: u32 },
}

/// One MIP level of a texture: `width × height` linear-space RGBA
/// pixels in `[0, +∞)` (HDR-safe).
#[derive(Debug, Clone)]
pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

impl MipLevel {
    #[inline]
    fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        // Bounds-checked indexing matches the rest of the renderer's
        // safety posture (`#![forbid(unsafe_code)]` at the workspace
        // level). Wrap with modulo so the sampler implements the GL
        // `REPEAT` wrap mode without panicking on out-of-range UVs.
        let xi = x.rem_euclid(self.width.max(1));
        let yi = y.rem_euclid(self.height.max(1));
        self.pixels[(yi as usize) * (self.width as usize) + (xi as usize)]
    }
}

/// One entry in a [`TextureAtlas`]: a MIP chain plus the source path
/// (kept for diagnostics — the atlas otherwise owns the decoded
/// pixels and the on-disk file is never re-read).
#[derive(Debug, Clone)]
pub struct AtlasEntry {
    pub source: Option<PathBuf>,
    pub levels: Vec<MipLevel>,
}

impl AtlasEntry {
    pub fn base_width(&self) -> u32 {
        self.levels.first().map_or(0, |l| l.width)
    }
    pub fn base_height(&self) -> u32 {
        self.levels.first().map_or(0, |l| l.height)
    }
    pub fn mip_count(&self) -> usize {
        self.levels.len()
    }
}

/// CPU-side texture atlas. Owns decoded pixel data + MIP chains for
/// every texture referenced by the active `MaterialLibrary`.
#[derive(Debug, Default, Clone)]
pub struct TextureAtlas {
    entries: Vec<AtlasEntry>,
    /// Dedup cache: maps a `(canonicalised path, color space)` pair to
    /// the previously-registered [`TextureId`] so a second
    /// [`Self::load_path_with_color_space`] for the same on-disk file
    /// reuses the existing decode + MIP chain instead of decoding
    /// twice. Critical for projects where many materials reference
    /// the same shared albedo / normal map.
    path_dedup: HashMap<(PathBuf, ColorSpace), TextureId>,
}

impl TextureAtlas {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, id: TextureId) -> Option<&AtlasEntry> {
        self.entries.get(id.index())
    }

    /// Decode a PNG / JPEG file from disk and register it with the
    /// atlas, assuming the file is sRGB-encoded (the convention for
    /// **colour** textures: albedo, emissive). For **data** textures
    /// — normal maps, metallic/roughness, AO, displacement — use
    /// [`Self::load_path_with_color_space`] with [`ColorSpace::Linear`].
    ///
    /// If the same path was already loaded with the same color space,
    /// the existing [`TextureId`] is returned and no second decode
    /// runs (dedup cache).
    pub fn load_path(&mut self, path: impl AsRef<Path>) -> Result<TextureId, TextureError> {
        self.load_path_with_color_space(path, ColorSpace::Srgb)
    }

    /// Decode a PNG / JPEG file from disk and register it with the
    /// atlas, treating its 8-bit values as either sRGB (colour textures:
    /// albedo, emissive) or linear (data textures: normal map,
    /// metallic-roughness, AO, displacement). The MIP chain is
    /// generated eagerly via box-filter downsample down to a 1×1 base.
    ///
    /// HDR float inputs (`.hdr`, `.exr`) are always treated as
    /// linear-light regardless of `color_space`.
    ///
    /// Repeat calls for the same `(path, color_space)` pair reuse the
    /// previously-registered [`TextureId`] (dedup cache). Calling with
    /// a different color space for the same path produces a fresh
    /// entry — the two decode paths are not interchangeable.
    pub fn load_path_with_color_space(
        &mut self,
        path: impl AsRef<Path>,
        color_space: ColorSpace,
    ) -> Result<TextureId, TextureError> {
        let path_ref = path.as_ref();
        let canonical = path_ref.to_path_buf();
        if let Some(id) = self.path_dedup.get(&(canonical.clone(), color_space)) {
            return Ok(*id);
        }
        let img = image::open(path_ref).map_err(|e| TextureError::Decode {
            path: path_ref.to_path_buf(),
            source: e,
        })?;
        let id = self.register_image_with_source(img, Some(canonical.clone()), color_space);
        self.path_dedup.insert((canonical, color_space), id);
        Ok(id)
    }

    /// Register a [`DynamicImage`] already loaded by the caller,
    /// assuming an sRGB color space. The pipeline can use this when
    /// textures live in `project.sqlite` blobs rather than on the host
    /// filesystem. For data textures, use
    /// [`Self::register_image_with_color_space`].
    pub fn register_image(&mut self, img: DynamicImage) -> TextureId {
        self.register_image_with_source(img, None, ColorSpace::Srgb)
    }

    /// Register a [`DynamicImage`] already loaded by the caller, with
    /// an explicit color space. Use [`ColorSpace::Linear`] for data
    /// textures (normal map, metallic-roughness, AO, displacement)
    /// and [`ColorSpace::Srgb`] for colour textures (albedo, emissive).
    pub fn register_image_with_color_space(
        &mut self,
        img: DynamicImage,
        color_space: ColorSpace,
    ) -> TextureId {
        self.register_image_with_source(img, None, color_space)
    }

    /// Register a raw linear `f32 × 4` buffer (e.g. an HDR sample, a
    /// procedurally generated test pattern, or a Radiance / OpenEXR
    /// image already converted to linear pixels). The MIP chain is
    /// generated by box-filter downsample.
    pub fn register_linear_rgba_f32(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<[f32; 4]>,
    ) -> TextureId {
        debug_assert_eq!(pixels.len(), (width as usize) * (height as usize));
        let base = MipLevel {
            width,
            height,
            pixels,
        };
        let levels = build_mip_chain(base);
        let id = TextureId(self.entries.len() as u32);
        self.entries.push(AtlasEntry {
            source: None,
            levels,
        });
        id
    }

    fn register_image_with_source(
        &mut self,
        img: DynamicImage,
        source: Option<PathBuf>,
        color_space: ColorSpace,
    ) -> TextureId {
        let (w, h) = img.dimensions();
        let is_hdr = matches!(
            img,
            DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
        );
        let base_pixels = decode_to_linear_rgba(&img, is_hdr, color_space);
        let base = MipLevel {
            width: w,
            height: h,
            pixels: base_pixels,
        };
        let levels = build_mip_chain(base);
        let id = TextureId(self.entries.len() as u32);
        self.entries.push(AtlasEntry { source, levels });
        id
    }

    /// Build a [`TextureAtlas`] from every texture map referenced by
    /// the supplied [`PbrMaterial`]s. Returns the atlas and a parallel
    /// vector of [`MaterialTextureBindings`] — one per input material,
    /// in the same order — so the path-tracer scene builder can wire
    /// textures into [`super::material::PathTraceMaterial`] without
    /// re-resolving paths.
    ///
    /// `blob_resolver` is a callback that maps a `TextureRef.blob_hash`
    /// to an absolute on-disk path. Most call sites use the project
    /// package's blob store (`BlobStore::path_for_hash`). Returning
    /// `None` from the resolver — or hitting a decode error — logs the
    /// failure (via `tracing`) and leaves the corresponding binding
    /// `None` so the renderer falls back to the flat colour.
    pub fn from_material_library(
        materials: &[aec_materials::PbrMaterial],
        mut blob_resolver: impl FnMut(&str) -> Option<PathBuf>,
    ) -> (Self, Vec<MaterialTextureBindings>) {
        let mut atlas = TextureAtlas::new();
        // Per-slot color space follows the glTF spec:
        //   - albedo / base-colour  -> sRGB    (colour data)
        //   - normal map            -> Linear  (encoded XYZ data; sRGB
        //                                       would tilt the
        //                                       decoded normal)
        //   - metallic-roughness    -> Linear  (encoded scalar data;
        //                                       sRGB would shift
        //                                       roughness/metallic
        //                                       values toward black)
        //   - emissive              -> sRGB    (colour data)
        // The atlas's path_dedup cache handles the case where the
        // same on-disk file is referenced by multiple materials —
        // a second material with the same blob_hash points at the
        // already-registered TextureId rather than triggering a
        // re-decode.
        let mut bindings = Vec::with_capacity(materials.len());
        for mat in materials {
            let mut b = MaterialTextureBindings::default();
            let slots: [(
                Option<&aec_materials::TextureRef>,
                &mut Option<TextureId>,
                ColorSpace,
            ); 4] = [
                (mat.albedo_map.as_ref(), &mut b.albedo, ColorSpace::Srgb),
                (mat.normal_map.as_ref(), &mut b.normal, ColorSpace::Linear),
                (
                    mat.metallic_roughness_map.as_ref(),
                    &mut b.metallic_roughness,
                    ColorSpace::Linear,
                ),
                (mat.emissive_map.as_ref(), &mut b.emissive, ColorSpace::Srgb),
            ];
            for (slot, dst, color_space) in slots {
                if let Some(tex_ref) = slot {
                    if let Some(p) = blob_resolver(&tex_ref.blob_hash) {
                        match atlas.load_path_with_color_space(&p, color_space) {
                            Ok(id) => *dst = Some(id),
                            Err(err) => {
                                tracing::warn!(
                                    material = %mat.id,
                                    blob_hash = %tex_ref.blob_hash,
                                    error = %err,
                                    "skipping texture (decode failed)"
                                );
                            }
                        }
                    } else {
                        tracing::warn!(
                            material = %mat.id,
                            blob_hash = %tex_ref.blob_hash,
                            "skipping texture (blob not found in store)"
                        );
                    }
                }
            }
            bindings.push(b);
        }
        (atlas, bindings)
    }
}

/// Per-material binding into [`TextureAtlas`]. Built alongside the
/// atlas by [`TextureAtlas::from_material_library`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaterialTextureBindings {
    pub albedo: Option<TextureId>,
    pub normal: Option<TextureId>,
    /// glTF-standard packed map: green = roughness, blue = metallic.
    pub metallic_roughness: Option<TextureId>,
    pub emissive: Option<TextureId>,
}

/// Bilinear sample at `uv` (with GL `REPEAT` wrap). `lod` selects the
/// MIP level via floor + ceil with linear interpolation between them
/// (trilinear filtering). `lod = 0.0` reads the base mip directly.
pub fn bilinear_sample(atlas: &TextureAtlas, id: TextureId, uv: [f32; 2], lod: f32) -> Vec4 {
    let Some(entry) = atlas.entry(id) else {
        return Vec4::new(1.0, 0.0, 1.0, 1.0); // magenta = "missing" sentinel
    };
    if entry.levels.is_empty() {
        return Vec4::ZERO;
    }
    let max_lod = (entry.levels.len() - 1) as f32;
    let lod = lod.clamp(0.0, max_lod);
    let l0 = lod.floor() as usize;
    let l1 = (l0 + 1).min(entry.levels.len() - 1);
    let t = lod - l0 as f32;
    let s0 = sample_level(&entry.levels[l0], uv);
    if l0 == l1 {
        return s0;
    }
    let s1 = sample_level(&entry.levels[l1], uv);
    s0.lerp(s1, t)
}

/// Bilinear sample at `uv` of one specific MIP level. Wraps with
/// `REPEAT`, identical to GL's `GL_REPEAT`.
fn sample_level(level: &MipLevel, uv: [f32; 2]) -> Vec4 {
    // GL convention: (0,0) is bottom-left, but our path tracer & glTF
    // textures store (0,0) as top-left. We flip V here so the resulting
    // sample matches the rendering convention used by `Mesh.uvs`.
    let u = uv[0] - uv[0].floor();
    let v = 1.0 - (uv[1] - uv[1].floor());
    let fx = u * level.width as f32 - 0.5;
    let fy = v * level.height as f32 - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let x0i = (x0 as i64).rem_euclid(level.width.max(1) as i64) as u32;
    let y0i = (y0 as i64).rem_euclid(level.height.max(1) as i64) as u32;
    let x1i = (x0i + 1) % level.width.max(1);
    let y1i = (y0i + 1) % level.height.max(1);
    let p00 = Vec4::from_array(level.pixel(x0i, y0i));
    let p10 = Vec4::from_array(level.pixel(x1i, y0i));
    let p01 = Vec4::from_array(level.pixel(x0i, y1i));
    let p11 = Vec4::from_array(level.pixel(x1i, y1i));
    let p0 = p00.lerp(p10, tx);
    let p1 = p01.lerp(p11, tx);
    p0.lerp(p1, ty)
}

/// Convenience: sample the RGB triple at `uv` (mip 0).
pub fn sample_rgb(atlas: &TextureAtlas, id: TextureId, uv: [f32; 2]) -> Vec3 {
    let s = bilinear_sample(atlas, id, uv, 0.0);
    Vec3::new(s.x, s.y, s.z)
}

/// Convert an arbitrary [`DynamicImage`] to linear-space RGBA `f32`.
///
/// * 8-bit / 16-bit inputs are decoded according to `color_space`:
///   [`ColorSpace::Srgb`] applies the standard sRGB → linear transfer
///   (the convention for **colour** textures — albedo, emissive),
///   while [`ColorSpace::Linear`] divides by 255 with no gamma (the
///   convention for **data** textures — normal map,
///   metallic-roughness, AO, displacement, per the glTF spec). The
///   alpha channel is always treated as linear coverage regardless
///   of `color_space`.
/// * 32-bit float inputs (Radiance HDR / OpenEXR) are always treated
///   as already-linear regardless of `color_space`; no transfer is
///   applied. `.hdr`/`.exr` are by definition linear-light formats.
fn decode_to_linear_rgba(
    img: &DynamicImage,
    hdr_is_linear: bool,
    color_space: ColorSpace,
) -> Vec<[f32; 4]> {
    if hdr_is_linear {
        let rgba32 = img.to_rgba32f();
        return rgba32
            .pixels()
            .map(|p| [p.0[0], p.0[1], p.0[2], p.0[3]])
            .collect();
    }
    let rgba8 = img.to_rgba8();
    match color_space {
        ColorSpace::Srgb => rgba8
            .pixels()
            .map(|p| {
                let r = srgb_to_linear(p.0[0] as f32 / 255.0);
                let g = srgb_to_linear(p.0[1] as f32 / 255.0);
                let b = srgb_to_linear(p.0[2] as f32 / 255.0);
                let a = p.0[3] as f32 / 255.0;
                [r, g, b, a]
            })
            .collect(),
        ColorSpace::Linear => rgba8
            .pixels()
            .map(|p| {
                [
                    p.0[0] as f32 / 255.0,
                    p.0[1] as f32 / 255.0,
                    p.0[2] as f32 / 255.0,
                    p.0[3] as f32 / 255.0,
                ]
            })
            .collect(),
    }
}

/// Standard sRGB → linear transfer function. Matches the formula used
/// by the wgpu pipeline (`wgpu::TextureFormat::Rgba8UnormSrgb` reads).
#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Build a MIP chain by box-filter downsample. Each subsequent level
/// halves both dimensions, terminating at 1×1.
pub fn build_mip_chain(base: MipLevel) -> Vec<MipLevel> {
    let mut chain = vec![base];
    loop {
        let prev = chain.last().expect("base level present");
        if prev.width <= 1 && prev.height <= 1 {
            break;
        }
        let next = box_downsample(prev);
        chain.push(next);
    }
    chain
}

fn box_downsample(prev: &MipLevel) -> MipLevel {
    let nw = (prev.width / 2).max(1);
    let nh = (prev.height / 2).max(1);
    let mut pixels = Vec::with_capacity((nw * nh) as usize);
    for y in 0..nh {
        for x in 0..nw {
            let x0 = (2 * x).min(prev.width - 1);
            let y0 = (2 * y).min(prev.height - 1);
            let x1 = (x0 + 1).min(prev.width - 1);
            let y1 = (y0 + 1).min(prev.height - 1);
            let mut acc = [0.0f32; 4];
            for &(xi, yi) in &[(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let p = prev.pixel(xi, yi);
                for (a, &v) in acc.iter_mut().zip(p.iter()) {
                    *a += v;
                }
            }
            for a in &mut acc {
                *a *= 0.25;
            }
            pixels.push(acc);
        }
    }
    MipLevel {
        width: nw,
        height: nh,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn checkerboard_4x4_image() -> DynamicImage {
        // 4x4 checkerboard where (0,0) and alternating cells are pure
        // white (1.0 linear after sRGB-linearisation), the others are
        // pure black. The image is sRGB-encoded 8-bit RGBA.
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(4, 4);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let on = (x + y) % 2 == 0;
            *p = if on {
                Rgba([255, 255, 255, 255])
            } else {
                Rgba([0, 0, 0, 255])
            };
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn registers_checkerboard_and_builds_mip_chain() {
        let mut atlas = TextureAtlas::new();
        let id = atlas.register_image(checkerboard_4x4_image());
        let entry = atlas.entry(id).expect("entry");
        // 4x4 → 2x2 → 1x1
        assert_eq!(entry.mip_count(), 3);
        assert_eq!(entry.base_width(), 4);
        assert_eq!(entry.base_height(), 4);
    }

    #[test]
    fn bilinear_at_pixel_centers_returns_pixel_value() {
        let mut atlas = TextureAtlas::new();
        let id = atlas.register_image(checkerboard_4x4_image());
        // Pixel (0, 0) of the 4x4 image is white (linear 1.0); the
        // sampler's V-flip places this at UV (0.125, 0.875).
        let s = bilinear_sample(&atlas, id, [0.125, 0.875], 0.0);
        assert!((s.x - 1.0).abs() < 1e-3);
        assert!((s.y - 1.0).abs() < 1e-3);
        assert!((s.z - 1.0).abs() < 1e-3);
    }

    #[test]
    fn bilinear_between_pixels_is_average() {
        let mut atlas = TextureAtlas::new();
        let id = atlas.register_image(checkerboard_4x4_image());
        // The midpoint of pixels (0,0)=white and (1,0)=black is UV
        // (0.25, 0.875) → after sRGB-linearisation should be 0.5.
        let s = bilinear_sample(&atlas, id, [0.25, 0.875], 0.0);
        assert!(
            (s.x - 0.5).abs() < 0.05,
            "bilinear midpoint expected ~0.5, got {}",
            s.x
        );
    }

    #[test]
    fn mip_level_clamps_to_max() {
        let mut atlas = TextureAtlas::new();
        let id = atlas.register_image(checkerboard_4x4_image());
        // LOD beyond the last mip should not panic and should resolve
        // to the smallest level (1x1).
        let s = bilinear_sample(&atlas, id, [0.5, 0.5], 99.0);
        // 1x1 mip of a 50/50 checkerboard averages to ~0.5.
        assert!(
            (s.x - 0.5).abs() < 0.05,
            "1x1 mip expected ~0.5, got {}",
            s.x
        );
    }

    #[test]
    fn srgb_to_linear_endpoints() {
        assert!(srgb_to_linear(0.0) < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn repeat_wrap_does_not_panic_on_out_of_range_uv() {
        let mut atlas = TextureAtlas::new();
        let id = atlas.register_image(checkerboard_4x4_image());
        // Negative / >1 UVs must wrap, not panic / return NaN.
        let s = bilinear_sample(&atlas, id, [-3.7, 2.3], 0.0);
        assert!(s.x.is_finite());
        assert!(s.y.is_finite());
        assert!(s.z.is_finite());
    }

    #[test]
    fn from_material_library_collects_referenced_blobs() {
        use aec_materials::material::{PbrMaterial, TextureRef};
        // Synthesise a 1x1 white PNG, write it to a tempdir, and
        // reference it from a material's `albedo_map`.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("white.png");
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(1, 1);
        *img.get_pixel_mut(0, 0) = Rgba([255, 255, 255, 255]);
        DynamicImage::ImageRgba8(img).save(&path).unwrap();
        let mat = PbrMaterial {
            albedo_map: Some(TextureRef {
                blob_hash: "white".into(),
                channel: "albedo".into(),
                width: 1,
                height: 1,
            }),
            ..PbrMaterial::new("mat:white", "White")
        };
        let path_clone = path.clone();
        let (atlas, bindings) =
            TextureAtlas::from_material_library(std::slice::from_ref(&mat), |hash| {
                if hash == "white" {
                    Some(path_clone.clone())
                } else {
                    None
                }
            });
        assert_eq!(atlas.len(), 1);
        assert_eq!(bindings.len(), 1);
        let tid = bindings[0].albedo.expect("albedo bound");
        let s = bilinear_sample(&atlas, tid, [0.5, 0.5], 0.0);
        assert!((s.x - 1.0).abs() < 1e-3);
    }

    #[test]
    fn missing_blob_is_skipped_without_error() {
        use aec_materials::material::{PbrMaterial, TextureRef};
        let mat = PbrMaterial {
            albedo_map: Some(TextureRef {
                blob_hash: "missing".into(),
                channel: "albedo".into(),
                width: 1,
                height: 1,
            }),
            ..PbrMaterial::new("mat:missing", "Missing")
        };
        let (atlas, bindings) =
            TextureAtlas::from_material_library(std::slice::from_ref(&mat), |_| None);
        assert!(atlas.is_empty());
        assert!(bindings[0].albedo.is_none());
    }

    /// Devin Review Phase 17 Group A pass 4: 8-bit data textures
    /// (normal / metallic-roughness / AO) must NOT have the sRGB
    /// transfer applied at decode time. A mid-grey 128/255 normal-map
    /// texel must decode to ~0.502 linear (which a downstream
    /// `2x - 1` map sends to a flat ~0.0 tangent-space normal), not
    /// to ~0.214 (which the sRGB path produces and which a downstream
    /// mapper would send to -0.572 — a strongly bent fake normal).
    #[test]
    fn linear_color_space_skips_srgb_transfer_for_data_textures() {
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(1, 1);
        *img.get_pixel_mut(0, 0) = Rgba([128, 128, 255, 255]);
        let dyn_img = DynamicImage::ImageRgba8(img);

        let mut atlas = TextureAtlas::new();
        let lin_id = atlas.register_image_with_color_space(dyn_img.clone(), ColorSpace::Linear);
        let srgb_id = atlas.register_image_with_color_space(dyn_img, ColorSpace::Srgb);

        let s_lin = bilinear_sample(&atlas, lin_id, [0.5, 0.5], 0.0);
        let s_srgb = bilinear_sample(&atlas, srgb_id, [0.5, 0.5], 0.0);

        // Linear path: 128/255 = 0.50196, no transfer.
        assert!(
            (s_lin.x - 128.0 / 255.0).abs() < 1e-4,
            "ColorSpace::Linear must decode 128/255 verbatim, got {}",
            s_lin.x
        );
        // sRGB path: 128/255 = 0.502 sRGB → ~0.2158 linear.
        assert!(
            (s_srgb.x - 0.215_86).abs() < 1e-3,
            "ColorSpace::Srgb must apply the sRGB transfer (~0.2158 for 128/255), got {}",
            s_srgb.x
        );
        // The two decoders must produce materially different results
        // — a regression where both paths went through sRGB would
        // collapse this difference.
        assert!(
            (s_lin.x - s_srgb.x).abs() > 0.25,
            "Linear vs sRGB decode must differ by >0.25 for 128/255; got delta {}",
            (s_lin.x - s_srgb.x).abs()
        );
    }

    /// `from_material_library` must use ColorSpace::Linear for the
    /// normal-map and metallic-roughness slots per the glTF spec.
    /// This test threads a single grey PNG through both slots and
    /// verifies the *Linear* decode result.
    #[test]
    fn material_library_normal_and_mr_decode_as_linear() {
        use aec_materials::material::{PbrMaterial, TextureRef};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("grey.png");
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(1, 1);
        *img.get_pixel_mut(0, 0) = Rgba([128, 128, 128, 255]);
        DynamicImage::ImageRgba8(img).save(&path).unwrap();

        let mat = PbrMaterial {
            normal_map: Some(TextureRef {
                blob_hash: "grey-n".into(),
                channel: "normal".into(),
                width: 1,
                height: 1,
            }),
            metallic_roughness_map: Some(TextureRef {
                blob_hash: "grey-mr".into(),
                channel: "metallic_roughness".into(),
                width: 1,
                height: 1,
            }),
            ..PbrMaterial::new("mat:grey", "Grey")
        };
        let path_clone = path.clone();
        let (atlas, bindings) =
            TextureAtlas::from_material_library(std::slice::from_ref(&mat), |hash| match hash {
                "grey-n" | "grey-mr" => Some(path_clone.clone()),
                _ => None,
            });

        let n_id = bindings[0].normal.expect("normal bound");
        let mr_id = bindings[0]
            .metallic_roughness
            .expect("metallic-roughness bound");
        let s_n = bilinear_sample(&atlas, n_id, [0.5, 0.5], 0.0);
        let s_mr = bilinear_sample(&atlas, mr_id, [0.5, 0.5], 0.0);

        let expected = 128.0_f32 / 255.0;
        assert!(
            (s_n.x - expected).abs() < 1e-3,
            "normal-map slot must decode as linear (~{expected}), got {}",
            s_n.x
        );
        assert!(
            (s_mr.y - expected).abs() < 1e-3,
            "MR roughness channel must decode as linear (~{expected}), got {}",
            s_mr.y
        );
    }

    /// Devin Review Phase 17 Group A pass 4: when many materials
    /// reference the same blob_hash, the atlas must decode the on-disk
    /// file exactly once. Without the dedup cache a 100-material
    /// project sharing a single 2K albedo would decode it 100 times
    /// at scene-build, which on a real workload is hundreds of MB of
    /// redundant pixel data and several seconds of decode wall-time.
    #[test]
    fn material_library_dedups_identical_blob_hashes() {
        use aec_materials::material::{PbrMaterial, TextureRef};
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shared.png");
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(2, 2);
        for (_, _, p) in img.enumerate_pixels_mut() {
            *p = Rgba([200, 100, 50, 255]);
        }
        DynamicImage::ImageRgba8(img).save(&path).unwrap();

        let shared_ref = TextureRef {
            blob_hash: "shared".into(),
            channel: "albedo".into(),
            width: 2,
            height: 2,
        };
        let mut mats = Vec::new();
        for i in 0..5 {
            mats.push(PbrMaterial {
                albedo_map: Some(shared_ref.clone()),
                ..PbrMaterial::new(format!("mat:{i}"), format!("Material {i}"))
            });
        }

        let mut decode_calls = 0_u32;
        let path_clone = path.clone();
        let (atlas, bindings) = TextureAtlas::from_material_library(&mats, |hash| {
            if hash == "shared" {
                decode_calls += 1;
                Some(path_clone.clone())
            } else {
                None
            }
        });

        // The resolver is called once per slot per material (the
        // dedup happens inside `load_path_with_color_space`), but the
        // atlas must hold exactly ONE entry — proving the underlying
        // decode + MIP-chain construction ran exactly once.
        assert_eq!(
            atlas.len(),
            1,
            "expected 1 atlas entry, got {}",
            atlas.len()
        );
        // Every material's albedo binding must point at the same
        // TextureId.
        let first_id = bindings[0].albedo.expect("albedo bound");
        for b in &bindings[1..] {
            assert_eq!(b.albedo, Some(first_id), "all materials share the texture");
        }
    }

    /// Different color spaces against the same path must NOT collide
    /// in the dedup cache — the two decodes produce different pixel
    /// values, so they must be stored as distinct atlas entries.
    #[test]
    fn dedup_cache_keys_on_color_space_too() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.png");
        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(1, 1);
        *img.get_pixel_mut(0, 0) = Rgba([128, 128, 128, 255]);
        DynamicImage::ImageRgba8(img).save(&path).unwrap();

        let mut atlas = TextureAtlas::new();
        let lin_id = atlas
            .load_path_with_color_space(&path, ColorSpace::Linear)
            .expect("decode");
        let srgb_id = atlas
            .load_path_with_color_space(&path, ColorSpace::Srgb)
            .expect("decode");
        let lin_id_2 = atlas
            .load_path_with_color_space(&path, ColorSpace::Linear)
            .expect("decode");

        assert_eq!(atlas.len(), 2, "Linear and Srgb must be separate entries");
        assert_ne!(lin_id, srgb_id);
        // Second Linear call must reuse the existing decode.
        assert_eq!(lin_id, lin_id_2);
    }
}
