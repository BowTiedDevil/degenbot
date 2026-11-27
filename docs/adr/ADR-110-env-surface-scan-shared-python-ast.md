# ADR-110: The environment surface is scanned once, as a syntax tree

**Status: accepted.** Records the scanner behind ADR-062 D7's one-env-reading-owner
rule and ADR-065's cross-language half: one stdlib Python implementation now
answers both questions the configuration discipline asks about sources — which
environment names the companion reads, and which `DEGENBOT_*` names exist in the
Rust tree — and the Rust test tier reaches it through thin adapters instead of
carrying a second scanner.

> **One scanner for the environment surface, in stdlib Python, over a syntax
tree. Silence is never its answer.**

The contract is one sentence because the alternative is a gate whose green run
cannot say what it examined. A token walk answers a spelling it was not taught
with silence; a snapshot answers a tree it did not read with silence. Neither
silence is evidence.

## Context

ADR-062 D7 makes `degenbot-config` the tree's one env-reading owner, and ADR-065
records the Python half of that rule: `os.environ` / `os.getenv` /
`os.environb` may appear in the companion only at enumerated sites. Two scanners
enforced it, both living inside the `degenbot-config` acceptance tests:

- the Rust half reads `.rs` sources and enumerates names per file;
- the companion half was a hand-rolled line lexer tracking code/string mode.

The lexer's header named two measured gaps, and both fail *open*:

1. **An unterminated triple-quoted literal silences the rest of the file.** The
   walk stays in string mode, every later read reads as prose, and the gate
   passes — a source that does not parse is the one input that must not be
   silent.
2. **Import aliasing is invisible.** `import os as o` then `o.environ.get(...)`,
   or `from os import environ` then `environ[...]`, are reads the walk recognises
   as *syntax* and cannot name, because it matches the literal tokens
   `os.environ` / `os.getenv`.

The schema-inventory half had a related posture: an `rg` sweep with a committed
snapshot as the fallback when `rg` is absent. A snapshot is a remembered answer
about a tree nobody read.

## The measured comparison

Run before the lexer was removed, over the same population and the same shape
corpus.

| Surface | Hand-rolled lexer | Shared AST scan |
|---|---|---|
| `src/degenbot` (172 Python sources), raw findings | 3 — `runner/config.py:86: name`, `runner/config.py:130: _RETIRED_INJECTION_KEY`, `logging.py:388: DEGENBOT_DEBUG` | the same 3, same lines, same names |
| the same tree with the allowlist applied | no findings | no findings |
| `os.environ.get("NAME")`, `os.getenv("NAME")`, `os.environ.copy()`, `for k in os.environ`, computed f-string name | agrees (`NAME`, `NAME`, `<computed>`, `<computed>`, `<computed>`) | agrees |
| `import os as o` → `o.environ.get("NAME")` | **silent** | `NAME` |
| `from os import environ` → `environ["NAME"]` | **silent** | `NAME` |
| `from os import getenv as g` → `g("NAME")` | **silent** | `NAME` |
| `from os import environ as ENV` → `"NAME" in ENV` | **silent** | `NAME` |
| unterminated triple-quoted literal before a read | **silent** (fails open) | refused, naming `file:line` |
| `DEGENBOT_*` inventory of `rust/crates` | `rg` sweep: 218 names; committed snapshot: **197** | 218 names, byte-identical to the `rg` sweep |

The snapshot was 21 names stale: every missing name was a test-harness knob the
schema mapping subtracts before comparing, which is exactly why the staleness was
invisible — the fallback could be wrong without ever being red. The `rg` sweep
and the Python sweep are identical, so the tool dependency bought nothing the
sweep itself needs.

## Decision

### D1 — One implementation, in `scripts/env_surface.py`

The companion read scan and the `DEGENBOT_*` inventory are two commands of one
stdlib module (`reads`, `inventory`). It parses with `ast`, so string content,
comments, f-string holes, and multi-line literals are decided by the grammar
rather than by mode tracking, and an import alias resolves to the name it binds.
The canonical module name `os` counts as bound whether or not a source imports
it — a missing import is a different defect — while an alias exists only through
its import.

### D2 — Silence is never an answer

A source that does not parse, a missing or empty scan surface, a source that is
not UTF-8, and an inventory sweep that finds no name are refusals: exit 2 with
the reason named. Findings exit 1. A clean scan exits 0. There is no skip, no
partial verdict, and no fallback population.

### D3 — One report, one convention

Findings print `path:line: NAME`, repo-relative. A name is reported as written at
the call site, so a computed read is reported by the identifier it computes from
(the loader's `env::var(name)` seam and the companion's retired-knob loop alike)
and an expression no name is reducible from is `<computed>`. The Rust half keeps
the same vocabulary, so one message format covers both languages.

### D4 — The Rust tier keeps thin adapters, and `python3` is declared

`no_stray_env_reads` (companion reads) and `schema_completeness` (key inventory)
stay Rust acceptance tests that invoke the shared module, so
`rust-test-affected` keeps running both against the tree a push changes. They are
adapters: no scanning logic, no allowlist, no snapshot. `python3` is a declared
test-tool dependency (`scripts/hooks/ci-recipe-tools.txt`, asserted by CI's
`rust-test` job); when it is absent the adapters PANIC with a named refusal
rather than skipping. A change to the shared script selects
`degenbot-config` in the pre-push test selection, because that member's tests
execute it.

### D5 — The Python suite runs the same scan

The default pytest suite scans `src/degenbot` in-process (`tests/test_env_surface.py`),
so a Python-only diff — and the Python CI job, which never invokes cargo — is
judged by the same implementation. The allowlist lives beside the scanner
(`PYTHON_ALLOWLIST`), and a pytest assertion holds it to the sources: every
sanctioned path exists, is a companion source, and reads exactly its enumerated
names.

### D6 — One walker per shape of question

Grep-shaped scans (does this identifier appear? where is this literal spelled?)
go through `tests/source_scan.py`. AST-shaped scans (env reads, attribute-load
censuses) go through this module's source enumeration, so the config field-reader
census, the companion scan, and the Rust adapters agree on which files a Python
tree holds. The FFI seam policy (`tests/test_ffi_boundary.py`) became the one home
for the driver-facing import restriction: it scans `src/degenbot`, `tests`, and
`examples` with the same rules and one walker, so the example ban is a scope of
the policy rather than a second test with its own substring scan.

## Consequences

- A new companion read fails in two tiers: the Rust adapter (pre-push) and the
  pytest scan (default suite and the Python CI job). A new alias spelling or an
  unparseable companion source fails everywhere at once, because one
  implementation decides both.
- The gate now needs `python3` where it previously needed `rg`. That is a
  deliberate trade: the interpreter is already a declared dependency of the
  pytest and pre-push tiers, and the `rg`-with-snapshot posture was measured
  wrong (21 stale names) rather than merely redundant.
- The retired artifacts went with the decision: the in-test lexer and its shape
  table, the committed `degenbot_env_inventory.txt` snapshot, the
  `just env-inventory` recipe that regenerated it, and the standalone example
  ban test the FFI policy now covers.
- The scanner reports names, not values, and reads the trees it is given: a read
  that arrives through generated code or another checkout's copy is outside its
  population. That limit is stated where the population is chosen.

## Alternatives rejected

- **Keep the hand-rolled lexer.** It cannot see aliases and fails open on a
  source that does not parse; both are reads a contributor can write today.
- **Keep the `rg` sweep with the snapshot fallback.** Measured 21 names stale,
  and a snapshot answer is about no tree at all.
- **Move the gate entirely into pytest.** The pre-push tier would stop running it
  for a Rust-only push, and the push verdict has to cover the tree being pushed.
- **Require `rg` instead.** A gate whose verdict depends on a tool's presence
  reports the environment; the sweep is ten lines of Python.
- **Keep the schema-inventory sweep as text rather than a syntax tree.** It is a
  name inventory over every file type in `rust/crates` — including comments and
  data files — so text is the right population for that question; only the
  *reader* changed.
