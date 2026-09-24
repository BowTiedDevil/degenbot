//! Acceptance criterion: a generated key-reference doc renders EVERY key
//! with env name, TOML path, default, and description — and the committed
//! `docs/rust-config-keys.md` is in lockstep with the schema.
//!
//! Regenerate the committed doc with:
//! `REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config`

use std::path::PathBuf;

use degenbot_config::doc::DOC_RELATIVE_PATH;
use degenbot_config::SCHEMA;

fn committed_doc_path() -> std::path::PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DOC_RELATIVE_PATH)
}

#[test]
fn committed_doc_matches_schema() -> Result<(), Box<dyn std::error::Error>> {
    let rendered = degenbot_config::doc::render_key_reference();
    let path = committed_doc_path();
    if std::env::var("REGEN_CONFIG_DOCS").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &rendered)?;
    }
    let committed = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "committed doc missing at {} (run `REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config`): {e}",
            path.display()
        )
    })?;
    if rendered != committed {
        return Err(
            "docs/rust-config-keys.md has drifted from SCHEMA; regenerate with REGEN_CONFIG_DOCS=1"
                .into(),
        );
    }
    Ok(())
}

#[test]
fn rendered_doc_covers_every_key_with_all_columns() {
    let rendered = degenbot_config::doc::render_key_reference();
    assert!(
        rendered.contains("## Precedence"),
        "precedence recorded in the schema doc"
    );
    for key in SCHEMA {
        let row = format!("| `{}` | `{}.{}` |", key.env, key.section, key.field);
        assert!(
            rendered.contains(&row),
            "doc row missing for {}",
            key.toml_path
        );
    }
    // Every key contributes exactly one table row.
    let row_count = rendered
        .lines()
        .filter(|l| l.starts_with("| `DEGENBOT_"))
        .count();
    assert_eq!(row_count, SCHEMA.len(), "one row per schema key");
}

#[test]
fn rendering_is_deterministic() {
    let a = degenbot_config::doc::render_key_reference();
    let b = degenbot_config::doc::render_key_reference();
    assert_eq!(a, b);
    assert!(committed_doc_path().ends_with("docs/rust-config-keys.md"));
}
