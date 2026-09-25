# LFJ LBPair storage layout — UNVERIFIED-PENDING

> **STATUS: UNVERIFIED — DO NOT DECODE FROM THIS DOCUMENT.**
>
> No decoder, journal extraction, journal `PoolFamily` arm, or capability row
> for the LFJ (Trader Joe) Liquidity Book `LBPair` may land until every open
> question below is answered against the **real contract**. This file records
> the verification gap and the exact inputs the operator must supply; it
> deliberately asserts *no* event signature, *no* storage slot, and *no* bin
> formula, because a plausible-but-wrong slot layout silently produces false
> pool state — the worst failure mode this system can have (ADR-014 / ADR-059
> D2's no-guessing rule).

## Why this document exists

ADR-059 E3 makes LFJ binned liquidity the first genuinely new structure through
the pool-family kernel (`Structure::BinnedLiquidity`, `PoolFamily::LfjBinned`).
Its contract is the Liquidity Book `LBPair`: a pair of tokens plus a fixed
`bin_step` price granularity over *indexed bins* rather than Uniswap ticks. The
current chunk (ergo `QZ7ONQ`) owns the decoders + journal extraction. Both need
contract-level truth:

- **decoders** need the exact `LBPair` event signatures and indexed-parameter
  layout to build topic0 constants and data slicing;
- **journal extraction** needs the exact storage layout to read per-bin state
  from a touched `LBPair` via `eth_getStorageAt` / `extsload`.

Neither can be derived from this repository. The verification boundary for this
chunk was explicit: derive the layout authoritatively or stop and document. The
repo has no LFJ artifact to derive from, so the chunk stops here and documents.

## Evidence: what this repository actually contains

Verified by searching the working tree at HEAD `2ccb1f8ee`:

| Surface | State | Location |
|---|---|---|
| Binned-liquidity taxonomy arm | present | `rust/crates/foundation/degenbot-pools/src/pool.rs` (`Structure::BinnedLiquidity`, `BinnedLiquidityVariant::Lfj`) |
| Capability family + NONE rows | present, all-false | `rust/crates/foundation/degenbot-pools/src/capability.rs` (`Family::LfjBinned`) |
| Journal extraction arm | present, declines to decode | `rust/crates/engine/degenbot-simulation/src/sim/evm/journal_pools.rs` |
| DB kind / subclass table | present | `rust/crates/foundation/degenbot-db/src/{schema.rs,schema_head.sql}` (`lfj_pools`, kind `lfj_binned`); Python consumers use the typed `degenbot.db` mirror |
| Loud unsupported marker (D8) | present | `rust/crates/engine/degenbot-bot/src/connector_index.rs` (`unsupported_kind`) |
| **`LBPair` event ABI / topic constants** | **absent** | no `lfj_*` module in `rust/crates/foundation/degenbot-decoders/src/` |
| **`LBPair` deployed address** | **absent** | `src/degenbot/registry/deployments.json`, `rust/crates/foundation/degenbot-db/src/species.toml` carry no LFJ/Trader Joe row; the species shape test uses a placeholder factory `0x…dead` |
| **Verified `LBPair` source** | **absent** | `contract_reference/` holds only `uniswap/{V2,V3,V4}` and `aave/`; no `traderjoe/` |
| **Factory / creation-event data** | **absent** | no LFJ factory address, no creation-event topic |

A sweep for `trader`, `lbpair`, and `liquidity book` (case-insensitive, all
file types, excluding build/caches) returns only the taxonomy/manifest surfaces
table above plus ADR-059 and the ergo plan body — i.e. the repo knows the LFJ
family *exists* but holds zero contract-interface data.

The environment's configured RPC (`DEGENBOT_RPC_HTTP_CHAINID_1`) reaches
Ethereum mainnet (chain id 1, `cast` 1.7.1 available), but a live `cast`
cross-check is only possible once a **real deployed `LBPair` address** is known.
Locating such an address from outside this repository was explicitly out of
scope for this chunk, and guessing one would defeat the purpose of the check.

## Open questions (the operator asks)

Each item below is a question with the *shape* of the answer required. Nothing
is asserted; candidates named in parentheses are placeholders to be confirmed
or discarded, **not** values to implement against.

### A. Deployment identity

1. **Which chain(s) ship first?** The environment is configured for chain 1
   only (`DEGENBOT_RPC_HTTP_CHAINID_1`); the operator must name the target
   chain(s) and supply an RPC for each.
2. **`LBPair` factory address per chain** — the address whose logs seed pool
   discovery, and the canonical **factory creation event** signature + `indexed`
   layout (`PairCreated`/`LBPairCreated`-style; exact name and parameters TBD).
3. **Which Liquidity Book version?** The `LBPair` has shipped multiple deployed
   revisions with differing storage/event surfaces. The operator must pin the
   exact version/revision to target; this is a correctness input, not a nicety.
4. **A concrete `LBPair` address per chain** for fixture capture and layout
   cross-checking.

### B. Event shapes (required before any decoder lands)

For each event, the exact **canonical Solidity signature** (name + parameter
order + types) and **which parameters are `indexed`** are required; once the
canonical text is verified, `cast sig-event '<signature>'` computes topic0 and
`eth_getLogs` against a real pair confirms it.

1. **Swap** — the swap event consumed by journal extraction. Parameter list,
   indexed placement, and fee/amount fields TBD.
2. **Mint / Burn (per-bin liquidity)** — the add/remove-liquidity events. Whether
   one bin per event, a bin array per event, and the exact signature TBD.
3. **Sync / fee-accrual / bins-updated equivalent** — the event (if any) that
   reports accrued fees or a changed bin set, needed to keep extracted bins
   current between swaps.
4. **Any LP/receipt token event** (e.g. `Transfer`) emitted by `LBPair`, if the
   LP position representation matters to admission.
5. **Factory creation event** (see A2).

### C. Storage layout (required before journal extraction lands)

The journal extractor reads raw storage from a touched address. The following
must be resolved from the verified source (e.g. `forge inspect`), exactly as
`v4_poolmanager_storage_layout.md` does for the Uniswap V4 `PoolManager`:

1. **Top-level slot assignment.** The `LBPair`'s C3-linearized base contracts
   and the resulting slot index of every storage-bearing variable
   (immutables vs storage must be distinguished — immutables are in bytecode,
   not storage).
2. **Which top-level slot holds each piece of pair-wide state:**
   - active bin id;
   - token0 / token1 / factory / `bin_step` (where not immutable);
   - the root of the per-bin state structure.
3. **Per-bin state structure:** is it a `mapping` or an array (packed bin-array
   chunks)? What is the **key type** (e.g. `uint24` / `int24` / `int32` / a
   `bytes32` composed key)? What is the **value struct** and its field order,
   packing, and width — specifically where **liquidity** and **reserves X/Y**
   live, and where **fee accrual** (cumulative / per-bin owed fees) lives?
4. **Bin-position / tree index:** the structure that resolves the next non-empty
   bin (a position array plus a search tree), its shape, base slot, and key
   derivation.
5. **Mapping slot-derivation base(s).** For every mapping, the exact base slot
   and the hashing convention: `keccak256(abi.encode(key, base))` vs
   `keccak256(key ‖ base)`, and the key sign-extension rule for signed keys
   (int24/int32 keys must be sign-extended to 256 bits before hashing, or the
   computed slot diverges for negative bins).
6. **Read strategy.** Whether a public per-bin view getter exists (prefer it),
   or raw `eth_getStorageAt` / batched `extsload` is required.

### D. Bin indexing scheme

1. **Bin id type and range**, and how the active bin id is stored.
2. **Price formula** — the exact integer expression (base/precision order with
   the decimals adjustment and `bin_step`). The formula must come from the
   verified source, not from memory or this doc.
3. **token0/token1 ordering and decimal scaling** as they affect decoded bin
   price.

## How to close each question (verification procedure)

Preferred, and the pattern the repo already uses for Uniswap V4 — vendor the
**verified deployed source** under `contract_reference/traderjoe/` (mirroring
`contract_reference/uniswap/V4/PoolManager.sol`), then:

1. derive slot assignment with `forge inspect` from that source;
2. capture one real `Swap` / `Mint` / `Burn` log from a supplied `LBPair`
   address and record it as a decoder fixture;
3. cross-check every candidate slot with `cast storage` / `cast call` against
   the live pair and assert the decoded value is non-degenerate (not all-zero,
   bin id in range, reserves consistent with a `Swap` event from the same block).

Alternative when only an ABI is available: operator supplies the verified ABI +
a real address; topic0s come from `cast sig-event '<canonical text>'` and are
then confirmed against `eth_getLogs` output for that pair.

A layout that cannot be cross-checked against a real pair is **not** verified
and must not land.

## Consequence until closed (the shipped honest state)

The following are the current, test-pinned truths and must not change until the
questions above are answered:

- `rust/crates/foundation/degenbot-decoders/src/` exports **no** LFJ module or topic
  constant.
- `PoolFamilyTag::from_identity(Identity::BinnedLiquidity { .. })` returns
  `None` — pinned by
  `identity_projection_admits_only_the_structures_extraction_types`
  (`rust/crates/engine/degenbot-simulation/src/sim/evm/journal_pools.rs`).
- `capabilities(arm, Family::LfjBinned) == FamilyCapabilities::NONE` on both
  arms — pinned by `lfj_is_declared_unsupported_on_every_arm` and
  `lfj_identity_reaches_the_declared_lfj_family`
  (`rust/crates/foundation/degenbot-pools/src/capability.rs`).
- A touched `lfj_binned` pools row observes `family-unsupported:lfj_binned` —
  pinned by `load_tolerates_unsupported_pool_kind`
  (`rust/crates/engine/degenbot-bot/src/connector_index.rs`).
- The Python manifest declares `lfj_binned` unsupported with no invented
  deployment — pinned by `test_lfj_kind_is_declared_unsupported_until_a_tier_admits_it`
  (`tests/database/test_species_manifest.py`).

## Gap analysis — exact operator inputs that unblock this chunk

To lift the block, supply **all** of the following:

1. **A real deployed `LBPair` address** on the target chain, plus the target
   chain id and an RPC URL for it if not chain 1.
2. **The LFJ `LBPair` factory address** per target chain.
3. **The verified `LBPair` source code** (vendored into
   `contract_reference/traderjoe/`, as with the Uniswap/Aave references) **or**
   an equivalent verified ABI JSON plus the source revision identifier.
4. **Confirmation of the target Liquidity Book version/revision** (A3).
5. Optionally, one captured `Swap` (and `Mint`/`Burn`) log from (1) to seed
   decoder fixtures.

With (1)–(4), this chunk can land: `lfj_swap_decoder.rs` / `lfj_mint_burn_decoder.rs`
/ pair-created decoder with verified topic0 constants, the storage-layout doc
promoted from UNVERIFIED-PENDING to the `v4_poolmanager_storage_layout.md`
format, and a journal extraction arm that decodes bins from a known-pool set.
Until then, the capability rows stay `NONE` and the D8 marker stays loud.
