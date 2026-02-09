---
title: Uniswap Multi-Pool Cycle Arbitrage
category: arbitrage
tags:
 - arbitrage
 - optimization
 - convex
 - cvxpy
 - dpp
 - uniswap-v2
 - aerodrome
related_files:
  - ../../src/degenbot/arbitrage/uniswap_lp_cycle.py
  - ../../src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py
  - ../../src/degenbot/arbitrage/uniswap_curve_cycle.py
  - ../../src/degenbot/arbitrage/types.py
  - ../../src/degenbot/uniswap/v2_liquidity_pool.py
  - ../../src/degenbot/aerodrome/pools.py
complexity: complex
---

# Uniswap Multi-Pool Cycle Arbitrage

## Overview

The `_UniswapMultiPoolCycleTesting` class ([`uniswap_multipool_cycle_testing.py`](../../src/degenbot/arbitrage/uniswap_multipool_cycle_testing.py)) implements a convex optimization approach to find optimal arbitrage profits across a sequence of Uniswap V2-style AMM pools. It uses CVXPY with DPP (Disciplined Parametric Programming) to efficiently solve for maximum profit token cycle paths.

### Why Convex Optimization?

Traditional approaches use binary search (see `UniswapLpCycle._calculate`) which works well for simple arbitrage but has limitations:

- **Binary Search**: Iteratively narrows search space, O(log(max_input)) iterations per pool
 - Pros: Simple, guaranteed convergence
 - Cons: Sequential pool swaps, no global optimality guarantee, slow for multi-pool cycles

- **Convex Optimization (CVXPY)**: Solves all pools simultaneously with geometric mean constraints
 - Pros: Global optimum guarantee, single solve, DPP enables fast parameterized re-solving
 - Pros: Directly models Uniswap V2 invariant: `x*y=k`
 - Cons: One-time problem compilation cost, requires numerical stability handling

DPP allows problem structure to be compiled once and re-solved with updated parameters, making this approach efficient for repeated calculations with changing pool states.

## Architecture

### Template Pattern with DPP

The class uses a two-stage approach:

1. **Problem builder** (`_build_convex_problem`): Creates a pre-compiled CVXPY problem template with placeholder values for a given number of pools.

2. **Parameterized re-solving**: Instance methods update the parameters with real pool data and re-solve without rebuilding the optimization structure.

3. **Class-level cache** (`convex_problems`): Compiled problems are stored in a dict keyed by pool count for reuse.

## Convex Problem Formulation

### Model Represents

A token cycle where:
- `token0` (profit token) is deposited into pool 0
- Swapped for `token1` at pool 0
- `token1` is passed to pool 1, swapped for `token2`
- Continues until `tokenN` is swapped back for `token0` at final pool

### Decision Variables

- `initial_pool_deposit`: Amount of profit token entering the cycle at pool 0
- `final_pool_withdrawal`: Amount of profit token exiting the cycle at last pool
- `forward_token_amount_variables`: Amounts of intermediate tokens passed between pools

### Parameters (Updated Per Instance)

- `compressed_reserves_pre_swap`: Pool reserves scaled to [0.0, 1.0] for numerical stability
- `swap_fees`: Fee rates for each token in each pool
- `pool_ks_pre_swap`: Pre-swap geometric means (k-values) for each pool

### Reserve Update Formula

```
post_reserves = pre_reserves + deposits - withdrawals - (swap_fees × deposits)
```

### Constraint Direction Proof

The constraint `k_post ≥ k_pre` holds because swap fees increase pool reserves. Since fees are kept by the pool, post-swap reserves ≥ pre-swap reserves, and the geometric mean (k-value) increases monotonically with reserve growth. This ensures the solver only finds profitable arbitrage cycles where fees accumulate in pools.

### Constraints

```
k_pre[pool] ≤ k_post[pool] for all pools
```

This ensures each pool maintains or increases its geometric mean (k-value. Since swap fees stay in the pool, reserves increase, satisfying the constraint.

### Objective

```
maximize: final_pool_withdrawal - initial_pool_deposit
```

Directly maximizes the arbitrage profit.

## Key Design Decisions

### 1. Balance Compression for Numerical Stability

Reserves are compressed to [0.0, 1.0] by dividing by the maximum token balance:

```python
balance_compression_factors = [1 / np.max(uncompressed_reserves[:, token_idx]) ...]
compressed_reserves = uncompressed_reserves × balance_compression_factors
```

**Why this is necessary**: CVXPY solvers (e.g., CLARABEL) apply small numerical perturbations (~10⁻⁸ tolerance) when searching for optimal solutions. With reserves on the order of 10¹⁸, these perturbations would have no effect on the objective function, causing convergence failures or incorrect results. Compression to [0.0, 1.0] ensures perturbations meaningfully affect the optimization landscape.

### 2. FakeToken Padding

The pre-compiled problem expects square matrices:
- 2 pools → 2×2 matrix with 2 tokens
- 3 pools → 3×3 matrix with 3 tokens

Arbitrary pool sequences that share tokens create non-square matrices. `FakeToken` objects are added as placeholder tokens to make the matrix square.

### 3. Re-calculation After Optimization

After CVXPY solves for optimal swap amounts, the implementation re-calculates exact swaps using pool methods:

```python
amount_out = pool.calculate_tokens_out_from_tokens_in(
 token_in=token_in,
 token_in_quantity=amount_in,
 override_state=pool_state,
)
```

**Why this is necessary**:
- CVXPY operates in compressed float space (64-bit floats)
- Pool methods use exact integer arithmetic (10^18 scale)
- Direct use of CVXPY solution would lose precision and could cause on-chain failures

**Potential divergence**: Small rounding differences between CVXPY (float64) and pool methods (exact integer arithmetic) may cause actual execution profit to differ from optimization result.

### 4. Error Handling

The implementation handles several error scenarios:

- **SolverError**: CVXPY fails to converge due to numerical issues or infeasible constraints
 - Raised as `ArbitrageError` with solver details

- **NoSolverSolution**: Problem status check fails (e.g., "optimal_inaccurate")
 - Indicates solver couldn't find a valid solution

- **Unprofitable**: `problem.value <= 0`
 - No profitable arbitrage exists given current pool states

- **Zero amount swaps**: Input or output amount is zero
 - Indicates degenerate solution or pool state issue

### 5. Performance Characteristics

**Compilation cost**: One-time per pool count
- Problem structure is built once via `_build_convex_problem`
- Typical compilation time: 10-50ms depending on pool count

**Re-solving speed**: Fast with DPP
- Updating parameters and re-solving: 1-10ms
- Significantly faster than binary search for 3+ pool cycles

**Memory usage**: Class-level cache
- `convex_problems` dict stores compiled problems
- Memory overhead per problem: ~1-5MB depending on pool count

**When to use convex optimization**:
- Multi-pool cycles (3+ pools): Superior to binary search
- High-precision requirements: Global optimum guarantee
- Repeated calculations: DPP amortizes compilation cost

**When to avoid**:
- 2-pool cycles: Use [`uniswap_2pool_cycle_testing.py`](../../src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py) (simpler, faster)
- Single pool: Binary search sufficient
- Memory-constrained environments: Cache consumes memory

### 6. Comparison with 2-Pool Variant

 Aspect | Multi-pool | 2-pool ([`uniswap_2pool_cycle_testing.py`](../../src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py)) |
--------|-----------|-------------------------------------------|
 Problem structure | N×N matrix | 2×2 matrix |
 Variables | N forward tokens | 1 forward token |
 Complexity | O(N²) | O(1) |
 Pool count support | 2-5 pools (payload generation) | 2 pools only |
 Use case | Complex arbitrage paths | Simple 2-pool cycles |
 Recommended | 3+ pools | 2 pools |

### 7. Payload Generation

Payload generation is hardcoded for 2-5 pools. Each pool count requires separate logic:

**Why separate functions**: The swap chain order is hardcoded for performance and safety:
- Final pool swapped first (to send profit token back to contract)
- Transfer token to first pool
- Execute swaps in reverse order (last pool → first pool)

**Payload structure**:
- `b"x"` callback mechanism triggers contract callback after final swap
- Reverse execution order ensures tokens flow correctly through the chain
- Each payload is a tuple: `(target_address, calldata, will_callback)`

**Extension path**: To support N pools, generalize with loop-based payload generation:
```python
for i in reversed(range(len(pools))):
 # Generate payload for pool i
 # ...
```

## Usage Flow

1. **Initialization**: Create instance with pool sequence and input token

2. **Calculation** (`_calculate` method):
 - Fetches or builds pre-compiled problem for pool count
 - Updates parameters with current pool states
 - Solves convex optimization
 - Re-calculates exact swap amounts using pool methods
 - Returns `ArbitrageCalculationResult` with swap amounts

3. **Execution** (`generate_payloads` method): Call with swap amounts to get transaction payloads for 2-5 pool cycles

## Known Limitations

- **Re-calculation divergence**: CVXPY uses float64 while pool methods use exact integers (10^18 scale), causing small rounding differences
- **FakeToken edge cases**: Padding assumes unused tokens can be placeholders; may fail if real tokens have zero balances
- **Payload generation**: Currently hardcoded for 2-5 pools; generalizing to N pools requires loop-based generation

## Code References

### Key Functions and Methods

  Name | Purpose |
 ------|---------|
  `FakeToken` | Placeholder class for matrix padding |
  `FakePool` | Placeholder pool for problem building |
  `_build_convex_problem` | Creates pre-compiled CVXPY problem template |
  `_UniswapMultiPoolCycleTesting` | Main arbitrage calculation class |
  `convex_problems` | Class cache of compiled problems |
  `_calculate` | Solves optimization for arbitrage profit |
  `order_tokens` | Orders tokens with FakeToken padding |
  `generate_payloads` | Generates transaction payloads |

## See Also

- [`uniswap_lp_cycle.py`](../../src/degenbot/arbitrage/uniswap_lp_cycle.py) - Base class `UniswapLpCycle` with binary search implementation
- [`uniswap_2pool_cycle_testing.py`](../../src/degenbot/arbitrage/uniswap_2pool_cycle_testing.py) - Two-pool variant with simpler optimization
- [`uniswap_curve_cycle.py`](../../src/degenbot/arbitrage/uniswap_curve_cycle.py) - Curve pool variant with different constraints
- [`types.py`](../../src/degenbot/arbitrage/types.py) - `ArbitrageCalculationResult`, `UniswapV2PoolSwapAmounts`
- [`v2_liquidity_pool.py`](../../src/degenbot/uniswap/v2_liquidity_pool.py) - `UniswapV2Pool` implementation
- [`pools.py`](../../src/degenbot/aerodrome/pools.py) - `AerodromeV2Pool` implementation

## External References

- [CVXPY DPP Tutorial](https://www.cvxpy.org/tutorial/dpp/index.html) - Disciplined Parametric Programming
- [CVXPY CLARABEL Solver](https://www.cvxpy.org/tutorial/advanced/index.html#choosing-a-solver) - Solver used for optimization
- [Geometric Mean Properties](https://en.wikipedia.org/wiki/Geometric_mean) - Mathematical background for constraints
