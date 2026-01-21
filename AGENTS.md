## Commands
- `uv run pytest tests/path/to/test_file.py::test_function_name` - Run single test
- `uv run pytest` - Run all tests with the options and plugins specified in [pyproject.toml](pyproject.toml)
- `uv run pytest --skip-fixture fixture_name` - Skip tests using specific fixture
- `uv run pytest -n0` - Run tests with single worker (required due to pytest-xdist default plugin)
- `uv run pytest -s` - Run tests with stdout captured
- `ruff check` - Check Python code against linting rules
- `ruff format` - Format Python code
- `mypy` - Type check (strict for degenbot.* modules, relaxed for tests)
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)

## Project Setup
The Python virtual environment in **[.venv](./.venv/)** contains an editable installation of the `degenbot` package. Any changes made in this project are reflected in the virtual environment without needing to re-install the package.

## Code Style

**Python Version**: Check `pyproject.toml` for `requires-python` key to determine the minimum supported Python version. Generate code using types, imports, and PEP features supported by the specified version.
Examples:
- Use `type Name = Type` aliases instead of `TypeVar`
- Use structural pattern matching (`match` and `case`)
- Use PEP 695 generic syntax: `class MyClass[T]: ...` not `class MyClass(Generic[T]): ...`

**Formatting**:
- 4-space indent, 100 char line length

**Docstrings**:
Minimalist PEP 257. One-line default. Extended docs with Args/Returns/Raises sections only when behavior is non-obvious. Type hints replace parameter docs. No reST/Sphinx tags.

**Imports**:
- Order: stdlib → third-party → local (degenbot imports)
- If a particular import needs to be performed before others, isolate it with an `# isort: split` block
- Use `if TYPE_CHECKING: import some_module` for imports used only for type hinting
- Do not use wildcard imports (`from module import *`)

**Naming**:
- Classes: PascalCase with `Abstract` prefix for abstract classes (`AbstractErc20Token`)
- Functions/variables: snake_case
- Constants: UPPER_CASE
- Private members: leading underscore (`_private_method`, `_some_attribute`)
- Protocols: descriptive noun describing a role or behavior (`Publisher`, `Subscriber`)
- Test doubles: PascalCase with `Fake` prefix for objects used only for testing (`FakeErc20Token`)

**Type Hints**:
- Required on all functions, methods, and class attributes
- Use `int | None` not `Optional[int]`
- Use built-in Python containers: `list[T]` not `typing.List[T]`
- Overload with `@overload` for functions with multiple signatures
- Use `Self` return type for methods returning self
- Type variables: PEP 695 syntax `class MyClass[T]`

**Error Handling**:
- All exceptions inherit from `DegenbotError`
- Use specific subtypes: `DegenbotValueError`, `DegenbotTypeError`, `ExternalServiceError`
- Include `message: str | None = None` attribute on custom exceptions
- Use exception chaining to preserve tracebacks: `raise NewError(...) from original_error`
- Create new custom exception types when the error represents a distinct failure category with specific handling needs
- Don't use bare `except:`
- Catch specific exceptions, avoid catching `Exception` broadly

**Logging**:
- Import: `from degenbot.logging import logger` from [`src/degenbot/logging.py`](src/degenbot/logging.py)
- Use lazy formatting for performance: `logger.debug("Value: %s", value)` not `logger.debug(f"Value: {value}")`
- Levels: debug (detailed diagnostics), info (normal operation), warning (unexpected but recoverable), error (failure requiring attention)
- Don't log sensitive data (addresses, keys)

**Testing**:
- Fixtures in [`tests/conftest.py`](tests/conftest.py). Fork fixtures execute against forked RPC with automatic cleanup.
- Test functions: `test_<description>`. Use `pytest.raises` for exception testing. Parametrize with `@pytest.mark.parametrize`.
- Comment complex tests with block-level descriptions.
- Use fixtures over mocks when testing against forked RPC; mocks only for external services.
- Target 90%+ coverage for production code.

**Solidity Porting**:
- Modules ported from Solidity (look for "ref:" in docstrings) use integer division. Use `//` (flooring) to match Solidity behavior.
- Reference format: `ref: https://github.com/...` for single references, or `References:` section for multiple.

**Validation**:
- Inputs and outputs to functions or methods may be validated by using Pydantic's `@validate_call` decorator
- Use types from `degenbot.validation.evm_values`: `ValidatedUint160`, `ValidatedUint256`, `ValidatedInt128`, `ValidatedInt256`, etc. from [`src/degenbot/validation/evm_values.py`](src/degenbot/validation/evm_values.py)

**Design Patterns**:
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- Consider `@functools.lru_cache` for memoization of computationally expensive functions with a narrow range of inputs
- For passing value objects to functions and methods, prefer encapsulating in immutable `dataclass` using `frozen=True`

**Documentation**:
- Module docs in `docs/<category>/<module>.md` with YAML frontmatter for user-facing features, complex systems, and public APIs
- Include: title, category, tags, related_files, complexity
- Categories: arbitrage, cli (expand as needed)
- Canonical tags: arbitrage, optimization, convex, cvxpy, uniswap-v2/v3/v4, aerodrome, balancer, curve, solidly, pancakeswap, sushiswap, swapbased, rust, pyo3, state-management, liquidity, cli, aave, chainlink
- Complexity: simple (self-contained), standard (multiple components), complex (advanced/optimized), architectural (system-wide patterns)
- Use mermaid diagrams for data flows, tables for comparisons, code blocks with language specifiers
- Structure: Overview, Background, [Main Sections], Key Concepts, See Also
- Update docs on significant behavior changes, keep related_files current
- **Always create a link for any file or directory path**: Use Markdown link syntax `[filename](path/to/file)` or `[dirname](path/to/dir/)` to make all paths clickable
- Reference: [docs/arbitrage/uniswap_multipool_cycle_testing.md](docs/arbitrage/uniswap_multipool_cycle_testing.md)

Inline comments: Use sparingly; code should be self-documenting through clear names and structure. Avoid "obvious" comments like `# increment counter`.

**Database**:
- SQLAlchemy ORM with typed models
- Use `scoped_session` for thread safety
- Migrations via Alembic in [`src/degenbot/migrations`](src/degenbot/migrations)

**Async**:
- Use `AsyncWeb3` from web3.py
- Use `async_connection_manager` for async connections from [`src/degenbot/connection/async_connection_manager.py`](src/degenbot/connection/async_connection_manager.py)
- Async tests are automatically executed with the `pytest-asyncio` plugin

## Rust Extension (degenbot_rs)
Built with PyO3 for performance-critical operations. Import as `import degenbot_rs`.

**Dependencies**: `pyo3`, `alloy-primitives`, `num-bigint`, `rayon`

**Exposed Functions**: Uniswap V3 tick/sqrt price conversions, checksum address validation (single, parallel, sequential).

**Source**: [`rust/src/lib.rs`](rust/src/lib.rs) with `#[pyfunction]` and `#[pymodule]` attributes

