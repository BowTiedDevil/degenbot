# Validation Agent Documentation

## Overview

EVM-compatible value validation using Pydantic. Ensures integers fit within Solidity type bounds.

## Components

**EVM Values** (`evm_values.py`)
- `ValidatedInt16`, `ValidatedInt24`, `ValidatedInt128`, `ValidatedInt256` - Signed integers with bounds
- `ValidatedUint8`, `ValidatedUint24`, `ValidatedUint128`, `ValidatedUint160`, `ValidatedUint256` - Unsigned integers with bounds
- `ValidatedUint128NonZero`, `ValidatedUint160NonZero`, `ValidatedUint256NonZero` - Non-zero unsigned variants

## Module Exports

`__init__.py` is currently empty; import directly from `evm_values.py`.

## Design Patterns

- Pydantic Annotated types with Field constraints
- Bounds match Solidity MIN/MAX constants
- PEP 695 `type` syntax for type aliases (Python 3.12+)
- NonZero variants use `gt=MIN` for strict inequality
- Constants imported from `degenbot.constants`
- Type aliases enable static checking of EVM constraints
