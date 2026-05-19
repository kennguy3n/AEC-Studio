//! The on-disk `.aecstudio` project package format.
//!
//! Layout (kept in sync with ARCHITECTURE.md § "Project file structure"):
//!
//! ```text
//! project.aecstudio/
//! ├── manifest.json
//! ├── project.sqlite          (SQLCipher-encrypted)
//! ├── project.nonce           (32-byte project key nonce; deletes = forget)
//! ├── commands/               (append-only command log, one .jsonl per day)
//! ├── checkpoints/
//! ├── geometry/               (content-addressed mesh blobs)
//! ├── materials/
//! ├── assets/
//! ├── sheets/
//! ├── bim/
//! ├── renders/
//! ├── revisions/
//! ├── audit/
//! ├── ai/
//! └── exports/
//! ```

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::ProjectSettings;
use crate::crypto::{derive_project_key, generate_project_nonce, Key32};
use crate::db;
use crate::error::{AecError, AecResult};
use crate::manifest::ProjectManifest;
use crate::types::ProjectId;

/// Sub-directories that must exist inside a valid `.aecstudio` package.
pub const PACKAGE_DIRS: &[&str] = &[
    "commands",
    "checkpoints",
    "geometry",
    "materials",
    "assets",
    "sheets",
    "bim",
    "renders",
    "revisions",
    "audit",
    "ai",
    "exports",
];

/// A handle to an on-disk project package.
#[derive(Debug)]
pub struct ProjectPackage {
    root: PathBuf,
    manifest: ProjectManifest,
}

impl ProjectPackage {
    /// Create a new package on disk at `root`. Fails if the path already
    /// exists.
    pub fn create(
        root: impl AsRef<Path>,
        name: impl Into<String>,
        settings: ProjectSettings,
        template_id: Option<String>,
        master_key: &[u8; 32],
    ) -> AecResult<Self> {
        let root = root.as_ref().to_path_buf();
        if root.exists() {
            return Err(AecError::AlreadyExists(root.display().to_string()));
        }
        fs::create_dir_all(&root)?;
        for d in PACKAGE_DIRS {
            fs::create_dir_all(root.join(d))?;
        }

        // Write the nonce file, derive the encryption key, and create the
        // encrypted SQLite database. The nonce file is plaintext on
        // purpose: deleting it is the crypto-forget gesture.
        let nonce = generate_project_nonce()?;
        let nonce_path = root.join("project.nonce");
        fs::write(&nonce_path, nonce)?;
        let key = derive_project_key(master_key, &nonce);
        let db_path = root.join("project.sqlite");
        let _conn = db::open_encrypted(&db_path, &key)?;
        // Drop the connection — callers re-open through `open` when they
        // need to write.

        let manifest = ProjectManifest::new(ProjectId::new(), name, settings, template_id);
        let pkg = Self { root, manifest };
        pkg.write_manifest()?;
        Ok(pkg)
    }

    /// Open an existing package. Returns the loaded manifest; the database
    /// is opened on demand via [`Self::open_database`].
    pub fn open(root: impl AsRef<Path>) -> AecResult<Self> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() {
            return Err(AecError::InvalidPackage {
                path: root.display().to_string(),
                reason: "not a directory".into(),
            });
        }
        for d in PACKAGE_DIRS {
            if !root.join(d).is_dir() {
                return Err(AecError::InvalidPackage {
                    path: root.display().to_string(),
                    reason: format!("missing subdirectory `{d}`"),
                });
            }
        }
        let manifest_path = root.join("manifest.json");
        let manifest_json = fs::read_to_string(&manifest_path)?;
        let manifest: ProjectManifest = serde_json::from_str(&manifest_json)?;
        manifest.validate()?;
        Ok(Self { root, manifest })
    }

    /// Open the encrypted SQLite database, deriving the key from the
    /// per-project nonce.
    pub fn open_database(&self, master_key: &[u8; 32]) -> AecResult<rusqlite::Connection> {
        let key = self.derive_key(master_key)?;
        db::open_existing(&self.root.join("project.sqlite"), &key)
    }

    pub fn derive_key(&self, master_key: &[u8; 32]) -> AecResult<Key32> {
        let nonce_path = self.root.join("project.nonce");
        let nonce = fs::read(&nonce_path)?;
        if nonce.is_empty() {
            return Err(AecError::InvalidKey("project nonce file is empty".into()));
        }
        Ok(derive_project_key(master_key, &nonce))
    }

    pub fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Touch the manifest's `updated_at` and persist.
    pub fn save(&mut self) -> AecResult<()> {
        self.manifest.touch();
        self.write_manifest()
    }

    fn write_manifest(&self) -> AecResult<()> {
        let path = self.root.join("manifest.json");
        let json = serde_json::to_vec_pretty(&self.manifest)?;
        let mut f = fs::File::create(&path)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        Ok(())
    }

    /// Append a JSON-line command record to today's append-only log.
    pub fn append_command_log(&self, line: &serde_json::Value) -> AecResult<()> {
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let file = self.root.join("commands").join(format!("{date}.jsonl"));
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        let serialized = serde_json::to_string(line)?;
        f.write_all(serialized.as_bytes())?;
        f.write_all(b"\n")?;
        Ok(())
    }

    pub fn summary(&self) -> ProjectSummary {
        ProjectSummary {
            project_id: self.manifest.project_id.clone(),
            name: self.manifest.name.clone(),
            path: self.root.display().to_string(),
            updated_at: self.manifest.updated_at,
            template_id: self.manifest.template_id.clone(),
        }
    }
}

/// Lightweight project descriptor used by Home page and the recents store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub name: String,
    pub path: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
}

/// A recents-store entry, persisted in a single JSON file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub project_id: ProjectId,
    pub name: String,
    pub path: String,
    pub last_opened_at: DateTime<Utc>,
}

/// JSON-file-backed recents store. Keeps the last `max_entries` projects.
#[derive(Debug)]
pub struct RecentsStore {
    path: PathBuf,
    max_entries: usize,
    entries: Vec<RecentEntry>,
}

impl RecentsStore {
    pub fn new(path: impl Into<PathBuf>, max_entries: usize) -> AecResult<Self> {
        let path = path.into();
        let entries = if path.exists() {
            let raw = fs::read_to_string(&path)?;
            let parsed: Vec<RecentEntry> =
                serde_json::from_str(&raw).map_err(|e| AecError::CorruptRecents(e.to_string()))?;
            parsed
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            max_entries,
            entries,
        })
    }

    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }

    pub fn record(&mut self, summary: &ProjectSummary) -> AecResult<()> {
        self.entries.retain(|e| e.project_id != summary.project_id);
        self.entries.insert(
            0,
            RecentEntry {
                project_id: summary.project_id.clone(),
                name: summary.name.clone(),
                path: summary.path.clone(),
                last_opened_at: Utc::now(),
            },
        );
        if self.entries.len() > self.max_entries {
            self.entries.truncate(self.max_entries);
        }
        self.flush()
    }

    pub fn forget(&mut self, project_id: &ProjectId) -> AecResult<()> {
        self.entries.retain(|e| &e.project_id != project_id);
        self.flush()
    }

    fn flush(&self) -> AecResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&self.entries)?;
        fs::write(&self.path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Region;

    fn make_pkg(td: &tempfile::TempDir) -> (PathBuf, [u8; 32]) {
        let path = td.path().join("apartment.aecstudio");
        let master = [99u8; 32];
        let pkg = ProjectPackage::create(
            &path,
            "Apartment",
            ProjectSettings::from_region(Region::Eu),
            Some("interior.apartment".into()),
            &master,
        )
        .unwrap();
        assert!(path.is_dir());
        for d in PACKAGE_DIRS {
            assert!(pkg.root().join(d).is_dir(), "missing dir {d}");
        }
        (path, master)
    }

    #[test]
    fn create_then_open_roundtrip() {
        let td = tempfile::tempdir().unwrap();
        let (path, master) = make_pkg(&td);
        let pkg = ProjectPackage::open(&path).unwrap();
        assert_eq!(pkg.manifest().name, "Apartment");
        let _conn = pkg.open_database(&master).unwrap();
    }

    #[test]
    fn refuse_duplicate_create() {
        let td = tempfile::tempdir().unwrap();
        let (path, master) = make_pkg(&td);
        let err = ProjectPackage::create(
            &path,
            "Apartment 2",
            ProjectSettings::default(),
            None,
            &master,
        )
        .unwrap_err();
        matches!(err, AecError::AlreadyExists(_));
    }

    #[test]
    fn deleting_nonce_locks_out_the_database() {
        let td = tempfile::tempdir().unwrap();
        let (path, master) = make_pkg(&td);
        // Replace the nonce with bogus bytes → key derivation produces a
        // different key → SQLCipher refuses to open.
        let nonce_path = path.join("project.nonce");
        std::fs::write(&nonce_path, vec![0u8; 32]).unwrap();
        let pkg = ProjectPackage::open(&path).unwrap();
        let err = pkg.open_database(&master);
        assert!(err.is_err(), "tampered nonce must not yield a working DB");
    }

    #[test]
    fn save_updates_manifest_timestamp() {
        let td = tempfile::tempdir().unwrap();
        let (path, _master) = make_pkg(&td);
        let mut pkg = ProjectPackage::open(&path).unwrap();
        let before = pkg.manifest().updated_at;
        std::thread::sleep(std::time::Duration::from_millis(5));
        pkg.save().unwrap();
        assert!(pkg.manifest().updated_at > before);
    }

    #[test]
    fn command_log_is_append_only() {
        let td = tempfile::tempdir().unwrap();
        let (path, _master) = make_pkg(&td);
        let pkg = ProjectPackage::open(&path).unwrap();
        pkg.append_command_log(&serde_json::json!({"cmd": 1}))
            .unwrap();
        pkg.append_command_log(&serde_json::json!({"cmd": 2}))
            .unwrap();
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let logfile = path.join("commands").join(format!("{date}.jsonl"));
        let raw = fs::read_to_string(logfile).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn recents_store_records_most_recent_first() {
        let td = tempfile::tempdir().unwrap();
        let recents_path = td.path().join("recents.json");
        let mut store = RecentsStore::new(&recents_path, 5).unwrap();
        let s1 = ProjectSummary {
            project_id: ProjectId::new(),
            name: "A".into(),
            path: "/a".into(),
            updated_at: Utc::now(),
            template_id: None,
        };
        let s2 = ProjectSummary {
            project_id: ProjectId::new(),
            name: "B".into(),
            path: "/b".into(),
            updated_at: Utc::now(),
            template_id: None,
        };
        store.record(&s1).unwrap();
        store.record(&s2).unwrap();
        assert_eq!(store.entries()[0].name, "B");
        assert_eq!(store.entries()[1].name, "A");

        // Re-opening picks up the persisted entries.
        let reopened = RecentsStore::new(&recents_path, 5).unwrap();
        assert_eq!(reopened.entries().len(), 2);
    }

    #[test]
    fn recents_store_enforces_max_entries() {
        let td = tempfile::tempdir().unwrap();
        let mut store = RecentsStore::new(td.path().join("recents.json"), 2).unwrap();
        for i in 0..5 {
            store
                .record(&ProjectSummary {
                    project_id: ProjectId::new(),
                    name: format!("P{i}"),
                    path: format!("/{i}"),
                    updated_at: Utc::now(),
                    template_id: None,
                })
                .unwrap();
        }
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.entries()[0].name, "P4");
        assert_eq!(store.entries()[1].name, "P3");
    }
}
