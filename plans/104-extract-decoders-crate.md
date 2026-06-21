# Design & scope — degenbot-decoders extraction
## Overview

Extract the five pure Uniswap event-log decoders into a new workspace crate
`degenbot-decoders` (`rust/crates/degenbot-decoders/`). These decoders are
*provably* pure leaf code — each imports only `alloy::primitives` and
`alloy::rpc::types::Log`, hand-slices bytes, and returns `Option<Event>`. They
have **no** `degenbot-core`, **no** `degenbot-abi`, **no** `tokio`, and **no**
bot-state coupling. Today they live inside `degenbot-bot` (3,422-line
`bot_core/mod.rs` + `optimizers/`), so a standalone Rust consumer that wants to
decode a V3 `Swap` log must pull `degenbot-bot` → tokio + full alloy + rpc +
the whole pump/engine stack. This closes that standalone-core gap and fixes an
inconsistency the layout accidentally created: V2 Sync's decoder lives in
`optimizers/` while V3/V4 live in `bot_core/` — there is no principled reason
for that split.

## Problem

### Deletion test

If you deleted the new crate (collapsed the decoders back into `bot_core` +
`optimizers`): the standalone-decode claim documented in `rust/CONTEXT.md` /
ADR-005 ("a standalone Rust consumer can run the whole bot without Python")
breaks again for log decoding — decoding a single V3 Swap log would re-pull the
entire engine + tokio runtime. And the V2-decoder-in-`optimizers` anomaly
returns. The crate earns its keep by *compiler-enforcing* a purity that today
only holds by convention.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Standalone-decode forces engine dep | `bot_core/{v3_swap_decoder,v3_mint_burn_decoder,v4_swap_decoder,v4_modify_liquidity_decoder}.rs`, `optimizers/v2_sync_decoder.rs` | A consumer decoding one log pulls ~23.6k LoC + tokio + full alloy |
| Inconsistent placement | `optimizers/v2_sync_decoder.rs` vs `bot_core/v3_*.rs` | V2 Sync is no more "optimizer-y" than V3 Swap; placement looks accidental, misleads new contributors |
| `py_binding.rs` reaches `PoolId` through bot | `src/py_binding.rs:415,2869` (`degenbot_bot::bot_core::v4_swap_decoder::PoolId`) | Root cdylib depends on the engine crate for a pure type |
| Purity enforced by convention only | `rust/AGENTS.md` "no pyo3 in cores" is a lint; `cargo tree` proves purity but no crate boundary codifies the decoders' alloy-only surface | A future edit could silently add a `tokio`/`bot_core` import to a decoder |

## Solution

### What moves → `degenbot-decoders`

| File (from) | LoC | Moves as | Public surface |
|-------------|-----|----------|----------------|
| `optimizers/v2_sync_decoder.rs` | 236 | `src/v2_sync_decoder.rs` | `V2_SYNC_TOPIC`, `SyncEvent`, `decode_sync_log` |
| `bot_core/v3_swap_decoder.rs` | 369 | `src/v3_swap_decoder.rs` | `V3_SWAP_TOPIC`, `V3SwapEvent`, `decode_v3_swap_log` |
| `bot_core/v3_mint_burn_decoder.rs` | 678 | `src/v3_mint_burn_decoder.rs` | `V3_MINT_TOPIC`, `V3_BURN_TOPIC`, `V3MintEvent`, `V3BurnEvent`, `decode_v3_mint_log`, `decode_v3_burn_log` |
| `bot_core/v4_swap_decoder.rs` | 394 | `src/v4_swap_decoder.rs` | `V4_SWAP_TOPIC`, `V4SwapEvent`, `PoolId`, `decode_v4_swap_log` |
| `bot_core/v4_modify_liquidity_decoder.rs` | 423 | `src/v4_modify_liquidity_decoder.rs` | `V4_MODIFY_LIQUIDITY_TOPIC`, `V4ModifyLiquidityEvent`, `decode_v4_modify_liquidity_log` |

~2,100 LoC, alloy-only. Crate deps = `alloy` only (mirrors `degenbot-cl-math`'s
intended purity, minus even `degenbot-core` — decoders return `Option`, no
typed errors).

### What stays in `degenbot-bot` (`bot_core/log_dispatcher.rs`)

The **state-coupled** dispatch layer does NOT move — it touches `BotState`:

- `LogDecoder` trait + its 5 concrete wrapper impls (`V2SyncDecoder`…
  `V4ModifyLiquidityDecoder`) — these call the pure decode fns and re-wrap
  into `DecodedPoolEvent`.
- `DecodedPoolEvent` enum + `apply(&mut BotState)` + `resolve_pool_id`.
- `LogDispatcher` bus + `PoolStateSubscriber` trait.

After the move, `log_dispatcher.rs` imports the decode fns from
`degenbot_decoders::{...}`; the wrapper structs stay local. This is a
relocation of already-leaf code, **not** a new trait seam — it sidesteps
ADR-003's "no abstraction against a sample-of-one" guardrail.

### Intra-crate path fixes after the move

- `v4_modify_liquidity_decoder.rs`: `crate::bot_core::v4_swap_decoder::PoolId`
  → `crate::v4_swap_decoder::PoolId`; test refs at
  `crate::bot_core::v4_swap_decoder::{V4_SWAP_TOPIC, decode_v4_swap_log}` →
  `crate::v4_swap_decoder::{...}`.
- `v3_mint_burn_decoder.rs` test: `crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC`
  → `crate::v3_swap_decoder::V3_SWAP_TOPIC`.

### Consumer rewires (bot crate)

- `bot_core/mod.rs`: drop `pub mod v3_swap_decoder; v3_mint_burn_decoder;
  v4_swap_decoder; v4_modify_liquidity_decoder;` (lines ~31,33,34,36).
- `optimizers/mod.rs`: drop `pub mod v2_sync_decoder;` (line ~23).
- `bot_core/log_dispatcher.rs:23-26`: the four `decode_v*_log` imports →
  `degenbot_decoders::...`.
- `bot_core/block_pump.rs:45-50`: the six `*_TOPIC` imports →
  `degenbot_decoders::...`.
- `bot_core/reorg_coordinator.rs:449,658`: `use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;`
  and `use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;` →
  `degenbot_decoders::...`.
- `degenbot-bot/Cargo.toml`: add `degenbot-decoders = { path = "../degenbot-decoders" }`.

### Consumer rewires (root cdylib)

- `Cargo.toml` (root): add `degenbot-decoders = { path = "crates/degenbot-decoders" }`
  to `[dependencies]`; add `crates/degenbot-decoders` to `[workspace] members`;
  add `[profile.release.package.degenbot-decoders] codegen-units = 16`.
- `src/py_binding.rs:415,2869`: `degenbot_bot::bot_core::v4_swap_decoder::PoolId`
  → `degenbot_decoders::v4_swap_decoder::PoolId`.
- `src/lib.rs`: update the `abi`/`bot_core` re-export comment block if it
  mentions decoder modules (no re-export added — root reaches decoders via the
  direct path dep only where `py_binding` needs `PoolId`).

### justfile

- `check-no-pyo3-in-cores`: add `degenbot-decoders` to the loop list (it is
  pyo3-free; the loop codifies that).

## Files Involved

**Primary:**
- `rust/crates/degenbot-decoders/Cargo.toml` — new (mirror `degenbot-cl-math`
  manifest: alloy dep, `edition = "2021"`, `publish = false`, deny-warnings
  lints).
- `rust/crates/degenbot-decoders/src/lib.rs` — new; `pub mod` the 5 modules +
  crate doc stating "alloy-only leaf; no pyo3, no tokio, no degenbot-core".
- `rust/crates/degenbot-decoders/src/{v2_sync_decoder,v3_swap_decoder,v3_mint_burn_decoder,v4_swap_decoder,v4_modify_liquidity_decoder}.rs`
  — moved from `degenbot-bot` with intra-crate paths fixed.

**Secondary:**
- `rust/crates/degenbot-bot/src/bot_core/mod.rs` — drop 4 `pub mod` decls.
- `rust/crates/degenbot-bot/src/optimizers/mod.rs` — drop 1 `pub mod` decl.
- `rust/crates/degenbot-bot/src/bot_core/{log_dispatcher,block_pump,reorg_coordinator}.rs` — rewire imports.
- `rust/crates/degenbot-bot/Cargo.toml` — add path dep.
- `rust/Cargo.toml` (root) — workspace member, path dep, release profile entry.
- `rust/src/py_binding.rs` — 2 `PoolId` path updates.
- `rust/src/lib.rs` — comment update.
- `justfile` — add `degenbot-decoders` to `check-no-pyo3-in-cores`.
- `rust/CONTEXT.md`, `rust/AGENTS.md`, `CONTEXT-MAP.md` — module-table + bullet updates.

## Benefits

- **Locality**: all Uniswap event-log decoders in one place (ends the
  `optimizers/` vs `bot_core/` split).
- **Depth**: a 2,100-LoC alloy-only leaf with a shallow seam (5 free fns +
  plain structs) sits as a compile-enforced pure core; the deep
  `BotState`-coupled dispatch stays in `degenbot-bot`.
- **Leverage**: the `LogDecoder` trait's stated extensibility ("a future
  Curve/Aave decoder registers here without `Bot` knowing its event shapes")
  gets a natural home — non-Uniswap decoders can land in this crate alongside.
- **Standalone-core claim**: honored for log decoding — decode a V3 Swap log
  without tokio / engine / rpc.

## Risks

- **Mid-slice broken compile**: moving the files leaves dangling
  `crate::bot_core::v3_swap_decoder` refs in `bot_core` + root. Mitigation:
  Slice 2 rewires ALL consumers + deletes originals in one green step; Slice 1
  copies first so the suite stays green throughout (Red/Green discipline from
  `AGENTS.md`).
- **`PoolId` reachability from root**: if root's path dep on
  `degenbot-decoders` is missed, `py_binding.rs` fails to compile. Mitigation:
  Slice 3 adds the root dep explicitly and runs `just check-no-pyo3-in-cores`.
- **No semantic change**: decoders are bit-for-bit moves. If `just test-rust`
  red after Slice 2, it's a missed import, not a logic change — grep
  `bot_core::v[234]_` to find stragglers.

## Relationship to Other Plans

- **Plan 105** (Extract `degenbot-uniswap` crate): **independent but
  file-overlapping**. Both edit `bot_core/mod.rs`, root `Cargo.toml`, and
  `justfile`. Sequence them — do NOT run in parallel. This plan (104) touches
  decoder modules + `PoolId`; Plan 105 touches `dex_identity` + `v2_encoding`.
  No crate depends on the other (decoders are alloy-only; uniswap needsabi/core — no decoder use).
- **Plan 103** (rust-workspace-split, completed): this is a continuation —
  103's target diagram put `dex_identity` in core, which 105 finally lands
  (elsewhere); 103 did not anticipate the decoders leaf, which 104 extracts.
- **Ergo epic `XQ5UX6` Slice 13** ("Crate split — degenbot-core / degenbot-python / umbrella"): orthogonal — that's a Python-umbrella split, not a Rust-core leaf extraction.

---
# Slice 1: Scaffold crate + copy decoders (green, suite unaffected)

RED/GREEN: copy first so `degenbot-bot` still owns its originals and the test suite stays green throughout.

1. `mkdir -p rust/crates/degenbot-decoders/src`
2. Create `rust/crates/degenbot-decoders/Cargo.toml` mirroring
   `degenbot-cl-math`'s manifest: `name = "degenbot-decoders"`, `edition = "2021"`,
   `publish = false`, `[dependencies] alloy = { version = "^2.0" }` ONLY (no
   core, no abi, no tokio), plus the standard deny-warnings `[lints]` block.
3. Create `rust/crates/degenbot-decoders/src/lib.rs`: `pub mod v2_sync_decoder; pub mod v3_swap_decoder; pub mod v3_mint_burn_decoder; pub mod v4_swap_decoder; pub mod v4_modify_liquidity_decoder;` with a crate doc stating "alloy-only leaf; no pyo3/tokio/degenbot-core — independently testable log decoders for Uniswap V2/V3/V4 events (extensible to Curve/Aave)."
4. Copy (not move) the 5 decoder files from `degenbot-bot` into the new crate's `src/`.
5. Fix intra-crate paths in the COPIES:
   - `v4_modify_liquidity_decoder.rs`: `use crate::bot_core::v4_swap_decoder::PoolId;` → `use crate::v4_swap_decoder::PoolId;`; test refs `crate::bot_core::v4_swap_decoder::{V4_SWAP_TOPIC, decode_v4_swap_log}` → `crate::v4_swap_decoder::{...}`.
   - `v3_mint_burn_decoder.rs` test: `crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC` → `crate::v3_swap_decoder::V3_SWAP_TOPIC`.
6. Add `crates/degenbot-decoders` to root `rust/Cargo.toml` `[workspace] members` and a `[profile.release.package.degenbot-decoders] codegen-units = 16` entry.
7. Run `cargo build -p degenbot-decoders --manifest-path rust/Cargo.toml` → GREEN.
8. Run `cargo test -p degenbot-decoders --manifest-path rust/Cargo.toml` → GREEN (decoder unit tests move with their files).
9. Run `just test-rust` → GREEN (bot still uses its own copies; nothing rewired yet).

Deliverable: new crate compiles + tests pass standalone; full suite still green.

---
# Slice 2: Rewire bot crate to the new crate + delete originals

1. `rust/crates/degenbot-bot/Cargo.toml`: add `degenbot-decoders = { path = "../degenbot-decoders" }` to `[dependencies]`.
2. Rewire imports in `rust/crates/degenbot-bot/src/`:
   - `bot_core/log_dispatcher.rs` lines ~23-26: `decode_sync_log` / `decode_v3_swap_log` / `decode_v3_mint_log` / `decode_v3_burn_log` / `decode_v4_swap_log` / `decode_v4_modify_liquidity_log` → from `degenbot_decoders::{v2_sync_decoder, v3_swap_decoder, v3_mint_burn_decoder, v4_swap_decoder, v4_modify_liquidity_decoder}`.
   - `bot_core/block_pump.rs` lines ~45-50: the six `*_TOPIC` imports → `degenbot_decoders::...`.
   - `bot_core/reorg_coordinator.rs` lines ~449,658: `use crate::bot_core::v3_swap_decoder::V3_SWAP_TOPIC;` and `use crate::bot_core::v4_swap_decoder::V4_SWAP_TOPIC;` → `use degenbot_decoders::{v3_swap_decoder::V3_SWAP_TOPIC, v4_swap_decoder::V4_SWAP_TOPIC};`.
3. Update `DecodedPoolEvent::V4Swap`/`V4Liquidity` field type `crate::bot_core::v4_swap_decoder::PoolId` → `degenbot_decoders::v4_swap_decoder::PoolId` (in `log_dispatcher.rs`).
4. Delete the 5 original decoder files from `degenbot-bot`:
   `git rm rust/crates/degenbot-bot/src/optimizers/v2_sync_decoder.rs rust/crates/degenbot-bot/src/bot_core/{v3_swap_decoder,v3_mint_burn_decoder,v4_swap_decoder,v4_modify_liquidity_decoder}.rs`.
5. `bot_core/mod.rs`: drop `pub mod v3_swap_decoder; v3_mint_burn_decoder; v4_swap_decoder; v4_modify_liquidity_decoder;` (lines ~31,33,34,36).
6. `optimizers/mod.rs`: drop `pub mod v2_sync_decoder;` (line ~23).
7. Hunt stragglers: `rg "bot_core::v[234]_(swap|mint_burn|modify_liquidity|sync)_decoder" rust/crates/degenbot-bot/src rust/src` → must be empty.
8. Run `just test-rust` → GREEN (all decoder behavior identical; tests moved with the files).
9. Run `just lint-rust` → GREEN.

---
# Slice 3: Root cdylib rewire + justfile no-pyo3 enforcement

1. `rust/Cargo.toml` (root) `[dependencies]`: add `degenbot-decoders = { path = "crates/degenbot-decoders" }`.
2. `rust/src/py_binding.rs` lines ~415,2869: `degenbot_bot::bot_core::v4_swap_decoder::PoolId` → `degenbot_decoders::v4_swap_decoder::PoolId`.
3. `rust/src/lib.rs`: update the re-export comment block: note decoders now live in `degenbot-decoders` and root reaches `PoolId` via the direct path dep (no `pub use` re-export needed — only `py_binding` consumes it).
4. `justfile` `check-no-pyo3-in-cores`: add `degenbot-decoders` to the `for crate in ...` list.
5. Run `just check-no-pyo3-in-cores` → "OK" (proves the new crate is pyo3-free).
6. Run `just lint-rust` → GREEN.
7. Run `just test-rust-python` → GREEN (PyO3-wrapped tests pass; `PoolId` path resolves).

---
# Slice 4: Docs sync + full validation

1. `rust/CONTEXT.md`: add a `degenbot-decoders` term entry ("Pure-alloy Uniswap V2/V3/V4 event-log decoders extracted from `degenbot-bot`; the `LogDecoder` trait + `DecodedPoolEvent`/`LogDispatcher` dispatch bus remain in `bot_core/log_dispatcher.rs`"). Update any `rust/crates/degenbot-bot/src/bot_core/v*_decoder.rs` path references.
2. `rust/AGENTS.md`: add a `#### degenbot-decoders` subsection to "Module Organization" with its file table; note it mirrors `degenbot-cl-math`'s alloy-only purity; update the `degenbot-bot` table to drop the 5 decoder rows.
3. `CONTEXT-MAP.md`: refresh the Rust Extension bullet to mention `degenbot-decoders`.
4. Run `just format` (rustfmt the new crate).
5. Run `just lint` → GREEN (rust + python).
6. Run `just test-all` → GREEN.
7. `git status` — confirm 5 files moved out of `degenbot-bot`, new crate added, no orphaned references.

## Status
[ ] Slice 1: scaffold crate + copy decoders
[ ] Slice 2: rewire bot crate + delete originals
[ ] Slice 3: root cdylib + justfile no-pyo3
[ ] Slice 4: docs + full validation
