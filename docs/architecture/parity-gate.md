# The parity gate

Every port of Python behaviour into Rust (math, decode, encode, signing, DB
writes, RPC wire shapes) crosses a seam where a subtle divergence silently
corrupts results: the two implementations *look* equivalent and are not. The
parity gate is the rule that closes that seam:

> **A Rust port must be proven value-exact against the Python implementation —
> the oracle — before the Python side is deleted, and the oracle dies with the
> routing cutover.**

The gate has three phases. Each leaves the tree green before the next starts.

1. **Prove.** Write the Rust leaf and a parity test that drives the *existing*
   Python port over a fixture corpus and asserts the outputs match —
   byte-for-byte for bytes and encoded payloads, field-for-field for DB rows,
   integer-exact for swap math, wire-shape-equal for RPC structs. The test is
   red against a placeholder implementation and green against a faithful port.
2. **Route.** Re-point the Python companion (or the callsite) at the Rust seam.
   The Python implementation now has zero live consumers.
3. **Retire.** Delete the Python port *and its parity tests in the same
   change*. A parity test whose oracle is gone is a tautology, and an
   "equivalence" harness comparing two live implementations must not be left
   running indefinitely.

## What must match, and against which oracle

The oracle is always the retired-or-retiring Python implementation, named per
seam:

| Seam | Must match | Oracle |
|------|------------|--------|
| DB writes (`degenbot-db`, `degenbot-aave`) | multi-table row state, field-for-field, incl. create-vs-mutate trajectory and column defaults | the Python ORM trajectory (`event_handlers.py::_process_*` + `get_or_create_*`) |
| EIP-1559 signing (`degenbot-submission`) | raw signed bytes, byte-for-byte, and the decode-what-you-signed round trip | `eth_account.Account.sign_transaction` |
| Typed RPC structs (`degenbot-rpc`) | JSON wire shape | web3.py round trips (the execution-apis spec shapes) |
| Priority fee / fee history (`degenbot-rpc`, `degenbot-arbitrage`) | computed values | the Python `_compute_priority_fee` / `fee_history` |
| Price readers (`degenbot-price`) | decoded values incl. decimal correction | `ChainlinkPriceContract` / `OraclePriceFetcher.fetch` |
| Revert classification (`degenbot-decoders`) | label strings | the retired Python `classify_revert` corpus |
| Calldata + solvers (`degenbot-arbitrage`) | encoded payload bytes | the Python encoder goldens |
| Sim state overrides (`degenbot-simulation`) | warmup slots + override values, field-for-field | the Python oracle's emitted values |
| Storage layout (`degenbot-executor`) | warmup slots, byte-for-byte | `cmd_executor.initialize()` |
| Ported math (`*-math` crates) | integer-exact contract arithmetic | the Python port over a fixture corpus |

Two adjacent gates are distinct and keep their own homes: the **on-chain golden
parity** tests compare against recorded on-chain truth
([golden-onchain-parity.md](golden-onchain-parity.md)), and the **Tier-2
dual-driver sim parity** pair is documented with the sim inspector
([revm-inspector-diagnostics.md](revm-inspector-diagnostics.md)).

## Delegation has its own proof

When a Python companion *delegates* to a Rust seam, a parity test alone cannot
detect a silent fallback to dead Python code. A **delegation spy** (the
`_DelegateSpy` pattern: a wrapper that records the delegated call and its
arguments, then passes through) asserts "the Rust seam was hit with the right
arguments". Use it for every routing cutover where the math already has parity
coverage — it proves the seam, not just the numbers.

## Keeping the corpus

After retirement, the Rust `#[cfg(test)]` fixture corpus (and any committed
fixture DB) is the permanent regression set. A later edit that changes a
Rust-side result must update the corpus deliberately — never relax an assertion
to make a diff go away.
