# Plan 024: Extract Generic Address Registry Base Class

**Status: IMPLEMENTED**

## Overview

Extract the common key-handling and deduplication logic from `PoolRegistry`, `TokenRegistry`, and `ManagedPoolRegistry` into PEP-695 parameterized base classes. Eliminate code duplication and enable new registries without re-implementing the pattern.

## Design Decision: PEP-695 Generics

Python 3.12's PEP-695 syntax is used throughout: `class AddressRegistry[T]` instead of the pre-3.12 `TypeVar` + `Generic[...]` boilerplate. This is a deliberate choice because:

- The project requires Python ≥ 3.12 (per `pyproject.toml`)
- PEP-695 reduces syntactic noise substantially
- `class Foo[T](Base[T]):` is cleaner than `T = TypeVar("T"); class Foo(Base[T], Generic[T]):`
- `_storage` is a regular method (not `@property`) so mypy tracks mutable dict access correctly through the generic parameter

## Files Involved

**Existing:**

- `src/degenbot/registry/pool.py` (~125 lines) — Contains `PoolRegistry`, `ManagedPoolRegistry` classes
- `src/degenbot/registry/token.py` (~40 lines) — Contains `TokenRegistry` class
- `src/degenbot/registry/__init__.py` — Exports all registry classes

**New:**

- `src/degenbot/registry/base.py` — New module with `AbstractAddressRegistry[T]`, `AddressRegistry[T]`, `MultiKeyAddressRegistry[T]` base classes
- `tests/registry/test_address_registry.py` — Unit tests for base classes

**Modified:**

- `src/degenbot/registry/pool.py` — Refactor to inherit from `AddressRegistry` / `MultiKeyAddressRegistry`
- `src/degenbot/registry/token.py` — Refactor to inherit from `AddressRegistry`
- `src/degenbot/registry/__init__.py` — Add exports for new base classes
- `src/degenbot/registry/CONTEXT.md` — Updated term table and relationships
- `tests/registry/test_pool_registry.py` — Update tests for inheritance changes
- `tests/registry/test_token_registry.py` — Update tests for inheritance changes

## Problem

The `PoolRegistry`, `TokenRegistry`, and `ManagedPoolRegistry` classes implement the same pattern with slight variations. All three independently implement:

1. **Address checksumming** — `get_checksum_address()` on every add/get
2. **Tuple key construction** — `(chain_id, address)` or `(chain_id, manager_addr, pool_id)`
3. **Deduplication check** — raise if exists on add
4. **Key-based store lookup** — `dict.get(key)` pattern
5. **Removal with default** — `.pop(key, None)`

## Solution

Extract a PEP-695 generic `AddressRegistry[T]` base parameterized by value type. Use `MultiKeyAddressRegistry[T]` for multi-field keys (V4 pools).

### Core Base Registry (PEP-695)

```python
# src/degenbot/registry/base.py

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any, Protocol

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.types.abstract import AbstractRegistry

if TYPE_CHECKING:
    from collections.abc import Iterable
    from eth_typing import ChecksumAddress


class AddressFunction(Protocol):
    def __call__(self, address: str | bytes) -> ChecksumAddress: ...


class AbstractAddressRegistry[T](AbstractRegistry, ABC):
    """Abstract PEP-695 generic base for address-based registries."""

    def __init__(
        self,
        *,
        checksum_fn: AddressFunction = get_checksum_address,
        on_duplicate: str = "error",
        name: str = "AbstractAddressRegistry",
    ) -> None:
        if on_duplicate not in {"error", "ignore", "replace"}:
            raise ValueError(...)
        self._checksum_fn = checksum_fn
        self._on_duplicate = on_duplicate
        self._name = name

    @abstractmethod
    def _build_key(self, chain_id: int, **address_args: str | bytes) -> tuple[Any, ...]: ...

    @abstractmethod
    def _storage(self) -> dict[Any, T]: ...

    def get(self, chain_id: int, **address_args: str | bytes) -> T | None:
        key = self._build_key(chain_id, **address_args)
        return self._storage().get(key)

    def add(self, item: T, chain_id: int, **address_args: str | bytes) -> None:
        key = self._build_key(chain_id, **address_args)
        if key in self._storage():
            if self._on_duplicate == "error":
                raise DegenbotValueError(
                    message=f"{self._name} is already registered at key {key}"
                )
            if self._on_duplicate == "ignore":
                return
        self._storage()[key] = item

    def remove(self, chain_id: int, **address_args: str | bytes) -> T | None:
        key = self._build_key(chain_id, **address_args)
        return self._storage().pop(key, None)

    def list_all(self) -> Iterable[T]:
        yield from self._storage().values()

    def __len__(self) -> int:
        return len(self._storage())

    def reset(self) -> None:
        self._storage().clear()


class AddressRegistry[T](AbstractAddressRegistry[T]):
    """Single-address key: (chain_id, checksummed_address)."""

    def __init__(self, *, name: str = "AddressRegistry", ...) -> None:
        super().__init__(...)
        self._items: dict[tuple[int, ChecksumAddress], T] = {}

    def _build_key(self, chain_id: int, address: str | bytes = "", ...) -> tuple[int, ChecksumAddress]:
        first_address = next(iter(address_args.values()), address)
        if not first_address:
            raise ValueError("No address provided")
        return (chain_id, self._checksum_fn(first_address))

    def _storage(self) -> dict[tuple[int, ChecksumAddress], T]:
        return self._items


class MultiKeyAddressRegistry[T](AbstractAddressRegistry[T]):
    """Multi-field key: (chain_id, checksummed_field_1, ...).

    Configure key structure via the `address_fields` parameter.
    The special field name `"pool_id"` is kept as HexBytes (not checksummed).
    """

    def __init__(
        self,
        *,
        address_fields: tuple[str, ...],
        name: str = "MultiKeyAddressRegistry",
        ...
    ) -> None:
        super().__init__(...)
        self._address_fields = address_fields
        self._items: dict[tuple[Any, ...], T] = {}

    def _build_key(self, chain_id: int, **address_args: str | bytes) -> tuple[Any, ...]:
        key_parts: list[Any] = [chain_id]
        for field in self._address_fields:
            value = address_args[field]
            if field == "pool_id":
                key_parts.append(HexBytes(value))
            else:
                key_parts.append(self._checksum_fn(value))
        return tuple(key_parts)

    def _storage(self) -> dict[tuple[Any, ...], T]:
        return self._items
```

### Refactored Concrete Registries

```python
# src/degenbot/registry/token.py

from degenbot.registry.base import AddressRegistry


class TokenRegistry(AddressRegistry["Erc20Token"]):
    def __init__(self) -> None:
        super().__init__(name="Token")

    def get(self, token_address: str, chain_id: int) -> "Erc20Token | None":
        return super().get(chain_id=chain_id, address=token_address)

    def add(self, token_address: str, chain_id: int, token: "Erc20Token") -> None:
        super().add(item=token, chain_id=chain_id, address=token_address)

    def remove(self, token_address: str, chain_id: int) -> None:
        super().remove(chain_id=chain_id, address=token_address)
```

```python
# src/degenbot/registry/pool.py

from degenbot.registry.base import AddressRegistry, MultiKeyAddressRegistry


class ManagedPoolRegistry(MultiKeyAddressRegistry["AbstractLiquidityPool"]):
    def __init__(self) -> None:
        super().__init__(
            address_fields=("pool_manager_address", "pool_id"),
            name="ManagedPool",
        )

    # get/add/remove delegate to super with named params


class PoolRegistry(AddressRegistry["AbstractLiquidityPool"]):
    def __init__(self, managed_pool_registry: ManagedPoolRegistry | None = None) -> None:
        super().__init__(name="Pool")
        self._managed_pool_registry = managed_pool_registry or ManagedPoolRegistry()

    # get/add/remove delegate to base or managed_pool_registry for V4 pool_id
```

### Creating a New Registry

```python
from degenbot.registry.base import AddressRegistry


class ContractRegistry(AddressRegistry[ContractRecord]):
    def __init__(self) -> None:
        super().__init__(name="Contract")

    def add(self, contract: ContractRecord, chain_id: int) -> None:
        super().add(item=contract, chain_id=chain_id, address=contract.address)

    def get(self, chain_id: int, contract_address: str) -> ContractRecord | None:
        return super().get(chain_id=chain_id, address=contract_address)

    def remove(self, chain_id: int, contract_address: str) -> ContractRecord | None:
        return super().remove(chain_id=chain_id, address=contract_address)
```

## Implementation Notes

### `_storage` Is a Method, Not a Property

The base class declares `_storage(self) -> dict[Any, T]` as an abstract method rather than an abstract property. This is a PEP-695 / mypy interaction: when `_storage` is a `@property`, mypy loses track of the mutable dict through the generic parameter `T`, which causes:
- `self._storage.get(key)` to return `Any` instead of `T | None`
- `self._storage[key] = item` to lose type safety

Calling it as `self._storage().get(key)` preserves full generic type inference.

### Override Signatures

Concrete registries use `# type: ignore[override]` on `get`/`add`/`remove` because they narrow the base class's `**address_args: str | bytes` to named parameters (e.g. `pool_address: str`). This is intentional — the signatures are stable public API that predates the generic base.

```python
def get(  # type: ignore[override]
    self,
    chain_id: int,
    pool_address: str,
    pool_id: str | None = None,
) -> AbstractLiquidityPool | None: ...
```

### `TypeVar("T")` Is Not Used

No `TypeVar` declarations appear in the codebase for these classes. The PEP-695 bracket syntax `class Foo[T]` creates the type parameter implicitly.

## What Stays the Same

- Public API of `TokenRegistry`, `PoolRegistry`, `ManagedPoolRegistry` — unchanged
- Type signatures and method names — unchanged
- Deduplication behavior (raises error on duplicate) — unchanged
- Checksumming behavior (via `get_checksum_address`) — unchanged
- Integration with `Bot` class — unchanged
- All existing tests — behavior unchanged

## What Changes

| Before | After |
|--------|-------|
| Three independent implementations of same pattern | PEP-695 generic base (`AbstractAddressRegistry[T]`) with two concrete subclasses |
| `PoolRegistry.__init__` handles storage init | Base class handles storage; `PoolRegistry.__init__` just sets composition |
| `get_checksum_address` called explicitly in each method | Base class checksums automatically in `_build_key` |
| Deduplication check duplicated | Base class checks based on `on_duplicate` parameter |
| `ManagedPoolRegistry` stores key manually | `MultiKeyAddressRegistry` builds key from `address_fields` tuple |
| Adding new registry requires copying pattern | Inherit from `AddressRegistry[T]` or `MultiKeyAddressRegistry[T]` |
| Pre-3.12 `TypeVar` + `Generic` boilerplate | PEP-695 `class Foo[T]` syntax |

## Metrics

| Metric | Before | After |
|--------|-------|-------|
| `registry/pool.py` lines | ~125 | ~80 |
| `registry/token.py` lines | ~40 | ~20 |
| `registry/base.py` lines (new) | 0 | ~220 |
| Deduplication implementations | 3 copies | 1 copy in base |
| Checksumming implementations | 3 copies | 1 call in base |
| TypeVar declarations | 1 (`T = TypeVar("T")`) | 0 (PEP-695 implicit) |

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Breaking changes to public API | Public methods maintain same signatures and behavior; internal implementation delegates to base |
| `ManagedPoolRegistry` inheritance order changes | Only inherits from base, not multiple classes; maintained V4-specific test coverage |
| `pool_id` HexBytes handling differs | Tested multi-key registry with various pool_id values (bytes, hex string); HexBytes conversion verified |
| `on_duplicate` parameter misuse | Documented clearly in docstrings; defaults to `"error"` |
| Generic types confuse IDEs | Uses PEP-695 bracket syntax; type hints throughout; `# type: ignore[override]` on narrowing overrides |
| Performance regression from abstraction | Base class adds a thin method-call layer; dict operations remain O(1) |
| Test failures from internal structure changes | All 365 existing tests pass unchanged |

## Definition of Done

- [x] `src/degenbot/registry/base.py` created with `AbstractAddressRegistry[T]`, `AddressRegistry[T]`, `MultiKeyAddressRegistry[T]`
- [x] PEP-695 generic syntax used throughout (no `TypeVar` declarations)
- [x] `TokenRegistry` migrated to inherit from `AddressRegistry[Erc20Token]`
- [x] `PoolRegistry` migrated to inherit from `AddressRegistry[AbstractLiquidityPool]`
- [x] `ManagedPoolRegistry` migrated to inherit from `MultiKeyAddressRegistry[AbstractLiquidityPool]`
- [x] All existing tests pass (365 passed, 1 skipped)
- [x] `just lint` passes (mypy + ruff clean)
- [x] Public API unchanged — no breaking changes
- [x] `src/degenbot/registry/CONTEXT.md` updated with new terms and relationships
- [x] `src/degenbot/registry/__init__.py` updated with new exports
