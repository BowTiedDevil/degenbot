# Handoff: crates.io publishing prep — degenbot Rust crates

**Date:** 2026-08-20 · **Author:** pi research session · **For:** the agent owning the crates.io-publishing ergo epic.
**Status:** Research + local verification complete. The publish-readiness gate is **currently RED** (one proven blocker, §3.1). No crate of ours exists on crates.io yet.

Read this top-to-bottom before touching crates. §2 = what must be true when you're done, §3 = the verified starting state, §5/§7 = what to build, §8 = hard constraints.

---

## 1. Mission

Bring the degenbot Rust core to the state where the 26 publishable workspace crates can be published to crates.io safely and repeatably:

1. Package verification (`cargo publish --dry-run`) passes for the whole workspace — fix whatever escapes the crate boundary (the crates embed data files with relative `include_str!` paths).
2. A CI gate runs that check on every PR (astral's `check-publish.yml` pattern, §4/§6).
3. A release workflow publishes the workspace **in dependency order**, with registry auth, modeled on astral's `publish-crates.py` approach.
4. A human-triggered first publish (decision D1, §9).

The strategic research behind "how many crates and which" is in §4–§5. **Short version: 26 published crates is normal** (polars publishes ~30, uv ~65, ruff ~35); the reference projects publish everything that isn't dev tooling. Do not shrink the crate count "for hygiene" — fix the blockers and publish the full closure.

---

## 2. Definition of ready (acceptance gates, in order)

| # | Gate | Command / evidence |
|---|------|--------------------|
| G1 | Whole-workspace package verification passes | `cd rust && cargo publish --workspace --dry-run --allow-dirty` exits 0. (~20–40 min wall: it verification-builds each of the 28 members. Start it early in each iteration.) |
| G2 | Pyo3-free cores invariant intact | `just check-no-pyo3-in-cores` green (publishing must not drag pyo3 into the cores; AGENTS.md architectural vision). |
| G3 | PR gate | A CI workflow runs G1's command on every PR and blocks merges on failure (model: astral `check-publish.yml`, §6). |
| G4 | Release workflow | A workflow publishes the 26 crates in topological dependency order with `CARGO_REGISTRY_TOKEN`, raised publish timeout, and no `--no-verify` unless the G3 gate exists (model: astral `publish-crates.yml`, §6). |
| G5 | Version-bump runbook | Documented recipe: bump `0.6.0-alpha.5` → next version in **both** `rust/Cargo.toml [workspace.package] version` **and** the 26 `version` literals in `rust/Cargo.toml [workspace.dependencies]` (ADR-009 lockstep), re-run G1. Note: the maturin-built PyPI wheel's version also tracks this same literal — bumping moves the wheel version too (see comment at top of `rust/Cargo.toml`). |
| G6 | First publish (HUMAN-GATED) | Only after D1–D3 answered, §9. |

---

## 3. Verified local state (audited 2026-08-20 — re-verify with the given commands)

**Inventory** — 28 workspace members under `rust/crates/`; 26 publishable; 2 already `publish = false`:

- Private (correct as-is, keep): `degenbot-python` (published name `degenbot_rs`; the PyO3 cdylib shell — **never** a crates.io artifact; that's how pydantic/uv/ruff treat binding shells, §4.3) and `degenbot-execution-sample`.
- Publishable: the `degenbot` umbrella + the 25 core crates (`degenbot-aave`, `-abi`, `-arbitrage`, `-balancer-math`, `-bot`, `-concentrated-liquidity-math`, `-core`, `-curve-math`, `-db`, `-decoders`, `-execution`, `-executor`, `-fork`, `-order-index`, `-pathfinding`, `-pool-updater`, `-pools`, `-price`, `-rpc`, `-simulation`, `-solidly-math`, `-solvers`, `-submission`, `-uniswap`, `-v2-math`).
- Re-verify: `rg -l 'publish = false' rust/crates/*/Cargo.toml` (expect 2 hits) and `rg -m1 '^name' rust/crates/*/Cargo.toml`.

**Versioning** — ADR-009 single-source lockstep, already publish-shaped:

- One version literal: `rust/Cargo.toml [workspace.package] version = "0.6.0-alpha.5"`; every crate uses `version.workspace = true`.
- `rust/Cargo.toml [workspace.dependencies]` carries **explicit `version = "0.6.0-alpha.5"` alongside each internal `path`** so published manifests resolve — the comment at `rust/Cargo.toml:69-71` documents this ("26 publishable crates pass cargo publish manifest validation"). Keep the literals in sync on bumps (G5).
- No `publish` CI and no version-bump tooling exists yet (nothing in `justfile` mentions crates.io publishing; no cargo-release/release-plz). Both are in scope (§7).

**Metadata** — all 28 crates carry `description`, `license.workspace`, `repository.workspace`; 26 have `README.md` (missing only in the 2 private crates — fine). No `homepage`/`documentation` links anywhere; add a `docs.rs` link per crate once publishing starts (optional, nice for the registry listing).

**Name squatting check** — `https://crates.io/api/v1/crates?q=degenbot&per_page=30` returned **0 results** and `.../crates/degenbot` → "does not exist". All 26 names are available today. Claim them at first publish.

### 3.1 PROVEN BLOCKER (G1 fails today)

`cd rust && cargo publish --workspace --dry-run --allow-dirty` (cargo 1.97.1) verification-builds members in dependency order and **fails at `degenbot-uniswap`**:

```
error: couldn't read `src/../../../../src/degenbot/registry/deployments.json`: No such file or directory (os error 2)
   --> src/deployments.rs:123:32
error: failed to verify package tarball
```

Root cause: `rust/crates/degenbot-uniswap/src/deployments.rs:123` embeds `src/degenbot/registry/deployments.json` (11 KB, checked-in, **in the Python tree at the repo root**) via `include_str!("../../../../src/degenbot/registry/deployments.json")`. That file is the canonical deployment registry — `src/degenbot/registry/deployment_loader.py` (Python) reads the *same* file; the module doc (`deployments.rs` lines 11–17) intentionally claims "There is no second copy." `cargo package` only tars files inside the crate dir, so the escape break the package build. Any `include_str!`/`include_bytes!` path that resolves outside the crate dir is publish-blocking; ones that stay inside (even `concat!(env!("CARGO_MANIFEST_DIR"), ...)` self-includes, as in `degenbot-executor/src/grammar_walker/shapes/two_hop_v4_led.rs:352` and `two_hop_seed_v4.rs:191`) are fine because the referenced file ships in the tarball.

**Fix options (pick one, note it in the epic):**

- **A (recommended, smallest blast radius):** vendor a byte-identical copy at `rust/crates/degenbot-uniswap/src/deployments.json`, point the `include_str!` at it, add a just recipe + CI check that byte-compares the two copies (the repo already runs this exact discipline for the tier-3 oracle artifacts — see the "regenerate + publish the artifacts ... byte-compare against what the Rust tier-3b tests" comments in `justfile` ~lines 200–260; and `just verify-deployments` already gates the canonical file). Update the module doc: single *logical* source, two byte-identical physical copies enforced by CI.
- **B (true single file, bigger move):** move the canonical file into the Rust tree (e.g. `rust/crates/degenbot-uniswap/data/deployments.json`) and repoint the Python loader. Preserves one physical copy but changes Python package layout and every consumer of the old path. Heavier review; do it only if the team wants the Rust tree to own the registry.
- **Not an option:** `build.rs` reading the Python-tree file at build time — during `cargo publish` verification, build scripts only see packaged (in-crate) files, so this fails the same way.

**Suspects to confirm on the next green path:** after the uniswap fix, re-run G1 and let it surface anything else. Known-include inventory (all audited 2026-08-20): in-crate `schema_head.sql` in `degenbot-db/src/schema.rs:21` (OK); `testdata/liquidity_mapping_fixtures.json` in `degenbot-concentrated-liquidity-math` (file exists at `src/testdata/`, include is inside `#[cfg(test)]` — OK, but G1's verify build will confirm); test-dir includes in `degenbot-curve-math/tests/oracle_crosscheck.rs`, `degenbot-balancer-math/tests/oracle_crosscheck.rs`, `degenbot-executor/tests/walker_shapes_layout.rs` (check paths on the next pass; `cargo package` includes `tests/` and an escaping include there would break test-compilation of the published package, not necessarily G1's lib verify).

### 3.2 Release policy already in the tree

`rust/crates/degenbot-simulation/Cargo.toml` (~line 97): "a dev-dep back from degenbot-pools would be a publish-blocking cycle ... and the **release policy is to never publish with `--no-verify`**." Respect it: no `--no-verify` in our workflows; the G3 PR dry-run gate is what makes astral's `--no-verify` safe, so if we ever want it, G3 must exist first and the comment must cite it (as astral's workflow cites theirs).

---

## 4. Research: how uv / ruff / polars / pydantic actually do it (verified 2026-08-20)

Method: read each repo's root `Cargo.toml` + per-crate `Cargo.toml`s on `main`, queried the crates.io HTTP API (`/api/v1/crates/{name}`, search endpoint), and read the publish workflows. **Environment quirk:** probing `index.crates.io` raw paths 404s from this box even for known-published crates (ruff, polars, serde_json all 404) — do NOT use sparse-index probing as an existence check here; use the crates.io HTTP API (needs a `User-Agent` header).

| Project | Workspace crates | Published to crates.io (today) | Version scheme |
|---|---|---|---|
| **pydantic** (`pydantic/pydantic-core`) | 1 (`pydantic-core`) | **No crates.io record at all** (API 404 for `pydantic-core`/`pydantic_core`/`pydantic`; docs.rs 404; `static.crates.io` artifacts 403 while a `polars` control 200s) — the repo still builds `name = "pydantic-core"` 2.41.5 | n/a — the Rust core ships **only** as the PyPI wheel via maturin; the crates.io surface is abandoned/absent |
| **ruff** (astral-sh) | 48 (`crates/*` glob + `red_knot/`) | ~35: product tier `ruff`, `ruff_linter`, `ruff_wasm` @ **0.16.3**; internal libs (`ruff_text_size`, `ruff_source_file`, `ruff_python_parser`, `ruff_diagnostics`, `ruff_db`, `ruff_server`, … ~20) in lockstep @ **0.0.9**; the `ty` checker's libs (`ty_static`, `ty_python_core`, …) also @ 0.0.9. Dev/bench/mdtest/integration-test crates: `publish = false` (`ruff_dev` is `0.0.0` + private) | **No** `version` in `[workspace.package]`; each crate hardcodes its literal version; CI keeps product tier and 0.0.x internal tier in lockstep. Two-tier because the internal line evolves on its own cadence |
| **uv** (astral-sh) | 69 (`crates/*` glob) | ~66 — everything except `uv-dev`, `uv-bench`, `uv-trampoline` (nightly). `uv` + `uv-version` @ **0.12.5**; all internals @ **0.0.72**. They even **renamed** standalone `pypi-types`/`platform-tags` into branded `uv-pypi-types`/`uv-platform-tags` to complete the brand prefix | Same two-tier literal scheme as ruff |
| **polars** (pola-rs) | 36 (29 in `crates/` + py-polars runtime + examples) | ~30: umbrella `polars` + all domain crates (`-core`, `-lazy`, `-sql`, `-io`, `-parquet`, `-time`, `-utils`, `-python`, …) at **one shared version** (0.55.2 released / 0.55.1 in tree); new internals `polars-descriptions`, `-observer`, `-dylib` unpublished; leftovers `polars-pipe` 0.48.1, `polars-view` 0.55.3 linger at their own versions; extension family `pyo3-polars` 0.28.0 / `pyo3-polars-derive` 0.22.0 versioned independently | **Single shared version** via `version.workspace = true` from `[workspace.package] version = "0.55.1"` — the same scheme degenbot uses (ADR-009). Note: `release-rust.yml` is a `# TODO: Implement` stub (`if: false`) — their Rust releases are published manually by maintainers |

**Ratios:** uv ≈95% of workspace crates are published, polars ≈93%, ruff ≈72%, pydantic 0%. degenbot's 26 publishable is *fewer* than polars' published set. **The count is not the smell** — the smells are (a) publishing dev tooling, (b) publishing the pyo3 shell, (c) version drift, (d) publishing without a dry-run gate, none of which apply to us today except the G1 blocker.

### 4.1 Astral's publish pipeline (the gold standard to copy)

From `astral-sh/uv` and `astral-sh/ruff`, `.github/workflows/publish-crates.yml` + `check-publish.yml` (ruff's is near-identical):

- **PR gate** (`check-publish.yml`, `workflow_call`): `cargo publish --workspace --dry-run` with a 20-min timeout — manifest validation + closure resolution verified on every PR.
- **Release workflow** (`publish-crates.yml`, a subworkflow of the cargo-dist-driven `release.yml`, invoked inside a cargo-dist plan job):
  - `environment: release` (protects the job); `permissions: contents: read, id-token: write`.
  - Auth via `rust-lang/crates-io-auth-action` (OIDC — **no long-lived token**; requires the crates.io owner to be linked to the GitHub org/repo, decision D2).
  - Installs a pinned **nightly** toolchain "required for the unstable `-Zpublish-timeout` flag, which lets us raise the per-crate wait above the 60s default … crates.io indexing has been known to lag long enough to exceed that during workspace publishes."
  - Publishes via a custom script in **dependency order**: `python3 scripts/publish-crates.py --cargo 'cargo +nightly-…' --no-verify -- -Zpublish-timeout --config 'publish.timeout=600'` with `CARGO_REGISTRY_TOKEN` from the OIDC step. `--no-verify` is commented safe *because* the dry-run gate runs elsewhere in CI.

### 4.2 Why they publish internals at all

Two real reasons, both applicable to us:

1. **The closure rule** (below): publishing the umbrella *requires* publishing everything it depends on — so "publish the product" = "publish the family".
2. **External reuse of internals**: ruff's `ruff_python_parser`/`ruff_python_ast` have 190k+ recent downloads (RustPython vendors them; fork `rustpython-ruff_python_parser` has 250k). polars users `cargo add polars-core` / `polars-sql` directly. Our math crates (`-v2-math`, `-curve-math`, `-balancer-math`, `-concentrated-liquidity-math`, `-solidly-math`, `-pools`) are exactly the kind of surface an external Rust MEV dev wants without the bot — publishing them is a feature, not noise.

### 4.3 Binding shells stay off the registry

pydantic's core is the (unpublished) `pydantic-core` crate consumed by a maturin wheel; uv's `uv-dev` and ruff's `ruff_dev` are private; `polars-python`/`pyo3-polars` are the exception *because* they carry an independent Rust API / registry consumers. `degenbot_rs` stays `publish = false`. ✓ (already the case)

---

## 5. The dependency-closure rule (what actually determines "how many")

`cargo publish` rewrites each `path` dependency into a registry version requirement, and **crate verification fails if a dependency isn't published**. The `degenbot` umbrella (`rust/crates/degenbot/Cargo.toml`) directly depends on **all 25 core crates**, so publishing the umbrella forces publishing all of them: **26 artifacts minimum**. There is no "publish the umbrella alone" path. uv lives with the same fact at 65 crates. Consequence: the only way to publish fewer names is to merge crates (e.g. fold the five `*-math` crates into one `degenbot-math`) — per §4, not worth it; keep workspace modularity and control the public surface with docs/visibility instead.

Also: the umbrella's public surface (its re-exports) is the de-facto public API for standalone `cargo add degenbot` consumers (the `examples/standalone_consumer.rs` ADR-005 check). Keep the umbrella pyo3-free — it is one of the two first-class consumers in AGENTS.md.

---

## 6. Options and recommendation

- **A — full-closure publish (recommended target state).** All 26 crates, one lockstep version (ADR-009), gated by G1–G5. This makes the `cargo add degenbot` promise in AGENTS.md real. Cost: we own 26 registry names; per astral's precedent that's normal.
- **B — pydantic-style: nothing on crates.io yet.** The wheel is the artifact; standalone Rust consumers use `cargo add degenbot --git`. Zero registry surface until 0.7/1.0. Legitimate, but it abandons the ADR-005 standalone-consumer-on-crates.io story and the math-crate reuse angle.
- **C — staged: publish the small stable leaf closures now** (the math family: `-core`, `-pools`, `-v2-math`, `-curve-math`, `-balancer-math`, `-concentrated-liquidity-math`, `-solidly-math` — closures are tiny, external-deps only), hold the bot-side crates + umbrella. Mirrors how polars users depend on `polars-core` directly. Acceptable as an intermediate; leaves a partial registry state to document in the crates' READMEs, and D1's alpha question still applies.

**Recommendation: A**, executed in the §7 order (C falls out naturally as the first green slice if the team wants something live sooner). Decision D1 (publish alphas now vs hold) gates any actual upload.

---

## 7. Implementation plan (ordered)

Work in a branch; each step ends with the G1 dry-run re-run (it's the local oracle; use `just` if you add a recipe for it).

- **T1 — Fix the uniswap `deployments.json` blocker** (§3.1). Option A: vendor copy + byte-compare just recipe/CI check (follow the tier-3 artifact discipline in `justfile` ~200–260; `just verify-deployments` already gates the canonical file). Option B: move canonical file to the Rust tree, repoint Python. Record the choice (D4) in the epic notes and update the `deployments.rs` module doc.
- **T2 — Drive G1 green.** Re-run the dry-run until all 28 members verify; fix further escapes the same way (audit remaining `tests/` includes, §3.1). Add a `just publish-dry-run` recipe so the oracle is one command.
- **T3 — PR gate workflow** (G3). Model `astral-sh/uv` `.github/workflows/check-publish.yml`: `cargo publish --workspace --dry-run`, 20-min timeout, on PR + merge queue (mirror the repo's existing CI runners/features — read `.github/workflows/ci.yml` first and match its toolchain setup).
- **T4 — Release workflow** (G4). Model `astral-sh/uv` `.github/workflows/publish-crates.yml`: `workflow_dispatch` (or a tag trigger — decide with the epic owner), protected `environment`, `rust-lang/crates-io-auth-action` OIDC (needs D2) or a secret token as fallback, a small script that topologically orders the 26 crates from `cargo metadata --format-version 1` (publish deps before dependents; the umbrella last) and runs `cargo publish -p <crate>` per crate; pass `-Zpublish-timeout --config 'publish.timeout=600'` with a pinned nightly toolchain (index lag between 26 fast publishes breaks the 60s default; astral's workflow explains this verbatim). No `--no-verify` unless T3 is merged and cited (repo policy, §3.2).
- **T5 — Version-bump runbook** (G5). One-page doc (fits in this file's §2/G5 or a justfile comment): bump both literal sites, re-run G1, tag, publish. Optionally a `just bump-version X.Y.Z` recipe that rewrites both sites and fails the build (via the dry-run) if either is stale.
- **T6 — First publish** (G6, HUMAN). After D1–D3: run the T4 workflow, watch the 26 publishes, verify `https://crates.io/crates/degenbot` (and spot-check two math crates), then run the standalone consumer example against the **registry** version (not path deps) to close the loop.

---

## 8. Hard constraints (do-not list)

- **Never `cargo publish --no-verify`** (in-tree release policy, §3.2).
- **Never publish `degenbot_rs`** (`degenbot-python`) or `degenbot-execution-sample` — keep `publish = false`.
- **Keep cores pyo3-free** (`just check-no-pyo3-in-cores` must keep passing; the umbrella has no pyo3 dep — ADR-005 standalone claim). Publishing work must not add a pyo3 dependency anywhere in the 26.
- **Keep ADR-009 lockstep**: exactly one version literal in the tree; the 26 `[workspace.dependencies]` version literals are sync copies of it — a bump that touches only one site is a bug. Bumps also move the maturin wheel version (one edit moves both halves, per `rust/Cargo.toml` header comment).
- **Keep the umbrella pyo3-free and the `check-no-pyo3-in-cores` / hotpath-feature patterns intact** (AGENTS.md: hotpath is `default-features = false`, no-pyo3-in-cores invariant).
- **ADR-010 kill list is orthogonal** — this work must not touch `src/degenbot/migrations/`, Alembic/SQLAlchemy deps, `ALEMBIC_HEAD`, or the `alembic_version` branch. (`degenbot-db`'s `schema_head.sql` / `SCHEMA_HEAD` constant is in-crate and fine as-is.)
- **No `include_str!`/`include_bytes!` may resolve outside the crate dir** in any of the 26 publishable crates — add this to the G3 gate's checks if you can express it cheaply (otherwise cover by T2's full dry-run).
- **Don't rename crates or add a second version tier** mid-stream; if the team later wants uv/ruff-style two-tier versioning for a stable-internal/product split, that's a separate ADR-scale decision.

---

## 9. Open decisions (need a human before T6)

- **D1 — Publish alphas now, or hold until 0.7/1.0?** `0.6.0-alpha.5` is a valid crates.io prerelease; publishing now claims names early and lets external math-crate users iterate. Holding means 0 registry surface during alpha. (Astral/polars both publish pre-1.0 freely; polars has been on 0.x for years.)
- **D2 — crates.io owner for OIDC.** The crates must be owned by a GitHub-linked owner (the `BowTiedDevil` account or a team/org) for `crates-io-auth-action` OIDC to mint tokens; otherwise fall back to a `CARGO_REGISTRY_TOKEN` secret (weaker, but works).
- **D3 — Option A vs C** (§6). Recommendation: A, with T2's green slice optionally shipped early as C.
- **D4 — `deployments.json` canonical placement** (T1 option A vs B).

---

## 10. Appendix: verified reference data (2026-08-20)

Re-verify with: `curl -sH 'User-Agent: <you>' https://crates.io/api/v1/crates/<name>` (JSON `crate.max_version`) and search `https://crates.io/api/v1/crates?q=<term>&per_page=100` (parse with python3 `json.load`; do **not** probe `index.crates.io` raw paths from this environment — it 404s falsely, §4). Note: large raw API JSON truncates in some sandboxes — pipe through `python3 -c 'import json,sys; …'` to extract fields.

**ruff published set (astral-sh/ruff main):**
- 0.16.3: `ruff`, `ruff_linter`, `ruff_wasm`
- 0.0.9: `ruff_annotate_snippets`, `ruff_cache`, `ruff_db`, `ruff_diagnostics`, `ruff_formatter`, `ruff_graph`, `ruff_index`, `ruff_macros`, `ruff_markdown`, `ruff_memory_usage`, `ruff_notebook`, `ruff_options_metadata`, `ruff_python_ast`, `ruff_python_codegen`, `ruff_python_formatter`, `ruff_python_importer`, `ruff_python_index`, `ruff_python_literal`, `ruff_python_parser`, `ruff_python_semantic`, `ruff_python_stdlib`, `ruff_python_trivia`, `ruff_ranged_value`, `ruff_server`, `ruff_source_file`, `ruff_text_size`, `ty_static`, `ty_module_resolver`, `ty_site_packages`, `ty_combine`, `ty_python_core`, `ty_python_semantic`
- unpublished workspace members: `ruff_benchmark`, `ruff_dev` (0.0.0, `publish=false`), `ruff_mdtest`, `*integration_tests`, `ty_test`, `ty_wasm`, `ty_ide`, `ty_server`, `ty_project`, `ty_completion_*`, `ty_vendored`, …
- naming note: astral's internal crates use **underscores** (`ruff_python_ast`) and they're published that way; `ty` (the checker's product name) is a **squat** on crates.io (created 2021-01-01 by someone else) — they publish `ty_*` libs without the bare name. Lesson: check names before committing to a brand; ours are clean (§3).

**uv published set (astral-sh/uv main, 69 workspace dirs):** `uv` + `uv-version` @ 0.12.5; @ 0.0.72: `uv-audit`, `uv-auth`, `uv-bin-install`, `uv-build-backend`, `uv-build-frontend`, `uv-build`, `uv-cache-info`, `uv-cache-key`, `uv-cache`, `uv-cli`, `uv-client`, `uv-configuration`, `uv-console`, `uv-dirs`, `uv-dispatch`, `uv-distribution-filename`, `uv-distribution-types`, `uv-distribution`, `uv-errors`, `uv-extract`, `uv-fastid`, `uv-flags`, `uv-fs`, `uv-git-types`, `uv-git`, `uv-globfilter`, `uv-install-wheel`, `uv-installer`, `uv-keyring`, `uv-logging`, `uv-macros`, `uv-metadata`, `uv-netrc`, `uv-normalize`, `uv-once-map`, `uv-options-metadata`, `uv-pep440`, `uv-pep508`, `uv-performance-memory-allocator`, `uv-platform-tags`, `uv-platform`, `uv-preview`, `uv-publish`, `uv-pypi-types`, `uv-python`, `uv-redacted`, `uv-requirements-txt`, `uv-requirements`, `uv-resolver`, `uv-scripts`, `uv-settings`, `uv-shell`, `uv-small-str`, `uv-state`, `uv-static`, `uv-toml`, `uv-tool`, `uv-torch`, `uv-trampoline-builder`, `uv-types`, `uv-unix`, `uv-virtualenv`, `uv-warnings`, `uv-windows`, `uv-workspace`. Unpublished: `uv-dev` (`publish=false`), `uv-bench`, `uv-trampoline` (nightly, workspace-excluded). Brand consolidation: `pypi-types`→`uv-pypi-types`, `platform-tags`→`uv-platform-tags` (old names now 404 on crates.io).

**polars published set (pola-rs/polars main, `crates/` glob + extras):** @ shared 0.55.2: `polars` (umbrella), `polars-arrow`, `-async`, `-buffer`, `-compute`, `-config`, `-core`, `-dtype`, `-error`, `-expr`, `-ffi`, `-io`, `-json`, `-lazy`, `-mem-engine`, `-ops`, `-ooc`, `-parquet`, `-plan`, `-python`, `-row`, `-schema`, `-sql`, `-stream`, `-testing`, `-time`, `-utils`; outliers: `polars-pipe` 0.48.1 (stale), `polars-view` 0.55.3 (independent, not in current `crates/`), `pyo3-polars` 0.28.0, `pyo3-polars-derive` 0.22.0 (independent family). Unpublished: `polars-descriptions`, `polars-observer`, `polars-dylib` + the example harnesses (`polars-runtime-32/64`, `pyo3-polars` examples). Version mechanism: `[workspace.package] version = "0.55.1"` + `version = { workspace = true }` in every crate.

**pydantic-core anomaly:** `pydantic/pydantic-core` main still declares `name = "pydantic-core"` 2.41.5 with `include = [...]` curation for publishing, yet the crates.io API, docs.rs, and static.crates.io all report no record today, and the repo's only workflow is `ci.yml` (no publish workflow). Most plausible reading: the crates.io surface was dropped/deleted and the Rust core is distributed solely via the PyPI wheel. Pattern takeaway: a single-crate Rust core whose only audience is a PyPI wheel does **not** need a crates.io presence; ours differs because AGENTS.md declares a first-class pure-Rust consumer (`cargo add degenbot`).

**Astral workflow bodies (verbatim key steps, both repos):** release job — `environment: release`; `permissions: contents: read, id-token: write`; checkout with `persist-credentials: false`; `rust-lang/crates-io-auth-action` → `CARGO_REGISTRY_TOKEN`; pinned nightly install (comment: needed for `-Zpublish-timeout`, "crates.io indexing has been known to lag long enough to exceed that [60s] during workspace publishes"); `python3 scripts/publish-crates.py --cargo cargo +nightly-<pinned> --no-verify -- -Zpublish-timeout --config 'publish.timeout=600'` (comment: "`--no-verify` is safe because we do a publish dry-run elsewhere in CI"). PR gate — `cargo publish --workspace --dry-run`, `timeout-minutes: 20`.
