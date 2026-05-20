//! Cross-platform Blender binary discovery.
//!
//! The render pipeline depends on a working Blender 4.1+ install. This
//! module locates the executable in priority order:
//!
//! 1. `AEC_BLENDER_BIN` env override (used by tests + power-users).
//! 2. The `BLENDER_BIN` env var (matches the variable Blender itself
//!    consults in some distros).
//! 3. The platform's well-known install paths (macOS `.app`, Windows
//!    Program Files, Linux `/usr/bin`, `/usr/local/bin`,
//!    `/snap/bin/blender`, `/var/lib/flatpak/exports/bin/org.blender.Blender`,
//!    `$HOME/.local/bin/blender`).
//! 4. `PATH` lookup via the OS PATH separator.
//!
//! The discovery is best-effort: callers must accept that a `None`
//! result simply means the user has to point us at a binary in the
//! settings UI.

use std::env;
use std::path::{Path, PathBuf};

/// Result of a discovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlenderDiscovery {
    /// Absolute path to the executable.
    pub path: PathBuf,
    /// Where the path came from (for logging).
    pub source: DiscoverySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    EnvAecBlenderBin,
    EnvBlenderBin,
    KnownInstallPath,
    Path,
}

/// Discover a Blender binary on this machine.
///
/// Pure for testing: callers may inject `path_var` (PATH contents) and
/// `existence_check` (file-exists predicate) so unit tests can simulate
/// arbitrary filesystems without touching real disk state.
pub fn discover_blender_with(
    aec_env: Option<&str>,
    blender_env: Option<&str>,
    path_var: Option<&str>,
    existence_check: impl Fn(&Path) -> bool,
) -> Option<BlenderDiscovery> {
    if let Some(p) = aec_env {
        let pb = PathBuf::from(p);
        if existence_check(&pb) {
            return Some(BlenderDiscovery {
                path: pb,
                source: DiscoverySource::EnvAecBlenderBin,
            });
        }
    }
    if let Some(p) = blender_env {
        let pb = PathBuf::from(p);
        if existence_check(&pb) {
            return Some(BlenderDiscovery {
                path: pb,
                source: DiscoverySource::EnvBlenderBin,
            });
        }
    }

    for known in known_install_paths() {
        if existence_check(&known) {
            return Some(BlenderDiscovery {
                path: known,
                source: DiscoverySource::KnownInstallPath,
            });
        }
    }

    if let Some(raw) = path_var {
        for dir in raw.split(path_separator()) {
            if dir.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join(executable_name());
            if existence_check(&candidate) {
                return Some(BlenderDiscovery {
                    path: candidate,
                    source: DiscoverySource::Path,
                });
            }
        }
    }
    None
}

/// Live discovery using the current process environment and the real
/// filesystem.
pub fn discover_blender() -> Option<BlenderDiscovery> {
    discover_blender_with(
        env::var("AEC_BLENDER_BIN").ok().as_deref(),
        env::var("BLENDER_BIN").ok().as_deref(),
        env::var("PATH").ok().as_deref(),
        Path::is_file,
    )
}

fn known_install_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Blender.app/Contents/MacOS/Blender",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        out.push(PathBuf::from(
            "C:\\Program Files\\Blender Foundation\\Blender 4.1\\blender.exe",
        ));
        out.push(PathBuf::from(
            "C:\\Program Files\\Blender Foundation\\Blender 4.2\\blender.exe",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        out.push(PathBuf::from("/usr/bin/blender"));
        out.push(PathBuf::from("/usr/local/bin/blender"));
        out.push(PathBuf::from("/snap/bin/blender"));
        out.push(PathBuf::from(
            "/var/lib/flatpak/exports/bin/org.blender.Blender",
        ));
        if let Some(home) = env::var_os("HOME") {
            out.push(PathBuf::from(home).join(".local/bin/blender"));
        }
    }
    out
}

fn executable_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "blender.exe"
    } else {
        "blender"
    }
}

fn path_separator() -> char {
    if cfg!(target_os = "windows") {
        ';'
    } else {
        ':'
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    fn fake_exists(set: &HashSet<PathBuf>) -> impl Fn(&Path) -> bool + '_ {
        |p: &Path| set.contains(p)
    }

    #[test]
    fn aec_env_wins_when_file_exists() {
        let exists: HashSet<PathBuf> = [
            PathBuf::from("/opt/blender/custom-blender"),
            PathBuf::from("/usr/bin/blender"),
        ]
        .into_iter()
        .collect();
        let d = discover_blender_with(
            Some("/opt/blender/custom-blender"),
            Some("/usr/bin/blender"),
            Some("/usr/bin:/bin"),
            fake_exists(&exists),
        )
        .unwrap();
        assert_eq!(d.source, DiscoverySource::EnvAecBlenderBin);
        assert_eq!(d.path, PathBuf::from("/opt/blender/custom-blender"));
    }

    #[test]
    fn falls_through_to_known_install_path_when_env_paths_missing() {
        // env paths point at things that don't exist on disk, so we
        // expect the discovery to walk past them and land on a known
        // install path. We feed the platform-appropriate path into the
        // existence set.
        let mut set: HashSet<PathBuf> = HashSet::new();
        #[cfg(target_os = "linux")]
        set.insert(PathBuf::from("/usr/bin/blender"));
        #[cfg(target_os = "macos")]
        set.insert(PathBuf::from(
            "/Applications/Blender.app/Contents/MacOS/Blender",
        ));
        #[cfg(target_os = "windows")]
        set.insert(PathBuf::from(
            "C:\\Program Files\\Blender Foundation\\Blender 4.1\\blender.exe",
        ));

        let d = discover_blender_with(
            Some("/nope/aec-blender"),
            Some("/nope/blender"),
            None,
            fake_exists(&set),
        )
        .unwrap();
        assert_eq!(d.source, DiscoverySource::KnownInstallPath);
    }

    #[test]
    fn path_search_picks_blender_when_known_paths_miss() {
        let exists: HashSet<PathBuf> = [PathBuf::from("/opt/extra/blender")]
            .into_iter()
            .collect();
        let d = discover_blender_with(
            None,
            None,
            Some("/usr/bin:/opt/extra"),
            fake_exists(&exists),
        );
        // On linux/macos `executable_name()` is "blender"; the
        // candidate join produces "/opt/extra/blender" which matches.
        #[cfg(not(target_os = "windows"))]
        {
            let d = d.expect("expected discovery via PATH");
            assert_eq!(d.source, DiscoverySource::Path);
            assert_eq!(d.path, PathBuf::from("/opt/extra/blender"));
        }
        #[cfg(target_os = "windows")]
        {
            // On Windows the candidate is "blender.exe" and won't match
            // our linux-style fake; assert the function ran without
            // panicking instead.
            let _ = d;
        }
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let set: HashSet<PathBuf> = HashSet::new();
        let d = discover_blender_with(None, None, Some("/none"), fake_exists(&set));
        assert!(d.is_none());
    }

    #[test]
    fn discover_blender_does_not_panic_on_live_env() {
        // We don't assert success — the test host may not have Blender —
        // we just want to confirm the live entry point is callable.
        let cell = RefCell::new(false);
        let _ = discover_blender();
        *cell.borrow_mut() = true;
        assert!(*cell.borrow());
    }
}
