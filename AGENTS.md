# AGENTS.md

## Refactoring & Feature Development

Use Red/Green TDD while refactoring and implementing new features.

## Commands

### Python
- `uv run pytest -x -q --no-header --tb=no -n0 --no-cov` - Run Python tests
- `uv run ruff check` - Run Python code lint checks
- `uv run mypy` - Run Python code type checks

### Rust
- `cargo clippy --all-targets --all-features --manifest-path rust/Cargo.toml` - Run Rust code checks
- `cargo test --manifest-path rust/Cargo.toml` - Run Rust tests
- `cargo build --release --features extension-module --manifest-path rust/Cargo.toml` - Build Python extension

## Database
- SQLite database path configured in `~/.config/degenbot/config.toml`
- SQLAlchemy ORM models in `src/degenbot/database/models/`
- Use scoped session from `degenbot.database.db_session()`

## Python Design

### Patterns
- Prefer frozen `dataclass` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known

### Docstrings
Minimal PEP 257. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class SomeClass:
    """
    Class description.
    
    [additional detail as needed]
    """

    pass
```

### Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

### Logging
- Use `degenbot.logger`

### Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

## Refactoring

- Unless directed otherwise, design standalone features without a backwards compatibility layer. Use a feature flag during development and testing to enable hard cutover.

## Solidity

### Arithmetic wrapping behavior
- Solidity arithmetic silently wraps for versions < 0.8.0, e.g. `uint8(255) + 1 == 0`
- Solidity arithmetic is checked by default for versions 0.8.0+, e.g. `uint8(255) + 1` will revert

### Porting to Python
- Solidity contracts requires explicit integer division to match the EVM. The Python `//` operator is equivalent to the Solidity `/` operator.
