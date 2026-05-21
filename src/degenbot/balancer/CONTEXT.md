# Context — Balancer V2 Weighted Pools

## Pool Types

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Weighted Pool** | A Balancer V2 pool using the weighted product invariant with configurable token weights | Balancer pool (too vague), weighted AMM |
| **BalancerV2Pool** | The Python class representing a Balancer V2 weighted pool | Balancer pool class |
| **Pool ID** | A 32-byte identifier unique to each Balancer pool, encoding the pool address, specialization, and nonce | Pool identifier |
| **Vault** | The singleton Balancer V2 Vault contract (`0xBA12222222228d8Ba445958a75a0704d566BF2C8`) that holds all pool tokens and executes swaps | Balancer vault, the vault |
| **BalancerQueries** | A helper contract (`0xE39B5e3B6D74016b2F6A9673D7d7493B6DF549d5`) for simulating Vault queries without state changes | Query helper |

## Specialization

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **General** | Specialization type 0 — pools with any number of tokens | GENERAL |
| **Minimal Swap Info** | Specialization type 1 — pools that only reveal token addresses in swap events | MINIMAL_SWAP_INFO |
| **Two Token** | Specialization type 2 — pools with exactly two tokens for optimized gas | TWO_TOKEN |

## Math Libraries

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **FixedPoint** | 18-decimal fixed-point arithmetic library (`fixed_point.py`) providing `mul_down`, `mul_up`, `div_down`, `div_up`, `pow_down`, `pow_up`, `complement` | FP, fixed math |
| **LogExpMath** | Natural logarithm and exponentiation via Taylor series (`log_exp_math.py`) — the core approximation engine for `pow` | Log-exp, Ln/pow |
| **WeightedMath** | Invariant and swap calculation formulas for weighted pools (`weighted_math.py`) | WM, weighted calculations |
| **ScalingHelpers** | Fixed-point scaling for tokens with non-18 decimals (`scaling_helpers.py`) — `_upscale`, `_downscale_down`, `_downscale_up`, `_compute_scaling_factor` | Scaling, decimal helpers |
| **InputHelpers** | Validation helpers for swap input amounts (`input_helpers.py`) | Validators |
| **Helpers** | Shared helper functions (`helpers.py`) | Utility functions |

## PowVersion

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **PowVersion.V1** | The original FixedPoint.pow implementation used by `WeightedPool2Tokens` — general LogExpMath path with error bound for all exponent values; no fast paths | Old pow, original version |
| **PowVersion.V2** | The updated FixedPoint.pow used by newer `WeightedPool` contracts — includes fast paths for y == ONE (return x), y == TWO (mul x x), y == FOUR (mul (mul x x) (mul x x)) that bypass the error-bound calculation | New pow, fast-path version |
| **Bytecode Detection** | The method of determining `PowVersion` by checking the deployed pool contract bytecode for the TWO constant (`1bc16d674ec80000`), which only appears in V2 bytecode | Version detection |

## Critical Semantics

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **Truncated Division** | Solidity's `/` operator truncates toward zero for negative operands, unlike Python's `//` which floors toward -∞. Implemented via `_truncated_div` in `log_exp_math.py` | Floor division, integer division (ambiguous) |
| **Rounding Direction** | Down for GIVEN_IN (seller gets less), up for GIVEN_OUT (buyer pays more). Applied consistently through `_calc_out_given_in` (round down) and `_calc_in_given_out` (round up) | Rounding mode |
| **Fee Ordering (GIVEN_OUT)** | Solidity's onSwap path: downscale up first, then add swap fee amount. Python must match this order exactly or the accumulated rounding differs | Fee application order |
| **Scaling Factor** | Computed as `ONE * 10**decimalsDifference` where `decimalsDifference = 18 - token_decimals`. Tokens with < 18 decimals have scaling factors > ONE; tokens with 18 decimals have scaling factor == ONE | Decimal factor, normalization factor |

## Deployments

| Term | Definition | Aliases to avoid |
|------|------------|------------------|
| **BROKEN_BALANCER_V2_POOLS** | A frozenset of pool addresses where on-chain swaps are disabled or the pool is otherwise broken (e.g., BAL#327 SWAPS_DISABLED). Pools in this set should be skipped during discovery and testing | Broken pools, disabled pools |
| **deployments.py** | Module centralizing contract addresses and broken pool addresses for Balancer V2, following the same pattern as `curve/deployments.py` | Addresses module |

## Library File Layout

```
balancer/
├── __init__.py
├── CONTEXT.md          (this file)
├── deployments.py     (contract addresses, broken pool set)
├── libraries/
│   ├── constants.py    (ONE, TWO, FOUR, MAX_POW_RELATIVE_ERROR, PowVersion)
│   ├── fixed_point.py  (mul_down/up, div_down/up, pow_down/up, complement)
│   ├── helpers.py      (shared helpers)
│   ├── input_helpers.py (validation)
│   ├── log_exp_math.py  (ln, pow via Taylor series with _truncated_div)
│   ├── scaling_helpers.py (_upscale, _downscale_down/up, _compute_scaling_factor)
│   └── weighted_math.py (calculate_invariant, _calc_out_given_in, _calc_in_given_out)
├── managers.py         (empty — reserved for future pool tracker)
├── pools.py            (BalancerV2Pool class, detect_pow_version)
└── types.py            (BalancerV2PoolState frozen dataclass)
```

## Relationships

- **BalancerV2Pool** delegates all math to **WeightedMath** functions, which in turn delegate exponentiation to **FixedPoint.pow_down/pow_up**
- **FixedPoint.pow_down/pow_up** accept a `version: PowVersion` kwarg that controls whether fast paths for y == ONE/TWO/FOUR are active; the version is detected from bytecode at construction time and stored on the pool instance
- **ScalingHelpers** are called directly by **BalancerV2Pool** — upscale before calculation, downscale after
- **LogExpMath** is a private implementation detail of **FixedPoint** — no other module should import it directly
- **BROKEN_BALANCER_V2_POOLS** in **deployments.py** is used by pool discovery and testing to filter out pools where on-chain swaps are disabled

## Resolved ambiguities

### WeightedPool2Tokens vs WeightedPool

These are distinct deployed contract types with different FixedPoint library versions. **WeightedPool2Tokens** uses PowVersion.V1 (general path only); **WeightedPool** uses PowVersion.V2 (with fast paths). The pool class `BalancerV2Pool` handles both via the `pow_version` attribute — there is no separate class per contract type.

### Vault vs Pool Contract

In Balancer V2, the **Vault** holds all tokens and executes swaps. The **Pool Contract** holds the invariant logic and state (weights, fees). When we say "the pool" we mean the Pool Contract; when we say "the vault" we mean the singleton Vault. This distinction matters because swap calls go to the Vault, not the pool contract.

### Worker (scraper) vs Pool class

The vault scraper script (`scripts/balancer_vault_scraper.py`) discovers pools by scanning Vault events. It is standalone and does not import from degenbot internals. The pool class (`BalancerV2Pool`) is the in-memory calculation object. They have no runtime dependency on each other.
