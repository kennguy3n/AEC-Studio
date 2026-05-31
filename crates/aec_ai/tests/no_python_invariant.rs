//! Phase 18 Group E Task 23 — workspace-level integration test for
//! the **no-Python-shipped-to-end-users** invariant.
//!
//! AEC Studio ships exactly one process tree on the end user's
//! machine: the Electron renderer (TypeScript → bundled to JS),
//! the Rust bridge (compiled `.node` addon), and one or more
//! **native** sidecar binaries (`llama-server` for text, and a
//! `stable-diffusion.cpp`-derived server for image-gen). No
//! Python interpreter, no `.py` script, no `pip` / `conda` /
//! `venv` artefacts, no Python quantisation runtime (`mlx-lm`,
//! `gemlite`, `HQQ`, `bitsandbytes`).
//!
//! The CI pipeline already lifts a `grep` of the source tree as
//! a no-python-runtime gate (see `.github/workflows/ci.yml`).
//! Lifting the same gate into `cargo test --workspace` means:
//!
//! 1. **Local developers** can run `cargo test no_python_invariant`
//!    before pushing — they don't have to wait for CI to notice a
//!    regression.
//! 2. **Forks / red-team / regression branches** get the same
//!    guarantee without depending on the upstream CI runner.
//! 3. **The invariant is testable**, not merely asserted-by-doc.
//!    A future maintainer who edits the registry to add a
//!    `format: "gemlite"` line, drops a `requirements.txt` into a
//!    crate, or restores the deleted `workers/blender/` worker
//!    will fail this test deterministically.
//!
//! The test walks the workspace root (computed from
//! `CARGO_MANIFEST_DIR` of this crate by going two levels up) and
//! enforces a sequence of properties, each as its own `#[test]`
//! so failures localise to a specific invariant.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Walk the workspace tree and yield every regular file path
/// **relative to the workspace root**. Skips `node_modules`,
/// `target`, `.git`, and `dist` because those contain third-party
/// or build-artefact files that the invariant does not cover —
/// installer assembly already filters these out via the
/// electron-builder `files:` allow-list.
fn workspace_files() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

fn workspace_root() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/aec_ai → crates → repo root
    crate_dir
        .parent()
        .expect("crates/aec_ai must have a parent")
        .parent()
        .expect("crates must have a parent")
        .to_path_buf()
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip build artefacts and third-party trees — the
        // invariant covers source-controlled files, not `node_modules`
        // or `target/` which inflate the walk by ~10⁵ entries each.
        if matches!(
            name.as_ref(),
            "node_modules" | "target" | ".git" | "dist" | "dist-electron"
        ) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            walk(root, &path, out);
        } else if meta.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
}

/// Allow-list of `.py` files that may live in the repo. Both are
/// build-time developer tools that run on the contributor's
/// machine and produce static PNG assets the shipped binary then
/// embeds; **neither ships in the installer**. Adding a new entry
/// here requires a corresponding electron-builder / packaging
/// audit to prove the new `.py` does not land in `dist/` /
/// `extraResources`.
const ALLOWED_PY_FILES: &[&str] = &[
    "scripts/generate_template_previews.py",
    "packaging/generate_icons.py",
];

/// Filenames / suffixes that identify a Python dependency
/// manifest. The invariant rejects any of these anywhere in the
/// workspace (other than the build-tool allow-list, which has
/// none of these files today and is checked separately).
const PYTHON_MANIFEST_FILENAMES: &[&str] = &[
    "pyproject.toml",
    "Pipfile",
    "Pipfile.lock",
    "poetry.lock",
    "setup.py",
    "setup.cfg",
    "environment.yml",
    "conda-meta",
];

/// Workers directories Phase 9 removed. The bridge replaced these
/// with native Rust crates (`aec_render::panorama`,
/// `aec_render::walkthrough`, `aec_bim::*`). The test confirms
/// they stay removed.
const FORBIDDEN_WORKERS_DIRS: &[&str] = &[
    "workers/ai",
    "workers/blender",
    "workers/ifc",
    "workers/python",
];

#[test]
fn workspace_root_resolves_to_a_real_directory() {
    // Sanity-check the env-var-based root derivation — a wrong
    // root would silently pass every other test by walking an
    // empty tree.
    let root = workspace_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "expected workspace root {root:?} to contain Cargo.toml",
    );
    assert!(
        root.join("crates").is_dir(),
        "expected workspace root {root:?} to contain crates/",
    );
    assert!(
        root.join("apps").is_dir(),
        "expected workspace root {root:?} to contain apps/",
    );
}

#[test]
fn only_build_time_python_scripts_are_allowed() {
    // The two allow-listed `.py` files are build-time tools that
    // produce static assets the binary then embeds; they do not
    // ship to end users. Every other `.py` is a regression — even
    // a one-off `experiments/test.py` would defeat the invariant
    // because the file would land in the source tarball / git
    // archive / contributor checkout.
    let files = workspace_files();
    let allowed: BTreeSet<&str> = ALLOWED_PY_FILES.iter().copied().collect();
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        if file.extension().and_then(|s| s.to_str()) == Some("py") {
            let rel = file.to_string_lossy().replace('\\', "/");
            if !allowed.contains(rel.as_str()) {
                violations.push(rel);
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — no-Python invariant violated. \
         Found .py file(s) outside the build-tool allow-list \
         ({:?}): {:#?}. Either remove the file (preferred) or add it to ALLOWED_PY_FILES \
         after auditing that it does NOT land in the shipped installer payload \
         (electron-builder `files:` / `extraResources` / `asarUnpack`).",
        ALLOWED_PY_FILES,
        violations,
    );
}

#[test]
fn build_time_python_scripts_actually_exist() {
    // Defensive: if the allow-list points at a file that has
    // already been removed, the previous test still passes (no
    // violation) but the list silently rots. This test catches
    // that — every allow-list entry must correspond to a real
    // file in the tree.
    let root = workspace_root();
    for entry in ALLOWED_PY_FILES {
        let p = root.join(entry);
        assert!(
            p.is_file(),
            "allow-list entry {entry:?} does not exist at {p:?} — \
             either remove it from ALLOWED_PY_FILES or restore the file.",
        );
    }
}

#[test]
fn no_python_dependency_manifests_in_workspace() {
    // `requirements*.txt`, `pyproject.toml`, `Pipfile`, etc. mark
    // a directory as a Python project. None of these may live in
    // the workspace because the bridge runtime has no Python
    // interpreter to consume them.
    let files = workspace_files();
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let is_requirements_txt = name.starts_with("requirements")
            && Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"));
        if is_requirements_txt {
            violations.push(file.to_string_lossy().to_string());
            continue;
        }
        if PYTHON_MANIFEST_FILENAMES.contains(&name) {
            violations.push(file.to_string_lossy().to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — found Python dependency manifest(s): {:#?}. \
         The bridge runtime has no Python interpreter; these files signal \
         a Python-runtime regression. Remove them.",
        violations,
    );
}

#[test]
fn no_forbidden_workers_directories() {
    // Phase 9 removed `workers/ai/`, `workers/blender/`,
    // `workers/ifc/` in favour of in-process Rust. This test
    // pins their continued absence — restoring any of these dirs
    // implies the bridge has reverted to spawning Python workers
    // for rendering / BIM / AI, which is the exact regression
    // class the no-Python invariant guards against.
    let root = workspace_root();
    let mut violations: Vec<String> = Vec::new();
    for dir in FORBIDDEN_WORKERS_DIRS {
        if root.join(dir).is_dir() {
            violations.push((*dir).to_string());
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — forbidden Python-worker directories \
         have reappeared: {:#?}. The bridge runtime is fully native; any \
         workers/ subtree implies a regression to spawning Python workers.",
        violations,
    );
}

#[test]
fn no_python_runtime_dependencies_in_cargo_manifests() {
    // Walk every `Cargo.toml` in the workspace and reject any
    // dependency whose name implies a Python interop runtime.
    // The list below is the union of every Python-from-Rust
    // binding crate that publishes to crates.io as of writing —
    // adding a new variant is intentional and forces a review.
    let banned_crates = [
        "pyo3",
        "rustpython",
        "python3-sys",
        "cpython",
        "inline-python",
        "pyembed",
        "mlx-rs",
        "mlx-rust",
        "gemlite",
        "hqq",
        "hqq-rs",
        "bitsandbytes",
    ];
    let files = workspace_files();
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        if file.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml") {
            let abs = workspace_root().join(file);
            let Ok(text) = fs::read_to_string(&abs) else {
                continue;
            };
            for banned in &banned_crates {
                // Match `name = "..."`, `name = { ... }`, or a
                // bare `name =` on its own line — the goal is to
                // catch dependency declarations without
                // false-positiving on doc strings that mention
                // the crate by name.  Each dependency is its own
                // TOML key, so `^\s*<name>\s*=` is the canonical
                // shape.
                for line in text.lines() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with(&format!("{banned} ="))
                        || trimmed.starts_with(&format!("{banned}="))
                        || trimmed.starts_with(&format!("\"{banned}\" ="))
                    {
                        violations.push(format!(
                            "{file:?} declares forbidden dependency `{banned}`: {line:?}",
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — Cargo.toml(s) declare Python-runtime \
         dependencies: {violations:#?}. The bridge runtime is fully \
         native; adding pyo3 / mlx-rs / gemlite etc. is a no-Python \
         regression.",
    );
}

#[test]
fn no_python_runtime_dependencies_in_package_json() {
    // Same shape as the Cargo.toml test, applied to npm
    // manifests. The renderer is a TypeScript app that should
    // not pull in a Python interop layer (`python-shell`,
    // `pyodide` would each break the no-Python invariant: the
    // former spawns an interpreter, the latter embeds one in
    // WebAssembly which still counts as shipping Python to the
    // user's runtime).
    let banned_packages = [
        "python-shell",
        "pyodide",
        "pyodide-js",
        "mlx-lm",
        "gemlite",
        "hqq",
    ];
    let files = workspace_files();
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        if file.file_name().and_then(|s| s.to_str()) == Some("package.json") {
            // Skip lockfiles (handled by their own check) and
            // node_modules (already excluded by `walk`).
            let abs = workspace_root().join(file);
            let Ok(text) = fs::read_to_string(&abs) else {
                continue;
            };
            for banned in &banned_packages {
                // npm dep is always `"name": "..."` on its own
                // JSON property line. The leading `"` + colon
                // shape avoids false-positives on doc strings.
                let needle = format!("\"{banned}\":");
                if text.contains(&needle) {
                    violations.push(format!(
                        "{file:?} declares forbidden npm dependency `{banned}`",
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — package.json(s) declare Python-runtime \
         dependencies: {violations:#?}.",
    );
}

#[test]
fn electron_builder_configs_have_no_python_references() {
    // Audit every electron-builder YAML for the strings that
    // would cause a Python file to land in the installer. The
    // `files:` / `extraResources:` / `asarUnpack:` keys are the
    // only places electron-builder lets you ship arbitrary
    // resources; the invariant rejects any `*.py` glob, `python`
    // / `mlx` / `gemlite` substring in the entire YAML.
    let root = workspace_root();
    let configs = [
        "packaging/macos/electron-builder.macos.yml",
        "packaging/linux/electron-builder.linux.yml",
        "packaging/windows/electron-builder.windows.yml",
    ];
    let banned_substrings = [
        ".py", // catches `*.py`, `**/*.py`
        "python",
        "Python",
        "PYTHON",
        "mlx-lm",
        "mlx_lm",
        "gemlite",
        "GEMLITE",
        "pyo3",
        "PyO3",
        "pyodide",
        "Pyodide",
        "HQQAdapter",
        "MLXAdapter",
        "conda",
    ];
    let mut violations: Vec<String> = Vec::new();
    for cfg in &configs {
        let p = root.join(cfg);
        assert!(p.is_file(), "expected electron-builder config at {p:?}");
        let text = fs::read_to_string(&p).expect("electron-builder config must be readable");
        for banned in &banned_substrings {
            if text.contains(banned) {
                violations.push(format!(
                    "{cfg:?} contains forbidden substring {banned:?} — \
                     electron-builder may ship a Python artefact",
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Phase 18 Group E Task 23 — electron-builder configs reference \
         Python / MLX / gemlite tooling: {violations:#?}. Remove the \
         reference or the installer will ship a Python interpreter.",
    );
}

#[test]
fn shipped_registry_passes_no_python_validation_at_runtime() {
    // Integration twin of the unit test in `registry::tests`.
    // The shipped `ai_models.json` (compiled in via
    // `include_str!`) must pass the boot-time no-Python check.
    // A regression here means a future maintainer edited the
    // JSON to point at a Python-only model (gemlite / mlx) and
    // the change slipped past the unit test — which it
    // shouldn't, but the integration coverage makes the
    // invariant load-bearing across the workspace.
    use aec_ai::registry::ModelRegistry;
    let r = ModelRegistry::embedded();
    r.validate_at_boot()
        .expect("shipped ai_models.json must pass no-Python boot validation");
}
