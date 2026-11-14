//! Where the committed fixture corpus is, resolved once instead of guessed.
//!
//! ## The hazard this module exists to close
//!
//! Every program in this package reads or writes the committed corpus, and the
//! obvious way to name it is `env!("CARGO_MANIFEST_DIR").join("../../../../tests/fixtures")`.
//! That expression is a *claim about where the package sits in the tree*, and it was
//! true only for the program's old home (`rust/crates/<role>/<crate>`, four components
//! up). This package sits at `rust/tools/<package>`, two components up: the same
//! expression names a directory that exists — so nothing fails — and every read or
//! write lands somewhere else. That is the silent half of the move hazard, and it is
//! why the anchor is resolved from a marker the repository root actually carries
//! ([`REPO_ROOT_MARKERS`]) rather than counted in `..` segments.
//!
//! ## The two ways a program names a home
//!
//! - **Derived:** [`fixtures_home`] resolves the repository root from this package's
//!   manifest directory and appends [`FIXTURES_HOME`]. No program spells `..`
//!   arithmetic; a program that is later moved (or a package that is renamed) fails
//!   loudly at the resolver instead of writing into a stranger's directory.
//! - **Explicit:** [`fixtures_home_from_args`] honors a `--fixtures-home <dir>` override.
//!   A drift gate needs it: proving that `--check` goes red must never touch the
//!   committed corpus, so the proof runs the same binary against a copied-then-mutated
//!   tree. The override re-points the homes only; nothing about the comparison changes.

use std::path::{Path, PathBuf};

/// Repository-root markers. A directory is the repository root only if it carries
/// **both** of these; a loose single-file check would accept a bare `justfile` or an
/// unrelated `rust/Cargo.toml` copy higher up the tree.
pub const REPO_ROOT_MARKERS: [&str; 2] = ["justfile", "rust/Cargo.toml"];

/// The repository-relative home the committed fixture corpus lives under.
pub const FIXTURES_HOME: &str = "tests/fixtures";

/// The command-line flag that overrides [`fixtures_home`].
pub const FIXTURES_HOME_FLAG: &str = "--fixtures-home";

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

/// Resolve one repository-relative path (e.g. `tier3-oracle/artifacts`) against the
/// resolved root.
///
/// # Errors
///
/// Propagates [`repo_root`]'s error.
pub fn repo_relative(relative: &str) -> Result<PathBuf, String> {
    Ok(repo_root()?.join(relative))
}

/// The committed fixture corpus root: `<repo root>/tests/fixtures`.
///
/// # Errors
///
/// Propagates [`repo_root`]'s error.
pub fn fixtures_home() -> Result<PathBuf, String> {
    repo_relative(FIXTURES_HOME)
}

/// The effective fixture corpus root for one invocation: the `--fixtures-home <dir>`
/// override when the arguments carry one, [`fixtures_home`] otherwise.
///
/// Both spellings are accepted (`--fixtures-home <dir>` and `--fixtures-home=<dir>`).
/// Every other argument is ignored here — the programs keep their own argv handling.
///
/// # Errors
///
/// Returns a message when the flag is present without a value, with an empty value
/// (`--fixtures-home=`, or an empty separated argument), or with another flag as its
/// value (`--fixtures-home --check`, `--fixtures-home=--check`): the override is a
/// directory, never a positional. Reporting that as an error rather than accepting it
/// matters — a silently swallowed `--check` would run the drift gate against the
/// committed corpus while the operator believed it pointed elsewhere. Propagates
/// [`fixtures_home`]'s error when no override is given.
pub fn fixtures_home_from_args(args: &[String]) -> Result<PathBuf, String> {
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == FIXTURES_HOME_FLAG {
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("{FIXTURES_HOME_FLAG} needs a directory argument"))?;
            return override_directory(value);
        }
        if let Some(value) = arg.strip_prefix(&format!("{FIXTURES_HOME_FLAG}=")) {
            return override_directory(value);
        }
        index += 1;
    }
    fixtures_home()
}

/// Accept one `--fixtures-home` value only if it can name a directory.
///
/// A flag-shaped value means the operator forgot the argument (`--fixtures-home
/// --check`), so it is rejected instead of becoming a relative directory literally
/// named `--check`; an empty value is the same mistake spelled with nothing at all.
fn override_directory(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!("{FIXTURES_HOME_FLAG} needs a directory argument"));
    }
    if value.starts_with("--") {
        return Err(format!(
            "{FIXTURES_HOME_FLAG} needs a directory argument, but got the flag {value}"
        ));
    }
    Ok(PathBuf::from(value))
}

/// Compare produced artifact bytes with the committed files `checks` names, printing
/// the same `DRIFT:` / `MISSING:` lines the drift gates have always printed, and return
/// the number of artifacts that do not match.
///
/// Read-only: a `--check` run must never write to the corpus, so the only side effect
/// here is stderr. The caller turns a non-zero return into a failing exit code.
#[must_use]
#[expect(
    clippy::print_stderr,
    reason = "the drift report IS this gate's interface: a mismatch has to name the file on stderr, and the exit code carries the verdict"
)]
pub fn report_drift(checks: &[(PathBuf, &[u8])]) -> usize {
    let mut drifted = 0usize;
    for (path, produced) in checks {
        match std::fs::read(path) {
            Ok(committed) if committed == *produced => {}
            Ok(_committed) => {
                eprintln!("DRIFT: {} (regenerated bytes differ)", path.display());
                drifted += 1;
            }
            Err(err) => {
                eprintln!("MISSING: {} ({err})", path.display());
                drifted += 1;
            }
        }
    }
    drifted
}

#[cfg(test)]
mod tests {
    // A test module may fail loudly; the library's own code may not.
    #![expect(clippy::expect_used)]

    use super::{
        fixtures_home_from_args, repo_root, repo_root_from, report_drift, FIXTURES_HOME,
        FIXTURES_HOME_FLAG,
    };
    use std::path::{Path, PathBuf};

    /// One throwaway tree under the system temp directory; removed on drop. The
    /// library's own tests must not add a dependency for this.
    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "degenbot-fixture-tools-{label}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn mark_repo_root(dir: &Path) {
        std::fs::write(dir.join("justfile"), "\n").expect("justfile marker");
        std::fs::create_dir_all(dir.join("rust")).expect("rust dir");
        std::fs::write(dir.join("rust/Cargo.toml"), "\n").expect("manifest marker");
    }

    /// Pins: the resolver returns the NEAREST ancestor carrying both markers, so a
    /// nested checkout resolves to itself and the `..`-counting arithmetic the old
    /// example used can never come back.
    #[test]
    fn repo_root_is_the_nearest_ancestor_carrying_both_markers() {
        let scratch = ScratchDir::new("nearest");
        let outer = scratch.path().join("outer");
        let inner = outer.join("inner");
        let leaf = inner.join("tools/fixture-tools");
        std::fs::create_dir_all(&leaf).expect("leaf dir");
        mark_repo_root(&outer);
        mark_repo_root(&inner);

        assert_eq!(
            repo_root_from(&leaf).expect("inner root"),
            inner,
            "the nearest marked ancestor wins"
        );
    }

    /// Pins: one marker is not enough (a bare `justfile` above the tree must not be
    /// mistaken for the repository root) and the miss is a returned error, not a
    /// fallback root and not a panic.
    #[test]
    fn repo_root_requires_both_markers_and_errors_without_them() {
        let scratch = ScratchDir::new("markers");
        let half = scratch.path().join("half");
        let leaf = half.join("tools/fixture-tools");
        std::fs::create_dir_all(&leaf).expect("leaf dir");
        std::fs::write(half.join("justfile"), "\n").expect("justfile marker");

        let error = repo_root_from(&leaf).expect_err("a lone justfile is not a root");
        assert!(
            error.contains("justfile + rust/Cargo.toml"),
            "the error names the markers it wanted: {error}"
        );
    }

    /// Pins: this package's own manifest directory resolves to a repository root that
    /// carries the committed corpus — the anchor the two generators depend on.
    #[test]
    fn this_package_resolves_the_repository_root_and_its_corpus() {
        let root = repo_root().expect("repository root");
        assert!(root.join(FIXTURES_HOME).is_dir(), "corpus home exists");
    }

    /// Pins: the explicit override wins over the derived home in both spellings, and a
    /// value that cannot be a directory is an error (silently ignoring it would run the
    /// gate against the committed corpus while the operator believed otherwise).
    #[test]
    fn fixtures_home_override_beats_the_derived_home() {
        let separated = vec![FIXTURES_HOME_FLAG.to_string(), "/tmp/a".to_string()];
        assert_eq!(
            fixtures_home_from_args(&separated).expect("separated form"),
            PathBuf::from("/tmp/a")
        );

        let joined = vec!["--check".to_string(), "--fixtures-home=/tmp/b".to_string()];
        assert_eq!(
            fixtures_home_from_args(&joined).expect("joined form"),
            PathBuf::from("/tmp/b")
        );

        // A relative override names a directory relative to the caller's cwd, exactly
        // as the old `--fixtures-home <dir>` handling did; the migration does not
        // require the operator to spell an absolute path.
        let relative = vec!["--fixtures-home".to_string(), "copied-fixtures".to_string()];
        assert_eq!(
            fixtures_home_from_args(&relative).expect("relative separated form"),
            PathBuf::from("copied-fixtures")
        );

        let valueless = vec![FIXTURES_HOME_FLAG.to_string()];
        assert!(
            fixtures_home_from_args(&valueless).is_err(),
            "a flag with no value at all is an error"
        );

        // A flag-shaped value is a forgotten argument, not a directory named `--check`.
        // Accepting it would let `--fixtures-home --check` pass through silently while
        // leaving the home un-overridden (or, worse, point the check at a nonsense dir).
        let flag_as_separated_value = vec![FIXTURES_HOME_FLAG.to_string(), "--check".to_string()];
        let error = fixtures_home_from_args(&flag_as_separated_value)
            .expect_err("a flag is not a directory");
        assert!(
            error.contains("--check"),
            "the error names the rejected value: {error}"
        );

        let flag_as_joined_value = vec!["--fixtures-home=--check".to_string()];
        let error = fixtures_home_from_args(&flag_as_joined_value)
            .expect_err("a flag is not a directory (joined form)");
        assert!(
            error.contains("--check"),
            "the error names the rejected value: {error}"
        );

        // An empty separated value is the same mistake spelled with nothing at all.
        let empty_separated_value = vec![FIXTURES_HOME_FLAG.to_string(), String::new()];
        assert!(
            fixtures_home_from_args(&empty_separated_value).is_err(),
            "an empty directory argument is an error"
        );
    }

    /// Pins: with no override the effective home is the committed corpus (so the recipes
    /// keep checking the real fixtures), and the `--check` positional arguments are not
    /// mistaken for a home.
    #[test]
    fn fixtures_home_without_override_is_the_committed_corpus() {
        let recipe_args = vec![
            "--check".to_string(),
            "tests/fixtures/cassettes/wave2".to_string(),
            "tests/fixtures/sql_goldens/wave2".to_string(),
        ];
        let home = fixtures_home_from_args(&recipe_args).expect("committed corpus home");
        assert!(home.ends_with(FIXTURES_HOME), "{}", home.display());
        assert!(home.is_dir(), "{}", home.display());
    }

    /// Pins: `report_drift` counts a matching artifact as clean, a differing artifact as
    /// drift, and a missing artifact as drift — the three verdicts the `--check` gates
    /// turn into exit codes.
    #[test]
    fn report_drift_counts_matches_drift_and_missing_files() {
        let scratch = ScratchDir::new("drift");
        let matching = scratch.path().join("matching.json");
        let differing = scratch.path().join("differing.json");
        let missing = scratch.path().join("missing.json");
        std::fs::write(&matching, b"same").expect("matching file");
        std::fs::write(&differing, b"committed").expect("differing file");

        let clean = report_drift(&[(matching, b"same".as_slice())]);
        assert_eq!(clean, 0, "byte-identical artifact is not drift");

        let drifted = report_drift(&[
            (differing, b"regenerated".as_slice()),
            (missing, b"regenerated".as_slice()),
        ]);
        assert_eq!(drifted, 2, "one differing + one missing artifact");
    }
}
