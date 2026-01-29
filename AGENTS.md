This guide provides coding standards and conventions for agents working on the degenbot codebase.

## Commands
- `uv run pytest tests/path/to/test_file.py::test_function_name` - Run specific test
- `uv run pytest` - Run all tests with options/plugins from [pyproject.toml](pyproject.toml)
- `uv run pytest -n0` - Run tests with single worker (pytest-xdist)
- `uv run pytest -m base` - Run base tests (tests requiring no external resources)
- `ruff check` - Lint Python code
- `ruff format` - Format Python code
- `mypy` - Type check with strict mode enabled
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)

## Project Setup
Virtual environment in [.venv](.venv/) contains an editable installation of the `degenbot` package

**Python Version**: 3.12 or newer

## Code Style

**Lint**: Resolve all lint/type checking warnings. Avoid `# noqa`/`# type: ignore` tags except when necessary.

**Docstrings**: Minimal PEP 257, three-line with blank line after closing triple quote. Extend docstring with Args/Returns/Raises only for non-obvious behavior. Type hints replace parameter docs. No reST/Sphinx tags:
```python
class EVMRevertError(DegenbotError):
    """
    Raised when a simulated EVM contract operation would revert.
    """
```

**Naming**:
- Variables: snake_case; prefer long names
- Constants: UPPER_SNAKE_CASE
- Concrete Classes: PascalCase
- Abstract Classes: PascalCase with "Abstract" prefix

**Type Hints**: 
- Required everywhere
- Avoid `Any`
- Avoid `Optional[T]`, use `T | None`
- Use PEP 695 type aliases for complex types:
    ```python
        type BlockNumber = int
        type ChainId = int
        type Tick = int
        type Word = int
    ```

**Functions**: Prefer calling with keyword arguments. No boolean mode flags, prefer separate simple functions with a single control flow.

**Error Handling**:
- All exceptions inherit from [`DegenbotError`](src/degenbot/exceptions/base.py):
    ```python
    class ArbitrageError(DegenbotError):
        """
        Exception raised inside arbitrage helpers.
        """
    ```
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

**Logging**: Use module-level logger from [`src/degenbot/logging.py`](src/degenbot/logging.py)

**Testing**:
- Create a test double (PascalCase with `Fake` prefix) instead of mocking
- Comment complex tests explaining the behavior being tested
- Fixtures in [`conftest.py`](tests/conftest.py) for shared test resources
- Autouse fixture resets global state between tests
- Target 90%+ coverage
- Use `@pytest.mark.arbitrum`, `@pytest.mark.base`, or `@pytest.mark.ethereum` markers for RPC-dependent tests:
    ```python
    class FakeSubscriber:
        """
        This subscriber class provides a record of received messages, and can be used to test that
        publisher/subscriber methods operate as expected.
        """

        def __init__(self) -> None:
            self.inbox: list[dict[str, Any]] = []
    ```

**Solidity Porting**: Modules ported from Solidity contracts require integer division. Python `//` operator equals Solidity `/` operator.

**Code Organization**:
- Group related functionality in packages by domain (`uniswap`, `curve`, `aave`)
- Keep library functions separate from stateful classes
- Use type alias modules (`types.py`, `v2_types.py`, `v3_types.py`) for shared types
- Place specific business logic in domain packages, cross-cutting concerns in core (`src/degenbot/`)

**Design Patterns**:
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- `@functools.lru_cache` for memoization of expensive functions with narrow input ranges
- `WeakSet` for subscriber references to prevent memory leaks in pub/sub
- Use `dataclass` for value objects passed between functions; Use `slots=True` to reduce memory use, and `frozen=True` if value object should be immutable:
    ```python
    @dataclass(slots=True, frozen=True)
    class PathStep:
        address: ChecksumAddress
        type: type[LiquidityPoolTable | UniswapV4PoolTable]
        hash: str | None = None
    ```
- Prefer `TypedDict` if key and value types are known

**Documentation**:
- Module docs in `docs/<category>/<module>.md` with YAML frontmatter (title, category, tags, related_files, complexity)
- Complexity levels: simple, standard, complex, architectural
- Use mermaid diagrams, tables, code blocks with language specifiers
- Structure: Overview, Background, [Main Sections], Key Concepts, See Also
- Update docs on behavior changes, keep related_files current
- **Add a link to a file or directory**: 
    ```markdown
    [filename](path/to/file)` or `[dirname](path/to/dir/)
    ```
- **Do not add line numbers to code**

**Database**:
- SQLAlchemy ORM with typed models in [`src/degenbot/database/models/`](src/degenbot/database/models/)
- Use `scoped_session` for thread safety
- Migrations via Alembic in [`src/degenbot/migrations/`](src/degenbot/migrations/)

**Type Checking**:
- Mypy strict mode enabled for main code, some rules are disabled for tests

## ExecPlans
When writing complex features or significant refactors, use an ExecPlan as described in [`PLANS.md`](PLANS.md) from design through implementation.