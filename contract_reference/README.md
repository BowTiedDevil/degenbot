# Contract Reference

Verified smart contract source code for protocols supported by degenbot. These files are the ground truth for matching on-chain behavior in Python — every mathematical function, rounding mode, and event definition should be traced back to these sources.

## Purpose

Degenbot's Python implementations must produce integer-exact results matching on-chain contract execution. The Solidity sources here serve as:

- **Reference implementations** for porting math to Python (tick math, swap calculations, invariant solving)
- **Revision snapshots** for contracts that were redeployed with logic changes (Aave Pool, AToken, VariableDebtToken)
- **Event definitions** for decoding and processing on-chain logs

When a Python function implements on-chain logic, it should include a `See: contract_reference/...` comment pointing to the exact file and line range.

## Directory Layout

```
contract_reference/
├── uniswap/                          # Uniswap V2/V3/V4
│   ├── V2/
│   │   └── UniswapV2Factory.sol      # Full V2 core: Factory, Pair, ERC20, SafeMath, Math, UQ112x112
│   ├── V3/
│   │   └── UniswapV3Factory.sol      # Full V3 core: Factory, Pool, Oracle, Tick, TickBitmap, SqrtPriceMath, SwapMath, etc.
│   └── V4/
│       └── PoolManager.sol           # Full V4 core: PoolManager, Pool, Hooks, TickBitmap, SqrtPriceMath, SwapMath, etc.
└── aave/                             # Aave V3
    ├── Pool/                         # Pool contract revisions (rev_1 through rev_10)
    ├── AToken/                       # AToken contract revisions (rev_1 through rev_5)
    ├── VariableDebtToken/             # Standard variable debt token revisions
    ├── GhoVariableDebtToken/         # GHO-specific variable debt token revisions (rev_1 through rev_6)
    ├── GhoDiscountRateStrategy/      # GHO discount rate calculation
    ├── AaveOracle/                   # Oracle contract
    ├── stkAAVE/                      # Staked AAVE token revisions (rev_1 through rev_6)
    └── RewardsController/            # Rewards controller (rev_1)
```

## Contract Files

### Uniswap V2 (`uniswap/V2/UniswapV2Factory.sol`)

A single concatenated file containing all V2 core contracts:

| Contract / Library | Description |
|---|---|
| `IUniswapV2Factory` / `UniswapV2Factory` | Pool creation, fee-to settings |
| `IUniswapV2Pair` / `UniswapV2Pair` | Constant-product AMM with directional fees, `swap()`, `mint()`, `burn()` |
| `IUniswapV2ERC20` / `UniswapV2ERC20` | LP token implementation |
| `SafeMath`, `Math`, `UQ112x112` | Arithmetic helpers |

Source: [Uniswap V2 Core](https://github.com/Uniswap/v2-core) + [V2 Periphery](https://github.com/Uniswap/v2-periphery)

### Uniswap V3 (`uniswap/V3/UniswapV3Factory.sol`)

A single concatenated file containing all V3 core contracts:

| Contract / Library | Description |
|---|---|
| `IUniswapV3Factory` / `UniswapV3Factory` | Pool creation, fee/spacing enabled checks |
| `IUniswapV3Pool` / `UniswapV3Pool` | Concentrated-liquidity AMM, `swap()`, `mint()`, `burn()`, `collect()` |
| `Oracle` | Observation-based TWAP and accumulator tracking |
| `Tick` | Tick state management (liquidity gross/net) |
| `TickBitmap` | Sparse tick bitmap for next-initialized-tick searches |
| `SqrtPriceMath` | √price-to-amount conversions |
| `SwapMath` | Step-wise swap calculation (crossing ticks) |
| `TickMath` | Tick ↔ √price conversion |
| `FullMath`, `FixedPoint96`, `FixedPoint128` | Fixed-point arithmetic |
| `SafeCast`, `LowGasSafeMath`, `UnsafeMath`, `BitMath` | Arithmetic helpers |
| `Position` | Position liquidity computation |
| `LiquidityMath` | Liquidity delta calculations |

Source: [Uniswap V3 Core](https://github.com/Uniswap/v3-core)

### Uniswap V4 (`uniswap/V4/PoolManager.sol`)

A single concatenated file containing all V4 core contracts:

| Contract / Library | Description |
|---|---|
| `IPoolManager` / `PoolManager` | Singleton architecture: `swap()`, `modifyLiquidity()`, `donate()`, `take()`, `settle()`, `clear()`, `lock()`, `unlock()` |
| `Pool` | Internal pool state logic (ephemeral storage via `extsload`/`exttload`) |
| `IHooks` / `Hooks` | Hook flags (before/after swap, modify liquidity, donate) |
| `TickBitmap` | Sparse tick bitmap (same algorithm as V3) |
| `SqrtPriceMath` | √price-to-amount conversions (same math as V3) |
| `SwapMath` | Step-wise swap calculation (same algorithm as V3) |
| `TickMath` | Tick ↔ √price conversion (same math as V3) |
| `FullMath`, `FixedPoint96`, `FixedPoint128` | Fixed-point arithmetic |
| `SafeCast`, `UnsafeMath`, `BitMath` | Arithmetic helpers |
| `Position` | Position liquidity computation |
| `LiquidityMath` | Liquidity delta calculations |
| `ProtocolFeeLibrary`, `LPFeeLibrary` | Protocol fee and LP fee bit-packing |
| `CurrencyLibrary`, `CurrencyDelta` | Currency address utilities |
| `PoolIdLibrary`, `Slot0Library` | Pool ID and slot0 bit-packing |
| `Lock` | Reentrancy lock |
| `ERC6909Claims` | ERC-6909 token claims (receipt tokens) |
| `Extsload`, `Exttload` | Ephemeral and transient storage access |
| `BalanceDeltaLibrary`, `BeforeSwapDeltaLibrary` | Delta value helpers |

Source: [Uniswap V4 Core](https://github.com/Uniswap/v4-core)

### Aave V3 (`aave/`)

Aave contracts are organized by contract type with revision files for each deployed version. See `src/degenbot/aave/CONTEXT.md` for revision-to-deployment mapping.

| Directory | Files | Description |
|---|---|---|
| `Pool/` | `rev_1.sol` – `rev_10.sol` | Main Pool contract (Supply, Borrow, Withdraw, Repay, Liquidation, FlashLoan) |
| `AToken/` | `rev_1.sol` – `rev_5.sol` | Interest-bearing collateral token |
| `VariableDebtToken/` | `rev_1.sol`, `rev_3.sol` – `rev_5.sol` | Standard variable-rate debt token |
| `GhoVariableDebtToken/` | `rev_1.sol` – `rev_6.sol` | GHO-specific variable-rate debt token with discount logic |
| `GhoDiscountRateStrategy/` | `contract.sol` | GHO discount rate calculation |
| `AaveOracle/` | `contract.sol` | Price oracle |
| `stkAAVE/` | `rev_1.sol` – `rev_6.sol` | Staked AAVE (StakedTokenV3) |
| `RewardsController/` | `rev_1.sol` | Rewards distribution |

Source: [Aave V3 Protocol](https://github.com/bgd-labs/aave-v3-origin)
