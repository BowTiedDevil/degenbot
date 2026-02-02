# Validation Agent Documentation

## Overview

EVM-compatible value validation using Pydantic. Ensures integers fit within Solidity type bounds.

## Components

**EVM Values**
- `evm_values.py` - Pydantic-validated types for EVM integers
  - `ValidatedInt16/24/128/256` - Signed integers with bounds
  - `ValidatedUint8/24/128/160/256` - Unsigned integers with bounds
  - NonZero variants for values that must be greater than zero

## Design Patterns

- Pydantic Annotated types with Field constraints
- Bounds match Solidity MIN/MAX constants
- Type aliases enable static checking of EVM constraints
- Zero/non-zero variants for common requirements (addresses, amounts)
