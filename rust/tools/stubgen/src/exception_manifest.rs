// The `create_exception!` declaration manifest (ADR-066 D3).
//
// The island exception types carry no `_PYO3_DEF`, so the introspection chunks
// never describe them and the generated stubs could not spell them (18
// stubtest allowlist entries at the ADR-066 re-gate). Most register at
// `#[pymodule_init]` time (`shells/degenbot-python/src/lib.rs::
// register_exception_types` and `fleet.rs::init`); naming one in an export
// list is the same position — the export writes the runtime `__all__` but no
// chunk, so its class still comes from this manifest
// (`SessionConsumerError` on `degenbot._ffi.simulation` is that case). This table is the tool-layer declaration manifest — the
// same declaration-table pattern as `config_projection.rs`
// (SCHEMA/SECTION_PATHS): one entry per island exception, from which the
// generator splices `class <Name>(<Base>):` (doc plus typed attribute
// declarations) into the owning module's stub file.
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

use crate::annotation_imports;

/// The builtin bases a manifested island may declare (the runtime bases the
/// `create_exception!` arms use). Anything else must itself be manifested in
/// the same module.
const BUILTIN_BASES: &[&str] = &["Exception", "RuntimeError", "ValueError"];

/// One public attribute a manifested island stamps on the raised instance.
///
/// The declaration source is the runtime construction audit: each island's
/// one builder stamps the same names on every instance it raises, and this
/// table spells them so the class body can declare `name: <ty>`. The
/// `tests/ffi` attribute-surface suite ties both ends — the raised instance's
/// `__dict__` against these declarations.
#[derive(Clone, Copy)]
pub(crate) struct ExceptionAttribute {
    /// The Python-visible attribute name (the runtime `setattr` key).
    pub name: &'static str,
    /// The stamped value's stub-expressible type.
    pub ty: AttributeType,
}

/// The stub-expressible attribute types the manifest vocabulary covers.
#[derive(Clone, Copy)]
pub(crate) enum AttributeType {
    /// `int`.
    Int,
    /// `str`.
    Str,
    /// `int | None`.
    OptionalInt,
    /// `dict[str, Any]` — the typed consumer report the loop hands back.
    AnyDict,
    /// A class of the island's own module (the seam's enums, e.g.
    /// `SwapRefusalKind`), validated against the introspected class names.
    ClassRef(&'static str),
    /// The same reference, `None`-able (e.g. `ConsumerErrorKind | None`).
    OptionalClassRef(&'static str),
}

impl AttributeType {
    /// The stub spelling of the type.
    pub(crate) fn render(self) -> String {
        match self {
            Self::Int => "int".to_owned(),
            Self::Str => "str".to_owned(),
            Self::OptionalInt => "int | None".to_owned(),
            Self::AnyDict => "dict[str, Any]".to_owned(),
            Self::ClassRef(name) => (*name).to_owned(),
            Self::OptionalClassRef(name) => format!("{name} | None"),
        }
    }

    /// `(bound name, import line)` the rendered spelling needs, when any. A
    /// same-module class reference resolves inside the stub's own module and
    /// needs no import; only the `Any`-bearing spelling names an import.
    pub(crate) fn import(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::AnyDict => Some(("Any", "from typing import Any")),
            Self::Int
            | Self::Str
            | Self::OptionalInt
            | Self::ClassRef(_)
            | Self::OptionalClassRef(_) => None,
        }
    }

    /// The class the stub spelling references, when any.
    pub(crate) fn class_ref(self) -> Option<&'static str> {
        match self {
            Self::ClassRef(name) | Self::OptionalClassRef(name) => Some(name),
            Self::Int | Self::Str | Self::OptionalInt | Self::AnyDict => None,
        }
    }
}

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

    /// The public attributes the island's builder stamps on every raised
    /// instance, rendered into the class body as `name: <ty>` declarations.
    /// Empty for the message-only islands.
    pub attributes: &'static [ExceptionAttribute],
}

pub(crate) const EXCEPTION_ISLANDS: &[ExceptionIsland] = &[
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "VerificationMismatchError",
        attributes: &[],
        base: "RuntimeError",
        message: "A verification mismatch: the engine's tick data does not match on-chain state.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "VerificationRpcError",
        attributes: &[],
        base: "RuntimeError",
        message: "An RPC/transport error during on-chain verification (e.g. provider construction failed).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnsupportedPoolFamilyError",
        attributes: &[],
        base: "RuntimeError",
        message: "A construction route refused a pool whose factory no rung serves (no built-in DEX variant preset, no identity selector answered, or CREATE2 verification failed). Loud typed abort under the loud-abort rule (ADR-055 D4) — never a silent skip.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolRegistrationError",
        attributes: &[],
        base: "ValueError",
        message: "A pool was refused at registration (duplicate address, out-of-spec field, V4 amount-modifying hook, V4 dynamic fee, or V4 high static fee > 65535). Subclasses classify the specific admission reason so build_paths skips rejected pools by type, not string matching.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "HookedPoolRejectedError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A V4 pool with an amount-modifying hook was rejected at registration: the solver's concentrated-liquidity math assumes no hook intervention.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "DynamicFeePoolRejectedError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A V4 pool with a dynamic fee was rejected at registration: the solver assumes a fixed fee.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "HighFeePoolRejectedError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A V4 pool whose static fee exceeds the cmd_executor's 2-byte encoding limit (fee > 65535) was rejected at registration: the executor encodes fee as u16 in both V4_SWAP_COMPACT and V4_SWAP_DYNAMIC, so such pools cannot be encoded. They are also unprofitable (32%+ per swap).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolAlreadyRegisteredError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A pool at this address is already registered. Subclasses PoolRegistrationError (a wiring/programming error surfaced at admission time, distinct from per-field spec violations / V4 admission categories).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "SpecViolationError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A pool was refused as out of spec, either at registration (a field violates its on-chain Solidity bound: e.g. V2 reserve > uint112, V3/V4 sqrtPriceX96 / tick / fee / tickSpacing out of range) or at build (the pool is unbuildable, e.g. a known-broken pool, too few coins, an unreadable coin). The message identifies the offending field, its value, and the bound it violates; a build refusal names its reason in parentheses, e.g. `pool build refused: out of spec (curve broken pool)`.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolBuildRpcError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A pool build could not complete because a provider call failed (transport error, failed eth_call, unreachable endpoint). Transient and retry-worthy: the refusal says nothing about the pool's admissibility because its state was never read.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PoolBuildDecodeError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A pool build read chain data successfully but the returned values do not decode into a valid pool shape (e.g. a token reporting more than 18 decimals, a value that does not fit its field). Permanent for the observed state: re-reading the same data decodes the same way, so the pool is inadmissible rather than momentarily unreachable.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "TickMapIntakeInconsistentError",
        attributes: &[],
        base: "PoolRegistrationError",
        message: "A tracked pool was refused at intake: the database snapshot's tick rows and the chain word bits disagree. Stable for the session — the snapshot does not change under the crawl — so the refusal memoizes as a pool fact instead of being retried.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PossibleInaccurateResult",
        attributes: &[],
        base: "ValueError",
        message: "The simulated swap crosses a pool whose amount-modifying hook may have invalidated the result; the attached amounts are the standard-math approximation.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "PathRegistryFullError",
        attributes: &[],
        base: "ValueError",
        message: "The engine path registry is at its configured registered-path cap. Benign stop: discovery must stop offering new candidate paths.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "BootRefused",
        attributes: &[],
        base: "RuntimeError",
        message: "The fleet host refused to boot: the detected CPU budget is below the pinned-role floor, or a boot invariant failed. The library never aborts the host process on this arm; the message carries the detected budget, the floor, and one operator hint.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "StrategyHostError",
        attributes: &[],
        base: "RuntimeError",
        message: "A strategy-host operator verb was refused (a lifecycle transition the FSM does not allow).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnknownStrategyError",
        attributes: &[],
        base: "StrategyHostError",
        message: "The named strategy is not registered on the host: the operator named a driver the host never admitted.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "UnconfiguredStrategyError",
        attributes: &[],
        base: "StrategyHostError",
        message: "The named strategy is registered but unconfigured: no config facet with its required keys was booted, so it cannot be enabled.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "SwapMathOverflowError",
        attributes: &[],
        base: "ValueError",
        message: "The swap math overflowed a uint256 intermediate (the on-chain getAmountOut/getAmountIn SafeMath revert).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::engine::errors",
        name: "SwapNotComputableError",
        attributes: &[],
        base: "ValueError",
        message: "The exact-output swap is not computable: an overdraw of the output reserve, an exhausted range, or a non-computable invariant.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "rpc::errors",
        name: "ChainMismatchError",
        attributes: &[
            ExceptionAttribute {
                name: "expected",
                ty: AttributeType::Int,
            },
            ExceptionAttribute {
                name: "actual",
                ty: AttributeType::Int,
            },
            ExceptionAttribute {
                name: "endpoint",
                ty: AttributeType::Str,
            },
        ],
        base: "ValueError",
        message: "The endpoint serves a different chain than the one it was bound to.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi",
        rust_path: "bot::pool",
        name: "SwapRefused",
        attributes: &[
            ExceptionAttribute {
                name: "kind",
                ty: AttributeType::ClassRef("SwapRefusalKind"),
            },
            ExceptionAttribute {
                name: "word",
                ty: AttributeType::OptionalInt,
            },
            ExceptionAttribute {
                name: "amount",
                ty: AttributeType::OptionalInt,
            },
        ],
        base: "Exception",
        message: "The swap-simulation seam refused the read (ADR-037). Typed attributes: `kind` is a `SwapRefusalKind` member; `word` is the bitmap word the walk stalled on for the fetch arms, `None` elsewhere; `amount` is the signed request the read could not meet (negative = exact input, positive = exact output), `None` on the fetch arms, which never got to answer for an amount.",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi.fleet",
        rust_path: "fleet",
        name: "PostureRetuneError",
        attributes: &[],
        base: "ValueError",
        message: "The fleet posture re-tune channel refused the patch (unknown key, non-dict patch, empty patch, or a threshold outside its typed range).",
    },
    ExceptionIsland {
        python_module: "degenbot._ffi.simulation",
        rust_path: "simulation::session",
        name: "SessionConsumerError",
        attributes: &[
            ExceptionAttribute {
                name: "kind",
                ty: AttributeType::OptionalClassRef("ConsumerErrorKind"),
            },
            ExceptionAttribute {
                name: "batch_index",
                ty: AttributeType::Int,
            },
            ExceptionAttribute {
                name: "report",
                ty: AttributeType::AnyDict,
            },
        ],
        base: "RuntimeError",
        message: "A session consume-loop abort: a pipeline leaf failure, or a lifecycle refusal. A leaf failure's message is the leaf's own detail under the shared loud-abort prefix; a refusal (no attached session, a spent claim, the driver refusing the move, or a second attach) is prefix-free and carries the refusal's own text, because no leaf ran. `kind` is the core `ConsumerErrorKind` classification (`None` for an adapter-local slot refusal); `batch_index` is the batch whose pass failed and `report` the counts the loop reached before it stopped (the typed consumer report).",
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
    let imports = attribute_imports(&islands, &out);
    if imports.is_empty() {
        Ok(out)
    } else {
        Ok(annotation_imports::insert_imports(&out, &imports))
    }
}

/// The import lines the islands' attribute annotations need and the stub does
/// not already bind (a duplicate binding is a redefinition).
fn attribute_imports(islands: &[&ExceptionIsland], stub: &str) -> Vec<&'static str> {
    let mut needed: Vec<&'static str> = Vec::new();
    for island in islands {
        for attribute in island.attributes {
            if let Some((name, line)) = attribute.ty.import() {
                if !annotation_imports::imports_name(stub, name)
                    && !stub.contains(line)
                    && !needed.contains(&line)
                {
                    needed.push(line);
                }
            }
        }
    }
    needed
}

/// The class declaration for one island exception: the manifested base, the
/// manifested message as its doc, and one `name: <ty>` declaration per
/// stamped attribute (sorted by name, like the islands themselves, so the
/// emission is declaration-independent).
fn island_class(island: &ExceptionIsland) -> String {
    let mut class = format!(
        "class {name}({base}):\n    \"\"\"\n    {message}\n    \"\"\"\n",
        name = island.name,
        base = island.base,
        message = island.message
    );
    let mut attributes: Vec<&ExceptionAttribute> = island.attributes.iter().collect();
    attributes.sort_unstable_by_key(|attribute| attribute.name);
    if !attributes.is_empty() {
        class.push('\n');
        for attribute in attributes {
            class.push_str("    ");
            class.push_str(attribute.name);
            class.push_str(": ");
            class.push_str(&attribute.ty.render());
            class.push('\n');
        }
    }
    class
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
/// name or Rust path segment absent from the cdylib's symbol table, a base
/// that is neither builtin nor manifested in the same module, or an attribute
/// reference that names no class of the island's own module).
pub(crate) fn validate(
    islands: &[ExceptionIsland],
    module_paths: &BTreeSet<String>,
    class_names: &BTreeSet<String>,
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
        for attribute in island.attributes {
            if let Some(referenced) = attribute.ty.class_ref() {
                let resolved = class_names
                    .contains(&format!("{}::{referenced}", island.python_module))
                    || islands.iter().any(|other| {
                        other.name == referenced && other.python_module == island.python_module
                    });
                if !resolved {
                    errors.push(format!(
                        "attribute `{}` (island `{}`) references `{referenced}`, which is neither an introspected class nor a manifested island of `{}`",
                        attribute.name, island.name, island.python_module
                    ));
                }
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
            attributes: &[],
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

    fn classes(entries: &[(&str, &str)]) -> BTreeSet<String> {
        entries
            .iter()
            .map(|(module, name)| format!("{module}::{name}"))
            .collect()
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
    fn validate_err(
        islands: &[ExceptionIsland],
        paths: &BTreeSet<String>,
        classes: &BTreeSet<String>,
        blob: &str,
    ) -> String {
        let result = validate(islands, paths, classes, blob);
        assert!(
            result.is_err(),
            "validate must fail: {:?}",
            result.as_ref().ok()
        );
        result.err().unwrap_or_default()
    }

    #[test]
    fn attribute_types_render_their_stub_spellings() {
        assert_eq!(AttributeType::Int.render(), "int");
        assert_eq!(AttributeType::Str.render(), "str");
        assert_eq!(AttributeType::OptionalInt.render(), "int | None");
        assert_eq!(AttributeType::AnyDict.render(), "dict[str, Any]");
        assert_eq!(
            AttributeType::ClassRef("SwapRefusalKind").render(),
            "SwapRefusalKind"
        );
        assert_eq!(
            AttributeType::OptionalClassRef("ConsumerErrorKind").render(),
            "ConsumerErrorKind | None"
        );
    }

    #[test]
    fn splice_declares_the_typed_attributes_in_the_class_body() {
        let refusal = ExceptionIsland {
            attributes: &[
                ExceptionAttribute {
                    name: "word",
                    ty: AttributeType::OptionalInt,
                },
                ExceptionAttribute {
                    name: "kind",
                    ty: AttributeType::ClassRef("SwapRefusalKind"),
                },
                ExceptionAttribute {
                    name: "amount",
                    ty: AttributeType::OptionalInt,
                },
            ],
            ..island("SwapRefused", "Exception")
        };
        let out = splice_ok("class Later:\n    pass\n", &[refusal]);
        assert_eq!(
            out,
            concat!(
                "class SwapRefused(Exception):\n",
                "    \"\"\"\n",
                "    synthetic message.\n",
                "    \"\"\"\n",
                "\n",
                "    amount: int | None\n",
                "    kind: SwapRefusalKind\n",
                "    word: int | None\n",
                "\n",
                "class Later:\n",
                "    pass\n",
            )
        );
    }

    #[test]
    fn splice_inserts_the_typing_import_an_attribute_needs() {
        let report = ExceptionIsland {
            attributes: &[ExceptionAttribute {
                name: "report",
                ty: AttributeType::AnyDict,
            }],
            ..island("SessionConsumerError", "RuntimeError")
        };
        let out = splice_ok(
            "from typing import Final\n\ndef later() -> int: ...\n",
            &[report],
        );
        assert!(
            out.starts_with("from typing import Final\nfrom typing import Any\n\n"),
            "the missing typing import lands with the stub's own imports: {out}"
        );
        assert_eq!(out.matches("from typing import Any").count(), 1);
    }

    #[test]
    fn splice_leaves_an_import_the_stub_already_binds_alone() {
        let report = ExceptionIsland {
            attributes: &[ExceptionAttribute {
                name: "report",
                ty: AttributeType::AnyDict,
            }],
            ..island("SessionConsumerError", "RuntimeError")
        };
        let out = splice_ok(
            "from typing import Any, Final\n\ndef later() -> int: ...\n",
            &[report],
        );
        assert_eq!(out.matches("from typing").count(), 1, "no duplicate import");
        assert!(out.contains("    report: dict[str, Any]\n"));
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
        assert!(validate(&[root, fleet], &paths, &classes(&[]), &blob).is_ok());
    }

    #[test]
    fn validate_accepts_a_class_reference_the_runtime_registers() {
        let refusal = ExceptionIsland {
            attributes: &[ExceptionAttribute {
                name: "kind",
                ty: AttributeType::OptionalClassRef("ConsumerErrorKind"),
            }],
            ..island("SessionConsumerError", "RuntimeError")
        };
        let blob = blob_with(&["bot", "engine", "errors", "SessionConsumerError"]);
        let paths = modules(&["degenbot._ffi"]);
        let classes = classes(&[("degenbot._ffi", "ConsumerErrorKind")]);
        assert!(validate(&[refusal], &paths, &classes, &blob).is_ok());
    }

    #[test]
    fn validate_names_an_attribute_reference_drift() {
        let refusal = ExceptionIsland {
            attributes: &[ExceptionAttribute {
                name: "kind",
                ty: AttributeType::ClassRef("SwapRefusalKindX"),
            }],
            ..island("SwapRefused", "Exception")
        };
        let blob = blob_with(&["bot", "engine", "errors", "SwapRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let classes = classes(&[("degenbot._ffi", "SwapRefusalKind")]);
        let err = validate_err(&[refusal], &paths, &classes, &blob);
        assert!(
            err.contains("SwapRefusalKindX"),
            "names the drifted reference: {err}"
        );
        assert!(err.contains("kind"), "names the attribute: {err}");
    }

    #[test]
    fn validate_names_a_perturbed_manifest_name() {
        let drifted = island("BootRefusedX", "RuntimeError");
        let blob = blob_with(&["bot", "engine", "errors", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[drifted], &paths, &classes(&[]), &blob);
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
        let err = validate_err(&[moved], &paths, &classes(&[]), &blob);
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
        let err = validate_err(&[island], &paths, &classes(&[]), &blob);
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
        assert!(validate(&[drifted], &paths, &classes(&[]), blob).is_err());
    }

    #[test]
    fn validate_names_a_base_that_is_neither_builtin_nor_manifested() {
        let drifted = island("BootRefused", "NotAnException");
        let blob = blob_with(&["bot", "engine", "errors", "BootRefused"]);
        let paths = modules(&["degenbot._ffi"]);
        let err = validate_err(&[drifted], &paths, &classes(&[]), &blob);
        assert!(err.contains("NotAnException"), "names the base: {err}");
    }

    #[test]
    fn manifest_declares_every_stamped_attribute() {
        let mut declared: Vec<(&str, &str, String)> = Vec::new();
        for island in EXCEPTION_ISLANDS {
            for attribute in island.attributes {
                declared.push((island.name, attribute.name, attribute.ty.render()));
            }
        }
        declared.sort_unstable();
        assert_eq!(
            declared,
            vec![
                ("ChainMismatchError", "actual", "int".to_owned()),
                ("ChainMismatchError", "endpoint", "str".to_owned()),
                ("ChainMismatchError", "expected", "int".to_owned()),
                ("SessionConsumerError", "batch_index", "int".to_owned()),
                (
                    "SessionConsumerError",
                    "kind",
                    "ConsumerErrorKind | None".to_owned(),
                ),
                (
                    "SessionConsumerError",
                    "report",
                    "dict[str, Any]".to_owned()
                ),
                ("SwapRefused", "amount", "int | None".to_owned()),
                ("SwapRefused", "kind", "SwapRefusalKind".to_owned()),
                ("SwapRefused", "word", "int | None".to_owned()),
            ]
        );
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

            let mut attribute_names = std::collections::BTreeSet::new();
            for attribute in island.attributes {
                assert!(
                    !attribute.name.is_empty()
                        && attribute
                            .name
                            .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                        && attribute
                            .name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{}.{}: an attribute name is a Python identifier",
                    island.name,
                    attribute.name
                );
                assert!(
                    attribute_names.insert(attribute.name),
                    "{}.{}: duplicate attribute name",
                    island.name,
                    attribute.name
                );
            }
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
