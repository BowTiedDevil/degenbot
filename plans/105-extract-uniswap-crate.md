# Design & scope — degenbot-uniswap extraction
## Overview

Extract two pure Uniswap-protocol-domain modules out of `degenbot-bot` into a
new workspace crate `degenbot-uniswap` (`rust/crates/degenbot-uniswap/`):

1. **`dex_identity.rs`** (539 LoC) — `DexIdentity` / `DexVariant` /
   `ReservesAbi` value objects + `pub const` per-DEX presets (factory,
   deployer, CREATE2 init hash, fee params). Imports **only**
   `alloy::primitives::{address, b256, Address, B256}` — zero `crate::`
   references. Plan 103's target diagram explicitly placed `dex_identity` in
   `degenbot-core`, but it was never moved; `rust/AGENTS.md`'s core-crate table
   omits it (the move was dropped). This lands it — in `degenbot-uniswap`
   rather than `degenbot-core`, because DEX presets are Uniswap-V2-domain data,
   not "foundational utilities."
2. **`v2_encoding.rs`** (144 LoC) — `EncodedCall`, `V2_SWAP_SELECTOR`, and
   `encode_v2_swap()`. Imports `alloy::primitives`, `degenbot_abi`
   (`abi_encoder::encode_rust`, `abi_types::AbiValue`), and
   `degenbot_core::errors::AbiDecodeError`. Uniswap V2 swap callldata encoding
   is protocol-domain, not engine state — it belongs with the identity
   presets, not inside the 3,422-line `bot_core/mod.rs`.

Net: `degenbot-bot` loses ~683 LoC of pure domain data it was only carrying as
a re-export conduit (`dex_identity` has **zero** internal bot consumers —
verified; `bot` held it solely so root's `py_dex_identity.rs` could reach it).
The standalone-Rust-core claim: a consumer building a Sushiswap V2 pool can
look up its factory/init-hash/fees and encode a V2 swap call **without**
pulling tokio / the engine / the pump.

## Problem

### Deletion test

If you collapsed this crate back into `degenbot-bot`: (a) Plan 103's deferred
`dex_identity` move stays unfinished (the core-crate table keeps silently
contradicting the target diagram); (b) `dex_identity` keeps masquerading as
`bot_core` state when it is in fact pure protocol data with no `BotState`
coupling; (c) a standalone consumer re-pulls the engine stack to look up a
factory address. The crate earns its keep by relocating already-leaf code —
not a new abstraction (no new trait), so it does not trip ADR-003's
sample-of-one guardrail.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Plan 103 left `dex_identity` in the wrong crate | `bot_core/dex_identity.rs` + `AGENTS.md` core table omission | The target diagram (core) and the reality (bot) disagree; deferred cleanup |
| `dex_identity` carried only as re-export conduit | `bot_core/mod.rs:20` `pub mod dex_identity;` (zero internal `use`) | `degenbot-bot` drags Uniswap-V2 presets for `py_dex_identity`'s benefit alone |
| V2 encoding buried in 3,422-line `bot_core/mod.rs` | `bot_core/mod.rs:16,30,1480-1485` + `v2_encoding.rs` | Encoding is protocol-domain, not state; living in `mod.rs` inflates the engine crate |
| Standalone claim half-honored | `rust/CONTEXT.md` "standalone Rust consumer" | Can't construct a `DexIdentity` or encode a V2 swap without the engine crate |

## Solution

### What moves → `degenbot-uniswap`

| File (from) | LoC | Moves as | Public surface |
|-------------|-----|----------|----------------|
| `bot_core/dex_identity.rs` | 539 | `src/dex_identity.rs` | `DexIdentity`, `DexVariant`, `ReservesAbi` (+ any associated enums/types), `pub const` presets |
| `bot_core/v2_encoding.rs` | 144 | `src/v2_encoding.rs` | `EncodedCall`, `V2_SWAP_SELECTOR`, `encode_v2_swap` |

Crate deps: `alloy`, `degenbot-abi` (for `v2_encoding`'s encoder/`AbiValue`),
`degenbot-core` (for `v2_encoding`'s `AbiDecodeError`). Both are downward edges
already used elsewhere. **No** `pyo3`, **no** `tokio`, **no** `degenbot-rpc`/
`degenbot-bot`/`degenbot-decoders`. This crate is independent of Plan 104's
`degenbot-decoders` (decoders are alloy-only and do not consume identity/
encoding; identity/encoding do not consume decoders).

### Consumer rewires (bot crate)

- `bot_core/mod.rs:16`: `use crate::bot_core::v2_encoding::{encode_v2_swap, EncodedCall};`
  → `use degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};`
- `bot_core/mod.rs:20`: drop `pub mod dex_identity;` (no internal consumer —
  `DexIdentity`/`DexVariant`/`ReservesAbi` are not referenced anywhere in
  `degenbot-bot/src` outside `dex_identity.rs` itself).
- `bot_core/mod.rs:30`: drop `pub mod v2_encoding;`.
- `degenbot-bot/Cargo.toml`: add `degenbot-uniswap = { path = "../degenbot-uniswap" }`.

### Consumer rewires (root cdylib)

- `Cargo.toml` (root) `[dependencies]`: add `degenbot-uniswap = { path = "crates/degenbot-uniswap" }`.
- `Cargo.toml` `[workspace] members`: add `crates/degenbot-uniswap`; add
  `[profile.release.package.degenbot-uniswap] codegen-units = 16`.
- `src/py_dex_identity.rs:26`: `use degenbot_bot::bot_core::dex_identity::{DexIdentity, DexVariant, ReservesAbi};`
  → `use degenbot_uniswap::dex_identity::{DexIdentity, DexVariant, ReservesAbi};`
  (also update the module doc-link at `py_dex_identity.rs:1`).
- `src/lib.rs`: update the re-export Comment if it mentions
  `dex_identity`/`v2_encoding` living in bot_core (no `pub use` re-export added
  — root reaches the crate via the direct path dep).

### justfile

- `check-no-pyo3-in-cores`: add `degenbot-uniswap` to the loop list.

## Files Involved

**Primary:**
- `rust/crates/degenbot-uniswap/Cargo.toml` — new (deps: `alloy`,
  `degenbot-abi = { path = "../degenbot-abi" }`,
  `degenbot-core = { path = "../degenbot-core" }`; standard deny-warnings lints).
- `rust/crates/degenbot-uniswap/src/lib.rs` — new; `pub mod dex_identity; pub mod v2_encoding;`
  + crate doc: "Uniswap-protocol domain crate — DEX identity presets + V2 swap
  callldata encoding. No pyo3/tokio/rpc; depends on degenbot-abi (encoder) +
  degenbot-core (errors)."
- `rust/crates/degenbot-uniswap/src/{dex_identity,v2_encoding}.rs` — moved from `degenbot-bot`.

**Secondary:**
- `rust/crates/degenbot-bot/src/bot_core/mod.rs` — drop 2 `pub mod` decls + rewire `v2_encoding` import.
- `rust/crates/degenbot-bot/Cargo.toml` — add path dep.
- `rust/Cargo.toml` (root) — workspace member, path dep, release profile entry.
- `rust/src/py_dex_identity.rs` — rewire import + doc-link.
- `rust/src/lib.rs` — comment update.
- `justfile` — add `degenbot-uniswap` to `check-no-pyo3-in-cores`.
- `rust/CONTEXT.md`, `rust/AGENTS.md`, `CONTEXT-MAP.md` — module-table updates.

## Benefits

- **Locality**: Uniswap-V2 domain data (identity presets + swap encoding)
  colocated, separated from engine state.
- **Depth**: a ~683-LoC pyo3-free/tokio-free leaf with a shallow seam (value
  objects + one encode fn); the deep `BotState` machinery stays in
  `degenbot-bot`.
- **Leverage**: a standalone Rust consumer can resolve a DEX preset and encode
  a V2 swap without the engine — completes the ADR-005 standalone claim for
  identity + encoding.
- **Plan 103 completion**: lands the deferred `dex_identity` move and reconciles
  the `AGENTS.md` core-crate table with the target diagram.

## Risks

- **`dex_identity` was a re-export conduit**: dropping `pub mod dex_identity;`
  from `bot_core/mod.rs` could break a consumer reaching it via
  `degenbot_bot::bot_core::dex_identity`. Verified: only `py_dex_identity.rs`
  + a `lib.rs` comment use that path — both rewired in Slice 2/3. Mitigation:
  Slice 2 includes a straggler grep
  `rg "bot_core::dex_identity|bot_core::v2_encoding" rust/`.
- **`v2_encoding` import drift**: `bot_core/mod.rs:16` is the sole consumer;
  a missed rewire fails the bot build. Mitigation: same straggler grep.
- **No semantic change**: pure file move. A red `just test-rust` after Slice 2
  means a missed import, not a logic change.

## Relationship to Other Plans

- **Plan 104** (Extract `degenbot-decoders` crate): **independent but
  file-overlapping**. Both edit `bot_core/mod.rs`, root `Cargo.toml`, and
  `justfile`. Sequence them — do NOT parallelize. The two new crates do NOT
  depend on each other (decoders: alloy-only; uniswap: alloy+abi+core).
- **Plan 103** (rust-workspace-split, completed): this lands the deferred
  `dex_identity` move from 103's target diagram (relocated to a Uniswap-domain
  crate rather than `degenbot-core`, since DEX presets are protocol-domain).
- **Ergo epic `XQ5UX6` Slice 7** ("DEX subclass collapse — LiquidityPool + DexIdentity"): complementary — Slice 7 is the Python-side `DexIdentity` collapse; this plan is the Rust-crate home for the same value object. They share the `DexIdentity` concept but not files.

---
# Slice 1: Scaffold crate + copy modules (green, suite unaffected)

RED/GREEN: copy first so `degenbot-bot` keeps its originals and the suite stays green.

1. `mkdir -p rust/crates/degenbot-uniswap/src`
2. Create `rust/crates/degenbot-uniswap/Cargo.toml`: `name = "degenbot-uniswap"`, `edition = "2021"`, `publish = false`; `[dependencies]` = `alloy = { version = "^2.0" }`, `degenbot-abi = { path = "../degenbot-abi" }`, `degenbot-core = { path = "../degenbot-core" }`; standard deny-warnings `[lints]` block.
3. Create `rust/crates/degenbot-uniswap/src/lib.rs`: `pub mod dex_identity; pub mod v2_encoding;` + crate doc ("Uniswap-protocol domain crate — DEX identity presets + V2 swap callldata encoding. No pyo3/tokio/rpc.").
4. Copy (not move) `dex_identity.rs` and `v2_encoding.rs` from `degenbot-bot/src/bot_core/` into the new crate's `src/`. (No intra-crate path fixes needed — neither file uses `crate::` paths.)
5. Add `crates/degenbot-uniswap` to root `rust/Cargo.toml` `[workspace] members` and a `[profile.release.package.degenbot-uniswap] codegen-units = 16` entry.
6. Run `cargo build -p degenbot-uniswap --manifest-path rust/Cargo.toml` → GREEN.
7. Run `cargo test -p degenbot-uniswap --manifest-path rust/Cargo.toml` → GREEN (any `dex_identity`/`v2_encoding` unit tests move with their files).
8. Run `just test-rust` → GREEN (bot unchanged).

Deliverable: new crate compiles + tests pass standalone; full suite green.

---
# Slice 2: Rewire bot crate to the new crate + delete originals

1. `rust/crates/degenbot-bot/Cargo.toml`: add `degenbot-uniswap = { path = "../degenbot-uniswap" }` to `[dependencies]`.
2. Rewire `rust/crates/degenbot-bot/src/bot_core/mod.rs`:
   - line ~16: `use crate::bot_core::v2_encoding::{encode_v2_swap, EncodedCall};` → `use degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};`
   - drop `pub mod dex_identity;` (line ~20) and `pub mod v2_encoding;` (line ~30).
3. Delete the originals: `git rm rust/crates/degenbot-bot/src/bot_core/dex_identity.rs rust/crates/degenbot-bot/src/bot_core/v2_encoding.rs`.
4. Straggler grep: `rg "bot_core::dex_identity|bot_core::v2_encoding|crate::bot_core::dex_identity|crate::bot_core::v2_encoding" rust/` → must be empty (any hits are missed rewires).
5. Run `just test-rust` → GREEN.
6. Run `just lint-rust` → GREEN.

---
# Slice 3: Root cdylib rewire + justfile no-pyo3 enforcement

1. `rust/Cargo.toml` (root) `[dependencies]`: add `degenbot-uniswap = { path = "crates/degenbot-uniswap" }`.
2. `rust/src/py_dex_identity.rs:26`: `use degenbot_bot::bot_core::dex_identity::{DexIdentity, DexVariant, ReservesAbi};` → `use degenbot_uniswap::dex_identity::{DexIdentity, DexVariant, ReservesAbi};`
3. `rust/src/py_dex_identity.rs:1`: update the module doc-link `DexIdentity`](degenbot_bot::bot_core::dex_identity)` → `DexIdentity`](degenbot_uniswap::dex_identity)`.
4. `rust/src/lib.rs`: update the re-export comment block to note `dex_identity`/`v2_encoding` now live in `degenbot-uniswap` (no `pub use` re-export — `py_dex_identity` uses the direct path dep).
5. `justfile` `check-no-pyo3-in-cores`: add `degenbot-uniswap` to the `for crate in ...` list.
6. Run `just check-no-pyo3-in-cores` → "OK" (proves the new crate is pyo3-free).
7. Run `just lint-rust` → GREEN.
8. Run `just test-rust-python` → GREEN (`DexIdentity` path resolves through PyO3 wrappers).

---
# Slice 4: Docs sync + full validation

1. `rust/CONTEXT.md`: add a `degenbot-uniswap` term entry ("Pure Uniswap-protocol domain crate — DEX identity presets (`DexIdentity`/`DexVariant`/`ReservesAbi`) + V2 swap callldata encoding (`encode_v2_swap`/`EncodedCall`). Depends on degenbot-abi (encoder) + degenbot-core (errors); no pyo3/tokio."). Update path references in the `dex_identity` and `v2_encoding`/`EncodedCall`/`V2_SWAP_SELECTOR` entries.
2. `rust/AGENTS.md`: add a `#### degenbot-uniswap` subsection to "Module Organization" with its file table; remove `dex_identity` row from the `degenbot-bot` `bot_core/` table and `v2_encoding` from the `bot_core/` row.
3. `CONTEXT-MAP.md`: refresh the Rust Extension bullet to mention `degenbot-uniswap`.
4. Run `just format`.
5. Run `just lint` → GREEN (rust + python).
6. Run `just test-all` → GREEN.
7. `git status` — confirm `dex_identity.rs` + `v2_encoding.rs` moved out of `degenbot-bot`, new crate added, no orphaned references.

## Status
[ ] Slice 1: scaffold crate + copy modules
[ ] Slice 2: rewire bot crate + delete originals
[ ] Slice 3: root cdylib + justfile no-pyo3
[ ] Slice 4: docs + full validation
