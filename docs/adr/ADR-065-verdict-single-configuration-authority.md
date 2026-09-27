# ADR-065: The verdict is the single configuration authority

**Status: accepted.** Records the contract the configuration epic established
across eight chunks: the operator configuration resolves once at module init
into one frozen verdict, that verdict is the only object the Python driver
reads, and a cross-language parity oracle is what keeps the two surfaces from
drifting apart again. Predecessors: **ADR-062** (one operator file, four
layers) decides what the cascade is and which file it reads; **ADR-063** (the
file is portable, the secret is not) decides how a portable file carries a
non-portable credential. This ADR records the enforcement of those decisions,
not new policy. Companion: the
[env-read gate scanner evaluation](../architecture/env-read-gate-scanner-evaluation.md),
which records the gate that polices the single env-reading owner ADR-062 D7
names.

> **The verdict is the single configuration authority. It resolves once at
> module init, carries provenance for every declared key, and is frozen.**

The contract is one sentence because future work will either honour it or
erode it, and erosion leaves no compile error of its own. A second authority
does not announce itself; it appears as a value re-read at a call site, a
default that disagrees with the schema's, or a key reported as `file` when the
environment won it. Each of those is a way to make the answer depend on *where
you ask* rather than *what this process is configured by*.

## Context

ADR-062 left the cascade in one crate and the typed holder in place, but the
Python driver still had surfaces to read it through one key at a time, and the
raw tables that the typed schema deliberately does not declare still needed a
file. The configuration epic closed those gaps in eight independently
shippable chunks:

- the Python companion stopped carrying a second resolution order; every
  remaining operator knob is a declared schema key with one default in one
  place;
- the dotenv layer went, because it was a config source the schema did not own
  and it was hand-parsed twice;
- the per-key resolvers on the FFI collapsed into one `ResolvedConfig`;
- the eleven `ArbitrageConfig` fields nothing read were removed and a reader
  census was added;
- four environment reads that decided their value at import were moved to the
  point where something can decide them;
- the env-read gate gained its Python half, so the one-owner rule became
  enforced across the language seam instead of only in Rust;
- the resolution oracle was made to compare the whole verdict, not one value;
- resolution became a constructible value, so a test can ask how the cascade
  resolves explicit inputs without reinstalling a global.

The contract those chunks establish has no first home in code: it is a
property of the relationship between the loader, the holder, the verdict, and
the FFI seam. This record is that home.

## Decision

### D1 — One load, one authority, one frozen projection

The four-layer cascade (explicit override, environment, operator file,
declared default) runs once, at FFI module init, and produces a
`degenbot-config` `LoadedConfig`. The typed holder receives that config, the
layers are published beside it, and `ResolvedConfig` is built from the same
publish — so the typed getters and the driver-domain resolvers cannot describe
different configurations.

`ResolvedConfig` is `#[pyclass(frozen)]`. Python reads values and provenance
through it and can edit neither. Its `values` projection walks
`degenbot_config::SCHEMA` through the generated `BotConfig::value` reader, so
declaring a key in `config_schema!` is the only edit a new key needs anywhere
in the workspace. An unset optional key is `None`, never a fabricated default,
because "the operator said nothing" and "the operator chose this" are
different facts.

The holder's first-wins install is kept: a test harness or an embedding that
installed a configuration before this module loaded one keeps its own. When
another owner installed first, the published layers carry *that* config with no
provenance, which the resolvers already read as the floor layer.

### D2 — Install-once is a production guarantee enforced in the type

The verdict is a `&'static Verdict` held in a `std::sync::OnceLock`.
Re-installing a global would require the `OnceLock` to become an `RwLock` and
the borrow an `Arc` across the whole FFI surface; that convenience is not
taken. A resolver therefore cannot see a different file or a different
environment than the holder received, and the compiler is what says so. A test
that needs a second cascade constructs one instead of reinstalling the global.

The install order is a property, not a coincidence: module init loads once,
installs the typed holder, publishes the layers, and then eagerly builds the
verdict from those layers. Both `OnceLock`s resolve to the same load whichever
is read first.

### D3 — Provenance is part of the authority, not metadata about it

A value reported with the wrong layer is the divergence class this epic set out
to remove: the pre-epic code had a Python default opposite the schema's, beside
a live read that never consulted the schema at all. The verdict therefore
carries two provenance keyings, and the difference between them is deliberate:

- `provenance` is keyed by **dotted TOML path** — the operator's vocabulary —
  and names the layer that supplied each declared key (`default`, `file`,
  `env`, or `cli`).
- `entry_provenance` is keyed by the **env family** (the key's env-name prefix)
  and then by the operator-chosen entry. A per-chain endpoint table is
  overridden one entry at a time, so a key-level map cannot describe it. The
  aggregate `provenance` entry for a family key records the highest-ranked
  contributing layer.

Both projections are schema-driven. A key no layer supplied is **absent**
rather than reported as the floor, and a layer recorded against a name no key
declares has no dotted path to appear under. Filling either absence in is how
an `env` winner gets reported as a `file` winner, so both absences are the
answer.

### D4 — The cross-language parity oracle is the enforcement mechanism

The oracle is the closing gate: a Rust half that loads a fixed operator file
and per-environment variables through `degenbot-config`, asserts the operator's
intent on a sample of keys, and writes the **whole** verdict — every declared
key's value, its winning layer, and the per-entry layer of each table — to a
committed JSON artifact; and a Python half that loads the same file and
environments and compares.

Three properties make it a parity check and not a tautology:

1. **The Python half crosses the raw FFI**, not the `degenbot.config` wrapper.
   A wrapper would compare a function with its own caller and pass with the
   whole cascade broken.
2. **The Rust half re-derives from public `degenbot-config` APIs** rather than
   calling the binding's own projections, so a projection bug on either side
   surfaces as a diff instead of two sides agreeing with each other.
3. **The whole projection is compared**, not one key, because the pre-epic
   divergence was a key nobody enumerated. Seeded mutations (value, key-layer,
   entry-layer, case layer, case value, refusal, empty provenance, foreign
   layer) each fail the comparator with the diverging key named.

The oracle compares the whole verdict across four fixed environments —
one per layer that can decide a declared key — and pins precedence in both
directions on the same keys (`session.chain_id` file then env; `nodes.http`
file then env with a per-entry env override that leaves a sibling entry on the
file). The unprefixed `VERIFICATION_RETRY_*` family is covered because a naming
exception is exactly where a divergence would hide.

### D5 — Resolution is a constructible value; the two doors are not interchangeable

The frozen verdict's `&'static` borrow makes re-installation type-incompatible,
so a test that wanted a different input had to become a different process. Four
private-seam hypothetical entries now answer **how** the cascade resolves
explicit inputs:

- `resolve_hypothetical(env, file)` — the whole projection, as a
  `HypotheticalConfig`;
- `resolve_hypothetical_node_uri`, `resolve_hypothetical_chain_id`,
  `resolve_hypothetical_database_path` — the argument-taking cascade methods as
  standalone siblings.

Each is a pure function of its inputs (a captured environment over the named
file, or the standard file the captured environment selects), installs
nothing, and is reachable only from the raw FFI seam (ADR-013) — deliberately
not re-exported from `degenbot.config`. `HypotheticalConfig` is its own value
type because a `ResolvedConfig` cannot be built without the process-wide
`OnceLock`.

Two doors follow, and using the wrong one is a tautology:

- `resolve_hypothetical(...)` for claims about **how** the cascade resolves
  inputs.
- `resolved_config()` for claims about **what this process installed**.

Because the hypothetical installs nothing, the parity oracle resolves every
recorded environment in **one process** instead of forking a fresh interpreter
per environment.

### D6 — Raw-table readers resolve the file the verdict installed

Two tables are deliberately outside the typed schema: `[failure_policy]`
(ADR-040 D3) and `[deployments]`, the Python deployment-registry overlay. Both
are read as raw tables from the file the loader selected. The deployment
overlay reads the path through `config_file_path()`, which is the verdict's
record of the selected file; the failure-policy reader is handed
`standard_file_path()` at module init, the same load the holder received.
Neither re-derives the discovery rule, and the licence to carry a key the typed
schema does not declare is a property of the **reader**, not of when the file
was chosen. Those are independent axes — the overlay is what makes it matter,
because it carries the factory and init-hash identity of every pool the process
will look up, and a reader that re-resolved could take pool identity from one
file while every typed value came from another.

## Boundaries worth stating explicitly

- **`DEGENBOT_DEBUG` is a ratified Python-only read.** It is output plumbing in
  the `RUST_LOG` class — where records go, not how the bot is configured — and
  therefore outside the schema's reach. It is one of three entries in the
  companion half of the env-read gate's allowlist, each a named limit rather
  than an approved leak; the other two are presence refusals that honour
  nothing.
- **`Source::Cli` exists on the resolution surface, not in declared-key
  provenance.** The installed verdict is built from `load_process_config()`,
  and that load has no CLI layer. The FFI exposure of `cli` is the
  explicit-override argument on the resolution methods (`node_uri`,
  `resolve_chain_id`, `resolve_database_path`), so those layers are recorded
  where the layer genuinely exists. The parity oracle follows the verdict
  rather than reshaping it to make the comparison easier.
- **Two provenance keyings, one object.** `provenance` is keyed by dotted TOML
  path and `entry_provenance` by env family. They describe the same load at two
  granularities; neither is derived from the other, and each keying is the
  vocabulary its consumer already speaks.

## Consequences

The driver reads one object. A new schema key reaches Python with no FFI edit
and is compared by the oracle with no fixture edit. A diagnostic can print the
endpoint and the layer that supplied it without a reader re-deriving the
cascade, and a key reported through the wrong path fails the parity gate rather
than passing a value-only check.

The cost is a seam that is deliberately private: the hypothetical entries are
not part of `degenbot.config`, and a test that wants a second cascade must
reach the raw FFI. That is the price of making the installed verdict a value
rather than a process-wide mutable. The alternative — an install that can be
replaced — would put the whole FFI surface behind a lock to buy a testability
convenience the hypothetical door already provides.

What this does not do: it does not give Python a config model, it does not
relax the fail-loud posture when no layer supplies a value, and it does not
replace ADR-062's four-layer order or ADR-063's `${env:NAME}` expansion. It
records who is allowed to answer a configuration question, and how a second
answer is caught.
