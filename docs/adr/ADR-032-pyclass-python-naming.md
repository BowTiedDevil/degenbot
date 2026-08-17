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
