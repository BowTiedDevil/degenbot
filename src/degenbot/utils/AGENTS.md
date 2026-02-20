# Utils Agent Documentation

## Overview

Utility functions ported from Solidity libraries. Currently implements Solady compression utilities.

## Components

**Solady** (`solady/`)
- `libzip.py` - FastLZ compression/decompression algorithm
  - `flz_compress()` - Compress data for efficient calldata
  - `flz_decompress()` - Decompress FastLZ-encoded data

## Module Exports

Both `__init__.py` files are empty; import directly from `degenbot.utils.solady.libzip`.

## Constants

Five important constants defined in `libzip.py`:
- `MAX_LITERALS = 32`
- `MAX_MATCH_LENGTH = 262`
- `MAX_MATCH_OFFSET = 8191`
- `MAX_1BYTE_INT = 0xFF`
- `MAX_3BYTE_INT = 0xFFFFFF`

## Type Signatures

- `flz_compress(uncompressed_data: str | bytes)` - Returns `HexBytes`
- `flz_decompress(compressed_data: bytes | bytearray | HexStr)` - Returns `bytes`

## Error Handling

- `flz_decompress` raises `ValueError` on invalid instructions

## Algorithm Details

Three instruction types:
- Literal runs (instruction type 0)
- Short matches (types 1-6)
- Long matches (type 7)

Reference: https://github.com/Vectorized/solady/blob/main/js/solady.js

## Design Patterns

- Ported from reference Solidity implementations
- Integer constants match Solidity limits
- Supports both hex strings and bytes input
- Uses HexBytes for consistent output type
