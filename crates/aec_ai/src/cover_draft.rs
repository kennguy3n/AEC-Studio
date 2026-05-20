//! Cover-page-draft AI tool.
//!
//! The model emits a structured JSON object describing the proposal
//! cover: a short title, a one-line subtitle, the concept paragraph
//! that anchors the document, and an optional tone tag the proposal
//! writer can use to nudge typography or colour choices.
//!
//! This module owns the typed result, parsing, and a guardrails layer
//! that downgrades obviously broken outputs (empty paragraph, banned
//! tone) to a fallback. The actual model invocation lives in
//! [`crate::planner`] and [`crate::runtime`] — this file is the
//! parser + validator.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoverDraftError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("title must not be empty")]
    EmptyTitle,
    #[error("subtitle must not be empty")]
    EmptySubtitle,
    #[error("paragraph must contain at least {min} characters, got {actual}")]
    ParagraphTooShort { min: usize, actual: usize },
}

/// Optional tone tag from the AI cover output. Kept narrow so the
/// proposal renderer can branch on it without parsing free-form text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverDraftTone {
    Warm,
    Minimal,
    Industrial,
    Playful,
    Classical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverPageDraft {
    pub title: String,
    pub subtitle: String,
    pub paragraph: String,
    pub tone: Option<CoverDraftTone>,
}

/// Minimum paragraph length we'll accept from the model before
/// reverting to the fallback. Anything shorter is almost certainly
/// a hallucination ("Done.") or the model running out of budget.
pub const MIN_PARAGRAPH_LEN: usize = 40;

impl CoverPageDraft {
    /// Parse a raw model response. Returns an error if the JSON is
    /// invalid, the title or subtitle are empty, or the paragraph is
    /// shorter than [`MIN_PARAGRAPH_LEN`].
    pub fn parse(raw: &str) -> Result<Self, CoverDraftError> {
        let draft: CoverPageDraft = serde_json::from_str(raw.trim())?;
        if draft.title.trim().is_empty() {
            return Err(CoverDraftError::EmptyTitle);
        }
        if draft.subtitle.trim().is_empty() {
            return Err(CoverDraftError::EmptySubtitle);
        }
        if draft.paragraph.trim().len() < MIN_PARAGRAPH_LEN {
            return Err(CoverDraftError::ParagraphTooShort {
                min: MIN_PARAGRAPH_LEN,
                actual: draft.paragraph.trim().len(),
            });
        }
        Ok(draft)
    }

    /// Fallback used when the model is unavailable or produces an
    /// unusable response. Generates a generic but presentable cover
    /// derived from the project metadata.
    pub fn fallback(project_name: &str, client_name: &str) -> Self {
        Self {
            title: project_name.to_string(),
            subtitle: format!("Concept proposal for {client_name}"),
            paragraph: format!(
                "{project_name} is a thoughtful design that balances the everyday rhythms of {client_name} with a quietly considered material palette and a flexible plan. The pages that follow lay out the mood, the spatial moves, and the schedule that brings them to life."
            ),
            tone: Some(CoverDraftTone::Warm),
        }
    }

    /// Build the subtitle for the proposal cover. Used by aec_export.
    pub fn proposal_subtitle(&self, studio_name: &str) -> String {
        format!("{} · {}", self.subtitle, studio_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_response() {
        let raw = r#"{
            "title": "Loft 12B",
            "subtitle": "A warm home for a family of three",
            "paragraph": "Loft 12B reimagines a single-floor apartment as a quiet, light-filled retreat with rooms that flex between work and play.",
            "tone": "warm"
        }"#;
        let d = CoverPageDraft::parse(raw).unwrap();
        assert_eq!(d.title, "Loft 12B");
        assert_eq!(d.tone, Some(CoverDraftTone::Warm));
    }

    #[test]
    fn rejects_empty_title() {
        let raw = r#"{"title":"   ","subtitle":"x","paragraph":"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy"}"#;
        let err = CoverPageDraft::parse(raw).unwrap_err();
        assert!(matches!(err, CoverDraftError::EmptyTitle));
    }

    #[test]
    fn rejects_short_paragraph() {
        let raw = r#"{"title":"Loft","subtitle":"x","paragraph":"too short"}"#;
        let err = CoverPageDraft::parse(raw).unwrap_err();
        assert!(matches!(err, CoverDraftError::ParagraphTooShort { .. }));
    }

    #[test]
    fn rejects_invalid_json() {
        let err = CoverPageDraft::parse("not json").unwrap_err();
        assert!(matches!(err, CoverDraftError::Json(_)));
    }

    #[test]
    fn fallback_is_usable() {
        let d = CoverPageDraft::fallback("Loft 12B", "Eva K.");
        assert_eq!(d.title, "Loft 12B");
        assert!(d.paragraph.len() >= MIN_PARAGRAPH_LEN);
        assert_eq!(d.tone, Some(CoverDraftTone::Warm));
    }

    #[test]
    fn parse_accepts_missing_tone() {
        let raw = r#"{"title":"Loft","subtitle":"sub","paragraph":"a paragraph that is long enough to pass the length floor we set so it survives validation."}"#;
        let d = CoverPageDraft::parse(raw).unwrap();
        assert!(d.tone.is_none());
    }

    #[test]
    fn subtitle_is_studio_branded() {
        let d = CoverPageDraft::fallback("Loft 12B", "Eva K.");
        let s = d.proposal_subtitle("AEC Studio");
        assert!(s.contains("Eva K."));
        assert!(s.contains("AEC Studio"));
    }
}
