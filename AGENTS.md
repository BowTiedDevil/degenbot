Coding standards and conventions for the degenbot codebase.

For documentation read by humans and agents, see `docs/AGENTS.md`.

## Commands
- `uv run pytest tests/...::test_name` - Run specific test
- `uv run pytest` - Run all tests
- `uv run pytest -n0` - Run without parallelization
- `ruff check` - Lint Python code
- `ruff format` - Format Python code
- `mypy` - Type check with strict mode enabled
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)

## Lint & Types
- Avoid `# noqa` / `# type: ignore` tags
- Resolve `ruff check` warnings
- Resolve `mypy` warnings

## Type Hints
Required for all function parameters, return values, and class attributes. Avoid `Any`, use `T | None` instead of `Optional[T]`. Use PEP 695 aliases where the base type is vague:
```python
type BlockNumber = int
type ChainId = int
```

## Naming
- Variables: snake_case; prefer long names
- Constants: UPPER_SNAKE_CASE
- Concrete Classes: PascalCase
- Abstract Classes: PascalCase with "Abstract" prefix

## Architecture
- Domain packages: `src/degenbot/uniswap/`, `src/degenbot/curve/`, `src/degenbot/aave/`
- Core utilities: `src/degenbot/exceptions/`, `src/degenbot/logging.py`
- Type aliases: Domain-specific `types.py` files
- Database: `src/degenbot/database/models/`, `src/degenbot/migrations/`

When working with code from a specific module, check for an `AGENTS.md` file in that module's directory for module-specific conventions.

## Project Setup
Virtual environment in `.venv/` contains an editable installation of the `degenbot` package

Python Version: 3.12 or newer

## Docstrings
Minimal PEP 257. Additional detail follows a blank line if needed. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class EVMRevertError(DegenbotError):
    """Raised when a simulated EVM contract operation would revert."""
```

## Functions
- Prefer calling with keyword arguments
- No boolean mode flags, instead create two simple functions

## Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

## Logging
Use module-level logger from `src/degenbot/logging.py`

## Testing
- Create a test double (PascalCase with `Fake` prefix) instead of mocking
- Comment complex tests
- Fixtures in `tests/conftest.py`
- Autouse fixture resets global state
- Target 90%+ coverage
- Use `@pytest.mark.arbitrum`, `@pytest.mark.base`, or `@pytest.mark.ethereum` for chain-specific tests

## Solidity
Modules ported from Solidity contracts require integer division. Python `//` operator equals Solidity `/` operator.

## Code Organization
- Group related functionality in packages by domain (`uniswap`, `curve`, `aave`)
- Keep library functions separate from stateful classes
- Use type alias modules (`types.py`, `v2_types.py`, `v3_types.py`) for shared types
- Place specific business logic in domain packages, cross-cutting concerns in core (`src/degenbot/`)

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

## Documentation Path References
- Files in `docs/<category>/` → use `../../src/degenbot/<module>/file.py`
- Files in `docs/` root → use `../src/degenbot/<module>/file.py`
- Cross-reference docs → use `../category/file.md`
- External URLs → use full `https://`
- Validate: `uv run python scripts/validate_doc_links.py`

## Editing This File
AGENTS.md is for AI agents only. When editing it:

- No Markdown links - Use plain paths: `src/degenbot/logging.py`
- No TOC or index - Read sequentially
- Minimal examples - Only for non-obvious rules
- No styling - Omit colors and visual formatting
- Concise - Give clear direction
- When writing AGENTS.md files, list items in order of importance, descending
