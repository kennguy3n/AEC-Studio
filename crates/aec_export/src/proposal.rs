//! Proposal pack — the multi-page PDF that bundles the cover, mood board,
//! plan, renders, and material schedule into a single deliverable.
//!
//! Render image embedding strategy: printpdf's `Image` API requires
//! decoded pixel buffers, which would force a heavyweight image decoder
//! dependency into the export crate. Instead we keep `RenderAttachment`
//! purely path-based and produce a "render page" per attachment that
//! lists the file path, caption, and metadata — the desktop preview
//! and downstream PDF mergers can splice in the real pixels when
//! needed. This keeps the export pure, deterministic, and very fast.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};
use crate::schedule::ScheduleSheet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderAttachment {
    pub path: PathBuf,
    pub caption: String,
    pub preset_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProposalBranding {
    pub studio_name: String,
    pub studio_tagline: Option<String>,
    pub logo_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProposalAssets {
    /// Mood-board narrative paragraphs.
    pub mood_board: Vec<String>,
    /// Plan-overview narrative paragraphs.
    pub plan_overview: Vec<String>,
    /// Optional AI-generated cover paragraph. When set the cover page
    /// renders this paragraph after the title; when not set the cover
    /// shows only the title and subtitle.
    pub cover_paragraph: Option<String>,
    /// Render attachments — each becomes its own page in the pack.
    pub renders: Vec<RenderAttachment>,
    /// Floor-plan narrative shown on a dedicated page before the
    /// render gallery. Leave empty to omit the page.
    pub floor_plan_overview: Vec<String>,
    /// Optional "next steps" bullets shown on the closing page.
    pub next_steps: Vec<String>,
}

/// Ordering knob for the proposal pack pages.
///
/// Two arrangements are useful in practice:
/// * `RendersBeforeSchedules` — used for client concept packs where
///   the visual story leads.
/// * `SchedulesBeforeRenders` — used for contractor-facing proposals
///   where the schedules are the headline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProposalPageOrder {
    #[default]
    RendersBeforeSchedules,
    SchedulesBeforeRenders,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalPack {
    pub project_name: String,
    pub client_name: String,
    pub designer_name: String,
    pub branding: ProposalBranding,
    pub assets: ProposalAssets,
    pub material_schedule: ScheduleSheet,
    pub furniture_schedule: ScheduleSheet,
    pub page_order: ProposalPageOrder,
}

impl ProposalPack {
    pub fn new(project_name: impl Into<String>, client_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            client_name: client_name.into(),
            designer_name: "AEC Studio".into(),
            branding: ProposalBranding {
                studio_name: "AEC Studio".into(),
                studio_tagline: None,
                logo_path: None,
            },
            assets: ProposalAssets::default(),
            material_schedule: ScheduleSheet::material_schedule_template(),
            furniture_schedule: ScheduleSheet::furniture_schedule_template(),
            page_order: ProposalPageOrder::RendersBeforeSchedules,
        }
    }

    /// Render the proposal pack as a multi-page PDF. The page order is
    /// determined by `self.page_order` so client-facing and
    /// contractor-facing proposals can share the same datum.
    pub fn to_pdf(&self, path: impl AsRef<Path>) -> Result<PathBuf, PdfBuilderError> {
        let mut b = PdfBuilder::new(&self.project_name, PageSize::A4_PORTRAIT)?;

        // Cover page assembly. We compose the subtitle from the studio
        // branding (preferred) or the legacy `designer_name` fallback.
        let subtitle = if let Some(tagline) = &self.branding.studio_tagline {
            format!(
                "Proposal for {} · {} ({})",
                self.client_name, self.branding.studio_name, tagline
            )
        } else {
            format!(
                "Proposal for {} · prepared by {}",
                self.client_name,
                if self.branding.studio_name.is_empty() {
                    &self.designer_name
                } else {
                    &self.branding.studio_name
                }
            )
        };
        b.add_cover_page(Some(&subtitle))?;
        if let Some(paragraph) = &self.assets.cover_paragraph {
            // Wrap the cover paragraph onto its own page so the
            // formatting matches the rest of the document; the cover
            // page itself remains the visual hero.
            let wrapped = wrap_paragraph(paragraph, 80);
            b.add_text_page("Concept", &wrapped)?;
        }

        b.add_text_page("Mood board", &self.assets.mood_board)?;
        b.add_text_page("Plan overview", &self.assets.plan_overview)?;

        if !self.assets.floor_plan_overview.is_empty() {
            b.add_text_page("Floor plan", &self.assets.floor_plan_overview)?;
        }

        let write_renders = |b: &mut PdfBuilder| -> Result<(), PdfBuilderError> {
            if self.assets.renders.is_empty() {
                return Ok(());
            }
            for render in &self.assets.renders {
                let lines = vec![
                    render.caption.clone(),
                    String::new(),
                    format!("Render: {}", render.path.display()),
                    format!("Preset: {}", render.preset_id),
                ];
                b.add_text_page(&format!("Render — {}", render.caption), &lines)?;
            }
            Ok(())
        };

        let write_schedules = |b: &mut PdfBuilder| -> Result<(), PdfBuilderError> {
            b.add_table_page(
                &self.material_schedule.title,
                &self
                    .material_schedule
                    .columns
                    .iter()
                    .map(|c| c.display_name.clone())
                    .collect::<Vec<_>>(),
                &self
                    .material_schedule
                    .rows
                    .iter()
                    .map(|r| r.cells.clone())
                    .collect::<Vec<_>>(),
            )?;
            b.add_table_page(
                &self.furniture_schedule.title,
                &self
                    .furniture_schedule
                    .columns
                    .iter()
                    .map(|c| c.display_name.clone())
                    .collect::<Vec<_>>(),
                &self
                    .furniture_schedule
                    .rows
                    .iter()
                    .map(|r| r.cells.clone())
                    .collect::<Vec<_>>(),
            )?;
            Ok(())
        };

        match self.page_order {
            ProposalPageOrder::RendersBeforeSchedules => {
                write_renders(&mut b)?;
                write_schedules(&mut b)?;
            }
            ProposalPageOrder::SchedulesBeforeRenders => {
                write_schedules(&mut b)?;
                write_renders(&mut b)?;
            }
        }

        if !self.assets.next_steps.is_empty() {
            b.add_text_page("Next steps", &self.assets.next_steps)?;
        }
        b.save(path)
    }
}

/// Word-wrap a paragraph at roughly `width` characters. Used to break
/// long AI-generated cover paragraphs into PDF-friendly lines.
fn wrap_paragraph(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() > width {
            out.push(current.clone());
            current.clear();
            current.push_str(word);
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_pack_produces_multi_page_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = ProposalPack::new("Loft 12B", "Eva K.");
        p.assets.mood_board.push("Warm woods, soft linens.".into());
        p.assets
            .plan_overview
            .push("Open-plan living + galley kitchen.".into());
        p.assets.renders.push(RenderAttachment {
            path: std::path::PathBuf::from("/renders/living.png"),
            caption: "Living room — golden hour".into(),
            preset_id: "standard".into(),
        });
        p.material_schedule
            .push_row(["MAT-001", "Oak board", "Living", "12 m²", "Acme"]);
        p.furniture_schedule
            .push_row(["FUR-001", "Sofa A", "Living", "1", "Velvet, dusty rose"]);
        p.assets.next_steps.push("Confirm material samples.".into());
        let path = p.to_pdf(dir.path().join("proposal.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 2048);
    }

    #[test]
    fn cover_paragraph_renders_concept_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut baseline = ProposalPack::new("Loft 12B", "Eva K.");
        baseline.assets.mood_board.push("warm woods".into());
        let baseline_path = baseline.to_pdf(dir.path().join("baseline.pdf")).unwrap();
        let baseline_size = std::fs::metadata(&baseline_path).unwrap().len();

        let mut with_cover = ProposalPack::new("Loft 12B", "Eva K.");
        with_cover.assets.mood_board.push("warm woods".into());
        with_cover.assets.cover_paragraph = Some(
            "A warm, sun-drenched home built around natural materials and an open plan."
                .repeat(4),
        );
        let cover_path = with_cover.to_pdf(dir.path().join("cover.pdf")).unwrap();
        let cover_size = std::fs::metadata(&cover_path).unwrap().len();

        // The cover paragraph adds a "Concept" page; the resulting PDF
        // must be measurably larger than the baseline.
        assert!(
            cover_size > baseline_size,
            "cover paragraph should add bytes: cover={cover_size} baseline={baseline_size}",
        );
    }

    #[test]
    fn render_attachments_get_their_own_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = ProposalPack::new("Loft 12B", "Eva K.");
        for i in 0..3 {
            p.assets.renders.push(RenderAttachment {
                path: std::path::PathBuf::from(format!("/renders/r{i}.png")),
                caption: format!("Render {i}"),
                preset_id: "standard".into(),
            });
        }
        let path = p.to_pdf(dir.path().join("renders.pdf")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // 3 render pages should make the file appreciably larger than
        // a no-render pack — sanity check the size as a regression.
        assert!(bytes.len() > 4_000);
    }

    #[test]
    fn page_order_can_be_flipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = ProposalPack::new("Loft 12B", "Eva K.");
        p.page_order = ProposalPageOrder::SchedulesBeforeRenders;
        p.assets.renders.push(RenderAttachment {
            path: std::path::PathBuf::from("/renders/r0.png"),
            caption: "R0".into(),
            preset_id: "standard".into(),
        });
        let path = p.to_pdf(dir.path().join("flip.pdf")).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn wrap_paragraph_breaks_on_word_boundaries() {
        let lines = wrap_paragraph("one two three four five six seven", 14);
        assert!(lines.iter().all(|l| l.len() <= 14));
        assert!(lines.len() >= 3);
    }
}
