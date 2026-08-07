# Master Plan: Thread the CL-hop clamp (`consumed_inputs`) through the executor forward amounts

Status: **PLAN** (not implemented). Multiple workers may take small chunks after the
foundation task lands. Read this whole doc before editing any composer.

## TL;DR (what a worker needs)

The solver can over-feed a concentrated-liquidity (CL = V3/V4) pool: it commits an
input larger than the pool's max-convertible capacity, so the on-chain exact-in loop
cannot exhaust the input and **marches empty bitmap words to `MIN/MAX_SQRT_PRICE`** —
the path-5000 `EMPTY-HALT` (20.7M gas under a 5M ceiling). The fix is **Depth-2
Option-1**: an engine-side clamp makes `SolvePathResult.consumed_inputs[i]` the
*truthful executable input committed to hop `i`* (capped at `input_consumed - 1` for
over-fed CL hops). **Increment A (the clamp itself) is DONE and committed**
(`570a2d2c`). This plan covers **Increment B**: make the executor containers
actually feed `consumed_inputs[i]` into each CL pool instead of the previous hop's
full output (`hop_outputs[i-1]`), and thread `consumed_inputs` through the sim +
submission + Python driver so sim/replay/on-chain all see the clamped forward.

Do **NOT** re-implement the clamp, the solver, or the tier-3 oracle. They are done.
This is purely an **executor + data-threading** change.

---

## 1. Background

### 1.1 The bug (UO3JM4 / path-5000)

- Path 5000 = V2 MATIC/WETH → V4 UNI/MATIC (fee=100, ts=1) → V3 UNI/WETH, block
  25704509. The V4 hop (zfo=false, MATIC→UNI) has a single tracked band
  `[-257352, 35067]` at current tick 35050 — only ~17 ticks of headroom.
- The Möbius solver's int crossing walk (`mobius_v3_int.rs`) over-predicts
  `v4_simulate_swap` by a few wei on low-fee / `tick_spacing=1` V4 pools.
- Recorded: `v4_input = 15351327867212777`, `input_consumed = 15351327867192638`,
  leftover **20,139 wei**. Feeding the full `v4_input` at the executor's default
  price limit → **20,776,614 gas, EMPTY**. Clamping to `input_consumed − margin`
  → **190,755 gas, same ~460,882 output, clean stop** (march ends on
  `amountRemaining==0` at the last funded tick).

### 1.2 The chosen design: Depth-2 Option-1 (approved)

- The pure solver (`degenbot-solvers`) runs **lock-free** on its frozen
  `IntV3TickRangeSequence` (ADR-015: the guard drops before the rayon `par_iter`)
  and reports `consumed_inputs[i] = hop_outputs[i-1]` (the full forward) — it cannot
  see pool state, so it cannot clamp.
- Therefore the clamp is a **pool-state-aware reconciliation** that re-reads the live
  `V3PoolState`/`V4PoolState` from the core at the solve→result merge seam, exactly
  mirroring `resolve_path` (the input-side seam) on the output side.
- **Increment A (committed `570a2d2c`)**: `ArbitrageEngine::clamp_cl_hop_capacity`
  runs post-solve at all three seams (`rebuild_and_solve_affected`, `solve_all`,
  `register_and_solve_path`), clamps each over-fed CL hop's `consumed_inputs[i]` to
  `input_consumed - 1`, leaves `hop_outputs[i]` untouched (for an over-feeding CL
  pool `output(capacity) == output(over-feed)`).

### 1.3 Why the executor must change (the gap this plan closes)

`clamp_cl_hop_capacity` already makes `consumed_inputs` truthful, **but nothing
consumes it yet.** The executor containers read only `hop_outputs`:

```rust
// ComposerInputs (composers.rs:289) carries hop_outputs but NOT consumed_inputs:
pub struct ComposerInputs<'a> {
    pub executor_address: Address,
    pub pool_manager_address: Address,
    pub weth_address: Address,
    pub optimal_input: u128,
    pub hop_outputs: &'a [u128],
    pub opts: EncodeOptions,
}
```

Every `enc_v3_swap_compact` / `enc_v4_swap_compact` that feeds a CL pool draws its
`amount_specified` from `hop_outputs[i-1]` (the previous hop's full output). That is
the over-feed the clamp is supposed to prevent. Until the composer consumes
`consumed_inputs[i]`, the clamp is diagnostic-only and the on-chain fill still
marches.

---

## 2. Motivation (why we do the executor change at all)

1. **Correctness / prevention of the EMPTY-HALT.** Without the composer change, the
   clamped `consumed_inputs` never reaches the encoded stream, so the 20.7M-gas
   march is the on-chain reality regardless of the engine clamp.
2. **One honest source of truth.** `SolvePathResult.consumed_inputs` is documented as
   *"for V3/V4 hops, if the range boundary is hit, this may be less than the input"*
   — the clamp finally fulfils that docblock. The executor should read it.
3. **Removes `hop_outputs` double-duty.** Today `hop_outputs[i]` means both "hop i's
   predicted output" AND "forward amount into hop i+1". After this work,
   `consumed_inputs[i]` = executable input into hop i (clamped); `hop_outputs[i]` =
   predicted output of hop i (take/exit amount). This is the "one concept, one
   spelling" decoupling (AGENTS.md).
4. **Sim ↔ on-chain consistency.** The in-process revm sim (`simulate_path_on_evm`)
   encodes the same command stream the executor submits. If the composer clamps but
   the sim does not (or vice-versa), sim and submission diverge — the exact class
   ADR-020 tier-2 dual-driver parity exists to catch. **The clamp must flow to both.**

---

## 3. The core semantic: forward-in vs take/exit-out

### 3.1 The one rule to apply

For a CL hop at index `i`, the **swap-in amount** (`amount_specified` fed into the
pool) must be:

```
CL-swap-in(i) = consumed_inputs[i]
```

NOT `hop_outputs[i-1]`. For a fully-satisfied (non-over-fed) hop these are equal
(`consumed_inputs[i] == hop_outputs[i-1]`), so the change is a **no-op for the vast
majority of paths** — it only differs when the clamp engaged.

The **take / exit / forward-out** amounts stay on `hop_outputs`:
- The final hop's output (profit take) = `hop_outputs[last]`.
- The forward amount that flows *into a V2 pool* stays `hop_outputs` semantics (V2
  has no empty-march class; `consumed_inputs[i] == hop_outputs[i-1]` for V2, so
  either is identical — prefer to leave V2 forwards untouched to minimize diff).

### 3.2 Concrete example (the canonical path-5000 family `three_hop_v2_v4_v3`)

```rust
let out_a = hop_outputs[0];  // V2 output = the MATIC fed into V4
let out_b = hop_outputs[1];  // V4 output = UNI fed into V3 (exit)
...
// V4 swap-in (CL pool): currently out_a = hop_outputs[0]
enc_v4_swap_compact(c0_b, c1_b, fee_b, ts_b, zero_idx, hb.zfo, out_a)
//       CHANGE to: consumed_inputs[1]
// V4 take + V3 swap-in (exit CL pool): out_b = hop_outputs[1]  → unchanged (V3 exit)
enc_v4_take_compact(forward_b_idx, v3c_idx, out_b)
enc_v3_swap_compact(v3c_idx, hc.zfo, out_b, ...)
```

So in this family only **one** site changes: the V4 swap-in amount. The V3 exit is a
final hop — leave `out_b = hop_outputs[1]`. (If a path had a CL hop in the middle
feeding *another* non-final CL/V2 hop, that middle hop is the one whose swap-in uses
`consumed_inputs`.)

### 3.3 General rule per hop position

| Hop `i` in the path | Swap-in amount into hop `i` |
|---|---|
| First hop (`i=0`) | `optimal_input` (= `consumed_inputs[0]`, never clamped since it's the flash input) |
| CL hop (`V3`/`V4`, `i>0`) | **`consumed_inputs[i]`** ← the change |
| V2 / Curly / Balancer / Solidly hop (`i>0`) | `hop_outputs[i-1]` (unchanged — no march class) |

Notes:
- If hop `i` is CL but is the **final** hop, its `consumed_inputs[i]` is still the
  swap-in (use it), and its output take is `hop_outputs[i]`. A final CL hop that
  over-feeds is exactly the case we must clamp (its own output is the profit).
- `consumed_inputs[i]` for `i == 0` is always `optimal_input`; the clamp never
  changes index 0. So reads of `consumed_inputs[0]` are interchangeable with
  `optimal_input` — prefer reading `optimal_input` for the first-hop site.

---

## 4. Footgun warnings (read before editing ANY composer)

1. **`hop_outputs[i]` is overloaded — never mass-replace.** It means "hop i's
   output" AND (implicitly, at the call sites) "the forward into hop i+1". You must
   change **only the CL swap-in sites**, never export-wide. The exit/take amount of
   the final hop is `hop_outputs[last]` and MUST stay on `hop_outputs`. Grep the
   exact call site you are editing, not the whole function.

2. **Only CL pools (V3/V4) have the march class.** Do not touch V2/Curve/Balancer/
   Solidly swap-in amounts. For those, `consumed_inputs[i] == hop_outputs[i-1]`, so
   leaving them on `hop_outputs` is byte-identical and minimizes the diff. Changing
   them anyway is harmless but adds noise; follow the master plan's minimal-diff
   rule and leave non-CL sites alone.

3. **The sim and the submission MUST clamp together.** `simulate_path_on_evm`
   (backrun-strategy `simulator.rs`) calls `encode_cmd_stream` with the same
   `hop_outputs` — it builds the literal bytestream the executor runs. If you change
   the composer alone, the sim still encodes the unclamped forward (or vice versa)
   and they diverge. **Thread `consumed_inputs` into `SimulatePath`, `DispatchCandidate`,
   and the encoder call in the same change as the composer family**, and keep the
   sim's pre-encode int128 guard reading the same clamped input.

4. **The Python driver drops `consumed_inputs` today.** `src/degenbot/runner/
   dispatch.py` unpacks `(pid, inp, prof, ho, _ci, sb, sn)` and builds
   `DispatchCandidate` from `hop_outputs` only, discarding `_ci` (consumed_inputs).
   This is a second source of truth for the forward vector. It must pass
   `consumed_inputs` (as `ci`) into `DispatchCandidate` so the Rust seam can thread
   it. This is a small, high-leverage line.

5. **Golden-master parity tests will change.** `composers_parity.rs` (2-hop),
   `composers_3hop_parity.rs` (3-hop), `native_eth_3hop_bridge.rs`, and the
   `native_v4_*_path_ends.rs` files encode **byte-exact expected streams** derived
   from the Rust `enc_*` primitives. Any composer edit that changes an amount must
   be reflected in the corresponding test's expected bytes. **A "passing"
   golden test after a composer change is a RED flag — it means the test still
   encodes the old amount.** Follow RED→GREEN: break the golden vector first, then
   update it deliberately.

6. **Native-ETH / WETH-bridge shapes are the brittlest.** The 2-hop `v4_v4` /
   `v4_v3` / `v3_v4` and the `three_hop_v4_*` / `*_v4_*` families that bridge
   native ETH into/out of a V4 pool have `enc_weth_withdraw` / `enc_weth_deposit` /
   `V4_TAKE(native, self)` sequences where the forward amount appears in multiple
   opcodes. In those, identify the CL swap-in specifically — do not blanket-edit the
   WETH bridge amounts.

7. **`consumed_inputs` may be shorter than `hop_outputs` in degenerate cases.** Both
   have one entry per hop, but guard reads: use `.get(i)` and bail the site gracefully
   (the existing `encode_*` returns `Option` / `None`). Do not index-blanket.

8. **Fees/ts must already be threaded.** V4 identity carries `pool_key.fee` /
   `pool_key.tick_spacing`; V3 identity carries `fee` / `tick_spacing`. These are on
   `V4HopInfo` / `V3HopInfo` in the composers already — you do not need the core. The
   clamp ran at the engine; the executor only needs the **amounts**, not pool state.

---

## 5. The full threading path (what "Increment B" touches)

```
ArbitrageEngine.solve_* → clamp_cl_hop_capacity (Increment A, DONE)
  → self.results[pid] = SolvePathResult { consumed_inputs (truthful), hop_outputs, ... }
  → result_channel.rs / c_api: tuple (pid, opt_input, profit, hop_outputs, consumed_inputs, ...)
      → src/degenbot/runner/dispatch.py:  **CHANGE** stop dropping `_ci`; pass consumed_inputs
      → DispatchCandidate { hop_outputs, consumed_inputs (NEW) }        [dispatch.rs]
          → to_simulate_path → SimulatePath { ..., consumed_inputs (NEW) } [simulator.rs]
              → simulate_path_on_evm → encode_cmd_stream(..., consumed_inputs)  [sim]
              → list-independent C3 int128 guard reads consumed_inputs[i] for V4 hops
          → encode_cmd_stream / encode_cmd_3_hop(..., consumed_inputs)  [composers.rs]
              → ComposerInputs { ..., consumed_inputs: &[u128] (NEW) }  [composers.rs]
                  → each CL swap-in uses consumed_inputs[i]
```

Concretely, the NEW/NEW-field points:
- `ComposerInputs.consumed_inputs: &'a [u128]` (new field) — composers.rs
- `encode_cmd_stream(...)` + `encode_cmd_3_hop(...)` + `encode_execute_call(...)`:
  add a `consumed_inputs: &[u128]` param, build `ComposerInputs` with it.
- All `encode_cmd_stream` callers gain the arg: `simulator.rs`, `dispatch.rs`
  (`encode_execute_call` path), the three examples, and the parity test files.
- `SimulatePath.consumed_inputs: Vec<u128>` (new field) — simulator.rs
- `DispatchCandidate.consumed_inputs: Vec<u128>` (new field) — dispatch.rs
- Python `dispatch.py` unpacks and forwards `_ci` into `DispatchCandidate`.

---

## 6. Recommended implementation strategy (stage per family, RED→GREEN)

1. **Foundation task (highest priority, do first):** thread `consumed_inputs` through
   the plumbing (ComposerInputs, encode signatures, SimulatePath, DispatchCandidate,
   Python driver) with `consumed_inputs` accepted but **semantics unchanged** — i.e.
   every CL swap-in still reads `hop_outputs[i-1]` so all goldens stay green. This
   lands the data path with zero behavior change and unblocks every family task.
2. **One reference family — `three_hop_v2_v4_v3`** (the path-5000 shape): implement
   the semantic flip end-to-end, update its golden vector deliberately (RED first),
   and add an assertion that reads `consumed_inputs` (verifying the clamp reach
   through the whole chain incl. sim). Establish the pattern.
3. **Per-family tasks:** each subsequent family (2-hop CL + 3-hop CL) is a small
   RED→GREEN flip. Group clause: prefer one task per family (or per closely-similar
   pair) so each is independently goldens-gated and reviewable.

### 6.1 Backlog decomposition

| Task | Family / scope | CL swap-in sites | Reference |
|---|---|---|---|
| Foundation | Plumbing: ComposerInputs + encode sigs + SimulatePath + DispatchCandidate + Python | none (noop) | §5 |
| Reference | `three_hop_v2_v4_v3` | `out_a` → `consumed_inputs[1]` | §3.2, §6.2 |
| 2-hop L1 | `encode_cmd_v2_v4`, `encode_cmd_v3_v4` | fwd-in → `consumed_inputs[1]` | |
| 2-hop L2 | `encode_cmd_v4_v4`, `encode_cmd_v3_v3` | fwd-in(s) → `consumed_inputs[1]` (and `[1]`→`[1]`) | |
| 2-hop L3 | `encode_cmd_v4_v3`, `encode_cmd_v3_v2`, `encode_cmd_v4_v2` | per CL hop | |
| 2-hop L4 | `encode_cmd_v2_v3` | fwd-in → `consumed_inputs[1]` | |
| 3-hop A | `three_hop_v2_v2_v3/v4`, `three_hop_v2_v3_v3/v4` | hop2 CL swap-in → `consumed_inputs[2]` | |
| 3-hop B | `three_hop_v2_v3_v2`, `three_hop_v2_v4_v2` | hop1 CL swap-in → `consumed_inputs[1]` | |
| 3-hop C | `three_hop_v2_v4_v4`, `three_hop_v2_v3_v4` (covered), `three_hop_v3_*` | per CL hop | |
| 3-hop D | `three_hop_v4_*` (native-bridge-heavy) | per CL hop, watch WETH bridge | §4.6 |
| Sim+dispatch verify | end-to-end: sim encodes clamped, DispatchCandidate threads, Python driver | — | §4.3 |

This is the *shape*; the ergo backlog under this plan lists each concrete task.

---

## 7. Validation gates

Every task (including the foundation) must keep these green; the per-family flips
carry their own golden gate.

- **Golden byte-stream parity:** `cargo test -p degenbot-executor` — every
  `composers_parity.rs` / `composers_3hop_parity.rs` / `native_*` vector must be
  **deliberately updated** (RED→GREEN) when a composer amount changes.
- **Crate build/lint:** `cargo build -p degenbot-executor -p degenbot-backrun-strategy
  -p degenbot-bot -p degenbot` and `cargo clippy -p ...` clean.
- **Engine clamp (unchanged behavior):** `cargo test -p degenbot-bot
  clamp_cl_hop_capacity` stays green — the clamp increment is committed and must not
  regress.
- **Sim encodes the clamped forward (the end-to-end proof):** after the reference
  family + sim threading, feed the path-5000 clamped result through
  `simulate_path_on_evm` and assert the encoded stream uses `consumed_inputs[1]` for
  the V4 swap-in (not `hop_outputs[0]`), and that the sim terminates cleanly under
  the gas ceiling.
- **Tier-2 dual-driver parity (ADR-020):** `cargo test -p degenbot` keeps the parity
  pairs GREEN — a lossy FFI seam (arg ordering, sign, rounding) surfaces here.
- Deferred to the umbrella: full `just test-all` + `just lint` before merge (the
  pre-push hook enforces this).

---

## 8. Key files

- `rust/crates/degenbot-executor/src/composers.rs` — the containers.
- `rust/crates/degenbot-executor/src/encoders.rs` — `enc_v3_swap_compact` /
  `enc_v4_swap_compact` / WETH-bridge primitives (read-only for us).
- `rust/crates/degenbot-executor/tests/{composers_parity,composers_3hop_parity,
  native_eth_3hop_bridge,native_v4_v2_mixed_path_ends,native_v4_v2_v4_path_ends,
  native_v4_v3_v4_path_ends}.rs` — golden-master gates.
- `rust/crates/degenbot-backrun-strategy/src/{dispatch,simulator}.rs` — candidate /
  sim threading.
- `src/degenbot/runner/dispatch.py` — Python driver (stop dropping `_ci`).
- `rust/crates/degenbot-bot/src/solvers/arb_engine/solver_dispatch.rs` — the DONE
  clamp (`clamp_cl_hop_capacity`, `cl_hop_clamp_margin`).

---

## 9. Decisions already made (do not re-litigate)

- **Margin = 1 wei** (VAASFM), applied as `input_consumed.saturating_sub(margin)` in
  `exact_input_clamp_bound` / `clamp_cl_hop_capacity`. Env overridable via
  `CLAMP_MARGIN` for sweeps.
- **Clamp location = engine post-solve merge seam** (`degenbot-bot`), NOT the pure
  solver (lock-free ADR-015 cannot reach pool state). Increment A is committed.
- **Rotation / direction:** keep the executor's default min/max price limit by
  direction — NO executor redeploy of the price-limit handling. We clamp the *amount*
  fed, not the limit.
- **No backwards-compat layer** (AGENTS.md): `consumed_inputs` becomes authoritative;
  do not preserve a legacy path that re-derives forwards from `hop_outputs`.
- **The executor needs only amounts, not pool state** — the clamp already ran; the
  containers just thread `consumed_inputs` through.
