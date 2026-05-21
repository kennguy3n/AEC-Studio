//! Native walkthrough render pipeline.
//!
//! Renders a camera path frame-by-frame using the native CPU/GPU path
//! tracer, writes each frame as `frame_{:05}.png`, and optionally
//! stitches the sequence into an MP4 via `ffmpeg`. Replaces the legacy
//! `workers/blender/walkthrough.py` worker that this PR4 deletes.
//!
//! ## Resume support
//!
//! [`WalkthroughPipeline::render`] takes a `resume_from` argument — when
//! the caller has previously rendered some frames (tracked on
//! [`RenderJob::completed_frames`]), pass the next frame to render and
//! the pipeline will skip earlier frames. This matches the existing
//! frame-tracking contract documented on `RenderJob`.
//!
//! ## ffmpeg stitching
//!
//! Stitching is delegated to a system `ffmpeg` binary. When the binary
//! is not on `$PATH`, the pipeline returns
//! [`WalkthroughOutput::ImageSequence`] pointing at the output directory
//! — callers (UI / docs page) can display this as "frames available;
//! stitch externally". This mirrors the documented behaviour of the
//! legacy worker.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aec_materials::MaterialLibrary;
use image::{ImageBuffer, Rgb};
use serde::{Deserialize, Serialize};

use crate::cameras::CameraPath;
use crate::final_render::{build_path_trace_scene, path_trace_config_from_preset, scene_sky};
use crate::gpu_trace::render_or_fallback;
use crate::path_trace::{CameraProjection, CancelToken};
use crate::preset::RenderPreset;
use crate::scene::{RenderCamera, RenderScene};

/// Output of a [`WalkthroughPipeline::render`] / `stitch` call.
///
/// Mirrors the legacy `aec_render::WalkthroughOutput` shape so consumers
/// further up the stack (queue, bridge, UI) don't need to be rewritten
/// when the Blender worker disappears. `Video` is produced when `ffmpeg`
/// is available; `ImageSequence` when the binary cannot be invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalkthroughOutput {
    /// `ffmpeg` produced an MP4 at this path.
    Video {
        path: PathBuf,
        frame_count: u32,
        /// Frames per second the MP4 was encoded at.
        fps: u32,
    },
    /// `ffmpeg` was unavailable — frames left in `dir`.
    ImageSequence { dir: PathBuf, frame_count: u32 },
}

/// Per-frame progress report consumed by the UI / queue layer.
#[derive(Debug, Clone, Copy)]
pub struct WalkthroughProgress {
    pub frame: u32,
    pub total_frames: u32,
    pub elapsed: Duration,
}

pub type WalkthroughProgressFn =
    std::sync::Arc<dyn Fn(WalkthroughProgress) + Send + Sync + 'static>;

/// Errors produced by [`WalkthroughPipeline`].
#[derive(Debug, thiserror::Error)]
pub enum WalkthroughError {
    #[error("walkthrough has no keyframes; cannot render")]
    EmptyPath,
    #[error("frame_start {start} must be <= frame_end {end}")]
    InvalidRange { start: u32, end: u32 },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image encode error: {0}")]
    Encode(#[from] image::ImageError),
    #[error("walkthrough cancelled at frame {0}")]
    Cancelled(u32),
}

/// Options that control how the walkthrough's frame sequence is stitched
/// into an MP4. Mirrors the (FPS, glob) tuple the legacy worker used.
#[derive(Debug, Clone)]
pub struct WalkthroughStitchOptions {
    /// Frames per second of the resulting MP4. Defaults to 24.
    pub fps: u32,
    /// `printf`-style frame pattern used by `ffmpeg`. Defaults to
    /// `frame_%05d.png` which matches the filename pattern the renderer
    /// writes.
    pub frame_pattern: String,
}

impl Default for WalkthroughStitchOptions {
    fn default() -> Self {
        Self {
            fps: 24,
            frame_pattern: "frame_%05d.png".into(),
        }
    }
}

/// Native walkthrough pipeline.
pub struct WalkthroughPipeline {
    materials: MaterialLibrary,
}

impl Default for WalkthroughPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl WalkthroughPipeline {
    pub fn new() -> Self {
        Self {
            materials: MaterialLibrary::new(),
        }
    }

    pub fn with_materials(materials: MaterialLibrary) -> Self {
        Self { materials }
    }

    /// Render frames `[frame_start, frame_end]` (inclusive) along
    /// `camera_path` and write each as `frame_{NNNNN}.png` into
    /// `output_dir`. When `resume_from` is `Some(n)`, frames strictly
    /// less than `n` are skipped — use this with
    /// [`crate::RenderJob::next_walkthrough_frame`] to resume a job.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        scene: &RenderScene,
        preset: &RenderPreset,
        camera_path: &CameraPath,
        output_dir: impl AsRef<Path>,
        frame_start: u32,
        frame_end: u32,
        resume_from: Option<u32>,
        cancel: Option<CancelToken>,
        progress: Option<WalkthroughProgressFn>,
    ) -> Result<Vec<u32>, WalkthroughError> {
        if camera_path.is_empty() {
            return Err(WalkthroughError::EmptyPath);
        }
        if frame_start > frame_end {
            return Err(WalkthroughError::InvalidRange {
                start: frame_start,
                end: frame_end,
            });
        }
        let output_dir = output_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&output_dir)?;

        // Build the BVH + materials once for the whole walkthrough — the
        // scene geometry doesn't change between frames, only the camera.
        let sky = scene_sky(scene);
        let pt_scene = build_path_trace_scene(scene, &self.materials, sky);
        let config = path_trace_config_from_preset(&preset.config, CameraProjection::Perspective);

        let total_frames = (frame_end - frame_start) + 1;
        let mut written = Vec::with_capacity(total_frames as usize);
        let start = Instant::now();

        // Use the scene's first camera as the template (for focal length,
        // sensor / exposure params) and replace its position+target per
        // frame.
        let template = scene.cameras.first().cloned().unwrap_or(RenderCamera {
            id: "walkthrough".into(),
            position_mm: [0.0; 3],
            target_mm: [0.0, 0.0, -1000.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        });

        for frame in frame_start..=frame_end {
            if let Some(r) = resume_from {
                if frame < r {
                    continue;
                }
            }
            if let Some(c) = cancel.as_ref() {
                if c.is_cancelled() {
                    return Err(WalkthroughError::Cancelled(frame));
                }
            }

            // CameraPath::sample clamps outside its keyframe range, so it
            // always yields a usable keyframe for any frame index.
            let kf = camera_path.sample(frame).expect("path is non-empty");
            let camera = RenderCamera {
                id: format!("{}_frame_{:05}", template.id, frame),
                position_mm: kf.position_mm,
                target_mm: kf.target_mm,
                ..template
            };

            let buffer = render_or_fallback(&pt_scene, &camera, &config, None, cancel.clone());
            if let Some(c) = cancel.as_ref() {
                if c.is_cancelled() {
                    return Err(WalkthroughError::Cancelled(frame));
                }
            }

            let width = buffer.width;
            let height = buffer.height;
            let srgb = buffer.into_srgb8();
            let img = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, srgb)
                .expect("buffer size matches width * height * 3");
            let path = output_dir.join(format!("frame_{frame:05}.png"));
            img.save(&path)?;
            written.push(frame);

            if let Some(cb) = progress.as_ref() {
                cb(WalkthroughProgress {
                    frame,
                    total_frames,
                    elapsed: start.elapsed(),
                });
            }
        }

        Ok(written)
    }

    /// Stitch a directory of `frame_*.png` files into an MP4 via
    /// `ffmpeg`. Returns [`WalkthroughOutput::Video`] when stitching
    /// succeeds, [`WalkthroughOutput::ImageSequence`] when `ffmpeg` is
    /// unavailable or fails.
    pub fn stitch(
        input_dir: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        opts: &WalkthroughStitchOptions,
    ) -> WalkthroughOutput {
        Self::stitch_with_ffmpeg(input_dir, output_path, opts, find_ffmpeg().as_deref())
    }

    /// Same as [`stitch`](Self::stitch) but with an explicit `ffmpeg`
    /// path injection. Pass `None` to skip the subprocess and force the
    /// `ImageSequence` outcome \u2014 used by unit tests and by callers
    /// that have already determined the binary is unavailable.
    pub fn stitch_with_ffmpeg(
        input_dir: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
        opts: &WalkthroughStitchOptions,
        ffmpeg: Option<&Path>,
    ) -> WalkthroughOutput {
        let input_dir = input_dir.as_ref().to_path_buf();
        let output_path = output_path.as_ref().to_path_buf();
        let frame_count = count_frames_matching(&input_dir, &opts.frame_pattern);

        let Some(ffmpeg) = ffmpeg else {
            return WalkthroughOutput::ImageSequence {
                dir: input_dir,
                frame_count,
            };
        };

        let input_glob = input_dir.join(&opts.frame_pattern);
        let result = std::process::Command::new(ffmpeg)
            .args(["-y", "-framerate", &opts.fps.to_string(), "-i"])
            .arg(&input_glob)
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&output_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() && output_path.exists() => WalkthroughOutput::Video {
                path: output_path,
                frame_count,
                fps: opts.fps,
            },
            _ => WalkthroughOutput::ImageSequence {
                dir: input_dir,
                frame_count,
            },
        }
    }
}

/// Count files in `dir` whose name matches the `printf`-style pattern.
/// The walkthrough pattern is always `frame_{:05}.png`-style, so we
/// detect matches by stripping the `%0Nd` portion and checking the
/// suffix.
fn count_frames_matching(dir: &Path, pattern: &str) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    // Extract the literal prefix/suffix around the `%0Nd` token. If the
    // pattern doesn't contain a `%0Nd`, fall back to "starts with
    // 'frame_'" which matches the canonical output.
    let (prefix, suffix) = split_printf_pattern(pattern).unwrap_or(("frame_", ".png"));
    let mut count = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(prefix) && name.ends_with(suffix) {
            // Body between prefix and suffix must be all digits.
            let body = &name[prefix.len()..name.len() - suffix.len()];
            if !body.is_empty() && body.chars().all(|c| c.is_ascii_digit()) {
                count += 1;
            }
        }
    }
    count
}

fn split_printf_pattern(pattern: &str) -> Option<(&str, &str)> {
    let pct = pattern.find('%')?;
    let d = pattern[pct..].find('d')?;
    let prefix = &pattern[..pct];
    let suffix = &pattern[pct + d + 1..];
    Some((prefix, suffix))
}

/// Cross-platform PATH lookup for the `ffmpeg` binary. Returns
/// `Some(absolute_path)` when a candidate executable is found,
/// otherwise `None`. We do not shell out — this is a pure directory
/// probe so it's safe to call inside async runtimes and from test
/// contexts where mutating `$PATH` is forbidden by lint rules.
fn find_ffmpeg() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let sep = if cfg!(windows) { ';' } else { ':' };
    let path = path.to_string_lossy().to_string();
    for dir in path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let base = std::path::Path::new(dir).join("ffmpeg");
        if base.exists() {
            return Some(base);
        }
        if cfg!(windows) {
            for ext in ["exe", "bat", "cmd"] {
                let with = base.with_extension(ext);
                if with.exists() {
                    return Some(with);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::CameraKeyframe;
    use crate::scene::SerializedMesh;

    fn tiny_scene() -> RenderScene {
        let mut scene = RenderScene::new();
        scene.push_mesh(SerializedMesh {
            id: "floor".into(),
            indices: vec![0, 1, 2, 0, 2, 3],
            positions: vec![
                [-1000.0, 0.0, -1000.0],
                [1000.0, 0.0, -1000.0],
                [1000.0, 0.0, 1000.0],
                [-1000.0, 0.0, 1000.0],
            ],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            uvs: vec![[0.0, 0.0]; 4],
            material_id: None,
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        });
        scene.push_camera(RenderCamera {
            id: "cam0".into(),
            position_mm: [0.0, 1500.0, 2500.0],
            target_mm: [0.0, 0.0, 0.0],
            focal_length_mm: 35.0,
            exposure_ev: 0.0,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
        });
        scene.push_light(crate::scene::RenderLight::SunSky {
            azimuth_deg: 135.0,
            elevation_deg: 45.0,
            intensity: 2.0,
            color_temperature_k: 5500.0,
        });
        scene
    }

    fn fast_preset() -> RenderPreset {
        let mut p = RenderPreset::walkthrough();
        p.config.resolution_x = 16;
        p.config.resolution_y = 16;
        p.config.samples = 1;
        p.config.tile_size_px = 16;
        p.config.denoise = false;
        p
    }

    fn simple_path() -> CameraPath {
        CameraPath::from_keyframes(vec![
            CameraKeyframe {
                frame: 0,
                position_mm: [0.0, 1500.0, 2500.0],
                target_mm: [0.0, 0.0, 0.0],
            },
            CameraKeyframe {
                frame: 4,
                position_mm: [1000.0, 1500.0, 2500.0],
                target_mm: [0.0, 0.0, 0.0],
            },
        ])
        .unwrap()
    }

    #[test]
    fn render_writes_all_frames_in_range() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = WalkthroughPipeline::new();
        let written = pipeline
            .render(
                &tiny_scene(),
                &fast_preset(),
                &simple_path(),
                tmp.path(),
                0,
                3,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(written, vec![0, 1, 2, 3]);
        for frame in 0..=3 {
            let path = tmp.path().join(format!("frame_{frame:05}.png"));
            assert!(path.exists(), "missing frame {frame}");
        }
    }

    #[test]
    fn render_resume_skips_completed_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = WalkthroughPipeline::new();
        let written = pipeline
            .render(
                &tiny_scene(),
                &fast_preset(),
                &simple_path(),
                tmp.path(),
                0,
                3,
                Some(2),
                None,
                None,
            )
            .unwrap();
        assert_eq!(written, vec![2, 3]);
        assert!(!tmp.path().join("frame_00000.png").exists());
        assert!(!tmp.path().join("frame_00001.png").exists());
        assert!(tmp.path().join("frame_00002.png").exists());
        assert!(tmp.path().join("frame_00003.png").exists());
    }

    #[test]
    fn render_empty_path_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = WalkthroughPipeline::new();
        let err = pipeline
            .render(
                &tiny_scene(),
                &fast_preset(),
                &CameraPath::new(),
                tmp.path(),
                0,
                3,
                None,
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, WalkthroughError::EmptyPath));
    }

    #[test]
    fn render_invalid_range_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = WalkthroughPipeline::new();
        let err = pipeline
            .render(
                &tiny_scene(),
                &fast_preset(),
                &simple_path(),
                tmp.path(),
                10,
                3,
                None,
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            WalkthroughError::InvalidRange { start: 10, end: 3 }
        ));
    }

    #[test]
    fn render_cancellation_stops_immediately() {
        let tmp = tempfile::tempdir().unwrap();
        let pipeline = WalkthroughPipeline::new();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = pipeline
            .render(
                &tiny_scene(),
                &fast_preset(),
                &simple_path(),
                tmp.path(),
                0,
                3,
                None,
                Some(cancel),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, WalkthroughError::Cancelled(0)));
    }

    #[test]
    fn stitch_returns_image_sequence_when_ffmpeg_unavailable_or_fails() {
        // Determinism rule: never mutate `$PATH` in tests (forbidden by
        // the workspace `unsafe-code` lint). Instead, exercise
        // `stitch_with_ffmpeg` with `ffmpeg = None`, which is exactly
        // what the public `stitch()` does when PATH lookup fails.
        let tmp = tempfile::tempdir().unwrap();
        for f in 0..3 {
            std::fs::write(
                tmp.path().join(format!("frame_{f:05}.png")),
                b"not a real PNG",
            )
            .unwrap();
        }
        let opts = WalkthroughStitchOptions::default();
        let out = WalkthroughPipeline::stitch_with_ffmpeg(
            tmp.path(),
            tmp.path().join("out.mp4"),
            &opts,
            None,
        );
        match out {
            WalkthroughOutput::ImageSequence { dir, frame_count } => {
                assert_eq!(dir, tmp.path().to_path_buf());
                assert_eq!(frame_count, 3);
            }
            WalkthroughOutput::Video { .. } => {
                panic!("expected ImageSequence when ffmpeg=None")
            }
        }
    }

    #[test]
    fn stitch_with_nonexistent_ffmpeg_path_falls_back_to_image_sequence() {
        // Covers the second branch: ffmpeg path provided but execution
        // fails because the binary doesn't exist.
        let tmp = tempfile::tempdir().unwrap();
        for f in 0..2 {
            std::fs::write(
                tmp.path().join(format!("frame_{f:05}.png")),
                b"not a real PNG",
            )
            .unwrap();
        }
        let opts = WalkthroughStitchOptions::default();
        let fake = std::path::Path::new("/nonexistent/ffmpeg/binary/path");
        let out = WalkthroughPipeline::stitch_with_ffmpeg(
            tmp.path(),
            tmp.path().join("out.mp4"),
            &opts,
            Some(fake),
        );
        match out {
            WalkthroughOutput::ImageSequence { dir, frame_count } => {
                assert_eq!(dir, tmp.path().to_path_buf());
                assert_eq!(frame_count, 2);
            }
            WalkthroughOutput::Video { .. } => {
                panic!("expected ImageSequence when ffmpeg binary is missing")
            }
        }
    }

    #[test]
    fn count_frames_matching_counts_only_matching_names() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("frame_00000.png"), b"x").unwrap();
        std::fs::write(tmp.path().join("frame_00001.png"), b"x").unwrap();
        std::fs::write(tmp.path().join("frame_00002.png"), b"x").unwrap();
        std::fs::write(tmp.path().join("note.txt"), b"x").unwrap();
        std::fs::write(tmp.path().join("preview.png"), b"x").unwrap();
        assert_eq!(
            super::count_frames_matching(tmp.path(), "frame_%05d.png"),
            3
        );
    }

    #[test]
    fn split_printf_pattern_extracts_prefix_and_suffix() {
        assert_eq!(
            super::split_printf_pattern("frame_%05d.png"),
            Some(("frame_", ".png"))
        );
        assert_eq!(super::split_printf_pattern("nopattern.png"), None);
    }

    #[test]
    fn walkthrough_output_serde_roundtrips() {
        let video = WalkthroughOutput::Video {
            path: PathBuf::from("/tmp/walk.mp4"),
            frame_count: 120,
            fps: 24,
        };
        let json = serde_json::to_string(&video).unwrap();
        assert!(json.contains("\"kind\":\"video\""));
        assert!(json.contains("\"frame_count\":120"));
        let back: WalkthroughOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, video);

        let seq = WalkthroughOutput::ImageSequence {
            dir: PathBuf::from("/tmp/walk"),
            frame_count: 48,
        };
        let json = serde_json::to_string(&seq).unwrap();
        assert!(json.contains("\"kind\":\"image_sequence\""));
        assert!(json.contains("\"frame_count\":48"));
        let back: WalkthroughOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, seq);
    }
}
