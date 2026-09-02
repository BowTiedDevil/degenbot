//! Transparent read access for large committed capture fixtures.
//!
//! Large solver-capture fixtures are committed as zstd-compressed `.jsonl.zst`
//! (multi-MB JSONL compresses 100-400x losslessly) so the repository stays far
//! below git-host file limits while replay tests, examples, and the CI golden
//! gate keep reading natural `.jsonl` paths. Regeneration is unchanged: capture
//! scripts emit plain JSONL; packaging to `.zst` happens at commit time (see
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

fn zst_sibling(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".zst");
    PathBuf::from(s)
}

fn decode_zst_file(path: &Path) -> String {
    let f =
        std::fs::File::open(path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let mut decoder =
        zstd::Decoder::new(f).unwrap_or_else(|e| panic!("cannot decode {}: {e}", path.display()));
    let mut out = String::new();
    decoder
        .read_to_string(&mut out)
        .unwrap_or_else(|e| panic!("cannot read decoded {}: {e}", path.display()));
    out
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
}
