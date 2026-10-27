// Imports for PyO3's manual return annotations (ADR-066 D2).
//
// `#[pyo3(signature = (...) -> "T")]` is PyO3's escape hatch for a return type
// the macro cannot derive — a coroutine-returning method, whose Rust signature
// says `Bound<'_, PyAny>` while the value it resolves to is typed. The macro
// carries that annotation as a string constant (`PyExpr::str_constant`), so
// pyo3-introspection emits it verbatim as a QUOTED annotation and never
// resolves it against a module: the names such an annotation mentions are
// invisible to the emitter's import pass, and a stub that spells
// `-> "Awaitable[int]"` would reference an unimported name.
//
// This pass closes that gap for the names the binding layer actually annotates
// with. The table is the ONE home of the mapping: a name absent from it lands
// in the generated tree as an unimported annotation, which the stub lane
// reports — it cannot pass silently.

/// `(name spelled inside a quoted annotation, import line that provides it)`.
const QUOTED_ANNOTATION_IMPORTS: &[(&str, &str)] =
    &[("Awaitable", "from collections.abc import Awaitable")];

/// Add the import lines the stub's quoted annotations require.
pub fn apply(stub: &str) -> String {
    let needed: Vec<&str> = QUOTED_ANNOTATION_IMPORTS
        .iter()
        .filter(|(name, import_line)| {
            annotation_referenced(stub, name)
                && !imports_name(stub, name)
                && !stub.contains(*import_line)
        })
        .map(|(_, import_line)| *import_line)
        .collect();

    if needed.is_empty() {
        return stub.to_string();
    }

    insert_imports(stub, &needed)
}

/// Whether a quoted annotation — the only form a manual `PyO3` annotation
/// takes — begins with `name`.
fn annotation_referenced(stub: &str, name: &str) -> bool {
    stub.contains(&format!("\"{name}"))
}

/// Whether any top-level import line already binds `name`, in any spelling
/// (`from m import name`, `from m import other as name`, `import m.name`). A
/// duplicate binding is a redefinition, so the pass never adds one.
fn imports_name(stub: &str, name: &str) -> bool {
    stub.lines()
        .filter(|line| line.starts_with("from ") || line.starts_with("import "))
        .any(|line| {
            line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|token| token == name)
        })
}

/// Insert `lines` after the stub's last top-level import line. A stub with no
/// import block gets them first, where the emitter puts its own imports; the
/// recipe's `ruff check --fix` then sorts them into canonical order.
fn insert_imports(stub: &str, lines: &[&str]) -> String {
    let all: Vec<&str> = stub.lines().collect();
    let insert_at = all
        .iter()
        .rposition(|line| line.starts_with("from ") || line.starts_with("import "))
        .map_or(0, |index| index + 1);

    let mut out: Vec<String> = Vec::with_capacity(all.len() + lines.len());
    for (index, line) in all.iter().enumerate() {
        if index == insert_at {
            out.extend(lines.iter().map(ToString::to_string));
        }
        out.push((*line).to_string());
    }
    if insert_at >= all.len() {
        out.extend(lines.iter().map(ToString::to_string));
    }

    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_annotation_gains_its_import() {
        let stub = concat!(
            "from typing import Any\n",
            "\n",
            "class Provider:\n",
            "    def get_block_number(self, /) -> \"Awaitable[int]\":\n",
            "        \"\"\"doc.\"\"\"\n",
        );
        assert_eq!(
            apply(stub),
            concat!(
                "from typing import Any\n",
                "from collections.abc import Awaitable\n",
                "\n",
                "class Provider:\n",
                "    def get_block_number(self, /) -> \"Awaitable[int]\":\n",
                "        \"\"\"doc.\"\"\"\n",
            )
        );
    }

    #[test]
    fn stub_without_quoted_annotations_is_untouched() {
        let stub = "from typing import Any\n\ndef f() -> Any: ...\n";
        assert_eq!(apply(stub), stub);
    }

    #[test]
    fn already_imported_name_is_not_duplicated() {
        let stub = concat!(
            "from collections.abc import Awaitable\n",
            "\n",
            "def f() -> \"Awaitable[int]\": ...\n",
        );
        assert_eq!(apply(stub), stub);
    }

    #[test]
    fn aliased_import_still_counts_as_bound() {
        assert!(imports_name("import typing as Awaitable\n", "Awaitable"));
        assert!(imports_name(
            "from collections.abc import Awaitable as _Awaitable\n",
            "Awaitable"
        ));
        assert!(!imports_name("from typing import Any\n", "Awaitable"));
        // A quoted annotation is not an import.
        assert!(!imports_name(
            "def f() -> \"Awaitable[int]\": ...\n",
            "Awaitable"
        ));
    }

    #[test]
    fn stub_without_imports_gets_them_first() {
        let stub = "def f() -> \"Awaitable[int]\": ...\n";
        assert_eq!(
            apply(stub),
            "from collections.abc import Awaitable\ndef f() -> \"Awaitable[int]\": ...\n"
        );
    }

    #[test]
    fn import_block_at_end_of_stub_is_appended_to() {
        let stub = "def f() -> \"Awaitable[int]\": ...\nfrom typing import Any\n";
        assert_eq!(
            apply(stub),
            "def f() -> \"Awaitable[int]\": ...\nfrom typing import Any\nfrom collections.abc import Awaitable\n"
        );
    }
}
