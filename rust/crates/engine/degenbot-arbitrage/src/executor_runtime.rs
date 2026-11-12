//! The executor-runtime bytecode artifact: resolution + loading.
//!
//! The settlement-arbitrage sim injects the executor contract's runtime
//! bytecode into its state-overrides overlay (`stateOverrides.code`), so a
//! session needs the committed artifact's bytes before its first sim. The
//! engine owns both halves of that:
//!
//! - [`resolve_executor_runtime_path`] — one precedence chain over the two
//!   operator values that can move the path (an explicit artifact path, then
//!   the declared `dispatch.contracts_dir` key) and one computed candidate for
//!   a source checkout. No filesystem walk.
//! - [`load_executor_runtime`] — the read + the `0x`-hex decode, with errors
//!   that name the file and the knob that would fix it.
//!
//! A driver shell only carries the operator values across its boundary; it
//! resolves nothing, stats nothing, reads nothing, and names no filename. That
//! is why the pure-Rust consumers (`degenbot-cli`, the settlement-bot example)
//! can provision the same artifact the Python cockpit does, from one
//! implementation.

use std::fmt;
use std::path::{Path, PathBuf};

use alloy::primitives::Bytes;

/// The canonical artifact filename, in any contracts directory.
pub const EXECUTOR_RUNTIME_FILE: &str = "cmd_executor_runtime_bytecode.txt";

/// A resolved + loaded executor-runtime artifact.
#[derive(Debug, Clone)]
pub struct ExecutorRuntime {
    /// The artifact file the bytecode was read from.
    pub path: PathBuf,
    /// The decoded runtime bytecode (the code injected at the executor address).
    pub bytecode: Bytes,
}

/// Why the executor-runtime artifact could not be loaded.
///
/// Every variant names the file it was looking at: an operator reaching this
/// error already knows the sim cannot run, and the path is the one fact that
/// says which of the resolution inputs actually won.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecutorRuntimeError {
    /// The resolved path holds no artifact.
    NotFound {
        /// The resolved artifact path.
        path: PathBuf,
    },
    /// The artifact exists but could not be read.
    Unreadable {
        /// The resolved artifact path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The artifact's text is not `0x`-prefixed hex bytecode.
    Parse {
        /// The resolved artifact path.
        path: PathBuf,
        /// What about the text failed validation.
        detail: String,
    },
}

impl fmt::Display for ExecutorRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { path } => write!(
                f,
                "executor runtime bytecode not found at {}: pass the explicit artifact path \
                 (EXECUTOR_RUNTIME) or set the dispatch.contracts_dir key \
                 (DEGENBOT_CONTRACTS_DIR) to the directory holding {EXECUTOR_RUNTIME_FILE} — a \
                 wheel install, which ships no contracts directory, must set one of them",
                path.display()
            ),
            Self::Unreadable { path, source } => write!(
                f,
                "executor runtime bytecode at {} could not be read: {source}",
                path.display()
            ),
            Self::Parse { path, detail } => write!(
                f,
                "executor runtime bytecode at {} is not valid 0x-hex text: {detail}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ExecutorRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            Self::NotFound { .. } | Self::Parse { .. } => None,
        }
    }
}

/// The source-layout candidate: `<repo-root>/rust/crates/foundation/degenbot-executor/releases/v2/deployment/<file>`.
///
/// `CARGO_MANIFEST_DIR` is this crate's compile-time location, so the hop to
/// the repository root is fixed rather than a search:
/// `<root>/rust/crates/engine/degenbot-arbitrage`. A wheel build compiles from
/// a scratch directory, so its candidate simply does not exist at runtime —
/// the package ships no contracts directory and the operator supplies the
/// artifact through one of the two keys.
fn source_layout_candidate() -> Option<PathBuf> {
    repository_root().map(|root| {
        root.join("rust/crates/foundation/degenbot-executor/releases/v2/deployment")
            .join(EXECUTOR_RUNTIME_FILE)
    })
}

/// The checkout root this crate was compiled in, when there is one.
///
/// `CARGO_MANIFEST_DIR` is this crate's compile-time location, so the hop to the
/// repository root is fixed rather than a search:
/// `<root>/rust/crates/engine/degenbot-arbitrage`. The release manifests name
/// their artifacts repository-relative, so the release-bound binding resolves
/// them against this same root. A wheel build compiles from a scratch directory,
/// so its root simply does not hold the artifacts — the caller supplies
/// [`Path::new`]-based roots explicitly instead of searching.
#[must_use]
pub fn repository_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .map(Path::to_path_buf)
}

/// Resolve the executor-runtime artifact path — first hit wins, no walk.
///
/// Resolution order:
/// 1. `explicit` — the operator's artifact path (the `EXECUTOR_RUNTIME` key).
/// 2. `contracts_dir` — the declared `dispatch.contracts_dir` key, with the
///    canonical [`EXECUTOR_RUNTIME_FILE`] name.
/// 3. The one computed source-layout candidate ([`source_layout_candidate`]).
///
/// An empty path is "unset": a driver shell commonly carries an unset key as
/// `""`, and a relative `cmd_executor_runtime_bytecode.txt` would silently
/// resolve against the process's working directory.
///
/// Existence is not checked here — [`load_executor_runtime`] owns the read and
/// reports which path it tried.
#[must_use]
pub fn resolve_executor_runtime_path(
    explicit: Option<&Path>,
    contracts_dir: Option<&Path>,
) -> PathBuf {
    if let Some(path) = explicit.filter(|path| !path.as_os_str().is_empty()) {
        return path.to_path_buf();
    }
    if let Some(directory) = contracts_dir.filter(|dir| !dir.as_os_str().is_empty()) {
        return directory.join(EXECUTOR_RUNTIME_FILE);
    }
    source_layout_candidate().unwrap_or_else(|| PathBuf::from(EXECUTOR_RUNTIME_FILE))
}

/// Resolve, read, and decode the executor-runtime bytecode artifact.
///
/// The artifact is the deployed runtime with its five immutables already baked
/// in (owner, WETH, PoolManager, and the two precomputed V4 CurrencyDelta
/// slots) — `contracts/recompile.py` owns that layout. The body is one line of
/// `0x`-prefixed hex.
///
/// # Errors
///
/// - [`ExecutorRuntimeError::NotFound`] when the resolved path holds no file —
///   the message names both keys that would point it somewhere real.
/// - [`ExecutorRuntimeError::Unreadable`] when the file exists but cannot be
///   read.
/// - [`ExecutorRuntimeError::Parse`] when the text is not a non-empty,
///   even-length `0x`-hex body; the message names the file.
pub fn load_executor_runtime(
    explicit: Option<&Path>,
    contracts_dir: Option<&Path>,
) -> Result<ExecutorRuntime, ExecutorRuntimeError> {
    let path = resolve_executor_runtime_path(explicit, contracts_dir);
    let bytecode = load_runtime_file(&path)?;
    Ok(ExecutorRuntime { path, bytecode })
}

/// Read and decode ONE runtime-artifact file, at an already-resolved path.
///
/// The release-bound binding resolves its artifact from the release manifest
/// (`artifact.runtime_code`, repository-relative) instead of the operator
/// precedence chain, and reads it through this same strict `0x`-hex decode: one
/// decode rule for every executor runtime artifact, whoever named the file.
///
/// # Errors
///
/// - [`ExecutorRuntimeError::NotFound`] when `path` holds no file.
/// - [`ExecutorRuntimeError::Unreadable`] when the file exists but cannot be
///   read.
/// - [`ExecutorRuntimeError::Parse`] when the text is not a non-empty,
///   even-length `0x`-hex body; the message names the file.
pub fn load_runtime_file(path: &Path) -> Result<Bytes, ExecutorRuntimeError> {
    let text = std::fs::read_to_string(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => ExecutorRuntimeError::NotFound {
            path: path.to_path_buf(),
        },
        _ => ExecutorRuntimeError::Unreadable {
            path: path.to_path_buf(),
            source,
        },
    })?;
    parse_bytecode(path, &text)
}

/// Decode the artifact's `0x`-hex body into runtime bytecode.
///
/// The decode is strict on purpose: a body that decodes to nothing (an empty
/// `0x`), an odd number of hex digits, or a non-hex character would otherwise
/// produce an overlay account whose code cannot execute the bundle, and the
/// failure would surface as a sim revert far from the artifact that caused it.
fn parse_bytecode(path: &Path, text: &str) -> Result<Bytes, ExecutorRuntimeError> {
    let parse_error = |detail: String| ExecutorRuntimeError::Parse {
        path: path.to_path_buf(),
        detail,
    };
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("0x")
        .ok_or_else(|| parse_error(format!("expected a 0x prefix, found {:?}", prefix(trimmed))))?;
    if body.is_empty() {
        return Err(parse_error(
            "the body after the 0x prefix is empty".to_owned(),
        ));
    }
    if body.len() % 2 != 0 {
        return Err(parse_error(format!(
            "odd number of hex digits ({}), so the body is not whole bytes",
            body.len()
        )));
    }
    let bytes = degenbot_core::hex_utils::decode_hex(body)
        .map_err(|error| parse_error(error.to_string()))?;
    Ok(Bytes::from(bytes))
}

/// The first few characters of an artifact body, for an error message.
fn prefix(text: &str) -> String {
    text.chars().take(24).collect()
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        load_executor_runtime, resolve_executor_runtime_path, ExecutorRuntimeError,
        EXECUTOR_RUNTIME_FILE,
    };

    /// A unique scratch directory, removed on drop.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "degenbot-executor-runtime-{}-{sequence}-{label}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("scratch dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// Write an artifact at `dir/<name>` and return the file path.
        fn write_artifact(dir: &Path, body: &str) -> PathBuf {
            let file = dir.join(EXECUTOR_RUNTIME_FILE);
            fs::write(&file, body).expect("write artifact");
            file
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_explicit_path_wins_over_the_contracts_dir_key() {
        let explicit = PathBuf::from("/operator/rt.txt");
        let contracts = PathBuf::from("/declared/contracts");

        let resolved =
            resolve_executor_runtime_path(Some(explicit.as_path()), Some(contracts.as_path()));

        assert_eq!(resolved, explicit);
    }

    #[test]
    fn the_contracts_dir_key_wins_over_the_source_candidate() {
        let contracts = PathBuf::from("/declared/contracts");

        let resolved = resolve_executor_runtime_path(None, Some(contracts.as_path()));

        assert_eq!(resolved, contracts.join(EXECUTOR_RUNTIME_FILE));
    }

    #[test]
    fn the_source_layout_candidate_is_the_last_resort() {
        let resolved = resolve_executor_runtime_path(None, None);

        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some(EXECUTOR_RUNTIME_FILE)
        );
        assert_eq!(
            resolved
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("deployment"),
            "the fixed hop lands in the accepted release's deployment/ directory"
        );
    }

    #[test]
    fn an_empty_path_is_unset() {
        let contracts = PathBuf::from("/declared/contracts");

        assert_eq!(
            resolve_executor_runtime_path(Some(Path::new("")), Some(contracts.as_path())),
            contracts.join(EXECUTOR_RUNTIME_FILE)
        );
    }

    #[test]
    fn the_explicit_artifact_is_read_and_decoded() {
        let scratch = ScratchDir::new("explicit");
        let file = ScratchDir::write_artifact(scratch.path(), "0xdeadbeef\n");

        let loaded =
            load_executor_runtime(Some(file.as_path()), None).expect("explicit artifact loads");

        assert_eq!(loaded.path, file);
        assert_eq!(loaded.bytecode.as_ref(), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn the_contracts_dir_key_locates_the_canonical_filename() {
        let scratch = ScratchDir::new("contracts-dir");
        let file = ScratchDir::write_artifact(scratch.path(), "0xabcd");

        let loaded = load_executor_runtime(None, Some(scratch.path())).expect("artifact loads");

        assert_eq!(loaded.path, file);
        assert_eq!(loaded.bytecode.as_ref(), &[0xab, 0xcd]);
    }

    #[test]
    fn a_missing_artifact_names_the_overrides_that_would_fix_it() {
        let scratch = ScratchDir::new("missing");
        let missing = scratch.path().join("absent.txt");

        let error = load_executor_runtime(Some(missing.as_path()), None)
            .expect_err("a missing artifact is an error");

        let message = error.to_string();
        assert!(matches!(error, ExecutorRuntimeError::NotFound { .. }));
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
        assert!(message.contains("EXECUTOR_RUNTIME"), "{message}");
        assert!(message.contains("dispatch.contracts_dir"), "{message}");
    }

    #[test]
    fn a_non_hex_body_is_a_parse_error_naming_the_file() {
        let scratch = ScratchDir::new("malformed");
        let file = ScratchDir::write_artifact(scratch.path(), "not-hex");

        let error = load_executor_runtime(Some(file.as_path()), None)
            .expect_err("a non-hex body is an error");

        let message = error.to_string();
        assert!(matches!(error, ExecutorRuntimeError::Parse { .. }));
        assert!(message.contains(&file.display().to_string()), "{message}");
        assert!(message.contains("0x prefix"), "{message}");
    }

    #[test]
    fn an_odd_length_body_is_a_parse_error() {
        let scratch = ScratchDir::new("odd-length");
        let file = ScratchDir::write_artifact(scratch.path(), "0xabc");

        let error = load_executor_runtime(Some(file.as_path()), None)
            .expect_err("an odd-length body is an error");

        assert!(matches!(error, ExecutorRuntimeError::Parse { .. }));
        assert!(error.to_string().contains("odd number of hex digits"));
    }

    #[test]
    fn an_empty_body_is_a_parse_error() {
        let scratch = ScratchDir::new("empty-body");
        let file = ScratchDir::write_artifact(scratch.path(), "0x");

        let error = load_executor_runtime(Some(file.as_path()), None)
            .expect_err("an empty body is an error");

        assert!(matches!(error, ExecutorRuntimeError::Parse { .. }));
        assert!(error.to_string().contains("is empty"));
    }
}
