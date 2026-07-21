# Onchain pool-state probe — seam consolidation

**Status:** slice A (hygiene) planned 2026-07-20; slice B (batch-probe
extraction) to be grilled separately. Recorded in [`CONTEXT.md`](../../CONTEXT.md)
under "Onchain pool-state probe."

## The finding

An architecture review (`/improve-codebase-architecture`, candidate ①) surfaced
that the deep home for onchain pool-state probing —
`rust/crates/degenbot-rpc/src/abi.rs` — **already exists**. It owns
`encode_*` / `decode_*` / `fetch_*` for every probe shape in the codebase:

| Probe | encode | decode | fetch |
|---|---|---|---|
| V2 reserves | ✅ | ✅ | ✅ |
| V3 slot0 / liquidity | ✅ | ✅ | ✅ |
| V4 slot0 / liquidity | ✅ | ✅ | ✅ |
| tick_bitmap / tick_data (V3 + V4) | ✅ | ✅ | ✅ |
| balance_of / allowance / total_supply | ✅ | ✅ | ✅ |

The reference adapter proving the seam is real is
`PyBotIo` (`degenbot-python/src/bot/py_bot_io.rs`): every `fetch_*` on `PyBotIo`
is a thin `degenbot_rpc::abi::encode_*` → call → `decode_*` delegation (13 call
sites). The pattern works.

**But four consumers circumvented the home and reinvented the primitives from
scratch:**

1. `solvers/arb_engine/diagnostic.rs` — `fn_selector`/`encode_call`/`build_v2/3/4_calls`/`decode_v2/3/4_results`/`uint_value`/`int_value_to_i32` (alloy `DynSolValue` directly, bypassing the sol! macro path the home uses).
2. `bot_core/liquidity_verifier.rs` — `encode_calldata`/`decode_uint256/128`/`decode_int128`/`decode_v3/v4_*_result`.
3. `pool-updater/src/verify.rs` — `ticks_calldata`/`tick_bitmap_calldata`/`int_selector_calldata`/`decode_ticks_return`/`decode_tick_bitmap_return` (V3 half only).
4. `aave/src/updater/verify.rs` — `decode_uint256_return`.

`encode_call` (diagnostic) and `encode_calldata` (verifier) are byte-identical
with different names — the literal duplication that surfaced the candidate.

## Slice A — hygiene (route the stragglers, delete the copies)

### Scope

Each straggler's encode/decode helpers route through the home; the reinventions
delete. The per-consumer build/decode orchestration (multicall3 batch loops,
`DiagnosticPoolState` / `VerificationMismatch` / `LiquidityDivergence`
assembly) **stays** — that's slice B's concern.

### Error shape — per-consumer adapter, home interface unchanged

The stragglers return consumer-specific errors:
`Result<FetchOutcome, ProviderError>` (diagnostic),
`Result<_, LiquidityVerifyError>` (verifier — distinguishes `Rpc` revert from
`Mismatch` short-return), `Result<_, RunError>` (pool-updater), `Option<_>`
(aave).

The home returns `ProviderResult<T>` (`Result<T, ProviderError>`), collapsing
the verifier's revert-vs-mismatch distinction into `DecodingError`. **This is
fine** because the distinction lives in `require_success` inspecting
`MulticallResult.success` *before* decode — decode is reached only on the
success path. A per-consumer `From<ProviderError>` adapter at the call site
maps `DecodingError → Mismatch` (verifier) / `RunError::Provider` (pool-updater)
etc., preserving exactly today's behaviour.

The home's interface is **not** extended with a richer `DecodeOrRevert` — that
would be slice-B-shaped work smuggled into slice A.

### Test discipline — migrate the independent oracle first

The home's `mod tests` carries the independent-oracle discipline (reference
vectors from `eth_abi` + `eth_utils.keccak`, a *different* encoder than
alloy's `sol!`), but **has no tests for `decode_tick_data` /
`decode_tick_bitmap` / `decode_v4_tick_*`** — exactly the gap the
`pool-updater/verify.rs` tests (`decode_ticks_return_matches_ref_encoder`,
`decode_tick_bitmap_return_matches_ref_encoder`, `ticks_calldata_positive_tick`,
`tick_bitmap_calldata_sign_extends_negative_word`, built with `cast keccak` +
hand-rolled `DynSolValue`) currently fill.

Migrating those independent-oracle tests to the home **fills a real coverage
gap**, not duplicates. Ordering is load-bearing:

1. **Red+Green commit (#1):** the migrated independent-oracle tests land in
   `degenbot-rpc/src/abi.rs`'s `mod tests` against the *existing* home decode.
   The home decode is proven correct before any straggler touches it.
2. **Reroute commits (#2–#5):** each consumer reroutes + deletes its copies,
   individually green, guarded first by #1's home tests.

### Out of scope for slice A

- **`extsload` (V4 storage-slot reads).** `pool-updater/verify.rs`'s V4 path
  probes `PoolManager` storage slots via `extsload(bytes32[])` (selector
  `0xdbd035ff`), NOT an ABI method call. The home has no extsload surface. This
  is a genuinely different probe mechanism and is **not** force-unified —
  deferred to slice B, where its shape decides whether it joins a `ProbeRequest`
  enum or stays a peer.
- **The batch-multicall orchestration.** What each straggler *also* reinvents
  is the cross-hop / all-ticks multicall3 batch build + heterogeneous-result
  decode — a layer the home's single-call `fetch_*` does not cover. That is the
  real deepening and lands as slice B.
- **`u256_to_hex`/`u128_to_hex`/`parse_hex_*`/`fmt_addr`/`fmt_u256` in
  `diagnostic.rs`.** These are the diagnostic *model's* serialization helpers
  (used by `HopRecompute`/`DiagnosticPathState` JSON, the recompute arms), not
  probe primitives. They belong to the `diagnostic.rs` concern split (architecture
  review candidate ②), not to slice A.
- **`require_success` (in diagnostic + verifier).** Per-consumer; it's the
  (α) adapter site mapping `MulticallResult.success`. Stays.

### ADR alignment

No conflict. ADR-003 names the onchain probe as cross-consumer infrastructure
("diagnostics, verification … all consume it without going through the solve
engine"); this decision *realizes* that for the single-call layer. No new crate
deps (all four straggler crates already depend on `degenbot-rpc`), no pyo3
in cores, no behaviour change.

## Slice B — batch-probe extraction (deferred indefinitely 2026-07-20)

The B-grilling surfaced that the three multicall3-batch shapes (diagnostic's
cross-hop heterogeneous, verifier's two-phase discover-then-verify, pool-updater's
mixed-type index-split) plus the V4 `extsload` single-`eth_call` path differ on
too many axes (dispatch mechanism, phase count, output type) to unify behind one
`ProbeRequest` enum without re-introducing the ADR-014 trap. See `CONTEXT.md`
"Onchain pool-state probe" for the full disposition + forcing functions.

**After slice A, the dangerous duplication (byte-identical encode/decode copies)
is fully eliminated.** The residual is structural scaffolding (~90 lines across
3 consumers: the build-vec → `multicall3_batch` → zip+decode-by-index loop with
per-consumer index bookkeeping). Not a bug-hiding class today. Revisit only on a
4th batch consumer or an index-split off-by-one.