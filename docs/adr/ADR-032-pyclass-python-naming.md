# ADR-032: `#[pyclass]` Python naming convention — clean names; `Py` prefix is Rust-internal

**Status: accepted.**

> Follows the ADR-026 precedent for converging divergent naming:
> policy + mechanical enforcement + direct renames for the existing
> population (no backward-compat alias shims), each rename in its own
> task.

## Context

The PyO3 layer (the `degenbot_rs` cdylib, Python module `degenbot._ffi`)
has two coexisting conventions for registered `#[pyclass]` types:

- **Clean Python-facing names**: `ArbitrageEngine`, `AlloyProvider`,
  `Contract`, `AsyncContract`, `AnvilFork`, `CancelHandle`, `BlockStream`,
  `PathIterator`, `LogFilter`, `AlloySubscription`, `Erc20TokenRow`,
  the `*Row` / `*RowInput` types, and the typed exceptions.
- **`Py`-prefixed visible names**: the grandfather list in the Decision
  below (27 names at census time).

The consumer layer already splits policy on this: `degenbot/dispatch/__init__.py`
strips the prefix via alias re-exports (stable companion names for driver
code), while `degenbot/bot/__init__.py` *recommends* the prefixed names
(`from degenbot.bot import Bot, PyBot, PyBotIo`). New types have been picking
a convention by drift, not by policy.

## Decision

1. New `#[pyclass]` types MUST use clean Python-facing names: leading
   capital, no `Py` prefix. (`Py`-prefixed *Rust* type names remain an
   acceptable Rust-internal convention; only the Python-facing `name=`
   registration is constrained here. The `*_py` pyfunction suffix stays
   where it marks the FFI seam — the companion packages strip it for driver
   code.)
2. A `Py`-prefixed Python-visible name is a Rust-internal seam type and may
   exist only on the grandfather list. The prefix is never extended.
3. **Grandfather list** (runtime census 2026-08-17, 27 names):
   `PyAavePriceOracle`, `PyBalanceVectorView`, `PyBot`, `PyBotIo`,
   `PyChainlinkPriceFeed`, `PyCollateralPositionData`,
   `PyConcentratedLiquidityView`, `PyDatabasePositionQuery`,
   `PyDatabaseSnapshot`, `PyDebtPositionData`, `PyDexIdentity`,
   `PyDispatchCandidate`, `PyDispatchOutcome`, `PyDispatcher`,
   `PyDivergentPool`, `PyErc20Token`, `PyLiquidityPool`,
   `PyPayloadComposer`, `PyPool`, `PyReservePairView`, `PySimulateContext`,
   `PySolveResult`, `PySubscription`, `PySubmitCandidate`, `PyTxParams`,
   `PyTxSigner`, `PyUserPositionSummary`.
4. **Retirement**: task `VD5MD5` (epic `C7D2CH`) renames the grandfather
   population to clean names per the ADR-026 precedent (direct rename, all
   import sites updated in-repo, no backward-compat aliases). Each rename
   removes the name from this list and from the gate test's grandfather set
   in the same commit.
5. **Enforcement**: `tests/rust/test_pyclass_naming_gate.py` walks the
   runtime `degenbot._ffi` module tree and asserts, in both directions, that
   the set of registered `Py`-prefixed class names equals the grandfather
   list. A new `Py`-prefixed registration — or a dead list entry — fails the
   test.

## Consequences

- The `VD5MD5` rename is mechanical and auditable: the list IS the scope.
- Companion packages (`degenbot.dispatch`, `degenbot.bot`) keep working
  without alias churn until the rename lands; after it, prefix-stripping
  aliases become the vestige to remove.
- The gate is a runtime test over the built extension: it runs in the same
  `tests/rust` suite as the registration↔stub drift gate (task `DSWX6Z`),
  so a renamed class that skips its stub update fails both gates.

## Post-adoption: VD5MD5 rename complete (2026-08-17)

The grandfather list is **empty**: all 27 census names were renamed
(task `VD5MD5`, direct rename, no backward-compat aliases, per the ADR-026
precedent). Mapping:

| Python name before | Python name after | Note |
| --- | --- | --- |
| `PyBot` | `RustBot` | the Python session class `degenbot.bot.Bot` keeps the clean name; the Rust engine handle is origin-descriptive |
| `PyBotIo` | `RustBotIo` | same collision reasoning |
| `PyErc20Token` | `RustErc20Token` | the Python model class `degenbot.erc20.Erc20Token` keeps the clean name |
| `PySubscription` | `PoolStateSubscription` | distinct from the Python `provider.Subscription` wrapper; role-specific name (pool-state-change subscription handle) |
| `PyDatabaseSnapshot` | `RustDatabaseSnapshot` | the Python `uniswap.{v3,v4}_snapshot.DatabaseSnapshot` sources keep the clean name |
| `PyDatabasePositionQuery` | `RustDatabasePositionQuery` | the Python `aave.analysis.orchestrator.DatabasePositionQuery` shell keeps the clean name |
| `PyLiquidityPool` | `LiquidityPool` | mechanical |
| `PyPool` | `Pool` | mechanical (structural handle mirroring `degenbot_pools::Pool`) |
| `PyDexIdentity` | `DexIdentity` | mechanical; `degenbot.types` already exported this alias — now a direct import |
| `PyDispatcher`, `PyTxSigner`, `PyDivergentPool`, `PySubmitCandidate`, `PyTxParams` | `Dispatcher`, `TxSigner`, `DivergentPool`, `SubmitCandidate`, `TxParams` | mechanical; companion aliases in `degenbot.dispatch` became direct imports |
| `PySimulateContext`, `PyDispatchCandidate`, `PyDispatchOutcome`, `PySolveResult`, `PyPayloadComposer` | `SimulateContext`, `DispatchCandidate`, `DispatchOutcome`, `SolveResult`, `PayloadComposer` | mechanical |
| `PyChainlinkPriceFeed`, `PyAavePriceOracle` | `ChainlinkPriceFeed`, `AavePriceOracle` | mechanical; `degenbot.chainlink` / `degenbot.aave` alias imports became direct |
| `PyUserPositionSummary`, `PyCollateralPositionData`, `PyDebtPositionData` | `UserPositionSummary`, `CollateralPositionData`, `DebtPositionData` | mechanical (db analysis trio) |
| `PyReservePairView`, `PyConcentratedLiquidityView`, `PyBalanceVectorView` | `ReservePairView`, `ConcentratedLiquidityView`, `BalanceVectorView` | mechanical (state views) |

Mechanically: the 15 pyclasses with explicit `name = "PyX"` attributes took
the new value; the 12 struct-default pyclasses gained an explicit
`name = "X"` (Rust struct names keep the `Py` qualifier — Rust-internal,
per D1). All Python import sites, companion alias imports (collapsed to
direct imports), `.pyi` stubs, tests, and examples were updated in the same
change; Python-visible repr/error-message strings in Rust were aligned.
