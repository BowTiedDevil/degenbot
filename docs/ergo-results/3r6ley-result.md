# 3R6LEY (S5) — corpus capture + golden pins for the six config-dispatch events

## Outcome

The formerly-fenced config-dispatch events now carry live-chain golden
coverage. Three mainnet spans were located by a read-only `eth_getLogs` scan of
the `PoolAddressesProvider`, recorded to cassettes, and gated by a new replay
suite whose statement-ledger and DB-dump goldens pin the SQL trace and end
state. The existing golden gates pass byte-identical and untouched.

## Deliverables (new files only)

- `rust/crates/facade/degenbot/examples/generate_aave_config_windows.rs` — the
  wave-5 recorder (modeled on `generate_aave_upgrade_windows.rs`): seeds a
  throwaway harness DB, runs the real `run_aave_update` over the recording
  transport, flushes canonical cassette bytes, replays through the ledger, and
  records every window twice (a determinism gate). `--check` regenerates and
  exits non-zero on drift.
- `tests/fixtures/cassettes/wave5/w5_aave_pool_data_provider_updated.json`
  (+ `.statement-ledger.json`, `.db-dump.json`, `.seed.json`)
- `tests/fixtures/cassettes/wave5/w5_aave_address_set_registration.json`
  (+ goldens/seed)
- `tests/fixtures/cassettes/wave5/w5_aave_bootstrap_proxy_created.json`
  (+ goldens/seed)
- `tests/fixtures/cassettes/wave5/CONFIG-SPAN-MAP.md` — provenance, the way
  the wave-4 `UPGRADE-MAP.md` documents its windows.
- `rust/crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs` —
  the replay gate (cassette + statement-ledger + dump goldens, ADR-068 D3/D4,
  `REGENERATE_SQL_GOLDENS=1` arm), with per-span served/chunk literals and
  negative probes.

## The spans and what they pin

| window | cursor / to_block | transition | pin |
|---|---|---|---|
| `w5_aave_pool_data_provider_updated` | 20,398,668 / 20,398,680 | `PoolUpdated` + `PoolConfiguratorUpdated` + `PoolDataProviderUpdated` in one tx | revisions land via the NEW implementation reads (`POOL` = 4, `POOL_CONFIGURATOR` = 3); the data-provider row's address is rewritten by old-address (`0x7B4EB56E...` -> `0x20E074F6...`). Probe: mutate the recorded `POOL_REVISION()` answer -> the revision column moves, the dump golden is loud. |
| `w5_aave_address_set_registration` | 20,977,086 / 20,977,096 | `PoolUpdated` + the sole `AddressSet` (a fresh registration) | the decode names `MOCK_STABLE_DEBT` and the apply inserts the row; the dump carries it and the ledger carries the insert. |
| `w5_aave_bootstrap_proxy_created` | 16,291,126 / 16,291,135 | `ProxyCreated`(POOL) + `PoolUpdated` @ 16,291,127; `ProxyCreated`(POOL_CONFIGURATOR) + `PoolConfiguratorUpdated` @ 16,291,130 | the proxy creation resolves each implementation and reads its revision at the tx block; the paired `*Updated` reads the same (impl, selector, block), so the per-chunk memo serves ONE `eth_call` per contract (pinned). Probe: mutate the shared configurator answer -> the revision column moves. |

## Negative finding (documented, not fabricated)

- **No `AddressSet` rotation (`old != 0`) exists on the mainnet deployment.**
  The only observed `AddressSet` (block 20,977,092) is a fresh registration
  (`old == 0`). The S1 rotation arm therefore cannot be pinned from chain data
  and continues to rest on its `ChunkContext` unit test. (Caveat: the scan
  covered `[16,291,000 .. 21,090,999]` exhaustively and probed several windows
  above it; see PENDING.)
- The third bootstrap `ProxyCreated` (block 16,291,136, a non-ASCII id the
  resolver cannot name) is excluded from the green window by design — it would
  abort the chunk by the loud-abort policy. Its policy pin stays in the S2 unit
  test.

## Recorder deviation (flagged)

The task named `record_updater_cassette`. Its `--kind aave-run` path resolves
its substrate by scanning the span's own Pool logs and does not seed the
`POOL_DATA_PROVIDER` row; on these spans that aborts (empty reserve set at
bootstrap; `PoolDataProviderUpdated` update-by-old-address cannot find its row).
The wave-4 precedent — and the substrate resolver that reads
`getReservesList()` and seeds the data-provider row — is
`generate_aave_upgrade_windows.rs`, so the wave-5 recorder mirrors it. As an
independent cross-check, `record_updater_cassette --check` re-verified all
three cassettes byte-identical through its own canonical writer.

## Gates run (foreground, scoped)

- `cargo test -p degenbot-aave` — **PASS**. 256 lib unit tests + all
  integration suites green, including `aave_cassette_replay`,
  `aave_config_cassette_replay`, `wave3_capture_replay`,
  `wave4_upgrade_span_replay` (byte-identical, untouched) and the new
  `wave5_config_span_replay` (1 passed).
- `cargo clippy -p degenbot-aave --all-targets` — **clean** (rustc 1.98.1, the
  pinned toolchain; `cargo test --no-run -p degenbot-aave` also clean).
- `generate_aave_config_windows` recording run (foreground, ~37 s) —
  determinism gate: two passes byte-identical across all three windows.
- `record_updater_cassette --check <3 cassettes>` — all green.

## PENDING (manager re-run) — RESOLVED by the manager

The exhaustive scan of `[21,091,000 .. 26,149,200]` ran to completion (chunked
`eth_getLogs`, ~5.06M blocks): exactly ONE `AddressSet` — block 22,638,170, id
`UMBRELLA`, `old == 0` (a registration, not a rotation). Combined with the
worker's `[16,291,000 .. 21,090,999]` sweep: **no `AddressSet` rotation exists
in mainnet history**, so the S1 rotation arm stays pinned by its unit tests
until one occurs live.

The original PENDING note follows for the record:

- **Exhaustive high-block `AddressSet` scan** (`[21,091,000 .. 26,149,200]`).
  The node's throughput on those blocks exceeds the sandbox foreground bound
  (a 1M-block request does not finish). The scan is what would confirm the
  "no rotation exists above block 21,090,999" caveat. Suggested probe:

  ```bash
  python3 - <<'PY'
  import json, urllib.request
  N="http://host.containers.internal:8545/"
  AP="0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e"
  AS="0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7"
  def c(p):
      r=urllib.request.Request(N,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":p}).encode(),headers={"Content-Type":"application/json"})
      return json.load(urllib.request.urlopen(r,timeout=110))
  # iterate 100k-block chunks over [21091000, 26149200]; print any AddressSet
  PY
  ```

## Foreign tree activity (not mine)

- Modified: `GLOSSARY.md`,
  `docs/adr/ADR-023-construction-io-retirement-disposition.md`,
  `docs/adr/index.md`.
- Untracked: `docs/ergo-results/hqylek-result.md`.

These pre-date / are unrelated to this chunk and were left untouched.
