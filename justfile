# Justfile for degenbot development
# https://github.com/casey/just

# Default recipe - show available commands
default:
    @just --list

# ========== Rust Development ==========

# Run Rust tests (auto-initialize only; do not pass `extension-module` here)
test-rust:
    env PYO3_PYTHON="{{justfile_directory()}}/.venv/bin/python" cargo test --features auto-initialize --manifest-path rust/Cargo.toml -- --test-threads=1

# Run wrapped Rust Python tests
test-rust-python:
    env -u RUST_LOG uv run pytest tests/rust -x -q --no-header

# Run Stylus contract tests
test-stylus:
    cargo test --manifest-path stylus/Cargo.toml --locked --offline --lib --features native-test

# Run Rust linter (clippy)
lint-rust:
    env PYO3_PYTHON="{{justfile_directory()}}/.venv/bin/python" cargo clippy --all-targets --manifest-path rust/Cargo.toml -- -D warnings

# Run Stylus linter
lint-stylus:
    cargo clippy --manifest-path stylus/Cargo.toml --tests --locked --offline --features native-test -- -D warnings

# Build Rust release library (links Python - for testing only)
build-rust-debug:
    env PYO3_PYTHON="{{justfile_directory()}}/.venv/bin/python" cargo build --release --manifest-path rust/Cargo.toml

# Build Rust extension wheel (correct Python linker and abi3 metadata)
build-rust-extension:
    env -u RUST_LOG uv run --no-project maturin build --release --manifest-path rust/Cargo.toml

# Generate Alloy/Rust bindings from the parent mev-arbitrum Foundry contracts
gen-contract-bindings contracts_root="../../contracts" bindings_path="rust/crates/contract_bindings":
    env -u RUST_LOG uv run python -m degenbot.devtools.foundry_bindings generate --contracts-root "{{justfile_directory()}}/{{contracts_root}}" --bindings-path "{{justfile_directory()}}/{{bindings_path}}"

# Check generated contract bindings are in sync with parent Foundry artifacts
check-contract-bindings contracts_root="../../contracts" bindings_path="rust/crates/contract_bindings":
    env -u RUST_LOG uv run python -m degenbot.devtools.foundry_bindings check --contracts-root "{{justfile_directory()}}/{{contracts_root}}" --bindings-path "{{justfile_directory()}}/{{bindings_path}}"

# ========== Python Development ==========

# Build and install Python extension in development mode
dev:
    env -u RUST_LOG uv run --no-project maturin develop --manifest-path rust/Cargo.toml
    just fix-ext

# Re-align the built extension's Mach-O __LINKEDIT (pre-release macOS/linker
# workaround: Darwin 27 dyld rejects the mis-aligned LINKEDIT string pool that
# ld-1328 / lld-22 emit for this binary). Idempotent; runs as a standalone
# script so it works even when the freshly built .so does not yet load. Remove
# this once the system linker emits an 8-aligned LINKEDIT string pool.
fix-ext:
    env -u RUST_LOG uv run python src/degenbot/devtools/fix_macho_linkedit_alignment.py src/degenbot/degenbot_rs.abi3.so

# Build Python extension wheels
build-wheels:
    env -u RUST_LOG uv run --no-project maturin build --release --manifest-path rust/Cargo.toml

# Compile Solidity test contracts
compile-test-contracts:
    cd tests/aave/libraries/contracts && forge build --quiet

# Run Python tests
test-python: compile-test-contracts
    env -u RUST_LOG uv run pytest tests/ -x -q --no-header

# Run Autoresearch harness tests
test-autoresearch:
    env -u RUST_LOG uv run pytest -o addopts="" tests/autoresearch --confcutdir=tests/autoresearch -x -q --no-header

# Run Python tests that require live RPC endpoints
test-python-live: compile-test-contracts
    env -u RUST_LOG uv run pytest tests/ -x -q --no-header --run-live-rpc

# Run Python tests that require an operator-populated local database
test-python-database:
    env -u RUST_LOG uv run pytest tests/database -x -q --no-header --run-database

# Run every Python test category; requires reliable RPC and database prerequisites
test-python-all: compile-test-contracts
    env -u RUST_LOG uv run pytest tests/ -x -q --no-header --run-live-rpc --run-database

# Run Python tests with coverage
test-python-cov: compile-test-contracts
    env -u RUST_LOG uv run pytest tests/ -x -q --no-header --cov=src/degenbot --cov-branch

# Run all tests (Rust + Python)
test-all: test-rust test-stylus test-python

# ========== Code Quality ==========

# Run all linters (Rust + Python)
lint: lint-rust lint-stylus
    env -u RUST_LOG uv run ruff check src/
    env -u RUST_LOG uv run mypy src/

# Format all code
format: 
    cargo fmt --manifest-path rust/Cargo.toml
    cargo fmt --manifest-path stylus/core/Cargo.toml
    cargo fmt --manifest-path stylus/executor_abi_adapter/Cargo.toml
    cargo fmt --manifest-path stylus/lp_transfer_adapter/Cargo.toml
    cargo fmt --manifest-path stylus/pool_adapter/Cargo.toml
    cargo fmt --manifest-path stylus/runtime_adapter/Cargo.toml
    cargo fmt --manifest-path stylus/token_risk_adapter/Cargo.toml
    env -u RUST_LOG uv run ruff format src/

# ========== CI/CD ==========

# Simulate CI Rust checks
ci-rust: lint-rust test-rust
    env -u RUST_LOG uv run --no-project maturin build --release --manifest-path rust/Cargo.toml

# Simulate CI Stylus checks
ci-stylus: lint-stylus test-stylus
    cargo build --manifest-path stylus/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline

# Check deployable Stylus artifacts against an Arbitrum endpoint.
stylus-check endpoint="https://arb1.arbitrum.io/rpc":
    cargo build --manifest-path stylus/runtime_adapter/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline
    cargo build --manifest-path stylus/executor_abi_adapter/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline
    cargo build --manifest-path stylus/lp_transfer_adapter/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline
    cargo build --manifest-path stylus/pool_adapter/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline
    cargo build --manifest-path stylus/token_risk_adapter/Cargo.toml --release --target wasm32-unknown-unknown --locked --offline
    cd stylus/runtime_adapter && cargo stylus check --endpoint {{endpoint}} --wasm-file ../target/wasm32-unknown-unknown/release/degenbot_runtime_adapter.wasm
    cd stylus/executor_abi_adapter && cargo stylus check --endpoint {{endpoint}} --wasm-file ../target/wasm32-unknown-unknown/release/degenbot_executor_abi_adapter.wasm
    cd stylus/lp_transfer_adapter && cargo stylus check --endpoint {{endpoint}} --wasm-file ../target/wasm32-unknown-unknown/release/degenbot_lp_transfer_adapter.wasm
    cd stylus/pool_adapter && cargo stylus check --endpoint {{endpoint}} --wasm-file ../target/wasm32-unknown-unknown/release/degenbot_pool_adapter.wasm
    cd stylus/token_risk_adapter && cargo stylus check --endpoint {{endpoint}} --wasm-file ../target/wasm32-unknown-unknown/release/degenbot_token_risk_adapter.wasm

# Probe local WebAssembly inspection dependencies
stylus-wasm-probe:
    stylus/tools/wasm-inspect.sh --probe

# Inspect a compiled Stylus wasm artifact
stylus-wasm-inspect wasm="stylus/target/wasm32-unknown-unknown/release/degenbot_stylus_core.wasm":
    stylus/tools/wasm-inspect.sh --wasm {{ wasm }}

# Simulate full CI pipeline
ci-full: ci-rust ci-stylus test-python

# ========== Documentation ==========

# Build documentation
docs:
    cargo doc --no-deps --manifest-path rust/Cargo.toml
    env -u RUST_LOG uv run mkdocs build 2>/dev/null || echo "mkdocs not configured"

# Serve documentation locally
serve-docs:
    cargo doc --open 2>/dev/null --manifest-path rust/Cargo.toml || echo "Open rust/target/doc/degenbot_rs/index.html"
