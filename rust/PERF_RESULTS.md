# Rust build/lint/check performance — experiment results

Companion to `rust/PERF_BASELINE.md` (which holds the pre-experiment numbers).
This file records each lever's before/after, whether it was kept, and why.

**Durable status:** the kept changes (mold, nextest, r-a `targetDir`, dev-profile
cdylib) are provisioned by the devcontainer / pyproject so they survive
rebuilds — see the "Durable devcontainer changes" section below. The repo
itself stays free of any mold config so CI (ubuntu-latest, no mold) and
non-devcontainer cloners keep building with the default linker.

Hardware/toolchain at experiment time: 24 cores, Fedora, rustc 1.96.1.
Installed during experiments: `mold 2.40.4`, `lld 22.1.8`,
`cargo-nextest 0.9.140`, `cargo-machete`, `cargo-shear`.

## Summary table (kept changes only)

| Lever | Status | Baseline → After | Δ |
|-------|--------|------------------|---|
| mold linker | **KEPT** (devcontainer `~/.cargo/config.toml`) | clean build 59s→51s; test-build 64s→48s; cdylib link 27s→19.5s | −14% build, −25% test-build, −28% cdylib |
| **dev-profile cdylib** | **KEPT** (`pyproject.toml [tool.maturin] profile = "dev"`) | **uv-sync cdylib rebuild 149s→24s** (fires on every .rs edit) | **−84%** |
| nextest | **RECOMMENDED** (measured, not wired into justfile) | build+run 94s→75s (clean) | −20% |
| build-override opt-level=3 | REVERTED (regression) | 51s→75s clean | +47% (worse) |
| r-a `cargo.targetDir:true` | RECOMMENDED (IDE config, not A/B-able here) | kills the 19s first-check tax | estimated |
| combine integration tests | DEFERRED (structure risk) | — | low ceiling now that mold cut link cost |
| aws-lc-sys / libsqlite3-sys C-builds | SKIPPED (deliberate design) | — | 18s on clean, but out of scope |
| feature/dep prune | INVESTIGATED (candidates need per-dep verify) | — | mostly false positives |

## Net effect of kept changes

- Clean `cargo build --workspace`: **59s → 51s**
- `cargo test --workspace --no-run`: **64s → 48s**
- cdylib (`degenbot_rs`) link: **27.0s → 19.5s** (largest single-unit win)
- `cargo nextest run` clean build+run: **75s** vs `cargo test` 94s (if nextest wired in)
- Steady warm `cargo check --workspace`: unchanged at **2.7s** (already the dev-loop floor)
- 1749 tests pass under the new config (sanity-checked green)

## Per-lever detail

### Lever 1 — Faster linker (mold) ✅ KEPT

- Installed `mold 2.40.4` + `lld 22.1.8` via `dnf`.
- Config: `rust/.cargo/config.toml` →
  ```toml
  [target.x86_64-unknown-linux-gnu]
  rustflags = ["-C", "link-arg=-fuse-ld=mold"]
  ```
  (gcc 16 is the linker driver; `-fuse-ld=mold` switches the backend. No clang needed.)

  **NOTE — design changed for durability.** The committed `rust/.cargo/config.toml`
  was REMOVED because CI (`just lint-rust`/`test-rust`/`build-rust-extension` on
  `ubuntu-latest`) and non-devcontainer cloners have no mold and would fail to
  link. mold is now scoped to the devcontainer image via a user-level
  `~/.cargo/config.toml` baked into the Dockerfile (read by cargo in ADDITION
  to any repo-local config). See "Durable devcontainer changes" below.
- B0 clean build: 59s → **51s** (−8s). Most crates are rlibs (link-light), so the
  clean-build gain is modest.
- B4 test build (`--no-run`, 52 binaries): 64s → **48s** (−16s / −25%). This is the
  lever's sweet spot — 52 separate links.
- cdylib `degenbot_rs` link unit: 27.0s → **19.5s** (−28%). This also speeds every
  `uv sync` cdylib rebuild.
- lld was installed but not separately benchmarked; mold matched/exceeded expectations.
  To try lld instead: change rustflags to `["-C", "link-arg=-fuse-ld=lld"]`.

### Lever 2 — `[profile.dev.build-override] opt-level = 3` ❌ REVERTED

- Added `[profile.dev.build-override] opt-level = 3` to optimize proc-macros
  (≈19s aggregate on clean: syn, derive_more-impl, pyo3-macros-backend,
  serde_derive, syn-solidity).
- Result: clean build **51s → 75s** (+47%). The optimization pass on the
  proc-macros cost far more than it saved running them — rustc 1.96's proc-macro
  execution is already cheap, and opt-level=3 is a slow pass.
- The article itself warns this is marginal/sometimes counterproductive; confirmed
  here. Reverted. Profile is back to `codegen-units = 16` only.

### Lever 3 — `cargo-nextest` ✅ RECOMMENDED (not yet wired into justfile)

- Installed `cargo-nextest 0.9.140`.
- nextest run on warm bins (1749 tests): **11s**.
- nextest clean build+run: **75s** vs `cargo test` clean build+run 94s (−20%).
  The build portion is unchanged (same rustc, same 52 binaries) — the win is test
  execution parallelism + less per-binary overhead.
- **Not wired into `just test-rust`** because that recipe feeds the pre-push hook
  and CI (they'd need nextest installed). Recommendation: add a `test-rust-nextest`
  recipe or gate nextest on availability. Switching `test-rust` outright would
  change CI behavior — needs a maintainer decision.

### Lever 4 — rust-analyzer `cargo.targetDir: true` ✅ RECOMMENDED (config-only)

- This is a VS Code/rust-analyzer setting, not measurable via cargo in a shell.
- Symptom it addresses: the B1 "first warm check after build" tax of **19s** vs
  the steady-state **2.7s** — rust-analyzer and cargo builds sharing `target/`
  invalidate each other's fingerprints, and `uv sync` cdylib rebuilds compound it.
- Recommended `.vscode/settings.json`:
  ```json
  { "rust-analyzer.cargo.targetDir": true }
  ```
  Builds r-a artifacts under `target/rust-analyzer/`, isolating them from
  `cargo build`/`uv sync` runs.

### Lever 5 — Combine integration tests ⏸ DEFERRED

- 27 integration test `.rs` files across 9 crates (degenbot-db: 7,
  degenbot-pools: 6, degenbot: 4, …). Each is a separate link.
- Merging per-crate into `tests/main.rs`+`mod` would cut 27 links → 9.
- **Risk:** must expose internal types as `pub`; the parity/dual-driver tests
  (`rust/crates/degenbot/tests/parity_*.rs`) are part of the ADR-005 mechanically
  enforced structure — restructuring them risks that contract.
- **Ceiling is now lower** since mold already cut link cost. Estimated 5–10s on
  test-build; not worth the structural churn right now.

### Lever 6 — C-build avoidance (aws-lc-sys, libsqlite3-sys) ⏭ SKIPPED

- `aws-lc-sys` (10s) comes from rustls's `aws-lc-rs` crypto provider (via alloy).
  Switching to `ring` is a behavioral/security change — out of scope for perf-only.
- `libsqlite3-sys` (8s) is `rusqlite`'s `bundled` feature. `crates/degenbot-db/Cargo.toml`
  comment explicitly states "bundled libsqlite3-sys, no system" — a deliberate choice
  for wheel portability/reproducibility (the shipped cdylib embeds SQLite). Changing
  it affects the runtime artifact. Out of scope.
- Combined 18s on clean builds is the theoretical ceiling; both are intentional.

### Lever 7 — Dep/feature prune 🔬 INVESTIGATED

- `cargo-machete` and `cargo-shear` run across the workspace.
- cargo-shear flagged ~20 "unused" deps. Spot-checks show most are **false positives**:
  - `thiserror`, `serde` — used via `#[derive(Error)]`/`#[derive(Serialize)]` (derive
    macros don't show as direct references).
  - `proptest` — a `[dev-dependencies]` entry used in `tests/`, which shear's src-only
    scan misses.
- Genuine removals would help, but each needs its own `cargo check` + `cargo test`
  verification. Blindly applying `--fix` risks breaking the build.
- Candidates flagged (verify before removing): `dashmap`, `lru`, `rand`, `log`,
  `degenbot-abi`, `degenbot-solidly-math`, `degenbot-curve-math`, `degenbot-core`
  across degenbot-bot / degenbot-rpc / degenbot-python / degenbot-fork /
  degenbot-submission / degenbot-pools.
- Empty file flagged for deletion: `crates/degenbot-python/src/bot/engine/snapshot.rs`.
- `cargo features prune` (cargo-features-manager) was listed in the article but not
  tried — installable if a feature-level audit is wanted.

### Lever 8 — Dev-profile cdylib rebuild (the big agent-loop win) ✅ KEPT

**The finding.** `[tool.maturin] profile = "release"` in `pyproject.toml` meant
*every* `uv sync`-driven cdylib rebuild — which fires on any `.rs` edit via
`[tool.uv] cache-keys` watching `rust/crates/*/src/**/*.rs` — was a full
release build: `opt-level=3` + `lto="thin"` + `codegen-units=1`. That is the
single largest "small change → full run" cost an automated agent pays, and it
fires on every Rust edit.

**The fix.** `profile = "dev"` in `pyproject.toml` `[tool.maturin]`. The
editable-install cdylib (the `.so` Python loads in the devcontainer) now
builds at `opt-level=0`, no LTO.

**Measured (uv sync, single `.rs` touch):**
- release profile: **149s**
- dev profile: **24s**
- → **−125s / −84%** per agent Rust edit.

At the cargo level the same ratio holds: `cargo build --release -p degenbot_rs
--features …` = 156s vs `cargo build -p degenbot_rs --features …` (dev) = 32s.

**Independence from the published wheel (verified).** `[tool.maturin] profile`
is consumed ONLY by the local editable-install build path (`uv sync` /
`just dev` / `maturin develop`). The published wheel is built by the
PyO3/maturin-action in `.github/workflows/publish-to-pypi.yaml` line 71,
which passes `--release` explicitly AND overrides the features list
(`--features pyo3/extension-module`, dropping `degenbot-bot/hotpath`). So
the shipped artifact stays fully LTO-optimized, runtime-hotpath-free, and
unaffected by this setting. CI build/test jobs (`just lint-rust` /
`test-rust` / `build-rust-extension`) use cargo directly and don't read
`[tool.maturin]` at all — they are also unaffected.

**Tradeoff acknowledged.** The dev `.so` is now unoptimized: swap/solver math
runs slower in the devcontainer. That is acceptable (and correct) for the
dev/agent loop, where the goal is iteration speed, not hot-path latency.
Hot-path latency measurement must use a release build (`just build-rust-debug`
or a release wheel), which is unchanged.

**Verified green** with the new dev `.so`: 1749 Rust tests (nextest) + 360
rust-wrapped Python tests (pytest tests/rust) pass.

### Lever 9 — Dev-profile debuginfo trim ❌ REVERTED

Tried `[profile.dev] debug = "line-tables-only"` to cut debuginfo generation +
link cost on the test-build path (the corrode.dev article's `debug=0`/`strip`
suggestion, adapted to keep backtrace line numbers). Measured against the
test-build (B4) path: no clear win (54s vs 48s, within noise / slightly worse
on a clean rebuild), and it costs debugger + variable-inspection fidelity.
Not worth it for this workspace. `[profile.dev]` stays at `codegen-units = 16`
only.

## Reproducing measurements

```bash
# clean build with timings
cd rust && cargo clean
time cargo build --workspace --timings
# parse slowest units:
python3 - <<'PY'
import re,glob
f=sorted(glob.glob('target/cargo-timings/*.html'))[-1]
d=open(f).read()
pairs=[(n,float(t)) for n,t in re.findall(r'"name":\s*"([^"]+)"[^}]*?"duration":\s*([0-9.]+)', d, re.S)]
for n,t in sorted(pairs,key=lambda x:-x[1])[:15]: print(f'{t:6.2f}s  {n}')
PY

# test build (mold active)
python_libdir="$(/workspaces/degenbot/.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
time cargo test --workspace --no-run          # 48s with mold
time cargo nextest run --workspace            # 75s build+run (nextest), 11s warm run
```

## Files changed by these experiments

- `rust/.cargo/config.toml` — was created during experiments, then **REMOVED**
  (would break CI / non-devcontainer cloners). mold is provisioned via the
  devcontainer image instead — see below.
- `rust/Cargo.toml` — `[profile.dev]` temporarily gained then lost `build-override`
  (lever #2, regressed) and `debug = "line-tables-only"` (lever #9, no win);
  net no change from baseline.
- `pyproject.toml` — `[tool.maturin] profile` flipped `"release"` → `"dev"`
  (lever #8 — the headline agent-loop win). VERIFIED independent of the
  published wheel (CI maturin-action passes `--release` explicitly).
- `rust/PERF_BASELINE.md` — pre-experiment baseline reference (kept).
- `rust/PERF_RESULTS.md` — this file (kept).

Tooling installs done during experiments (`mold`, `lld`, `cargo-nextest`,
`cargo-machete`, `cargo-shear`) lived only in devcontainer state. The kept
ones are now **durable via the devcontainer** — see below.

## Durable devcontainer changes

These make the kept levers survive a `--remove-existing-container` rebuild.
None touch the repo's build config, so CI and non-devcontainer cloners are
unaffected.

- `.devcontainer/Dockerfile`
  - dnf layer: added `mold` + `lld` (faster linkers).
  - added `cargo install --locked cargo-nextest` (baked into `~/.cargo/bin`).
  - added a user-level `~/.cargo/config.toml` with the mold rustflag
    (`[target.x86_64-unknown-linux-gnu] rustflags = ["-C","link-arg=-fuse-ld=mold"]`).
    This is cargo's HOME config, read in addition to any repo-local config,
    and travels with the image — so it applies on every entry path (VSCode
    "Reopen in Container" AND `attach.sh`'s `podman start`+`exec`).
- `.devcontainer/devcontainer.json`
  - added `"rust-analyzer.cargo.targetDir": true` to the vscode settings —
    isolates r-a's build artifacts into `target/rust-analyzer/` so they don't
    invalidate (or get invalidated by) `cargo build` / `uv sync`, killing the
    ~19s first-check tax (lever #4).
- `.devcontainer/README.md` — documented mold/lld and cargo-nextest in the
  tool tables + caveats.
- `justfile` — added a dev-only `test-rust-nextest` recipe that uses nextest
  and falls back to `cargo test` if nextest is absent. The CI-facing
  `test-rust` recipe is UNCHANGED (still `cargo test`) so CI (no nextest)
  stays green.

## Durable repo changes (not devcontainer-scoped)

- `pyproject.toml` `[tool.maturin] profile = "dev"` (lever #8). This is a
  real repo change (committed), NOT devcontainer-scoped — because
  `[tool.maturin] profile` is consumed only by the local editable-install
  build path, and the published wheel passes `--release` explicitly at the
  CI `maturin-action` invocation. Every clone (devcontainer or otherwise)
  gets the fast dev `.so` on `uv sync`; every published wheel stays
  LTO-optimized. This is the one durable repo change from the perf work.

`cargo-machete` and `cargo-shear` were NOT baked in (lever #7 is
investigative; the findings are mostly derive-macro false positives that
need per-dep verification — not worth provisioning as defaults).
