# Evaluation: hand-rolled scanner vs `ast` walk for the Python env-read gate

**Status: decided — keep the hand-rolled scanner and fix gaps as they are
found. Revisit if a real false negative escapes the gate.**

The gate lives in
`rust/crates/foundation/degenbot-config/tests/no_stray_env_reads.rs` and
enforces ADR-062 D7: `degenbot-config` is the one env-reading owner, so the
Python companion may read `os.environ` / `os.getenv` / `os.environb` only at
the sites the test's allowlist enumerates. The single env-reading owner this
gate polices is one half of the contract recorded in
[ADR-065](../adr/ADR-065-verdict-single-configuration-authority.md): the
verdict is the single configuration authority. The Python half of the gate is a
hand-rolled lexer (`PythonWalk` plus its name-resolution helpers) rather than
a parser. This note records an evaluation of replacing it with a Python `ast`
walk: the verdict match, the implementation cost, and the reason the
hand-rolled scanner is retained for now. It does not change the gate.

## The alternative that was built

A working replacement was written and run against the repository. It walks
the companion tree with Python's `ast` module, finds the same call and
subscript nodes, and resolves the read's name the way the hand-rolled scanner
does:

- `ast.Constant` → the string literal value,
- `ast.Name` → the identifier,
- anything else → `<computed>`.

It reproduced the gate's verdict exactly at the time of measurement: the same
reported sites with the same names, including the computed `name` read and the
`INJECT_EXECUTOR_CODE` membership test, and it agreed with every shape-table
case then present (13 of 13). It also handles the two aliasing shapes listed
below, which the hand-rolled lexer does not.

### Size

| implementation | scanner logic |
| --- | --- |
| current hand-rolled lexer | ~310 lines |
| `ast` walk | ~81 lines |

The `ast` walk is smaller because the interpreter owns tokenisation, string
and f-string handling, and syntax; the replacement only classifies the nodes
the interpreter produces.

### `parents` bookkeeping is required

A mapping read through a call — `os.environ.get("NAME")` — is both a
`Subscript` on `os.environ` and the argument of a `Call`. A naive walk reports
it twice: once for the subscript node and once for the mapping node reached
through the call. The working version carried a `parents` map so a subscript
that is already the argument of a recognised call is not reported again. A
first pass omitted it and double-reported; a replacement must pin the
single-report-per-site behaviour with a test.

### The cost: an interpreter on `PATH`

The hand-rolled scanner is self-contained Rust: `cargo test` runs it with no
external process. The `ast` walk needs a Python interpreter on `PATH` and a
subprocess to run it. That makes the gate's own execution depend on the
environment it polices, and a missing or mis-versioned interpreter becomes a
new failure mode — or, with loose wiring, a silently skipped gate. The gate's
value is that it fails closed in CI, so a new process dependency has to
preserve that.

## Measured residual limits of the hand-rolled scanner

| shape | behaviour | severity |
| --- | --- | --- |
| a file opens a triple-quoted literal and never closes it | every later read in that file is silenced — **fails open** | the one that matters |
| `import os as o` then `o.environ.get(...)` | not detected — module aliasing is invisible | naming limit |
| `from os import environ` then `environ[...]` | not detected | naming limit |
| PEP 701 nested same-quote f-string | handled | — |
| a read split across lines | reported as `<computed>` | loud, wrong name |
| a decorator-position read | handled | — |

The fail-open row is a limit of a gate whose silence must be understood: a
green run means "no read was recognised", not "no read exists". The shape
table in the gate pins the unterminated-literal behaviour so the limit is
visible at the scanner. The aliasing rows are a naming limit, not a lexing
one — the read is recognised, but the scanner matches the literal
`os.environ` / `os.getenv` spellings, so a module or name alias is invisible.

## Decision and revisit triggers

**Keep the hand-rolled scanner.** The `ast` walk is smaller and strictly more
correct, but it trades a self-contained gate for an interpreter dependency the
current scanner does not need, and the known gaps do not affect the sanctioned
companion sites. The scanner's limits are documented and pinned rather than
silent.

Revisit if any of these hold:

- A real read slips through the aliasing or fail-open gaps, or the gate's
  silence is mistaken for a clean tree.
- A replacement can obtain the `ast` verdict without adding a process
  dependency to the gate — e.g. an in-process parser or a Rust parser with
  the same fidelity.
- The gate moves to a harness that already runs a Python interpreter, so the
  dependency is not incremental.
