# Builder/Relay Revert Protection & Bankroll Guardrails

Investigated 2026-09-16 against the live endpoint set. Purpose: protect the
exercise bankroll from the dominant loss mode — *an opportunity is sim-valid,
the bot submits, another searcher's tx lands first, ours reverts on-chain (the
executor's profit check / stale state), and we pay the gas anyway.*

## The headline answer

**Yes — multiple free, reputable mainnet endpoints accept plain signed
EIP-1559 transactions via `eth_sendRawTransaction` with revert protection**
(builders simply never include a reverting private tx: it pays nothing).
No bundle machinery, no account, no reputation staking is required for the
basic modes. The same signed bytes the bot already produces can be broadcast
to these endpoints instead of (or in addition to) the public mempool.

## Endpoint findings (empirically probed)

Probe method: read-only `eth_chainId` (liveness/chain), plus
`eth_sendRawTransaction("0x")` (a "typed transaction too short"/"RLP" decode
error proves the method is routed; a `Method not found` would exclude the
endpoint).

| Endpoint | liveness / chain | accepts `eth_sendRawTransaction` | revert protection | notes |
|---|---|---|---|---|
| `https://rpc.flashbots.net?hint=hash` | ✔ chain 1 | ✔ (`typed transaction too short`) | ✔ (private; reverting txs are never valuable to builders) | Flashbots Protect. `hint=hash` shares only the tx hash — minimal leakage; routes to the Flashbots builder network |
| `https://rpc.flashbots.net/fast` | ✔ chain 1 | ✔ | ✔ | Broadest builder coverage incl. MEV-Share — more eyes on the calldata (leakage risk on the arb path itself) |
| `https://rpc.mevblocker.io/noreverts` | ✔ chain 1 | ✔ (`RLP string too short`) | ✔ (explicit) | CoW DAO's MEV-Blocker. `/noreverts` is the exact posture we want |
| `https://rpc.mevblocker.io/fullprivacy` | ✔ chain 1 | ✔ | ✔ | Strongest privacy variant (no searcher flow sharing, no rebates) |
| ~~`https://rpc.beaverbuild.org`~~ | ✔ (picky JSON-RPC hygiene) | ✔ | **undocumented — EXCLUDED from allowlist** (no operator no-revert SLA found) | Beaver. Send-only surface |
| ~~`https://rpc.titanbuilder.xyz`~~ | ✔ (send-only surface) | ✔ | **undocumented — EXCLUDED from allowlist** (docs cover bundles, not plain-tx no-revert) | Titan. Send-only surface |
| `https://api.securerpc.com/v1` | no response within 8 s | — | — | Excluded (unreachable from this environment) |

**Recommended allowlist (CLOSED — exactly three entries; in priority
order):**

1. `https://rpc.flashbots.net?hint=hash` — Flashbots Protect, hash-only hint
   (`/fast` is the same Protect service, broader builder share).
2. `https://rpc.mevblocker.io/noreverts` — MEV-Blocker, explicit revert
   protection.
3. `https://rpc.mevblocker.io/fullprivacy` — MEV-Blocker, strongest privacy.

Broadcast the SAME signed tx to several of them in parallel — same nonce,
same bytes, no conflict; whichever builder wins the block delivers it.

**Excluded and why** (the allowlist admits only endpoints whose operator
*documents* a no-reverting-inclusion guarantee):

- Beaver (`rpc.beaverbuild.org`) — docs (`beaverbuild.org/docs.html`)
  verified 2026-09-16: raw txs are private ("the transaction will stay
  private") but carry **no** no-revert statement; revert handling exists
  only in the bundle API (`revertingTxHashes` opt-in — default excludes
  reverts, bundle machinery out of scope). Out.
- Titan (`rpc.titanbuilder.xyz`) — docs (`docs.titanbuilder.xyz`, GitBook
  markdown) verified 2026-09-16: raw txs are private ("never broadcast...
  to the public mempool") with **no** no-revert statement; bundle API has
  Flashbots-style `revertingTxHashes` semantics. Out.
- SecureRPC — unreachable from this environment. Out.
- **Anything else, full stop.** The agent must not research or adopt
  endpoints beyond this table; the point of the allowlist is a pinned,
  audited surface, not breadth.

**BuilderNet (investigated 2026-09-16 — deliberately NOT added):**

- What it is: Flashbots' TEE-based decentralized builder network (operators:
  Flashbots, Beaverbuild, Nethermind). Direct endpoints exist:
  `rpc.buildernet.org` (+ regional `direct-us/eu/ap`), supporting
  `eth_sendRawTransaction` (private, propagated node-to-node, inclusion tried
  for up to 5 blocks) and `eth_sendBundle` (atomic; documented: "if any
  transaction in the bundle fails, the entire bundle fails" — a
  bundle-granularity no-revert guarantee).
- Why it stays out of the allowlist:
  1. The direct raw-tx API documents privacy and retry semantics but
     **states no plain-tx no-reverting-inclusion guarantee** — the atomicity
     guarantee attaches to the *bundle* API only.
  2. Direct submission requires `X-Flashbots-Signature` request signing
     (verified: unsigned requests are rejected with `Invalid signature`) — a
     real integration lift beyond the bot's plain raw-tx broadcast leaf.
  3. **It is unnecessary**: per BuilderNet's own docs, "Orderflow sent to
     Flashbots Protect ... is automatically shared with BuilderNet." The
     existing allowlist already reaches BuilderNet's builders under Protect's
     documented no-revert SLA, with plain `eth_sendRawTransaction` and no
     request signing.
- If single-tx-bundle submission to BuilderNet is ever wanted later, the
  documented atomicity makes it a legitimate protected surface — but that is
  an opt-in integration decision with the signing lift, not part of this
  exercise's allowlist.

## Trust model — how strong is the "no reverting inclusion" claim?

Be precise: it is an **operator commitment enforced by builder software and
incentives, not a protocol or cryptographic guarantee**. Three legs support
it, in decreasing strength:

1. **Documented SLA (quoted 2026-09-16).** Flashbots Protect: "No failed
   transactions: Transactions are only included in the block if they do not
   revert. Users do not pay fees for failed transactions." MEV-Blocker
   publishes a per-endpoint matrix: `/noreverts`, `/fullprivacy`,
   `/maxbackruns` are revert-protected; **`/fast` and `/nochecks` are NOT —
   an endpoint-selection trap** (the allowlist names only protected variants).
2. **Mechanism + incentives.** Builders simulate private orderflow and drop
   reverting txs rather than mine them for the (post-burn, tiny) priority
   fee. The relays' entire business is orderflow exclusivity; a publicly
   observable revert-mining incident ends it, and our conservative fee
   policy keeps the per-cheat payoff at fractions of a dollar.
3. **Auditability.** Any landed tx of ours with `status == 0` is a
   disproof, publicly checkable. The guarantee is falsifiable per-relay at
   near-zero cost.

**What does not protect us:** re-broadcasting the same signed tx to the
public mempool (public reverting txs land normally — the guarantee covers
only private routing); and inclusion-rate reduction is structural — a
private tx can only land in blocks built by that relay's builder network.

**Empirical falsification test (add to the exercise once funded):** submit a
crafted-to-revert `execute()` (e.g. empty/invalid command stream) with a
~1-wei priority fee through the allowlist. Never-included across N blocks =
consistent with protection. Any landed revert = shield disproven for that
relay. Acceptance: see S7.

## What revert protection does and does not buy

- **Race losses become free.** If a competing searcher takes the arb first
  in the same block, our tx's on-chain profit check / stale-state paths
  revert → excluded by builders → no gas spent. This is the bankroll shield.
- **Latency and hit-rate get worse, not better.** Private txs wait for a
  participating builder to win a block, and priority-fee-only txs sit thin
  in builder ordering. Expect lower inclusion probability than the public
  mempool — acceptable for micro-arb attempts because failed attempts are
  free.
- **A landed-but-profitable-below-gas tx is still possible.** Revert
  protection only blocks *reverts*. If state shifts make the arb thinner
  but still non-reverting, we can land a tx whose profit doesn't cover gas.
  Layers 2–4 below cover that residue.

## The bankroll guardrail stack (four layers, ordered)

1. **Revert shield (relay submission).** Failed/raced attempts cost zero gas.
2. **On-chain seatbelt (executor `config` param).** `pack_config` supports
   `check_mode` 1/2/3 with an `expected_value` (pre-tx balance) and optional
   builder bribe bips (`rust/crates/degenbot-executor/src/config.rs`). Every
   live submission MUST carry a non-zero `check_mode` — acceptance criterion
   S6 decodes a submitted tx's calldata to prove it. Verified in code
   (2026-09-16): the composer's default is `check_mode=1` (custody capture →
   WETH+ETH balance assert; `composers.rs::config_for_options`); ERC6909
   capture maps to `check_mode=2`. `check_mode=0` (skip) is opt-in only. The
   seatbelt bounds the worst case of a landed tx to *gas only*, never
   principal.
3. **Fee sanity (off-chain).** Current policy: `maxFeePerGas = 1.5 ×
   base_fee_next + priority_fee`, priority from percentiles (10, 50) of the
   last 10 blocks (`fee.rs`). With the revert shield, priority can stay
   conservative (we lose the race for free anyway). Guard: the
   `MIN_PROFIT_NET`/`MIN_PROFIT_MARGIN_BPS` gates already require
   net-of-gas-positive sims before any submission.
4. **Budget & counters (operational).** The existing stop conditions do the
   rest: hard gas budget, consecutive-revert cap (which the revert shield
   renders rare), timebox, STOP file.

## Fork-verification (the "partial credit" mechanism)

The user asked for points when the proposed tx *would have executed
correctly at the calculation block*. Verified feasible: the local reth node
serves **historical `eth_call`** (probed: WETH `totalSupply()` and an
`eth_getBalance` at tip−50,000 both succeed on first try).

Procedure (per candidate the bot judged submittable):
1. Record the candidate's `solve_block` and the exact `execute(bytes,uint256)`
   calldata (the submit leaf builds it before broadcast).
2. Replay: `cast call <EXEC> '<calldata-as-per-ABI>' --from <OP> --rpc-url <RPC>
   --block <solve_block>` — or `eth_call` with the raw `data`.
3. Check the call succeeds and measure the executor token-delta at that block
   (eth_call returns the packed post-balance for check_mode 1, or read
   `WETH.balanceOf(<EXEC>)` pre/post at that block via two scoped calls).

A candidate that replays green at its `solve_block` earns partial credit
even if the live submission lost the race or was never built into a block.

## Integration seam (where a relay plugs in)

`degenbot-submission` separates **reads** from the **broadcast leaf**:

- Reads (nonce via dispatcher-claimed values, `eth_feeHistory`,
  `eth_create_access_list`) run on the configured provider (local node).
- Broadcast is one call: `AlloyProvider::eth_send_raw_transaction` in
  `submit.rs`, using the `provider: &AlloyProvider` argument of
  `dispatch_and_submit`.

The minimal change: give the submit leaf a **dedicated broadcast provider**
(or a fan-out set) distinct from the read provider. Rust-side this is a
second `AlloyProvider` constructed with the relay URL(s); reads keep the
local node. The signer (`TxSigner`, `secp256k1`, chain id 1) and fee
finalization are untouched — the same signed bytes go to any endpoint.

> Delivery policy note (`submit.rs` header): the *policy* of when-not-to-
> submit stays Python; the relay *target* is config/plumbing, matching that
> disposition.

## Integration caveat — private-pending nonce blindness

The dispatcher claims nonces from reads against the local node
(`eth_getTransactionCount`-derived). A **private pending tx is invisible on
the local node**, so the next batch can re-claim the same nonce. Outcomes by
relay: same-nonce replacement (higher effective fee usually wins) or silent
drop. The existing `blocks_before_nonce_expires = 5` + `monitor.rs`
replacement logic partially covers this, but an agent wiring a relay should
expect and handle nonce re-use. Mitigations: serialize submissions (one
in-flight nonce), or read `eth_getTransactionCount` *from the relay* where
supported (Flashbots Protect supports standard read methods; Beaver/Titan
are send-only — another reason to prefer Flashbots/MEV-Blocker as primary).


