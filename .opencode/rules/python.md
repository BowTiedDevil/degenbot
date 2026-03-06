# Python Design Rules

## Patterns 
- Use `Protocol` for interfaces (`Publisher`, `Subscriber`)
- Mixins for shared functionality (`PublisherMixin`)
- Use `dataclass(slots=True, frozen=True)` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known

## Docstrings
Minimal PEP 257. Type hints supersede parameter docs. No reST/Sphinx tags:
```python
class EVMRevertError(DegenbotError):
    """
    Raised when a simulated EVM contract operation would revert.
    
    [additional detail as needed]
    """
```

## Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

## Logging
- Use `degenbot.logger`

## Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

