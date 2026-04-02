# Justfile for degenbot development
# https://github.com/casey/just

# Default recipe - show available commands
default:
    @just --list

# ========== Rust Development ==========

# Run Rust tests
test-rust:
    cd rust && cargo test

# Run Rust linter (clippy)
lint-rust:
    cd rust && cargo clippy --all-targets --all-features -- -D warnings

# Build Rust release library (links Python - for testing only)
build-rust-debug:
    cd rust && cargo build --release

# Build Rust extension module (correct for Python extension)
build-rust-extension:
    cd rust && cargo build --release --features extension-module

# ========== Python Development ==========

# Build and install Python extension in development mode
dev:
    uv run maturin develop --release

# Build Python extension wheels
build-wheels:
    uv run maturin build --release

# Run Python tests
test-python:
    uv run pytest tests/ -x -q --no-header

# Run Python tests with coverage
test-python-cov:
    uv run pytest tests/ -x -q --no-header --cov=src/degenbot --cov-branch

# Run all tests (Rust + Python)
test-all: test-rust test-python

# ========== Code Quality ==========

# Run all linters (Rust + Python)
lint: lint-rust
    uv run ruff check src/
    uv run mypy src/

# Format all code
format:
    cd rust && cargo fmt
    uv run ruff format src/

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: lint-rust test-rust
    cd rust && cargo build --release --features extension-module

# Simulate full CI pipeline
ci-full: ci-rust test-python

# ========== Documentation ==========

# Build documentation
docs:
    cd rust && cargo doc --no-deps
    uv run mkdocs build 2>/dev/null || echo "mkdocs not configured"

# Serve documentation locally
serve-docs:
    cd rust && cargo doc --open 2>/dev/null || echo "Open rust/target/doc/degenbot_rs/index.html"
