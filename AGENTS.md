## Commands
- `uv run pytest tests/path/to/test_file.py::test_function_name` - Run single test
- `uv run pytest` - Run all tests with the options and plugins specified in `pyproject.toml`
- `uv run pytest --skip-fixture fixture_name` - Skip tests using specific fixture
- `uv run pytest -n0` - Run tests with single worker (required due to pytest-xdist default plugin)
- `uv run pytest -s` - Run tests with debug output from logger
- `ruff check` - Check Python code against linting rules
- `ruff format` - Format Python code
- `mypy` - Type check (strict for degenbot.* modules, relaxed for tests)
- `uv run maturin develop` - Build Rust extension (degenbot_rs module)

## Project Setup
The Python virtual environment contains an editable installation of the `degenbot` package. Any changes made in this project are instantly available without needing to make modifications to the virtual environment.

## Code Style

**Python Version**: Check `pyproject.toml` for `requires-python` key to determine the minimum supported Python version. Generate code using types, imports, and PEP features supported by the specified version.
Examples:
- Use `type Name = Type` aliases instead of `TypeVar`
- Use structural pattern matching (`match` and `case`)
- Use PEP 695 generic syntax: `class MyClass[T]: ...` not `class MyClass(Generic[T]): ...`

**Formatting**:
- 4-space indent, 100 char line length

**Docstrings**:
Minimalist PEP 257 with Google-style extensions when needed.

Simple/obvious functions:
```python
"""
Brief description in one line.
"""
```

Complex functions (when behavior needs clarification):
```python
"""
Brief description.

Extended explanation if function has unusual behavior or requires context.

Args:
    param1: Description only if not obvious from name/type
    param2: Description if has constraints or special handling

Returns:
    Description if return value needs clarification

Raises:
    ExceptionType: When and why
"""
```

Classes:
```python
"""
Brief description of the class.

Extended description if class has special usage patterns.
"""
```

Modules:
```python
"""
Brief description of module purpose.
"""
```

Principles:
- One-line default - most functions only need brief description
- Type hints replace param docs - only document if behavior is non-obvious
- Google-style section headers when extended documentation is needed
- Triple quotes required even for one-liners
- No reST/Sphinx tags - keep it plain text

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
- Import: `from degenbot.logging import logger`
- Use lazy formatting for performance: `logger.debug("Value: %s", value)` not `logger.debug(f"Value: {value}")`
- Levels: debug (detailed diagnostics), info (normal operation), warning (unexpected but recoverable), error (failure requiring attention)
- Don't log sensitive data (addresses, keys)

**Testing**:
- Fixtures in `tests/conftest.py` (use autouse for setup/teardown)
- Test functions: `test_<description>`
- Use `pytest.raises` for exception testing
- Parametrize with `@pytest.mark.parametrize`
- Comment complex tests with block-level descriptions
- Save tests in `tests/temp` directory, execute with `uv run pytest tests/temp/test_name.py`
- Fork-specific fixtures in `tests/conftest.py` execute blockchain calls against forked RPC (managed automatically, no cleanup needed)
- Coverage: Target 90% or higher for all production code
- Temp tests: Promote valuable tests to permanent location, delete others after verification
- Use fixtures over mocks when testing against forked RPC; mocks only for external services

**Solidity Porting**:
- Some Python modules are ported from Solidity contract libraries (look for "ref:" in docstrings)
- Solidity numbers are integers, so all division operations (e.g. `a / b`) represent floored integer division
- Python libraries ported from these libraries require explicit flooring division (e.g. `a // b`) to match Solidity behavior
- Reference URLs in docstrings: use format `ref: https://github.com/...` for single references, or `References:` section for multiple

**Validation**:
- Inputs and outputs to functions or methods may be validated by using Pydantic's `@validate_call` decorator
- Use types from `degenbot.validation.evm_values`: `ValidatedUint160`, `ValidatedUint256`, `ValidatedInt128`, `ValidatedInt256`, etc.

**Design Patterns**:
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- `@functools.lru_cache` for memoization of computationally expensive functions with a narrow range of inputs
- `WeakSet` for subscriber references to prevent memory leaks
- `dataclass` for passing value objects, prefer immutability with `frozen=True` argument to decorator

**Documentation**:
- All modules must have summaries in `docs/<category>/<module>.md` with YAML frontmatter
- Include: title, category, tags (canonical list only), related_files, complexity
- Canonical tags: arbitrage, optimization, convex, cvxpy, uniswap-v2/v3/v4, aerodrome, balancer, curve, solidly, pancakeswap, sushiswap, swapbased, rust, pyo3, state-management, liquidity
- Complexity: simple, standard, complex, architectural
- Inline comments: Use sparingly; code should be self-documenting through clear names and structure
- Avoid "obvious" comments like `# increment counter` - use descriptive variable names instead

**Database**:
- SQLAlchemy ORM with typed models
- Use `scoped_session` for thread safety
- Migrations via Alembic in `src/degenbot/migrations`

**Async**:
- Use `AsyncWeb3` from web3.py
- Use `async_connection_manager` for async connections
- Async tests are automatically executed with the `pytest-asyncio` plugin

## Rust Extension (degenbot_rs)
Built with PyO3 for performance-critical operations. Import as `import degenbot_rs`.

**Dependencies**: `pyo3` (v0.26), `alloy-primitives`, `num-bigint`, `rayon`

**Exposed Functions**:
- `get_sqrt_ratio_at_tick_alloy_translator(tick: int) -> BigUint` - Uniswap V3 tick to sqrt price
- `get_tick_at_sqrt_ratio_alloy_translator(sqrt_price_x96: BigUint) -> int` - Reverse conversion
- `to_checksum_address(address: str|bytes) -> str` - Single address checksumming
- `to_checksum_addresses_parallel(addresses: list[bytes]) -> list[str]` - Batch parallel
- `to_checksum_addresses_sequential(addresses: list[bytes]) -> list[str]` - Batch sequential

**Source**: `rust/src/lib.rs` with `#[pyfunction]` and `#[pymodule]` attributes
**Build**: `[tool.maturin]` in pyproject.toml with `bindings = "pyo3"`, `profile = "release"`

