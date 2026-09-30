// The typed seam projection's `.pyi` face (ADR-066 D3).
//
// The schema macro (`degenbot_config::config_schema!`) emits `SCHEMA` — one
// `KeyDecl` per declared key, kind included — and `SECTION_PATHS` from the
// same declaration arms that drive the runtime `__getattr__` machinery in
// `shells/degenbot-python/src/config.rs`. This module turns those declaration
// tables into the stub face: the introspection chunks can only spell the
// runtime machinery (`__getattr__(name: str) -> Any`; mypy rejects any other
// `__getattr__` spelling, so per-name overloads cannot build), so the
// generator instead emits per-section `@type_check_only` face classes with
// one typed read-only property per declared key, and `ConfigValues` inherits
// a face base carrying one section property per top-level section. mypy then
// resolves `values.<section>.<field>` to the declared kind, and an undeclared
// section or key is a type error — the closed projection, at the type level.
//
// The face base is a type-checking-only inheritance shim: at runtime the
// sections resolve through the getattro slot, which introspection cannot see
// (the same gap that keeps one `__getattr__` residual entry in
// `tests/rust/stubtest_allowlist.txt` for `ConfigSectionValues`). stubtest
// skips `@type_check_only` classes, so the shim costs no allowlist entries,
// and the drift gate (`just gen-stubs --check`) regenerates the whole face
// from the declaration tables — machine-emitted only, per ADR-066 D3.

use std::collections::{BTreeMap, BTreeSet};

use degenbot_config::{BaseKind, KeyDecl, ValueKind, SCHEMA, SECTION_PATHS};

/// The Python-visible type for one declared kind. Mirrors the binding layer's
/// `value_into_py`: the projection returns exactly these objects, so the stub
/// annotation names the same type, and an optional key adds `| None` for the
/// "operator said nothing" projection.
fn python_type(kind: &ValueKind) -> String {
    let base = match kind.base {
        BaseKind::Bool | BaseKind::BoolInverted => "bool",
        BaseKind::Ms
        | BaseKind::Usize
        | BaseKind::U64
        | BaseKind::U128
        | BaseKind::I64
        | BaseKind::I32 => "int",
        BaseKind::F64 => "float",
        BaseKind::Str | BaseKind::Path | BaseKind::Enum(..) => "str",
        BaseKind::Map(..) | BaseKind::StrMap => "dict[str, str]",
    };
    if kind.optional {
        format!("{base} | None")
    } else {
        base.to_owned()
    }
}

/// `_Config<pascal(path)>Face` for a dotted section path (`nodes` →
/// `_ConfigNodesFace`, `strategy.settlement` → `_ConfigStrategySettlementFace`).
fn face_name(path: &str) -> String {
    let pascal: String = path
        .split(['.', '_'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    format!("_Config{pascal}Face")
}

/// One section's typed face: the class name plus the property lines for its
/// declared keys and nested facet namespaces.
struct SectionFace {
    class_name: String,
    properties: Vec<String>,
}

/// The typed property lines for one declared key: a read-only property, since
/// the projection is a frozen view.
fn key_property(field: &str, py_type: &str) -> String {
    format!("    @property\n    def {field}(self) -> {py_type}: ...")
}

/// The typed property lines for one nested facet namespace.
fn child_property(segment: &str, child_face: &str) -> String {
    format!("    @property\n    def {segment}(self) -> {child_face}: ...")
}

/// One face class's full text, or nothing when it carries no members.
fn face_class(face: &SectionFace) -> Option<String> {
    if face.properties.is_empty() {
        return None;
    }
    let mut out = format!("@type_check_only\nclass {}:", face.class_name);
    for property in &face.properties {
        out.push('\n');
        out.push_str(property);
    }
    Some(out)
}

/// The face classes for every declared section path, parents before children,
/// keyed by path so the emission is declaration-independent and sorted.
fn section_faces(keys: &[KeyDecl], section_paths: &[&str]) -> Vec<String> {
    // path -> (declared key properties, nested namespace properties)
    let mut faces: BTreeMap<&str, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for key in keys {
        faces
            .entry(key.section)
            .or_default()
            .0
            .push(key_property(key.field, &python_type(&key.kind)));
    }
    for path in section_paths {
        if let Some((parent, segment)) = path.rsplit_once('.') {
            faces
                .entry(parent)
                .or_default()
                .1
                .push(child_property(segment, &face_name(path)));
        }
    }
    faces
        .into_iter()
        .map(|(path, (mut keys, mut children))| {
            keys.sort();
            children.sort();
            keys.extend(children);
            SectionFace {
                class_name: face_name(path),
                properties: keys,
            }
        })
        .filter_map(|face| face_class(&face))
        .collect()
}

/// The `_ConfigValuesFace` base: one typed section property per top-level
/// section, which `ConfigValues` inherits in the stub.
fn values_face(section_paths: &[&str]) -> String {
    let mut tops: BTreeMap<&str, String> = BTreeMap::new();
    for path in section_paths {
        let top = path.split('.').next().unwrap_or(path);
        tops.entry(top).or_insert_with(|| face_name(top));
    }
    let mut out = String::from("@type_check_only\nclass _ConfigValuesFace:");
    for (top, face) in tops {
        out.push('\n');
        out.push_str(&child_property(top, &face));
    }
    out
}

/// The docstring lines following a member's def line, if the member carries
/// one of the generator's own docstring shapes (a `"""`-only opener/closer
/// pair, or a single-line docstring). `Err` on anything else: the member
/// shape is not what this generator emits.
fn member_docstring(lines: &[&str], def_at: usize) -> Result<Option<Vec<String>>, String> {
    let Some(body) = lines.get(def_at + 1) else {
        return Ok(None);
    };
    let trimmed = body.trim_start();
    if !trimmed.starts_with("\"\"\"") {
        return Ok(None);
    }
    if trimmed.len() > 6 && trimmed.ends_with("\"\"\"") {
        return Ok(Some(vec![(*body).to_owned()]));
    }
    let mut close_at = def_at + 2;
    while close_at < lines.len() && lines[close_at].trim() != "\"\"\"" {
        close_at += 1;
    }
    if close_at >= lines.len() {
        return Err(format!(
            "unterminated __getattr__ docstring after def at line {}",
            def_at + 1
        ));
    }
    Ok(Some(
        lines[def_at + 1..=close_at]
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
    ))
}

/// Rewrite `class ConfigValues:` into `class ConfigValues(_ConfigValuesFace):`
/// with its machinery `__getattr__` member removed, and emit the face classes
/// immediately above it. The face classes are private (`_`-prefixed), so the
/// generated `__all__` (public exports only) never lists them.
fn splice_values(stub: &str, faces: &[String], values_face: &str) -> Result<String, String> {
    let lines: Vec<&str> = stub.lines().collect();
    let class_at = lines
        .iter()
        .position(|line| *line == "class ConfigValues:")
        .ok_or_else(|| "class ConfigValues not found".to_string())?;
    let decorator_at = lines[..class_at]
        .iter()
        .rposition(|line| *line == "@final")
        .ok_or_else(|| "ConfigValues is not @final in the emitted stub".to_string())?;
    let mut def_at = None;
    for (i, line) in lines.iter().enumerate().skip(class_at + 1) {
        if !line.is_empty() && !line.starts_with(' ') {
            break; // left the class body
        }
        if line.starts_with("    def __getattr__(") {
            def_at = Some(i);
            break;
        }
    }
    let Some(def_at) = def_at else {
        return Err("class ConfigValues carries no __getattr__ member to replace".to_string());
    };
    let docstring = member_docstring(&lines, def_at)?;
    let consumed = 1 + docstring.as_ref().map_or(0, Vec::len);
    let mut out: Vec<String> = lines[..decorator_at]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    for face in faces {
        out.push(face.clone());
        out.push(String::new());
    }
    out.push(values_face.to_owned());
    out.push(String::new());
    out.push("@final".to_owned());
    out.push("class ConfigValues(_ConfigValuesFace):".to_owned());
    out.extend(
        lines[class_at + 1..def_at]
            .iter()
            .map(|line| (*line).to_owned()),
    );
    out.extend(
        lines[def_at + consumed..]
            .iter()
            .map(|line| (*line).to_owned()),
    );
    let mut joined = out.join("\n");
    joined.push('\n');
    Ok(joined)
}

/// Add names to the stub's `from typing import ...` line, keeping the sorted
/// spelling the tree already carries. No-op for names already imported.
fn ensure_typing_import(stub: &str, names: &[&str]) -> String {
    const PREFIX: &str = "from typing import ";
    let mut lines: Vec<String> = stub.lines().map(str::to_owned).collect();
    if let Some(at) = lines.iter().position(|line| line.starts_with(PREFIX)) {
        let mut merged: BTreeSet<String> = lines[at][PREFIX.len()..]
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        merged.extend(names.iter().map(|name| (*name).to_owned()));
        lines[at] = format!(
            "{PREFIX}{}",
            merged.into_iter().collect::<Vec<_>>().join(", ")
        );
    } else {
        let declaration_at = lines
            .iter()
            .position(|line| line.starts_with("class ") || line.starts_with('@'))
            .unwrap_or(lines.len());
        let mut sorted: Vec<&str> = names.to_vec();
        sorted.sort_unstable();
        lines.insert(declaration_at, format!("{PREFIX}{}", sorted.join(", ")));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Splice the typed projection face into one generated stub. A no-op for
/// stubs without the projection classes; an `Err` names the seam shape this
/// generator no longer recognizes — never silently fall back to the
/// machinery face.
pub(crate) fn apply(stub: &str) -> Result<String, String> {
    if !stub.contains("class ConfigValues:") {
        return Ok(stub.to_owned());
    }
    let faces = section_faces(SCHEMA, SECTION_PATHS);
    let values = values_face(SECTION_PATHS);
    let out = splice_values(stub, &faces, &values)?;
    Ok(ensure_typing_import(&out, &["type_check_only"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic declaration tables shaped like the real macro's emission; the
    // real-table smoke test below pins the actual schema's face.
    const T_BOOL: ValueKind = ValueKind {
        base: BaseKind::Bool,
        optional: false,
    };
    const T_U64: ValueKind = ValueKind {
        base: BaseKind::U64,
        optional: false,
    };
    const T_F64: ValueKind = ValueKind {
        base: BaseKind::F64,
        optional: false,
    };
    const T_STR: ValueKind = ValueKind {
        base: BaseKind::Str,
        optional: false,
    };
    const T_OPT_STRMAP: ValueKind = ValueKind {
        base: BaseKind::StrMap,
        optional: true,
    };
    const T_MAP_ENUM: ValueKind = ValueKind {
        base: BaseKind::Map("Level"),
        optional: false,
    };

    const fn key(section: &'static str, field: &'static str, kind: ValueKind) -> KeyDecl {
        KeyDecl {
            section,
            field,
            env: "DEGENBOT_TEST",
            env_prefix: None,
            toml_path: section,
            kind,
            default_repr: "",
            description: "",
        }
    }

    const KEYS: &[KeyDecl] = &[
        key("dispatch", "min_profit_margin_bps", T_U64),
        key("dispatch", "erc6909_profit", T_BOOL),
        key("diagnostics", "tracemalloc_secs", T_F64),
        key("diagnostics", "label", T_STR),
        key("nodes", "http", T_OPT_STRMAP),
        key("telemetry", "diag", T_MAP_ENUM),
        key("session", "chain_id", T_U64),
        key("strategy.settlement", "enabled", T_BOOL),
    ];

    const PATHS: &[&str] = &[
        "dispatch",
        "diagnostics",
        "nodes",
        "telemetry",
        "session",
        "strategy",
        "strategy.settlement",
    ];

    /// `splice_values` for the success path: asserts the result rather than
    /// unwrapping (the workspace denies `unwrap_used`/`expect_used`).
    fn splice_ok(stub: &str, faces: &[String], values_face: &str) -> String {
        let result = splice_values(stub, faces, values_face);
        assert!(
            result.is_ok(),
            "splice must succeed: {:?}",
            result.as_ref().err()
        );
        result.ok().unwrap_or_default()
    }

    /// `apply` for the success path, same policy.
    fn apply_ok(stub: &str) -> String {
        let result = apply(stub);
        assert!(
            result.is_ok(),
            "apply must succeed: {:?}",
            result.as_ref().err()
        );
        result.ok().unwrap_or_default()
    }

    #[test]
    fn python_type_maps_each_declared_kind() {
        let cases = [
            (BaseKind::Bool, false, "bool"),
            (BaseKind::BoolInverted, false, "bool"),
            (BaseKind::Ms, false, "int"),
            (BaseKind::Usize, false, "int"),
            (BaseKind::U64, false, "int"),
            (BaseKind::U128, false, "int"),
            (BaseKind::I64, false, "int"),
            (BaseKind::I32, false, "int"),
            (BaseKind::F64, false, "float"),
            (BaseKind::Str, false, "str"),
            (BaseKind::Path, false, "str"),
            (BaseKind::Enum("Mode", &["a", "b"]), false, "str"),
            (BaseKind::Map("Level"), false, "dict[str, str]"),
            (BaseKind::StrMap, false, "dict[str, str]"),
            (BaseKind::Str, true, "str | None"),
            (BaseKind::StrMap, true, "dict[str, str] | None"),
        ];
        for (base, optional, expected) in cases {
            assert_eq!(
                python_type(&ValueKind { base, optional }),
                expected,
                "{base:?} optional={optional}"
            );
        }
    }

    #[test]
    fn face_name_pascals_the_section_path() {
        assert_eq!(face_name("nodes"), "_ConfigNodesFace");
        assert_eq!(
            face_name("strategy.settlement"),
            "_ConfigStrategySettlementFace"
        );
        assert_eq!(face_name("state_lock"), "_ConfigStateLockFace");
    }

    #[test]
    fn section_faces_type_each_declared_key_to_its_kind() {
        let faces = section_faces(KEYS, PATHS);
        let text = faces.join("\n");
        assert!(text.contains("@type_check_only\nclass _ConfigDispatchFace:"));
        assert!(text.contains("    @property\n    def min_profit_margin_bps(self) -> int: ..."));
        assert!(text.contains("    @property\n    def erc6909_profit(self) -> bool: ..."));
        assert!(text.contains("    @property\n    def tracemalloc_secs(self) -> float: ..."));
        // optional keys carry the None projection
        assert!(text.contains("    @property\n    def http(self) -> dict[str, str] | None: ..."));
        // facet namespaces project their child face
        assert!(text.contains(
            "    @property\n    def settlement(self) -> _ConfigStrategySettlementFace: ..."
        ));
        // the child face itself is emitted with its own declared keys
        assert!(text.contains("@type_check_only\nclass _ConfigStrategySettlementFace:"));
        assert!(text.contains("    @property\n    def enabled(self) -> bool: ..."));
        // nothing outside the declaration is typed
        assert!(!text.contains("not_a_key"));
    }

    #[test]
    fn section_faces_are_sorted_and_parents_precede_children() {
        let faces = section_faces(KEYS, PATHS);
        let names: Vec<&str> = faces
            .iter()
            .map(|face| face.split('\n').nth(1).unwrap_or_default())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "faces emit in sorted path order");
    }

    #[test]
    fn values_face_lists_top_level_sections_deduplicated() {
        let face = values_face(PATHS);
        assert!(face.starts_with("@type_check_only\nclass _ConfigValuesFace:"));
        assert!(face.contains("    @property\n    def dispatch(self) -> _ConfigDispatchFace: ..."));
        // "strategy" appears once despite strategy.settlement also declaring it
        assert_eq!(face.matches("def strategy(").count(), 1);
        assert!(
            !face.contains("settlement"),
            "only top-level sections are listed"
        );
    }

    #[test]
    fn splice_values_replaces_the_machinery_member_with_the_inherited_face() {
        let stub = concat!(
            "from typing import Any, Final, final\n",
            "\n",
            "@final\n",
            "class ConfigSectionValues:\n",
            "    def __getattr__(self, name: str, /) -> Any: ...\n",
            "\n",
            "@final\n",
            "class ConfigValues:\n",
            "    \"\"\"\n",
            "    The typed seam projection.\n",
            "    \"\"\"\n",
            "    def __getattr__(self, section: str, /) -> ConfigSectionValues:\n",
            "        \"\"\"\n",
            "        One declared section by name.\n",
            "        \"\"\"\n",
            "\n",
            "@final\n",
            "class Later:\n",
            "    def keep(self, /) -> int: ...\n",
        );
        let faces = section_faces(KEYS, PATHS);
        let out = splice_ok(stub, &faces, &values_face(PATHS));
        assert!(out.contains("class ConfigValues(_ConfigValuesFace):"));
        assert!(out.contains("    The typed seam projection.\n"));
        assert!(out.contains("@type_check_only\nclass _ConfigValuesFace:"));
        assert!(out.contains("@final\nclass Later:\n    def keep(self, /) -> int: ...\n"));
        // the machinery member is gone
        assert!(!out.contains("def __getattr__(self, section: str, /)"));
        // ConfigSectionValues is untouched (its residual entry stays)
        assert!(out.contains(
            "class ConfigSectionValues:\n    def __getattr__(self, name: str, /) -> Any: ..."
        ));
        // the face classes land above ConfigValues, below the untouched class
        let face_at = out.find("@type_check_only").unwrap_or_default();
        let values_at = out.find("class ConfigValues(").unwrap_or_default();
        let section_at = out.find("class ConfigSectionValues:").unwrap_or_default();
        assert!(section_at < face_at && face_at < values_at);
    }

    #[test]
    fn splice_values_fails_loud_on_unexpected_class_shape() {
        let faces: Vec<String> = Vec::new();
        // class missing entirely
        assert!(splice_values("class Other:\n    pass\n", &faces, "").is_err());
        // machinery member missing
        let no_member = "@final\nclass ConfigValues:\n    pass\n";
        assert!(splice_values(no_member, &faces, "").is_err());
        // unterminated docstring: not the generator's own emission
        let unterminated = concat!(
            "@final\nclass ConfigValues:\n",
            "    def __getattr__(self, section: str, /) -> ConfigSectionValues:\n",
            "        \"\"\"\n",
            "        never closed\n",
        );
        assert!(splice_values(unterminated, &faces, "").is_err());
    }

    #[test]
    fn ensure_typing_import_merges_sorted() {
        let stub = "from typing import Any, Final, final\n\n@final\nclass A:\n    x: Any\n";
        let out = ensure_typing_import(stub, &["type_check_only"]);
        assert!(out.starts_with("from typing import Any, Final, final, type_check_only\n"));
        let out = ensure_typing_import(&out, &["type_check_only"]);
        assert!(out.starts_with("from typing import Any, Final, final, type_check_only\n"));
    }

    #[test]
    fn apply_splices_the_real_schema() {
        let stub = concat!(
            "from typing import Any, Final, final\n",
            "\n",
            "@final\n",
            "class ConfigValues:\n",
            "    def __getattr__(self, section: str, /) -> ConfigSectionValues:\n",
            "        \"\"\"help\"\"\"\n",
        );
        let out = apply_ok(stub);
        assert!(out.contains("from typing import Any, Final, final, type_check_only\n"));
        assert!(!out.contains("def __getattr__(self, section: str, /)"));
        assert!(out.contains("class ConfigValues(_ConfigValuesFace):"));
        // the real schema's u64 key types as int
        assert!(out.contains("    @property\n    def min_profit_margin_bps(self) -> int: ..."));
    }

    #[test]
    fn apply_is_a_noop_without_the_projection_classes() {
        let stub = "@final\nclass Pool:\n    def fee(self, /) -> int: ...\n";
        assert_eq!(apply_ok(stub), stub);
    }
}
