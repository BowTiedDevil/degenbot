# Justfile for degenbot development
# https://github.com/casey/just

# Default recipe - show available commands
default:
    @just --list

# Print the project's single source-of-truth version (the [workspace.package]
# literal in rust/Cargo.toml, inherited by every crate + bridged into the wheel
# by maturin — ADR-009).
version:
    #!/usr/bin/env python3
    import json, subprocess
    meta = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1",
         "--manifest-path", "rust/Cargo.toml", "--no-deps"]
    )
    pkgs = json.loads(meta)["packages"]
    print(next(p["version"] for p in pkgs if p["name"] == "degenbot_rs"))

# ========== Rust Development ==========

# Run the standalone-Rust-consumer smoke (ADR-005 standalone claim). Proves a
# `cargo add degenbot` consumer reaches BotState/DexIdentity/calc math with no
# Python in the build graph. `examples/standalone_consumer.rs` panic!s on any
# check failure, so this is the standalone-consumer gate. The example is a
# `cargo add degenbot` showcase binary AND a CI-runnable assertion.
test-standalone:
    cargo run --manifest-path rust/Cargo.toml -p degenbot --example standalone_consumer

# ========== Tests ==========
#
# "Run the tests" is no longer a language choice: Python is a driver shell over
# the Rust core, so the default gate runs BOTH the native Rust suite and the
# full pytest suite (which itself drives the core through the PyO3 seam, golden
# on-chain-oracle replay, and the wrapped `tests/rust`) under one entrypoint.
# CI and the pre-push hook still address the language tracks directly
# (`test-rust` / `test-python`) so the python-version matrix and job
# partitioning keep working. Deliberately excluded from `test` (run on demand):
# toolchain-gated Tier-3 harness rebuilds (`test-tier3` / `verify-tier3-*`) and
# net-gated suites (`record-golden`, `verify-deployments`).

# Default gate: standalone smoke + cargo workspace + full pytest.
test: test-rust test-python

# Run only the Rust track (standalone smoke + cargo workspace). CI's rust-test
# job and the pre-push hook call this subunit directly; humans use `just test`.
test-rust: test-standalone
    #!/usr/bin/env bash
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml --workspace

# Run Rust tests via cargo-nextest (dev only; CI uses `test-rust` above because
# the CI runner does not install nextest). ~20% faster build+run than cargo test
# via execution parallelism (see rust/PERF_RESULTS.md lever #3). Falls back to
# cargo test if cargo-nextest is not installed (e.g. outside the devcontainer).
test-rust-nextest: test-standalone
    #!/usr/bin/env bash
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    if cargo nextest --version >/dev/null 2>&1; then
        cargo nextest run --manifest-path rust/Cargo.toml --workspace
    else
        echo "cargo-nextest not installed; falling back to cargo test" >&2
        cargo test --manifest-path rust/Cargo.toml --workspace
    fi

# Run Rust linter (clippy)
lint-rust:
    cargo clippy --fix --all-targets --all-features --fix --allow-dirty --manifest-path rust/Cargo.toml -- --deny warnings

# Lint Rust (check-only; non-mutating). Mirrors the clippy gate CI runs,
# minus `--fix`, so a pre-commit run cannot dirty staged files. Stricter than
# CI's `lint-rust`: fails on any warning `--fix` would have auto-applied.
lint-rust-check: check-no-inner-allow
    cargo clippy --all-targets --all-features --manifest-path rust/Cargo.toml -- --deny warnings

# Forbid file-level inner "#![allow]" - clippy's allow_attributes catches only the
# outer #[allow] form; this closes the historical inner-attribute loophole it
# misses. One reasoned outer #[allow(..., reason = "...")] remains permitted for
# the legitimate cross-target conditional suppressions #[expect] cannot express.
check-no-inner-allow:
    @if rg -n '#!\[allow\(' rust/crates -g '*.rs'; then \
        echo "ERROR: inner #![allow] is forbidden - use #[expect], or a reasoned outer #[allow] for cross-target conditionals" >&2; \
        exit 1; \
    else \
        echo "ok: no inner #![allow] in Rust sources"; \
    fi

# Check Rust formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check:
    cargo fmt --manifest-path rust/Cargo.toml --all -- --check

# Enforce the no-pyo3-in-core invariant (Plan 103). Pure Rust core crates must
# not depend on pyo3 under their default features. Add new core crates here.
check-no-pyo3-in-cores:
    #!/usr/bin/env bash
    set -euo pipefail
    for crate in degenbot-core degenbot-concentrated-liquidity-math degenbot-v2-math degenbot-curve-math degenbot-balancer-math degenbot-abi degenbot-rpc degenbot-bot degenbot-decoders degenbot-uniswap degenbot-pathfinding degenbot degenbot-solidly-math degenbot-price degenbot-db degenbot-pool-updater degenbot-aave degenbot-execution degenbot-executor degenbot-submission degenbot-simulation degenbot-pools degenbot-solvers degenbot-order-index; do
        if cargo tree --manifest-path rust/Cargo.toml -p "$crate" 2>/dev/null | grep -qi 'pyo3 v'; then
            echo "ERROR: $crate pulls pyo3 under default features (must be feature-gated)." >&2
            exit 1
        fi
    done
    echo "OK: core crates + umbrella are pyo3-free under default features"

# Build Rust release library (links Python - for testing only)
build-rust-debug:
    cargo build --release -p degenbot_rs --manifest-path rust/Cargo.toml

# Build Rust extension module (correct for Python extension)
build-rust-extension:
    cargo build --release -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# ========== Python Development ==========

# Build and install Python extension in development mode
dev:
    uv run maturin develop

# Build Python extension wheels
build-wheels:
    uv run maturin build --release

# Run only the Python track (full pytest). CI's python-test matrix job and the
# pre-push hook call this subunit directly; humans use `just test`. Under the
# default offline marker filter (`-m "not slow and not base and not online_rpc"`)
# this covers the PyO3 seam, golden on-chain-oracle replay, AND the wrapped
# `tests/rust` suite. A focused parity-only run is `uv run pytest -m onchain_oracle`.
test-python:
    uv run pytest -x -q --no-header

# Re-populate golden files for on-chain-oracle parity tests. Requires a working
# fork (tests.env RPC or local node). Pass a nodeid to refresh a single test:
#   just record-golden -- tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations
# Single-process (-n0): parametrized parity tests share one golden file per
# test function and accumulate keys across params; xdist parallelism would race
# the shared file (last-writer-wins, losing keys). Replay is read-only and safe
# under xdist, but record accumulates writes.
record-golden *args:
    DEGENBOT_GOLDEN_MODE=record uv run pytest -m onchain_oracle -q --no-header -n0 {{ args }}

# Verify every shipped deployment address is actually deployed on-chain (cast).
# Tier 1 (bytecode presence) by default; escalate via the env var:
#   DEGENBOT_VERIFY_DEPLOYMENTS=2 just verify-deployments   # +selector fingerprint
#   DEGENBOT_VERIFY_DEPLOYMENTS=3 ETHERSCAN_API_KEY=... just verify-deployments  # +Etherscan source
#   DEGENBOT_VERIFY_DEPLOYMENTS=4 just verify-deployments   # +init_code_hash reproduces pool address
# Requires a reachable RPC per chain (tests.env / env vars). Deselected from the
# default `test-python` run (online_rpc marker) — run on demand only.
verify-deployments *args:
    DEGENBOT_VERIFY_DEPLOYMENTS=${DEGENBOT_VERIFY_DEPLOYMENTS:-1} uv run pytest -m online_rpc -q --no-header -p no:randomly {{ args }} tests/registry/test_deployment_onchain_verification.py

# Tier-3a byte-exact oracle: SwapMath.computeSwapStep (V3 + V4) vs real
# canonical core libraries run as EVM bytecode in revm (ergo task OZRQS6,
# epic UP5NH6). Builds BOTH harnesses (V3 via direct solc 0.7.6 — v3-core
# pragmas <0.8 + foundry can't resolve solc <0.8 here; V4 via forge 0.8.26)
# via tier3-oracle/build-tier3-harnesses.sh, then runs the proptest with
# --include-ignored. The proptest asserts each Rust output field === the
# on-chain output byte-for-byte. The V3 direct-solc + V4 forge split is a
# documented toolchain deviation (foundry's solc-list endpoint is
# unreachable for <0.8 in this env); the script caches solc 0.7.6 in the svm
# dir for reuse.
test-tier3-step:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-harnesses.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-concentrated-liquidity-math --test tier3_compute_swap_step_vs_revm

# Tier-3b end-to-end V3 `Pool.swap` oracle (ergo UP5NH6 / 2LTKVO).
# Builds the v3-core harness (solc 0.7.6) then drives the Rust
# `v3_simulate_swap` against real UniswapV3Pool bytecode in revm.
test-tier3-swap:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-v3-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_v3_pool_swap_vs_revm

# Tier-3 V2 `Pair.swap` oracle (ergo UP5NH6 / TLBUNW — family 1/3 of SH6HAK).
# Builds the v2-core harness (solc 0.5.16) then drives the engine's
# `IntHopState::swap` (V2 getAmountOut) against real UniswapV2Pair bytecode
# in revm, asserting byte-exact via the K-invariant boundary.
test-tier3-v2:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-v2-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_v2_pair_swap_vs_revm

# Tier-3b V4 `PoolManager.swap` end-to-end oracle (ergo UP5NH6 / 2LTKVO —
# the V4 half). Builds the v4-core harness (solc 0.8.26: PoolManager singleton
# + unlocker + mock tokens) then drives the Rust `v4_simulate_swap` against the
# canonical `PoolManager` bytecode in revm through the unlock/settle dance,
# asserting amount0/amount1 byte-exact to the on-chain BalanceDelta.
test-tier3-v4:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-v4-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_v4_pool_swap_vs_revm

# Tier-3 Curve stableswap `get_dy` oracle (ergo UP5NH6 / YXMNWB — family 2/3
# of SH6HAK's Tier-3 cutover). Builds the src-curve harness (solc 0.8.26 — a
# faithful Solidity port of the STANDARD stableswap `get_dy`; Curve's canonical
# source is Vyper, absent here) then drives the engine's `simulate_swap`
# standard path against the on-chain `getDy`, asserting byte-exact across a
# pinned case + proptest.
test-tier3-curve:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-curve-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_curve_swap_vs_revm

# Tier-3 Balancer weighted/stable swap oracle (ergo UP5NH6 / EZLECC — family
# 3/3 of SH6HAK's Tier-3 cutover). Builds the src-balancer harness (solc
# 0.7.6) over the CANONICAL balancer-v2-monorepo math cores (FixedPoint/
# LogExpMath/WeightedMath/StableMath, pinned commit f8b6f44), then drives the
# engine's `simulate_swap` weighted + stable (invariant_version==1) paths
# against the on-chain bytecode, asserting byte-exact across pinned cases +
# proptests.
test-tier3-balancer:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-balancer-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_balancer_swap_vs_revm

# Tier-3 PancakeSwap V3 `PancakeV3Pool.swap` oracle (task: PancakeSwap V3
# variant harness). Builds the src-pancake-v3 harness (solc 0.7.6) over the REAL
# `PancakeV3Pool` — the Etherscan-verified deployed source (pool
# 0x1445F32D1A74872bA41f3D8cF4022E9996120b31) vendored under `lib/pancake-src/`
# — then drives the engine's `v3_simulate_swap` against the on-chain swap,
# asserting byte-exact math AND that the 9-field `Swap` event variant decodes
# only via the PancakeSwap decoder (not the Uniswap one), matching the sim.
test-tier3-pancake:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-pancake-v3-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_pancake_v3_swap_vs_revm

# Tier-3 PancakeSwap V2 pair swap oracle (the fork-fee sub-slice of the V2
# family — the source of `tier3_v2_pair_swap_vs_revm.rs`'s deferral). Builds
# the src-pancake2 harness (solc 0.5.16) over the REAL Ethereum-mainnet
# `PancakePair` — the deployed PancakeSwap V2 fork of UniswapV2Pair, hardcoded
# 0.25% fee (`mul(25)/10000`, matching the engine `PANCAKESWAP_V2` preset) + 3-tuple
# timestamped reserves — then drives the engine's `IntHopState::swap` against
# the on-chain K-invariant boundary, asserting byte-exactness at the fork fee.
test-tier3-pancake2:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-pancake2-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot-pools --test tier3_pancake_v2_swap_vs_revm

# Tier-3 path-5000 V4 CL-hop clamp regression (ergo BHTWBZ): prove the
# CL-hop input clamp turns the 20.7M-gas EMPTY-HALT into a clean byte-exact
# fill under the executor's 5M ceiling, using the real v4-core PoolManager.
# The test loads the committed V4SwapOracleHarness bytecode (no toolchain),
# so it ALSO runs in the default `just test-rust`; this recipe rebuilds +
# republishes the v4 harness (like the other families) then re-runs the pair.
test-tier3-path5000:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-v4-swap-harness.sh
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml -p degenbot --test tier3_path5000_v4_clamp

# Tier-3 umbrella: rebuild + republish every canonical-reference harness, then
# run the full on-chain oracle suite. The individual tests ALSO run in the
# default `just test-rust` (they load the COMMITTED bytecode from
# `tier3-oracle/artifacts/`), so this recipe's unique role is regenerate +
# publish the artifacts (after a harness-source edit) and re-run each family.
# Recompiling dozens of revm harnesses makes this slow — run explicitly, or in
# the CI `tier3-oracle` job.
test-tier3: test-tier3-step test-tier3-swap test-tier3-v2 test-tier3-v4 test-tier3-path5000 test-tier3-curve test-tier3-balancer test-tier3-pancake test-tier3-pancake2

# Validate the committed tier-3 harness bytecode: recompile EVERY harness with
# the real solc/forge toolchain (into a throwaway dir, PUBLISH=0 — committed
# artifacts are never mutated) and byte-compare the creation bytecode against
# what the Rust tests load from `tier3-oracle/artifacts/`. This is the
# authoritative compile-vs-use check (covers a harness-source OR pinned
# vendored-lib edit without a rebuild); the toolchain-free complement
# `tier3_harness_artifacts.rs` runs in the default suite. Requires the
# toolchain (bootstrap-libs + svm solc). Wired into the CI `tier3-oracle` job.
verify-tier3-artifacts:
    tier3-oracle/verify-tier3-artifacts.sh

# Validate the committed Vyper executor artifact (BHL2R2 / tier-3b): recompile
# src-executor/cmd_executor.vy with the pinned vyper 0.5.0a3 (into a throwaway
# dir, PUBLISH=0) and byte-compare creation + runtime against what the Rust
# tier-3b tests load from `tier3-oracle/artifacts/executor/`. This is the
# authoritative compile-vs-use check for the vyper artifact; the toolchain-free
# complement `tier3_executor_artifacts.rs` runs in the default suite. Requires
# the toolchain: `/workspaces/executor` uv project (vyper 0.5.0a3). Wired into
# the CI `tier3-oracle` job (not the default cargo-test path).
verify-tier3-executor-artifact:
    tier3-oracle/verify-tier3-executor-artifact.sh

# Rebuild + publish every Tier-3 harness artifact (bytecode + source-hash
# manifest) from the current sources, without running the test suites. Run this
# after editing a `tier3-oracle/src*/**/*.sol` harness (or bumping a pinned
# vendored lib), then commit the updated `tier3-oracle/artifacts/`.
rebuild-tier3-artifacts:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-harnesses.sh
    tier3-oracle/build-tier3-v2-swap-harness.sh
    tier3-oracle/build-tier3-v3-swap-harness.sh
    tier3-oracle/build-tier3-v4-swap-harness.sh
    tier3-oracle/build-tier3-curve-swap-harness.sh
    tier3-oracle/build-tier3-balancer-swap-harness.sh
    tier3-oracle/build-tier3-pancake-v3-swap-harness.sh
    tier3-oracle/build-tier3-pancake2-swap-harness.sh

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 --fix "**/*.md" "!node_modules/**" "!.opencode/node_modules/**" "!.venv/**" "!tier3-oracle/lib/**"

# Lint Python files
lint-python:
    uv run ruff check --fix src/ 
    uv run ty check --fix --no-progress src/

# Lint Python (check-only; non-mutating). Mirrors the ruff+ty gate CI runs,
# minus `--fix`, so a pre-commit run cannot dirty staged files. Stricter than
# CI's `lint-python`: fails on any issue `--fix` would have auto-applied.
lint-python-check:
    uv run ruff check src/
    uv run ty check --no-progress src/

# Dead-code detector (off the gate — output is a triage list). Each hit
# needs an `rg` call before deletion: vulture is static and can't see
# FFI-seam callers (Rust core) or framework dispatch (pydantic validators,
# SQLAlchemy TypeDecorator signatures, `__exit__`/Protocol params). 80%
# confidence is the operating point; the 60% tier is mostly framework-
# dispatched methods (validators, properties on models, enum members).
# Complements ruff: ruff's F401 rule exempts `if TYPE_CHECKING:` imports
# and there is no ruff unreachable-code rule, so vulture catches both.
dead-code:
    uv run vulture src/degenbot vulture_whitelist.py --min-confidence 80

# Deeper dead-code sweep — catches unused functions/methods/classes too
# (the 80% tier only catches unused variables, imports, unreachable code).
# Output is much noisier (~hundreds of findings, mostly framework-dispatched
# pydantic validators, @property on models, enum members). Use periodically
# for intentional dead-code audits; not a routine gate. Generate whitelist
# candidates with: vulture src/degenbot --min-confidence 60 --make-whitelist
dead-code-deep:
    uv run vulture src/degenbot --min-confidence 60 --make-whitelist

# Check Python formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check-python:
    uv run ruff format --check src/

# Lint commit messages across a range (default: everything not yet pushed).
# Examples: just lint-commits              # @{push}..HEAD
#           just lint-commits HEAD~5..HEAD # explicit range
# just lint-commits main..HEAD   # branch commits
lint-commits range="@{push}..HEAD":
    #!/usr/bin/env bash
    set -euo pipefail
    range="{{ range }}"
    if [[ "$range" == *".."* ]]; then
      from="${range%%..*}"
      to="${range##*..}"
      [ -z "$to" ] && to=HEAD
    else
      from="$range"
      to=HEAD
    fi
    npx --yes @commitlint/cli --from "$from" --to "$to"

# Run all linters (Rust + Python + Markdown)
lint: fmt-check fmt-check-python lint-rust lint-python lint-markdown

# Format all code
format:
    uv run ruff format src/
    cargo fmt --manifest-path rust/Cargo.toml --all

# ========== Dependency Updates ==========

# Upgrade Python and Rust dependencies.
#
# Two passes over Rust deps, because Cargo splits the job:
#   1. `cargo upgrade --incompatible` rewrites the version *requirements* in
#      every member Cargo.toml to the latest published, including across
#      semver major boundaries (e.g. revm 41 -> 42). `cargo update` alone
#      cannot do this — it only refreshes Cargo.lock within the existing
#      `^x.y.z` range, so a major release on crates.io is invisible to it.
#   2. `cargo update` then refreshes Cargo.lock to satisfy the new
#      requirements. `cargo upgrade` already rewrites the lock too, but the
#      explicit pass also pulls compatible patch bumps it left at the floor.
#
# Requires the `cargo-edit` subcommand (`cargo upgrade`); install with
#   `cargo install --locked cargo-edit`
# Upgrade Python and Rust dependencies (incl. semver-major bumps).
update-deps:
    uv sync --upgrade
    cargo upgrade --manifest-path rust/Cargo.toml --incompatible
    cargo update --manifest-path rust/Cargo.toml

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: fmt-check check-no-pyo3-in-cores lint-rust test-rust
    cargo build --release -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# Simulate full CI pipeline
ci-full: ci-rust lint-markdown test-python

# ========== Repository Setup ==========

# Install prek git hooks and configure commit template.
# Run this once after cloning. Commit MESSAGE lint runs at commit time (low
# friction, since .commitlintrc.yml is relaxed: free-form scope, 100-col) so a
# bad message is caught the moment it is written — not at push, when amending
# days-old commits needs a deep rebase. Code linters (clippy/ty) run at pre-push
# + CI: a code-lint failure is fixable with a follow-up commit. Hooks are
# declared in prek.toml. For manual message range checks: just lint-commits.
setup-git-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    # prek installs into git's effective hooks dir (default .git/hooks). Clear
    # any stale custom hooksPath from the old .githooks setup so it isn't used.
    git config --unset core.hooksPath 2>/dev/null || true
    git config commit.template .commit-template
    # prek is installed as a global uv tool (~/.local/bin/prek) so the hooks
    # it generates do NOT pin a throwaway venv path — they fall back to `prek`
    # on PATH, which survives venv recreations and is reproducible across
    # host/container installs. Install on demand if missing (host first run).
    command -v prek >/dev/null 2>&1 || uv tool install prek
    prek install
    echo "✓ prek hooks installed:"
    echo "    pre-commit : Markdown lint + PLC0415 noqa guard (staged files)"
    echo "                + instant checks (Rust/Python fmt, Rust no-pyo3)"
    echo "    commit-msg : commitlint against .commitlintrc.yml (relaxed rules)"
    echo "    pre-push   : commitlint push-range + Rust/Python lint (clippy/ty)"
    echo "                 + build & test suite (rust build/test, python build/test)"
    echo "    Bypass: git push --no-verify (CI still runs)."
    echo "✓ commit template configured."

# ========== Documentation ==========

# Render a Mermaid diagram (Markdown or .mmd) to PNG.
# Example: just mermaid-png docs/architecture/rust-solver-engine.md
mermaid-png input output='':
    scripts/mermaid-export {{ input }} {{ output }} -f png

# Render a Mermaid diagram (Markdown or .mmd) to SVG.
# Example: just mermaid-svg docs/architecture/rust-solver-engine.md
mermaid-svg input output='':
    scripts/mermaid-export {{ input }} {{ output }} -f svg

# Build documentation
docs:
    cargo doc --no-deps --manifest-path rust/Cargo.toml
    uv run mkdocs build 2>/dev/null || echo "mkdocs not configured"

# Serve documentation locally
serve-docs:
    cargo doc --open 2>/dev/null --manifest-path rust/Cargo.toml || echo "Open rust/target/doc/degenbot_rs/index.html"
