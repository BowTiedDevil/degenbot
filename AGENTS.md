# AGENTS.md

## Refactoring & Feature Development

Use Red/Green TDD while refactoring and implementing new features.

## Commands

Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests (includes `compile-test-contracts` via Forge)
- `just test-python-cov` - Run Python tests with coverage
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes — just run the tests and the import will trigger a rebuild.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run clippy, ruff, and mypy
- `just format` - Run `cargo fmt` and `ruff format`

## Database
- Config file at `~/.config/degenbot/config.toml`; database path defaults to `~/.config/degenbot/degenbot.db` (overridable via `database.path` setting)
- SQLAlchemy ORM models in `src/degenbot/database/models/`
- Use the scoped session context manager: `with db_session() as session:` from `degenbot.database.db_session`

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
```

### Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

### Logging
- Use `from degenbot.logging import logger`

### Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

## Refactoring

- Unless directed otherwise, design standalone features without a backwards compatibility layer. Use a feature flag during development and testing to enable hard cutover. Feature flags are typically implemented as a module-level boolean constant (e.g. `USE_NEW_PATH = bool(os.environ.get("DEGENBOT_NEW_FEATURE", ""))`) that gates the new code path.

## Solidity

### Arithmetic wrapping behavior
- Solidity arithmetic silently wraps for versions < 0.8.0, e.g. `uint8(255) + 1 == 0`
- Solidity arithmetic is checked by default for versions 0.8.0+, e.g. `uint8(255) + 1` will revert

### Porting to Python
- Solidity contracts requires explicit integer division to match the EVM. The Python `//` operator is equivalent to the Solidity `/` operator.
