# ADR-066: Deterministic Stub Generation — `experimental-inspect` + `pyo3-introspection` retire the hand-maintained `.pyi` set

**Status: accepted (architecture).** Decided at the architecture-review grilling. The
stub-diff spike is the first implementation gate and reopens this decision only if
generation proves unable to cover the seam surface.

## Context

ADR-053 evaluated `pyo3-stub-gen` — a proc-macro rewriter of the seam's source —
rejected it, and kept the 27 hand-maintained stub files under `src/degenbot/_ffi/`,
policed by `mypy.stubtest` (`tests/ffi/stubtest_allowlist.txt`) and a bespoke AST drift
gate. The ecosystem has since moved: PyO3's `experimental-inspect` feature (documented
at `pyo3.rs` v0.29.2) embeds introspection data in the built cdylib, and the first-party
`pyo3-introspection` crate (0.29.2, released in lockstep with the workspace's
`pyo3 ^0.29`) reads it back — `introspect_cdylib(path)` returns the module model and
`module_stub_files()` emits the root `__init__.pyi` plus per-submodule stubs: exactly
the artifact shape the seam hand-maintains today.

The governing principle from the same design session: wherever deterministic generation
is available, use it. A Rust-only consumer and a Python driver must reach the core on
equal footing, and the PyO3 layer stays minimal — hand-written seam artifacts are a
third place for drift to hide.

## Decision

**D1 — A named stub-generation lane.** A build of `degenbot_rs` with `pyo3`'s
`experimental-inspect` feature exists solely to be introspected; a just recipe runs
`introspect_cdylib` over that build and writes the stub files. Release wheels exclude
the feature, per the release-feature policy.

**D2 — Generated stubs are committed, with a regenerate-and-diff gate.** The stub set
stays in the tree (Python imports need it beside the compiled module), but source and
stubs may only agree: the `REGEN_*`-pattern drift gate already used for
`docs/rust-config-keys.md` fails on any diff.

**D3 — The existing gates become generator verification.** `mypy.stubtest` against the
running extension and the AST drift gate stay in place, re-read as checks on the
generator's output. Generator gaps live in `tests/ffi/stubtest_allowlist.txt` and only
there — nobody hand-edits a generated stub on top, because that re-enters the drift
blind spot this ADR closes.

**D4 — The principle spans the seam.** The typed configuration surface owed to
ADR-065 (a named-property projection over the verdict, replacing dotted-path string
lookups) is emitted from the `config_schema!` declaration site rather than hand-written,
and the dead seam doors beside it (`verification_retry_policy_defaults`, the duplicate
`discovery_batch_size` clamp) close. ADR-053's `pyo3-stub-gen` rejection stands; its
"the hand-maintained stub set stays" decision is superseded.

## Consequences

- The first implementation step is the spike the decision names: generate against
  today's cdylib and diff the output over the 27 hand-maintained files. That delta is
  precisely the future contents of the allowlist — and the honest measure of how far
  annotation generation still is (PyO3 calls it "a first step"; issue #2454 tracks the
  rest).
- `pyo3-introspection` tracks `pyo3` in version lockstep, so the generator pin follows
  the workspace's `pyo3` pin; a `pyo3` bump implies a generator bump and a fresh diff.
- The pure-Rust consumer is untouched: stubs are a Python-visible artifact only.

## Alternatives considered

- **Keep hand-writing (ADR-053's path).** Rejected: the drift gates exist to police
  human transcription work a deterministic tool now performs.
- **`pyo3-stub-gen`.** Stays rejected per ADR-053 — it rewrites the seam's source via
  proc macros instead of introspecting the built artifact; it was never this proposal.

## Gate outcome — the first spike

The stub-diff spike measured the mechanism against the real seam: the generator
(`pyo3-introspection` 0.29.2, audited) works, introspection data embeds correctly,
but `pyo3-macros-backend`'s fn-based `#[pymodule]` expansion passes empty member
lists and the incomplete flag, and this seam registers imperatively
(`PyModule::new` + `add_function` + `add_submodule`) at ~27 sites. Result: 27 of
27 deltas are generator-gap at the module-structure level; zero generator-correct,
zero generator-broken.

The decision therefore stands, with a named prerequisite now visible: the
registration surface must move to declarative `#[pymodule] mod` form (tracked as
an implementation task). The post-conversion re-gate is folded into that task's
acceptance: if generation still cannot cover the seam after conversion, this ADR
reopens under its own terms.

Post-conversion re-gate: the decision stands. The root module introspects 28
classes, 36 functions, and all 26 declarative submodule trees with reachable
members — 27 generated stubs, 1:1 with the hand tree, `incomplete=false` on 24 of
26. Classification: generator-correct in bulk (including catching ten runtime
members of `degenbot._ffi.simulation` missing from the hand `.pyi`, soaked today
by the stubtest allowlist), generator-gap confined to the `create_exception!`
island types, imperative U256 attributes, the #2454 annotation frontier,
`__all__` emission, and `__new__`-vs-`__init__` representation; generator-broken:
zero. The stub-generation lane builds with the gap list as the allowlist's origin.
