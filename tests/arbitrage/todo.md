# Synthetic Pool State Generator & Arbitrage Testing Framework

## Overview

Build a testing infrastructure that generates synthetic pool states with known profitable arbitrage conditions, enabling regression testing and performance benchmarking of arbitrage optimization algorithms.

---

## Phase 1: Core Generator

### 1.1 Generator Types (`generator/types.py`)

- [x] Create `PoolGenerationConfig` dataclass for V2 pools
- [x] Create `V3PoolGenerationConfig` dataclass (extends base, adds tick params)
- [x] Create `V4PoolGenerationConfig` dataclass (V3 + pool_id)
- [x] Create `ArbitrageFixtureConfig` dataclass for scenario configuration
- [x] Create `PriceDiscrepancyConfig` for controlling profit injection

### 1.2 Pool State Generator (`generator/pool_generator.py`)

- [x] Create `PoolStateGenerator` class
- [x] Implement `generate_v2_pool_state()` - basic V2 state creation
- [x] Implement `generate_v3_pool_state()` - V3 state with tick bitmap
- [x] Implement `generate_v4_pool_state()` - V4 state with pool_id
- [x] Implement `generate_tick_bitmap_for_price_range()` - minimal tick population
- [x] Implement `inject_price_discrepancy()` - create arbitrage opportunity between pools

### 1.3 Profitable Pair Generation

- [x] Implement `generate_profitable_v2_pair()` - two V2 pools with price diff
- [x] Implement `generate_profitable_v3_pair()` - two V3 pools with price diff
- [x] Implement `generate_profitable_v4_pair()` - two V4 pools with price diff
- [x] Implement `generate_profitable_mixed_pair()` - V2 vs V3 arbitrage
- [x] Add profit validation: verify generated state is actually profitable

### 1.4 Unit Tests for Generator

- [x] Test V2 pool state generation
- [x] Test V3 pool state generation with tick bitmap
- [x] Test V4 pool state generation
- [x] Test profit injection creates valid arbitrage opportunity
- [x] Test determinism: same seed produces same output

---

## Phase 2: Fixtures

### 2.1 Fixture Definition (`generator/fixtures.py`)

- [x] Create `ArbitrageCycleFixture` frozen dataclass
- [x] Implement `to_json()` serialization
- [x] Implement `from_json()` deserialization
- [x] Implement `save()` and `load()` for file I/O
- [x] Add `validate()` method to verify fixture integrity

### 2.2 Fixture Factory

- [x] Create `FixtureFactory` class

#### Simple Cases (hand-crafted, exact values)

- [x] `simple_v2_arb_profitable()` - basic V2-V2 arbitrage
- [x] `simple_v2_arb_cross_fee()` - V2 pools with different fees (0.05% vs 0.3%)
- [x] `simple_v3_arb_same_tick_spacing()` - V3 pools same fee tier, different prices
- [x] `simple_v3_arb_cross_fee_tier()` - V3 pools at different fee tiers
- [x] `simple_mixed_v2_v3()` - V2 vs V3 arbitrage
- [x] `simple_v4_arb()` - basic V4-V4 arbitrage
- [x] `simple_v4_vs_v3()` - V3 vs V4 arbitrage

#### Stress Tests (randomly generated)

- [x] `random_v2_pair()` - random V2 pair with constraints
- [x] `random_v3_pair()` - random V3 pair with constraints
- [x] `random_v4_pair()` - random V4 pair with constraints
- [x] `random_multi_pool_cycle()` - 3+ pool cycles

### 2.3 Unit Tests for Fixtures

- [x] Test fixture serialization round-trip
- [x] Test fixture file save/load
- [x] Test all simple cases produce valid fixtures
- [x] Test random fixtures with various seeds

---

## Phase 3: Presets & Serialization

### 3.1 Presets (`presets.py`)

- [x] Define `SIMPLE_FIXTURES` list
- [x] Define `STRESS_FIXTURES_V2` list (100 seeds)
- [x] Define `STRESS_FIXTURES_V3` list (100 seeds)
- [x] Define `STRESS_FIXTURES_V4` list (100 seeds)
- [x] Define `ALL_FIXTURES` combined list
- [x] Implement `generate_all_fixtures()` to write JSON files
- [x] Implement `load_fixture_by_name()` convenience function

### 3.2 Generate Fixture Files

- [x] Create `fixtures/` directory
- [x] Generate all simple fixture JSON files
- [x] Generate V2 stress test fixture files
- [x] Generate V3 stress test fixture files
- [x] Generate V4 stress test fixture files

---

## Phase 4: Baseline Recording

### 4.1 Baseline Types (`baseline.py`)

- [ ] Create `CalculationBaseline` frozen dataclass
- [ ] Implement `to_json()` serialization
- [ ] Implement `from_json()` deserialization

### 4.2 Baseline Manager

- [ ] Create `BaselineManager` class
- [ ] Implement `record()` - capture calculation result
- [ ] Implement `get_baseline()` - retrieve by fixture ID
- [ ] Implement `all_baselines()` - get all records
- [ ] Implement `save_all()` - persist to disk
- [ ] Implement `load_all()` - load from disk
- [ ] Add git commit hash capture for traceability

### 4.3 Baseline CLI (optional)

- [ ] Add `--record-baseline` pytest flag
- [ ] Add `--update-baseline` pytest flag
- [ ] Add `--baseline-dir` pytest flag

---

## Phase 5: Regression Tests

### 5.1 Core Regression Tests (`test_regression.py`)

- [ ] Test simple fixtures match expected profit within tolerance
- [ ] Test simple fixtures match baseline (if exists)
- [ ] Test stress fixtures vs baseline
- [ ] Test calculation time doesn't regress beyond threshold

### 5.2 Benchmark Integration

- [ ] Add pytest-benchmark integration
- [ ] Create benchmark suite for simple fixtures
- [ ] Create benchmark suite for stress fixtures
- [ ] Add benchmark comparison to baseline

### 5.3 CI Integration

- [ ] Ensure tests run in CI without network
- [ ] Add baseline update workflow documentation
- [ ] Add regression threshold configuration

---

## Phase 6: Polish & Documentation

### 6.1 Code Quality

- [ ] Run `ruff check` and fix all issues
- [ ] Run `mypy` and fix all type issues
- [ ] Add docstrings to all public classes/functions

### 6.2 Documentation

- [ ] Add README for `tests/arbitrage/generator/`
- [ ] Document fixture JSON schema
- [ ] Document baseline JSON schema
- [ ] Add usage examples

### 6.3 Integration

- [ ] Export all public APIs from `tests/arbitrage/__init__.py`
- [ ] Export all public APIs from `tests/arbitrage/generator/__init__.py`

---

## Future (Deferred)

### Curve Support

- [ ] Add `CurvePoolGenerationConfig`
- [ ] Implement `generate_curve_pool_state()`
- [ ] Implement `generate_profitable_curve_pair()`
- [ ] Add Curve fixtures to presets
- [ ] Add Curve regression tests

### Advanced Scenarios

- [ ] Multi-hop cycles (3+ pools)
- [ ] Cross-DEX arbitrage (Uniswap vs Curve)
- [ ] Flash loan cost modeling
- [ ] Gas cost modeling

---

## Notes

### Design Decisions

1. **Frozen dataclasses**: Match existing pool state patterns, enable hashing/caching
2. **Separate fixture files**: JSON for portability and git-friendliness
3. **Tolerance-based comparison**: Allow small numerical deviations
4. **Mock pools where possible**: Avoid network dependencies
5. **Deterministic generation**: Same seed → same fixture

### File Structure

```
tests/arbitrage/
├── __init__.py
├── conftest.py
├── test_uniswap_lp_cycle.py
├── test_uniswap_2pool_cycle.py
├── test_uniswap_curve_cycle.py
├── test_regression.py
│
├── generator/
│   ├── __init__.py
│   ├── types.py
│   ├── pool_generator.py
│   └── fixtures.py
│
├── presets.py
├── baseline.py
│
├── fixtures/
│   └── *.json
│
└── baselines/
    └── *.json
```

---

## Progress Log

| Date | Phase | Description |
|------|-------|-------------|
| 2026-04-04 | 1.1 | Completed generator types.py |
| 2026-04-04 | 1.2 | Completed pool_generator.py core methods |
| 2026-04-04 | 1.3 | Completed profitable pair generation methods |
| 2026-04-04 | 2.1-2.2 | Completed fixtures.py with FixtureFactory |
| 2026-04-04 | 1.4 & 2.3 | Completed all unit tests (49 new tests, 76 total) |
| 2026-04-04 | 3 | Completed presets.py + fixture generation (37 tests, 113 total) |
