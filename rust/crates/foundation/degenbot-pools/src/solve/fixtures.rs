//! Transparent read access for large committed capture fixtures.
//!
//! Large solver-capture fixtures are committed as zstd-compressed `.jsonl.zst`
//! (multi-MB JSONL compresses 100-400x losslessly) so the repository stays far
//! below git-host file limits while replay tests, examples, and the CI golden
//! gate keep reading natural `.jsonl` paths. Regeneration uses `write_fixture`:
//! plain JSONL and `.zst` siblings are replaced atomically, decoded bytes are
//! read back, identical plain shadows are removed before packed replacement,
//! and divergent shadows fail without changing either file (see
//! `tests/fixtures/.gitignore`).
//
// Misuse here is fatal by design: a missing fixture must stop the test/example
// with a recovery hint, and decode failures are equally unrecoverable. The
// panic-family lints are opted out file-wide instead of threading Results
// through 8 example/test call sites (matches the diagnostic-bin convention).
#![expect(clippy::panic)]

use std::io::Read;
use std::path::{Path, PathBuf};

/// Read a capture fixture transparently.
///
/// Resolution order for `path`:
///
/// 1. an explicit `.zst` suffix decodes directly;
/// 2. a readable plain file is returned verbatim;
/// 3. `path` + `.zst` is decoded in memory;
/// 4. otherwise panic, naming the missing paths and how to recover.
pub fn read_fixture<P: AsRef<Path>>(path: P) -> String {
    read_fixture_inner(path.as_ref())
}

fn read_fixture_inner(path: &Path) -> String {
    // 1. explicit .zst path (or .zst beside it, case 3) decodes in memory
    if path.extension().is_some_and(|e| e == "zst") {
        return decode_zst_file(path);
    }
    // 2. readable plain file wins (regenerated captures, or repo without packaging)
    if let Ok(content) = std::fs::read_to_string(path) {
        return content;
    }
    // 3. packaged sibling
    let zst = zst_sibling(path);
    if zst.exists() {
        return decode_zst_file(&zst);
    }
    // 4. nothing found
    panic!(
        "capture fixture missing: neither {} nor {} present; decode with \
         `zstd -d -f {} -o {}`, or regenerate the capture (docs/cache-lab-report.md)",
        path.display(),
        zst.display(),
        zst.display(),
        path.display(),
    );
}

/// The committed-fixture path for `file_name` (relative to this crate's
/// `tests/fixtures/`). Centralized so examples stop hand-rolling
/// `CARGO_MANIFEST_DIR` joins.
#[must_use]
pub fn fixture_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(file_name)
}

/// Parse fixture JSONL content into per-line JSON values.
///
/// Fatal by design like [`read_fixture`]: a malformed line is a capture-format
/// violation, so the panic names the 1-based line number and its prefix
/// instead of silently dropping rows out of a measurement.
///
/// # Panics
///
/// Panics when any non-empty line is not valid JSON.
#[must_use]
pub fn rows(content: &str) -> Vec<serde_json::Value> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let number = index + 1;
            let preview: String = line.chars().take(80).collect();
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("capture fixture line {number} is not valid JSON ({e}): {preview}…")
            })
        })
        .collect()
}

/// [`read_fixture`] + [`rows`]: the whole corpus-read path in one call.
pub fn load_rows<P: AsRef<Path>>(path: P) -> Vec<serde_json::Value> {
    rows(&read_fixture(path))
}

#[derive(Debug)]
pub enum FixtureWriteError {
    ExplicitPackedPath(PathBuf),
    DivergentShadow {
        plain: PathBuf,
        packed: PathBuf,
    },
    StaleTemp(PathBuf),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    ReadBackMismatch(PathBuf),
    Validation {
        path: PathBuf,
        reason: String,
    },
}

impl std::fmt::Display for FixtureWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExplicitPackedPath(path) => write!(
                f,
                "regeneration requires the logical plain fixture path, not explicit .zst: {}",
                path.display()
            ),
            Self::DivergentShadow { plain, packed } => write!(
                f,
                "plain fixture shadow differs from its packed artifact: {} != {}",
                plain.display(),
                packed.display()
            ),
            Self::StaleTemp(path) => write!(
                f,
                "stale regeneration temp exists; inspect or remove it explicitly: {}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} failed for {}: {source}", path.display()),
            Self::ReadBackMismatch(path) => write!(
                f,
                "regenerated fixture read-back differs from produced content: {}",
                path.display()
            ),
            Self::Validation { path, reason } => write!(
                f,
                "pre-rename validation failed for {}: {reason}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for FixtureWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct FixtureWriteOutcome {
    pub destination: PathBuf,
    pub packed: bool,
    pub removed_plain_shadow: bool,
}

#[doc(hidden)]
pub fn write_plain_atomic<P: AsRef<Path>>(path: P, content: &str) -> Result<(), FixtureWriteError> {
    write_bytes_atomic(path.as_ref(), content.as_bytes())
}

#[doc(hidden)]
pub fn write_plain_atomic_checked<P: AsRef<Path>>(
    path: P,
    content: &str,
    verify: impl FnOnce(&Path) -> Result<(), FixtureWriteError>,
) -> Result<(), FixtureWriteError> {
    write_bytes_atomic_checked(path.as_ref(), content.as_bytes(), verify)
}

#[doc(hidden)]
pub fn write_fixture<P: AsRef<Path>>(
    path: P,
    content: &str,
) -> Result<FixtureWriteOutcome, FixtureWriteError> {
    let path = path.as_ref();
    if path.extension().is_some_and(|extension| extension == "zst") {
        return Err(FixtureWriteError::ExplicitPackedPath(path.to_path_buf()));
    }
    let packed = zst_sibling(path);
    let temp = temp_path(path);
    if temp.exists() {
        return Err(FixtureWriteError::StaleTemp(temp));
    }
    let mut removed_plain_shadow = false;
    if path.exists() && packed.exists() {
        let plain = std::fs::read_to_string(path).map_err(|source| FixtureWriteError::Io {
            operation: "read plain shadow",
            path: path.to_path_buf(),
            source,
        })?;
        let packed_bytes =
            try_decode_zst_file(&packed).map_err(|source| FixtureWriteError::Io {
                operation: "decode packed shadow",
                path: packed.clone(),
                source,
            })?;
        if plain != packed_bytes {
            return Err(FixtureWriteError::DivergentShadow {
                plain: path.to_path_buf(),
                packed,
            });
        }
        std::fs::remove_file(path).map_err(|source| FixtureWriteError::Io {
            operation: "remove identical plain shadow",
            path: path.to_path_buf(),
            source,
        })?;
        removed_plain_shadow = true;
    }
    let packed_write = packed.exists();
    let destination = if packed_write {
        let encoded =
            zstd::encode_all(content.as_bytes(), 3).map_err(|source| FixtureWriteError::Io {
                operation: "encode fixture",
                path: packed.clone(),
                source,
            })?;
        write_bytes_atomic(&packed, &encoded)?;
        packed
    } else {
        write_plain_atomic(path, content)?;
        path.to_path_buf()
    };
    let read_back = try_read_fixture(path).map_err(|source| FixtureWriteError::Io {
        operation: "read regenerated fixture",
        path: path.to_path_buf(),
        source,
    })?;
    if read_back != content {
        return Err(FixtureWriteError::ReadBackMismatch(path.to_path_buf()));
    }
    Ok(FixtureWriteOutcome {
        destination,
        packed: packed_write,
        removed_plain_shadow,
    })
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "fixture".into(), |name| name.to_string_lossy());
    path.with_file_name(format!(".{name}.regen.{}.tmp", std::process::id()))
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), FixtureWriteError> {
    write_bytes_atomic_checked(path, bytes, |_| Ok(()))
}

fn write_bytes_atomic_checked(
    path: &Path,
    bytes: &[u8],
    verify: impl FnOnce(&Path) -> Result<(), FixtureWriteError>,
) -> Result<(), FixtureWriteError> {
    let temp = temp_path(path);
    if temp.exists() {
        return Err(FixtureWriteError::StaleTemp(temp));
    }
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|source| FixtureWriteError::Io {
                operation: "create temp",
                path: temp.clone(),
                source,
            })?;
        std::io::Write::write_all(&mut file, bytes).map_err(|source| FixtureWriteError::Io {
            operation: "write temp",
            path: temp.clone(),
            source,
        })?;
        std::io::Write::flush(&mut file).map_err(|source| FixtureWriteError::Io {
            operation: "flush temp",
            path: temp.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| FixtureWriteError::Io {
            operation: "sync temp",
            path: temp.clone(),
            source,
        })?;
        drop(file);
        verify(&temp)?;
        std::fs::rename(&temp, path).map_err(|source| FixtureWriteError::Io {
            operation: "rename temp",
            path: path.to_path_buf(),
            source,
        })?;
        let read_back = std::fs::read(path).map_err(|source| FixtureWriteError::Io {
            operation: "read written bytes",
            path: path.to_path_buf(),
            source,
        })?;
        if read_back != bytes {
            return Err(FixtureWriteError::ReadBackMismatch(path.to_path_buf()));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn try_read_fixture(path: &Path) -> std::io::Result<String> {
    if path.extension().is_some_and(|extension| extension == "zst") {
        return try_decode_zst_file(path);
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        return Ok(content);
    }
    try_decode_zst_file(&zst_sibling(path))
}

fn try_decode_zst_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut decoder = zstd::Decoder::new(file)?;
    let mut out = String::new();
    decoder.read_to_string(&mut out)?;
    Ok(out)
}

fn zst_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".zst");
    PathBuf::from(s)
}

fn decode_zst_file(path: &Path) -> String {
    try_decode_zst_file(path).unwrap_or_else(|e| panic!("cannot decode {}: {e}", path.display()))
}

#[cfg(test)]
mod additions_tests {
    use super::*;

    #[test]
    fn rows_parse_per_line_json() {
        let rows = rows("{\"a\":1}\n\n{\"b\":2}\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["a"], 1);
        assert_eq!(rows[1]["b"], 2);
    }

    #[test]
    #[should_panic(expected = "line 2 is not valid JSON")]
    fn rows_fail_loud_with_the_line_number() {
        let _ = rows("{\"a\":1}\n{oops");
    }

    #[test]
    fn fixture_path_resolves_under_the_crate() {
        let path = fixture_path("heavy_mixed_solve_captures.jsonl");
        assert!(path.ends_with("tests/fixtures/heavy_mixed_solve_captures.jsonl"));
    }

    #[test]
    fn load_rows_reads_the_committed_corpus_through_the_zst_sibling() {
        // The packaged corpus: transparent .zst sibling resolution + rows.
        let rows = load_rows(fixture_path("heavy_mixed_solve_captures.jsonl"));
        assert!(!rows.is_empty(), "the committed corpus must load rows");
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    fn fix_dir(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(prefix).tempdir().unwrap()
    }

    fn write_zst(zst: &Path, content: &[u8]) {
        let mut enc =
            zstd::stream::write::Encoder::new(std::fs::File::create(zst).unwrap(), 3).unwrap();
        std::io::Write::write_all(&mut enc, content).unwrap();
        enc.finish().unwrap();
    }

    #[test]
    fn plain_fixture_is_returned_verbatim() {
        let dir = fix_dir("fixture_plain");
        let p = dir.path().join("cap.jsonl");
        std::fs::write(&p, "line1\nline2\n").unwrap();
        assert_eq!(read_fixture(&p), "line1\nline2\n");
    }

    #[test]
    fn zst_sibling_is_decoded_losslessly() {
        let dir = fix_dir("fixture_zst");
        let dir = dir.path();
        // Repetitive JSONL resembling the real captures (key to the
        // 100-400x pack ratio) plus a non-ASCII token.
        let content = "{\"hops\": [[{\"fee\": 3000, \"lat\": \"\u{3bb}\"}]]}\n".repeat(512);
        let plain = dir.join("cap.jsonl");
        write_zst(&dir.join("cap.jsonl.zst"), content.as_bytes());
        assert_eq!(read_fixture(&plain), content);
    }

    #[test]
    fn explicit_zst_path_decodes_directly() {
        let dir = fix_dir("fixture_zst_arg");
        let zst = dir.path().join("cap.jsonl.zst");
        write_zst(&zst, b"payload\n");
        assert_eq!(read_fixture(&zst), "payload\n");
    }

    #[test]
    #[should_panic(expected = "capture fixture missing")]
    fn missing_fixture_panics_with_recovery_hint() {
        let dir = fix_dir("fixture_none");
        read_fixture(dir.path().join("nope.jsonl"));
    }

    #[test]
    fn plain_atomic_write_roundtrip_cleans_temp() {
        let dir = fix_dir("fixture_write_plain");
        let path = dir.path().join("cap.jsonl");
        write_plain_atomic(&path, "new\n").unwrap();
        assert_eq!(read_fixture(&path), "new\n");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn checked_plain_atomic_validation_preserves_previous_and_cleans_temp() {
        let dir = fix_dir("fixture_write_checked");
        let path = dir.path().join("cap.jsonl");
        std::fs::write(&path, "old\n").unwrap();
        let result = write_plain_atomic_checked(&path, "new\n", |temp| {
            assert_eq!(std::fs::read_to_string(temp).unwrap(), "new\n");
            Err(FixtureWriteError::Validation {
                path: temp.to_path_buf(),
                reason: "interrupted".to_string(),
            })
        });
        assert!(matches!(result, Err(FixtureWriteError::Validation { .. })));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn write_fixture_rejects_explicit_zst_input() {
        let dir = fix_dir("fixture_write_explicit_zst");
        let path = dir.path().join("cap.jsonl.zst");
        assert!(matches!(
            write_fixture(&path, "new\n"),
            Err(FixtureWriteError::ExplicitPackedPath(_))
        ));
    }

    #[test]
    fn write_fixture_rejects_divergent_shadow_unchanged() {
        let dir = fix_dir("fixture_write_divergent");
        let plain = dir.path().join("cap.jsonl");
        let packed = zst_sibling(&plain);
        std::fs::write(&plain, "plain\n").unwrap();
        write_zst(&packed, b"packed\n");
        assert!(matches!(
            write_fixture(&plain, "new\n"),
            Err(FixtureWriteError::DivergentShadow { .. })
        ));
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "plain\n");
        assert_eq!(read_fixture(&packed), "packed\n");
    }

    #[test]
    fn write_fixture_replaces_packed_and_removes_identical_shadow() {
        let dir = fix_dir("fixture_write_packed");
        let plain = dir.path().join("cap.jsonl");
        let packed = zst_sibling(&plain);
        std::fs::write(&plain, "old\n").unwrap();
        write_zst(&packed, b"old\n");
        let outcome = write_fixture(&plain, "new\n").unwrap();
        assert!(outcome.packed);
        assert!(outcome.removed_plain_shadow);
        assert_eq!(outcome.destination, packed);
        assert!(!plain.exists());
        assert_eq!(read_fixture(&plain), "new\n");
        assert!(!temp_path(&plain).exists());
    }

    #[test]
    fn write_plain_atomic_removes_temp_on_rename_failure() {
        let dir = fix_dir("fixture_write_rename_failure");
        let path = dir.path().join("target.jsonl");
        std::fs::create_dir(&path).unwrap();
        assert!(write_plain_atomic(&path, "new\n").is_err());
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn write_fixture_rejects_stale_temp_without_mutation() {
        let dir = fix_dir("fixture_write_stale_temp");
        let plain = dir.path().join("cap.jsonl");
        std::fs::write(&plain, "old\n").unwrap();
        let temp = temp_path(&plain);
        std::fs::write(&temp, "partial").unwrap();
        assert!(matches!(
            write_fixture(&plain, "new\n"),
            Err(FixtureWriteError::StaleTemp(_))
        ));
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(&temp).unwrap(), "partial");
    }
}
