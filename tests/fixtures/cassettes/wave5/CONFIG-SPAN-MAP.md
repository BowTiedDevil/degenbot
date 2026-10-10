# config-span map: the wave-5 corpora provenance

Chain-derived in this workspace from the always-available archive reth node
(`http://host.containers.internal:8545/`, chain id 1, `reth/v2.7.0`), read-only.
The three wave-5 windows and the negative finding below are selected from a
log scan of the mainnet `PoolAddressesProvider`
(`0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e`). What follows is the discovery
output.

## Discovery method

`eth_getLogs` against the address provider, filtered to the six
config-dispatch topics (the keccak-quoted constants in
`degenbot-decoders/src/aave_event_decoder.rs`): `ProxyCreated`, `AddressSet`,
`PoolUpdated`, `PoolConfiguratorUpdated`, `PoolDataProviderUpdated`,
`PriceOracleUpdated`. Scanned ranges:

- `[16,291,000 .. 21,090,999]` — fully covered (1.2M-block chunks, no gaps).
- `[21,091,000 .. 26,149,200]` — probed at several windows; not exhaustively
  covered (the node's per-request throughput on the higher blocks is the
  limiting factor — a 1M-block request does not complete inside the sandbox's
  foreground bound).

## Event inventory observed

`ProxyCreated` (three, bootstrap only):

| block | id (topics[1]) | proxy = topics[2] | impl = topics[3] |
|---|---|---|---|
| 16,291,127 | `POOL` (ASCII) | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` | `0xfcc00a1e250644d89af0df661bc6f04891e21585` |
| 16,291,130 | `POOL_CONFIGURATOR` (ASCII) | `0x64b761D848206f447Fe2dd461b0c635Ec39EbB27` | `0xfda7ffa872bdc906d43079ea134ebc9a511db0c2` |
| 16,291,136 | `0x703c2c86...` (not ASCII) | `0x8164cc65827dcfe994ab23944cbc90e0aa80bfcb` | `0xe7b67f44ea304dd7f6d215b13686637ff64cd2b2` |

The third id is not the right-padded ASCII `POOL`/`POOL_CONFIGURATOR`, so the
resolver cannot name it; by the loud-abort policy it is fatal inside a chunk
(the S2 arm). The chosen bootstrap window ends at 16,291,135 to exclude it.

`AddressSet` (exactly one observed):

| block | id (topics[1]) | old | new |
|---|---|---|---|
| 20,977,092 | `MOCK_STABLE_DEBT` (ASCII) | `0x0` | `0x102633152313c81CD80419b6eCF66D14Ad68949a` |

Its `old` is the zero address — a fresh registration (an insert), not an
address rotation. No rotation (`old != 0`) was observed on the mainnet
deployment. The S1 rotation arm therefore cannot be pinned from chain data and
rests on its unit test (the `ChunkContext` seam).

`PoolUpdated` / `PoolConfiguratorUpdated` / `PoolDataProviderUpdated` /
`PriceOracleUpdated` (observed blocks):

| block | events |
|---|---|
| 16,291,126 | PriceOracleUpdated |
| 16,291,127 | ProxyCreated(POOL), PoolUpdated |
| 16,291,130 | ProxyCreated(POOL_CONFIGURATOR), PoolConfiguratorUpdated |
| 16,291,136 | ProxyCreated(unknown id) |
| 20,398,674 | PoolConfiguratorUpdated, PoolUpdated, PoolDataProviderUpdated |
| 20,920,979 | PoolConfiguratorUpdated, PoolUpdated, PoolDataProviderUpdated |
| 20,977,092 | PoolUpdated, AddressSet |
| 21,917,056 | PoolUpdated, PoolConfiguratorUpdated, PoolDataProviderUpdated |
| 22,839,362 | PoolConfiguratorUpdated, PoolUpdated, PoolDataProviderUpdated |
| 23,088,584 | PoolUpdated |
| 24,247,927 | PoolUpdated, PoolConfiguratorUpdated |
| 25,199,939 | PoolUpdated, PoolConfiguratorUpdated |

## The three wave-5 windows

Each window drives the real `run_aave_update` chunk loop over the recording
transport. The warm-boot substrate is the chain-current reserve set
(`resolve_aave_substrate` reads `getReservesList()`), resolved at the window
cursor except where the cursor predates the contract set.

| window | cursor | chunk_size | to_block | chunk | transition |
|---|---|---|---|---|---|
| `w5_aave_pool_data_provider_updated` | 20,398,668 | 13 | 20,398,680 | 20,398,669-680 | one transaction rotates Pool + configurator + data provider |
| `w5_aave_address_set_registration` | 20,977,086 | 11 | 20,977,096 | 20,977,087-096 | PoolUpdated + the sole `AddressSet` registration |
| `w5_aave_bootstrap_proxy_created` | 16,291,126 | 10 | 16,291,135 | 16,291,127-135 | the two resolvable proxy creations + their paired `*Updated` |

The bootstrap window resolves its substrate 74 blocks later (block
16,291,200): at the cursor the Pool, configurator, and data-provider proxies do
not all exist yet, so the resolver's getter calls would return empty. The later
block carries the same proxy addresses and a well-formed reserve set.

## Read surfaces each window records

Six `getLogs` passes per chunk (address provider, oracle, configurator, pool,
scaled-token set, discount config) plus the config-dispatch `eth_call`s:

- data-provider window: `POOL_REVISION()` on the new Pool implementation and
  `CONFIGURATOR_REVISION()` on the new configurator implementation, both at
  block 20,398,674 (two `eth_call`s).
- AddressSet window: `POOL_REVISION()` on the new Pool implementation at
  20,977,092, and the discount pre-pass's `getDiscountPercent` read (two
  `eth_call`s).
- bootstrap window: `POOL_REVISION()` on the Pool implementation at
  16,291,127 and `CONFIGURATOR_REVISION()` on the configurator implementation
  at 16,291,130 (two `eth_call`s). The paired `*Updated` events read the same
  implementations at the same blocks, so the per-chunk memo serves a single
  call per contract. The reserve set is empty, so the scaled-token pass issues
  no `getLogs` (five passes here).

## Goldens

Recorded by `cargo run -p degenbot --features degenbot/sql-ledger --example
generate_aave_config_windows -- --node <uri>`: cassette under
`tests/fixtures/cassettes/wave5/`, statement-ledger + db-dump + seed manifest
under `tests/fixtures/sql_goldens/wave5/`. The recorder runs every window
twice and aborts unless both passes are byte-identical. `--check` regenerates
and exits non-zero on drift; `record_updater_cassette --check` re-verifies the
canonical bytes through the other writer. The replay gate is
`rust/crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs`,
with the `REGENERATE_SQL_GOLDENS=1` arm through the same writer.

## Not covered

- The `AddressSet` rotation arm (S1): no mainnet rotation exists; pinned by
  unit test only.
- The unresolvable `ProxyCreated` abort (S2 loud arm) at block 16,291,136: the
  green replay excludes it; its unit test carries the policy pin.
- The `[21,091,000 .. 26,149,200]` region is not exhaustively scanned; any
  `AddressSet` rotation above block 21,090,999 would not have been observed.
