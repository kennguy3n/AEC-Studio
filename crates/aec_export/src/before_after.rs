//! Before / after deliverables generated from two project revision
//! snapshots.
//!
//! This module produces two distinct artifacts that the Deliver-mode
//! UI ships to clients:
//!
//! 1. **Side-by-side render pairs** — a list of
//!    [`BeforeAfterRenderPair`] entries describing one rendered camera
//!    per pair. The pair carries the on-disk image paths for the
//!    "before" and "after" renders, plus preset / duration metadata.
//!    Pairs can be produced manually or via
//!    [`BeforeAfterReport::discover_render_pairs`] which scans a
//!    project's `renders/` directory.
//!
//! 2. **A plan overlay** — an SVG drawing of the project's walls
//!    rendered with four semantic colours:
//!    * **red** for *demolition* (walls present in `base` but not in
//!      `head`),
//!    * **green** for *new* construction (present only in `head`),
//!    * **dark grey** for *unchanged* walls (kept exactly as-is),
//!    * **orange** for *modified* walls (same id on both sides but
//!      with different body bytes — typically a length / position /
//!      thickness change).
//!
//! Plan overlay generation reads both `.snap` SQLCipher snapshots
//! produced by
//! [`aec_core::RevisionStore::create_with_snapshot`], decodes each
//! `wall` entity's `body` JSON (the same shape the
//! `aec_command::commands::wall::CreateWall` struct writes), and
//! lays the resulting segments out in real model-space millimetres.
//! There is no rasterization in the path — the SVG is text and uses
//! `mm` units so the output is print-correct.
//!
//! The PDF builder is unchanged: it caption-lists each render pair
//! plus, if present, embeds the SVG plan overlay summary text.

use std::fs;
use std::path::{Path, PathBuf};

use aec_core::crypto::Key32;
use aec_core::error::AecError;
use aec_core::revision::{Revision, RevisionStore};
use aec_core::version_diff::{compare_revision_snapshots, VersionDiff};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};

#[derive(Debug, Error)]
pub enum BeforeAfterPdfError {
    #[error("pdf builder error: {0}")]
    Pdf(#[from] PdfBuilderError),
    #[error("no comparison pairs supplied")]
    Empty,
}

#[derive(Debug, Error)]
pub enum BeforeAfterReportError {
    #[error("aec_core: {0}")]
    AecCore(#[from] AecError),
    #[error("rusqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("malformed wall body for entity {id}: {detail}")]
    MalformedWall { id: String, detail: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("svg formatting: {0}")]
    Fmt(#[from] std::fmt::Error),
    #[error("pdf builder error: {0}")]
    Pdf(#[from] PdfBuilderError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAfterRenderPair {
    pub label: String,
    pub before_path: PathBuf,
    pub after_path: PathBuf,
    pub before_preset: String,
    pub after_preset: String,
    /// Wall-clock ms each render took. Used for the "Δ time" caption.
    pub before_ms: u64,
    pub after_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAfterPdfOptions {
    pub project_name: String,
    pub pairs: Vec<BeforeAfterRenderPair>,
}

/// One wall segment laid out for the plan overlay, with its diff
/// classification baked in.
///
/// All coordinates are in model-space millimetres with the building
/// convention `+y` = north / "up the page". The renderer
/// ([`render_plan_overlay_svg_impl`]) is responsible for flipping `y`
/// at emit-time so the resulting SVG reads correctly under SVG's own
/// `+y` = down user-coordinate system — it wraps the segment groups
/// in a `<g transform="translate(0, min_y + max_y) scale(1, -1)">`
/// so the segment coordinates themselves stay in plain model-space
/// millimetres (no per-coordinate transformation, no loss of
/// precision).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanOverlaySegment {
    /// Identifier for the wall this segment renders.
    ///
    /// For real diff segments (every `New` / `Unchanged` / `Modified`
    /// segment, and every `Demolition` segment that corresponds to an
    /// actually-deleted wall) this is the wall's raw `entities.id`.
    /// When a wall is classified as `Modified` the overlay *also*
    /// emits a second synthetic `Demolition` segment for the **old
    /// footprint** (so architectural renovation drawings can show the
    /// previous wall position in red beneath the new one); on that
    /// synthetic ghost the `entity_id` is the wall id with
    /// [`PlanOverlaySegment::GHOST_SUFFIX`] (`"::prev"`) appended as
    /// a *display convention* only — the authoritative "is this a
    /// synthetic ghost?" predicate is the typed [`is_ghost`] field,
    /// **not** a string suffix match on `entity_id`. This way a real
    /// wall whose id happens to end in `"::prev"` is never
    /// misclassified.
    ///
    /// [`is_ghost`]: PlanOverlaySegment::is_ghost
    pub entity_id: String,
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
    pub thickness_mm: f64,
    pub level: PlanOverlayLevel,
    /// `true` when this segment is the synthetic *old-footprint*
    /// ghost emitted alongside every `Modified` wall, `false` for
    /// every real diff segment (including real demolitions of
    /// actually-deleted walls).
    ///
    /// This is the authoritative ghost marker; never compare
    /// [`entity_id`](Self::entity_id) against
    /// [`GHOST_SUFFIX`](Self::GHOST_SUFFIX) to decide. Defaults to
    /// `false` when deserialized from legacy JSON that predates the
    /// field so external persisted reports continue to load (their
    /// segments will all be treated as real, which matches the
    /// pre-ghost-emission behaviour).
    #[serde(default)]
    pub is_ghost: bool,
}

impl PlanOverlaySegment {
    /// Suffix appended to [`PlanOverlaySegment::entity_id`] for the
    /// synthetic *old-footprint* `Demolition` segment emitted alongside
    /// every `Modified` wall — see the field doc on `entity_id`. This
    /// is a display convention to give ghost segments a stable unique
    /// SVG id and is **not** the predicate used to decide whether a
    /// segment is a ghost; that is the typed [`is_ghost`] flag.
    ///
    /// [`is_ghost`]: PlanOverlaySegment::is_ghost
    pub const GHOST_SUFFIX: &'static str = "::prev";

    /// True when this segment is the synthetic ghost of a `Modified`
    /// wall's previous footprint rather than a real demolition.
    ///
    /// `PlanOverlayCounts::demolition` deliberately conflates both —
    /// the architectural drawing convention treats them identically —
    /// but downstream consumers (e.g. a *"walls removed: N"* badge in
    /// the UI) can subtract `is_demolition_ghost` segments from the
    /// total to count *truly* deleted walls.
    ///
    /// The check is type-safe: it relies on the [`is_ghost`] flag,
    /// not on string-matching `entity_id` against
    /// [`GHOST_SUFFIX`](Self::GHOST_SUFFIX), so a real wall id that
    /// naturally ends in `"::prev"` is never misclassified as a
    /// synthetic ghost.
    ///
    /// [`is_ghost`]: Self::is_ghost
    pub fn is_demolition_ghost(&self) -> bool {
        self.level == PlanOverlayLevel::Demolition && self.is_ghost
    }

    /// Returns the underlying wall id with the
    /// [`GHOST_SUFFIX`](Self::GHOST_SUFFIX) stripped if this segment
    /// is a synthetic ghost, so a `Modified` wall and its demolition
    /// ghost both report the same id. For non-ghost segments this
    /// returns `entity_id` unchanged — including the case where a
    /// real wall id naturally ends in `"::prev"`.
    pub fn original_entity_id(&self) -> &str {
        if self.is_ghost {
            self.entity_id
                .strip_suffix(Self::GHOST_SUFFIX)
                .unwrap_or(&self.entity_id)
        } else {
            &self.entity_id
        }
    }
}

/// Classification of a wall segment in the before / after overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanOverlayLevel {
    /// Present in the base revision, absent in the head — drawn red
    /// (demolition).
    Demolition,
    /// Absent in the base revision, present in the head — drawn green
    /// (new construction).
    New,
    /// Present in both with the same body bytes — drawn dark grey
    /// (existing-to-remain).
    Unchanged,
    /// Present in both but with different body bytes (length / start /
    /// end / thickness moved) — drawn orange (modified).
    Modified,
}

impl PlanOverlayLevel {
    /// SVG stroke colour for this classification.
    pub fn stroke(&self) -> &'static str {
        match self {
            Self::Demolition => "#d62728",
            Self::New => "#2ca02c",
            Self::Unchanged => "#3a3a3a",
            Self::Modified => "#ff7f0e",
        }
    }

    /// Stable identifier for the SVG `<g>` group of this level (used
    /// by tests and downstream tooling to count segments per level
    /// without re-classifying).
    pub fn group_id(&self) -> &'static str {
        match self {
            Self::Demolition => "demolition",
            Self::New => "new",
            Self::Unchanged => "unchanged",
            Self::Modified => "modified",
        }
    }
}

/// The plan overlay produced by
/// [`BeforeAfterReport::plan_overlay`]: a flat list of classified
/// wall segments plus the bounding box that spans them all.
///
/// `bbox_mm` is the axis-aligned `[min_x, min_y, max_x, max_y]` in
/// model millimetres. When no walls exist, `bbox_mm` is `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanOverlay {
    pub segments: Vec<PlanOverlaySegment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox_mm: Option<[f64; 4]>,
}

impl PlanOverlay {
    /// Number of segments in each classification level.
    pub fn counts(&self) -> PlanOverlayCounts {
        let mut c = PlanOverlayCounts::default();
        for s in &self.segments {
            match s.level {
                PlanOverlayLevel::Demolition => c.demolition += 1,
                PlanOverlayLevel::New => c.new_construction += 1,
                PlanOverlayLevel::Unchanged => c.unchanged += 1,
                PlanOverlayLevel::Modified => c.modified += 1,
            }
        }
        c
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOverlayCounts {
    pub demolition: usize,
    pub new_construction: usize,
    pub unchanged: usize,
    pub modified: usize,
}

/// Full report that the Deliver-mode UI consumes when generating the
/// "before / after" deliverable from two revision snapshots.
///
/// Construct via [`BeforeAfterReport::compute`]. The `render_pairs`
/// field starts empty; callers attach explicit render pairs via
/// [`BeforeAfterReport::with_render_pairs`] or auto-discover them
/// from a project's `renders/` directory via
/// [`BeforeAfterReport::discover_render_pairs`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeforeAfterReport {
    pub project_name: String,
    pub base_revision_id: String,
    pub head_revision_id: String,
    pub base_tag: String,
    pub head_tag: String,
    pub plan_overlay: PlanOverlay,
    pub render_pairs: Vec<BeforeAfterRenderPair>,
    /// The underlying entity-level diff. Surfaced on the report so
    /// downstream consumers (PDF / UI) can show per-category change
    /// counts beyond just walls.
    pub version_diff: VersionDiff,
}

impl BeforeAfterReport {
    /// Build a report by diffing two revision snapshots.
    ///
    /// `key` is the SQLCipher project key (the same one used to write
    /// the original `project.sqlite` and therefore the `.snap`
    /// copies).
    pub fn compute(
        project_name: impl Into<String>,
        store: &RevisionStore,
        base: &Revision,
        head: &Revision,
        key: &Key32,
    ) -> Result<Self, BeforeAfterReportError> {
        let base_path = store
            .snapshot_path(base)
            .ok_or_else(|| AecError::Other("base revision has no snapshot".into()))?;
        let head_path = store
            .snapshot_path(head)
            .ok_or_else(|| AecError::Other("head revision has no snapshot".into()))?;
        if !base_path.exists() {
            return Err(AecError::Other(format!(
                "base snapshot file missing: {}",
                base_path.display()
            ))
            .into());
        }
        if !head_path.exists() {
            return Err(AecError::Other(format!(
                "head snapshot file missing: {}",
                head_path.display()
            ))
            .into());
        }

        // `.snap` files must be opened with `db::open_readonly` per the
        // convention established in PR #52 (`crates/aec_core/src/db.rs:56-60`,
        // `crates/aec_core/src/version_diff.rs::open_snapshot_db`, and
        // `crates/aec_core/tests/revision_snapshots.rs:109-119`). Using
        // `open_existing` would open the WAL-mode snapshot read-write,
        // creating stray `-wal` / `-shm` sidecar files in the revisions
        // directory and leaving the snapshot exposed to accidental writes.
        // `compare_revision_snapshots` below already opens the same two
        // snapshots correctly via `open_readonly`; this matches it.
        let base_conn = aec_core::db::open_readonly(&base_path, key)?;
        let head_conn = aec_core::db::open_readonly(&head_path, key)?;

        let base_walls = load_walls(&base_conn)?;
        let head_walls = load_walls(&head_conn)?;
        let plan_overlay = build_plan_overlay(&base_walls, &head_walls);
        let version_diff = compare_revision_snapshots(store, base, head, key)?;

        Ok(Self {
            project_name: project_name.into(),
            base_revision_id: base.id.clone(),
            head_revision_id: head.id.clone(),
            base_tag: base.tag.clone(),
            head_tag: head.tag.clone(),
            plan_overlay,
            render_pairs: Vec::new(),
            version_diff,
        })
    }

    /// Attach an explicit list of render pairs to the report.
    pub fn with_render_pairs(mut self, pairs: Vec<BeforeAfterRenderPair>) -> Self {
        self.render_pairs = pairs;
        self
    }

    /// Auto-discover render pairs from a project's renders directory.
    ///
    /// Looks for filenames following the
    /// `<camera>__<preset>__<revision-id>.<ext>` convention written
    /// by [`aec_render`]. For each `(camera, preset)` pair that has
    /// renders under both `base.id` and `head.id`, a
    /// [`BeforeAfterRenderPair`] is emitted with `label =
    /// "<camera> · <preset>"` and `before_ms` / `after_ms` set to
    /// `0` (the per-file render timing is not part of the filename;
    /// callers that want accurate durations should attach pairs
    /// explicitly).
    ///
    /// Missing or unreadable directories return an empty vector
    /// rather than an error so that
    /// "no renders were captured for these revisions" is a valid
    /// state — the report still ships, just with zero render pairs.
    pub fn discover_render_pairs(
        renders_dir: impl AsRef<Path>,
        base_revision_id: &str,
        head_revision_id: &str,
    ) -> Vec<BeforeAfterRenderPair> {
        discover_render_pairs_impl(renders_dir.as_ref(), base_revision_id, head_revision_id)
    }

    /// Render the plan overlay as a self-contained SVG document.
    ///
    /// Coordinates are in millimetres. The viewport is the bbox of
    /// all segments with a `margin_mm` border. When the overlay has no
    /// segments the SVG still emits a valid (empty) document with a
    /// "No walls in either revision" placeholder text.
    pub fn render_plan_overlay_svg(
        &self,
        margin_mm: f64,
    ) -> Result<String, BeforeAfterReportError> {
        render_plan_overlay_svg_impl(&self.plan_overlay, margin_mm)
    }

    /// Convenience: write the plan overlay SVG to a file. Returns the
    /// path written. Parent directories are created if missing.
    pub fn write_plan_overlay_svg(
        &self,
        out_path: impl AsRef<Path>,
        margin_mm: f64,
    ) -> Result<PathBuf, BeforeAfterReportError> {
        let p = out_path.as_ref();
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let svg = self.render_plan_overlay_svg(margin_mm)?;
        fs::write(p, svg)?;
        Ok(p.to_path_buf())
    }
}

#[derive(Debug, Clone)]
struct WallBody {
    id: String,
    start_mm: [f64; 2],
    end_mm: [f64; 2],
    thickness_mm: f64,
    /// BLAKE3 of the wall's `entities.body` JSON column so we can
    /// detect "same id, different body" as `Modified` without
    /// re-comparing every field.
    ///
    /// **Scope is intentionally geometry-only** and is *narrower* than
    /// [`aec_core::version_diff::snapshot_entities`], which hashes
    /// `kind || parent_id || body || components`. The plan overlay
    /// answers a single question — *"did this wall move, change
    /// length, or change thickness?"* — so re-parenting a wall (e.g.
    /// reassigning it to a different room without touching the
    /// segment endpoints) classifies the wall as `Unchanged` here
    /// even though the version diff reports it as `Modified`. Both
    /// classifications are correct for their respective consumers:
    /// the overlay tracks what a contractor on-site would see, while
    /// the version diff tracks the project's structural graph.
    body_hash: String,
}

fn load_walls(conn: &Connection) -> Result<Vec<WallBody>, BeforeAfterReportError> {
    let mut stmt = conn.prepare("SELECT id, body FROM entities WHERE kind = 'wall' ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let body: String = row.get(1)?;
        Ok((id, body))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, body) = r?;
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| BeforeAfterReportError::MalformedWall {
                id: id.clone(),
                detail: format!("invalid json: {e}"),
            })?;
        let start = parse_xy(&parsed, "start_mm", &id)?;
        let end = parse_xy(&parsed, "end_mm", &id)?;
        let thickness = parsed
            .get("thickness_mm")
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| BeforeAfterReportError::MalformedWall {
                id: id.clone(),
                detail: "missing or non-numeric thickness_mm".into(),
            })?;
        let body_hash = blake3::hash(body.as_bytes()).to_hex().to_string();
        out.push(WallBody {
            id,
            start_mm: start,
            end_mm: end,
            thickness_mm: thickness,
            body_hash,
        });
    }
    Ok(out)
}

fn parse_xy(
    v: &serde_json::Value,
    key: &str,
    id: &str,
) -> Result<[f64; 2], BeforeAfterReportError> {
    let arr = v.get(key).and_then(|v| v.as_array()).ok_or_else(|| {
        BeforeAfterReportError::MalformedWall {
            id: id.into(),
            detail: format!("missing or non-array {key}"),
        }
    })?;
    if arr.len() != 2 {
        return Err(BeforeAfterReportError::MalformedWall {
            id: id.into(),
            detail: format!("{key} must be a 2-element array, got {}", arr.len()),
        });
    }
    let x = arr[0]
        .as_f64()
        .ok_or_else(|| BeforeAfterReportError::MalformedWall {
            id: id.into(),
            detail: format!("{key}[0] is not a number"),
        })?;
    let y = arr[1]
        .as_f64()
        .ok_or_else(|| BeforeAfterReportError::MalformedWall {
            id: id.into(),
            detail: format!("{key}[1] is not a number"),
        })?;
    Ok([x, y])
}

fn build_plan_overlay(base: &[WallBody], head: &[WallBody]) -> PlanOverlay {
    use std::collections::BTreeMap;
    let base_map: BTreeMap<&str, &WallBody> = base.iter().map(|w| (w.id.as_str(), w)).collect();
    let head_map: BTreeMap<&str, &WallBody> = head.iter().map(|w| (w.id.as_str(), w)).collect();

    let mut segments: Vec<PlanOverlaySegment> = Vec::new();
    let mut bbox: Option<[f64; 4]> = None;
    let mut expand = |w: &WallBody| {
        let mut grow = |x: f64, y: f64| match bbox.as_mut() {
            Some(b) => {
                if x < b[0] {
                    b[0] = x;
                }
                if y < b[1] {
                    b[1] = y;
                }
                if x > b[2] {
                    b[2] = x;
                }
                if y > b[3] {
                    b[3] = y;
                }
            }
            // Seed the bbox from this very `grow(x, y)` invocation
            // rather than the captured wall's `start_mm`. The two are
            // equal today because the first `grow` call below always
            // passes `w.start_mm`, but using the local `(x, y)` makes
            // the closure robust to future refactors that might call
            // `grow(w.end_mm[0], w.end_mm[1])` first.
            None => bbox = Some([x, y, x, y]),
        };
        grow(w.start_mm[0], w.start_mm[1]);
        grow(w.end_mm[0], w.end_mm[1]);
    };

    for (id, w) in &head_map {
        if let Some(prev) = base_map.get(id) {
            let level = if prev.body_hash == w.body_hash {
                PlanOverlayLevel::Unchanged
            } else {
                PlanOverlayLevel::Modified
            };
            expand(w);
            segments.push(PlanOverlaySegment {
                entity_id: w.id.clone(),
                start_mm: w.start_mm,
                end_mm: w.end_mm,
                thickness_mm: w.thickness_mm,
                level,
                is_ghost: false,
            });
            if level == PlanOverlayLevel::Modified {
                // Modified walls also draw the *old* position
                // underneath in red so the demolition footprint
                // is visible — that's what the convention asks
                // for on architectural renovation drawings. The
                // `::prev` suffix on `entity_id` is a display-only
                // convention so the SVG group has a stable unique
                // id; the authoritative ghost marker is the typed
                // `is_ghost` flag (see `PlanOverlaySegment` docs).
                expand(prev);
                segments.push(PlanOverlaySegment {
                    entity_id: format!("{}{}", prev.id, PlanOverlaySegment::GHOST_SUFFIX),
                    start_mm: prev.start_mm,
                    end_mm: prev.end_mm,
                    thickness_mm: prev.thickness_mm,
                    level: PlanOverlayLevel::Demolition,
                    is_ghost: true,
                });
            }
        } else {
            expand(w);
            segments.push(PlanOverlaySegment {
                entity_id: w.id.clone(),
                start_mm: w.start_mm,
                end_mm: w.end_mm,
                thickness_mm: w.thickness_mm,
                level: PlanOverlayLevel::New,
                is_ghost: false,
            });
        }
    }
    for (id, w) in &base_map {
        if !head_map.contains_key(id) {
            expand(w);
            segments.push(PlanOverlaySegment {
                entity_id: w.id.clone(),
                start_mm: w.start_mm,
                end_mm: w.end_mm,
                thickness_mm: w.thickness_mm,
                level: PlanOverlayLevel::Demolition,
                is_ghost: false,
            });
        }
    }

    // Deterministic order: group by level (Demolition first so the
    // red segments sit beneath new construction in the SVG paint
    // order), then by entity id.
    segments.sort_by(|a, b| {
        level_paint_order(a.level)
            .cmp(&level_paint_order(b.level))
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });

    PlanOverlay {
        segments,
        bbox_mm: bbox,
    }
}

fn level_paint_order(l: PlanOverlayLevel) -> u8 {
    match l {
        PlanOverlayLevel::Demolition => 0,
        PlanOverlayLevel::Unchanged => 1,
        PlanOverlayLevel::Modified => 2,
        PlanOverlayLevel::New => 3,
    }
}

fn render_plan_overlay_svg_impl(
    overlay: &PlanOverlay,
    margin_mm: f64,
) -> Result<String, BeforeAfterReportError> {
    use std::fmt::Write;

    let mut out = String::new();
    let margin = margin_mm.max(0.0);

    // The viewBox is expressed in model-space millimetres. The SVG
    // user-coordinate system has `+y` pointing *down*, but building
    // plans are conventionally drawn with `+y` pointing *up* (north).
    // To get the correct visual orientation without rewriting every
    // line's coordinates (which would degrade the floating-point
    // round-trip with `bbox_mm`), we keep the viewBox and per-segment
    // coordinates in model-space and apply a single Y-flip transform
    // to the wrapping `<g class="model-space">` group below.
    let (vx, vy, vw, vh, flip_anchor) = match overlay.bbox_mm {
        Some([min_x, min_y, max_x, max_y]) => {
            let w = (max_x - min_x).max(1.0);
            let h = (max_y - min_y).max(1.0);
            (
                min_x - margin,
                min_y - margin,
                w + 2.0 * margin,
                h + 2.0 * margin,
                // `translate(0, min_y + max_y) scale(1, -1)` maps
                // model y=min_y → SVG y=max_y (bottom of viewBox) and
                // model y=max_y → SVG y=min_y (top of viewBox), which
                // is the Y-up → Y-down flip we want.
                Some(min_y + max_y),
            )
        }
        None => (0.0, 0.0, 100.0, 100.0, None),
    };

    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}mm" height="{h}mm" viewBox="{vx} {vy} {w} {h}">"#,
        w = format_mm(vw),
        h = format_mm(vh),
        vx = format_mm(vx),
        vy = format_mm(vy),
    )?;
    writeln!(out, r#"  <title>Before / after plan overlay</title>"#)?;
    writeln!(
        out,
        r#"  <desc>Generated by aec_export::before_after; red = demolition, green = new, orange = modified, dark grey = unchanged.</desc>"#
    )?;
    writeln!(
        out,
        r#"  <rect x="{vx}" y="{vy}" width="{w}" height="{h}" fill="white" />"#,
        vx = format_mm(vx),
        vy = format_mm(vy),
        w = format_mm(vw),
        h = format_mm(vh),
    )?;

    if overlay.segments.is_empty() {
        // Placeholder text deliberately renders *outside* any Y-flip
        // group so the glyphs read normally (a flipped <text> would
        // appear upside-down).
        writeln!(
            out,
            r##"  <text x="{}" y="{}" font-family="sans-serif" font-size="6" fill="#666">No walls in either revision</text>"##,
            format_mm(vx + 4.0),
            format_mm(vy + 10.0)
        )?;
        writeln!(out, "</svg>")?;
        return Ok(out);
    }

    // Group segments by level so the SVG output has a stable
    // structure that tests can interrogate directly.
    let mut by_level: std::collections::BTreeMap<u8, Vec<&PlanOverlaySegment>> =
        std::collections::BTreeMap::new();
    for s in &overlay.segments {
        by_level
            .entry(level_paint_order(s.level))
            .or_default()
            .push(s);
    }

    // Wrap every level group in a single Y-flip transform so the
    // segment lines themselves stay in plain model-space mm. We only
    // emit the wrapper when we actually have a bbox to anchor against
    // (empty overlays already short-circuited above, but defensive).
    let close_flip = if let Some(anchor) = flip_anchor {
        writeln!(
            out,
            r#"  <g class="model-space" transform="translate(0 {anchor}) scale(1 -1)">"#,
            anchor = format_mm(anchor)
        )?;
        true
    } else {
        false
    };

    for (_, group) in by_level {
        let level = group[0].level;
        writeln!(
            out,
            r#"    <g class="overlay-level" id="overlay-{group}" stroke="{stroke}" stroke-linecap="round" fill="none">"#,
            group = level.group_id(),
            stroke = level.stroke()
        )?;
        for s in group {
            let stroke_width = s.thickness_mm.max(20.0);
            writeln!(
                out,
                r#"      <line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke-width="{sw}" data-entity-id="{id}" />"#,
                x1 = format_mm(s.start_mm[0]),
                y1 = format_mm(s.start_mm[1]),
                x2 = format_mm(s.end_mm[0]),
                y2 = format_mm(s.end_mm[1]),
                sw = format_mm(stroke_width),
                id = svg_escape(&s.entity_id),
            )?;
        }
        writeln!(out, "    </g>")?;
    }

    if close_flip {
        writeln!(out, "  </g>")?;
    }

    writeln!(out, "</svg>")?;
    Ok(out)
}

fn format_mm(v: f64) -> String {
    // 3 dp is plenty for mm — sub-micron precision would be noise
    // and bloat the SVG. Trailing zeros are stripped manually so the
    // output is deterministic and human-readable.
    let s = format!("{v:.3}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn svg_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn discover_render_pairs_impl(
    renders_dir: &Path,
    base_revision_id: &str,
    head_revision_id: &str,
) -> Vec<BeforeAfterRenderPair> {
    use std::collections::BTreeMap;
    let Ok(entries) = fs::read_dir(renders_dir) else {
        return Vec::new();
    };
    // Index files by (camera, preset) -> { base_path, head_path }.
    let mut idx: BTreeMap<(String, String), (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Strip extension.
        let stem = match name.rsplit_once('.') {
            Some((stem, _)) => stem,
            None => name,
        };
        // Expect <camera>__<preset>__<rev-id>
        let parts: Vec<&str> = stem.split("__").collect();
        if parts.len() != 3 {
            continue;
        }
        let (camera, preset, rev) = (parts[0], parts[1], parts[2]);
        let key = (camera.to_string(), preset.to_string());
        let slot = idx.entry(key).or_insert((None, None));
        if rev == base_revision_id {
            slot.0 = Some(path);
        } else if rev == head_revision_id {
            slot.1 = Some(path);
        }
    }
    idx.into_iter()
        .filter_map(
            |((camera, preset), (before, after))| match (before, after) {
                (Some(before_path), Some(after_path)) => Some(BeforeAfterRenderPair {
                    label: format!("{camera} · {preset}"),
                    before_path,
                    after_path,
                    before_preset: preset.clone(),
                    after_preset: preset,
                    before_ms: 0,
                    after_ms: 0,
                }),
                _ => None,
            },
        )
        .collect()
}

impl BeforeAfterPdfOptions {
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<PathBuf, BeforeAfterPdfError> {
        if self.pairs.is_empty() {
            return Err(BeforeAfterPdfError::Empty);
        }

        let mut builder = PdfBuilder::new(
            format!("{} — before / after", self.project_name),
            PageSize::A4_LANDSCAPE,
        )?;
        builder.add_cover_page(Some("Comparison of render iterations"))?;

        for pair in &self.pairs {
            let delta_ms: i64 = pair.after_ms as i64 - pair.before_ms as i64;
            let delta_text = if delta_ms.abs() > 0 {
                format!("Δ time: {:+} ms", delta_ms)
            } else {
                "Δ time: 0 ms".to_string()
            };
            let lines: Vec<String> = vec![
                format!("Comparison: {}", pair.label),
                String::new(),
                format!("Before: {}", pair.before_path.display()),
                format!("  preset: {}", pair.before_preset),
                format!("  duration: {} ms", pair.before_ms),
                String::new(),
                format!("After:  {}", pair.after_path.display()),
                format!("  preset: {}", pair.after_preset),
                format!("  duration: {} ms", pair.after_ms),
                String::new(),
                delta_text,
                format!(
                    "Preset change: {} → {}",
                    pair.before_preset, pair.after_preset
                ),
            ];
            builder.add_text_page(&format!("Before / After — {}", pair.label), &lines)?;
        }

        let out = builder.save(path.as_ref())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_pair() -> BeforeAfterRenderPair {
        BeforeAfterRenderPair {
            label: "Living room".into(),
            before_path: PathBuf::from("/renders/living_before.png"),
            after_path: PathBuf::from("/renders/living_after.png"),
            before_preset: "standard".into(),
            after_preset: "studio".into(),
            before_ms: 9_500,
            after_ms: 38_200,
        }
    }

    #[test]
    fn writes_pdf_with_one_page_per_pair_plus_cover() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("before_after.pdf");
        let opts = BeforeAfterPdfOptions {
            project_name: "Apartment 12B".into(),
            pairs: vec![sample_pair(), sample_pair()],
        };
        opts.write_to(&path).unwrap();

        let mut f = std::fs::File::open(&path).unwrap();
        let mut header = [0u8; 5];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"%PDF-", "output must be a valid PDF");

        let meta = std::fs::metadata(&path).unwrap();
        // Cover + 2 pairs => at minimum 3 KB of content.
        assert!(meta.len() > 1_500);
    }

    #[test]
    fn empty_pairs_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = BeforeAfterPdfOptions {
            project_name: "Test".into(),
            pairs: vec![],
        };
        let err = opts.write_to(tmp.path().join("err.pdf")).unwrap_err();
        assert!(matches!(err, BeforeAfterPdfError::Empty));
    }

    fn wall(id: &str, start: [f64; 2], end: [f64; 2], thickness: f64) -> WallBody {
        let body = serde_json::json!({
            "entity_id": id,
            "start_mm": start,
            "end_mm": end,
            "thickness_mm": thickness,
            "height_mm": 2700.0,
        });
        let body_str = serde_json::to_string(&body).unwrap();
        WallBody {
            id: id.into(),
            start_mm: start,
            end_mm: end,
            thickness_mm: thickness,
            body_hash: blake3::hash(body_str.as_bytes()).to_hex().to_string(),
        }
    }

    #[test]
    fn build_plan_overlay_classifies_added_removed_modified_unchanged() {
        let base = vec![
            wall("w.A", [0.0, 0.0], [4000.0, 0.0], 200.0),
            wall("w.B", [0.0, 0.0], [0.0, 3000.0], 200.0),
            wall("w.C", [4000.0, 0.0], [4000.0, 3000.0], 200.0),
        ];
        let head = vec![
            wall("w.A", [0.0, 0.0], [4500.0, 0.0], 200.0), // modified (length changed)
            wall("w.C", [4000.0, 0.0], [4000.0, 3000.0], 200.0), // unchanged
            wall("w.D", [0.0, 3000.0], [4500.0, 3000.0], 200.0), // new
        ];
        // w.B is in base but not in head → demolition
        let overlay = build_plan_overlay(&base, &head);
        let counts = overlay.counts();
        assert_eq!(counts.unchanged, 1, "w.C should be unchanged");
        assert_eq!(counts.modified, 1, "w.A should be modified");
        assert_eq!(counts.new_construction, 1, "w.D should be new");
        // w.B (deleted) + the previous footprint of modified w.A both
        // count as demolition.
        assert_eq!(counts.demolition, 2);
        // bbox must encompass all coords across both sides.
        let bbox = overlay.bbox_mm.expect("bbox set");
        assert_eq!(bbox, [0.0, 0.0, 4500.0, 3000.0]);
    }

    #[test]
    fn plan_overlay_distinguishes_demolition_ghosts_from_real_deletions() {
        // The PlanOverlayCounts API conflates *truly deleted* walls and
        // the synthetic old-footprint ghosts of `Modified` walls — both
        // render in red per renovation-drawing convention. Downstream
        // consumers that need to distinguish the two cases rely on the
        // typed `is_ghost` flag surfaced by
        // `PlanOverlaySegment::is_demolition_ghost` /
        // `original_entity_id`; lock that contract in here so it can't
        // silently drift.
        let base = vec![
            wall("w.A", [0.0, 0.0], [4000.0, 0.0], 200.0),
            wall("w.B", [0.0, 0.0], [0.0, 3000.0], 200.0),
        ];
        let head = vec![
            wall("w.A", [0.0, 0.0], [4500.0, 0.0], 200.0), // modified
                                                           // w.B deleted
        ];
        let overlay = build_plan_overlay(&base, &head);

        let demolitions: Vec<_> = overlay
            .segments
            .iter()
            .filter(|s| s.level == PlanOverlayLevel::Demolition)
            .collect();
        // 1 real deletion (w.B) + 1 synthetic ghost (w.A::prev).
        assert_eq!(demolitions.len(), 2);

        let real_deletions: Vec<&PlanOverlaySegment> = demolitions
            .iter()
            .copied()
            .filter(|s| !s.is_demolition_ghost())
            .collect();
        assert_eq!(real_deletions.len(), 1);
        assert_eq!(real_deletions[0].entity_id, "w.B");
        // Real demolitions are *not* marked as ghosts — the typed
        // flag distinguishes them even though they share the same
        // `PlanOverlayLevel::Demolition` classification as ghosts.
        assert!(!real_deletions[0].is_ghost);

        let ghosts: Vec<&PlanOverlaySegment> = demolitions
            .iter()
            .copied()
            .filter(|s| s.is_demolition_ghost())
            .collect();
        assert_eq!(ghosts.len(), 1);
        assert!(ghosts[0].is_ghost);
        // The `::prev` suffix is a display convention so the SVG
        // group has a stable unique id; the authoritative marker is
        // `is_ghost`, not the suffix.
        assert_eq!(ghosts[0].entity_id, "w.A::prev");
        // original_entity_id strips the synthetic suffix so the ghost
        // and its Modified twin both report the same wall id.
        assert_eq!(ghosts[0].original_entity_id(), "w.A");

        // Non-ghost segments are unaffected by original_entity_id.
        let modified = overlay
            .segments
            .iter()
            .find(|s| s.level == PlanOverlayLevel::Modified)
            .expect("modified segment exists");
        assert_eq!(modified.entity_id, "w.A");
        assert_eq!(modified.original_entity_id(), "w.A");
        assert!(!modified.is_demolition_ghost());
        assert!(!modified.is_ghost);
    }

    #[test]
    fn plan_overlay_does_not_misclassify_real_walls_whose_id_ends_in_prev() {
        // Regression guard for the collision risk in the old
        // string-suffix-based ghost detection: a real wall whose
        // `entities.id` happens to end in `"::prev"` must NOT be
        // treated as a synthetic demolition ghost. The authoritative
        // marker is the typed `is_ghost` flag, set only by the
        // overlay builder when emitting an old-footprint segment for
        // a Modified wall — `entity_id` is never consulted for the
        // ghost decision.
        let base = vec![
            // Real wall whose id naturally ends in "::prev". In the
            // old string-suffix approach this would be misclassified
            // as a synthetic ghost when it appears as a Demolition.
            wall("legacy::prev", [0.0, 0.0], [4000.0, 0.0], 200.0),
            // Real wall that becomes Modified (will produce a real
            // synthetic ghost we still expect to flag correctly).
            wall("w.A", [0.0, 0.0], [0.0, 3000.0], 200.0),
        ];
        let head = vec![
            // legacy::prev removed in head — should classify as a
            // *real* demolition, NOT a ghost.
            // w.A modified — should produce a real Modified segment
            // *plus* a synthetic ghost suffixed "::prev".
            wall("w.A", [100.0, 0.0], [100.0, 3000.0], 200.0),
        ];
        let overlay = build_plan_overlay(&base, &head);

        let demolitions: Vec<&PlanOverlaySegment> = overlay
            .segments
            .iter()
            .filter(|s| s.level == PlanOverlayLevel::Demolition)
            .collect();
        // 1 real deletion (legacy::prev) + 1 synthetic ghost (w.A::prev).
        assert_eq!(demolitions.len(), 2);

        // The real deletion of `legacy::prev` must report as a
        // genuine demolition, even though its id ends in "::prev".
        let legacy = demolitions
            .iter()
            .find(|s| s.entity_id == "legacy::prev")
            .expect("legacy::prev demolition exists");
        assert!(
            !legacy.is_ghost,
            "real wall whose id ends in '::prev' must not be flagged as a ghost"
        );
        assert!(
            !legacy.is_demolition_ghost(),
            "is_demolition_ghost() must not misclassify a real wall whose id ends in '::prev'"
        );
        // original_entity_id() must NOT strip the trailing "::prev"
        // from a real wall id — the suffix is part of its real id.
        assert_eq!(legacy.original_entity_id(), "legacy::prev");

        // The synthetic ghost of `w.A` must still be flagged.
        let ghost = demolitions
            .iter()
            .find(|s| s.entity_id == "w.A::prev")
            .expect("w.A::prev synthetic ghost exists");
        assert!(ghost.is_ghost);
        assert!(ghost.is_demolition_ghost());
        assert_eq!(ghost.original_entity_id(), "w.A");
    }

    #[test]
    fn plan_overlay_svg_contains_level_groups_and_correct_colours() {
        let base = vec![wall("w.A", [0.0, 0.0], [4000.0, 0.0], 200.0)];
        let head = vec![
            wall("w.A", [0.0, 0.0], [4000.0, 0.0], 200.0),
            wall("w.B", [0.0, 0.0], [0.0, 3000.0], 200.0),
        ];
        let overlay = build_plan_overlay(&base, &head);
        let svg = render_plan_overlay_svg_impl(&overlay, 100.0).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg "));
        assert!(svg.contains(r#"id="overlay-unchanged""#));
        assert!(svg.contains(r#"id="overlay-new""#));
        // Demolition group must not exist (no walls deleted).
        assert!(!svg.contains(r#"id="overlay-demolition""#));
        assert!(svg.contains(PlanOverlayLevel::New.stroke()));
        assert!(svg.contains(PlanOverlayLevel::Unchanged.stroke()));
        // SVG output ends with the closing tag (sanity check on
        // structure).
        assert!(svg.trim_end().ends_with("</svg>"));
    }

    #[test]
    fn plan_overlay_svg_for_empty_overlay_is_still_valid() {
        let overlay = PlanOverlay {
            segments: vec![],
            bbox_mm: None,
        };
        let svg = render_plan_overlay_svg_impl(&overlay, 50.0).unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("No walls in either revision"));
        assert!(svg.trim_end().ends_with("</svg>"));
        // Empty overlay has no bbox → no Y-flip wrapper emitted
        // (otherwise the placeholder text would render upside-down).
        assert!(!svg.contains(r#"class="model-space""#));
    }

    #[test]
    fn plan_overlay_svg_wraps_segments_in_y_flip_transform() {
        // Plan with walls spanning the bbox in y; the renderer must
        // emit a single Y-flip `<g transform=...>` so SVG's y-down
        // user-coordinate system displays the building's y-up
        // convention correctly. Per-segment coordinates stay in plain
        // model-space mm so the SVG round-trips with `bbox_mm`.
        let base = vec![wall("w.south", [0.0, 0.0], [4000.0, 0.0], 200.0)];
        let head = vec![
            wall("w.south", [0.0, 0.0], [4000.0, 0.0], 200.0),
            // North wall at high model-y — appears at top of viewBox
            // post-flip.
            wall("w.north", [0.0, 3000.0], [4000.0, 3000.0], 200.0),
        ];
        let overlay = build_plan_overlay(&base, &head);
        let svg = render_plan_overlay_svg_impl(&overlay, 100.0).unwrap();

        // bbox is [0, 0, 4000, 3000] → flip anchor = min_y + max_y = 3000.
        assert!(
            svg.contains(r#"<g class="model-space" transform="translate(0 3000) scale(1 -1)">"#),
            "expected Y-flip wrapper anchored at min_y + max_y; got:\n{svg}"
        );
        // Segment coordinates remain in plain model-space mm — the
        // north wall is still at y=3000 in the emitted <line>; the
        // viewer flips at render-time via the wrapping <g> transform.
        assert!(
            svg.contains(r#"y1="3000" x2="4000" y2="3000""#),
            "north wall must keep model-space y=3000 in the <line>; got:\n{svg}"
        );
    }

    #[test]
    fn discover_render_pairs_only_matches_complete_before_and_after() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // base render: cam01 + standard for rev_base
        std::fs::write(dir.join("cam01__standard__rev_base.png"), b"a").unwrap();
        // head render: cam01 + standard for rev_head
        std::fs::write(dir.join("cam01__standard__rev_head.png"), b"b").unwrap();
        // Only head side present for cam02 — should NOT be paired.
        std::fs::write(dir.join("cam02__studio__rev_head.png"), b"c").unwrap();
        // Garbage file (not following the convention) — must be ignored.
        std::fs::write(dir.join("random.txt"), b"d").unwrap();

        let pairs = discover_render_pairs_impl(dir, "rev_base", "rev_head");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].label, "cam01 · standard");
        assert_eq!(pairs[0].before_preset, "standard");
        assert_eq!(pairs[0].after_preset, "standard");
        assert!(pairs[0]
            .before_path
            .ends_with("cam01__standard__rev_base.png"));
        assert!(pairs[0]
            .after_path
            .ends_with("cam01__standard__rev_head.png"));
    }

    #[test]
    fn discover_render_pairs_returns_empty_when_directory_missing() {
        // Non-existent path → empty vector, not an error.
        let pairs = discover_render_pairs_impl(
            std::path::Path::new("/tmp/devin-does-not-exist-xxx"),
            "rev_base",
            "rev_head",
        );
        assert!(pairs.is_empty());
    }

    #[test]
    fn format_mm_strips_trailing_zeros_and_handles_zero() {
        assert_eq!(format_mm(0.0), "0");
        assert_eq!(format_mm(100.0), "100");
        assert_eq!(format_mm(1.5), "1.5");
        assert_eq!(format_mm(1.500000), "1.5");
        assert_eq!(format_mm(0.123), "0.123");
        assert_eq!(format_mm(0.1234), "0.123");
    }

    #[test]
    fn svg_escape_handles_xml_special_characters() {
        assert_eq!(svg_escape("simple"), "simple");
        assert_eq!(svg_escape("a&b"), "a&amp;b");
        assert_eq!(svg_escape("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(svg_escape(r#"a"b'c"#), "a&quot;b&apos;c");
    }
}
