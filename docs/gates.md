# Gate ratchets

An index of the repo's **ratchets** and the `architecture_gates.rs` family: a
ratchet is a gate that fails on *new* occurrences of a defect class while
grandfathering the existing ones in a census, so the count can only shrink.
This page is the entry point for finding the right gate, reading its census,
and adding an entry correctly. Each ratchet below lists the failure class it
prevents (with the incident where known), where the gate body lives, where its
census/data file lives, how to add an entry, and how to run it locally.

The five ratchets:

| Ratchet | Gate body | Census / data | Local command |
| --- | --- | --- | --- |
| Comment hygiene | `scripts/hooks/comment-hygiene.sh` | `scripts/hooks/comment-hygiene-census.txt` (+ `-waivers.txt`) | `just lint-comment-hygiene` |
| Config-derived statics | `rust/crates/facade/degenbot/tests/architecture_gates.rs::config_derived_statics_are_classified` | `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt` | `just check-config-derived-statics` |
| Worker census | `rust/crates/foundation/degenbot-core/src/worker_census.rs` (+ `pubsub_runtime.rs` live counts) | the `register(...)` call sites themselves | `cargo test -p degenbot-core worker_census` |
| Stub drift | `prek.toml` `stub-drift-staged` -> `just gen-stubs --check`; `tests/ffi/test_ffi_stub_drift.py` (compile-coupled test carries the `stub_drift` marker; runs once as the CI `stub-drift` job) | the committed `src/degenbot/_ffi/*.pyi` tree | `just lint-stub-drift` |
| pytest noise gate | `scripts/pytest_noise_gate.sh` | the `signatures` array in that script | `just test-python-noise-gate` |

---

## 1. Comment hygiene

**Failure class.** Dead work-tracker citations and gate-state narration in code
comments. The 2026-09-15 tech-debt survey (`docs/findings/2026-09-15-tech-debt-survey.md`)
found 237 in-code tracker citations, zero resolvable. The rules live in
[comment hygiene](comments.md) and `AGENTS.md` ("Comment Hygiene").

**Gate body.** `scripts/hooks/comment-hygiene.sh`, two detectors:

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

**Census / waivers.**

- `scripts/hooks/comment-hygiene-census.txt` — the bare-token lexicon. The whole
  tree is currently swept, so this file is **lexicon-only** (one bare token per
  line, sorted). Extend it ONLY for a real word, acronym, or domain identifier
  the matcher trips on.
- `scripts/hooks/comment-hygiene-waivers.txt` — one repo-relative path per line;
  a waiver means "this file still carries pre-rule citations" and never licenses
  new ones. Empty today.

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

**Run it.** `just lint-comment-hygiene` (also the `comment-hygiene` pre-commit
hook; it is the one documented exception to staged scoping — `pass_filenames =
false`, it sweeps the working tree). For a red probe, run it with the census
forced empty: `CHF=/dev/null scripts/hooks/comment-hygiene.sh` fails on every
bare token.

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
- `cmd_executor_cutover_symbols_are_absent_from_strategy_callers`
- `no_alembic_references`
- `one_path_identity_owner_is_declared_once`
- `the_session_registers_no_path_store_of_its_own`
- `the_canonical_path_object_carries_no_strategy_policy`
- `one_position_observer_implementation_is_declared_once`
- `the_session_registers_no_position_store_of_its_own`
- `the_position_seam_adds_no_lending_edge_to_the_engine_or_the_core`
- `the_position_seam_sits_in_the_layer_both_sides_depend_on`
- `no_second_map_in_the_rust_core_is_keyed_by_a_session_identity_type`
- `retired_python_dedup_maps_do_not_reappear`
- `core_public_docs_do_not_define_contracts_through_python_symbols`
- `no_timeout_wrapped_value_sends_or_write_all`
- `config_derived_statics_are_classified`

Several are aliased as `check-*` recipes (`check-no-inner-allow`,
`check-engine-impl-blocks`, `check-cli-shell-purity`,
`check-config-derived-statics`), which `lint-rust-check` runs before clippy.

---

## Adding a new ratchet

Two idioms cover the family. Pick the one that matches what the gate scans.

**Script + lexicon/census** (comment hygiene, pytest noise gate). A shell
script under `scripts/` holds the detector and the exit-code contract; a
plain-text census or a `signatures`-style array beside it holds the
grandfathered entries. The script must (a) fail closed when its census is
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
