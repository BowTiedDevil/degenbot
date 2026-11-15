//! Where the committed fixtures the instruments read live, resolved from a marker
//! instead of `..` arithmetic.
//!
//! An instrument that names a committed artifact as `env!("CARGO_MANIFEST_DIR")`
//! plus a chain of `..` segments is asserting where this package sits in the
//! tree, and that is the one claim a move silently invalidates: this package
//! sits at `rust/tools/<package>`, two components shallower than
//! `rust/crates/<role>/<crate>`, so the same expression still names an existing
//! directory and every read lands somewhere else without failing.
//!
//! The root is therefore resolved from markers the repository actually carries
//! ([`REPO_ROOT_MARKERS`]), and every corpus path is derived from it. A package
//! that is moved or renamed fails loudly at the resolver instead of reading a
//! stranger's directory.

use std::path::{Path, PathBuf};

/// Repository-root markers. A directory is the repository root only if it
/// carries **both** of these; a loose single-file check would accept a bare
/// `justfile` or an unrelated `rust/Cargo.toml` copy higher up the tree.
pub const REPO_ROOT_MARKERS: [&str; 2] = ["justfile", "rust/Cargo.toml"];

/// Walk `start` and its ancestors, returning the nearest directory that carries
/// every [`REPO_ROOT_MARKERS`] entry.
///
/// The *nearest* match wins: a nested checkout inside another checkout resolves
/// to the inner one, which is the tree the running binary was built from.
///
/// # Errors
///
/// Returns a message naming the starting directory and the markers when no
/// ancestor carries them -- never a fallback guess, because a guessed root
/// reads fixtures out of the wrong tree.
pub fn repo_root_from(start: &Path) -> Result<PathBuf, String> {
    for candidate in start.ancestors() {
        if REPO_ROOT_MARKERS
            .iter()
            .all(|marker| candidate.join(marker).exists())
        {
            return Ok(candidate.to_path_buf());
        }
    }
    Err(format!(
        "no repository root above {} (looking for {})",
        start.display(),
        REPO_ROOT_MARKERS.join(" + ")
    ))
}

/// [`repo_root_from`] applied to this package's manifest directory.
///
/// # Errors
///
/// Propagates [`repo_root_from`]'s error: a package built outside a repository
/// checkout (a packaged `.crate`, a copied directory) has no resolvable root.
pub fn repo_root() -> Result<PathBuf, String> {
    repo_root_from(Path::new(env!("CARGO_MANIFEST_DIR")))
}

#[cfg(test)]
mod tests {
    // A test module may fail loudly; the library's own code may not.
    #![expect(clippy::expect_used)]

    use super::{repo_root, repo_root_from, REPO_ROOT_MARKERS};

    /// The corpus support reads these two committed artifacts by
    /// repository-relative path, so the resolved root is only correct if they
    /// are both under it.
    const CORPUS_ARTIFACTS: [&str; 2] = [
        "tier3-oracle/artifacts/harness/Token.json",
        "rust/crates/foundation/degenbot-executor/releases/v2/artifacts/cmd_executor.creation.hex",
    ];

    #[test]
    fn resolved_root_carries_the_committed_corpus_artifacts() {
        let root = repo_root().expect("repository root");
        assert!(root.join("justfile").is_file(), "{}", root.display());
        for artifact in CORPUS_ARTIFACTS {
            assert!(root.join(artifact).is_file(), "{}", root.display());
        }
    }

    /// A directory with no marker-bearing ancestor fails instead of guessing.
    #[test]
    fn unresolvable_root_is_an_error_naming_the_markers() {
        let scratch = std::env::temp_dir().join("degenbot-engine-lab-paths-unresolvable");
        let err = repo_root_from(&scratch).expect_err("no markers above a temp dir");
        for marker in REPO_ROOT_MARKERS {
            assert!(err.contains(marker), "{err}");
        }
    }
}
