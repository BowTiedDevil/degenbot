# Justfile for degenbot development
# https://github.com/casey/just

# Default recipe - show available commands
default:
    @just --list

# ========== Rust Development ==========

# Run Rust tests
test-rust:
    cargo test --features auto-initialize --manifest-path rust/Cargo.toml -- --test-threads=1

# Run wrapped Rust Python tests
test-rust-python:
    uv run pytest tests/rust -x -q --no-header

# Run Rust linter (clippy)
lint-rust:
    cargo clippy --all-targets --all-features --manifest-path rust/Cargo.toml -- -D warnings

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
    uv run pytest tests/ -x -q --no-header

# Run Python tests with coverage
test-python-cov: compile-test-contracts
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
    cargo fmt --manifest-path rust/Cargo.toml
    uv run ruff format src/

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: lint-rust test-rust
    cargo build --release --features extension-module --manifest-path rust/Cargo.toml

# Simulate full CI pipeline
ci-full: ci-rust test-python

# ========== Documentation ==========

# Build documentation
docs:
    cargo doc --no-deps --manifest-path rust/Cargo.toml
    uv run mkdocs build 2>/dev/null || echo "mkdocs not configured"

# Serve documentation locally
serve-docs:
    cargo doc --open 2>/dev/null --manifest-path rust/Cargo.toml || echo "Open rust/target/doc/degenbot_rs/index.html"
