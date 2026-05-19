//! Proposal pack — the multi-page PDF that bundles the cover, mood board,
//! plan, renders, and material schedule into a single deliverable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pdf::{PageSize, PdfBuilder, PdfBuilderError};
use crate::schedule::ScheduleSheet;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProposalAssets {
    /// Mood-board narrative paragraphs.
    pub mood_board: Vec<String>,
    /// Plan-overview narrative paragraphs.
    pub plan_overview: Vec<String>,
    /// Render captions (one per render image).
    pub render_captions: Vec<String>,
    /// Optional "next steps" bullets shown on the closing page.
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalPack {
    pub project_name: String,
    pub client_name: String,
    pub designer_name: String,
    pub assets: ProposalAssets,
    pub material_schedule: ScheduleSheet,
    pub furniture_schedule: ScheduleSheet,
}

impl ProposalPack {
    pub fn new(project_name: impl Into<String>, client_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            client_name: client_name.into(),
            designer_name: "AEC Studio".into(),
            assets: ProposalAssets::default(),
            material_schedule: ScheduleSheet::material_schedule_template(),
            furniture_schedule: ScheduleSheet::furniture_schedule_template(),
        }
    }

    /// Render the proposal pack as a multi-page PDF.
    pub fn to_pdf(&self, path: impl AsRef<Path>) -> Result<PathBuf, PdfBuilderError> {
        let mut b = PdfBuilder::new(&self.project_name, PageSize::A4_PORTRAIT)?;
        b.add_cover_page(Some(&format!(
            "Proposal for {} · prepared by {}",
            self.client_name, self.designer_name
        )))?;
        b.add_text_page("Mood board", &self.assets.mood_board)?;
        b.add_text_page("Plan overview", &self.assets.plan_overview)?;
        if !self.assets.render_captions.is_empty() {
            b.add_text_page("Renders", &self.assets.render_captions)?;
        }
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
        if !self.assets.next_steps.is_empty() {
            b.add_text_page("Next steps", &self.assets.next_steps)?;
        }
        b.save(path)
    }
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
        p.assets
            .render_captions
            .push("Living room — golden hour".into());
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
}
