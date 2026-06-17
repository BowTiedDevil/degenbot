{
  "id": "69cc2bea",
  "title": "Migrate Bot/PoolRegistry to wrap Rust-core PyPool handles (BotCore peer to UniswapEngine, ADR-003 follow-up)",
  "tags": [],
  "status": "open",
  "created_at": "2026-06-17T03:49:03.211Z"
}

## Context

ADR-003 (`docs/adr/ADR-003-botcore-state-layer.md`) settled the **state-and-concerns** architecture: `BotCore` is the single Rust owner of pool/token state, peer to `UniswapEngine` (which owns solving + the pump). That ADR deliberately did **not** decide how the Python `Bot` session class and its three registries migrate to wrap the new Rust-core handles. This todo tracks that follow-up session.

## Out of scope for ADR-003, in scope here

Today's Python `Bot` (`src/degenbot/bot.py`) owns:
- `ConnectionManager` (RPC providers) — I/O, stays Python
- `DatabaseSessionManager` — I/O, stays Python
- `PoolRegistry` (`Address → Python AbstractLiquidityPool`) — **migrates**: pools now have Rust-owned state
- `TokenRegistry` (`Address → Python Erc20Token`) — **migrates**: tokens have Rust-owned metadata (ADR-003: `BotCore.tokens` + `PyToken` handle)
- `ManagedPoolRegistry` (V4, `(chain, PoolManager, PoolId) → Python pool`) — **migrates**
- Per-family builders (`V2Builder`, `V3Builder`, `V4Builder`, `BalancerBuilder`, `CamelotBuilder`, `CurvePoolBuilder`) — I/O orchestration, stay Python, but their *return values* shift to Rust handles

## The decision this session must make

**What does `Bot.build_pool` return under ADR-003's architecture?** Three resolutions identified during grilling (consult `ADR-003` for context):

| Option | `Bot.build_pool` returns | Registry holds | Notes |
|---|---|---|---|
| **B1 — direct `PyPool`** | the Rust handle itself | `Address → PyPool` | cleanest long-term; breaks library consumers expecting `AbstractLiquidityPool` methods |
| **B2 — Python wrapper over `PyPool`** | thin Python class implementing `ArbitragePathPool`/`AbstractLiquidityPool` by delegating to `PyPool` | `Address → Python wrapper` | preserves library API; wrapper is stateless orchestration; **mirrors the `Erc20Token`-wraps-`PyToken` pattern ADR-003 settled for tokens** |
| **B3 — `PyPool` registered in `BotCore`, `Bot` registry removed** | `PyPool` | no Python `PoolRegistry` — `BotCore` is the registry | cleanest but biggest blast radius |

**Provisional recommendation from grilling: B2**, by direct analogy to the token pattern (`Erc20Token` wraps `PyToken`, keeps price oracle + display Python-side; the pool wrapper wraps `PyPool`, keeps subscription/publish Python-side if needed). But this needs revisiting against *real library callers* — don't commit before checking:
- who constructs `ArbitragePath(pools=[...])` and what pool types they pass
- whether `ArbitragePathPool`/`AbstractLiquidityPool` protocol surface is fully coverable by delegation to `PyPool`
- whether Python-side pool subscription (Pool State Messages) still has a role under ADR-003, or whether notification flows entirely through `UniswapEngine`'s result batches

## Related concepts (from ADR-003 grilling)

- **`PyPool`**: thin PyO3 handle over `Arc<Mutex<BotCore>>` + `pool_id`; reads state, exposes `calculate_tokens_out`/`calculate_tokens_in`/`encode_swap`. Under ADR-003 this is the Rust-core pool identity.
- **`PyToken`**: thin PyO3 handle over `Arc<Mutex<BotCore>>` + `Address`; reads token metadata (address, decimals, symbol, name). Python's `Erc20Token` wraps it with the price oracle.
- **`PoolRegistry`/`TokenRegistry`/`ManagedPoolRegistry`**: today class instances owned by `Bot`; under ADR-003 some/all may collapse into `BotCore`'s existing `pool_addresses` index.

## Prereqs

This session depends on ADR-003 being at least partially implemented (S1 — V2 state consolidation onto BotCore — should be done first, so `PyPool`/`PyBotCore` are real, working handles to wrap). Don't start before S1 lands.

## Definition of done

- [ ] Decision B1/B2/B3 recorded as an ADR (probably ADR-004 or similar)
- [ ] `Bot.build_pool`/`build_erc20token`/`build_managed_pool` return the chosen shape
- [ ] `PoolRegistry`/`TokenRegistry`/`ManagedPoolRegistry` either migrated or deleted per the decision
- [ ] `ArbitragePath` construction path verified against real library callers
- [ ] Production example (`examples/eth_backrun_v2_v3_v4_rust.py`) `EngineRegistry` either still works or has a clean replacement (it currently builds Python pools and re-registers with the engine — ADR-003's `PyPool`-as-construction-entry should simplify this)
