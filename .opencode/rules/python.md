# Python Rules

## Docstrings
Minimal PEP 257. Additional detail follows a blank line if needed. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class EVMRevertError(DegenbotError):
    """Raised when a simulated EVM contract operation would revert."""
```

## Function Calls
- Use keyword arguments unless the function is written specifically as positional

## Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

## Logging
- Import and use `logger` from `src/degenbot/logging.py`

## Testing
- Add comment to complex tests describing "what" and "why"
- Create a test double (PascalCase with `Fake` prefix) instead of mocking
- Add reusable fixtures in `conftest.py`
- Add mark for chain-specific tests: `@pytest.mark.arbitrum`, `@pytest.mark.base`, `@pytest.mark.ethereum`

## Design Patterns
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- `@functools.lru_cache` for expensive functions
- `WeakSet` for subscriber references in pub/sub
- Use `dataclass(slots=True, frozen=True)` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known