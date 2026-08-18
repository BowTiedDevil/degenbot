# ADR-034: ERC6909-vault profit capture — production wiring + the batch×capture decline

**Status: accepted.** Decided during ergo task `SMOZG3` (2026-08-18, epic
`4GHYBP`), on top of `NSOFR2` (ADR-033 encode intake).

## Context

`capture = Erc6909` (`ProfitCapture`) was modeled end to end — the axis,
`check_mode = 2` config packing, the `V4_MINT_COMPACT` opcode, the warm slots,
and a spike-oracle proof chain — but it was never production-wired: the
declarative harness ran every chain under `config=0` (fast path, no check),
and the Python cockpit constructed `DispatchCandidate` without the
`erc6909_profit` kwarg, so production always ran `capture = Custody`
(`check_mode = 1`). The task: make the V4 ERC6909-vault capture path
selectable in production dispatch, simulated end to end, and proven by an
oracle assertion on the contract-computed ERC6909 balance.

## Oracle findings (verified against the executor source — then
`tier3-oracle/src-executor/cmd_executor.vy`, now `executor/contracts/cmd_executor.vy`
after SRMMM7 — + runtime harness probes)

1. **Fresh mint suffices — no pre-held ERC6909 position required.**
   `V4_MINT_COMPACT` (0x58) converts the caller's *positive PM delta* into an
   ERC6909 claim on `self` (credit-before-debit; the stub PM models the same
   rule as v4-core's callback mint). The executor's own warmup command
   pre-warms the slot with a 1-wei mint. The harness now pins the zero-
   position regime explicitly.
2. **`check_mode = 2` semantics (the on-chain half of the oracle).** The
   slow path (modes 1 and 2) reads `_combined_balance(mode)` at start and
   end of the command loop and asserts `after >= before` **unconditionally**
   (the U3WVLL self-read fix — the `expected_value` config bits are IGNORED
   in all modes). Mode 2 reads
   `PM.erc6909BalanceOf(self, uint160(WETH))`. Mode 2 is a *position floor*
   (no ERC6909 decrease), not a profitability check: on a flash path
   `before = 0` and the assert is trivial; a losing path is still stopped at
   the protocol layer (flash repayment). Mode 1 (default Custody) is the
   active WETH+ETH money-loss floor.
3. **`use_v4_batch` × `erc6909_profit` on a WETH terminal is unexecutable
   (new finding).** `_cmd_v4_batch`'s tail settle TAKES any positive
   WETH/native delta into custody (`_v4_settle_currency`, positive branch →
   `PM.take`) inside the batch command — before the follow-up
   `V4_MINT_COMPACT` runs. The mint then finds no delta and reverts with the
   PoolManager's `D0` (credit-before-debit). Probed at runtime: the combined
   stream reverted `D0` on the real executor bytecode.
4. **Bribe × mode 2 is orthogonal.** Bribes compute on the TRUE delta from
   `_combined_balance(mode)` and pay from WETH/ETH — mode-independent; no
   interaction to gate.

## Decisions

1. **The declarative harness mirrors production config.** 
   `run_path_with_*` now executes under `config_for_options(opts, 0)` — the
   same Q35IJN expression the arbitrage strategy packs — instead of the raw
   `config=0`. The 36-family matrix therefore runs under the ACTIVE
   `check_mode=1` assert; the losing-self-fund negative control now proves
   the on-chain floor reverts (`InsufficientProfit`), and a sweep-opt-out
   control (`ProfitCapture::SweepToAddress` → `check_mode=3`) preserves the
   off-chain delta-guard probe.
2. **The oracle half of `check_mode=2` is measured, not assumed.**
   `ChainResult.erc6909_delta` (the `PM.balanceOf(executor, weth)` delta
   across `execute`) + `assert_erc6909_capture` (executed; delta > 0; 0.1%
   tolerance per `assert_profitable`; custody WETH must NOT also carry the
   profit) pin the magnitude the on-chain floor alone cannot see. Covered:
   `v4_v4` + `v4_v4_v4`, WETH terminal, starting from a zero ERC6909
   position.
3. **The operator knob is wired.** `DEGENBOT_ERC6909_PROFIT=1` →
   `driver_constants.ERC6909_PROFIT` → `_dispatch_profitable` →
   `DispatchCandidate(erc6909_profit=...)` → `to_simulate_path` →
   `encode_request` (ADR-033 intake) → `resolve_axes` → the `Erc6909` axis
   → `check_mode=2` (end-to-end pinned by a `degenbot-arbitrage` unit test).
   Stream effect is pure-V4-only (per `family_axis_support`: only
   `v4_v4`/`v4_v4_v4` branch the capture axis); every other family keeps
   custody capture and only the mode-2 ERC6909 floor arms — a safe, 
   documented degradation, not a mis-capture.
4. **The batch×capture combination DECLINES.** `v4_v4`/`v4_v4_v4` return
   `None` (the ADR-030 funnel decline) for
   `erc6909_profit && use_v4_batch` on a WETH terminal, via the shared
   `grammar_shape::erc6909_batch_capture_declines` guard, scoped to the
   plan branches that actually emit the mint (the currency-gap branch is
   opt-invariant — it never batches nor mints). The batch gas saving and
   the vault capture are mutually exclusive on the WETH terminal; the
   operator chooses one. Emitting the combined stream was a latent
   guaranteed-gas-loss (reverted on submission); declining routes it to the
   `encode-failed` fail bucket instead. The decline is **interim**: correct
   against the *current* executor bytecode, but the executor is **not
   deployed** — the bot operates it via state-override code injection
   (`INJECT_EXECUTOR_CODE` defaults to `1`, `rust-owned-bot.md` §11.4) and its
   Vyper source is in-repo with a pinned compile pipeline
   (`just verify-tier3-executor-artifact`). Making the combination
   executable is an in-scope contract change — tracked as **TGUZCT**
   (settle-skip batch variant + ledger-validated pairing).
5. **Warm slots: already production-wired.** `WarmupSlots` carries the PM
   ERC6909 WETH balanceOf slot (`erc6909_weth`) and is consumed by
   `state_override.rs` — no change needed.
6. **`EXECUTE_CONFIG` stays, re-documented.** It is the raw `config=0` for
   the offline calldata-dump examples (`path26154`/`path5000`), which encode
   but never execute. The stale "production still uses the hardcoded
   `EXECUTE_CONFIG = ZERO`" doc line in `config_for_options` is corrected —
   production packs through `config_for_options` (Q35IJN, now also mirrored
   by the harness).

## Consequences

- The declarative matrix is stronger: every family now executes with the
  on-chain profit assert armed, exactly like production; a regression that
  breaks the assert (e.g. a stream that stops paying its flash) shows up as
  a matrix revert, not a silent `check_mode=0` pass.
- A new encode decline exists (one cell of the
  terminal×mode×batch option space); the opt-matrix parity tests pin it.
- The operator flag is env-gated and off by default — no behavior change
  for existing runs; enabling it changes pure-V4 paths only.
- The `check_mode=2` floor is a *floor*, not a profit check (decision 1,
  finding 2); operators who want a profit *floor* under capture on
  self-funded flows should note mode 2 asserts the ERC6909 position, while
  mode 1 asserts the custody balance. A combined mode would be a contract
  change — **in scope**, because the executor is pre-deployment and its source
  is in-repo (pinned vyper pipeline). Tracked as **TGUZCT** (settle-skip
  batch variant + ledger pairing validation); the interim decline stays until
  it ships.
- No new crate edges; no pyo3 surface change (the `DispatchCandidate` kwarg
  pre-existed — ADR-033 D4 kept it first-class; this ADR wires the driver).
