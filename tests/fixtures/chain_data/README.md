# Chain-data fixtures

Per-chain fixture home: `tests/fixtures/chain_data/<chain_id>/` — `1`
(Ethereum), `42161` (Arbitrum), `8453` (Base). Two file kinds share each
directory; keep their conventions apart.

## Recorded-pool cassettes (legacy)

`balancer_*.json`, `curve_*.json`, `uniswap_v3_*_block_*.json`,
`uniswap_v4_*_block_*.json`, `aerodrome_*_block_*.json`,
`pancakeswap_*_block_*.json`, and the bare `block_<N>.json` docs hold
whole-pool construction state (immutables, tokens/reserves, runtime code).
The recorded-pool golden machinery (`tests/golden/recorded_pool.py`, bound
through the `recorded_pool_factory` fixture) reads them; the multi-block
`{"chain_id", "blocks": {...}}` docs in the same directories belong to this
kind. They carry no format marker and are never written by the recorder
below.

## py_oracle corpora (per-block `OfflineProvider` input)

`py_oracle_<scenario>_block<N>.json` holds one pinned block's recorded RPC
answers:

```json
{
  "format": "degenbot.chain-data/v1",
  "chain_id": 1,
  "block_number": 24407242,
  "timestamp": 1770494987,
  "calls": {"0x<to>:0x<calldata>": "<result hex without 0x>"},
  "code": {"0x<address>": "<runtime bytecode hex without 0x>"}
}
```

A `null` call result is a recorded revert. Consumers:
`degenbot.provider.OfflineProvider.from_json_file` (verifies the marker when
present; unmarked legacy files still load) and the offline replay suite
`tests/golden/test_oracle_replay_offline.py`, which also gates the corpus
shape and its agreement with the parity goldens.

### Naming

- Recorded-pool cassettes spell the pin `block_<N>`
  (`aerodrome_v3_cbeth_weth_block_46875151.json`).
- py_oracle corpora spell it `block<N>`
  (`py_oracle_uniswap_v3_quoter_block24407242.json`).
- Both spellings are historical; do not rename either.

### Regenerating the corpora

Machine-emitted only — never hand-edit the JSON:

```sh
cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
    --example record_py_oracle_corpus \
    --scenario <name> ... \
    --ethereum-node <archive-url> [--ethereum-node <fallback-url> ...] \
    --base-node <archive-url> [--base-node <fallback-url> ...] \
    --arbitrum-node https://arbitrum-one.public.blastapi.io \
    [--arbitrum-node <fallback-url> ...] \
    --pace-ms <ms>
```

The recorder records every scenario twice and requires the two passes
byte-identical; `--check` additionally requires byte-identity with the
corpora on disk (exit 1 on drift). Endpoint notes:

- Ethereum and Base scenarios need an endpoint that serves archive state at
  the parity pins (publicnode tiers answer archive requests with "Archive
  requests require a personal token").
- Base (`mainnet.base.org`) rate-limits bursts: pace it (`--pace-ms 1000`).
- Node flags are repeatable: each repetition extends that tier's ordered
  endpoint pool. On a transport-class failure (rate limit, timeout,
  connection) the recorder rotates to the next endpoint after a bounded
  backoff and re-records the scenario from scratch - one log line per
  rotation; fixture gaps and corpus drift stay terminal.
- Arbitrum: the official `arb1.arbitrum.io/rpc` rejects historical state;
  `https://arbitrum-one.public.blastapi.io` serves the camelot pin keyless.
- Omit `--scenario` to record every wired scenario.

The live drift gate (`just live-drift`, i.e. `pytest -m live_drift`) runs
`--check` per chain on demand.
