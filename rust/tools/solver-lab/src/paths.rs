//! Where the committed corpora live, resolved from a marker instead of `..` arithmetic.
//!
//! `synth_corpus_gen` writes into the *pools* crate's committed fixture corpus, which is
//! read back by that crate's `profit_envelope` kernel tests. Naming it as
//! `env!("CARGO_MANIFEST_DIR")` plus a chain of `..` segments is a claim about where this
//! package sits in the tree, and it is the one claim a move silently invalidates: this
//! package sits at `rust/tools/<package>`, two components shallower than
//! `rust/crates/<role>/<crate>`, so the same expression still names an existing directory
//! and every write lands somewhere else without failing.
//!
//! The root is therefore resolved from markers the repository actually carries
//! ([`REPO_ROOT_MARKERS`]), and the corpus home is derived from that root
//! ([`pools_fixtures_dir`]). A package that is moved or renamed fails loudly at the
//! resolver instead of writing into a stranger's directory.

use std::path::{Path, PathBuf};

/// Repository-root markers. A directory is the repository root only if it carries
/// **both** of these; a loose single-file check would accept a bare `justfile` or an
/// unrelated `rust/Cargo.toml` copy higher up the tree.
pub const REPO_ROOT_MARKERS: [&str; 2] = ["justfile", "rust/Cargo.toml"];

/// The pools crate's committed fixture corpus, repository-relative.
pub const POOLS_FIXTURES_HOME: &str = "rust/crates/foundation/degenbot-pools/tests/fixtures";

/// The reviewed concentrated-liquidity walk-count budget artifact.
pub const WALK_BUDGET_PATH: &str =
    "rust/tools/solver-lab/budgets/concentrated_liquidity_walk_counts.json";

/// Walk `start` and its ancestors, returning the nearest directory that carries every
/// [`REPO_ROOT_MARKERS`] entry.
///
/// The *nearest* match wins: a nested checkout inside another checkout resolves to the
/// inner one, which is the tree the running binary was built from.
///
/// # Errors
///
/// Returns a message naming the starting directory and the markers when no ancestor
/// carries them — never a fallback guess, because a guessed root writes fixtures into
/// the wrong tree.
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

/// The pools crate's committed fixture corpus: [`POOLS_FIXTURES_HOME`] under the resolved
/// repository root.
///
/// # Errors
///
/// Propagates [`repo_root`]'s error.
pub fn pools_fixtures_dir() -> Result<PathBuf, String> {
    Ok(repo_root()?.join(POOLS_FIXTURES_HOME))
}

/// Resolve the walk-count budget from an arbitrary checkout-relative directory.
///
/// # Errors
///
/// Propagates [`repo_root_from`]'s missing-marker error.
pub fn walk_budget_path_from(start: &Path) -> Result<PathBuf, String> {
    Ok(repo_root_from(start)?.join(WALK_BUDGET_PATH))
}

/// Resolve the committed walk-count budget independently of the process cwd.
///
/// # Errors
///
/// Propagates [`repo_root`]'s missing-marker error.
pub fn walk_budget_path() -> Result<PathBuf, String> {
    Ok(repo_root()?.join(WALK_BUDGET_PATH))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn walk_budget_resolution_is_independent_of_root_or_rust_cwd() {
        let root = repo_root().unwrap();
        let expected = root.join(WALK_BUDGET_PATH);
        assert_eq!(walk_budget_path_from(&root).unwrap(), expected);
        assert_eq!(walk_budget_path_from(&root.join("rust")).unwrap(), expected);
        assert_eq!(walk_budget_path().unwrap(), expected);
    }
}
