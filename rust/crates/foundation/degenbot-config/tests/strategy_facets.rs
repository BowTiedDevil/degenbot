//! The strategy facet sections are typed namespaces (ADR-055):
//! `SCHEMA` resolves each key of a keyed facet, the loader accepts an empty
//! facet table, and it still rejects an unknown key inside one.

use std::path::PathBuf;

use degenbot_config::{BotConfigLoader, ConfigError, LoadedConfig, SECTION_PATHS};

fn temp_toml(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "degenbot-config-strategy-facet-{}-{name}.toml",
        std::process::id()
    ));
    if let Err(e) = std::fs::write(&path, body) {
        unreachable!("temp toml write failed: {e}");
    }
    path
}

fn load(path: &PathBuf) -> Result<LoadedConfig, ConfigError> {
    BotConfigLoader::new()
        .without_env()
        .with_config_path(path)
        .load()
}

#[test]
fn section_paths_declare_every_facet() {
    assert!(SECTION_PATHS.contains(&"strategy.settlement"));
    assert!(SECTION_PATHS.contains(&"strategy.mevblocker_backrun"));
    assert!(SECTION_PATHS.contains(&"strategy.txpool_backrun"));
}

#[test]
fn empty_facet_tables_load() {
    let path = temp_toml(
        "empty",
        "[strategy.settlement]\n[strategy.mevblocker_backrun]\n[strategy.txpool_backrun]\n",
    );
    let loaded = load(&path);
    let _ = std::fs::remove_file(&path);
    if let Err(e) = loaded {
        unreachable!("empty facet tables must load: {e:?}");
    }
}

#[test]
fn unknown_key_in_a_facet_is_rejected() {
    let path = temp_toml("unknown", "[strategy.settlement]\nbogus = true\n");
    let loaded = load(&path);
    let _ = std::fs::remove_file(&path);
    let Err(error) = loaded else {
        unreachable!("unknown facet key must fail the load");
    };
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("unknown key bogus in section [strategy.settlement]"),
        "unexpected error: {rendered}"
    );
}
