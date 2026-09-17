# Sim-only bid gate (NYVL2F gate 2) — live run journal

Epoch base: 2026-09-16. Operator `0x5c603b8a…080e` (scratch funds, FORK-2 GO by operator).
Architecture: FORK-1 resolved `standalone sidecar` — this gate ran entirely through the new pipeline crates
(feed + classifier + submission bundle leaf) plus `cast`/`curl` for tx authoring; zero touches to the live
engine loop.

## Executor pre-state
- Executor `0x30b28ed8aa581fbc0191c3b532b0697773070e97` (user-journey deployment, 32,517 B,
  vyper 0.5.0b1 artifact — commit `706493fe1`), owner = operator, ETH/WETH balance 0.
- Funding via plain transfer REVERTS (fallback is owner-gated, `NotPlainEthTransfer` family):
  fixed by funding through WETH (`deposit()` + `transfer(exec, 0.0006 ETH)`), which the SWEEP
  auto-withdraw path unwraps.

## Backrun construction (the bid)
- Calldata: `execute(bytes,uint256)` = `0xab5898e8` + `[0x15 (WETH_WITHDRAW_ALL, 1-byte command)]`
  + config `2560003` = check_mode 3 (SWEEP, profit-assert defeated by design) | bribe_bips 10000
  | bribe_recipient_idx 0 = `block.coinbase` (bundle fee recipient).
- Sim oracle (`eth_call` on the provided node — see drift note): return value
  `0x0221b262dd8000` = 600,000,000,000,000 wei = exactly the executor's 0.0006 ETH balance,
  i.e. the full coinbase bid. Trace shows the final `raw_call(coinbase, value=bid)`.
- NOTABLE DOC-DRIFT FINDING (TR-15 class, MEVBlocker side): `eth_callMany` is documented at
  docs.mevblocker.io/how-to/searchers#simulate — NOT served by `rpc.mevblocker.io`,
  `/fast`, `/maxbackruns`, `boost.rpc.mevblocker.io/fast`, nor the searchers WS
  (`Method not found`). The provider-side annotations in
  `docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md`-adjacent work should treat
  the `/fast` sim-surface as unavailable until MEVBlocker ships it. Equivalent-oracle
  substitution recorded here.
- Also NOTABLE: empty command streams (`execute(0x, config)`) REVERT in SWEEP mode —
  the mode-3 command loop executes at least one command unconditionally
  (`_execute_command_at` reads past the empty `Bytes`). A 1-byte benign command
  (`0x15`) is the minimal valid sweep carrier. Discovered live; not a test artifact
  (the executor/ suite has no empty-stream SWEEP case — future test candidate).

## Round 1 (timing slack ~ minutes: target mined before bid landed on WS)
- Target: `0x2dc8b0eda36fcfe1384be8291eb205c67169be108f98b9b2e3193a7618c8bd55` — landed.
- Backrun signed (nonce 6) but target slot consumed by round-2; superseded.

## Round 2 (tight, same-second publish + WS bid)
- Target `0x1feebe1a6710e2c1fbdbb29d14b2c3ad67aa1d73a3ebc0542cd2fd1eb1139d0c` (self,
  nonce 5->6 slot, private via `rpc.mevblocker.io/noreverts`) — LANDED block 25993737 status
  0x1 (gas ~21k ⨯ ~0.1 gwei ≈ 0.0000021 ETH).
- Bundle bid: `eth_sendBundle` over `wss://searchers.mevblocker.io`,
  `txs=[target_hash, signed_backrun(nonce 7)]`, blocks 0x2599373b–d, uuid
  `degenbot-<block>-<sha256-target+block>`. Response: `bundle with 0x1feebe1a… forwarded`
  (relay-side acceptance, S1 evidence). Backrun `0x067c0d0c…ff05` DID NOT LAND —
  raced and dropped at ZERO cost (revert-shield semantics, S7 analogy holds).

## Round 3 (tightest: single-second publish→bid)
- Target `0x170591e48dcdb4066907459c886c7fc584f1c52ad70eb3780d01eb431961aef4` (nonce 7),
  accepted; bid for 5 block windows; backrun `0xa0072c5e…05ad` (nonce 8) — not landed
  in watch window (private flow inclusion timing), executor WETH balance intact
  (`0x0221b262dd8000` — bid still live for later matching inclusion).

## Cost ledger (through this gate)
- Round-2 target gas: ~0.0000021 ETH (maid: block 25993737)
- Funding pair (deposit+transfer) gas: ~0.000045 ETH combined
- Landed bribe: none (all bids raced) — budget consumed ≈ 0.00005 ETH of the
  ≤0.0005 authorization; executor holds 0.0006 WETH as the live unspent bid.

## Repro commands
1. Fund: `cast send $WETH 'deposit()' --value 0.0006ether --private-key $K --rpc-url $RPC;`
   `cast send $WETH 'transfer(address,uint256) returns (bool)' $EXEC 600000000000000 --private-key $K --rpc-url $RPC`
2. Backrun calldata: `cast calldata 'execute(bytes,uint256)' 0x15 2560003`
3. Sim oracle: `cast call $EXEC --data 0xab5898e8… --from $OP --rpc-url $RPC --trace`
4. Publish target: `curl -X POST https://rpc.mevblocker.io/noreverts --data '{eth_sendRawTransaction …}'`
5. Bid: `eth_sendBundle` over `wss://searchers.mevblocker.io` (scripts/backrun/observe_soak.py lineage;
   see /tmp/bid_now.py body for the exact frame shape)
6. Watch: `eth_getTransactionReceipt` polls for both legs.
