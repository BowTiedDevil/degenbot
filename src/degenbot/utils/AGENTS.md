# Utils Agent Documentation

## Overview

Utility functions ported from Solidity libraries. Currently implements Solady compression utilities.

## Components

**Solady** (`solady/`)
- `libzip.py` - FastLZ compression/decompression algorithm
  - `flz_compress()` - Compress data for efficient calldata
  - `flz_decompress()` - Decompress FastLZ-encoded data

## Design Patterns

- Ported from reference Solidity implementations
- Integer constants match Solidity limits
- Supports both hex strings and bytes input
- Uses HexBytes for consistent output type
