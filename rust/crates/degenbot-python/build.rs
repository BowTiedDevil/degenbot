//! Build-identity tag: bake a monotonic build counter + source fingerprint
//! into every compile of `degenbot_rs`. See `src/build_info.rs` and the
//! AGENTS.md section "Rebuilding the Rust `.so` after edits" — maturin/uv have
//! repeatedly served a stale cached cdylib after Rust edits while reporting a
//! successful rebuild. The counter + fingerprint are the receipt that
//! distinguishes a fresh build. Python reads them via
//! `degenbot._ffi.build_number` / `degenbot.build_info`.
//!
//! The counter only advances when the crate's source fingerprint changes, so
//! routine `cargo test` / `cargo clippy` / feature-variant rebuilds (which
//! re-run this script but compile byte-identical sources) keep the wheel-
//! installed copy fresh — only an actual source edit marks it stale.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Repo-root default (`<repo>/.build-number`), three levels above this crate.
/// Lives OUTSIDE `rust/target` so a `cargo clean` or gc-target sweep can never
/// roll the counter back, and is gitignored so builds never dirty a checkout.
const COUNTER_NAME: &str = ".build-number";

/// FNV-1a 64-bit (std-only; no hash dependency in a build script).
fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Content fingerprint of everything that defines the compiled artifact's
/// behavior: this crate's `build.rs`/`Cargo.toml`/`src/`, PLUS every sibling
/// workspace crate under `rust/crates` (`Cargo.toml` + `build.rs` + `src/`)
/// and the workspace manifest + lockfile. The linker folds those crates into
/// the shipped wheel, so a dependency-only edit MUST move the fingerprint —
/// otherwise the receipt blesses a stale wheel as fresh.
fn source_fingerprint(crate_dir: &Path) -> Option<u64> {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |tag: &[u8], content: &[u8]| {
        hash = fnv1a(tag, hash);
        hash = fnv1a(content, hash);
    };

    mix(b"build.rs", &fs::read(crate_dir.join("build.rs")).ok()?);
    mix(b"Cargo.toml", &fs::read(crate_dir.join("Cargo.toml")).ok()?);

    let mut files = BTreeMap::new();
    collect_source_files(&crate_dir.join("src"), &mut files);
    for (rel, content) in files {
        mix(rel.as_bytes(), &content);
    }

    // Workspace level: rust/Cargo.toml + rust/Cargo.lock + each crate dir.
    let crates_dir = crate_dir.parent()?.to_path_buf();
    let workspace_root = crates_dir.parent()?.to_path_buf();
    for extra in ["Cargo.toml", "Cargo.lock"] {
        if let Ok(content) = fs::read(workspace_root.join(extra)) {
            mix(extra.as_bytes(), &content);
        }
    }
    let mut crate_dirs: Vec<PathBuf> = fs::read_dir(&crates_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    crate_dirs.sort();
    for dir in crate_dirs {
        if dir == crate_dir {
            continue;
        }
        let name = dir.file_name()?.to_str()?.as_bytes().to_vec();
        if let Ok(content) = fs::read(dir.join("Cargo.toml")) {
            mix(&name, &content);
        }
        if let Ok(content) = fs::read(dir.join("build.rs")) {
            mix(&name, &content);
        }
        let mut files = BTreeMap::new();
        collect_source_files(&dir.join("src"), &mut files);
        for (rel, content) in files {
            mix(&name, rel.as_bytes());
            mix(&name, &content);
        }
    }
    Some(hash)
}

/// Recursively collect `dir`'s files, keyed by path relative to `dir`.
fn collect_source_files(dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, out);
        } else if let (Some(rel), Ok(content)) = (
            path.strip_prefix(dir).ok().and_then(|p| p.to_str()),
            fs::read(&path),
        ) {
            out.insert(rel.to_owned(), content);
        }
    }
}

fn main() {
    // Unreachable under cargo (it always sets the var, which names this
    // crate); degrade to cwd + warn rather than aborting a build over an
    // environment oddity.
    let crate_dir = if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(dir)
    } else {
        println!(
            "cargo:warning=degenbot build-number: CARGO_MANIFEST_DIR unset - receipt falls back to the current directory"
        );
        std::env::current_dir().unwrap_or_default()
    };
    let counter_path = match std::env::var_os("DEGENBOT_BUILD_NUMBER_FILE") {
        Some(override_path) => PathBuf::from(override_path),
        None => {
            // Repo root: three levels above this crate.
            match crate_dir.ancestors().nth(3) {
                Some(root) => root.join(COUNTER_NAME),
                None => PathBuf::from(COUNTER_NAME),
            }
        }
    };

    // Stored state is `"<count> <fingerprint-hex>"`. Missing (fresh clone
    // before the first build), unparsable, or missing-fingerprint contents
    // start the sequence at 0 / no-match, so the first build writes 1 with a
    // valid fingerprint.
    let stored = fs::read_to_string(&counter_path).ok().and_then(|text| {
        let mut parts = text.split_whitespace();
        let count = parts.next()?.parse::<u64>().ok()?;
        // Optional second token: legacy (pre-fingerprint) files have only
        // a bare count, which must still be honored (not reset).
        // The receipt writes the fingerprint hex-formatted; parse it as hex
        // or every receipt whose digits contain a-f misparses to None and
        // the counter advances spuriously on unchanged content.
        let fingerprint = parts
            .next()
            .and_then(|tok| u64::from_str_radix(tok, 16).ok());
        Some((count, fingerprint))
    });
    let fingerprint = source_fingerprint(&crate_dir);

    // Advance ONLY on a content change (or unknown first-build state). No
    // change -> re-emit the stored number unchanged, so no-change rebuilds
    // (test/clippy/feature-variant) never mark an installed wheel stale.
    let changed = stored.is_none()
        || fingerprint.is_none()
        || stored.as_ref().and_then(|(_, fp)| *fp) != fingerprint;
    let prior_count = stored.map_or(0, |(count, _)| count);
    let next = if changed {
        prior_count.saturating_add(1)
    } else {
        prior_count
    };

    // Write-back is best-effort: a failed write degrades the cross-check
    // (the file then reads as older than the baked values) but never breaks
    // the build. Emit the resolved identity REGARDLESS, so this build is at
    // least distinguishable from its predecessor even without a receipt file.
    // Temp-file + rename keeps a concurrent reader from seeing a partial
    // write; a true concurrent-writer tie (two cargo processes racing) is not
    // a supported workflow and is benign here — both artifacts then carry the
    // same identity.
    if let Some(parent) = counter_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let line = match fingerprint {
        Some(fp) => format!("{next} {fp:016x}\n"),
        None => format!("{next}\n"),
    };
    let tmp = counter_path.with_extension("tmp");
    if fs::write(&tmp, &line).is_ok() {
        let _ = fs::rename(&tmp, &counter_path);
    } else {
        println!(
            "cargo:warning=degenbot build-number: could not write {} - \
             staleness cross-check may report the installed .so as fresh",
            counter_path.display()
        );
    }

    // NOTE: no `cargo:rerun-if-*` directives on purpose. With none emitted,
    // cargo re-runs this script whenever ANY file in the package changes —
    // the broadest available trigger, so every real source edit re-runs it
    // (adding even one rerun-if line would narrow that set and reopen a
    // stale hole). The changed rustc-env values additionally force rustc to
    // recompile `build_info.rs` rather than reuse a cached artifact.
    println!("cargo:rustc-env=DEGENBOT_BUILD_NUMBER={next}");
    if let Some(fp) = fingerprint {
        println!("cargo:rustc-env=DEGENBOT_BUILD_FINGERPRINT={fp:016x}");
    }
    println!(
        "cargo:rustc-env=DEGENBOT_BUILD_NUMBER_FILE={}",
        counter_path.display()
    );
}
