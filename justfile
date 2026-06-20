# Justfile for degenbot development
# https://github.com/casey/just

# Default recipe - show available commands
default:
    @just --list

# ========== Rust Development ==========

# Run Rust tests
test-rust:
    #!/usr/bin/env bash
    python_libdir="$(.venv/bin/python3 -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    cargo test --manifest-path rust/Cargo.toml

# Run wrapped Rust Python tests
test-rust-python:
    uv run pytest tests/rust --ff -x -q --no-header

# Run Rust linter (clippy)
lint-rust:
    cargo clippy --all-targets --all-features --fix --allow-dirty --manifest-path rust/Cargo.toml -- --deny warnings

# Check Rust formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check:
    cargo fmt --manifest-path rust/Cargo.toml -- --check

# Build Rust release library (links Python - for testing only)
build-rust-debug:
    cargo build --release --manifest-path rust/Cargo.toml

# Build Rust extension module (correct for Python extension)
build-rust-extension:
    cargo build --release --features extension-module --manifest-path rust/Cargo.toml

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
    uv run pytest tests/ --ff -x -q --no-header

# Run all tests (Rust + Python)
test-all: test-rust test-python

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 "**/*.md" "!node_modules/**" "!.opencode/node_modules/**" "!.venv/**"

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
    uv run ruff check src/
    uv run ty check --no-progress src/

# Check Python formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check-python:
    uv run ruff format --check src/

# Run all linters (Rust + Python + Markdown)
lint: fmt-check fmt-check-python lint-rust lint-python lint-markdown lint-context-maps

# Format all code
format: 
    cargo fmt --manifest-path rust/Cargo.toml
    uv run ruff format src/

# ========== Dependency Updates ==========

# Upgrade Python and Rust dependencies
update-deps:
    uv sync --upgrade
    cargo update --manifest-path rust/Cargo.toml

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: fmt-check lint-rust test-rust
    cargo build --release --features extension-module --manifest-path rust/Cargo.toml

# Simulate full CI pipeline
ci-full: ci-rust lint-markdown test-python

# ========== Repository Setup ==========

# Install git hooks and configure commit template
setup-git-hooks:
    git config core.hooksPath .githooks
    git config commit.template .commit-template
    @echo "✓ Git hooks and commit template configured."

# ========== Documentation ==========

# Render a Mermaid diagram (Markdown or .mmd) to PNG.
# Example: just mermaid-png docs/architecture/rust-solver-engine.md
mermaid-png input output='':
    scripts/mermaid-export {{input}} {{output}} -f png

# Render a Mermaid diagram (Markdown or .mmd) to SVG.
# Example: just mermaid-svg docs/architecture/rust-solver-engine.md
mermaid-svg input output='':
    scripts/mermaid-export {{input}} {{output}} -f svg

# Build documentation
docs:
    cargo doc --no-deps --manifest-path rust/Cargo.toml
    uv run mkdocs build 2>/dev/null || echo "mkdocs not configured"

# Serve documentation locally
serve-docs:
    cargo doc --open 2>/dev/null --manifest-path rust/Cargo.toml || echo "Open rust/target/doc/degenbot_rs/index.html"
