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

# Run Rust tests
test-rust:
    #!/usr/bin/env bash
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml --workspace

# Run wrapped Rust Python tests
test-rust-python:
    uv run pytest tests/rust -x -q --no-header

# Run Rust linter (clippy)
lint-rust:
    cargo clippy --fix --all-targets --all-features --fix --allow-dirty --manifest-path rust/Cargo.toml -- --deny warnings

# Lint Rust (check-only; non-mutating). Mirrors the clippy gate CI runs,
# minus `--fix`, so a pre-push run cannot dirty committed files. Stricter than
# CI's `lint-rust`: fails on any warning `--fix` would have auto-applied.
lint-rust-check:
    cargo clippy --all-targets --all-features --manifest-path rust/Cargo.toml -- --deny warnings

# Check Rust formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check:
    cargo fmt --manifest-path rust/Cargo.toml --all -- --check

# Enforce the no-pyo3-in-core invariant (Plan 103). Pure Rust core crates must
# not depend on pyo3 under their default features. Add new core crates here.
check-no-pyo3-in-cores:
    #!/usr/bin/env bash
    set -euo pipefail
    for crate in degenbot-core degenbot-cl-math degenbot-abi degenbot-rpc degenbot-bot degenbot-decoders degenbot-uniswap degenbot-pathfinding degenbot degenbot-solidly-math degenbot-evm-math degenbot-price degenbot-db degenbot-pool-updater degenbot-executor degenbot-submission degenbot-simulation; do
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

# Compile Solidity test contracts
compile-test-contracts:
    cd tests/aave/libraries/contracts && forge build --quiet

# Run Python tests
test-python: compile-test-contracts
    uv run pytest -x -q --no-header

# Run only on-chain-oracle parity tests in REPLAY mode (offline, CI-safe, no RPC/secrets).
# Replay is read-only (asserts against recorded ints), so xdist parallelism is
# safe — the shared-golden-file race only affects record mode (see below).
test-offline-parity: compile-test-contracts
    uv run pytest -m onchain_oracle -q --no-header

# Re-populate golden files for on-chain-oracle parity tests. Requires a working
# fork (tests.env RPC or local node). Pass a nodeid to refresh a single test:
#   just record-golden -- tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations
# Single-process (-n0): parametrized parity tests share one golden file per
# test function and accumulate keys across params; xdist parallelism would race
# the shared file (last-writer-wins, losing keys). Replay is read-only and safe
# under xdist, but record accumulates writes.
record-golden *args: compile-test-contracts
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

# Run all tests (Rust + Python)
test-all: test-rust test-python

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 --fix "**/*.md" "!node_modules/**" "!.opencode/node_modules/**" "!.venv/**"

# Enforce the context-map maintenance contract (docs/agents/context-map-maintenance.md).
# Banned: brace dialect {Foo}, status-prose markers, references to the deleted
# connection module. No-op deps; grep-based; fails loud on drift.
lint-context-maps:
    #!/usr/bin/env bash
    set -euo pipefail
    files=( CONTEXT-MAP.md rust/CONTEXT.md src/degenbot/*/CONTEXT.md )
    fail=0
    # 1. Brace dialect is banned (use real markdown links or plain **Term**)
    if rg -n --no-heading "\{[A-Z][^}]+\}" "${files[@]}"; then
      echo "ERROR: brace dialect {Foo} is banned in context maps — use plain **Term** or a markdown link." >&2
      fail=1
    fi
    # 2. Status-prose markers are banned (status belongs in ADRs, not vocabulary)
    if rg -n --no-heading "Status: complete|Implementation status|Revised by ADR-|Prior to ADR-" CONTEXT-MAP.md rust/CONTEXT.md; then
      echo "ERROR: status-prose markers are banned in context maps (see docs/agents/context-map-maintenance.md)." >&2
      fail=1
    fi
    # 3. No references to the deleted connection module
    if rg -n --no-heading --ignore-case "connection manager" CONTEXT-MAP.md src/degenbot/provider/CONTEXT.md; then
      echo "ERROR: connection module was deleted (ADR-006 slice 8b); remove the reference." >&2
      fail=1
    fi
    if rg -n --no-heading "src/degenbot/connection/CONTEXT" CONTEXT-MAP.md; then
      echo "ERROR: src/degenbot/connection/ no longer exists; remove the link." >&2
      fail=1
    fi
    # 4. Relative-link targets must resolve on disk (resolved per-file)
    while IFS= read -r line; do
      # line is "filename:match", where match is '](target)'
      file="${line%%:*}"
      match="${line#*:}"              # "]../types/CONTEXT.md)"
      target="${match#\](}"           # strip "](" prefix → "../types/CONTEXT.md)"
      target="${target%)}"            # strip trailing ")"
      [[ -z "$target" ]] && continue
      dir="$(dirname "$file")"
      resolved="$dir/$target"
      if [[ ! -e "$resolved" ]]; then
        echo "ERROR: $file: broken CONTEXT link '$target' (resolved '$resolved' does not exist)." >&2
        fail=1
      fi
    done < <(rg --no-heading -o --with-filename "\]\([^)]+CONTEXT\.md\)" CONTEXT-MAP.md rust/CONTEXT.md src/degenbot/*/CONTEXT.md)
    exit $fail

# Lint Python files
lint-python:
    uv run ruff check --fix src/ 
    uv run ty check --fix --no-progress src/

# Lint Python (check-only; non-mutating). Mirrors the ruff+ty gate CI runs,
# minus `--fix`, so a pre-push run cannot dirty committed files. Stricter than
# CI's `lint-python`: fails on any issue `--fix` would have auto-applied.
lint-python-check:
    uv run ruff check src/
    uv run ty check --no-progress src/

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
lint: fmt-check fmt-check-python lint-rust lint-python lint-markdown lint-context-maps

# Format all code
format:
    uv run ruff format src/
    cargo fmt --manifest-path rust/Cargo.toml --all

# ========== Dependency Updates ==========

# Upgrade Python and Rust dependencies
update-deps:
    uv sync --upgrade
    cargo update --manifest-path rust/Cargo.toml

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: fmt-check check-no-pyo3-in-cores lint-rust test-rust
    cargo build --release -p degenbot_rs --features extension-module --manifest-path rust/Cargo.toml

# Simulate full CI pipeline
ci-full: ci-rust lint-markdown test-python

# ========== Repository Setup ==========

# Install prek git hooks and configure commit template.
# Run this once after cloning so commit messages are linted locally at
# commit time AND pre-push (catches `--no-verify` bypasses before they leave
# the machine, strictly earlier than CI). Hooks are declared in prek.toml.
# For manual range checks: just lint-commits.
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
    echo "    commit-msg : commitlint"
    echo "    pre-push   : commitlint push-range re-lint + full CI mirror"
    echo "                 (rust fmt/clippy/build/test, markdown lint,"
    echo "                  python build/fmt/lint/test)"
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
