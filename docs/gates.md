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

The nine ratchets:

| Ratchet | Gate body | Census / data | Local command |
| --- | --- | --- | --- |
| Comment hygiene | `scripts/hooks/comment-hygiene.sh` | `scripts/hooks/comment-hygiene-census.txt` (+ `-waivers.txt`) and `scripts/hooks/comment-hygiene-citation-census.txt` (Detector 3) | `just lint-comment-hygiene` |
| Config-derived statics | `rust/crates/facade/degenbot/tests/architecture_gates.rs::config_derived_statics_are_classified` | `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt` | `just check-config-derived-statics` |
| Worker census | `rust/crates/foundation/degenbot-core/src/worker_census.rs` (+ `pubsub_runtime.rs` live counts) | the `register(...)` call sites themselves | `cargo test -p degenbot-core worker_census` |
| Stub drift | `prek.toml` `stub-drift-staged` -> `just gen-stubs --check`; `tests/ffi/test_ffi_stub_drift.py` (compile-coupled test carries the `stub_drift` marker; runs once as the CI `stub-drift` job) | the committed `src/degenbot/_ffi/*.pyi` tree | `just lint-stub-drift` |
| pytest noise gate | `scripts/pytest_noise_gate.sh` | the `signatures` array in that script | `just test-python-noise-gate` |
| Capability catalog | `rust/crates/facade/degenbot/tests/architecture_gates.rs::capability_catalog_covers_every_public_crate` | `docs/capabilities.md` (the catalog itself) | `just check-capability-catalog` |
| Dependency charter | `rust/crates/facade/degenbot/tests/architecture_gates.rs::dependency_charter_is_respected` | `rust/crates/facade/degenbot/tests/dependency_charter.txt` | `just check-dependency-charter` |
| Publish-blocked census | `scripts/publish_blocked.py` | `scripts/publish-blocked-census.txt` | `just check-publish-blocked` |
| Tool catalog | `scripts/tool_catalog_check.py` | `rust/tools/README.md` (the living catalog itself) | `just check-tool-catalog` |

### The gate registry

`scripts/gates.toml` declares every git hook exactly once: its id, stage, entry
command, file trigger and flags. `prek.toml` stays the file git loads, but its
hook lines are rendered from the registry by `just gates-regen`. The prose above
a hook line stays in `prek.toml`, beside the gate it explains — only the facts
are generated.

- **Add a hook.** Append a `[[gate]]` block, grouped by stage, then run
  `just gates-regen` and write its rationale comment in `prek.toml`. The
  registry validates before it renders: a duplicate id, an unknown stage, a
  `just` recipe the justfile does not define, a script path that does not exist,
  or gates out of stage order fails with the reason named.
- **Retire a hook.** Delete the block and run `just gates-regen`; the line and
  its prose go with it ([ADR-087](adr/ADR-087-deleting-code-is-the-complete-retirement.md)
  — no waiver, no tombstone).
- **Never hand-edit a hook line.** `just gates-check` regenerates and diffs, and
  the `gate-registry` pre-commit hook runs it, so a hand-edited line cannot
  outlive its commit. `ci-parity` extracts entries with a line-anchored `sed`,
  so every hook line stays on one line; the renderer guarantees that shape.

The decision, its rejected alternatives and the remaining migration phases are
[ADR-102](adr/ADR-102-gate-registry-with-generated-wiring.md).

### Repairing a red gate

Every gate's red path ends with a `fix:` line naming its repair, so a failure is
actionable without reading the detector. Where the data is derivable, that line
is a command; where the repair is an edit, it names the file and the rule.

| Gate | Repair |
| --- | --- |
| Comment hygiene (citation) | Strip the citation (docs/comments.md); delete the census row it leaves in the same change |
| Comment hygiene (bare token) | Add the identifier to `scripts/hooks/comment-hygiene-census.txt` (lexicon section), or strip the token |
| Config-derived statics | Update `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt` |
| Worker census | Register the worker through `degenbot-core`'s `worker_census` |
| Stub drift | `just gen-stubs` |
| pytest noise gate | Fix the call site that sprays (the signatures live in `scripts/pytest_noise_gate.sh`) |
| Capability catalog | Add the crate to `docs/capabilities.md` |
| Dependency charter | Update `rust/crates/facade/degenbot/tests/dependency_charter.txt` |
| Publish-blocked census | `just check-publish-blocked --rows > scripts/publish-blocked-census.txt` |
| Tool catalog | Add or repair the row in `rust/tools/README.md`, then re-run `just check-tool-catalog` |
| Parity ledger | Reconcile `docs/architecture/rust-settlement-bot-parity.md` with the boot oracle |
| Executor release ratchet | `python3 scripts/executor_release_ratchet.py generate`, only while adding a release |
| Retirement-tripwire ratchet | Delete the artifact, or label it with the D2 marker naming the present code it watches |
| rustfmt / clippy / python lint | `just format`, `just lint-rust`, `just lint-python` |

### What each ratchet caught

A gate earns its place by the defect it caught that tests and review missed. A
row without a recorded catch is documentation consistency, not defect catching:
it survives by keeping its document true, or it is deleted (ADR-087).

| Ratchet | Recorded catch |
| --- | --- |
| Comment hygiene | 237 in-code tracker citations, none resolvable (2026-09-15 survey) |
| Config-derived statics | A process-once static freezing a snapshot of the config holder (the SERRGX seam audit) |
| Worker census | Two concurrent multi-thread tokio runtimes invisible to the census, the boot dump and the comm cross-check |
| Stub drift | A worker claimed a regeneration it had not run |
| pytest noise gate | Exception spray printed after the session ended while the exit code stayed 0 |
| Publish-blocked census | Members whose retained requirements cannot resolve on crates.io, discovered only at upload |
| Capability catalog | **None recorded** — keeps `docs/capabilities.md` complete; 24 of the last 200 commits touch it |
| Dependency charter | **None recorded** — keeps the charter list current; 2 of the last 200 commits touch it |
| Tool catalog | **None recorded** — keeps `rust/tools/README.md` complete; 53 of the last 200 commits touch it |

The three unrecorded rows are the repository's highest-churn bookkeeping, and
the churn there is substance rather than ceremony: the last six commits touching
`rust/tools/README.md` changed one to three lines each — retiring an experiment,
recording a measurement, re-pointing a row's subject. A row states something
`cargo metadata` cannot: what the target is *for*. Generating the derived cells
(name, source link, lane) would leave the prose, which is the part a person
writes, so these rows keep their place and their gate. What a missing row costs
is one line, not a table.

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
  the extensionless `justfile`; minus the
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
  scanned on comment lines only, every other file class on all lines. The
  `rust/crates/**` slice is the whole crate tree: crate `tests/`, `benches/`
  and `examples/` and inline `src/**/tests` modules are in scope, because a
  test comment cites a work item exactly as a shipped comment does and a
  shared test harness is where those citations accumulate. Only generated
  artifacts, the sha256-pinned `tier3-oracle/src*` harness sources, and this
  gate's own hook/census files are excluded.
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

**Run it.** The full Rust workspace suite executes it once. While diagnosing
a failure, `just check-config-derived-statics` runs only this filter:
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
  `stub_drift` marker, so the default pytest selection excludes it; it runs in
  the input-scoped CI `stub-drift` job when the binding, generator, stubs, or
  generation configuration changes. For a direct local run it probes cargo's debug build lock
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
build` can relink the `.so`. CI runs it when its inputs change; locally, the
staged FFI hook covers relevant commits and this recipe covers manual or
`--no-verify` workflows. The default pytest selection excludes the pytest face
via the `stub_drift` marker (run it directly with `pytest -m stub_drift`). `REGEN_STUBS=1 just gen-stubs --check` opts into
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

**Run it.** `just test-python-noise-gate` (also the CI python job's "Run Tests"
step in `.github/workflows/ci.yml`); the
single positional is a pytest keyword filter, trailing tokens forward verbatim.
The bare `just test-python` recipe stays for callers that only want the suite's
exit code. Red probe without re-running the suite:

```bash
printf '%s\n' 'Reconnection attempt' > /tmp/spray.log
bash scripts/pytest_noise_gate.sh scan /tmp/spray.log   # exits 3, prints the line
```

---

## 6. Publish-blocked census


**Failure class.** `cargo publish` rewrites every path and git dependency at
packaging: the source key is stripped and what remains is a version requirement
that must resolve on crates.io. A member whose retained requirements cannot
resolve there does not upload at all, and nothing watched which members those
were. The publish oracle (`just publish-dry-run`) then died on the first one,
taking the manifest and file-inclusion coverage of every later member down
with it — a failure that names one dependency and hides the population. The
instance behind this gate: the engine family (`evm2`, `evm2-jit`, and
siblings) is vendored under names we own (`rust/vendor/`, ADR-096) because
crates.io holds only upstream's name reservation. The git-declaration shape
this gate watches is exactly what held the engine closure
(`degenbot-simulation`, `degenbot-arbitrage`, and every member whose retained
requirements reach them) out of the publish chain; with the family vendored
the population is empty, and this gate is what keeps a git-only dependency
from silently re-entering a publishable member's manifest.


**Gate body.** `scripts/publish_blocked.py`, reconciling the census against
the static publication plan (`scripts/publication_plan.py`) over the manifests
`cargo metadata --no-deps --offline` reports. The plan is the one derivation of the
chain's static facts — eligibility (`publish != []`), the withheld population
and its reasons, the retained edges and their cycles, and the resolution set
(eligible minus withheld) the publish oracle uploads — and the census gate, the
crates.io live audit (`scripts/crates_io_pending.sh`, whose membership read is
the plan's `--eligible` CLI), and the verify oracle
(`scripts/publish_verify.py`) all consume it; no consumer restates the rule.
The gate itself owns only the census-file reconciliation below. A *site* is a dependency the published manifest
retains in rewritten form: a version-less dev-dependency is the one shape
cargo strips (so it blocks nothing); a version-less path or git dependency in
any other slot is rejected outright; a version-keyed path or git dependency
survives as that version requirement, which for a non-workspace package names
a crates.io release of code developed elsewhere — the member is withheld until
the dependency is declared as a registry dependency, which is also the only
shape in which what was tested is what was published. A member whose retained
requirement reaches a withheld member is withheld in turn, so the population
is closed over dependents.

Every retained requirement is also an upload-ordering constraint:
`cargo publish --workspace` uploads members in dependency order, and a
member's retained dev-dependencies participate — cargo resolves a member's
versioned dev-dependencies while preparing its package, including under
`--no-verify`, so the fresh sibling version must already be resolvable at
packaging time, and a retained dev edge orders its member after its target
exactly like a normal edge. The gate
therefore also fails outright on any cycle formed by registry-retained normal
and dev dependencies among publishable members, naming the cycle, because no
upload order — and so no crates.io version of the workspace — can satisfy it.
The dependency shape cargo strips at packaging (the version-less path/git
dev-dependency) is not a requirement and can never close a cycle. A cycle is
never recorded as a census row: it is a release-graph defect to repair in the
manifests, and no census exclusion may hide it.

The reconciliation is **bidirectional and fails closed**:

- a withheld member with no census row fails — the population cannot grow in
  silence, and the run cannot quietly lose coverage;
- a row with no withheld member fails — a member that packages again deletes
  its row in the same change;
- a row's reason must equal the reason the manifests imply, both ways;
- a missing census, an unparseable row, a workspace with no publishable
  member, or a `cargo metadata` failure fails instead of certifying a tree it
  did not examine.

Exit codes: `0` reconciled, `1` findings, `2` refused to certify.


**Census.** `scripts/publish-blocked-census.txt`, one row per withheld member:
`<member>` then a TAB then that reason **verbatim** from `--rows` (the git and
path dependencies the published form retains, then the withheld members those
requirements reach). The census mirrors the manifests and never improves on
them — that mirroring is what keeps the two from drifting. There is no
reasonless-high-water line here: a row's reason is computed,
so no row can state less than the site it mirrors.


**Adding an entry.** The population's destination is empty: when the withheld
members' external dependencies become registry dependencies (the engine on
crates.io, or the engine vendored as workspace members), the rows go with them
and the publish oracle covers every member again. A member that can package
deletes its row in the same change; a member entering the population lands
only as a reviewed row carrying the reason the manifests imply.


**Regenerating.** `just check-publish-blocked --rows` prints the rows the
manifests imply (sorted, reasons included); paste them over the rows section.
The header is maintained by hand.


**Run it.** `just check-publish-blocked` — also a dependency of
`lint-rust-check`, so `just test`, the pre-push `rust-clippy` gate and CI run
it over a complete tree, and the `publish-blocked` pre-commit hook runs it
when a commit touches a manifest, the census, the gate's own script, or the
publication plan. The gate and the plan import nothing outside the standard
library and drive only `cargo metadata --no-deps --offline` (the child cargo
cannot reach a registry), so a plain `python3` runs
them — the same uv-free constraint.
`tests/test_publish_blocked_gate.py` is the committed
negative probe: it drives the real gate with `--root` pointed at a throwaway
workspace and requires the red path for a fresh member, both reason-drift
directions, the stale row, the fail-closed exits, a retained normal+dev cycle
(named by the gate), and the green paths for the stripped dev-dependency shape
and an acyclic retained dev edge.

**The live release projection.** The static plan composes with the live
audit's conclusive output into one workflow-facing projection,
`scripts/publication_plan.py`'s `LivePublicationPlan`. The audit's
`--classify` mode prints one ``<live|pending> <member>`` line per eligible
member, and the planner's `--release-plan` CLI mode reads that stream on
stdin and emits the JSON contract the release workflow consumes — the
projection is the single machine-readable release contract, and no step
re-derives live/pending/withheld set arithmetic in shell:

```json
{"live": ["…"], "pending": ["…"], "withheld": ["…"],
 "verification_excludes": ["…"], "publish_excludes": ["…"],
 "fully_live": false}
```

Every list is sorted. `live` and `pending` carry the resolution members'
index states at the workspace version; withheld members land only in
`withheld`, whatever the index says — they have no resolution membership, so
they are never verify-built and (new with the projection) never uploaded:
`publish_excludes`, the `cargo publish --workspace --exclude` flag set of
the release rounds, is withheld plus live resolution. `verification_excludes`,
the `--exclude` set the pre-publish verification forwards to
`scripts/publish_verify.py`, is the live resolution members only — an
already-live member resolves against the loopback registry but gets no verify
build — and every still-pending resolution member is verified, including an
allowlisted one, which the oracle refuses loudly before any upload.
`fully_live` is true exactly when no resolution member is pending; the
pre-publish step exits there, before any packaging.

The projection fails closed: a static cycle (no upload order exists) is the
planner's refusal, exit 2; a classification line that is not
``<live|pending> <member>``, names an unknown member, names a member twice,
or omits an eligible member is a classification mismatch, exit 3 — the
defect is named on stderr and nothing is projected. The planner performs no
network I/O: the audit remains the only index reader, and each workflow
consumer (preflight, pre-publish verification, and every publish round)
re-runs the audit at its own point of use — fresh between steps and rounds —
and pipes that exact classification through `--release-plan` once. The
classification text protocol is unchanged and stays the audit's own output;
the JSON projection is not a second protocol beside it but its one
composition with the static plan.

### The publish-verify overlay

**Failure class.** `cargo publish --dry-run` resolves every member's rewritten
manifest against crates.io before any verify build — `--no-verify` does not
skip resolution. Under ADR-009 lockstep versioning a fresh release bumps every
member to a version crates.io has never seen, so at a fresh tag the plain dry
run dies on the first member another member requires, no matter the verify
mode. The real `cargo publish --workspace` survives only because it uploads
members in dependency order; a non-uploading verification cannot borrow that
ordering.
**Gate body.** `scripts/publish_verify.py`, driven by `just publish-dry-run`
and by release.yml's pre-publish verification step. For one run it stands up
ONE Cargo-owned loopback registry under `rust/target/publish-verify-overlay/`
(untracked; a loopback HTTP server serves it because cargo rejects `file://`
sparse indexes) and lets cargo drive it exactly like crates.io through two
seams: `source.crates-io.replace-with` resolves plain version requirements
(the rewritten manifests' shape) against the loopback index, and `cargo
publish --registry <name>` — with a `[registries.<name>]` table (index plus a
fixed dummy token) inside the run-owned `CARGO_HOME` — uploads through the
loopback's Publish API. Neither seam can reach crates.io's upload endpoint:
the replacement resolves only (cargo hardcodes the upload API to
https://crates.io behind it), and the named registry names only the loopback.
The registry serves the three surfaces cargo's publish flow needs: the sparse
index (entries cargo itself generates at upload), the `.crate` download tree,
and the upload endpoint, which requires the dummy token and appends one
receipt line per upload (`uploads.jsonl`) recording the loopback endpoint and
the auth that arrived — the committed probe asserts every receipt.
A run proceeds in two cargo-owned rounds over the same registry. The
population round (`cargo publish --workspace --registry <name> --no-verify`)
uploads every covered member without building: cargo packages each member,
generates the index entry from the packaged form, and polls the loopback
index until the entry is visible before its dependents are packaged — the
ordering, the metadata, and the polling are cargo's, and this script writes
none of them. The registry maps each upload payload mechanically onto the
sparse-index spelling (`version_req` becomes `req`, a dependency renamed
through `package = ...` keeps the alias in the entry dependency's `name` and
the original package name in its `package`, `dep:`-bearing features split
into `features2` behind the `v: 2` marker, the checksum is the sha256 of
the uploaded `.crate`, and the payload's optional `rust_version` and
`links` fields are carried under the same names when cargo sends them), so a
member compiled from the registry is linked under the alias its code
imports. The verification round (`cargo publish
--workspace --registry <name> --dry-run`, omitted in the `--no-verify` form)
verify-builds the pending members against the populated registry and aborts
each upload (nothing ever leaves the loopback). The lock's crates.io-sourced
external crates are mirrored verbatim from the real crates.io sparse index
into the same registry — so the loopback can never satisfy a requirement the
real registry cannot, and a pin locked to a path, a git repository, or an
alternate registry fails the run loud instead of resolving a same-named
stranger (a retained normal+dev dependency cycle among publishable members
has no upload order; the census gate rejects it outright with the cycle named
before this run uploads anything). The census
is reconciled first and its members are withheld
from the run by name, keeping their file-inclusion coverage through `cargo
package --list`. `publish = false` members are cargo-skipped and therefore
not eligible for the covered set at all, and coverage fails closed: covered
lines print only after the population round has succeeded, and the upload
receipts must name exactly the eligible covered set — a missing receipt
(certifying a member the registry never accepted) and an unexpected receipt
(an upload the run never accounted for) both fail the run. A member whose
`publish` allowlist NAMES registries fails
loud — cargo refuses to carry such a member to any other registry name, and
the one registry this run drives is the loopback (measured: `--registry
crates-io` bypasses any `[registries.crates-io]` table and uploads to the
real crates.io API, so no table spelling rescues it) —
and caller flags that would retarget the run (`--registry`, `--index`,
`--token`, `--config`, in `--x value` and `--x=value` forms) are refused
before any cargo invocation. The registry lives in a fresh per-run directory
under `rust/target/` (two runs never delete each other's served files) and is
served over loopback on an ephemeral port — no fixed port can be pre-bound by
something else, and concurrent runs cannot collide — and is removed when the
run ends. Every cargo invocation of a run executes with `CARGO_HOME` pointed
at the run's own directory inside that directory: the named-registry token
and source table live there, the loopback-registry caches and staging state
cargo writes during the rounds (the ephemeral `127.0.0.1-*` residue) land
there and are removed with it, leaving the caller's real Cargo home — and any
real token inside it — byte-for-byte unread and unchanged (the sentinel probe
runs the real script against an untouched `CARGO_HOME` and requires exactly
that), and every token environment override (`CARGO_TOKEN`, the legacy
`CARGO_REGISTRY_*` spellings, and every named-registry
`CARGO_REGISTRIES_*_TOKEN`) is stripped from the cargo child environment, so
cargo's env-over-config preference can never put a real credential ahead of
the run's fixed dummy (the receipt probe uploads under hostile token
variables and requires the dummy on every receipt). Runs sharing one workspace serialize their cargo
phases (metadata resolution, upload round, verification round — everything
touching the shared `rust/target/package` staging and the lockfile) through
an interprocess lock (`rust/target/publish-verify.lock`, an `flock` held from
the first cargo resolution to the last publish round, never deleted). Under
that lock, each publish round first removes Cargo's own temporary staging
directories from a previous round (`rust/target/package/tmp-crate` and
`tmp-registry` — residue a killed run leaves behind poisons a rerun; the
dirty-staging probe replants them and requires a clean rerun, with packaged
artifacts outside those two directories untouched). A
second same-tree run waits a bounded time — `PUBLISH_VERIFY_LOCK_WAIT_SECONDS`,
default 1800 seconds — and then fails loud with an error naming the lock file
and the recorded holder, while the lock path is derived from the tree root so
runs in different workspaces never wait on each other. The persistent mirror
cache under `rust/target/publish-verify-mirror/` (keyed by the lockfile hash)
and its atomic cache writes are unchanged. Only the sparse-index metadata is
mirrored eagerly — resolution needs it before any cargo round — while `.crate`
bytes are served lazily: the download handler of the one `LoopbackRegistry`
(start, index URL, receipts, close) validates each
`/dl/<name>/<version>/download` request against the locked external set,
reuses the persistent cache and then the caller's real crates.io cache —
resolved from the caller's real `CARGO_HOME` (falling back to `HOME/.cargo`)
at registry construction, crates.io (`index.crates.io-*`) cache directories
only, read-only, never written — and fetches misses under the same explicit
timeout, bounded retries with backoff, and crate-and-URL-naming failure,
writing the cache atomically and coalescing concurrent requests for the same
artifact onto one fetch — so a verified run touches the network only for the
artifacts its build graph requests. Every byte source is prevalidated by the
registry against the sha256 `cksum` of the already-mirrored overlay index
entry before it is served or cached: a corrupt persistent cache is discarded
and repaired from a good source, wrong-source or corrupt real-cache
candidates are ignored, and corrupt upstream bytes are rejected with a
checksum-naming error before they can poison the cache. Cargo remains the
independent checksum authority against the served index on top of that
prevalidation. A `--no-verify` run (which resolves from the index alone)
downloads nothing: its registry is built with downloads disabled, and every
`/dl` request fails loudly (503 naming the `--no-verify` mode) before any
uploaded/external resolution or upstream fetch, so no code path can ever
fetch an artifact under `--no-verify`. CLI-visible behavior is unchanged
except network timing and cache work: eager pre-materialization of every
external artifact became on-demand fetches served through the loopback
download handler.
The RESOLUTION set and the VERIFICATION set are
distinct: every publishable non-withheld member is uploaded to the loopback
(resolution), while only the pending members get a verify build. release.yml's
pre-publish step forwards cargo's own `--exclude <name>` spelling — the same
flags its upload rounds carry — for members the live-set audit reports
already live, and the oracle consumes those flags: an excluded member is
still uploaded in the population round and resolves like a live crate, but
gets no verify build of its own, because an already-live member cannot be
re-uploaded and re-verifying it is precisely what makes a resumed re-run
expensive. A fresh tag forwards nothing, so every pending member verifies,
and the release step exits before any packaging when the audit reports
nothing pending. An `--exclude` naming a non-member fails loud before any
cargo work; census withholding is independent of and unchanged by exclusions.
**Run it.** `just publish-dry-run` (verified) or `just publish-dry-run
--no-verify` (the CI PR form). `tests/test_publish_verify_oracle.py` is the
committed probe: it drives the real script with `--root` pointed at a
throwaway fresh-lockstep workspace, requires the plain publish to fail there
(the red shape, resolved against a loopback sparse index the probe serves
itself, so the default suite never queries crates.io nor depends on whether
the fixture names are registered), both dry-run modes to pass, a
census-withheld member to stay excluded by name, an unrecorded block to fail
loud, the resolution/verification set split (a fresh run verifying every
pending member, a resumed run verifying only pending members while a
pending member still resolves against a live `--exclude`d sibling, a fully
live call skipping every verification round, an unknown exclusion failing
loud before any cargo work, and the `=` exclusion form), the Cargo-generated
loopback entry to carry the uploaded crate's own checksum (and the payload's
`rust_version`/`links` fields), the upload receipts to prove the loopback-only
endpoint and dummy token (including under hostile token environment
variables), the coverage fail-closed shapes (a `publish = false` member out of
the eligible set, receipts exactly matching it, a failed population round
printing no covered lines), the dirty-staging rerun probe, the loopback
server's traversal refusal, a member that
cannot compile to fail the verified run, and
the network/lifecycle bounds above (bounded mirror fetches, the eager mirror
copying index metadata only, the lazy download handler — locked artifact
fetched once and reused, unused/unlocked artifacts never fetched, concurrent
requests coalesced, a failed miss answered loudly, real `cargo fetch` through
the handler, registry checksum prevalidation over every byte source — corrupt
persistent cache repaired, wrong-source/corrupt real-cache candidates ignored,
corrupt upstream bytes rejected before caching — downloads-disabled mode
refusing every `/dl` request with zero upstream hits, and corrupt bytes still
failing the cargo run loudly — ephemeral ports, per-run overlay directories,
and two
runs proceeding independently at once), the process-state quarantine (a
sentinel real `CARGO_HOME` byte-identical after a run, with the
`127.0.0.1-*` residue confined to the kept per-run overlay), and the
same-workspace coordination (two same-tree runs both completing serialized,
a run facing a held lock failing bounded with the lock and owner named, and a
failed run cleaning up only its own overlay directory).

---

## 7. Tool-catalog reconciliation

**Failure class.** `rust/tools/README.md` is the living catalog of the
development-tool surface: one row per public example, per lab `[[bin]]`, and
per owning-crate integration test, each naming its source path, its feature
lane, and the `just` recipe that runs it. Nothing watched it: a renamed or
deleted target, a moved source file, a changed `[[bin]]`
`required-features`, or a recipe that lost its name could leave the catalog
describing a surface that no longer exists (or missing one that does), and the
drift would surface only when a human followed a stale row.

**Gate body.** `scripts/tool_catalog_check.py`, driven by
`just check-tool-catalog`. It runs `cargo metadata --format-version 1
--no-deps --locked --offline` over `rust/Cargo.toml` and asks `just` itself
whether each referenced name resolves to an invocable recipe
(`scripts/just_recipe_index.py`: per-name `just --show`, private recipes
counted -- never a justfile text grammar, and `just --summary` runs only to
validate that the justfile parses), then reconciles the catalog in both
directions:

- every Public examples row resolves to exactly one live example target
  outside the tool packages, by source path and target name, and every such
  live target is covered by exactly one row;
- every current tool package -- discovered from the metadata, not from the
  README -- has exactly one Lab section naming it (whose manifest must live
  under `rust/tools`); its rows resolve to exactly that package's `[[bin]]`s,
  every `[[bin]]` of every current tool package is covered exactly once (a
  section present or not), and a row's documented primary Lane equals the
  target's `required-features` (an explicitly documented secondary build shape
  after the primary lane is ignored);
- the two integration tests the explicit current-source rule selects
  (`degenbot-ingestion`'s `headless_boot`, `degenbot-bot`'s
  `config_smoke_boot`) are reconciled in both directions: a live selected
  target without a row, a selected row without its live target, and an
  integration row outside the rule all fail; a target the rule once selected
  whose row and target are both gone has left the current surface and is not
  asserted (no revival tripwire);
- every `just <recipe>` a row references names a recipe in the current
  justfile;
- a source link must resolve to an existing file inside the repository, and to
  the target's own `src_path`;
- structural defects fail loudly: a row whose cell count disagrees with its
  header, a duplicate target identity, a declared row count that disagrees
  with its table, a missing or nonlocal source link, an ambiguous section, a
  missing catalog section.

The catalog's population is not merely what the README declares. The current
tool packages are discovered from the metadata -- every package whose manifest
lives under `rust/tools` must have exactly one Lab section here, and its
`[[bin]]`s are reconciled whether or not a section exists yet, so a new tool
package joins by gaining a section and cannot pass unwatched. The two
integration targets are selected by the explicit current-source rule, not by
the README's presence. There is no fixed row arithmetic and no removed-name
list: a target that leaves the manifests together with its row simply leaves
the catalog's surface. The README's human prose (purpose, inputs, write
behavior, gate semantics) stays authored there -- metadata cannot express it
and the gate never generates it.

Exit codes: `0` reconciled, `1` findings, `2` refused to certify (unreadable
inputs, a `cargo metadata` failure, a malformed metadata payload, a missing
catalog section, a justfile that yields no recipe names, or a recipe-existence
refusal -- `just` is missing or the justfile will not parse).

**Data.** No census file: `rust/tools/README.md` is the living data, and
per-slice history lives in git history and the ADRs.

**Run it.** `just check-tool-catalog` -- also a dependency of
`lint-rust-check`, so `just test`, the pre-push `rust-clippy` gate and CI run
it over a complete tree, and the `tool-catalog` pre-commit hook runs it when a
commit touches the README, the justfile, any rust manifest (so a manifest-only
example addition triggers), a `rust/tools` manifest or source, a `rust/examples`
crate, an example source, one of the two integration-test sources, or the gate
itself.
Interpreter: a plain `python3`, standard library only -- the same uv-free
constraint as sections 6 and 7.
`tests/test_tool_catalog_check.py` is the committed negative probe: it drives
the real gate over a throwaway workspace (a `--metadata` JSON dump standing in
for the cargo child) and requires the red paths for an added target, a row
whose target left the manifests, a tool package without a Lab section, a
duplicated Lab section, a duplicate row, a malformed row, a missing source
link, source drift, package drift, lane drift, an unknown recipe, nonlocal
source links, a declared-count disagreement, a current-source integration row
deleted while its live target remains, an integration row outside the
current-source rule, and the fail-closed exits, plus the green paths for the
secondary-build-shape lane form and the committed workspace itself.

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

Several have focused `check-*` aliases (`check-no-inner-allow`,
`check-engine-impl-blocks`, `check-cli-shell-purity`,
`check-config-derived-statics`). The full workspace suite is their canonical
gate; the aliases are for fast diagnosis, not additional mandatory executions.

---

## Adding a new ratchet

A new ratchet also ships its `fix:` line on the red path and its row in
[Repairing a red gate](#repairing-a-red-gate), so a failure names its own repair.

Two idioms cover the family. Pick the one that matches what the gate scans.

**Script + lexicon/census** (comment hygiene, pytest noise gate). A script
under `scripts/` holds the detector and the exit-code
contract — shell where the detector is one `rg` pass, Python where the parse is
more than a filter (`scripts/hooks/tripwire_ratchet.py`); a plain-text census or a
`signatures`-style array beside it holds the grandfathered entries. The script must (a) fail closed when its census is
missing, (b) run one pass (no per-token subprocesses) if it rides the
pre-commit tier, and (c) state its scope and blind spots in its header. Wire it
as a `just` recipe and, if it is instant, a hook declared in
`scripts/gates.toml` (file-scoped where possible; `pass_filenames = false` only
when it must sweep the tree) — then `just gates-regen`, never a hand-typed
`prek.toml` line.

**Cargo architecture-gate test + census file** (config-derived statics). Add a
`#[test]` to `architecture_gates.rs` that scans the workspace (derive crates
from `cargo metadata`), and a sibling `<name>_census.txt` whose header states
the detector, the classes, the format, and the documented blind spots. Make the
gate **bidirectional** (unlisted hit fails; stale listed entry fails). The
workspace suite runs it; add a focused `check-*` alias when that aids diagnosis,
without adding a duplicate pre-push execution. Use the `REGEN_*` opt-in pattern
for any gate that regenerates an artifact, and name the justfile recipe as the
caller (`just gen-stubs`, `just gen-config-docs`).
