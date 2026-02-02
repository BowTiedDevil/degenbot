# Aave Agent Documentation

## Overview

Aave V3 lending protocol implementation with multi-version library support for market management and position tracking.

## Components

**Core**
- `deployments.py` - AaveV3Deployment dataclass with pool addresses by chain

**Libraries** (`libraries/`)
- `v3_1/`, `v3_2/`, `v3_3/`, `v3_4/`, `v3_5/` - Protocol version-specific math and data structures

## Design Patterns

- Versioned libraries match Aave protocol upgrades
- Deployments use frozen dataclasses for immutability
- Scaled balance mechanics for interest-bearing positions
