//! Saved cameras: persistent camera entities with full DoF / exposure /
//! white-balance state, plus a thumbnail generator the UI uses for the
//! camera browser tile.
//!
//! `RenderCamera` (in `scene.rs`) is the minimal projection-only camera the
//! Blender worker consumes. `CameraSnapshot` here adds everything an artist
//! tweaks (focal length, sensor, exposure, DoF, white balance, aspect ratio,
//! optional preset key) and serialises out via serde so projects can store
//! it on disk. CRUD goes through the command engine so undo/redo applies.

use std::collections::BTreeMap;

use aec_core::types::EntityId;
use serde::{Deserialize, Serialize};

use crate::scene::RenderCamera;

/// One step in the camera CRUD journal. Persisted alongside the store so
/// the higher-level [`aec_command`] undo/redo journal can roundtrip these
/// edits the same way it does for model commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CameraJournalEntry {
    Upsert {
        snapshot: CameraSnapshot,
        previous: Option<CameraSnapshot>,
    },
    Remove {
        snapshot: CameraSnapshot,
    },
}

/// Append-only journal of camera CRUD operations. Used by the engine
/// integration in `aec_bridge` to fold these into the global undo/redo
/// journal.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CameraJournal {
    entries: Vec<CameraJournalEntry>,
}

impl CameraJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: CameraJournalEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[CameraJournalEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One saved camera. Aspect ratio + sensor size are stored explicitly so
/// the renderer can compute the matching focal length / FOV without
/// recomputing from the focal-length-in-mm field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraSnapshot {
    pub id: EntityId,
    pub name: String,
    pub position_mm: [f32; 3],
    pub target_mm: [f32; 3],
    pub up_mm: [f32; 3],
    pub focal_length_mm: f32,
    pub sensor_width_mm: f32,
    pub sensor_height_mm: f32,
    pub exposure_ev: f32,
    pub white_balance_k: f32,
    pub aperture_f: f32,
    pub focus_distance_mm: f32,
    pub aspect_ratio: f32,
    /// Optional template-camera key (e.g. `"interior_close_up"`,
    /// `"wide_angle"`, `"eye_level"`, `"birds_eye"`). When set the
    /// renderer can use the preset's default lighting alongside this
    /// camera's transform.
    pub preset_key: Option<String>,
}

impl CameraSnapshot {
    /// Sanity-check a camera's parameters and clamp the egregious cases.
    /// The renderer cannot recover from a zero-focal-length or
    /// zero-aperture camera, so we refuse to construct one.
    pub fn validate(&self) -> Result<(), CameraValidationError> {
        if !self.focal_length_mm.is_finite() || self.focal_length_mm <= 0.0 {
            return Err(CameraValidationError::FocalLength);
        }
        if !self.aperture_f.is_finite() || self.aperture_f <= 0.0 {
            return Err(CameraValidationError::Aperture);
        }
        if !self.sensor_width_mm.is_finite() || self.sensor_width_mm <= 0.0 {
            return Err(CameraValidationError::Sensor);
        }
        if !self.sensor_height_mm.is_finite() || self.sensor_height_mm <= 0.0 {
            return Err(CameraValidationError::Sensor);
        }
        if !self.aspect_ratio.is_finite() || self.aspect_ratio <= 0.0 {
            return Err(CameraValidationError::Aspect);
        }
        if !self.focus_distance_mm.is_finite() || self.focus_distance_mm < 0.0 {
            return Err(CameraValidationError::FocusDistance);
        }
        if same_point(self.position_mm, self.target_mm) {
            return Err(CameraValidationError::ZeroLengthForward);
        }
        Ok(())
    }

    /// Project the snapshot down to the minimal `RenderCamera` the
    /// Blender worker consumes. This is what gets stamped onto each
    /// queued render so the camera roundtrips even after subsequent
    /// edits to the snapshot.
    pub fn to_render_camera(&self) -> RenderCamera {
        RenderCamera {
            id: self.id.to_string(),
            position_mm: self.position_mm,
            target_mm: self.target_mm,
            focal_length_mm: self.focal_length_mm,
            exposure_ev: self.exposure_ev,
            white_balance_k: self.white_balance_k,
            aperture_f: self.aperture_f,
        }
    }

    /// Field of view (horizontal, radians). The horizontal FOV is what the
    /// renderer needs for projection-matrix setup.
    pub fn horizontal_fov_rad(&self) -> f32 {
        // FOV = 2 * atan(sensor / (2 * focal_length))
        2.0 * (self.sensor_width_mm / (2.0 * self.focal_length_mm)).atan()
    }
}

fn same_point(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() < f32::EPSILON
        && (a[1] - b[1]).abs() < f32::EPSILON
        && (a[2] - b[2]).abs() < f32::EPSILON
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraValidationError {
    FocalLength,
    Aperture,
    Sensor,
    Aspect,
    FocusDistance,
    /// Position and target coincide; the renderer cannot derive a
    /// forward direction.
    ZeroLengthForward,
}

impl std::fmt::Display for CameraValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CameraValidationError::FocalLength => {
                f.write_str("focal_length_mm must be positive and finite")
            }
            CameraValidationError::Aperture => {
                f.write_str("aperture_f must be positive and finite")
            }
            CameraValidationError::Sensor => {
                f.write_str("sensor dimensions must be positive and finite")
            }
            CameraValidationError::Aspect => {
                f.write_str("aspect_ratio must be positive and finite")
            }
            CameraValidationError::FocusDistance => {
                f.write_str("focus_distance_mm must be non-negative and finite")
            }
            CameraValidationError::ZeroLengthForward => {
                f.write_str("position_mm and target_mm coincide")
            }
        }
    }
}

impl std::error::Error for CameraValidationError {}

/// Catalogue of common interior / exterior cameras shipped with the
/// product. They define focal length, sensor, DoF baseline — callers
/// fill in position + target themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraPresetKind {
    InteriorCloseUp,
    WideAngle,
    EyeLevel,
    BirdsEye,
}

impl CameraPresetKind {
    pub fn key(self) -> &'static str {
        match self {
            CameraPresetKind::InteriorCloseUp => "interior_close_up",
            CameraPresetKind::WideAngle => "wide_angle",
            CameraPresetKind::EyeLevel => "eye_level",
            CameraPresetKind::BirdsEye => "birds_eye",
        }
    }

    /// Default focal length in mm for the preset, picked so the resulting
    /// FOV matches what an architect would compose for that shot.
    pub fn focal_length_mm(self) -> f32 {
        match self {
            CameraPresetKind::InteriorCloseUp => 35.0,
            CameraPresetKind::WideAngle => 18.0,
            CameraPresetKind::EyeLevel => 50.0,
            CameraPresetKind::BirdsEye => 24.0,
        }
    }

    /// Aperture f-number. Wider lenses run at smaller f-numbers; bird's
    /// eye runs deep to keep the whole site sharp.
    pub fn aperture_f(self) -> f32 {
        match self {
            CameraPresetKind::InteriorCloseUp => 4.0,
            CameraPresetKind::WideAngle => 5.6,
            CameraPresetKind::EyeLevel => 4.0,
            CameraPresetKind::BirdsEye => 11.0,
        }
    }
}

/// Persisted store of saved cameras. CRUD ops go through the command
/// engine so undo / redo applies to camera edits the same way it
/// applies to model edits.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CameraStore {
    by_id: BTreeMap<EntityId, CameraSnapshot>,
}

impl CameraStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn get(&self, id: &EntityId) -> Option<&CameraSnapshot> {
        self.by_id.get(id)
    }

    /// Return every saved camera sorted by name (stable iteration order
    /// for the UI).
    pub fn list(&self) -> Vec<&CameraSnapshot> {
        let mut out: Vec<&CameraSnapshot> = self.by_id.values().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Insert / replace through the camera journal. Returns the previous
    /// snapshot under the same id, if any, so callers can build an undo
    /// step.
    pub fn upsert(
        &mut self,
        journal: &mut CameraJournal,
        snapshot: CameraSnapshot,
    ) -> Result<Option<CameraSnapshot>, CameraValidationError> {
        snapshot.validate()?;
        let previous = self.by_id.insert(snapshot.id.clone(), snapshot.clone());
        journal.push(CameraJournalEntry::Upsert {
            snapshot,
            previous: previous.clone(),
        });
        Ok(previous)
    }

    /// Remove the snapshot under `id`, journaling the removal so it can
    /// be undone.
    pub fn remove(&mut self, journal: &mut CameraJournal, id: &EntityId) -> Option<CameraSnapshot> {
        let removed = self.by_id.remove(id);
        if let Some(ref snap) = removed {
            journal.push(CameraJournalEntry::Remove {
                snapshot: snap.clone(),
            });
        }
        removed
    }

    /// Apply a preset's lens/aperture defaults on top of a camera that
    /// already has position / target wired up. Returns the new snapshot
    /// after applying — caller still needs to upsert it.
    pub fn apply_preset(snapshot: &CameraSnapshot, preset: CameraPresetKind) -> CameraSnapshot {
        CameraSnapshot {
            focal_length_mm: preset.focal_length_mm(),
            aperture_f: preset.aperture_f(),
            preset_key: Some(preset.key().to_string()),
            ..snapshot.clone()
        }
    }
}

/// Tiny deterministic thumbnail (32x32 RGBA8). Each pixel's colour is a
/// function of the camera transform + lens so two snapshots with the
/// same parameters always render the same thumbnail — critical for
/// stable UI testing without a GPU.
///
/// The gradient is not photorealistic; it's a visual fingerprint. The
/// production renderer ships a real preview render of the scene from
/// the camera, but that requires the Blender worker to be live. The
/// fingerprint is used when the worker is offline / cold and when
/// rendering thumbnails in batch.
pub fn render_thumbnail_rgba8(snapshot: &CameraSnapshot) -> Vec<u8> {
    const SIZE: usize = 32;
    let mut out = vec![0u8; SIZE * SIZE * 4];
    // Project the camera's normalised forward vector onto colour space.
    let dx = snapshot.target_mm[0] - snapshot.position_mm[0];
    let dy = snapshot.target_mm[1] - snapshot.position_mm[1];
    let dz = snapshot.target_mm[2] - snapshot.position_mm[2];
    let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
    let nx = dx / len;
    let ny = dy / len;
    let nz = dz / len;
    // Hue from focal length, brightness from exposure.
    let hue = (snapshot.focal_length_mm / 200.0).clamp(0.0, 1.0);
    let brightness = 0.5 + 0.05 * snapshot.exposure_ev;
    let brightness = brightness.clamp(0.0, 1.0);
    for row in 0..SIZE {
        for col in 0..SIZE {
            let u = (col as f32) / (SIZE as f32 - 1.0);
            let v = (row as f32) / (SIZE as f32 - 1.0);
            // Mix horizontal direction into red, vertical into green.
            let red = ((0.5 + 0.5 * nx) * (1.0 - u) + hue * u) * brightness;
            let green = ((0.5 + 0.5 * ny) * (1.0 - v) + hue * v) * brightness;
            // Blue tracks depth and white-balance.
            let blue = ((0.5 + 0.5 * nz) * brightness
                + (snapshot.white_balance_k / 12000.0).clamp(0.0, 1.0) * 0.4)
                .clamp(0.0, 1.0);
            let idx = (row * SIZE + col) * 4;
            out[idx] = (red.clamp(0.0, 1.0) * 255.0) as u8;
            out[idx + 1] = (green.clamp(0.0, 1.0) * 255.0) as u8;
            out[idx + 2] = (blue * 255.0) as u8;
            out[idx + 3] = 255;
        }
    }
    out
}

/// A single keyframe in a [`CameraPath`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraKeyframe {
    pub frame: u32,
    pub position_mm: [f32; 3],
    pub target_mm: [f32; 3],
}

/// Ordered sequence of camera keyframes used by walkthrough renders.
/// Keyframes are sorted by `frame` on insert; the path also enforces
/// that frame numbers are unique so interpolation has a well-defined
/// bracket for every requested frame.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraPath {
    keyframes: Vec<CameraKeyframe>,
}

impl CameraPath {
    pub fn new() -> Self {
        Self {
            keyframes: Vec::new(),
        }
    }

    pub fn from_keyframes(mut kf: Vec<CameraKeyframe>) -> Result<Self, CameraPathError> {
        kf.sort_by_key(|k| k.frame);
        for win in kf.windows(2) {
            if win[0].frame == win[1].frame {
                return Err(CameraPathError::DuplicateFrame(win[0].frame));
            }
        }
        Ok(Self { keyframes: kf })
    }

    pub fn keyframes(&self) -> &[CameraKeyframe] {
        &self.keyframes
    }

    pub fn len(&self) -> usize {
        self.keyframes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }

    pub fn frame_range(&self) -> Option<(u32, u32)> {
        if self.keyframes.is_empty() {
            return None;
        }
        Some((
            self.keyframes.first().unwrap().frame,
            self.keyframes.last().unwrap().frame,
        ))
    }

    /// Linear interpolation at `frame` — clamps to the first/last
    /// keyframe outside the defined range.
    pub fn sample(&self, frame: u32) -> Option<CameraKeyframe> {
        if self.keyframes.is_empty() {
            return None;
        }
        if frame <= self.keyframes.first().unwrap().frame {
            return Some(self.keyframes.first().unwrap().clone());
        }
        if frame >= self.keyframes.last().unwrap().frame {
            return Some(self.keyframes.last().unwrap().clone());
        }
        for win in self.keyframes.windows(2) {
            let a = &win[0];
            let b = &win[1];
            if a.frame <= frame && frame <= b.frame {
                let span = (b.frame - a.frame) as f32;
                if span == 0.0 {
                    return Some(a.clone());
                }
                let t = (frame - a.frame) as f32 / span;
                let lerp = |x: f32, y: f32| x + (y - x) * t;
                return Some(CameraKeyframe {
                    frame,
                    position_mm: [
                        lerp(a.position_mm[0], b.position_mm[0]),
                        lerp(a.position_mm[1], b.position_mm[1]),
                        lerp(a.position_mm[2], b.position_mm[2]),
                    ],
                    target_mm: [
                        lerp(a.target_mm[0], b.target_mm[0]),
                        lerp(a.target_mm[1], b.target_mm[1]),
                        lerp(a.target_mm[2], b.target_mm[2]),
                    ],
                });
            }
        }
        None
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CameraPathError {
    #[error("duplicate keyframe at frame {0}")]
    DuplicateFrame(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_camera(id: &str) -> CameraSnapshot {
        let full = if id.starts_with("ent_") {
            id.to_string()
        } else {
            format!("ent_{id}")
        };
        CameraSnapshot {
            id: EntityId::from_string(full).expect("entity id"),
            name: "Cam A".into(),
            position_mm: [5000.0, 3000.0, 1600.0],
            target_mm: [0.0, 0.0, 1500.0],
            up_mm: [0.0, 0.0, 1.0],
            focal_length_mm: 35.0,
            sensor_width_mm: 36.0,
            sensor_height_mm: 24.0,
            exposure_ev: 0.5,
            white_balance_k: 5500.0,
            aperture_f: 5.6,
            focus_distance_mm: 4500.0,
            aspect_ratio: 16.0 / 9.0,
            preset_key: None,
        }
    }

    #[test]
    fn validate_accepts_realistic_camera() {
        let cam = sample_camera("cam_001");
        cam.validate().unwrap();
    }

    #[test]
    fn validate_rejects_zero_focal_length() {
        let mut cam = sample_camera("cam_002");
        cam.focal_length_mm = 0.0;
        assert!(matches!(
            cam.validate().unwrap_err(),
            CameraValidationError::FocalLength
        ));
    }

    #[test]
    fn validate_rejects_position_equals_target() {
        let mut cam = sample_camera("cam_003");
        cam.target_mm = cam.position_mm;
        assert!(matches!(
            cam.validate().unwrap_err(),
            CameraValidationError::ZeroLengthForward
        ));
    }

    #[test]
    fn upsert_then_remove_through_journal() {
        let mut journal = CameraJournal::new();
        let mut store = CameraStore::new();
        let cam = sample_camera("cam_004");
        let previous = store.upsert(&mut journal, cam.clone()).unwrap();
        assert!(previous.is_none());
        assert_eq!(store.len(), 1);
        // Update returns the previous snapshot.
        let mut updated = cam.clone();
        updated.focal_length_mm = 50.0;
        let prev = store
            .upsert(&mut journal, updated.clone())
            .unwrap()
            .unwrap();
        assert_eq!(prev.focal_length_mm, 35.0);
        // Remove returns the latest snapshot, then nothing.
        let removed = store.remove(&mut journal, &cam.id).unwrap();
        assert_eq!(removed.focal_length_mm, 50.0);
        assert!(store.remove(&mut journal, &cam.id).is_none());
        // Journal captured three real operations (upsert, upsert, remove).
        assert_eq!(journal.len(), 3);
        assert!(matches!(
            journal.entries()[0],
            CameraJournalEntry::Upsert { previous: None, .. }
        ));
        assert!(matches!(
            &journal.entries()[1],
            CameraJournalEntry::Upsert {
                previous: Some(_),
                ..
            }
        ));
        assert!(matches!(
            journal.entries()[2],
            CameraJournalEntry::Remove { .. }
        ));
    }

    #[test]
    fn list_is_sorted_by_name() {
        let mut journal = CameraJournal::new();
        let mut store = CameraStore::new();
        let mut a = sample_camera("cam_a");
        a.name = "Zoom".into();
        let mut b = sample_camera("cam_b");
        b.name = "Approach".into();
        store.upsert(&mut journal, a).unwrap();
        store.upsert(&mut journal, b).unwrap();
        let names: Vec<_> = store.list().iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["Approach".to_string(), "Zoom".to_string()]);
    }

    #[test]
    fn apply_preset_sets_focal_length_aperture_and_key() {
        let snap = sample_camera("cam_p");
        let interior = CameraStore::apply_preset(&snap, CameraPresetKind::InteriorCloseUp);
        assert_eq!(interior.focal_length_mm, 35.0);
        assert_eq!(interior.aperture_f, 4.0);
        assert_eq!(interior.preset_key, Some("interior_close_up".to_string()));
        let bird = CameraStore::apply_preset(&snap, CameraPresetKind::BirdsEye);
        assert_eq!(bird.focal_length_mm, 24.0);
        assert_eq!(bird.aperture_f, 11.0);
    }

    #[test]
    fn to_render_camera_strips_to_worker_payload() {
        let cam = sample_camera("cam_x");
        let rc = cam.to_render_camera();
        assert_eq!(rc.id, "ent_cam_x");
        assert_eq!(rc.focal_length_mm, 35.0);
        assert_eq!(rc.exposure_ev, 0.5);
        assert_eq!(rc.aperture_f, 5.6);
    }

    #[test]
    fn horizontal_fov_matches_lens_geometry() {
        let mut cam = sample_camera("cam_fov");
        // 36mm sensor at 50mm focal length → ~39.6° horizontal FOV.
        cam.focal_length_mm = 50.0;
        let fov = cam.horizontal_fov_rad().to_degrees();
        assert!((fov - 39.6).abs() < 0.5, "fov={fov}");
    }

    #[test]
    fn thumbnail_is_deterministic_for_same_camera() {
        let cam = sample_camera("cam_thumb");
        let a = render_thumbnail_rgba8(&cam);
        let b = render_thumbnail_rgba8(&cam);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32 * 32 * 4);
    }

    #[test]
    fn thumbnail_differs_when_camera_moves() {
        let cam1 = sample_camera("cam_a");
        let mut cam2 = sample_camera("cam_a");
        cam2.position_mm = [10000.0, 0.0, 1600.0];
        let a = render_thumbnail_rgba8(&cam1);
        let b = render_thumbnail_rgba8(&cam2);
        assert_ne!(a, b);
    }

    #[test]
    fn camera_snapshot_roundtrips_serde() {
        let cam = sample_camera("cam_serde");
        let json = serde_json::to_string(&cam).unwrap();
        let back: CameraSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cam);
    }

    fn kf(frame: u32, x: f32, tx: f32) -> CameraKeyframe {
        CameraKeyframe {
            frame,
            position_mm: [x, 0.0, 1500.0],
            target_mm: [tx, 0.0, 1500.0],
        }
    }

    #[test]
    fn camera_path_sorts_by_frame_on_construction() {
        let path =
            CameraPath::from_keyframes(vec![kf(10, 100.0, 0.0), kf(1, 0.0, 0.0), kf(5, 50.0, 0.0)])
                .unwrap();
        let frames: Vec<u32> = path.keyframes().iter().map(|k| k.frame).collect();
        assert_eq!(frames, vec![1, 5, 10]);
    }

    #[test]
    fn camera_path_rejects_duplicate_frames() {
        let err = CameraPath::from_keyframes(vec![kf(5, 0.0, 0.0), kf(5, 1.0, 0.0)]).unwrap_err();
        assert_eq!(err, CameraPathError::DuplicateFrame(5));
    }

    #[test]
    fn camera_path_interpolates_linearly() {
        let path = CameraPath::from_keyframes(vec![kf(0, 0.0, 0.0), kf(10, 100.0, 200.0)]).unwrap();
        let mid = path.sample(5).unwrap();
        assert!((mid.position_mm[0] - 50.0).abs() < 1e-3);
        assert!((mid.target_mm[0] - 100.0).abs() < 1e-3);
    }

    #[test]
    fn camera_path_clamps_outside_range() {
        let path = CameraPath::from_keyframes(vec![kf(5, 0.0, 0.0), kf(10, 100.0, 0.0)]).unwrap();
        let pre = path.sample(1).unwrap();
        let post = path.sample(50).unwrap();
        assert_eq!(pre.position_mm[0], 0.0);
        assert_eq!(post.position_mm[0], 100.0);
    }

    #[test]
    fn camera_path_frame_range_is_min_max() {
        let path = CameraPath::from_keyframes(vec![kf(3, 0.0, 0.0), kf(9, 0.0, 0.0)]).unwrap();
        assert_eq!(path.frame_range(), Some((3, 9)));
    }
}
