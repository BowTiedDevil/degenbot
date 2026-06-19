# AGENTS.md

## Planning

Use `ergo` for all feature planning. Discover usage with `ergo --help` and `ergo quickstart`.

## Refactoring & Feature Development

Use Red/Green TDD while refactoring and implementing new features.

## Commands

Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is automatically rebuilt on import by maturin. Do NOT manually rebuild the extension, recreate the virtual environment, or reinstall the package after making Rust code changes. Any `uv run ...` command will trigger a rebuild if needed.

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Git Commits

All commit messages must follow the project convention enforced by `commitlint`. Run `just setup-git-hooks` once after cloning to enable the local hook and editor template.

```
type(scope): subject

[optional body]

[optional footer]
```

### Types

| Type | Use for |
|------|---------|
| `feat` | New user-facing functionality |
| `fix` | Bug fixes |
| `refactor` | Structural changes with no public API change |
| `docs` | Documentation only |
| `lint` | Auto-formatter or linter-driven changes only |
| `test` | Adding or updating tests |
| `chore` | Repo maintenance (locks, CI, justfile, plan files) |
| `remove` | Pure deletion of dead code, shims, deprecated files |

### Scopes

Optional but encouraged. Closed list: `curve`, `aave`, `v2`, `v3`, `v4`, `aerodrome`, `camelot`, `balancer`, `arbitrage`, `builders`, `database`, `rust`, `python`, `sdk`. Omit scope for cross-cutting changes. Enforced in `.commitlintrc.yml` (the authoritative list — keep both in sync).

### Rules

- **Subject**: imperative mood, no trailing period, ≤72 characters
- **Body**: optional, wrap at 80 characters
- **Footer**: use `Plan: <number>` to reference an architecture plan (repeatable); use `BREAKING CHANGE: <description>` for breaking changes

### Examples

```
feat(curve): strategy objects & lending rate fetchers
fix(v3): correct tick spacing calculation
refactor: separate I/O from calculation in Aave position analysis
remove: delete position_analysis.py shim

Plan: 029
```

Bypass with `--no-verify` if needed, but CI will still reject non-conforming messages on PRs.

## Database
- Config file at `~/.config/degenbot/config.toml`; database path defaults to `~/.config/degenbot/degenbot.db` (overridable via `database.path` setting)
- SQLAlchemy ORM models in `src/degenbot/database/models/`
- Use the scoped session context manager: `with db_session() as session:` from `degenbot.database.db_session`

## Python Design

### Imports
- Add imports at the top level of each module
- Do not import inside classes or functions
- If a circular import is found, fix it immediately

### Patterns
- Prefer frozen `dataclass` for value objects passed between functions
- Prefer `TypedDict` if key and value types are known

### Error Handling
- All exceptions inherit from `DegenbotError` in `src/degenbot/exceptions/base.py`
- Create specific subtypes for distinct categories
- Catch specific exceptions (`except TimeoutError:`), avoid broad catches

Exceptions are organized into 4 domain-aligned files in `src/degenbot/exceptions/`: `base.py` (`DegenbotError`/`DegenbotValueError`/`DegenbotTypeError`/`ExternalServiceError`), `pool.py` (EVM/Curve/Tracker/LiquidityPool; `PossibleInaccurateResult` with subclasses `HookedPoolResult` and `StaleRateResult`), `arbitrage.py` (solver + swap encoding), `infrastructure.py` (connection/fetch/registry/database/anvil/ERC20).

#### Exception Messages

Use the `msg` variable pattern to avoid ruff TRY003 warnings:

```python
# Bad: message inline with exception (triggers TRY003)
raise ValueError(f"Unsupported pool type: {pool_type}")

# Good: assign to msg variable first
msg = f"Unsupported pool type: {pool_type}"
raise ValueError(msg)
```

### Logging
- Use `from degenbot.logging import logger`

### Testing
- Add docstring to complex tests describing "what" and "why"
- Create a test double with `Fake` prefix instead of mocking

## Backwards Compatibility
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Architecture & Domain Knowledge

The codebase uses layered documentation. This file holds operational guidance only — read the relevant focused doc before naming, editing, or extending a module.

- **[`CONTEXT-MAP.md`](CONTEXT-MAP.md)** — ubiquitous-language index + per-module `CONTEXT.md` pointers. Read the relevant module context before naming variables, classes, or docstrings.
- **[`docs/architecture/`](docs/architecture/)** — long-form architecture:
  - `io-free-pools.md` — I/O-free pool pattern, `CurveDataProvider` seam, `DyCalculator`, `DyCalculationInputs`
  - `rust-owned-bot.md` — Rust-owned backrun bot: `UniswapEngine`, V2/V3/V4 block engines, the pump + backfill, reorg journal, executor contract, swap encoding, Bot as single state owner, GIL discipline
  - `rust-solver-engine.md` — three-layer FFI diagram (Python / PyO3 / Rust core)
  - `semantic-matching.md` — event association by user+asset (not amounts)
- **[ADR records](docs/adr/)** — ADR-001 I/O-free pools, ADR-002 pool-type registry singleton, ADR-003 Bot as state owner, ADR-004 CL tickmap typed boundary, ADR-005 Polars-inspired three-layer FFI, ADR-006 per-chain bot orchestrator
- **[`docs/migration-guides/`](docs/migration-guides/)** — completed refactors:
  - `dex-subclass-collapse.md` — hollow V2 DEX subclasses (`SushiswapV2Pool`, `PancakeswapV2Pool`, `SwapbasedV2Pool`, `CamelotLiquidityPool`) deleted; canonical `LiquidityPool` registered per V2-family factory
  - `legacy-cycles-to-arbitrage-path.md` — legacy cycle classes → `ArbitragePath` + `ArbSolver`; `cvxpy` now an optional `[legacy-cycles]` extra
- **[`rust/AGENTS.md`](rust/AGENTS.md)** — Rust extension three-layer pattern, module organization, coding standards, GIL release protocol, Python↔Rust type-conversion protocol
- **[`contracts/README.md`](contracts/README.md)** — on-chain V2/V3/V4 arbitrage executor (`cmd_executor` / `tstore_executor`), command set, callbacks, V4 4-phase settlement, V3 auto-pay, bytecode recompilation, V3 vs V4 `amountSpecified` sign convention
- **[`contract_reference/README.md`](contract_reference/README.md)** — verified Solidity sources (Uniswap V2/V3/V4, Aave V3); ground truth for integer-exact Python ports
- **[`docs/aave/`](docs/aave/) + [`src/degenbot/aave/CONTEXT.md`](src/degenbot/aave/CONTEXT.md)** — Aave V3 processing flows, math-library layers, revision-specific processors
- **Other `AGENTS.md` files** — [`rust/AGENTS.md`](rust/AGENTS.md), [`src/degenbot/cli/AGENTS.md`](src/degenbot/cli/AGENTS.md) (Aave CLI), [`examples/AGENTS.md`](examples/AGENTS.md) (executor contracts), [`docs/aave/AGENTS.md`](docs/aave/AGENTS.md) (Aave docs nav)

### Quick index of cross-cutting conventions

(Full detail in the docs above — this index is for fast lookup.)

- **Pool class composition**: `class Pool(AbstractLiquidityPool, StateMixin, CalcMixin)`. Mixin/state-cache table and `PoolFamily` vs `PoolInvariant` enums in [`src/degenbot/types/CONTEXT.md`](src/degenbot/types/CONTEXT.md).
- **Bot session pattern**: all pool/token creation flows through `Bot.build_pool()` / `build_managed_pool()` / `build_erc20token()`; never instantiate pools directly. Builders receive a `BuilderContext`. See [`src/degenbot/builders/CONTEXT.md`](src/degenbot/builders/CONTEXT.md) and [`src/degenbot/types/CONTEXT.md`](src/degenbot/types/CONTEXT.md) (type resolution, builder base classes).
- **I/O-free pools**: builders fetch all data; pools update via `external_update()` (pure logic). See [`docs/architecture/io-free-pools.md`](docs/architecture/io-free-pools.md).
- **V4 filtering**: pools with amount-modifying hooks (`hook_flags & 0xCC != 0`) or dynamic fees (`fee == 0x100000`) are rejected in Python before `register_v4_pool`. See [`rust/CONTEXT.md`](rust/CONTEXT.md).
- **V3 vs V4 `amountSpecified`**: opposite sign conventions (V3 exact-input = positive; V4 exact-input = negative). See [`contracts/README.md`](contracts/README.md) § "V3 vs V4 amountSpecified Sign Convention" and [`src/degenbot/uniswap/CONTEXT.md`](src/degenbot/uniswap/CONTEXT.md).
- **Swap encoding**: each `SwapAmounts` subclass `encode(recipient=)`; `generate_payloads()` wires per-hop encode → `ApprovalStrategy` → `PayloadComposer`. See [`src/degenbot/arbitrage/CONTEXT.md`](src/degenbot/arbitrage/CONTEXT.md).
- **int128 overflow guard**: V4 `BalanceDelta` overflows ±2^127; encoders skip via `fits_int128()`. See [`src/degenbot/arbitrage/CONTEXT.md`](src/degenbot/arbitrage/CONTEXT.md) and [`contracts/README.md`](contracts/README.md).

## Agent skills

### Issue tracker

Local markdown files under `.scratch/<feature-slug>/`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context — `CONTEXT-MAP.md` at root pointing to per-module `CONTEXT.md` files. See `docs/agents/domain.md`.

## Architecture Plans

Refactoring plans live in `plans/`. Completed plans live in `plans/completed/`. Each plan tracks its own status via the checklist at the bottom of its file.

**New plans must follow [`plans/TEMPLATE.md`](plans/TEMPLATE.md).** The template requires: deletion test, friction table, vertical slices, design decisions, relationships to other plans, and a status checklist.

When a plan is marked complete: (1) move its file from `plans/` to `plans/completed/` and (2) update the link in any referencing documents to point to the new `completed/` path.

## Solidity Porting Essentials

- Solidity `< 0.8.0` arithmetic silently wraps; `0.8.0+` is checked (reverts on overflow).
- Python `//` matches Solidity `/` (integer division) — use `//` for EVM-exact math.
- Solidity `/` truncates toward zero; Python `//` floors toward −∞. Use `_truncated_div` in `log_exp_math.py` when matching Taylor-series divisions with negative operands.
- **Balancer V2 `FixedPoint.pow`**: deployed contracts use different implementations per version — `PowVersion` (V1/V2) controls the `y == ONE/TWO/FOUR` fast paths. Fee ordering in GIVEN_OUT must match Solidity: downscale first, then add fee.
- **Balancer V2 StableMath**: two invariant versions (V1 `_calculate_invariant` / V2 `_calculate_invariant_deployed`); using V2 when V1 is needed produces a systematic 1-wei error. ComposableStablePools with time-varying rates need a `CacheAwareRateProvider` replicating `_cacheTokenRateIfNecessary`; otherwise `StaleRateResult` is raised.
- When porting on-chain logic to Python, add a `See: contract_reference/...` comment. Verified sources: [`contract_reference/README.md`](contract_reference/README.md).
