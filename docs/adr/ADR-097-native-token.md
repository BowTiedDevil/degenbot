# ADR-097: Conceptual `NativeToken` — one chain-scoped identity, protocol-private sentinel translations

Status: accepted / implemented (the conceptual type, chain-facts metadata, protocol-private Curve/V4 translations, the Python projection, the session account-cache read-through, and the Aave `balanceOf` re-point are all live — see ADR-023 A6 for the account-query and construction collapse record)

## Context

A chain's native asset (Ether on Ethereum) has no ERC-20 contract, yet every
pool, path, and account read is typed over tokens. The codebase modeled it as
an artifact of whichever protocol it arrived through:

- Python's `EtherPlaceholder` (an `Erc20Token` subclass) registered the
  all-Es or zero address as a token contract, complete with a fabricated
  `name`/`symbol`/`decimals` triple hardcoded at the call site.
- Curve pools enumerate the all-Es coin as a placeholder address, and V4
  names the native currency `address(0)` in its `PoolKey`.
- Those raw sentinels became generic token input: anything that accepted an
  address accepted a sentinel, and the placeholder's metadata (`"Ether
  Placeholder"`) leaked into goldens, DB rows, and display.

Three problems follow from that shape. First, identity: a raw sentinel can be
read as "an ERC-20 at this address", so the same conceptual asset acquires
different identities in different protocols and one spelling can collide with
a real contract. Second, metadata: the native triple was hardcoded in the
Python builder, and would have to be hardcoded again in Curve and V4.
Third, WETH: the wrapped asset is a genuine ERC-20 whose reserves stand in
for native balances in V2/V3-style pools, and an address-driven placeholder
invites aliasing the two.

## Decision

### D1 — The native asset is a conceptual, chain-scoped `NativeToken` identity.

One `NativeToken` exists per chain, carried in the session token identity
(`TokenIdentity::Native`) beside `TokenIdentity::Erc20(address)`. It has **no
protocol address of its own**: `TokenIdentity::address()` is `None` for it,
and a raw sentinel string or address is never a way to spell it. Public
generic token/path inputs and results across both consumers use the
`Erc20Token | NativeToken` union, projected through the thin PyO3 adapter
like every other core type. A generic entry point that receives a raw
sentinel address fails loudly instead of resolving it to the native token.

### D2 — Native metadata is intrinsic chain facts, owned by the core.

`degenbot_core::chain_facts` is a small manifest: chain id →
name/symbol/decimals (Ethereum resolves to ETH/18). It is neither operator
config (ADR-065 — those describe deployments) nor the pool species manifest
(that describes pool behavior): a row describes the chain itself and is not
configurable. A chain with no row is a typed refusal (`UnknownChainFacts`),
never a guessed triple. Native construction entries resolve through these
facts and skip contract metadata RPCs entirely.

### D3 — Protocol translations are private, at the protocol's own call sites, and bidirectional.

Each protocol keeps its own wire spelling, translated only inside that
protocol's identity/call machinery:

- Curve accepts and returns the conceptual `NativeToken` by translating the
  all-Es coin placeholder at Curve construction and coin-read call sites
  only. Zero remains the Curve coin-enumeration terminator, and any other
  codeless coin still raises `NoCodeCoin` — the all-Es spelling is the ONE
  recognized native sentinel, re-pinned with a genuinely invalid non-sentinel
  coin in the negative test.
- V4 translates `NativeToken` to `address(0)` only inside `PoolManager`
  identity, `PoolKey`/pool-id derivation, and execution. Public inputs and
  results never require a caller to supply zero.

A translation is never reachable from the other protocol: the Curve all-Es
coin is not a V4 input, and V4's zero currency does not enter Curve's
enumeration. Raw protocol views survive only where execution requires them;
generic pool token getters and construction outcomes expose the
`Erc20Token | NativeToken` union.

### D4 — Sentinel DB evidence stays distinct; no coalescing.

Curve's sentinel row and V4's sentinel row are separate persisted evidence
with separate token IDs, exactly as their pools record them. Session/path
identity resolves to the conceptual `NativeToken` in protocol context, but
the database does not coalesce the rows across protocols: a replay or
migration reads each protocol's own record.

### D5 — WETH is a distinct token; native/WETH bridging is explicit and local.

WETH keeps its own identity and its own object. Where a path or an execution
step must bridge native value through a wrapped asset (V2/V3 reserves holding
WETH against a native leg), that relationship lives in explicit path
validation and the execution bridge — never in session identity, never as a
DB alias, and never as a property of either token.

## Consequences

- Both consumers name the native asset the same way, with one metadata
  source, and a new chain is a manifest row instead of a code edit in three
  places.
- Every protocol that spells native value with a sentinel owns one private,
  tested, bidirectional translation; the sentinel stops being a public
  spelling, so generic token inputs cannot acquire accidental meaning.
- The Python `EtherPlaceholder` subclass and its address-driven special cases
  are deleted outright (ADR-087) with no compatibility alias: callers move to
  the `NativeToken` interface, and behavior is preserved through that
  interface rather than through the deleted names.
- The cost is explicit plumbing at each protocol seam (Curve coin reads, V4
  pool keys/execution) and a hard failure at generic entry points that still
  hand over raw sentinels — deliberate, because silent acceptance of a
  sentinel is exactly the identity drift this decision removes.
