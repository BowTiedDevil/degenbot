---
name: node-identity
description: Identify which Ethereum node this environment is talking to (reth vs anvil) and why it matters for eth_callMany bundle simulation. Use when a test or bot run reports an unexpected node conclusion, when the sim gate fails closed, or before drawing any conclusion about node behavior from which binaries are installed.
---

# Local Node Identity

The node this environment points at is **reth**, not anvil. `reth` is not on `PATH`; `anvil` is (`/home/dev/.foundry/bin/anvil`). Both exist, which is exactly how a test ends up reporting an anvil conclusion from a reth response.

## Identify a node by asking it, never by which binary is installed

```bash
just node-identity
# equivalent to:
curl -s -X POST -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' \
  "$ETHEREUM_FULL_NODE_HTTP_URI"
```

`ETHEREUM_FULL_NODE_HTTP_URI` resolves to `reth/{VERSION}`.

## A locally spawned anvil is a separate node

An `AnvilFork` (`tests/standalone_anvil/`) is a separate process with its own socket; `AnvilFork()` without a `fork_url` does not talk to the default URI at all. This anvil build also takes only `--port <NUM>` — no `--ws-port`, and no `/ws` route — so it is HTTP plus IPC, with no WebSocket.

## Measured behavior, both bundle shapes, both transports

| node | transport | searcher-doc shape | mev-geth shape |
| --- | --- | --- | --- |
| reth | HTTP | `-32602` map-where-sequence-expected | succeeds |
| reth | WS | identical `-32602` | identical success |
| anvil | HTTP | `-32601 Method not found` | `-32601` |
| anvil | IPC | `-32601` | `-32601` |

Two conclusions worth not re-deriving:

- **Transport makes no difference.** Reth's answers are byte-identical over HTTP and WS, so there is no "use WS instead" workaround and no transport-specific concern for the sim gate.
- **The two-shape fallback is an endpoint-implementation difference, not a transport one.** `degenbot_strategy::frame_pipeline::simulate_candidate` tries the searcher-doc shape and then the mev-geth shape, because MEVBlocker-family nodes and mev-geth/reth-lineage nodes disagree about whether `params[0]` is the bundle or a list of blocks. Its comment is explicit that a shape or method-missing error must never read as "the bundle reverted".

## The sim gate fails closed on anvil

`eth_callMany` is the pre-submission sim gate: it atomically simulates [victim tx, backrun] against post-target state, never broadcasts, and a `false` result drops the candidate. It therefore **fails closed** on a node that implements neither shape — anvil returns `-32601` for both, so nothing would be submitted against anvil. If a run silently submits nothing, check which node answered `just node-identity` before suspecting the strategy.
