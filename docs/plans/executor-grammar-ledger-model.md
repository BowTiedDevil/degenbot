# Executor Command Grammar — Ledger Dataflow Model

**Status:** spike output (ergo `DS4OQD`, epic `463V2C`).
**Spec tie-in:** [ADR-029](docs/adr/ADR-029-executor-command-grammar-axes.md) (D1 axes, D2 open ledger, D3 derived enclosure, D4 hybrid).
**Sources of truth read for this model:**
- `tier3-oracle/src-executor/cmd_executor.vy` — the VM semantics (authoritative ledger effects; this document is a cross-reference of that file's command handlers, not an independent oracle).
- `rust/crates/degenbot-executor/src/encoders.rs` — the `enc_*` builders + opcode constants + sentinel table.
- `rust/crates/degenbot-executor/src/grammar.rs` — the 36 hand-written family adapters that this model generalizes.
- `rust/crates/degenbot-simulation/src/harness/declarative.rs` — the runtime matrix driving/measurement (`run_chain`, `assert_profitable`).

A family is described by **one command stream** whose correctness is judged by actual execution in revm (ADR-029 D5), measured as `actual_delta == predicted_profit` on the executor's WETH balance (`ChainResult`).

---

## 1. The ledger set

A **ledger** is an accounting location a command reads/writes. ADR-029 D2 fixes five concrete instances today, plus an open interface for external Vault/lender:

| Ledger | Symbol | Meaning / sign |
|---|---|---|
| Executor ERC-20 balance | `E[token]` | executor's held ERC-20 balance of `token` (incl. WETH) |
| Executor native balance | `E[NATIVE]` | executor's ETH balance |
| PoolManager delta | `PM[token]` | transient delta at the PM for the executor; **positive = PM owes executor (credit), negative = executor owes PM (debt)** |
| ERC-6909 held balance | `F[token]` | executor's ERC6909 claim in the PM, id = `uint160(token)` |
| Pair handoff (V2 excess) | `H[pool,token]` | tokens deposited into a V2 pair but not yet in reserves — the pre-fund that a `V2_SWAP_CALC`/`V2_SWAP_DIRECT` consumes |
| *(external)* | `V[addr,…]` | a Balancer-shaped Vault ledger / Aave-shaped lender funding source (ADR-029 D2, stub in `VIXQYH`) |

The **ordering invariant** the grammar enforces is **credit-before-debit within a ledger** (D2): no command may debit a ledger that does not already hold the credit it consumes.

---

## 2. Opcode → ledger effect table

Authoritative per-command effect, transcribed from `cmd_executor.vy`. `+`/`−` are executor-relative credit/debit. **Bold preconditions** are the enforced invariants.

### ERC20 / ETH / native (0x10–0x17) — touch `E[]`, `E[NATIVE]`

| Opcode | Command | Ledger effect | Precondition |
|---|---|---|---|
| `0x10` | `ERC20_TRANSFER(tok, rcp, amt)` | `E[tok] − amt`; `rcp` gains | `E[tok] ≥ amt` (reverts `BAL`) |
| `0x11` | `ERC20_XFER_BALANCE(tok, rcp)` | `E[tok] − balance` (all) | `E[tok] ≥ 0` (no-op at 0) |
| `0x12` | `WETH_DEPOSIT(amt)` | `E[NATIVE] − amt`; `E[WETH] + amt` | `E[NATIVE] ≥ amt` (msg.value) |
| `0x13` | `WETH_WITHDRAW(amt)` | `E[WETH] − amt`; `E[NATIVE] + amt` | `E[WETH] ≥ amt` |
| `0x14` | `WETH_DEPOSIT_ALL` | `E[NATIVE] − all`; `E[WETH] + all` | — |
| `0x15` | `WETH_WITHDRAW_ALL` | `E[WETH] − all`; `E[NATIVE] + all` | — |
| `0x16` | `SEND_ETH(rcp, amt)` | `E[NATIVE] − amt` | `E[NATIVE] ≥ amt` |
| `0x17` | `SEND_ETH_ALL(rcp)` | `E[NATIVE] − all` | — |

### V2 (0x20–0x22) — touch `H[]` and `E[]`

| Opcode | Command | Ledger effect | Precondition |
|---|---|---|---|
| `0x20` | `V2_SWAP_COMPACT(pool, zfo, amt_out, rcp, fee, fwd)` | flash: pool sends `amt_out` **credit** to `rcp`; repayment in callback (nested `fwd` stream or 1-byte auto-pay → `E[input] − owed`). **Flash source pool candidate.** | repayment made within `fwd` (chain/nested ops) |
| `0x21` | `V2_SWAP_CALC(pool, zfo, rcp, fee)` | consumes `H[pool, input]` excess; pool sends `amt_out` (computed on-chain from excess) to `rcp`. **No callback.** | **`H[pool,input]` credit exists before** (must be pre-funded by a prior `ERC20_TRANSFER`/`V4_TAKE` to the pair) |
| `0x22` | `V2_SWAP_DIRECT(pool, zfo, amt_out, rcp)` | like `CALC` but `amt_out` explicit. **No callback.** | **`H[pool,input]` credit exists before** |

> **Terminal-V2 rule (subsumes `2PT5HH`):** a terminal V2 hop must be **pre-funded-then-`V2_SWAP_CALC`** (or `_DIRECT`), never an exact-out `V2_SWAP_COMPACT`, because the CL hop feeding it can deliver 1 wei below the solver forward and an exact-out over-draws → `UniswapV2: K`. In ledger terms this is **credit-before-debit on `H[pool,input]`** — the pair is credited (take/transfer) before the swap debits it.

### V3 (0x30–0x31) — touch `E[]`; flash credit via callback

| Opcode | Command | Ledger effect | Precondition |
|---|---|---|---|
| `0x30` | `V3_SWAP_COMPACT(pool, zfo, amt, rcp, fwd)` | swap; callback pays pool the input delta from `E[]` (auto-pay when `fwd` empty) or runs `fwd` stream. **Flash source pool candidate.** | repayment made in callback (auto-pay or `fwd`) |
| `0x31` | `V3_SWAP_DELTA(pool, zfo, rcp)` | input seeded from `PM[input]` delta (swap negative `amtSpecified` = −input_delta). | **`PM[input]` credit (positive delta) before** |

### V4 swaps (0x40–0x42) — touch `PM[]` (only inside `PM.unlock`)

| Opcode | Command | Ledger effect | Precondition |
|---|---|---|---|
| `0x40` | `V4_SWAP_COMPACT(pool, amt)` | exact-input; `PM[out] +`, `PM[in] −` | runs under unlock |
| `0x41` | `V4_SWAP_DYNAMIC(pool)` | input from `PM[input]` delta | **`PM[input] ≥ 0`** |
| `0x42` | `V4_BATCH(entries)` | multi-swap + auto-settle native/WETH | all net currencies resolve to WETH/native; runs under unlock |

### V4 settlement / ERC6909 (0x50–0x59) — touch `PM[]` and `F[]`

| Opcode | Command | Ledger effect | Precondition |
|---|---|---|---|
| `0x50` | `V4_UNLOCK(fwd)` | enters `PM.unlock` context; runs `fwd` as `unlockCallback` | context delimiter, not a ledger op |
| `0x51` | `V4_TAKE(cur, rcp, amt)` | `PM[cur] − amt`; `rcp` gains `amt` | **`PM[cur] ≥ amt` (credit before take)** ← **D0 invariant** |
| `0x52` | `V4_TAKE_COMPACT(cur, rcp, amt)` | same as `0x51`, compact | **`PM[cur] ≥ amt`** ← **D0 invariant** |
| `0x53` | `V4_TAKE_DELTA(cur, rcp)` | take **full positive** `PM[cur]` (no-op if `≤ 0`) | implicitly positive delta (fail-closed no-op) |
| `0x54` | `V4_SYNC(cur)` | tells the PM to track `cur` reserves; **not** a delta change | warm-up for settle/take on that currency |
| `0x55` | `V4_SETTLE` | settles the *pending* currency: `E[cur] − owed` → PM (zeroes the negative delta) | **negative `PM[cur]`** + `E[cur] ≥ owed` |
| `0x56` | `V4_SETTLE_DELTA(cur)` | auto-settle one currency: if `PM[cur] < 0` → `E[cur] −` to PM; if `> 0` → take to executor | sign-dependent |
| `0x57` | `V4_SETTLE_ALL` | auto-settle all nonzero deltas (native, WETH, then table addrs): negative → settle from `E[]`, positive → take to executor | covers the whole `PM[]` |
| `0x58` | `V4_MINT_COMPACT(cur, rcp, amt)` | convert `PM[cur]` credit into `F[cur]` for `rcp` (no physical transfer) | **`PM[cur] ≥ amt`** ← **D0 invariant** (credit before mint) |
| `0x59` | `V4_BURN_COMPACT(cur, amt)` | burn executor's `F[cur]`, adding `PM[cur] + amt` | `F[cur] ≥ amt` |

**Sign-convention note (L7, from the contract):** V4 `amountSpecified` is **negative** (exact-output); the compact `enc_v4_*` accept a positive magnitude and negate internally. V3 is **positive** (exact-input). This matters for the derivation but is a per-command wire detail the encoder methods absorb (ADR-029 D5).

---

## 3. The two real bug classes as ledger precondition violations

Both historically-real bugs are **credit-before-debit violations**, now expressible purely in ledger terms:

1. **D0 / take-before-credit (`v2_v2_v4`, `v2_v4_v4`).** The old payload emitted `V4_TAKE_COMPACT(WETH,…)` before any swap created a positive `PM[WETH]`; v4-core `require(cur > 0)` → `"D0"`. **Invariant:** *no `V4_TAKE*`/`V4_MINT*` may debit `PM[cur]` unless a prior swap/settle created `PM[cur] ≥ amt`.* The fix (`e7182b95`) changed the **funding source** of these families to self-fund the leading V2 hop so the deficit is never netted through an early WETH take.
2. **Terminal exact-out V2 over-draw (`2PT5HH`).** A terminal `V2_SWAP_COMPACT` (or `_DIRECT`) with the raw solver `hop_outputs[last]` draws from `H[pool,input]` more than the CL feeder delivered (1 wei) → `UniswapV2: K`. **Invariant:** *a terminal V2 must `H[pool,input]`-credit-then-`V2_SWAP_CALC`.* The one-line rule fixes every affected family (V2/SWAP_CALC + settle discipline), so `2PT5HH` is subsumed by the model (WE45KC).

These are the two invariants the GCC6I6 ledger-validator must make **unrepresentable**.

---

## 4. Axis-derivation rubric (how to read a family's axes off its stream)

Given a command stream, derive:

- **funding source** — determined by the **outermost command** and the **first ledger credit**:
  - outer `V2_SWAP_COMPACT`/`V3_SWAP_COMPACT` with `fwd` carrying the rest → the seed is **flash-borrowed from that pool** (pool-flash). **Flash source pool = the pool whose swap is the outermost callback.**
  - first real op a `ERC20_TRANSFER(WETH, pool, optimal_input)` prefunding a `V2_SWAP_CALC`/`_DIRECT`, or a leading `V3_SWAP_COMPACT` whose callback **repays from a pre-held `E[WETH]`** → **self-fund (executor WETH)**.
  - outer `V4_UNLOCK` with no external prefund → **PM-ledger (no-prefund V4)**: the PoolManager delta accounting carries the entry credit.
- **profit capture** — the terminal excess destination. In every current WETH→WETH family the excess lands in `E[WETH]` (mode-1 check), or `F[WETH]` when the `erc6909_profit` opt mints (only `v4_v4`/`v4_v4_v4` honor the opt today — that is exactly the `WE45KC` generalization).
- **builder bribe** — *never* encoded in a family's stream; it is a `config` ABI axis (bribe_bips/bribe_recipient_idx) applied at the `execute()` boundary (ADR-029 D1). So every family row below reads `bribe = none (config axis)`. WE45KC adds at least one matrix row that pays a bribe.
- **hop coupling** — per boundary `(out_i → in_{i+1})`:
  - `pool→pool` — hop `i`'s recipient is hop `i+1`'s pool (V2 via `V2_SWAP_CALC` recipient; V3 via `V3_SWAP_COMPACT` recipient);
  - `exec` — output to `E[]`/SELF, then an `ERC20_TRANSFER`/`ERC20_XFER_BALANCE` bridges it to the next;
  - `pm` — output to `PM[]` via `V4_SYNC`+transfer+`V4_SETTLE`, consumed by a V4 swap;
  - `take` — a `V4_TAKE_COMPACT(cur, next_pool)` sends `PM[cur]` credit straight to the next pool (`H[]`/exec).
- **repayment pivot** — for flash streams, the hop/mechanism that settles the borrowed ledger: the `ERC20_TRANSFER(WETH, flash_pool, optimal_input)` in the outer callback (V2/V3 flash); for V4 streams, the `settle_all`/`settle_delta` that covers the negative `PM[WETH]`.
- **in-path vs off-path flash** — **all 36 current families use only in-path flash** (the borrowing pool is also a hop). Off-path flash (a pool borrowed from purely for its capital, outside the hop chain) is a modeled axis value but has **zero current rows** — it first appears in `VIXQYH` (Aave-lender-shaped stub) and is a live option for the terminal-V2-off-path capture idea.

---

## 5. Family annotations (36)

Legend — **Funding:** `F=in-path flash(srcPool)`, `S=self-fund(E[WETH])`, `P=PM-ledger(no-prefund)`.
**Coupling** per boundary `(a→b, b→c)`: `pool`, `exec`, `pm`, `take`.
**Pivot** = repayment/settlement mechanism. All rows: `capture = E[WETH]` (or `F[WETH]` under the `erc6909_profit` opt for the V4–V4 rows); `bribe = config axis`; `flash = in-path only`.

### 2-hop (9)

| Family | Funding | Coupling | Pivot | Notes |
|---|---|---|---|---|
| `v2_v3` | `F(v2_a)` | exec→exec | `E[WETH]→v2_a` in callback | leading V2 flash, repaid from V3's WETH output |
| `v3_v2` | `S` | pm→pool (take→`H`) | callback repays `E[WETH]→v3_a` | terminal V2 is `V2_SWAP_CALC` (pre-funded) |
| `v3_v3` | `S` | exec→pool | callback repays `E[WETH]→v3_a` | terminal V3 to SELF |
| `v4_v4` | `P` | pm (delta, no bridge) | `settle_all` | honors `use_v4_batch`/`erc6909_profit` opts |
| `v4_v3` | `P` | take→exec (V4 out taken, `E`→V3) | `settle_delta`/`settle_all` | V4 output `TAKE_COMPACT(cur,SELF)` then V3 |
| `v3_v4` | `S` (V3 flash repaid via `E[WETH]`) | exec→pm | `settle_all` | V3 output transferred to PM to fund V4 |
| `v4_v2` | `P` | take→`H` (V4 out `TAKE_COMPACT(cur, v2)`) | `settle_delta` | terminal V2 `V2_SWAP_CALC` |
| `v2_v4` | `F(v2_a)` | exec→pm | `settle_all` | V2 borrows; forward transferred to PM for V4 |
| `v2_v2` | `F(v2_a)` | pool→pool (`CALC` recipient) | `E[WETH]→v2_a` | the generic all-V2 speedrail (`encode_all_v2`) |

### 3-hop V2-leading (8)

| Family | Funding | Coupling | Pivot |
|---|---|---|---|
| `v2_v2_v2` | `F(v2_c)` | pool→pool→pool | `E[WETH]→v2_a` in c-callback |
| `v2_v2_v3` | `S` (led by `E→v2_a` + `CALC`) | pool→pool→exec | callback repays `E[WETH]→v2_a` |
| `v2_v2_v4` | `S` (self-fund leading V2, **fixed**) | pool→pool→pm (`V4_SETTLE`) | `settle_all` |
| `v2_v3_v2` | `F(v2_c)` | pool→pool→pool | `E[WETH]→v2_a` in c-callback |
| `v2_v3_v3` | `S` | exec→pool→exec | callback repays `E[WETH]→v2_a` |
| `v2_v3_v4` | `S` | exec→pm | `settle_all` |
| `v2_v4_v2` | `F(v2_c)` | take→pool→pool | `E[WETH]→v2_a` in c-callback |
| `v2_v4_v3` | `S` | pm→exec | `settle_delta(forward_a)` |
| `v2_v4_v4` | `S` (self-fund leading V2, **fixed**) | pm (no bridge) | `settle_all` |

### 3-hop V3-leading (9)

| Family | Funding | Coupling | Pivot |
|---|---|---|---|
| `v3_v2_v2` | `S` | pm→pool→pool | callback repays `E[WETH]→v3_a` |
| `v3_v2_v3` | `S` | pool→exec→exec | callback repays `E[WETH]→v3_a` |
| `v3_v2_v4` | `S` | take→pm | `settle_delta(forward_b)` |
| `v3_v3_v2` | `S` | pool→pool→`H` | callback repays `E[WETH]→v3_a`; terminal V2 `CALC` |
| `v3_v3_v3` | `S` | exec→pool→exec | callback repays `E[WETH]→v3_a` |
| `v3_v3_v4` | `S` | pool→pm | `settle_all` |
| `v3_v4_v2` | `S` | pm→pool→`H` | callback repays `E[WETH]→v3_a`; terminal V2 `CALC` |
| `v3_v4_v3` | `S` | take→exec→exec | callback repays `E[WETH]→v3_a` |
| `v3_v4_v4` | `S` | pm | `settle_all`; `TAKE_DELTA(WETH,SELF)` capture |

### 3-hop V4-leading (9)

| Family | Funding | Coupling | Pivot |
|---|---|---|---|
| `v4_v2_v2` | `P` | take→pool→pool | `settle_delta(WETH)` |
| `v4_v2_v3` | `P` | take→pool→exec | `settle_delta(WETH)` |
| `v4_v2_v4` | `P` | take→pool→pm | `settle_all` |
| `v4_v3_v2` | `P` | take→pool→`H` | `settle_delta(WETH)`; terminal V2 `CALC` |
| `v4_v3_v3` | `P` | take→pool→exec | `settle_delta(WETH)` |
| `v4_v3_v4` | `P` | pm | `settle_all` |
| `v4_v4_v2` | `P` | pm→take→`H` | `settle_all`; terminal V2 `CALC` |
| `v4_v4_v3` | `P` | pm→take→exec | `settle_all` |
| `v4_v4_v4` | `P` | pm (no bridge / batch) | `settle_all`; honors both opts |

---

## 6. Findings that shape the derivation (feed `6YUNQN`)

1. **The flash source pool is the *outermost* flash command's pool, not necessarily hop *a*.** In `v2_v3_v2`, `v2_v4_v2`, `v2_v2_v2` the outermost `V2_SWAP_COMPACT` is the **last** hop `c`; the entry credit then flows *backward* through the callback nesting (`c → b → a` execution order is the reverse of the hop order). The derivation cannot assume "leading hop = funding source."
2. **Funding and hop-coupling are coupled for the flash case** (ADR-029 Q3): the repayment pivot is *decided by* where the borrowed ledger is settled, and the self-fund-vs-flash choice is *forced* by the D0 invariant (a V2-leading, V4-terminal family must self-fund to avoid an early WETH take). The grammar picks the funding source, not the operator, for these 36.
3. **No off-path flash in the current 36**; in-path only. Off-path first appears with an external lender (Aave stub, `VIXQYH`) and is the natural home for the "borrow the profit token from a V2 pool as an off-path source, then repay last" idea.
4. **Terminal-V2 is a `H[pool,input]` credit-before-debit rule**, not a per-family special case. The same fix appears in `v2*`, `v3*`, and `v4*`-leading families wherever the terminal is V2.
5. **`V4_TAKE_COMPACT` is used for *three distinct* ledger moves** — capture (`TAKE(cur,SELF)`), hop-coupling (`TAKE(cur, next_pool)`), and funding (`TAKE(WETH, pool, optimal_input)` to pre-fund a leading V2). The derivation must disambiguate by recipient role, not by opcode.

---

## 7. Checkpoint questions for `6YUNQN` / open items

- Verify the funding-source labels above against a **per-family byte trace** during `6YUNQN` — the rubric is stable but a couple of `S` rows (`v2_v2_v3`, `v2_v3_v3`) deserve a second read (they repay from a pre-held `E[WETH]`, the definitional edge of self-fund vs in-path flash).
- Confirm these two invariants are the **complete** set the validator must enforce, or surface a third (e.g. `V4_SYNC`-before-`V4_SETTLE` ordering) from the trace.
- Decide whether `PM`-ledger (V4 no-prefund) should remain a *funding-source value* or be modeled as "funding = intrinsic to V4" — it is the one row that makes `FundingSource` non-trivial for pure-V4 paths.
