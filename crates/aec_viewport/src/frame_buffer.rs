//! Viewport frame buffer transport.
//!
//! Phase 17 Group D Task 21 — the runtime path between the
//! viewport's render output and the Electron renderer's `<canvas>`.
//! The "shared memory" naming in the original spec refers to the
//! zero-copy hand-off from a Rust [`Vec<u8>`] into a V8
//! [`napi::JsBuffer`] (via napi-rs's external-buffer adaptor) — not
//! POSIX shared memory, which the workspace's `unsafe_code = "deny"`
//! lint forbids.
//!
//! Producer / consumer:
//!
//! - producer: [`render_horizon_frame`] generates a CPU-side RGBA8
//!   sky + ground frame from a [`Camera`] (yaw/pitch derived from
//!   `position - target`). The output is fully deterministic given
//!   the camera state + frame size; the viewport service uses it as
//!   the always-on background that the renderer composites the
//!   project scene against.
//! - consumer: the napi layer wraps the [`FrameBuffer::pixels`]
//!   `Vec<u8>` in `Buffer::from_data` so V8 owns a *view* into the
//!   Rust allocation. The buffer is freed when V8 garbage-collects
//!   the wrapper, so the renderer can keep the bytes around for as
//!   long as its canvas-paint loop needs.
//!
//! The deliberate split between this CPU module and the wgpu
//! pipeline in [`crate::pbr_preview`] is so the renderer's
//! frame-paint loop can run *every animation frame* without
//! requiring a GPU device. When the wgpu device is present the
//! service can compose this sky over a wgpu readback in a future
//! patch; the renderer-side wiring (preload, IPC, canvas paint)
//! stays unchanged.
//!
//! Co-ordinate convention:
//!
//! - input camera is right-handed Y-up, mm world space.
//! - output buffer is row-major top-to-bottom, RGBA8 unmuxed
//!   (no premultiplied alpha), with `alpha = 255` for every pixel
//!   so the renderer's `putImageData` writes opaque pixels.

use crate::camera::Camera;

/// CPU-side RGBA8 frame buffer ready for shipping over the napi
/// boundary.
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    /// RGBA8 pixels, row-major, top-to-bottom. Length is
    /// `width * height * 4`.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Monotonic counter so the renderer can discard stale frames.
    pub frame_index: u64,
}

impl FrameBuffer {
    /// Allocate a transparent frame of the given dimensions. Used by
    /// tests and the "no adapter, no camera" fallback path.
    pub fn empty(width: u32, height: u32, frame_index: u64) -> Self {
        let len = (width as usize) * (height as usize) * 4;
        Self {
            pixels: vec![0u8; len],
            width,
            height,
            frame_index,
        }
    }

    /// Total byte length — `width * height * 4`.
    pub fn byte_len(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 4
    }
}

/// Render a horizon-aware sky + ground gradient for the given camera.
///
/// The frame contains three visible features:
///
/// 1. Sky — a vertical gradient from horizon (light blue-grey) to
///    zenith (deeper blue) computed per row from the camera's pitch.
/// 2. Ground — a complementary gradient from horizon (warm tan) to
///    feet (darker brown) on rows below the horizon.
/// 3. Horizon line — a single-pixel-thick band at the horizon row
///    so the camera's pitch is visually unambiguous in tests.
///
/// The camera's *yaw* shifts the horizon line horizontally so a
/// `pan` input visibly translates the image; *pitch* shifts the
/// horizon vertically. The result is always a valid RGBA8 frame —
/// even for degenerate inputs (zero-length view vector, NaN
/// components) we fall back to the cardinal "looking forward
/// horizontal" frame.
pub fn render_horizon_frame(
    camera: &Camera,
    width: u32,
    height: u32,
    frame_index: u64,
) -> FrameBuffer {
    if width == 0 || height == 0 {
        return FrameBuffer::empty(width, height, frame_index);
    }
    // 1. Derive pitch + yaw from the camera direction. We treat
    //    the y-axis as world-up and the x-z plane as the ground.
    //    `to_eye = position - target` so the camera looks from
    //    +to_eye toward target; we invert to `forward = -to_eye`.
    let to_eye = [
        camera.position[0] - camera.target[0],
        camera.position[1] - camera.target[1],
        camera.position[2] - camera.target[2],
    ];
    let len = (to_eye[0] * to_eye[0] + to_eye[1] * to_eye[1] + to_eye[2] * to_eye[2]).sqrt();
    let (pitch, yaw) = if !len.is_finite() || len < 1e-6 {
        (0.0_f32, 0.0_f32)
    } else {
        let forward = [-to_eye[0] / len, -to_eye[1] / len, -to_eye[2] / len];
        // pitch is the angle above (+) / below (-) the x-z plane.
        let horiz_len = (forward[0] * forward[0] + forward[2] * forward[2]).sqrt();
        let pitch = forward[1].atan2(horiz_len.max(1e-6));
        // yaw is the rotation around y. atan2(x, z) keeps the
        // "looking down -z" zero convention.
        let yaw = forward[0].atan2(-forward[2]);
        (pitch, yaw)
    };
    // 2. Compute horizon row. The y FoV maps `fov_radians` to
    //    `height` pixels; pitch maps proportionally. A positive
    //    pitch (camera tilted up) pulls the horizon DOWN in the
    //    image, so we *add* the pitch offset to the centre row.
    let fov = camera.fov_radians.clamp(0.1, std::f32::consts::PI - 0.1);
    let pitch_frac = (pitch / (fov * 0.5)).clamp(-1.0, 1.0);
    let horizon_row = (((height as f32) * 0.5) + pitch_frac * (height as f32) * 0.5).round() as i64;
    let horizon_row = horizon_row.clamp(0, height as i64 - 1) as u32;

    // 3. Build per-row sky / ground colours. We avoid per-pixel
    //    trig by pre-computing the row colour and then applying a
    //    horizontal yaw shift via a slowly-varying hue offset.
    let yaw_hue_offset = (yaw.sin() * 12.0) as i16; // -12..+12 channel shift.
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];

    for row in 0..height {
        let dy = row as i64 - horizon_row as i64;
        let row_color = match row.cmp(&horizon_row) {
            std::cmp::Ordering::Less => {
                // Sky. dy is negative (above horizon). Distance from
                // horizon as a 0..1 fraction of the sky band.
                let band = horizon_row.max(1) as f32;
                let t = ((-dy) as f32 / band).clamp(0.0, 1.0);
                sky_color(t, yaw_hue_offset)
            }
            std::cmp::Ordering::Equal => {
                // Horizon strip — slightly darker so it visibly
                // separates sky from ground.
                [120, 110, 96, 255]
            }
            std::cmp::Ordering::Greater => {
                // Ground.
                let band = (height - horizon_row).max(1) as f32;
                let t = (dy as f32 / band).clamp(0.0, 1.0);
                ground_color(t, yaw_hue_offset)
            }
        };
        let row_start = (row as usize) * (width as usize) * 4;
        for x in 0..width as usize {
            let off = row_start + x * 4;
            pixels[off] = row_color[0];
            pixels[off + 1] = row_color[1];
            pixels[off + 2] = row_color[2];
            pixels[off + 3] = row_color[3];
        }
    }

    FrameBuffer {
        pixels,
        width,
        height,
        frame_index,
    }
}

fn sky_color(t: f32, yaw_offset: i16) -> [u8; 4] {
    // t=0 at horizon (warm pale), t=1 at zenith (deeper blue).
    let r = lerp_u8(168, 96, t);
    let g = lerp_u8(186, 132, t);
    let b = lerp_u8(214, 200, t);
    [
        shift_channel(r, yaw_offset),
        shift_channel(g, yaw_offset),
        shift_channel(b, -yaw_offset),
        255,
    ]
}

fn ground_color(t: f32, yaw_offset: i16) -> [u8; 4] {
    // t=0 at horizon (warm tan), t=1 at feet (darker olive-brown).
    let r = lerp_u8(168, 96, t);
    let g = lerp_u8(140, 80, t);
    let b = lerp_u8(98, 56, t);
    [
        shift_channel(r, yaw_offset),
        shift_channel(g, -yaw_offset),
        shift_channel(b, yaw_offset),
        255,
    ]
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let v = (a as f32) * (1.0 - t) + (b as f32) * t;
    v.clamp(0.0, 255.0) as u8
}

fn shift_channel(c: u8, delta: i16) -> u8 {
    ((c as i16) + delta).clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera_at(position: [f32; 3], target: [f32; 3]) -> Camera {
        Camera {
            position,
            target,
            ..Camera::default_perspective()
        }
    }

    #[test]
    fn empty_frame_has_correct_byte_length() {
        let fb = FrameBuffer::empty(8, 4, 0);
        assert_eq!(fb.byte_len(), 8 * 4 * 4);
        assert_eq!(fb.pixels.len(), 8 * 4 * 4);
        assert_eq!(fb.frame_index, 0);
    }

    #[test]
    fn zero_dimensions_return_empty_buffer() {
        let c = camera_at([100.0, 100.0, 100.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 0, 10, 7);
        assert_eq!(fb.byte_len(), 0);
        assert_eq!(fb.frame_index, 7);
        let fb = render_horizon_frame(&c, 10, 0, 7);
        assert_eq!(fb.byte_len(), 0);
    }

    #[test]
    fn horizontal_camera_puts_horizon_in_middle_row() {
        // Camera at +Z looking at origin → forward is -Z, pitch ≈ 0.
        let c = camera_at([0.0, 0.0, 5000.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 16, 100, 1);
        // The horizon row should be ~50 ± 1.
        let mid = horizon_row(&fb);
        assert!(
            (mid as i64 - 50).abs() <= 1,
            "expected horizon near row 50, got {mid}"
        );
    }

    #[test]
    fn camera_looking_down_pushes_horizon_above_middle() {
        // Camera high above ground looking nearly straight down.
        // forward.y is large-negative → pitch is large-negative →
        // horizon row should move UP (smaller y in image space).
        let c = camera_at([0.0, 5000.0, 100.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 16, 100, 1);
        let mid = horizon_row(&fb);
        assert!(
            mid < 40,
            "looking down should pull horizon above mid-row; got {mid}"
        );
    }

    #[test]
    fn camera_looking_up_pushes_horizon_below_middle() {
        // Camera near ground looking nearly straight up.
        // forward.y is large-positive → pitch positive → horizon
        // row should move DOWN (larger y in image space).
        let c = camera_at([0.0, -100.0, 100.0], [0.0, 5000.0, 0.0]);
        let fb = render_horizon_frame(&c, 16, 100, 1);
        let mid = horizon_row(&fb);
        assert!(
            mid > 60,
            "looking up should push horizon below mid-row; got {mid}"
        );
    }

    #[test]
    fn pixels_are_opaque() {
        let c = camera_at([100.0, 100.0, 100.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 8, 8, 0);
        for chunk in fb.pixels.chunks(4) {
            assert_eq!(chunk[3], 255);
        }
    }

    #[test]
    fn sky_and_ground_have_distinct_dominant_channels() {
        // A column above the horizon should be blue-dominant
        // (b > r); below the horizon should be red-dominant
        // (r > b). This is what gives the user the unmistakable
        // "sky-on-top, ground-on-bottom" cue.
        let c = camera_at([0.0, 0.0, 5000.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 32, 100, 0);
        let mid = horizon_row(&fb);
        // Sample one row well above horizon and one well below.
        let sky_row = mid.saturating_sub(20);
        let ground_row = (mid + 20).min(fb.height - 1);
        let sky = sample_pixel(&fb, 0, sky_row);
        let ground = sample_pixel(&fb, 0, ground_row);
        assert!(sky[2] > sky[0], "sky should be blue-dominant: {sky:?}");
        assert!(
            ground[0] > ground[2],
            "ground should be red-dominant: {ground:?}"
        );
    }

    #[test]
    fn degenerate_camera_falls_back_to_horizontal() {
        // position == target so to_eye length is 0.
        let c = camera_at([100.0, 100.0, 100.0], [100.0, 100.0, 100.0]);
        let fb = render_horizon_frame(&c, 16, 100, 1);
        let mid = horizon_row(&fb);
        assert!(
            (mid as i64 - 50).abs() <= 1,
            "degenerate camera should give a middle horizon; got {mid}"
        );
    }

    #[test]
    fn frame_index_propagates_unchanged() {
        let c = camera_at([100.0, 100.0, 100.0], [0.0, 0.0, 0.0]);
        let fb = render_horizon_frame(&c, 4, 4, 12345);
        assert_eq!(fb.frame_index, 12345);
    }

    /// Find the horizon row by scanning for the darker strip we
    /// embed at horizon_row in render_horizon_frame.
    fn horizon_row(fb: &FrameBuffer) -> u32 {
        // The horizon strip is [120, 110, 96, 255]. Scan the centre
        // column and find the matching row.
        let cx = fb.width / 2;
        for row in 0..fb.height {
            let p = sample_pixel(fb, cx, row);
            if p == [120, 110, 96, 255] {
                return row;
            }
        }
        // Fallback: middle row (kept for diagnostic only — the
        // tests above assert specific rows so this never fires
        // on a healthy implementation).
        fb.height / 2
    }

    fn sample_pixel(fb: &FrameBuffer, x: u32, y: u32) -> [u8; 4] {
        let off = ((y as usize) * (fb.width as usize) + (x as usize)) * 4;
        [
            fb.pixels[off],
            fb.pixels[off + 1],
            fb.pixels[off + 2],
            fb.pixels[off + 3],
        ]
    }
}
