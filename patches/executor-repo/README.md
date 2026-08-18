# Executor-repo patch package — TGUZCT (batch × ERC6909 capture)

Prepared 2026-08-18 from the degenbot in-repo copy (`tier3-oracle/src-executor/cmd_executor.vy`)
while `/workspaces/executor` is read-only in this devcontainer. Apply in a session where the
executor repo is writable.

## Contents

| File | Purpose |
|---|---|
| `0001-sync-to-degenbot-u3wvll-copy.patch` | **Sync**: brings `contracts/cmd_executor.vy` from its current state to the degenbot in-repo copy — which carries the U3WVLL generation (self-read `_combined_balance`, unconditional `after >= before` assert, `expected_value` ignored, mode-3 SWEEP) currently *absent* from the executor repo. **Operator-confirmed 2026-08-18: the executor repo is intentionally behind — apply this patch.** The degenbot copy is the source of record: the committed artifacts build from it and pass `verify-tier3-executor-artifact` (BHL2R2). |
| `0002-tguazct-open-weth-batch.patch` | **The TGUZCT feature** (on top of 0001): `V4_BATCH_OPEN_WETH` (0x43) + named `InsufficientMintDelta` fail-fast. |
| `artifacts/` | Artifacts compiled HERE from 0001+0002 (vyper 0.5.0a3 on python 3.14.6; baseline compile of the unmodified source reproduced the committed artifacts byte-for-byte across all six files). Convenience for diffing/speed — **the of-record build is the executor repo's own toolchain** (`uv run vyper` in `/workspaces/executor` per `tier3-oracle/build-tier3-executor-harness.sh`). |
| `derive_artifacts.py` | The combined_json → artifact extraction used here (same logic as the build script's python step). |

## Apply + verify (executor-repo session)

```bash
cd <executor-repo>
git apply degenbot/patches/executor-repo/0001-sync-to-degenbot-u3wvll-copy.patch   # if sync is wanted/greenlit
git apply degenbot/patches/executor-repo/0002-tguazct-open-weth-batch.patch
uv run vyper -f combined_json contracts/cmd_executor.vy   # pinned 0.5.0a3; must compile clean
# rebuild/publish artifacts per the repo's usual flow, then in degenbot:
just verify-tier3-executor-artifact                        # compile-vs-use gate (BHL2R2)
```

## What 0002 changes

1. **`COMMAND_V4_BATCH_OPEN_WETH = 0x43`** — identical layout to `V4_BATCH` (0x42: `[0x42][num:1][entry:20B × N]`). `_cmd_v4_batch(data, offset, open_weth: bool)`: the tail still settles native, and still settles a **negative** WETH delta (the PM must always be repaid); with `open_weth=True` it skips the `take` of a **positive** WETH delta, leaving it open. The trailing `V4_MINT_COMPACT` then converts it (credit-before-debit, debiting the live delta back to zero before `PM.unlock`).
   - **Pairing invariant:** an open positive WETH delta without a consuming mint reverts at `PM.unlock` (v4-core `CurrencyNotSettled`; the stub harness models it as `"DELTA"`). The degenbot encoder's ledger validator makes that stream unrepresentable (permanent Rust-side rule) — the artifact alone does not forbid it.
   - **No immutable/ABI-surface change** for deployment: `code_layout` (immutables) is byte-identical to the baseline; deploy args unchanged. The JSON ABI gains one `error` entry (`InsufficientMintDelta`) only.
   - Size: creation 15851 → 16087 B (+236), runtime 15671 → 15907 B.
2. **`InsufficientMintDelta(actual, expected)` named error** in `V4_MINT_COMPACT` (defense in depth, option C): a pre-check `_read_pm_delta(currency) >= amount` before the `PM.mint` extcall, so a starving sequence (e.g. a full-settle `V4_BATCH` before the mint) reverts with a NAMED executor error instead of the PoolManager's opaque `D0`. Cost: one warm transient load per mint (~100 gas).

## Runtime evidence (probed in degenbot this session, patched artifact × stub PM harness)

Real patched bytecode was compiled and temporarily swapped into `tier3-oracle/artifacts/executor/`, then executed through the declarative harness (3 probes, all green, then reverted):

- **A (the feature):** WETH→t→WETH, entry 100 000, `use_v4_batch` + `erc6909_profit` (the combination SMOZG3 proved unexecutable on the baseline bytecode, reverting PM `D0`): executes green with `check_mode=2` floor armed; profit lands in the executor's ERC6909 claim (`assert_erc6909_capture`, 0.1% pattern; custody WETH does not double-carry).
- **B (named fail-fast):** the same stream with the batch opcode flipped 0x43→0x42 (full-settle, the pre-TGUZCT starvation stream) reverts with the **named** `InsufficientMintDelta` selector (keccak256 of `InsufficientMintDelta(uint256,uint256)`), not opaque `D0`.
- **C (the hazard):** open-weth batch with the mint + trailing settle-all removed — the open positive WETH delta reaches `PM.unlock` and reverts with the stub's `_checkDelta` `"DELTA"` (models v4-core `CurrencyNotSettled`).

## degenbot Rust-side handoff (lands WITH the artifact re-sync, same session)

The committed degenbot encoder still **declines** `use_v4_batch` × `erc6909_profit` on a WETH terminal (`grammar_shape::erc6909_batch_capture_declines`, interim, tagged in the docstrings). Once the artifact carrying 0x43 is re-synced into degenbot
(`tier3-oracle/artifacts/executor/` + `contracts/cmd_executor_runtime_bytecode.txt`), the Rust flip is:

1. `PlanStep::V4Batch` gains an `open_weth: bool` (or a sibling variant) → encodes 0x43 vs 0x42.
2. The two walker decline sites emit the open-weth batch for the capture combo (transform probed this session).
3. **Ledger pairing rule:** `V4Batch{open_weth} ⇒ immediately-following V4Mint{currency: WETH}` (new gate op) — makes the probe-C hazard unrepresentable.
4. `encode_intake`: the batch×erc6909 cell flips from declined → pins the 0x43 bytes; `glopcn_bytepin` must stay green (no other bytes change).
5. Declarative matrix: the decline test `erc6909_capture_with_batch_declines_unexecutable_combo` becomes its converse (executes + captures); the opt-matrix cell flips to builds-and-validates.
6. ADR-034 amendment: interim decline superseded; doc-strings read "current executor artifact" until the flip.
