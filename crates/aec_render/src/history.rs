//! Persistent render history.
//!
//! The history is the durable record of every completed render in a
//! project. Each entry stores the render job id, the preset id, a clone
//! of the camera snapshot at render time, the wall-clock timestamps, the
//! on-disk output path, an optional thumbnail blob hash, and the
//! BLAKE3 hash of the rendered image bytes when known. Entries are
//! append-only — failed/cancelled renders are not recorded.
//!
//! The store is backed by the project's `renders/history.json` file.
//! Loading is best-effort: if the file is missing the store starts
//! empty; if it's corrupt the store returns `HistoryError::Corrupt`
//! so the caller can decide whether to fall back to an empty store.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cameras::CameraSnapshot;
use crate::job::{RenderJob, RenderJobStatus};
use crate::preset::migrate_legacy_preset_id;

/// Serde adapter that rewrites a legacy `cycles_*` preset id to its
/// canonical short form when deserialising a [`RenderHistoryEntry`].
/// Without this, history entries written before the 2026-05 preset
/// rename would silently surface as "cycles_standard" etc. and the
/// before/after compare UI would diff "cycles_standard → standard"
/// even though nothing actually changed.
fn deserialize_migrated_preset_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(migrate_legacy_preset_id(&raw).to_string())
}

const HISTORY_FILE: &str = "history.json";

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("history file is corrupt: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("job `{0}` is not completed; only completed jobs may be recorded")]
    NotCompleted(String),
    #[error("unknown entry `{0}`")]
    UnknownEntry(String),
}

/// One entry in the render history. Records everything needed to
/// re-display the render later (preset, camera) and to compare it to
/// another render (timestamps, output path, hashes).
///
/// `Eq` is intentionally not implemented because [`CameraSnapshot`]
/// holds `f32` fields (focal length, aperture, etc.) for which
/// reflexive equality is not guaranteed (`NaN != NaN`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderHistoryEntry {
    pub job_id: String,
    /// Preset id, migrated from the legacy `cycles_*` form at
    /// deserialize time so old `history.json` files load cleanly.
    #[serde(deserialize_with = "deserialize_migrated_preset_id")]
    pub preset_id: String,
    pub camera: Option<CameraSnapshot>,
    pub created_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub output_path: PathBuf,
    /// BLAKE3 hex of the rendered image file, when computed.
    pub image_hash: Option<String>,
    /// BLAKE3 hex of a thumbnail blob, when one was generated.
    pub thumbnail_hash: Option<String>,
    /// Optional batch grouping id, copied from the source job.
    pub batch_id: Option<String>,
}

impl RenderHistoryEntry {
    /// Build an entry from a completed render job. Returns
    /// `HistoryError::NotCompleted` if the job is in any other terminal
    /// state — we don't record failures or cancellations because
    /// recording them would bloat the timeline and they have no output
    /// to display.
    pub fn from_job(job: &RenderJob) -> Result<Self, HistoryError> {
        if job.status != RenderJobStatus::Completed {
            return Err(HistoryError::NotCompleted(job.id.clone()));
        }
        let output_path = job
            .output_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("renders/{}.png", job.id)));
        let completed_at = job.completed_at.unwrap_or_else(Utc::now);
        Ok(Self {
            job_id: job.id.clone(),
            preset_id: job.preset.id.clone(),
            camera: None,
            created_at: job.created_at,
            completed_at,
            output_path,
            image_hash: None,
            thumbnail_hash: None,
            batch_id: job.batch_id.clone(),
        })
    }

    pub fn with_camera(mut self, camera: CameraSnapshot) -> Self {
        self.camera = Some(camera);
        self
    }

    pub fn with_image_hash(mut self, hash: impl Into<String>) -> Self {
        self.image_hash = Some(hash.into());
        self
    }

    pub fn with_thumbnail_hash(mut self, hash: impl Into<String>) -> Self {
        self.thumbnail_hash = Some(hash.into());
        self
    }

    /// Render wall-clock duration. Floors at zero for clock skew safety.
    pub fn render_duration(&self) -> Duration {
        let nanos = (self.completed_at - self.created_at).num_nanoseconds();
        match nanos {
            Some(n) if n > 0 => Duration::from_nanos(n as u64),
            _ => Duration::from_nanos(0),
        }
    }
}

/// Append-only history with on-disk persistence.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderHistory {
    pub entries: Vec<RenderHistoryEntry>,
}

impl RenderHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load history from a project root. The actual file is
    /// `<project_root>/renders/history.json`.
    pub fn load(project_root: &Path) -> Result<Self, HistoryError> {
        let path = Self::path_for(project_root);
        if !path.exists() {
            return Ok(Self::new());
        }
        let bytes = fs::read(&path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| HistoryError::Corrupt(e.to_string()))
    }

    /// Persist the history to `<project_root>/renders/history.json`.
    /// Creates the parent directory if missing.
    pub fn save(&self, project_root: &Path) -> Result<(), HistoryError> {
        let path = Self::path_for(project_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let pretty = serde_json::to_vec_pretty(self)
            .map_err(|e| HistoryError::Corrupt(e.to_string()))?;
        fs::write(path, pretty)?;
        Ok(())
    }

    fn path_for(project_root: &Path) -> PathBuf {
        project_root.join("renders").join(HISTORY_FILE)
    }

    /// Append a fully-formed history entry.
    pub fn record(&mut self, entry: RenderHistoryEntry) {
        self.entries.push(entry);
    }

    /// Convenience: build from job + camera + image hash and append in one step.
    pub fn record_job(
        &mut self,
        job: &RenderJob,
        camera: Option<CameraSnapshot>,
        image_hash: Option<String>,
    ) -> Result<&RenderHistoryEntry, HistoryError> {
        let mut entry = RenderHistoryEntry::from_job(job)?;
        if let Some(c) = camera {
            entry = entry.with_camera(c);
        }
        if let Some(h) = image_hash {
            entry = entry.with_image_hash(h);
        }
        self.entries.push(entry);
        // Safe: we just pushed.
        Ok(self.entries.last().unwrap())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, job_id: &str) -> Option<&RenderHistoryEntry> {
        self.entries.iter().find(|e| e.job_id == job_id)
    }

    /// Return entries sorted by completion time, newest first.
    pub fn by_recency(&self) -> Vec<&RenderHistoryEntry> {
        let mut out: Vec<&RenderHistoryEntry> = self.entries.iter().collect();
        out.sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
        out
    }
}

/// Result of comparing two history entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareResult {
    pub a_job_id: String,
    pub b_job_id: String,
    pub preset_changed: bool,
    pub camera_changed: bool,
    pub image_hash_changed: bool,
    /// Difference in render duration: positive means b took longer.
    pub duration_delta_ms: i64,
    /// Per-field human-readable diffs.
    pub field_diffs: Vec<FieldDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDiff {
    pub field: String,
    pub from: String,
    pub to: String,
}

/// Compare two completed renders by their history entries.
pub fn compare(
    a: &RenderHistoryEntry,
    b: &RenderHistoryEntry,
) -> CompareResult {
    let mut diffs = Vec::new();
    if a.preset_id != b.preset_id {
        diffs.push(FieldDiff {
            field: "preset".to_string(),
            from: a.preset_id.clone(),
            to: b.preset_id.clone(),
        });
    }
    let camera_changed = match (&a.camera, &b.camera) {
        (Some(ca), Some(cb)) => ca != cb,
        (None, None) => false,
        _ => true,
    };
    if camera_changed {
        diffs.push(FieldDiff {
            field: "camera".to_string(),
            from: a
                .camera
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "(none)".to_string()),
            to: b
                .camera
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "(none)".to_string()),
        });
    }
    let image_hash_changed = a.image_hash != b.image_hash;
    if image_hash_changed {
        diffs.push(FieldDiff {
            field: "image_hash".to_string(),
            from: a.image_hash.clone().unwrap_or_else(|| "(none)".into()),
            to: b.image_hash.clone().unwrap_or_else(|| "(none)".into()),
        });
    }
    let duration_delta_ms = (b.render_duration().as_millis() as i64)
        - (a.render_duration().as_millis() as i64);
    CompareResult {
        a_job_id: a.job_id.clone(),
        b_job_id: b.job_id.clone(),
        preset_changed: a.preset_id != b.preset_id,
        camera_changed,
        image_hash_changed,
        duration_delta_ms,
        field_diffs: diffs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::RenderPreset;
    use crate::scene::RenderScene;
    use chrono::TimeZone;

    fn completed_job(preset_id: &str, image_hash: Option<&str>) -> RenderJob {
        let mut preset = RenderPreset::standard();
        preset.id = preset_id.to_string();
        let mut j = RenderJob::new(preset, RenderScene::new());
        j.status = RenderJobStatus::Completed;
        j.progress = 1.0;
        // Deterministic timestamps so duration calculations are exact.
        j.created_at = Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap();
        j.completed_at = Some(Utc.with_ymd_and_hms(2026, 5, 19, 0, 5, 0).unwrap());
        j.output_path = Some(PathBuf::from(format!("renders/{}.png", j.id)));
        if let Some(h) = image_hash {
            // The history entry stores image_hash separately; we use it below.
            let _ = h;
        }
        j
    }

    #[test]
    fn from_job_rejects_non_completed() {
        let mut j = completed_job("standard", None);
        j.status = RenderJobStatus::Failed;
        let err = RenderHistoryEntry::from_job(&j).unwrap_err();
        assert!(matches!(err, HistoryError::NotCompleted(_)));
    }

    #[test]
    fn from_job_copies_metadata() {
        let job = completed_job("standard", None);
        let entry = RenderHistoryEntry::from_job(&job).unwrap();
        assert_eq!(entry.job_id, job.id);
        assert_eq!(entry.preset_id, "standard");
        assert_eq!(entry.render_duration(), Duration::from_secs(300));
    }

    #[test]
    fn history_records_and_retrieves() {
        let mut h = RenderHistory::new();
        let job = completed_job("high", None);
        h.record_job(&job, None, Some("abc123".into())).unwrap();
        assert_eq!(h.len(), 1);
        let e = h.get(&job.id).unwrap();
        assert_eq!(e.image_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn history_by_recency_sorts_newest_first() {
        let mut h = RenderHistory::new();
        let mut a = completed_job("quick", None);
        let mut b = completed_job("high", None);
        a.completed_at = Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap());
        b.completed_at = Some(Utc.with_ymd_and_hms(2026, 5, 19, 0, 0, 0).unwrap());
        h.record_job(&a, None, None).unwrap();
        h.record_job(&b, None, None).unwrap();
        let recent = h.by_recency();
        assert_eq!(recent[0].job_id, b.id);
        assert_eq!(recent[1].job_id, a.id);
    }

    #[test]
    fn compare_detects_preset_and_image_changes() {
        let mut h = RenderHistory::new();
        let a = completed_job("quick", None);
        let b = completed_job("high", None);
        h.record_job(&a, None, Some("aaa".into())).unwrap();
        h.record_job(&b, None, Some("bbb".into())).unwrap();
        let cmp = compare(&h.entries[0], &h.entries[1]);
        assert!(cmp.preset_changed);
        assert!(cmp.image_hash_changed);
        assert!(!cmp.camera_changed);
        assert_eq!(cmp.field_diffs.len(), 2);
    }

    #[test]
    fn compare_duration_delta_signed() {
        let mut a = completed_job("quick", None);
        let mut b = completed_job("quick", None);
        a.completed_at = Some(a.created_at + chrono::Duration::seconds(60));
        b.completed_at = Some(b.created_at + chrono::Duration::seconds(180));
        let ea = RenderHistoryEntry::from_job(&a).unwrap();
        let eb = RenderHistoryEntry::from_job(&b).unwrap();
        let cmp = compare(&ea, &eb);
        assert_eq!(cmp.duration_delta_ms, 120_000);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let job = completed_job("standard", None);
        let mut h = RenderHistory::new();
        h.record_job(&job, None, Some("xx".into())).unwrap();
        h.save(tmp.path()).unwrap();

        let loaded = RenderHistory::load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.entries[0].job_id, job.id);
        assert_eq!(loaded.entries[0].image_hash.as_deref(), Some("xx"));
    }

    #[test]
    fn load_missing_file_yields_empty_history() {
        let tmp = tempfile::tempdir().unwrap();
        let h = RenderHistory::load(tmp.path()).unwrap();
        assert!(h.is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("renders");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(HISTORY_FILE), b"{ not json").unwrap();
        let err = RenderHistory::load(tmp.path()).unwrap_err();
        assert!(matches!(err, HistoryError::Corrupt(_)));
    }
}
