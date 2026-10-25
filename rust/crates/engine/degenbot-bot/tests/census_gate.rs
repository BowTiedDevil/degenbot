//! The pipeline census structural gate (ADR-083).
//!
//! Emit sites report through the one door — `instruments::record` over the
//! closed `CensusEvent` vocabulary. The `pipeline()` accessor is the census
//! module's own lazy-resolution machinery: no emit site (and no other crate)
//! may name it. This gate pins that in-process, the same idiom as the
//! config-key and metric-cardinality gates.

use std::path::{Path, PathBuf};

fn crates_root() -> PathBuf {
    #[expect(clippy::expect_used)] // the crate role dir always has a crates parent
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate role dir has a crates parent")
        .to_path_buf()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name == "target" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, out);
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        {
            out.push(path);
        }
    }
}

/// `instruments::pipeline()` appears ONLY in `instruments.rs` (the census
/// module's own internals: the port adapters, the exporter hook, and
/// `record`). Anything else is an emit site bypassing the door — the
/// exact scatter this gate exists to keep closed.
#[test]
fn pipeline_accessor_is_census_module_private() {
    let mut files = Vec::new();
    collect_rs(&crates_root(), &mut files);
    let mut offenders = Vec::new();
    for file in files {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !source.contains("instruments::pipeline()") {
            continue;
        }
        let is_this_gate = file.ends_with("tests/census_gate.rs");
        let is_census_module = is_this_gate
            || (file.components().any(|c| c.as_os_str() == "degenbot-bot")
                && file.ends_with("src/instruments.rs"));
        if !is_census_module {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "pipeline() must not be named outside the census module — report the \n  happening through `instruments::record(CensusEvent::…)` instead; offenders: {offenders:?}"
    );
}
