//! Build-identity tag: bake a monotonic build counter + source fingerprint
//! into every compile of `degenbot_rs`. See `src/build_info.rs` and the
//! AGENTS.md section "Rebuilding the Rust `.so` after edits" — maturin/uv have
//! repeatedly served a stale cached cdylib after Rust edits while reporting a
//! successful rebuild. The counter + fingerprint are the receipt that
//! distinguishes a fresh build. Python reads them via
//! `degenbot._ffi.build_number` / `degenbot.build_info`.
//!
//! The fingerprint covers EVERY workspace source that can link into (or
//! configure the build of) the shipped cdylib — this crate, every sibling
//! crate under `rust/crates`, the workspace manifests, and any `.cargo`
//! config — not just this crate's own files. This script also emits a
//! `cargo:rerun-if-changed` per scanned file plus each scanned tree (so added
//! files are caught). Emitting directives narrows cargo's watch from the whole
//! package to exactly those paths, so the scan must stay complete; but the
//! default package watch never saw a dependency-only edit, so without them the
//! script never re-ran and the receipt blessed a stale wheel as fresh.
//!
//! The counter only advances when the fingerprint changes, so routine
//! `cargo test` / `cargo clippy` / feature-variant rebuilds (which re-run this
//! script but compile byte-identical sources) keep the wheel-installed copy
//! fresh — only an actual source edit marks it stale.

include!("build_scan.rs");

/// Repo-root default (`<repo>/.build-number`), three levels above this crate.
/// Lives OUTSIDE `rust/target` so a `cargo clean` or gc-target sweep can never
/// roll the counter back, and is gitignored so builds never dirty a checkout.
const COUNTER_NAME: &str = ".build-number";

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

    let scan = scan_workspace(&crate_dir);
    let fingerprint = scan.as_ref().map(|scan| scan.fingerprint);

    // Emit a trigger per scanned file, plus each scanned tree so a file ADDED
    // to a crate (or `.cargo`) also re-runs this script and moves the
    // fingerprint. Every emitted path is folded into the hash, so triggering
    // more often can never mark an unchanged artifact stale.
    if let Some(scan) = &scan {
        for file in &scan.files {
            println!("cargo:rerun-if-changed={}", file.path.display());
        }
        for dir in &scan.dirs {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
    }

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
    // The temp name is process-unique so two concurrent cargo builds cannot
    // interleave inside the same temp file; rename then publishes whole
    // content. Both artifacts end up carrying the same fingerprint, so the
    // last writer winning the counter is harmless.
    if let Some(parent) = counter_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let line = match fingerprint {
        Some(fp) => format!("{next} {fp:016x}\n"),
        None => format!("{next}\n"),
    };
    let tmp = counter_path.with_extension(format!("tmp.{}", std::process::id()));
    if fs::write(&tmp, &line).is_ok() {
        let _ = fs::rename(&tmp, &counter_path);
    } else {
        println!(
            "cargo:warning=degenbot build-number: could not write {} - \
             staleness cross-check may report the installed .so as fresh",
            counter_path.display()
        );
    }

    // Baked identity. The changed rustc-env values additionally force rustc to
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
