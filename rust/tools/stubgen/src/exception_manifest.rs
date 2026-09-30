// The `create_exception!` declaration manifest (ADR-066 D3).
//
// The island exception types register at `#[pymodule_init]` time
// (`shells/degenbot-python/src/lib.rs::register_exception_types` and
// `fleet.rs::init`), not as `#[pymodule_export]` declarations: they carry no
// `_PYO3_DEF`, so the introspection chunks never describe them and the
// generated stubs could not spell them (18 stubtest allowlist entries at the
// ADR-066 re-gate). This table is the tool-layer declaration manifest — the
// same declaration-table pattern as `config_projection.rs`
// (SCHEMA/SECTION_PATHS): one entry per island exception, from which the
// generator splices `class <Name>(<Base>):` into the owning module's stub
// file.
//
// `validate` is the manifest-vs-runtime gate: every manifested Python module
// must exist in the introspected module tree, and every manifested name and
// Rust path segment must exist as a mangled (length-prefixed) identifier in
// the introspected cdylib's symbol table. A manifest entry the runtime
// stopped exporting (a renamed `create_exception!` arm, a dropped `m.add`)
// fails generation with a named error instead of emitting a class the
// runtime never had; stubtest (`just lint-stubtest`) gates the reverse
// direction (a runtime island missing from the manifest).
//
// Symbol-table evidence is dev-profile-only by construction: `just gen-stubs`
// builds the cdylib under the workspace `[profile.dev]` (no LTO, no strip),
// so the `PyTypeInfo::type_object_raw` instantiation carrying each type's
// path is present in the symbol table. The generator still never executes
// the cdylib (ADR-066 D2) — symbols are read statically, from the same
// object-file parse pyo3-introspection performs for its chunks.

use std::collections::BTreeSet;
use std::path::Path;

/// The builtin bases a manifested island may declare (the runtime bases the
/// `create_exception!` arms use). Anything else must itself be manifested in
/// the same module.
const BUILTIN_BASES: &[&str] = &["Exception", "RuntimeError", "ValueError"];

/// One `create_exception!` island exception, as the runtime registers it.
#[derive(Clone, Copy)]
pub(crate) struct ExceptionIsland {
    /// The Python module the runtime registers the exception on — the stub
    /// file the class declaration splices into.
    pub python_module: &'static str,
    /// The Rust module path of the `create_exception!` arm, `::`-separated
    /// and without the crate name: the symbol-table validation's path
    /// evidence (each segment must appear length-prefixed in the cdylib).
    pub rust_path: &'static str,
    /// The Python-visible name (the `m.add` key).
    pub name: &'static str,
    /// The stub-expressible base: the runtime base exception (a builtin) or
    /// the manifested name of the island it subclasses.
    pub base: &'static str,
    /// The exception's doc, mirrored from the `create_exception!` arm.
    pub message: &'static str,
}

pub(crate) const EXCEPTION_ISLANDS: &[ExceptionIsland] = &[
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "VerificationMismatchError",
        base: "RuntimeError",
        message: "A verification mismatch: the engine's tick data does not match on-chain state.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "VerificationRpcError",
        base: "RuntimeError",
        message: "An RPC/transport error during on-chain verification (e.g. provider construction failed).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnsupportedPoolFamilyError",
        base: "RuntimeError",
        message: "A construction route refused a pool whose factory no rung serves (no built-in DEX variant preset, no identity selector answered, or CREATE2 verification failed). Loud typed abort under the loud-abort rule (ADR-055 D4) — never a silent skip.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolRegistrationError",
        base: "ValueError",
        message: "A pool was refused at registration (duplicate address, out-of-spec field, V4 amount-modifying hook, V4 dynamic fee, or V4 high static fee > 65535). Subclasses classify the specific admission reason so build_paths skips rejected pools by type, not string matching.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "HookedPoolRejectedError",
        base: "PoolRegistrationError",
        message: "A V4 pool with an amount-modifying hook was rejected at registration: the solver's CL math assumes no hook intervention.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "DynamicFeePoolRejectedError",
        base: "PoolRegistrationError",
        message: "A V4 pool with a dynamic fee was rejected at registration: the solver assumes a fixed fee.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "HighFeePoolRejectedError",
        base: "PoolRegistrationError",
        message: "A V4 pool whose static fee exceeds the cmd_executor's 2-byte encoding limit (fee > 65535) was rejected at registration: the executor encodes fee as u16 in both V4_SWAP_COMPACT and V4_SWAP_DYNAMIC, so such pools cannot be encoded. They are also unprofitable (32%+ per swap).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolAlreadyRegisteredError",
        base: "PoolRegistrationError",
        message: "A pool at this address is already registered. Subclasses PoolRegistrationError (a wiring/programming error surfaced at admission time, distinct from per-field spec violations / V4 admission categories).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "SpecViolationError",
        base: "PoolRegistrationError",
        message: "A field on the pool registration params violates its on-chain Solidity bound (e.g. V2 reserve > uint112, V3/V4 sqrtPriceX96 / tick / fee / tickSpacing out of range). The message identifies the offending field, its value, and the bound it violates.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PossibleInaccurateResult",
        base: "ValueError",
        message: "The simulated swap crosses a pool whose amount-modifying hook may have invalidated the result; the attached amounts are the standard-math approximation.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PathRegistryFullError",
        base: "ValueError",
        message: "The engine path registry is at its configured registered-path cap. Benign stop: discovery must stop offering new candidate paths.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "BootRefused",
        base: "RuntimeError",
        message: "The fleet host refused to boot: the detected CPU budget is below the pinned-role floor, or a boot invariant failed. The library never aborts the host process on this arm; the message carries the detected budget, the floor, and one operator hint.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "FleetIntakeFaultedError",
        base: "RuntimeError",
        message: "The fleet registration intake faulted: the sticky lane-death latch resolved held intake units terminally (they were never executed). Sticky until a fresh process.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "StrategyHostError",
        base: "RuntimeError",
        message: "A strategy-host operator verb was refused (a lifecycle transition the FSM does not allow).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnknownStrategyError",
        base: "StrategyHostError",
        message: "The named strategy is not registered on the host: the operator named a driver the host never admitted.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnconfiguredStrategyError",
        base: "StrategyHostError",
        message: "The named strategy is registered but unconfigured: no config facet with its required keys was booted, so it cannot be enabled.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "rpc::errors",
        name: "ChainMismatchError",
        base: "ValueError",
        message: "The endpoint serves a different chain than the one it was bound to.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi.fleet",
        rust_path: "fleet",
        name: "PostureRetuneError",
        base: "ValueError",
        message: "The fleet posture re-tune channel refused the patch (unknown key, non-dict patch, empty patch, or a threshold outside its typed range).",
    },
];

/// Splice one `class <Name>(<Base>):` declaration per manifested island into
/// the owning module's stub, immediately before the first top-level
/// declaration. Sorted by name so the emission is declaration-independent
/// (the drift gate diffs the committed set byte-for-byte).
pub(crate) fn splice(stub: &str, islands: &[ExceptionIsland]) -> Result<String, String> {
    if islands.is_empty() {
        return Ok(stub.to_owned());
    }
    let mut islands: Vec<&ExceptionIsland> = islands.iter().collect();
    islands.sort_unstable_by_key(|island| island.name);
    let anchor = declaration_anchor(stub).ok_or_else(|| {
        "no top-level declaration to anchor the exception island splice".to_string()
    })?;
    let lines: Vec<&str> = stub.lines().collect();
    let capacity = stub.len()
        + islands
            .iter()
            .map(|island| island.name.len() + island.base.len() + island.message.len() + 32)
            .sum::<usize>();
    let mut out = String::with_capacity(capacity);
    let head = lines[..anchor].join("\n");
    let head = head.trim_end_matches('\n');
    if !head.is_empty() {
        out.push_str(head);
        out.push_str("\n\n");
    }
    for island in &islands {
        out.push_str(&island_class(island));
        out.push('\n');
    }
    out.push_str(&lines[anchor..].join("\n"));
    out.push('\n');
    Ok(out)
}

/// The class declaration for one island exception: the manifested base plus
/// the manifested message as its doc.
fn island_class(island: &ExceptionIsland) -> String {
    format!(
        "class {name}({base}):\n    \"\"\"\n    {message}\n    \"\"\"\n",
        name = island.name,
        base = island.base,
        message = island.message
    )
}

/// The first top-level declaration line's index: anything that is neither
/// blank, a comment, an import, nor inside a module docstring. The docstring
/// tracking mirrors `append_dunder_all`'s (a line's `"""` count toggles
/// parity; only wholly-outside lines can anchor).
fn declaration_anchor(stub: &str) -> Option<usize> {
    let mut in_docstring = false;
    for (i, line) in stub.lines().enumerate() {
        let quote_count = line.matches("\"\"\"").count();
        if !in_docstring && quote_count == 0 {
            let trimmed = line.trim_start();
            let is_import = trimmed.starts_with("from ") || trimmed.starts_with("import ");
            if !trimmed.is_empty() && !trimmed.starts_with('#') && !is_import {
                return Some(i);
            }
        }
        in_docstring ^= quote_count % 2 == 1;
    }
    None
}

/// The manifest-vs-runtime gate: named errors for every manifested island the
/// runtime evidence does not back (module absent from the introspected tree,
/// name or Rust path segment absent from the cdylib's symbol table, or a base
/// that is neither builtin nor manifested in the same module).
pub(crate) fn validate(
    islands: &[ExceptionIsland],
    module_paths: &BTreeSet<String>,
    symbol_blob: &str,
) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    for island in islands {
        if !module_paths.contains(island.python_module) {
            errors.push(format!(
                "module `{}` is not in the introspected module tree (island `{}`)",
                island.python_module, island.name
            ));
        }
        if !contains_length_prefixed(symbol_blob, island.name) {
            errors.push(format!(
                "name `{}` has no length-prefixed identifier in the cdylib symbol table — the runtime no longer exports this exception",
                island.name
            ));
        }
        for segment in island.rust_path.split("::") {
            if !contains_length_prefixed(symbol_blob, segment) {
                errors.push(format!(
                    "rust path segment `{segment}` (island `{}`) is absent from the cdylib symbol table",
                    island.name
                ));
            }
        }
        let base_is_manifested = islands
            .iter()
            .any(|other| other.name == island.base && other.python_module == island.python_module);
        if !base_is_manifested && !BUILTIN_BASES.contains(&island.base) {
            errors.push(format!(
                "base `{}` (island `{}`) is neither a builtin exception nor a manifested island of the same module",
                island.base, island.name
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "exception manifest does not match the runtime ({} findings):\n{}",
            errors.len(),
            errors.join("\n")
        ))
    }
}

/// True when `blob` carries `ident` as a Rust-mangled identifier: the
/// manglers spell each path segment as a decimal length prefix followed by
/// the identifier, and a leading digit would mean a longer identifier's tail
/// (e.g. `115Error` is `15Error`, never `5Error`).
fn contains_length_prefixed(blob: &str, ident: &str) -> bool {
    let tagged = format!("{}{ident}", ident.len());
    let bytes = blob.as_bytes();
    let mut start = 0;
    while let Some(at) = blob[start..].find(&tagged) {
        let pos = start + at;
        let preceded_by_digit = pos > 0 && bytes[pos - 1].is_ascii_digit();
        if !preceded_by_digit {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// The cdylib's symbol names as one blob — the manifest gate's runtime
/// evidence. Static (no execution, ADR-066 D2), same object-file parse
/// pyo3-introspection performs for its chunks.
pub(crate) fn symbol_blob(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let parsed =
        goblin::Object::parse(&bytes).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let mut blob = String::new();
    match parsed {
        goblin::Object::Elf(elf) => {
            for sym in &elf.syms {
                if let Some(name) = elf.strtab.get_at(sym.st_name) {
                    blob.push_str(name);
                    blob.push('\n');
                }
            }
        }
        goblin::Object::Mach(goblin::mach::Mach::Binary(macho)) => {
            for (name, _) in macho.symbols().flatten() {
                blob.push_str(name);
                blob.push('\n');
            }
        }
        goblin::Object::Mach(goblin::mach::Mach::Fat(fat)) => {
            for arch in &fat {
                if let Ok(goblin::mach::SingleArch::MachO(macho)) = arch {
                    for (name, _) in macho.symbols().flatten() {
                        blob.push_str(name);
                        blob.push('\n');
                    }
                }
            }
        }
        other => {
            return Err(format!(
                "unsupported cdylib container for the manifest gate: {other:?} — `just gen-stubs` builds an ELF cdylib"
            ));
        }
    }
    if blob.is_empty() {
        return Err(format!(
            "no symbol names in {} — the manifest gate needs a dev-profile cdylib (no strip)",
            path.display()
        ));
    }
    Ok(blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn island(name: &'static str, base: &'static str) -> ExceptionIsland {
        ExceptionIsland {
            python_module: "degenbot._ffi",
            rust_path: "bot::engine::errors",
            name,
            base,
            message: "synthetic message.",
        }
    }

    /// A symbol blob spelling every manifest identifier the way the Rust
    /// manglers do: decimal length prefix, not preceded by a digit.
    fn blob_with(idents: &[&str]) -> String {
        let mut blob = String::new();
        for ident in idents {
            let tagged = format!("{}{ident}", ident.len());
            blob.push_str("NtNtCsym_");
            blob.push_str(&tagged);
            blob.push_str("NtB4_ ");
        }
        blob
    }

    fn modules(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    /// `splice` for the success path: asserts the result rather than
    /// panicking through a closure (the workspace denies `clippy::panic`).
    fn splice_ok(stub: &str, islands: &[ExceptionIsland]) -> String {
        let result = splice(stub, islands);
        assert!(
            result.is_ok(),
            "splice must succeed: {:?}",
            result.as_ref().err()
        );
        result.ok().unwrap_or_default()
    }

    /// `validate` for the failure path, same policy.
    fn validate_err(islands: &[ExceptionIsland], paths: &BTreeSet<String>, blob: &str) -> String {
        let result = validate(islands, paths, blob);
        assert!(
            result.is_err(),
            "validate must fail: {:?}",
            result.as_ref().ok()
        );
        result.err().unwrap_or_default()
    }

    #[test]
    fn splice_emits_one_class_per_island_with_base_and_doc() {
        let boot = island("BootRefused", "RuntimeError");
        let pool = island("PoolRegistrationError", "ValueError");
        let out = splice_ok(
            "from typing import Any\n\n@final\nclass Later:\n    pass\n",
            &[boot, pool],
        );
        assert!(out.contains("class BootRefused(RuntimeError):\n"));
        assert!(out.contains("class PoolRegistrationError(ValueError):\n"));
        assert!(out.contains("    \"\"\"\n    synthetic message.\n    \"\"\"\n"));
        // the surrounding declarations survive, and the block sits before them
        assert!(out.contains("@final\nclass Later:\n    pass\n"));
        let boot_at = out.find("class BootRefused").unwrap_or_default();
        let later_at = out.find("class Later").unwrap_or_default();
        assert!(boot_at < later_at, "islands splice before existing decls");
    }

    #[test]
    fn splice_is_a_noop_for_an_empty_island_set() {
        let stub = "class Later:\n    pass\n";
        assert_eq!(splice_ok(stub, &[]), stub);
    }

    #[test]
    fn splice_orders_islands_by_name_for_determinism() {
        let zeta = island("Zeta", "ValueError");
        let alpha = island("Alpha", "RuntimeError");
        let out = splice_ok("class Later:\n    pass\n", &[zeta, alpha]);
        let alpha_at = out.find("class Alpha").unwrap_or_default();
        let zeta_at = out.find("class Zeta").unwrap_or_default();
        assert!(alpha_at < zeta_at, "islands emit in sorted name order");
    }

    #[test]
    fn splice_anchors_before_the_first_declaration_outside_docstrings() {
        let stub = concat!(
            "\"\"\"Module doc.\n",
            "\n",
            "prose line:\n",
            "class NotARealDecl:\n",
            "\"\"\"\n",
            "from typing import Any\n",
            "\n",
            "class Later:\n",
            "    pass\n",
        );
        let boot = island("BootRefused", "RuntimeError");
        let out = splice_ok(stub, &[boot]);
        let splice_at = out.find("class BootRefused").unwrap_or_default();
        let docstring_decl_at = out.find("class NotARealDecl").unwrap_or_default();
        let later_at = out.find("\nclass Later:").unwrap_or_default();
        assert!(
            docstring_decl_at < splice_at && splice_at < later_at,
            "the docstring's prose class is not an anchor"
        );
    }

    #[test]
    fn splice_fails_loud_without_an_anchor() {
        let boot = island("BootRefused", "RuntimeError");
        let result = splice("from typing import Any\n", &[boot]);
        assert!(result.is_err(), "an import-only stub has no anchor");
    }

    #[test]
    fn validate_accepts_a_manifest_matching_the_symbol_blob() {
        let root = island("BootRefused", "RuntimeError");
        let fleet = ExceptionIsland {
            python_module: "degenbot._ffi.fleet",
            rust_path: "fleet",
            ..island("PostureRetuneError", "ValueError")
        };
        let blob = blob_with(&[
            "bot",
            "engine",
            "errors",
            "fleet",
            "BootRefused",
            "PostureRetuneError",
        ]);
        let paths = modules(&["degenbot._ffi", "degenbot._ffi.fleet"]);
        assert!(validate(&[root, fleet], &paths, &blob).is_ok());
    }

    #[test]
    fn validate_names_a_perturbed_manifest_name() {
        let drifted = island("BootRefusedX", "RuntimeError");
        let blob = blob_with(&["bot", "engine", "errors", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[drifted], &paths, &blob);
        assert!(
            err.contains("BootRefusedX"),
            "the error names the drift: {err}"
        );
    }

    #[test]
    fn validate_names_a_rust_path_segment_drift() {
        let moved = island("BootRefused", "RuntimeError");
        let blob = blob_with(&["bot", "engine", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[moved], &paths, &blob);
        assert!(err.contains("errors"), "names the missing segment: {err}");
    }

    #[test]
    fn validate_names_a_python_module_missing_from_the_tree() {
        let island = ExceptionIsland {
            python_module: "degenbot._ffi.fleet",
            rust_path: "fleet",
            ..island("BootRefused", "RuntimeError")
        };
        let blob = blob_with(&["fleet", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[island], &paths, &blob);
        assert!(
            err.contains("degenbot._ffi.fleet"),
            "names the module: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_length_prefix_false_positive() {
        // `115Error` carries a longer identifier whose tail contains `5Error`;
        // the leading digit must disqualify the match.
        let drifted = island("Error", "Exception");
        let blob = "NtNtCsym_115ErrorNtB4_ NtNtCsym_3botNtB4_ ";
        let paths = modules(&["degenbot._ffi"]);
        assert!(validate(&[drifted], &paths, blob).is_err());
    }

    #[test]
    fn validate_names_a_base_that_is_neither_builtin_nor_manifested() {
        let drifted = island("BootRefused", "NotAnException");
        let blob = blob_with(&["bot", "engine", "errors", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[drifted], &paths, &blob);
        assert!(err.contains("NotAnException"), "names the base: {err}");
    }

    #[test]
    fn manifest_table_is_self_consistent() {
        let mut names = std::collections::BTreeSet::new();
        for island in EXCEPTION_ISLANDS {
            assert!(
                island.python_module.starts_with("degenbot._ffi"),
                "{}: island modules live under degenbot._ffi",
                island.name
            );
            assert!(
                !island.message.is_empty(),
                "{}: the doc is the message",
                island.name
            );
            assert!(
                !island.message.contains('"'),
                "{}: the message must be docstring-safe",
                island.name
            );
            assert!(
                !island.message.contains('\n'),
                "{}: the message is a single docstring line",
                island.name
            );
            assert!(
                !island.rust_path.is_empty() && !island.rust_path.contains(' '),
                "{}: the rust path is ::-separated segments",
                island.name
            );
            assert!(
                names.insert(island.name),
                "{}: duplicate manifest name",
                island.name
            );
            let base_is_manifested = EXCEPTION_ISLANDS.iter().any(|other| {
                other.name == island.base && other.python_module == island.python_module
            });
            assert!(
                base_is_manifested || BUILTIN_BASES.contains(&island.base),
                "{}: base `{}` is neither a builtin exception nor a manifested island",
                island.name,
                island.base
            );
        }
        assert_eq!(
            EXCEPTION_ISLANDS.len(),
            18,
            "one entry per create_exception! island"
        );
    }

    #[test]
    fn splice_carries_the_real_fleet_island() {
        let fleet: Vec<ExceptionIsland> = EXCEPTION_ISLANDS
            .iter()
            .filter(|island| island.python_module == "degenbot._ffi.fleet")
            .copied()
            .collect();
        let out = splice_ok(
            "from _typeshed import Incomplete\n\ndef current_posture_policy() -> dict: ...\n",
            &fleet,
        );
        assert!(out.contains("class PostureRetuneError(ValueError):\n"));
        assert!(out.contains("The fleet posture re-tune channel refused the patch"));
        assert!(out.contains("def current_posture_policy() -> dict: ..."));
    }

    #[test]
    fn symbol_blob_of_garbage_is_a_named_error() {
        let path = std::env::temp_dir().join("degenbot-stubgen-not-a-cdylib");
        let wrote = std::fs::write(&path, b"definitely not an object file");
        assert!(
            wrote.is_ok(),
            "write {}: {:?}",
            path.display(),
            wrote.as_ref().err()
        );
        assert!(symbol_blob(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
