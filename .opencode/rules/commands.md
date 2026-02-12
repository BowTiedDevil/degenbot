# Commands

- `uv run pytest tests/...::test_name` - Run specific test
- `uv run pytest` - Run all tests
- `uv run pytest -n0` - Run without parallelization
- `uv run ruff check` - Lint Python code
- `uv run mypy` - Type check Python code
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)