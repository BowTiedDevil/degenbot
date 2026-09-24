//! The single config-write path: CLI verbs edit the typed config file
//! through this module so no other component ever rewrites a config.toml
//! programmatically.
//!
//! Semantics: validate BEFORE writing (a bad value never touches the file),
//! preserve everything else in the document (comments, operator-added
//! sections, key order) via a format-preserving TOL editor, and report when
//! the process environment will shadow the written key (12-factor: env beats
//! file — a silent shadow would be a lie that the value applies).

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table};

use crate::error::ConfigError;
use crate::loader::{EnvVars, ProcessEnv};
use crate::schema::{BaseKind, BotConfig, KeyDecl};

/// The result of a successful write: whether the written key will actually
/// apply at load time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The key was written and nothing shadows it: the value applies.
    Written,
    /// The key was written but the environment layer defines the key's env
    /// var, so the env value wins at load time. The caller must surface
    /// this loudly rather than let the operator believe the write applies.
    Shadowed {
        /// The env var name holding the shadowing value.
        env: &'static str,
    },
}

/// Write one schema key into the config file (process environment).
///
/// # Errors
///
/// `ConfigError` when the raw value does not parse into the key's kind
/// (the file is never touched) or the file cannot be read/written.
pub fn write_key(
    file: &Path,
    key: &'static KeyDecl,
    raw_value: &str,
) -> Result<WriteOutcome, ConfigError> {
    write_key_with_env(file, key, raw_value, &ProcessEnv)
}

/// Write one schema key into the config file with an explicit environment
/// source (the shadow check reads it; tests inject a map).
///
/// # Errors
///
/// `ConfigError` when the raw value does not parse into the key's kind (the
/// file is never touched) or the file cannot be read/written.
pub fn write_key_with_env(
    file: &Path,
    key: &'static KeyDecl,
    raw_value: &str,
    env: &dyn EnvVars,
) -> Result<WriteOutcome, ConfigError> {
    // Validate against a pristine typed config first: a value that cannot
    // assign must never reach the file (atomic refusal).
    BotConfig::default()
        .assign(key.section, key.field, raw_value)
        .map_err(|problem| ConfigError::of(vec![problem]))?;

    let mut document = read_document(file)?;
    let table = navigate_mut(&mut document, key);
    table.insert(key.field, valued(key, raw_value));
    write_document(file, &document)?;

    if env.get(key.env).is_some_and(|v| !v.is_empty()) {
        Ok(WriteOutcome::Shadowed { env: key.env })
    } else {
        Ok(WriteOutcome::Written)
    }
}

/// Remove one schema key's override from the config file so the declared
/// default applies again. Removing the last leaf of a facet table also
/// removes the empty table (and any parents it leaves empty), so a fully
/// unset facet leaves no residue the loader would classify as unknown.
///
/// # Errors
///
/// `ConfigError` when the file cannot be read/written.
pub fn remove_key(file: &Path, key: &'static KeyDecl) -> Result<(), ConfigError> {
    let mut document = read_document(file)?;
    let segments: Vec<&str> = key.toml_path.split('.').collect();
    remove_path(document.as_table_mut(), &segments);
    write_document(file, &document)?;
    Ok(())
}

/// Remove `segments` from `table`, pruning tables the removal leaves empty.
fn remove_path(table: &mut dyn toml_edit::TableLike, segments: &[&str]) -> bool {
    let Some((head, rest)) = segments.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(head).is_some();
    }
    let Some(child) = table.get_mut(head) else {
        return false;
    };
    let Some(child_table) = child.as_table_like_mut() else {
        return false;
    };
    let removed = remove_path(child_table, rest);
    if child_table.is_empty() {
        table.remove(head);
    }
    removed
}

/// Parse the file into a format-preserving document; a missing file or
/// parent directory is an empty document (the write creates them).
fn read_document(file: &Path) -> Result<DocumentMut, ConfigError> {
    match std::fs::read_to_string(file) {
        Ok(text) => text.parse().map_err(|e| {
            ConfigError::of(vec![format!(
                "--config {}: parse error: {e}",
                file.display()
            )])
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(ConfigError::of(vec![format!(
            "--config {}: unreadable: {e}",
            file.display()
        )])),
    }
}

/// Serialize the document, creating the parent directory first.
fn write_document(file: &Path, document: &DocumentMut) -> Result<(), ConfigError> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ConfigError::of(vec![format!(
                "--config {}: cannot create directory: {e}",
                file.display()
            )])
        })?;
    }
    std::fs::write(file, document.to_string()).map_err(|e| {
        ConfigError::of(vec![format!(
            "--config {}: cannot write: {e}",
            file.display()
        )])
    })
}

/// Descend to (creating as needed) the key's parent table.
///
/// # Panics
///
/// Panics when `key.toml_path` is empty or a parent segment is not a table;
/// `KeyDecl` schema paths are non-empty and table-structured by construction.
#[expect(clippy::expect_used)] // KeyDecl schema paths are non-empty table paths
fn navigate_mut<'a>(
    document: &'a mut DocumentMut,
    key: &'static KeyDecl,
) -> &'a mut dyn toml_edit::TableLike {
    let segments: Vec<&str> = key.toml_path.split('.').collect();
    let (leaf, parents) = segments.split_last().expect("a schema path has a leaf");
    let _ = leaf;
    let mut item = document.as_item_mut();
    for segment in parents {
        let table = item
            .as_table_like_mut()
            .expect("a schema path's parents are tables");
        item = table.entry(segment).or_insert(Item::Table(Table::new()));
    }
    item.as_table_like_mut()
        .expect("the navigated parent is a table")
}

/// Render the raw text as the typed TOML value the loader round-trips.
fn valued(key: &'static KeyDecl, raw: &str) -> Item {
    // Boolean-kind keys parse from both `true` and TOML booleans; writing a
    // native boolean keeps the file readable. Every other kind round-trips
    // through its quoted-string form (the loader accepts it for all kinds,
    // including the wei amounts documented as quoted decimal text).
    if matches!(key.kind.base, BaseKind::Bool | BaseKind::BoolInverted) {
        if let Ok(b) = raw.trim().parse::<bool>() {
            return Item::Value(b.into());
        }
    }
    Item::Value(raw.to_string().into())
}
