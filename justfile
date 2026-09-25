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

# Bump every crate to a new SEMVER version in one atomic edit (ADR-009 lockstep).
# cargo-edit updates the [workspace.package] literal, every inherited [package]
# version, every internal [workspace.dependencies] requirement, and Cargo.lock —
# version drift that felled the 0.6.0-alpha.6 crates.io publish is impossible
# here. Pass the crates.io SEMVER form (0.6.0-alpha.7), not the PEP440 tag form
# (0.6.0a7). Requires cargo-edit: cargo install cargo-edit
bump-version version:
    cargo set-version --workspace {{ version }} --manifest-path rust/Cargo.toml

#
# Release-checklist step (schema-carrying releases): bump the Rust schema
# ritual in lockstep — `RUST_SCHEMA_VERSION` in `degenbot-db/src/schema.rs` →
# append the matching `MigrationStep` to `RUST_MIGRATIONS` in
# `degenbot-db/src/migrate.rs` → extend the fixture matrix (one DB per released
# revision). Verify the publish chain with `just publish-dry-run`.

# ========== Rust Development ==========

# Local Cargo and maturin development builds intentionally use the workspace
# [profile.dev]: opt-level = 1, no LTO or stripping, and line-tables-only debug
# info. This favors representative optimization while keeping rebuilds cheaper
# than release. Release builds keep thin LTO, stripping, and the intentional
# per-package codegen-unit policy documented in rust/Cargo.toml.

# Print the active Rust toolchain and the repository policy. The root
# rust-toolchain.toml pins development and release builds to Rust 1.98.1;
# the workspace MSRV is Rust 1.97 and is checked separately in CI.
toolchain:
    @echo "Pinned Rust policy: 1.98.1 (workspace MSRV: 1.97)"
    @rustc --version
    @cargo --version

# Run the standalone-Rust-consumer smoke (ADR-005 standalone claim). Proves a
# `cargo add degenbot` consumer reaches BotState/DexIdentity/calc math with no
# Python in the build graph. `examples/standalone_consumer.rs` panic!s on any
# check failure, so this is the standalone-consumer gate. The example is a
# `cargo add degenbot` showcase binary AND a CI-runnable assertion.
test-standalone:
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot --example standalone_consumer
    # MROOY7 5WTYYQ: the WS-ingestion crate's headless boot (no Python, no net).
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-ingestion --example headless_boot

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

# Run every pre-push gate manually, in hook order and fail-fast — the
# object-DB GC brake, the commitlint push-range re-lint, then the Rust/Python
# code linters (clippy, ruff+ty, stubtest), then the Rust and Python build +
# test tracks, exactly as the installed prek pre-push hook runs them
# (prek.toml, stages = ["pre-push"]). The installed hook and ci.yml stay
# authoritative on an actual push; this is for checking the gates with
# `just pre-push` before `git push`.
#
# Manual pre-push gate check (mirrors prek.toml pre-push stage, fail-fast).
pre-push:
    #!/usr/bin/env bash
    set -euo pipefail

    run_gate() {
        local label="$1"
        shift
        echo
        echo "===================================================================="
        echo "▶ pre-push gate: ${label}"
        echo "===================================================================="
        "$@"
    }

    run_gate "Object DB GC"            scripts/hooks/object-gc.sh
    run_gate "commitlint (push range)" scripts/hooks/commitlint-push.sh
    run_gate "Rust clippy"             just lint-rust-check
    run_gate "Python lint"             just lint-python-check
    run_gate "Stubtest drift gate"     just lint-stubtest
    run_gate "Rust build"              just build-rust-extension
    run_gate "Rust tests"              just test-rust
    run_gate "Python build (maturin)"  just dev
    run_gate "Python tests"            just test-python

    echo
    echo "✓ all pre-push gates passed."

# Run only the Rust track (standalone smoke + cargo workspace). CI's rust-test
# job and the pre-push hook call this subunit directly; humans use `just test`.
test-rust: test-standalone
    #!/usr/bin/env bash
    python_libdir="$(uv run --no-sync python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # vendored deployments.json (degenbot-uniswap) must match the canonical
    # Python-tree registry file byte-for-byte (TGO5ZY: a crate can only
    # embed in-tarball files, so the embed uses the in-crate mirror)
    cmp -s src/degenbot/registry/deployments.json rust/crates/foundation/degenbot-uniswap/src/deployments.json || { echo 'ERROR: deployments.json vendor drift (canonical vs degenbot-uniswap mirror)' >&2; exit 1; }
    cargo test --locked --manifest-path rust/Cargo.toml --workspace

# crates.io publish oracle (crates-io-publishing-prep handoff §2, gate G1):
# verification-builds every publishable workspace member in dependency order.
# ~20-40 min cold. CI's PR gate (check-publish) runs the clean-tree form.
publish-dry-run:
    #!/usr/bin/env bash
    set -euo pipefail
    cd rust
    cargo publish --locked --workspace --dry-run --allow-dirty

# Run Rust linter (clippy) with each workspace member's declared default features.
# This is the local fix command; `lint-rust-check` below is the non-mutating gate.
lint-rust:
    cargo clippy --locked --workspace --fix --all-targets --allow-dirty --manifest-path rust/Cargo.toml -- --deny warnings

# Lint Rust (check-only; non-mutating). This is the authoritative CI/pre-push
# Clippy gate. It deliberately checks default features without `--all-features`,
# so test-only and mutually exclusive build variants cannot hide a default
# regression. `lint-rust` above remains the explicit local fix command.
lint-rust-check: check-no-inner-allow check-engine-impl-blocks check-cli-shell-purity
    cargo clippy --locked --workspace --all-targets --manifest-path rust/Cargo.toml -- --deny warnings

# Check every workspace member with its declared default features. This is the
# independent default-feature check; it is not an all-features build.
check-rust-default:
    cargo check --locked --workspace --all-targets --manifest-path rust/Cargo.toml

# Check the pure-Rust umbrella consumer surface, including its standalone
# examples, without selecting the PyO3 binding crate.
check-rust-consumer:
    cargo check --locked -p degenbot --all-targets --manifest-path rust/Cargo.toml

# Check the binding crate with its intentionally broad default domain surface,
# but without the extension-module link mode.
check-rust-binding-default:
    cargo check --locked -p degenbot_rs --all-targets --manifest-path rust/Cargo.toml

# Check the dev-wheel feature set. Keep this explicit: these profiling,
# telemetry, allocator, and allocator-selection features are development-only
# and are not part of the release-wheel or standalone-default matrix.
check-rust-dev-features:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg tokio_unstable" cargo check --locked -p degenbot_rs --all-targets --manifest-path rust/Cargo.toml --features dev-features

# Check the release-equivalent extension feature set in the release profile.
# The release maturin command uses `pyo3/extension-module`; this package feature
# forwards to the same PyO3 feature while retaining the binding crate defaults.
check-rust-extension-release:
    cargo check --locked --release -p degenbot_rs --lib --manifest-path rust/Cargo.toml --features extension-module

# Exhaustive all-features compilation is an explicit diagnostic, not the
# default gate. It intentionally includes test-only and mutually exclusive
# variants and must never be used to validate default or release behavior.
check-rust-all-features:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg tokio_unstable" cargo check --locked --workspace --all-targets --all-features --manifest-path rust/Cargo.toml

# Forbid file-level inner "#![allow]" - clippy's allow_attributes catches only the
# outer #[allow] form; this closes the historical inner-attribute loophole it
# misses. One reasoned outer #[allow(..., reason = "...")] remains permitted for
# the legitimate cross-target conditional suppressions #[expect] cannot express.
check-no-inner-allow:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- no_inner_allow_attributes --exact --nocapture

# Check Rust formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check:
    cargo fmt --manifest-path rust/Cargo.toml --all -- --check

# Enforce the no-pyo3-in-core invariant (Plan 103). Pure Rust core crates must
# not depend on pyo3 under their default features. Add new core crates here.
check-no-pyo3-in-cores:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- core_crates_are_pyo3_free_under_default_features --exact --nocapture

check-cli-core-purity:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- cli_core_is_clap_and_indicatif_free --exact --nocapture

# Enforce the ADR-051 D2 dependency charter for the argv facade: `degenbot-cli`
# may name workspace members (the clap-free semantics crate + the sink crates)
# plus a small, explicit external allowlist - argv spelling (clap), progress
# rendering (indicatif), the SIGINT runtime (tokio) and the sink stack (tracing
# / tracing-subscriber). Anything else is a layering regression: the facade
# maps argv into `degenbot-cli-core` and must never reach a domain-engine crate
# on its own. Mirrors check-cli-core-purity / check-no-pyo3-in-cores.
check-cli-shell-purity:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- cli_shell_names_only_allowlisted_externals --exact --nocapture


# Structural gate for epic 5TBT7L (arch review #11, candidate 2): the engine
# seam deepens until `EngineStages` is the ONE external driver surface and
# every `impl ArbitrageEngine` block outside `arb_engine/mod.rs` is dissolved.
# T6 flipped it GREEN and wired it into the Rust hygiene set
# (`lint-rust-check` + the `rust-engine-impl-blocks` pre-commit hook + CI).
# The `{` anchor keeps the prose mention of `impl ArbitrageEngine` in
# mod.rs's module-tree comment from being counted as a block.
#
# Two assertions:
#   1. exactly ONE `impl ArbitrageEngine` block, in arb_engine/mod.rs.
#   2. `ArbitrageEngine` appears in degenbot-python/src exactly ONCE - the
#      pyclass `name = "ArbitrageEngine",` compat string (the deliberate
#      Python-API name exemption). Any other hit is a seam regression.
check-engine-impl-blocks:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- one_engine_impl_block --exact --nocapture

# Build Rust extension module in the release-equivalent feature set. This uses
# the workspace development profile (opt-level = 1, no LTO or stripping) for
# local validation; the feature set is the same as the release maturin
# invocation.
build-rust-extension:
    cargo build --locked -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# Verify the installed degenbot._ffi extension was built from the current
# Rust sources: compares the monotonic build number build.rs bakes into the
# compiled library against the repo counter file (.build-number). The primary
# stale-.so detector - run this instead of trusting a silent maturin/uv
# "rebuild" (see AGENTS.md "Rebuilding the Rust .so after edits"). Exit 1 on a
# stale library:
verify-build-fresh:
    uv run --no-sync python -m degenbot.build_info

# ========== Build-Artifact Housekeeping ==========

# Reclaim disk space under the Cargo target root. Preview first; both modes
# report every cache family and exact target/reclaimable byte counts:
#   DRY_RUN=1 just gc-target     # report candidates; delete nothing
#   AGE=7 just gc-target         # remove artifacts older than seven days
#   AGE=0 DRY_RUN=1 just gc-target
#
# Normal Cargo caches age-sweep deps/examples/build/.fingerprint, always drop
# incremental state, and dedupe old large test binaries. Separate rebuildable
# families are age-swept too: coverage reports/builds, LLVM coverage, Criterion,
# wheels, and rustdoc output. `target/maturin` is always protected, as is the
# repository-root `.build-number` receipt outside the target root. See the
# Build-Artifact Housekeeping section in AGENTS.md for the family policy.
gc-target:
    scripts/gc-target.sh

# ========== Python Development ==========

# Install the locked Python environment without building the project a second
# time. `dev` is the sole editable-extension install path and selects the
# canonical Cargo `dev-features` alias explicitly.
bootstrap:
    uv sync --locked --dev --no-install-project
    just dev

# Build and install the Python extension in development mode. Maturin consumes
# pyproject's `profile = "dev"` setting, which matches rust/Cargo.toml's
# opt-level = 1 development profile (not an unoptimized debug build). Run
# `just bootstrap` first; `--no-sync` keeps this build from silently replacing
# the explicit development feature set with ordinary package defaults.
dev:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--cfg tokio_unstable" uv run --no-sync maturin develop --features dev-features

# Run only the Python track (full pytest). CI's python-test matrix job and the
# pre-push hook call this subunit directly; humans use `just test`. Under the
# default offline marker filter (`-m "not slow and not base and not online_rpc"`)
# this covers the PyO3 seam, golden on-chain-oracle replay, AND the wrapped
# `tests/rust` suite. A focused parity-only run is `uv run --no-sync pytest -m onchain_oracle`.
test-python:
    uv run --no-sync pytest -x -q --no-header

# Re-populate golden files for on-chain-oracle parity tests. Requires a working
# fork (tests.env RPC or local node). Pass a nodeid to refresh a single test:
#   just record-golden -- tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations
# Single-process (-n0): parametrized parity tests share one golden file per
# test function and accumulate keys across params; xdist parallelism would race
# the shared file (last-writer-wins, losing keys). Replay is read-only and safe
# under xdist, but record accumulates writes.
record-golden *args:
    DEGENBOT_GOLDEN_MODE=record uv run --no-sync pytest -m onchain_oracle -q --no-header -n0 {{ args }}

# Verify every shipped deployment address is actually deployed on-chain (cast).
# Tier 1 (bytecode presence) by default; escalate via the env var:
#   DEGENBOT_VERIFY_DEPLOYMENTS=2 just verify-deployments   # +selector fingerprint
#   DEGENBOT_VERIFY_DEPLOYMENTS=3 ETHERSCAN_API_KEY=... just verify-deployments  # +Etherscan source
#   DEGENBOT_VERIFY_DEPLOYMENTS=4 just verify-deployments   # +init_code_hash reproduces pool address
# Requires a reachable RPC per chain (tests.env / env vars). Deselected from the
# default `test-python` run (online_rpc marker) — run on demand only.
verify-deployments *args:
    DEGENBOT_VERIFY_DEPLOYMENTS=${DEGENBOT_VERIFY_DEPLOYMENTS:-1} uv run --no-sync pytest -m online_rpc -q --no-header -p no:randomly {{ args }} tests/registry/test_deployment_onchain_verification.py

# Re-populate the golden deployment-verification capture (tiers 1/2/4 facts for
# every factory row on a reachable chain) consumed by the hermetic replay
# tests tests/registry/test_deployment_golden_verification.py (default suite,
# fully offline). Requires a reachable RPC per chain (tests.env / env vars);
# unreachable chains contribute no rows. Tier 4 is asserted live while
# recording, so a capture is only committed when every recorded row verified.
record-deployment-golden *args:
    DEGENBOT_GOLDEN_MODE=record DEGENBOT_VERIFY_DEPLOYMENTS=4 uv run --no-sync pytest -m online_rpc -q --no-header -p no:randomly -n0 {{ args }} tests/registry/test_deployment_onchain_verification.py

# ========== Tier-3 On-Chain Oracles ==========
#
# `just test-tier3 [family]` — build a family's pinned canonical-reference
# harness (real solc/forge toolchain), republish its artifacts under
# `tier3-oracle/artifacts/`, and run that family's byte-exact
# Rust-vs-real-EVM test. No family (default `all`) runs EVERY family, in the
# order listed below.
#
# Family notes (harness sources under `tier3-oracle/src*/`, epic UP5NH6 task
# IDs unless noted):
#   step      SwapMath.computeSwapStep (V3 + V4) vs the real canonical core
#             libraries run as EVM bytecode in revm (OZRQS6). V3 via direct
#             solc 0.7.6 (v3-core pragmas <0.8 + foundry can't resolve solc
#             <0.8 in this env — documented toolchain deviation; the script
#             caches solc 0.7.6 in the svm dir) + V4 via forge 0.8.26.
#             Asserts each Rust output field === the on-chain output.
#   swap      V3 `Pool.swap` end-to-end (2LTKVO). solc 0.7.6 harness, drives
#             `v3_simulate_swap` against real UniswapV3Pool bytecode in revm.
#   v2        V2 `Pair.swap` (TLBUNW — family 1/3 of SH6HAK). solc 0.5.16
#             harness; `IntHopState::swap` (V2 getAmountOut) byte-exact via the
#             K-invariant boundary.
#   v4        V4 `PoolManager.swap` end-to-end (2LTKVO). solc 0.8.26 harness
#             (PoolManager singleton + unlocker + mock tokens);
#             `v4_simulate_swap` through the unlock/settle dance, with
#             amount0/amount1 byte-exact to the on-chain BalanceDelta.
#   path5000  path-5000 V4 CL-hop clamp regression (BHTWBZ): prove the CL-hop
#             input clamp turns the 20.7M-gas EMPTY-HALT into a clean
#             byte-exact fill under the executor's 5M ceiling. Rebuilds the
#             shared v4 harness and runs the pair from the umbrella `degenbot`
#             crate.
#   curve     Curve stableswap `get_dy` (YXMNWB — family 2/3 of SH6HAK). solc
#             0.8.26 harness — a faithful Solidity port of the STANDARD
#             stableswap `get_dy` (Curve's canonical source is Vyper, absent
#             here); the `simulate_swap` standard path is byte-exact to the
#             on-chain `getDy`.
#   balancer  Balancer weighted/stable (EZLECC — family 3/3 of SH6HAK). solc
#             0.7.6 harness over the CANONICAL balancer-v2-monorepo math cores
#             (FixedPoint/LogExpMath/WeightedMath/StableMath, pinned commit
#             f8b6f44); the `simulate_swap` weighted + stable
#             (invariant_version==1) paths are byte-exact.
#   pancake   PancakeSwap V3 `PancakeV3Pool.swap`. solc 0.7.6 harness over the
#             Etherscan-verified deployed source (pool 0x1445F32D1A74872bA41f3D8cF4022E9996120b31,
#             vendored under `lib/pancake-src/`); byte-exact math AND the
#             9-field `Swap` event variant decodes only via the PancakeSwap
#             decoder (not the Uniswap one).
#   pancake2  PancakeSwap V2 pair swap (the fork-fee sub-slice of the V2
#             family — the source of `tier3_v2_pair_swap_vs_revm.rs`'s
#             deferral). solc 0.5.16 harness over the REAL Ethereum-mainnet
#             `PancakePair` (hardcoded 0.25% fee = the engine's
#             `PANCAKESWAP_V2` preset, 3-tuple timestamped reserves);
#             `IntHopState::swap` byte-exact at the fork fee via the
#             K-invariant boundary.
#
# The same tests ALSO run in the default `just test-rust` (they load the
# COMMITTED bytecode from `tier3-oracle/artifacts/`, toolchain-free), so this
# recipe's unique role is regenerate + publish the artifacts (after a
# harness-source edit; `rebuild-tier3-artifacts` republishes without running
# them) and re-run the family. Recompiling dozens of revm harnesses is slow —
# run a single family, or `all`, accordingly.
test-tier3 family='all':
    #!/usr/bin/env bash
    set -euo pipefail

    run_family() {
        local harness pkg test
        case "$1" in
            step)     harness=build-tier3-harnesses.sh;           pkg=degenbot-math; test=tier3_compute_swap_step_vs_revm ;;
            swap)     harness=build-tier3-v3-swap-harness.sh;     pkg=degenbot-simulation; test=tier3_v3_pool_swap_vs_revm ;;  # 5D3YVK: relocated from pools
            v2)       harness=build-tier3-v2-swap-harness.sh;     pkg=degenbot-pools; test=tier3_v2_pair_swap_vs_revm ;;
            v4)       harness=build-tier3-v4-swap-harness.sh;     pkg=degenbot-simulation; test=tier3_v4_pool_swap_vs_revm ;;  # 5D3YVK: relocated from pools
            path5000) harness=build-tier3-v4-swap-harness.sh;     pkg=degenbot; test=tier3_path5000_v4_clamp ;;
            curve)    harness=build-tier3-curve-swap-harness.sh;  pkg=degenbot-pools; test=tier3_curve_swap_vs_revm ;;
            balancer) harness=build-tier3-balancer-swap-harness.sh; pkg=degenbot-pools; test=tier3_balancer_swap_vs_revm ;;
            pancake)  harness=build-tier3-pancake-v3-swap-harness.sh; pkg=degenbot-simulation; test=tier3_pancake_v3_swap_vs_revm ;;  # 5D3YVK: relocated from pools
            pancake2) harness=build-tier3-pancake-v2-swap-harness.sh; pkg=degenbot-pools; test=tier3_pancake_v2_swap_vs_revm ;;
            *) echo "unknown tier-3 family '$1' (families: step swap v2 v4 path5000 curve balancer pancake pancake2 | all)" >&2; exit 2 ;;
        esac
        tier3-oracle/"$harness"
        python_libdir="$(uv run --no-sync python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
        export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        cargo test --locked --manifest-path rust/Cargo.toml -p "$pkg" --test "$test"
    }

    if [ "{{ family }}" = "all" ]; then
        for f in step swap v2 v4 path5000 curve balancer pancake pancake2; do
            run_family "$f"
        done
    else
        run_family "{{ family }}"
    fi

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
# executor/contracts/cmd_executor.vy with the pinned vyper 0.5.0a3 (into a
# throwaway dir, PUBLISH=0) and byte-compare against what the Rust tier-3b tests
# load from `tier3-oracle/artifacts/executor/`. This is the authoritative
# compile-vs-use check for the vyper artifact; the toolchain-free complement
# `tier3_executor_artifacts.rs` runs in the default suite. Requires the
# toolchain: the in-repo `executor/` uv project (vyper ==0.5.0a3). Wired into
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
    tier3-oracle/build-tier3-pancake-v2-swap-harness.sh

# ========== CRAP Metric (cyclomatic complexity x coverage) ==========
#
# `cargo-crap` scores every function as CRAP = CC^2 x (1 - cov/100)^3 + CC —
# high where a function is both hard to understand and barely tested. Two
# tools, two steps, two recipes: `cargo-llvm-cov` produces the LCOV, `cargo
# crap` scores it.
#
#   just crap-coverage          # slow: instrumented rebuild + Rust tests + LCOV
#   just crap --summary         # fast: reuse the LCOV, print the per-crate roll-up
#   CRAP_LCOV=target/scoped.info CRAP_PACKAGES="-p degenbot-config" just crap-coverage
#
# Env overrides (all paths relative to rust/): CRAP_LCOV, CRAP_PACKAGES,
# CRAP_FEATURES, CRAP_THRESHOLD (default 30), CRAP_BASELINE. Tooling:
# cargo-llvm-cov + cargo-crap (cargo install --locked <name>); CI on a rustup
# toolchain also needs `rustup component add llvm-tools-preview` and must leave
# LLVM_COV/LLVM_PROFDATA unset - cargo-llvm-cov finds the component itself.
#
# Deliberately NOT wired into `pre-push` or the CI matrix yet: the coverage
# build is a full instrumented rebuild of the workspace (10-20 min cold) and the
# first run surfaced 340 functions over threshold — this is a repair backlog to
# work through by hand, not a gate to switch on. Use these recipes to iterate;
# promote `crap-gate` / `crap-ci` to a real CI job once the bulk of the
# fixes land, and `crap-baseline` / `crap-regression` to track the movement
# while it happens. .cargo-crap.toml holds the threshold + exclusions.
#
# Read the output knowing three things (all measured, see ergo DRBR7Z):
#   1. It measures the RUST suite only. pytest drives the same core through the
#      PyO3 seam and contributes no coverage here, so the binding layer
#      (`degenbot_rs`, ~60% of the findings) and the Python-driven updaters
#      score as untested even though pytest exercises them. `just coverage`
#      builds the pytest-driven arm (instrumented cdylib + LLVM_PROFILE_FILE)
#      — point CRAP_LCOV at its combined LCOV to score the union:
#        CRAP_LCOV=target/coverage/coverage.lcov just crap
#   2. `?` counts as a decision point, so `?`-chained plumbing — module
#      registration, Py-dict conversion — scores far above its real branching.
#      The CC-141 outlier of the first run was such a function.
#   3. A function cargo-crap walks but the coverage build never compiled has no
#      coverage data and scores pessimistically (CC^2 + CC). `crap-coverage`
#      therefore enables degenbot-bot/otel — instruments.rs / metrics.rs /
#      otel.rs are `#[cfg(feature = "otel")]` and were 98 phantom 0% functions
#      without it. The mirror case is unavoidable and harmless: with otel ON the
#      `#[cfg(not(feature = "otel"))]` no-op stub twin in degenbot-bot/src/lib.rs
#      is not compiled, so its ~68 one-line stubs also report no coverage data
#      (CC 1 each, CRAP 2 — they never surface in the report).
#
# All paths below are relative to the `rust/` cargo workspace root (the recipes
# cd there, because cargo-crap discovers members via `cargo metadata`).

# LCOV artifact: written by `crap-coverage`, read by `crap`/`crap-gate`.
# `rust/target/` is gitignored. CRAP_LCOV=target/scoped.info just crap
crap_lcov := env_var_or_default("CRAP_LCOV", "target/lcov.info")

# Packages to analyze. Empty = the whole workspace; set it to cargo-style
# selectors for a fast scoped loop (this also drops the feature flag, which
# only makes sense workspace-wide). CRAP_PACKAGES="-p degenbot-config -p degenbot-core"
crap_packages := env_var_or_default("CRAP_PACKAGES", "")

# Features for the coverage build. Only degenbot-bot/otel: it compiles the
# telemetry modules the walker would otherwise see as never-built. Deliberately
# The coverage build intentionally enables only `degenbot-bot/otel`; it is not
# the development-wheel alias. `pyo3/extension-module` must never be set on a
# test build, and hotpath/allocator-ctrl gate no walked source file.
crap_features := env_var_or_default("CRAP_FEATURES", "degenbot-bot/otel")

# CRAP score at or above which `crap-gate` fails. CRAP_THRESHOLD=50 just crap-gate
crap_threshold := env_var_or_default("CRAP_THRESHOLD", "30")

# JSON baseline for `crap-baseline` (written) / `crap-regression` (read).
crap_baseline := env_var_or_default("CRAP_BASELINE", "target/crap-baseline.json")

# Generate the LCOV coverage report that cargo-crap scores (slow: instrumented test run).
crap-coverage *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
        echo "ERROR: cargo-llvm-cov not found. Install: cargo install --locked cargo-llvm-cov" >&2
        exit 1
    fi
    # degenbot-python's test harnesses link libpython, same reason `test-rust`
    # exports this (resolve before cd'ing: uv wants the Python project root).
    python_libdir="$(uv run --no-sync python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # LLVM tool discovery. With a rustup toolchain cargo-llvm-cov locates
    # llvm-tools-preview by itself and these must stay UNSET. On a system-rustc
    # host (Fedora: sysroot /usr, no llvm-tools component) point it at the
    # distro LLVM, whose major version must match the rustc-bundled one:
    # compare `llvm-profdata --version` with `rustc -vV` (both say LLVM 22.1.8
    # here) — a mismatch fails the merge, not the build.
    if [[ -z "${LLVM_COV:-}" && -z "${LLVM_PROFDATA:-}" ]] \
        && command -v llvm-cov >/dev/null 2>&1 && command -v llvm-profdata >/dev/null 2>&1; then
        export LLVM_COV="$(command -v llvm-cov)"
        export LLVM_PROFDATA="$(command -v llvm-profdata)"
    fi
    cd rust
    read -r -a pkg_args <<< "{{ crap_packages }}"
    scope=(--workspace)
    if [ "${#pkg_args[@]}" -gt 0 ]; then
        scope=()
    fi
    # The feature list names workspace members, so it is only addressable when
    # the whole workspace is selected - keep it for a scoped run only if the
    # caller set CRAP_FEATURES explicitly (CRAP_FEATURES= disables it).
    features=()
    if [ -n "{{ crap_features }}" ] && { [ "${#pkg_args[@]}" -eq 0 ] || [ -n "${CRAP_FEATURES:-}" ]; }; then
        features=(--features "{{ crap_features }}")
    fi
    mkdir -p "$(dirname "{{ crap_lcov }}")"
    # --no-fail-fast: one red test must not cost us the whole coverage report;
    # the run still exits non-zero, so a red suite never scores as green.
    cargo llvm-cov "${scope[@]}" "${features[@]}" "${pkg_args[@]}" --no-fail-fast \
        --lcov --output-path "{{ crap_lcov }}" {{ args }}

# Score the existing LCOV with cargo-crap (fast; run crap-coverage first).
crap *args:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-crap >/dev/null 2>&1 || { echo "ERROR: cargo-crap not found. Install: cargo install --locked cargo-crap" >&2; exit 1; }
    cd rust
    [ -f "{{ crap_lcov }}" ] || {
        echo "ERROR: no LCOV at rust/{{ crap_lcov }} — run 'just crap-coverage' first." >&2
        echo "       (a coverage-free cargo-crap run scores every function as 0% covered)" >&2
        exit 1
    }
    read -r -a pkg_args <<< "{{ crap_packages }}"
    scope=(--workspace)
    [ "${#pkg_args[@]}" -gt 0 ] && scope=()
    cargo crap "${scope[@]}" "${pkg_args[@]}" --lcov "{{ crap_lcov }}" {{ args }}

# CI gate: exit 1 when any function exceeds CRAP_THRESHOLD (--format github/sarif pass through).
crap-gate *args:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-crap >/dev/null 2>&1 || { echo "ERROR: cargo-crap not found. Install: cargo install --locked cargo-crap" >&2; exit 1; }
    cd rust
    [ -f "{{ crap_lcov }}" ] || { echo "ERROR: no LCOV at rust/{{ crap_lcov }} — run 'just crap-coverage' first." >&2; exit 1; }
    read -r -a pkg_args <<< "{{ crap_packages }}"
    scope=(--workspace)
    [ "${#pkg_args[@]}" -gt 0 ] && scope=()
    cargo crap "${scope[@]}" "${pkg_args[@]}" --lcov "{{ crap_lcov }}" \
        --threshold {{ crap_threshold }} --fail-above {{ args }}

# Write the JSON baseline that crap-regression compares against (run on the default branch).
crap-baseline *args:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-crap >/dev/null 2>&1 || { echo "ERROR: cargo-crap not found. Install: cargo install --locked cargo-crap" >&2; exit 1; }
    cd rust
    [ -f "{{ crap_lcov }}" ] || { echo "ERROR: no LCOV at rust/{{ crap_lcov }} — run 'just crap-coverage' first." >&2; exit 1; }
    mkdir -p "$(dirname "{{ crap_baseline }}")"
    cargo crap --workspace --lcov "{{ crap_lcov }}" \
        --format json --sort file --output "{{ crap_baseline }}" {{ args }}
    echo "✓ baseline written: rust/{{ crap_baseline }} ($(wc -c < "{{ crap_baseline }}") bytes)"

# CI gate: exit 1 when any function's CRAP score rose since the baseline.
crap-regression *args:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-crap >/dev/null 2>&1 || { echo "ERROR: cargo-crap not found. Install: cargo install --locked cargo-crap" >&2; exit 1; }
    cd rust
    [ -f "{{ crap_lcov }}" ] || { echo "ERROR: no LCOV at rust/{{ crap_lcov }} — run 'just crap-coverage' first." >&2; exit 1; }
    [ -f "{{ crap_baseline }}" ] || { echo "ERROR: no baseline at rust/{{ crap_baseline }} — run 'just crap-baseline' first." >&2; exit 1; }
    read -r -a pkg_args <<< "{{ crap_packages }}"
    scope=(--workspace)
    [ "${#pkg_args[@]}" -gt 0 ] && scope=()
    cargo crap "${scope[@]}" "${pkg_args[@]}" --lcov "{{ crap_lcov }}" \
        --baseline "{{ crap_baseline }}" --fail-regression {{ args }}

# CI entrypoint: fresh coverage + the threshold gate in one command.
crap-ci: crap-coverage crap-gate

# ========== Code Coverage ==========
#
# `scripts/coverage.sh` runs three arms over LLVM instrumentation, all writing
# HTML (for humans) + `coverage.json` (llvm-cov export format, agents) under
# `rust/target/coverage/`:
#
#   just coverage-rust        # cargo workspace suite (cargo-llvm-cov)
#   just coverage-pyo3        # pytest driving an instrumented degenbot._ffi cdylib
#   just coverage             # both, merged into one report + coverage.lcov
#
# The merged report is also the highest-fidelity LCOV available (cargo suite
# ∪ pytest-driven binding coverage):
#   CRAP_LCOV=target/coverage/coverage.lcov just crap
#
# Slow (full instrumented rebuilds, ~10-20 min cold) — on-demand, not wired
# into pre-push/CI. Scoped runs keep the rebuild narrow:
#   COV_PACKAGES="-p degenbot-config" just coverage-rust
# Args pass to the pytest arm: just coverage-pyo3 tests/abi -q

coverage-rust *args:
    scripts/coverage.sh rust {{ args }}

coverage-pyo3 *args:
    scripts/coverage.sh pyo3 {{ args }}

coverage *args:
    scripts/coverage.sh all {{ args }}

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 --fix "**/*.md" "!node_modules/**" "!**/.venv/**" "!tier3-oracle/lib/**" "!logs/**"

# Lint Python files
lint-python:
    uv run --no-sync ruff check --fix src/
    uv run --no-sync ty check --fix --no-progress src/

# Lint Python (check-only; non-mutating). Mirrors the ruff+ty gate CI runs,
# minus `--fix`, so a pre-commit run cannot dirty staged files. Stricter than
# CI's `lint-python`: fails on any issue `--fix` would have auto-applied.
lint-comment-hygiene:
    scripts/hooks/comment-hygiene.sh

lint-python-check:
    uv run --no-sync ruff check src/
    uv run --no-sync ty check --no-progress src/

# Dead-code detector (off the gate — output is a triage list). Each hit
# needs an `rg` call before deletion: vulture is static and can't see
# FFI-seam callers (Rust core) or framework dispatch (pydantic validators,
# SQLAlchemy TypeDecorator signatures, `__exit__`/Protocol params). 80%
# confidence is the operating point; the 60% tier is mostly framework-
# dispatched methods (validators, properties on models, enum members).
# Complements ruff: ruff's F401 rule exempts `if TYPE_CHECKING:` imports
# and there is no ruff unreachable-code rule, so vulture catches both.
dead-code:
    uv run --no-sync vulture src/degenbot vulture_whitelist.py --min-confidence 80

# Deeper dead-code sweep — catches unused functions/methods/classes too
# (the 80% tier only catches unused variables, imports, unreachable code).
# Output is much noisier (~hundreds of findings, mostly framework-dispatched
# pydantic validators, @property on models, enum members). Use periodically
# for intentional dead-code audits; not a routine gate. Generate whitelist
# candidates with: vulture src/degenbot --min-confidence 60 --make-whitelist
dead-code-deep:
    uv run --no-sync vulture src/degenbot --min-confidence 60 --make-whitelist

# Check Python formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check-python:
    uv run --no-sync ruff format --check src/

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
    uv run --no-sync ruff format src/
    cargo fmt --manifest-path rust/Cargo.toml --all

# ========== Dependency Updates ==========

# Upgrade Python and Rust dependencies (incl. semver-major bumps) — the
# repo-local replacement for dependabot's pip + cargo ecosystems.
#
# Python — two passes, mirroring Cargo's split of requirements from lockfile:
#   1. `scripts/bump_python_deps.py` rewrites the version *requirements* in
#      pyproject.toml (main deps + pinned dependency-group entries) to the
#      latest stable on PyPI, across semver major boundaries (e.g.
#      `pydantic ~= 2.13` -> `~= 2.14`) — the `cargo upgrade` analog. Plain
#      `uv sync --upgrade`/`uv lock --upgrade` cannot do this: re-resolving
#      only advances within the existing ranges. Unpinned dev-group entries
#      are already open-ended, and the script honors the [tool.uv]
#      `exclude-newer` horizon so the new pins remain resolvable by uv.
#   2. `uv lock --upgrade` re-resolves uv.lock inside the new ranges — direct
#      + transitive, all groups — the `cargo update` analog; `uv sync` then
#      refreshes the venv.
#
# Rust — two passes, because Cargo splits the job:
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
update-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    uv run --no-sync python scripts/bump_python_deps.py
    uv lock --upgrade
    uv sync
    cargo upgrade --manifest-path rust/Cargo.toml --incompatible
    cargo update --manifest-path rust/Cargo.toml

# ========== Repository Setup ==========

# Refresh the committed DEGENBOT_* key-inventory snapshot the
# degenbot-config schema-completeness test compares against (reviewer note,
# 7LKJFY sign-off). The exclusion glob keeps the snapshot self-contamination
# (regeneration rewriting its own source) out of the sweep.
env-inventory:
    #!/usr/bin/env bash
    set -euo pipefail
    target="rust/crates/foundation/degenbot-config/tests/degenbot_env_inventory.txt"
    rg -o 'DEGENBOT_[A-Z_]+' rust/crates --no-filename \
        -g '!**/degenbot_env_inventory.txt' \
        -g '!**/degenbot-config/tests/**' | sort -u > "$target"
    echo "✓ inventory snapshot refreshed: $target ($(wc -l < "$target") keys)"
    echo '  then: cd rust && REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config'

# Install prek git hooks and configure template.
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
    echo "    pre-push   : object-DB GC + commitlint push-range + Rust/Python lint"
    echo "                 (clippy/ty) + build & test suite (rust build/test,"
    echo "                 python build/test)"
    echo "    Bypass: git push --no-verify (CI still runs)."
    echo "✓ commit template configured."


# ========== Settlement-Bot Parity Gate (RSP-8 / ergo 23DLCY) ==========
#
# The executable successor to the parity ledger
# (docs/architecture/rust-settlement-bot-parity.md): the fixture boot gate
# (Rust integration test + Python PyO3 probe against the shared oracle
# tests/standalone_parity/fixtures/settlement_bot_boot.json) plus the
# recorded dual-driver decision diff. CI-safe and offline (no RPC); the live
# anvil arm is opt-in via DEGENBOT_DUAL_DRIVER_GATE=1 + DEGENBOT_FORK_RPC
# (tests/standalone_parity/dual_driver_gate.py --live).
test-settlement-parity:
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-settlement-bot-example --test boot_gate
    uv run --no-sync pytest tests/standalone_parity/test_settlement_bot_boot_gate.py tests/standalone_parity/test_settlement_bot_dual_driver_gate.py -q
    uv run --no-sync python tests/standalone_parity/dual_driver_gate.py --recorded

# ========== No-Python Console Gate (ADR-051 D10 / ergo EA6DY7) ==========
#
# The Rust-owned console (`degenbot`) must build and run with NO Python in the
# path. This recipe builds the `degenbot-cli` binary, then runs the fixed argv
# smoke set (--help / --version / `database inspect` over committed
# Alembic-stamped fixtures copied to a temp dir / `exchange activate`+`deactivate`
# idempotence on a fresh DB copy) and diffs the machine-checkable stdout against
# the checked-in oracle
# (.github/workflows/cli-no-python-expected.txt). Bash + sed + diff only — no
# Python, no uv. Mirrors the `cli-no-python` CI job.
#
# Seeded-divergence proof (the comparator must be able to fail), selectable by
# DEGENBOT_CLI_GATE_SEED (both must exit 0, i.e. the injected divergence WAS
# caught):
#   DEGENBOT_CLI_GATE_SEED=expected just ci-no-python-cli-gate
#       mutate one expected line in a temp oracle copy; the diff must trip.
#   DEGENBOT_CLI_GATE_SEED=live just ci-no-python-cli-gate
#       point `database inspect` at a Rust-owned DB copy (real binary, real
#       different output); the diff must trip.
# Authoring aid only: DEGENBOT_CLI_GATE_DUMP_ACTUAL=1 prints the normalized
# capture so the oracle can be regenerated deliberately.
ci-no-python-cli-gate:
    cargo build --locked -p degenbot-cli --manifest-path rust/Cargo.toml
    bash .github/workflows/cli-no-python-gate.sh

# ADR-052 D6: Alembic is retired in-tree. No `alembic` reference may survive in any
# Python source under src/, and the migration-scripts package must be gone.
# Mirrors check-no-pyo3-in-cores: a permanent, mechanical sweep gate.
check-no-alembic:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- no_alembic_references --exact --nocapture

# ADR-052 D7: SQLAlchemy and the Python ORM are retired from the runtime and
# test-owned Python surfaces. The gate also checks the root dependency and lock
# declarations and proves its AST detector rejects a synthetic import.
check-no-sqlalchemy:
    uv run --no-sync pytest -q tests/test_no_sqlalchemy_test_imports.py

# ========== Stub-to-Runtime Drift Gate (ADR-053, ergo XNEJRD) ==========
#
# mypy.stubtest replaces the bespoke drift gate's R1/R3/R4 mechanics (and R2,
# verified below): it introspects the INSTALLED degenbot._ffi extension and
# compares it against the hand-maintained src/degenbot/_ffi/*.pyi stubs in
# BOTH directions, across every class member, with no curated table. The
# allowlist (tests/rust/stubtest_allowlist.txt) carries only PyO3-
# introspection noise and stub-only type exemptions, each group annotated with
# the drift rule it serves. What stubtest cannot see (Python-side surface in
# driver modules) stays in tests/rust/test_ffi_registration_surface.py.
lint-stubtest:
    #!/usr/bin/env bash
    set -euo pipefail
    # --ignore-disjoint-bases: PEP 800 disjoint-base flags on every #[pyclass]
    # are PyO3-synthetic runtime semantics, not stub drift. Staleness of the
    # introspected .so is owned by the receipt gates (verify-build-fresh,
    # AGENTS.md) — stubtest simply checks whatever is installed.
    uv run --no-sync stubtest --ignore-disjoint-bases \
        --allowlist tests/rust/stubtest_allowlist.txt degenbot._ffi
