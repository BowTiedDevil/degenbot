# Commands
- `uv run pytest -x -q --no-header --tb=no -n0 --no-cov` - Run tests
- `uv run ruff check` - Lint Python code
- `uv run mypy` - Type check Python code
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)
- `cd rust && cargo clippy --all-targets --all-features` - Check Rust code
- `cd rust && cargo test` - Test Rust code