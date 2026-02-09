Coding standards and conventions for the degenbot codebase.

For documentation read by humans and agents, see `docs/AGENTS.md`.

## Commands
- `uv run pytest tests/...::test_name` - Run specific test
- `uv run pytest` - Run all tests
- `uv run pytest -n0` - Run without parallelization
- `uv run ruff check` - Lint Python code
- `uv run ruff format` - Format Python code
- `uv run mypy` - Type check
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)

## Type Hinting, Checks
- Never use `Any` type
- Never use `# noqa` / `# type: ignore` tags
- Resolve lint and type check warnings before committing

## Architecture
- Domain packages: `src/degenbot/uniswap/`, `src/degenbot/curve/`, `src/degenbot/aave/`
- Core utilities: `src/degenbot/exceptions/`, `src/degenbot/logging.py`
- Type aliases: Domain-specific `types.py` files
- Database: `src/degenbot/database/models/`, `src/degenbot/migrations/`

When working with code from a specific module, check for an `AGENTS.md` file in that module's directory for module-specific conventions.

## Project Setup
Virtual environment in `.venv/` contains an editable installation of the `degenbot` package

## Docstrings
Minimal PEP 257. Additional detail follows a blank line if needed. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class EVMRevertError(DegenbotError):
    """Raised when a simulated EVM contract operation would revert."""
```

## Functions
- Prefer calling with keyword arguments

## Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

## Logging
Import and use `logger` from `src/degenbot/logging.py`

## Testing
- Add comment to complex tests describing "what" and "why"
- Create a test double (PascalCase with `Fake` prefix) instead of mocking
- Add reusable fixtures in `conftest.py`
- Add mark for chain-specific tests: `@pytest.mark.arbitrum`, `@pytest.mark.base`, `@pytest.mark.ethereum` 

## Solidity
Modules ported from Solidity contracts require integer division. Python `//` operator equals Solidity `/` operator.

## Code Organization
- Group related functionality in packages by domain (`uniswap`, `curve`, `aave`)
- Keep library functions separate from stateful classes
- Use type alias modules (`types.py`, `v2_types.py`, `v3_types.py`) for shared types
- Place specific logic in domain packages, cross-cutting concerns in core (`src/degenbot/`)

## Design Patterns
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- `@functools.lru_cache` for expensive functions
- `WeakSet` for subscriber references in pub/sub
- Use `dataclass(slots=True, frozen=True)` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known

## Database
- SQLAlchemy ORM with typed models in `src/degenbot/database/models/`
- Use `scoped_session` for thread safety
- Migrations via Alembic in `src/degenbot/migrations/`

## Editing This File
AGENTS.md is for AI agents only. When editing it:
- No styling - omit colors, visual formatting
- No metadata - omit frontmatter
- Plain paths from the project root: `path/to/file`
- Minimal examples - only for non-obvious rules
- Give concise instruction
- When writing AGENTS.md files, list items in order of importance, descending
