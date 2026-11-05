# Gate ratchets

An index of the repo's **ratchets** and the `architecture_gates.rs` family: a
ratchet is a gate that fails on *new* occurrences of a defect class while
grandfathering the existing ones in a census, so the count can only shrink.
This page is the entry point for finding the right gate, reading its census,
and adding an entry correctly (the capability-home gates are decided by
[ADR-084](adr/ADR-084-capability-homes.md)). Each ratchet below lists the
failure class it prevents (with the incident where known), where the gate body
lives, where its census/data file lives, how to add an entry, and how to run it
locally.

The eight ratchets:

| Ratchet | Gate body | Census / data | Local command |
| --- | --- | --- | --- |
| Comment hygiene | `scripts/hooks/comment-hygiene.sh` | `scripts/hooks/comment-hygiene-census.txt` (+ `-waivers.txt`) and `scripts/hooks/comment-hygiene-citation-census.txt` (Detector 3) | `just lint-comment-hygiene` |
| Config-derived statics | `rust/crates/facade/degenbot/tests/architecture_gates.rs::config_derived_statics_are_classified` | `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt` | `just check-config-derived-statics` |
| Worker census | `rust/crates/foundation/degenbot-core/src/worker_census.rs` (+ `pubsub_runtime.rs` live counts) | the `register(...)` call sites themselves | `cargo test -p degenbot-core worker_census` |
| Stub drift | `prek.toml` `stub-drift-staged` -> `just gen-stubs --check`; `tests/ffi/test_ffi_stub_drift.py` (compile-coupled test carries the `stub_drift` marker; runs once as the CI `stub-drift` job) | the committed `src/degenbot/_ffi/*.pyi` tree | `just lint-stub-drift` |
| pytest noise gate | `scripts/pytest_noise_gate.sh` | the `signatures` array in that script | `just test-python-noise-gate` |
| Lint-waiver census | `scripts/hooks/too_many_lines_waivers.py` | `scripts/hooks/too-many-lines-waiver-census.txt` | `just check-too-many-lines-waivers` |
| Capability catalog | `rust/crates/facade/degenbot/tests/architecture_gates.rs::capability_catalog_covers_every_public_crate` | `docs/capabilities.md` (the catalog itself) | `just check-capability-catalog` |
| Dependency charter | `rust/crates/facade/degenbot/tests/architecture_gates.rs::dependency_charter_is_respected` | `rust/crates/facade/degenbot/tests/dependency_charter.txt` | `just check-dependency-charter` |

---

## 1. Comment hygiene

**Failure class.** Dead work-tracker citations and gate-state narration in code
comments. The 2026-09-15 tech-debt survey (`docs/findings/2026-09-15-tech-debt-survey.md`)
found 237 in-code tracker citations, zero resolvable. The rules live in
[comment hygiene](comments.md) and `AGENTS.md` ("Comment Hygiene").

**Gate body.** `scripts/hooks/comment-hygiene.sh`, three detectors:

- Detector 1 (anchored) — `ergo <ID>` / `epic|task|slice <ID>` citations,
  migration-work-item labels (`ADR-005 slice 14j`, `slice 14l`, `Plan 102`,
  `plan-101` and the `slice-3b` hyphen form; the pattern requires a digit after
  the label word, so data-slice prose and the `plan(..)`/`plan[..]` call and
  index forms survive), and RED/GREEN gate narration co-occurring with a
  six-char ID token, scanned over
  the *working tree* (`rg`, not `git grep`) for the code-ish file classes
  (`*.py`, `*.rs`, `*.pyi`, `*.toml`, `*.yml`, `*.yaml`, `*.sh`, `*.sol`, and
  the extensionless `justfile`; minus `executor/**` and the
  `tier3-oracle/src*` harness sources, whose bytes are sha256-pinned in
  `tier3-oracle/artifacts/manifest.json` — a comment edit there fails the
  committed-artifact match tests until a solc/vyper rebuild). Build recipes
  carry the same prose a code comment does — the 2026-09-15 survey's
  follow-up sweep found tracker citations in `justfile`, CI workflows,
  `prek.toml`, the root `Cargo.toml`, crate manifests, Grafana rule
  comments, and the tier3-oracle build scripts once those classes were
  scanned, so they stay in scope. It scans
  the working tree on purpose: an index-bound enumeration once went green on a
  commit containing exactly the citations this gate exists to catch, so a
  brand-new untracked source file must be examined.
- Detector 2 (the bare-ID census ratchet) — any bare six-char `[A-Z0-9]{6}`
  token with no anchor word that is not accounted for in the census. Scan
  paths mirror Detector 1's file classes (`src tests examples` plus the build
  surface: `justfile`, `*.toml`, `*.yml`/`*.yaml`, `*.pyi`, `*.sol`,
  `.github/**`, `tier3-oracle/**`, `scripts/`, `run_bot.sh`); `*.rs` files are
  scanned on comment lines only, every other file class on all lines.
- Detector 3 (the citation census ratchet) — the shapes the first two cannot
  see, case-insensitive and word-boundary anchored: `TODO-<hex>` in any case
  and `TODO(<word>)` tracker IDs, `Epic <token>` / `slice <token>` work-item
  labels, `Phase <letter><digit>` labels, and `pre-`/`post-epic` narration.
  Bare `phase <N>` is deliberately OUT of scope: that prose is algorithm prose
  (the eager-vs-boundary-tail phases in
  `foundation/degenbot-pools/src/state/tick_bitmap.rs`, the pre-solve gap
  phases in the bot's `instruments.rs`, the GIL phases in `result_channel.rs`),
  so the phase shape wants a letter+digit token and the slice shape a token
  containing a digit. Scope: Detector 1's working-tree file classes minus this
  gate's own hook and census files (they spell the shapes by construction);
  markdown stays out.

**Census / waivers.**

- `scripts/hooks/comment-hygiene-census.txt` — the bare-token lexicon. The whole
  tree is currently swept, so this file is **lexicon-only** (one bare token per
  line, sorted). Extend it ONLY for a real word, acronym, or domain identifier
  the matcher trips on.
- `scripts/hooks/comment-hygiene-waivers.txt` — one repo-relative path per line;
  a waiver means "this file still carries pre-rule citations" and never licenses
  new ones. Empty today.
- `scripts/hooks/comment-hygiene-citation-census.txt` — the Detector 3 census,
  one `path:<matched band>` row per grandfathered site (the band exactly as
  `rg -o` prints it, whitespace collapsed, so a row survives a line reflow and
  disappears only when the citation does). The tree is swept: the one row left
  is a reviewed non-citation, with its reason written above it. The
  reconciliation is **bidirectional and fails closed** — an unlisted citation
  fails (a new citation landed), a listed row with no citation fails (a swept
  row has to be deleted in the same change), and a missing census file fails
  instead of passing over an empty census.

**Adding an entry.** Strip the citation from the code instead — a tracker
citation must NEVER be added to the census. The same holds for a migration
label: restate the rule it introduced in prose (or delete the sentence where
the surrounding text already carries it), and never waive one, because the
label names an ephemeral work item, not a fact about the code. Only a genuine identifier the
matcher false-positives on goes into the lexicon (the known shape is the
line-start deref: a Rust line whose first non-space char is `*` is read as a
block-comment continuation, so a six-char UPPERCASE static name on it trips the
ratchet; the fix is the lexicon or restructuring the line). For a deliberate,
explicitly reviewed exception, a census line containing `/` is read as a
grandfathered `path:token` citation — that form is currently unused.

Detector 3's census is never the answer for a citation either: strip the band,
and delete its row in the same change — the stale-row check turns a forgotten
row into a red gate. Regenerate the rows after a sweep with
`CCF=/dev/null scripts/hooks/comment-hygiene.sh 2>&1 | sed -n 's/.* -> census
row: //p' | sort -u`, then delete the rows for the sites this change swept. A
row survives only for a reviewed non-citation; the standing example is
`slice V3/V4` in `degenbot-decoders/src/uniswap_tick_range.rs`, the algorithm's
slicing verb over a protocol-version pair.

**Run it.** `just lint-comment-hygiene` (also the `comment-hygiene` pre-commit
hook; it is the one documented exception to staged scoping — `pass_filenames =
false`, it sweeps the working tree). For a red probe, run it with a census
forced empty: `CHF=/dev/null scripts/hooks/comment-hygiene.sh` fails on every
bare token, and `CCF=/dev/null scripts/hooks/comment-hygiene.sh` fails on every
Detector 3 band. `tests/test_comment_hygiene_gate.py` is Detector 3's committed
negative probe: it runs the real hook with `HCR` pointing the scan at a
throwaway tree (the remaining override; `WAF`/`CHF`/`CCF` point at `/dev/null`)
and requires the red path, every shape firing on its own source, both ratchet
directions, and the fail-closed missing census.

---

## 2. Config-derived statics

**Failure class.** A `OnceLock`/`LazyLock`/`OnceCell`/`Once<...>` process-once
static whose initialization path reads the config holder freezes a snapshot;
the `reinstall_for_tests` seam then leaves it stale-able. Chunk SERRGX made the
holder seam safe by auditing every such static, but that audit is a **snapshot**
(chunk SERRGX's result doc — removed in the `docs/ergo-results/` cleanup;
recoverable from git history) — the next holder-derived static would land
silently stale-able. This census makes the audit durable.

**Gate body.**
`rust/crates/facade/degenbot/tests/architecture_gates.rs::config_derived_statics_are_classified`
(a cargo test on the umbrella crate, recipe alias below). The detector is a
lexical direct scan: a `static NAME: <guard> = <init>;` candidate is a hit when
its initialization path — its own `= <init>;` plus the argument of every
name-associated `NAME.get_or_init(..)` / `NAME.set(..)` / `NAME.lazy(..)` — reads
a holder accessor (`holder::config(`, `holder::config_arc(`, or the
`stance::config(` delegate). Strings and comments are masked; `#[cfg(test)]`
items and `tests/`/`benches/`/`examples/` files are skipped. Known blind spots
(transitive init through helper fns, e.g. `runtime::RUNTIME`; leak-on-publish
`RwLock<Option<&'static T>>` slots) are recorded by name in the census header.

**Census.** `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt`,
one line per hit:

```
<repo-relative-path>:<STATIC_NAME>  <class>  — <one-line justification>
```

The gate is **bidirectional**: an unlisted hit fails (a new stale-able static
landed) and a listed-but-absent hit fails (a stale entry — shrink it in the same
change).

**Adding an entry.** Classify the new static as one of:

- `per-read` — re-reads the holder on each use (not actually frozen);
- `republishable` — refreshed by the test seam (leak-on-publish slot);
- `process-lifetime` — frozen at first init by design; the seam deliberately
  does not re-size/re-read it.

Then append the census line with a one-line justification. Stop deriving the
static from the holder if none of the three classes is honest.

**Run it.** `just check-config-derived-statics` (the
`rust-config-derived-statics` pre-push hook; also a dependency of
`lint-rust-check`, so `just lint` and CI run it). The recipe body is
`cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test
architecture_gates -- config_derived_statics_are_classified --exact --nocapture`.

---

## 3. Worker census

**Failure class.** An unregistered execution resource — a tokio runtime, a
worker seat pool, a semaphore capacity, a sidecar/drainer thread — is invisible
to the census gauge, the boot dump, and the `/proc` comm cross-check. The
incident: two concurrent multi-thread tokio runtimes both defaulted to
`tokio-runtime-worker`, making thread dumps unattributable.

**Gate body.** `rust/crates/foundation/degenbot-core/src/worker_census.rs`. Every
execution resource self-registers one `WorkerCensusEntry` (`resource`, `kind`,
`count`, `thread_name`, `sizing`, `binding`) into one process-wide table;
`snapshot()` returns the sorted table (the `resource` label of the
`degenbot_worker_census{resource=...}` gauge, exported by `degenbot-bot::instruments`),
and `emit_boot_table()` fires the one structured boot line. Registrations after
the boot dump each emit their own `info!` line. The unit tests in the module
(including `comm_histogram_census_matches_registry`) are the gate body.

The live-count case is
`rust/crates/foundation/degenbot-core/src/pubsub_runtime.rs`: `register` UPSERTS
by `resource`, so N owners each registering a fixed `count: 1` would leave one
row still saying `1` and undercount the fleet. `OwnedPubsubRuntime` keeps a
process-global per-resource live counter (incremented on `build`, decremented in
`Drop`) and re-publishes the current value, so the snapshot reports the true
fleet size at dump time. That is how `fork_pubsub_workers` and
`hosted_pubsub_workers` stay accurate.

**Census.** There is no separate file: the census *is* the set of
`worker_census::register(...)` call sites (the module docs enumerate the
registered ids and state the rule: if you add a spawn site — any
`thread::Builder::new`, a solve-executor `tokio::runtime::Builder`, or a capacity
constant that bounds concurrent execution — you MUST register it).

**Adding an entry.** At the new spawn site, call
`degenbot_core::worker_census::register(WorkerCensusEntry { resource, kind,
count, thread_name, sizing, binding })` with a distinct `thread_name` pattern
(two runtimes must never share `tokio-runtime-worker`) and a stable,
low-cardinality `resource` id (reuse an existing id rather than mint a
near-duplicate). For a live fleet, use `OwnedPubsubRuntime` so the count tracks
instances rather than freezing at first registration.

**Run it.** `cargo test -p degenbot-core worker_census` (4 tests). The runtime
surface is also exercised by the full Rust suite (`just test-rust`).

---

## 4. Stub drift

**Failure class.** The committed `src/degenbot/_ffi/*.pyi` tree is generator
output (ADR-066 D2/D3); a seam change can leave it stale, and hand edits over
generated output re-enter the drift blind spot. The hook exists because a worker
claimed a regeneration it had not actually performed — the committed tree was
stale while the report said otherwise.

**Gate body.** Two faces:

- `prek.toml` `stub-drift-staged` — a pre-commit hook that fires only when a
  staged file touches the binding crate's `src/`/`Cargo.toml` or the stub
  generator, then runs `just gen-stubs --check` (non-mutating; `REGEN_STUBS`
  unset).
- `tests/ffi/test_ffi_stub_drift.py` — the pytest face:
  `test_committed_stubs_carry_generator_header` and
  `test_config_projection_stubs_are_typed` stay in the default suite (cheap,
  committed-tree-only). `test_committed_stubs_match_generator_output` (which
  shells out to `just gen-stubs --check`) is compile-coupled and carries the
  `stub_drift` marker, so the default pytest selection excludes it; it runs once
  as the dedicated CI `stub-drift` job and as the pre-push `stub-drift` gate. For
  a direct local run it probes cargo's debug build lock
  (`rust/target/debug/.cargo-build-lock`) and fails with a distinct message when
  another lane is mid-build.

**Data file.** The committed `src/degenbot/_ffi/*.pyi` tree, regenerated by
`just gen-stubs`; the generator is `rust/tools/stubgen` (`degenbot-stubgen`).

**Adding an entry.** Never hand-edit a stub. After a seam change, run
`just gen-stubs` and commit the regenerated tree. Generator gaps live only in
`tests/ffi/stubtest_allowlist.txt` (ADR-066 D3), never in a hand edit.

**Run it.** `just lint-stub-drift` (an alias of `just gen-stubs --check`) — it
cargo-builds the cdylib with the temporary `experimental-inspect` feature, so
run it with exclusive use of the build graph — a concurrent `just dev`/`cargo
build` can relink the `.so`. It runs once per CI run as the dedicated
`stub-drift` job and as the `stub-drift` pre-push gate; the default pytest
selection excludes the pytest face via the `stub_drift` marker (run it directly
with `pytest -m stub_drift`). `REGEN_STUBS=1 just gen-stubs --check` opts into
writing, mirroring the `REGEN_*` pattern of `docs/rust-config-keys.md`.

---

## 5. pytest noise gate

**Failure class.** The historical "exception spray": after the pytest session
ended, `--- Logging error ---` blocks and raw `Reconnection attempt` lines
printed to the real stdout/stderr of the test process while the exit code stayed
`0`. Post-session output never reaches pytest plugins, so the tripwire cannot
live inside the suite (see the PUI7HC and KL2EZ3 result docs — removed in
the `docs/ergo-results/` cleanup; recoverable from git history).

**Gate body.** `scripts/pytest_noise_gate.sh`. `run [pytest args...]` captures the
whole child-process output (`2>&1 | tee`), reads the suite exit from
`PIPESTATUS[0]` **separately** from the scan, then scans the capture for the
signatures. Exit codes: `0` clean, `3` a spray signature, the suite's own
nonzero code propagated unchanged otherwise, `2` usage error. `scan <logfile>`
scans an existing capture (the negative-probe entry point).

**Data file.** The `signatures` array at the top of the script
(`'--- Logging error ---'`, `'Reconnection attempt'`).

**Adding an entry.** Add the new spray signature to that `signatures` array.

**Run it.** `just test-python-noise-gate` (also the `python-test` pre-push hook
and the CI python job's "Run Tests" step in `.github/workflows/ci.yml`); the
single positional is a pytest keyword filter, trailing tokens forward verbatim.
The bare `just test-python` recipe stays for callers that only want the suite's
exit code. Red probe without re-running the suite:

```bash
printf '%s\n' 'Reconnection attempt' > /tmp/spray.log
bash scripts/pytest_noise_gate.sh scan /tmp/spray.log   # exits 3, prints the line
```

---

## 6. Lint-waiver census

**Failure class.** An expected `clippy::too_many_lines` lint is a decision to keep
one function long, and the decision is worth what its recorded reason is worth: a
waiver with no reason is, in a later review, indistinguishable from a function nobody
got round to splitting. The population is large (161 sites in the Rust tree at this
census) and nothing watched it — a new waiver could land with no reason, and a reason
could stop being stated in the source while a document kept quoting it.

**Gate body.** `scripts/hooks/too_many_lines_waivers.py`. A *site* is a lint attribute
whose list names `clippy::too_many_lines`, spelled either as an outer attribute (on the
item that follows it) or as an inner `#![expect(...)]` (a waiver over the whole file).
Each site is keyed

```
<repo-relative-path>::<scope>
```

where the scope is the annotated item's name (the identifier after its declaration
keyword: `fn`, `mod`, `struct`, `impl`, ...) or `!module` for an inner attribute. Line
numbers are deliberately absent — a row has to survive every edit above it and vanish
only when the waiver does. Two same-named items in one file carry an occurrence suffix
(`::walk`, `::walk#2`); the nested `walk` helpers in the executor's grammar plan are
the only case today.

The reconciliation is **bidirectional and fails closed**:

- a site with no census row fails — a new waiver cannot land silently;
- a row with no site fails — a split function deletes its row in the same change;
- a row's reason must equal the reason its site states, both ways — a stated
  `reason = "..."` (or trailing `//` comment) that the row does not carry fails, and a
  row reason the source no longer states fails;
- a missing census, a census with no ceiling header, or a scan that found no Rust file
  at all fails instead of certifying a tree it did not examine.

Exit codes: `0` reconciled, `1` findings, `2` refused to certify.

**Census.** `scripts/hooks/too-many-lines-waiver-census.txt`, one row per site:
`<key>` then, when the site states one, a TAB and that reason **verbatim** from the
source (a `reason = "..."` argument or the trailing comment on the attribute),
whitespace collapsed onto one line so the row survives a rewrap. The census mirrors the
source and never improves on it — that mirroring is what keeps the two from drifting.
The `!module` scope is the inner-attribute form.

The header's `# reasonless-high-water: N` freezes the count of sites that state no
reason at all: still a reviewed decision, but an unexplained one and the first
candidates for a split. That number moves only downward, so a new reasonless waiver
fails even if its row is added; every shrink lowers the number in the same change.

Documented blind spots: the `allow(...)` spelling of the same waiver is out of scope
(the workspace denies that attribute at the lint root, so the expectation form is the
only live spelling), a waiver inside a macro expansion is invisible to a source scan,
and the ceiling is a reviewed baseline rather than a proof — widening it is a visible
diff line, which is the most a census can do.

**Adding an entry.** Strip the waiver and split the function; that is the shrink this
ratchet exists to make routine. A waiver that must stay states its reason in the
source (`reason = "..."`, or a trailing comment when the attribute is already long) and
mirrors that reason into its row. Never add a row without a reason, and never widen the
ceiling.

**Regenerating.** `just check-too-many-lines-waivers --rows` prints the rows the tree
implies (sorted, reasons included); paste them over the rows section. The header and
the ceiling are maintained by hand.

**Run it.** `just check-too-many-lines-waivers` — also a dependency of
`lint-rust-check`, so `just test`, the pre-push `rust-clippy` gate and CI run it over a
complete tree, and the `too-many-lines-waivers` pre-commit hook runs it when a commit
touches a Rust source or one of the gate's own two files.
`tests/test_too_many_lines_waiver_gate.py` is the committed negative probe: it drives
the real gate with `--root` pointed at a throwaway tree and requires the red path for a
fresh site, both reason-drift directions, the stale row, the breached ceiling, and the
fail-closed exits.

---

## The `architecture_gates.rs` family

`rust/crates/facade/degenbot/tests/architecture_gates.rs` is the idiom the
config-statics gate follows: cargo tests on the umbrella crate, with the crate
list derived from `cargo metadata` so the scans run under every `cargo test`
track, and thin justfile recipes as aliases (ADR-043 §10-style precedent). The
current gates:

- `workspace_membership_is_exact_and_role_grouped`
- `cli_shell_names_only_allowlisted_externals`
- `core_crates_are_pyo3_free_under_default_features`
- `cli_core_is_clap_and_indicatif_free`
- `one_engine_impl_block`
- `no_inner_allow_attributes`
- `one_path_identity_owner_is_declared_once`
- `the_session_registers_no_path_store_of_its_own`
- `the_canonical_path_object_carries_no_strategy_policy`
- `one_position_observer_implementation_is_declared_once`
- `the_session_registers_no_position_store_of_its_own`
- `the_position_seam_adds_no_lending_edge_to_the_engine_or_the_core`
- `the_position_seam_sits_in_the_layer_both_sides_depend_on`
- `no_second_map_in_the_rust_core_is_keyed_by_a_session_identity_type`
- `core_public_docs_do_not_define_contracts_through_python_symbols`
- `no_timeout_wrapped_value_sends_or_write_all`
- `config_derived_statics_are_classified`
- `capability_catalog_covers_every_public_crate` (ratchet #6 — cataloged in the table above)
- `dependency_charter_is_respected` (ratchet #7 — cataloged in the table above)
- `role_edge_direction_is_enforced` (ADR-084 D4 — upward role edges over `tests/role_edge_direction_census.txt`, allowlist empty at the W4 landing)

Several are aliased as `check-*` recipes (`check-no-inner-allow`,
`check-engine-impl-blocks`, `check-cli-shell-purity`,
`check-config-derived-statics`), which `lint-rust-check` runs before clippy.

---

## Adding a new ratchet

Two idioms cover the family. Pick the one that matches what the gate scans.

**Script + lexicon/census** (comment hygiene, pytest noise gate, lint-waiver
census). A script under `scripts/` holds the detector and the exit-code
contract — shell where the detector is one `rg` pass, Python where the parse is
more than a filter (`scripts/hooks/tripwire_ratchet.py`,
`scripts/hooks/too_many_lines_waivers.py`); a plain-text census or a
`signatures`-style array beside it holds the grandfathered entries. The script must (a) fail closed when its census is
missing, (b) run one pass (no per-token subprocesses) if it rides the
pre-commit tier, and (c) state its scope and blind spots in its header. Wire it
as a `just` recipe and, if it is instant, a `prek.toml` hook (file-scoped where
possible; `pass_filenames = false` only when it must sweep the tree).

**Cargo architecture-gate test + census file** (config-derived statics). Add a
`#[test]` to `architecture_gates.rs` that scans the workspace (derive crates
from `cargo metadata`), and a sibling `<name>_census.txt` whose header states
the detector, the classes, the format, and the documented blind spots. Make the
gate **bidirectional** (unlisted hit fails; stale listed entry fails). Alias it
as a `check-*` recipe and add that recipe to `lint-rust-check`, plus a
`prek.toml` pre-push hook. Use the `REGEN_*` opt-in pattern for any gate that
regenerates an artifact (`just gen-stubs`, `REGEN_CONFIG_DOCS=1`).
