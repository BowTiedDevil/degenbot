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
# the 1-vs-21 drift that felled the 0.6.0-alpha.6 crates.io publish is impossible
# here. Pass the crates.io SEMVER form (0.6.0-alpha.7), not the PEP440 tag form
# (0.6.0a7). Requires cargo-edit: cargo install cargo-edit
bump-version version:
    cargo set-version --workspace {{ version }} --manifest-path rust/Cargo.toml

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

# Run every pre-push gate manually, in hook order and fail-fast — the
# commitlint push-range re-lint, then the Rust/Python code linters, then the
# Rust and Python build + test tracks, exactly as the installed prek pre-push
# hook runs them (prek.toml, stages = ["pre-push"]). The installed hook and
# ci.yml stay authoritative on an actual push; this is for checking the gates
# with `just pre-push` before `git push`.
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

    run_gate "commitlint (push range)" scripts/hooks/commitlint-push.sh
    run_gate "Rust clippy"             just lint-rust-check
    run_gate "Python lint"             just lint-python-check
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
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # vendored deployments.json (degenbot-uniswap) must match the canonical
    # Python-tree registry file byte-for-byte (TGO5ZY: a crate can only
    # embed in-tarball files, so the embed uses the in-crate mirror)
    cmp -s src/degenbot/registry/deployments.json rust/crates/degenbot-uniswap/src/deployments.json || { echo 'ERROR: deployments.json vendor drift (canonical vs degenbot-uniswap mirror)' >&2; exit 1; }
    cargo test --manifest-path rust/Cargo.toml --workspace

# crates.io publish oracle (crates-io-publishing-prep handoff §2, gate G1):
# verification-builds every publishable workspace member in dependency order.
# ~20-40 min cold. CI's PR gate (check-publish) runs the clean-tree form.
publish-dry-run:
    #!/usr/bin/env bash
    set -euo pipefail
    cd rust
    cargo publish --workspace --dry-run --allow-dirty

# Run Rust linter (clippy)
lint-rust:
    cargo clippy --fix --all-targets --all-features --allow-dirty --manifest-path rust/Cargo.toml -- --deny warnings

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
    for crate in degenbot-core degenbot-math degenbot-abi degenbot-rpc degenbot-bot degenbot-decoders degenbot-uniswap degenbot-pathfinding degenbot degenbot-price degenbot-db degenbot-pool-updater degenbot-aave degenbot-execution degenbot-executor degenbot-submission degenbot-simulation degenbot-pools degenbot-solvers degenbot-order-index degenbot-arbitrage degenbot-fork degenbot-execution-sample; do
        if cargo tree --manifest-path rust/Cargo.toml -p "$crate" 2>/dev/null | grep -qi 'pyo3 v'; then
            echo "ERROR: $crate pulls pyo3 under default features (must be feature-gated)." >&2
            exit 1
        fi
    done
    echo "OK: core crates + umbrella are pyo3-free under default features"

# Build Rust extension module (correct for Python extension)
build-rust-extension:
    cargo build -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# ========== Build-Artifact Housekeeping ==========

# Reclaim disk space from the cargo build cache (rust/target). Incremental
# caches are pure loss and are always removed; deps/examples/build/
# .fingerprint entries with mtime older than {{age}} days are swept
# cargo-sweep style - cargo transparently rebuilds anything still referenced
# on its next use. `target/maturin` is NEVER touched so `uv run maturin
# develop` stays warm (see the stale-`.so` rule in AGENTS.md before
# forcing anything colder than that).
# Default is a PREVIEW: per-subtree reclaimable sizes, deletes nothing. Run
# the destructive pass explicitly:
#   just gc-target              # preview (deletes nothing), 14-day horizon
#   DRY=1 just gc-target        # delete artifacts older than the horizon
#   AGE=0 DRY=1 just gc-target  # sweep everything except target/maturin
#   DUPES=1 just gc-target      # preview the stale-variant dedupe too
#   DUPES=1 DRY=1 just gc-target  # delete stale variants, keep newest per basename
# DUPES=1 also sweeps duplicate-hash test binaries: cargo hashes metadata into
# each artifact name, so feature-flag/env churn (extension-module, hotpath,
# CI parity runs) leaves older variants of the same suite piling up at ~0.5 GiB
# apiece. Only executables >= 10 MiB are considered, and the newest mtime in
# each basename group is kept - anything else recompiles on its next use.
gc-target:
    #!/usr/bin/env bash
    set -euo pipefail
    age=${AGE:-14}
    dupes=${DUPES:-}
    dry=${DRY:-}
    roots=(rust/target/debug rust/target/release rust/target/rust-analyzer/debug)
    subtrees=(deps examples build .fingerprint)
    total_kb=0
    for root in "${roots[@]}"; do
        [ -d "$root" ] || continue
        inc="$root/incremental"
        if [ -z "${dry}" ]; then
            if [ -d "$inc" ]; then
                kb=$(du -sk "$inc" | cut -f1)
                total_kb=$((total_kb + kb))
            fi
        elif [ -d "$inc" ]; then
            rm -rf "$inc" 2>/dev/null || echo "WARN: could not fully remove $inc (a build may be writing into it)" >&2
        fi
        for sub in "${subtrees[@]}"; do
            dir="$root/$sub"
            if [ ! -d "$dir" ]; then continue; fi
            stale=$(find "$dir" -mindepth 1 -maxdepth 1 -mtime +"${age}" 2>/dev/null || true)
            if [ -z "$stale" ]; then continue; fi
            if [ -z "${dry}" ]; then
                kb=$(du -skc $stale 2>/dev/null | tail -1 | cut -f1)
                total_kb=$((total_kb + kb))
            else
                rm -rf $stale 2>/dev/null || echo "WARN: partial removal in $dir" >&2
            fi
        done
    done
    # Stale duplicate-hash variants (see dupes=1 in the header comment). Only
    # extensionless executables >= 10 MiB; the newest mtime per basename wins.
    if [ -n "${dupes}" ] && [ "${dupes}" != "0" ]; then
        tmp=$(mktemp)
        for dir in rust/target/debug/deps rust/target/debug/examples rust/target/release/deps rust/target/release/examples rust/target/rust-analyzer/debug/deps; do
            if [ ! -d "$dir" ]; then continue; fi
            find "$dir" -maxdepth 1 -type f -size +10M -printf '%T@ %f\n' \
                | awk '{ b=$2; sub(/-[0-9a-f]{16}$/, "", b); if (b in newest) { if ($1 > newest[b]) { print path[b]; newest[b]=$1; path[b]=$2 } else print $2 } else { newest[b]=$1; path[b]=$2 } }' > "$tmp"
            if [ -s "$tmp" ]; then
                dupes_list=$(awk -v d="$dir" '{ print d "/" $0 }' "$tmp")
                kb=$(du -skc $dupes_list 2>/dev/null | tail -1 | cut -f1)
                n=$(wc -l < "$tmp")
                if [ -n "${dry}" ]; then
                    rm -f $dupes_list
                    total_kb=$((total_kb + kb))
                    echo "deduped: $n stale variants in $dir"
                else
                    total_kb=$((total_kb + kb))
                    echo "dedupe candidate: $n stale variants in $dir ($(( kb / 1024 )) MiB)"
                fi
            fi
        done
        rm -f "$tmp"
    fi
    freed=$(numfmt --to=iec "$((total_kb * 1024))" 2>/dev/null || echo "${total_kb} KiB")
    if [ -z "${dry}" ]; then
        echo "PREVIEW: $freed reclaimable at a ${age}-day horizon (+dupes if requested) — pass dry=1 to delete"
    else
        echo "swept: ~$freed freed (rust/target now: $(du -sh rust/target | cut -f1))"
    fi

# ========== Python Development ==========

# Build and install Python extension in development mode
dev:
    uv run maturin develop

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
        python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
        export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        cargo test --manifest-path rust/Cargo.toml -p "$pkg" --test "$test"
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

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 --fix "**/*.md" "!node_modules/**" "!.opencode/node_modules/**" "!**/.venv/**" "!tier3-oracle/lib/**"

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
    uv run python scripts/bump_python_deps.py
    uv lock --upgrade
    uv sync
    cargo upgrade --manifest-path rust/Cargo.toml --incompatible
    cargo update --manifest-path rust/Cargo.toml

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

