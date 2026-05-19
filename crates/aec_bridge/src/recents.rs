//! Thin wrapper around [`aec_core::package::RecentsStore`] that fronts a
//! single, lazily-initialised store at a configurable path.

use std::path::PathBuf;

use thiserror::Error;

use aec_core::package::{RecentEntry, RecentsStore as CoreRecentsStore};
use aec_core::types::ProjectId;

#[derive(Debug, Error)]
pub enum RecentsStoreError {
    #[error("recents: {0}")]
    Recents(String),
}

impl From<aec_core::error::AecError> for RecentsStoreError {
    fn from(e: aec_core::error::AecError) -> Self {
        Self::Recents(e.to_string())
    }
}

/// Recents store for the bridge. Wraps the core implementation and keeps
/// the underlying file path so the bridge can re-open between calls.
pub struct RecentsStore {
    path: PathBuf,
    max_entries: usize,
    inner: CoreRecentsStore,
}

impl RecentsStore {
    pub fn open(path: impl Into<PathBuf>, max_entries: usize) -> Result<Self, RecentsStoreError> {
        let path = path.into();
        let inner = CoreRecentsStore::new(path.clone(), max_entries)?;
        Ok(Self {
            path,
            max_entries,
            inner,
        })
    }

    pub fn entries(&self) -> &[RecentEntry] {
        self.inner.entries()
    }

    pub fn record(
        &mut self,
        summary: &aec_core::package::ProjectSummary,
    ) -> Result<(), RecentsStoreError> {
        self.inner.record(summary)?;
        Ok(())
    }

    pub fn forget(&mut self, project_id: &ProjectId) -> Result<(), RecentsStoreError> {
        self.inner.forget(project_id)?;
        Ok(())
    }

    /// Re-read the store from disk. Useful after another process (CLI,
    /// test) has mutated the file.
    pub fn reload(&mut self) -> Result<(), RecentsStoreError> {
        self.inner = CoreRecentsStore::new(self.path.clone(), self.max_entries)?;
        Ok(())
    }
}
