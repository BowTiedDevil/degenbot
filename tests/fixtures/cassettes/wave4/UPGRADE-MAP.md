# Upgrade-span map: the wave-4 corpora provenance

Chain-derived on 2026-10-07: eth_getLogs over the Pool proxy, the
PoolAddressProvider, and all 134 scaled-token proxies, deployment to
tip, 400k-block chunks, no gaps (endpoint: an unauthenticated public
archive gateway). The five wave-4 windows and the unrecordable-window
adjudication below are selected from this map; the corpora are its
commit-proven record. Discovery method: read-only scan, no recalled
upgrade history. What follows is the discovery output verbatim.


Read-only discovery. No tracked file edited. Endpoint used: https://gateway.tenderly.co/public/mainnet (unauthenticated).

## (A) Risky-transition surface table

| # | Site (file:fn) | Behavior a refactor MUST preserve | Observable pin |
|---|---|---|---|
| 1 | apply.rs AaveChunkEvent::Upgraded -> DegenbotDb::apply_upgraded_on_conn (write/pools.rs:286) | Bump ONLY the matching column: a_token_revision when is_a_token else v_token_revision; missing asset row => DbError::MissingRow rolls back the chunk. When deprecated_gho_token_id=Some, clear aave_gho_tokens.v_gho_discount_token+v_gho_discount_rate_strategy and bulk UPDATE aave_v3_users SET gho_discount=0 WHERE market_id=<asset.market> AND gho_discount!=0. | Dump columns aave_v3_assets.a_token_revision/v_token_revision; aave_gho_tokens cleared cols; aave_v3_users.gho_discount. Ledger: exactly one UPDATE aave_v3_assets SET `<col>` = ?, and on deprecation the two GHO UPDATEs + one bulk user UPDATE. |
| 2 | substrate.rs record_asset_revision (overlay) | In-place revision bump in the cached AssetRow; address indexes keep pointing at the row id, so every lookup re-reads the fresh revision. mark_gho_dirty() on deprecation forces the next per-tx gho_asset() re-query. | Read-your-own-writes: the per-tx vtoken_revision read after an in-chunk Upgraded returns the NEW value; a stale overlay shows old => wrong discount path. Probe: same-chunk upgrade + later GHO tx. |
| 3 | config_dispatch.rs RevisionMemo key (Address,[u8;4],u64) | Memo key MUST include the block lane; scope is one chunk apply (process_chunk_on_conn). Same impl+selector at two blocks => two entries. | Recorded eth_call entry count: N distinct (impl,selector,block) => N entries, never fewer; a block-blind key collapses them. |
| 4 | config_dispatch.rs resolve_upgraded | Resolution order: a_token row first, then v_token; neither => Err("Unreachable"). Revision fn ATOKEN_REVISION() for aToken, DEBT_TOKEN_REVISION() for vToken, read at the tx block. Deprecation fires only when proxy == gho_asset.v_token_address AND rev>=4. | The exact eth_call target = the NEW implementation address (not the proxy); "to" in the cassette. Applied revision + deprecation flag in the dump. |
| 5 | config_dispatch.rs resolve_contract_revision_updated (PoolUpdated/PoolConfiguratorUpdated) | RPC *_REVISION() on the NEW address; update ONLY aave_v3_contracts.revision, never address (parity gate). | aave_v3_contracts.revision for name POOL/POOL_CONFIGURATOR; address unchanged in dump. |
| 6 | config_dispatch.rs dispatch_config_events intra-dispatch apply | Apply each config event to conn AS dispatched (logIndex order) so a later event sees an earlier apply. | Ledger statement ORDER within the chunk (ReserveInitialized asset row before a later config that reads it). |
| 7 | run.rs per-tx vtoken_revision re-resolve (process.rs step (a)) | Re-resolve the GHO vToken revision from conn per tx (read-your-own-writes), NOT a chunk-start snapshot. | A tx AFTER an in-chunk Upgraded takes the deprecation path (snapshot would serve the old rev => path #2 discount RPC). Probe: upgrade tx then a GHO tx same chunk. |
| 8 | run.rs bootstrap pass (fetch.rs bootstrap_pool_contracts) | Cold boot fetches AP ProxyCreated over [from_block, from_block+2000], applies POOL/POOL_CONFIGURATOR idempotently; warm boot is a no-op. | Ledger: on cold boot the AP getLogs + 2 *_REVISION eth_calls appear; on warm boot they must not. Cassettes must seed warm. |
| 9 | ADDITIONAL (missed): EIP_1967_IMPLEMENTATION_SLOT read in dispatch_reserve_initialized | ReserveInitialized resolves aToken+vToken impls via eth_getStorageAt at the event block, then *_REVISION() on the impls. NOT memoized (getStorageAt has no multicall shape). | Two eth_getStorageAt entries per ReserveInitialized + the two impl revision eth_calls; a cassette missing them fails a later reserve-init chunk. |
| 10 | ADDITIONAL: same-chunk scaled-token re-fetch (run.rs step (b)/(c)) | An in-chunk ReserveInitialized/DiscountTokenUpdated triggers an EXTRA scaled/stkAAVE getLogs for the chunk range, de-duped by (block,log_index). | Extra eth_getLogs entry in the cassette for that chunk; removing it silently drops Mint/Burn for the new asset. |
| 11 | ADDITIONAL: resolve_contract_revision_updated & ProxyCreated both ride the memo but sit in DIFFERENT call sites (chunk dispatch vs bootstrap) — each phase owns its own RevisionMemo instance. | The block lane keeps correctness across instances; a shared global memo would be wrong across chunks. | Two separate memo instances in the ledger trace shape (bootstrap rev read vs chunk rev read). |

## (B) Real upgrade map (Ethereum mainnet, chain 1)

Scan completeness: eth_getLogs over the PoolAddressProvider, the Pool proxy, and all 134 scaled-token proxies, [16,291,070 .. 26,142,880] (tip at probe), in 400k-block chunks via https://gateway.tenderly.co/public/mainnet. NO GAPS. 319 Upgraded logs + 10 Pool-proxy Upgraded + 27 AP config events.

Proxy set derivation: 67 reserves from Pool.getReservesList(); ReserveInitialized scans on POOL_CONFIGURATOR give 67 aTokens + 67 vTokens = 134 scaled proxies; the set equals the live getReserveData set exactly (current-only 0, historical-only 0). Plus POOL proxy 0x8787... and POOL_ADDRESS_PROVIDER 0x2f39... .

aToken/vToken Upgraded events (block, tx, proxy-class):
| block | tx | new impl (aToken / vToken) | note |
|---|---|---|---|
| 18,042,111 | 0xe9ef33ae... | GHO vToken 0x786d...d04b impl 0x7aa606... | rev 1->2 |
| 18,777,806 | 0x090beb39... | GHO vToken impl 0x20cb2f... | rev 2->3 |
| 18,870,593 | 0x338149cd... | aEthAAVE aToken 0xa700b4... impl 0x366ae3... | rev 1->2 (single) |
| 22,839,362 | 0x6f45f51f... | 48 reserves: 96 Upgraded (aTokens ->0x97f5b9..., vTokens ->0xb58ed8...); GHO vToken rev 3->4 | DEPRECATION + Pool proxy + PoolConfigurator + PoolDataProvider same tx |
| 23,088,584 | 0xa17567fa... | 50 reserves: 100 Upgraded; GHO vToken rev 4->5 | PoolUpdated same block |
| 24,247,927 | 0x675614a8... | 60 reserves: 120 Upgraded; GHO vToken rev 5->6 | PoolUpdated + PoolConfiguratorUpdated same tx |

Pool proxy (0x8787...) EIP-1967 Upgraded blocks: 17,214,196; 18,979,695; 20,398,674; 20,920,979; 20,977,092; 21,917,056; 22,839,362; 23,088,584; 24,247,927; 25,199,939.
PoolAddressProvider config events: PoolUpdated/PoolConfiguratorUpdated/PoolDataProviderUpdated at 16,291,127/16,291,130 (bootstrap), 20,398,674, 20,920,979, 21,917,056, 22,839,362, 23,088,584, 24,247,927, 25,199,939; PriceOracleUpdated at 16,291,126.

Distinct scaled-upgrade blocks: 18042111, 18777806, 18870593, 22839362, 23088584, 24247927. Closest distinct-block gap: 18042111->18777806 = 735,695 blocks (the 249,222 gap 22839362->23088584 and 3,968,769 before it are larger). TIGHTEST same-block multi-upgrade: 22839362 (96), 23088584 (100), 24247927 (120).

## (C) Cassette window plan

Recorded cassettes home: tests/fixtures/cassettes/wave4/`<name>`.json ; SQL goldens: tests/fixtures/sql_goldens/wave4/`<name>`.{statement-ledger,db-dump}.json. Chunk formula (run.rs): from_block=last_update_block+1; chunk_end=min(to_block, working_start+chunk_size-1). LogFetcher max_blocks_per_request=2000 but the LOOP chunk_size is independent and is what defines boundaries.

| Window | name | cursor (last_update_block) | chunk_size | to_block | chunks | transition |
|---|---|---|---|---|---|---|
| W1 | w4_aave_atoken_upgrade_in_chunk | 18,870,590 | 5 | 18,870,599 | 18,870,591-595 (upg 593 interior), 596-599 | single aToken rev 1->2, in-chunk, discount live |
| W2 | w4_aave_gho_deprecation_at_chunk_boundary | 22,839,357 | 5 | 22,839,366 | 22,839,358-362 (upg = chunk_end), 363-366 | GHO vToken rev 3->4 DEPRECATION + 96 upgrades AT boundary |
| W3 | w4_aave_upgrade_plus_same_block_config | 23,088,580 | 5 | 23,088,589 | 23,088,581-585 (upg 584 interior), 586-589 | 100 upgrades + PoolUpdated same block |
| W4 | w4_aave_multi_upgrade_one_chunk | 24,247,923 | 5 | 24,247,931 | 24,247,924-928 (upg 927 interior), 929-931 | 120 Upgraded + PoolUpdated + PoolConfiguratorUpdated one tx |
| W5 | w4_aave_pre_upgrade_control | 22,839,352 | 5 | 22,839,361 | 22,839,353-357, 358-361 | NO upgrade (GHO vToken rev 3, discount live) — A/B control for W2 |
| W6 | w4_aave_gho_deprecation_then_discount_read_in_chunk | 22,839,357 | 5,880 | 22,845,245 | 22,839,358-22,845,237 (upg 22,839,362 interior; GHO vToken Mint 22,845,229 interior), 22,845,238-245 | GHO vToken rev 3->4 DEPRECATION interior + the chain's FIRST post-upgrade GHO vToken Mint (a borrow) IN THE SAME CHUNK — the per-tx revision re-resolve pin. The 5,880-block chunk is forced by the chain: 22,845,229 is the first GHO vToken Mint/Burn after the deprecation tx (5,867 blocks of vToken silence, measured by address-filtered getLogs); the span carries ~9.7k scaled-set + ~8.9k pool logs (~17 MB cassette). W2's boundary shape cannot pin surface #7 (a chunk-start snapshot equals the per-tx re-resolve once the upgrade tx is the chunk's last); W6 pins it. |

Read surfaces each window MUST record: 6 getLogs passes (pool, configurator, scaled-token set, address-provider, discount-config, oracle); the EIP-1967 eth_getStorageAt pair per ReserveInitialized (none in these windows); the *_REVISION() eth_calls for every Upgraded (target = new impl, block pin); PoolUpdated/PoolConfiguratorUpdated *_REVISION(); any getDiscountPercent from the discount pre-pass; the scaled-token Upgraded logs themselves; and the Deferred/multi-row ReserveDataUpdated values flowing into the dump. Warm-boot seed => NO bootstrap getLogs.

Estimated requests/chunk: 6 getLogs + (W1: 1 rev eth_call; W2: 96 rev calls but memoized per (impl,block): 2 distinct impls [aToken+vToken shared across 48 reserves] => 2 eth_calls, block-pinned, + the GHO rev; W3: 2; W4: 3 incl. pool/configurator) + 0 discount calls. So ~7-9 round trips/chunk; 2 chunks => 14-18/window. Bytes: W1 ~3-8 KB/chunk (few logs); W2/W3/W4 ~55-80 KB for the mass block's scaled getLogs + small second chunk => ~60-100 KB/window. (W4 3.9M-block premium avoided: only the window's blocks are fetched.)

Negative probes (the pin that must bite):
- W1: mutate the recorded post-upgrade ATOKEN_REVISION answer (2->1) => dump a_token_revision stays 1 => dump gate LOUD; serve count unchanged (same keys) => only the dump bites.
- W2 (boundary): swap the pre-upgrade rev answer into the post-upgrade key => applied rev wrong; and reorder so the memo's block lane is bypassed (duplicate one key at two blocks) => cassette verify/digest LOUD.
- W3 (same-block config): delete the PoolUpdated entry => ContractRevisionUpdated missing => ledger + dump LOUD.
- W4 (multi-upgrade): mutate ONE of the 120 vToken answers => exactly that asset's v_token_revision differs; all others identical => proves per-asset resolution.
- Deprecation pin: seed GHO vToken rev < 4 in the DB while the cassette answers 4 => deprecation must still fire; flip the recorded answer to 3 => deprecation must NOT fire (no GHO clear, no bulk reset) => dump LOUD.
- W5 (control): assert Upgraded apply count = 0 and revisions unchanged; injecting an Upgraded log for an unknown proxy must Err (Unreachable) => run fails LOUD.
- W6 (read-your-own-writes): mutate the recorded GHO DEBT_TOKEN_REVISION answer (4->3; the GHO vToken's new impl is unique to it, so the memo mutation touches one asset) => the post-upgrade borrow's discount pre-pass reads rev 3 and takes the path-#2 branch: a getDiscountPercent(user) eth_call the corpus does not record => replay fails LOUD (transport method-not-found + a served/request gap). The unmutated cassette must stay green with ZERO getDiscountPercent entries — the recorded proof that the per-tx re-resolve served the applied revision. A chunk-start revision snapshot would produce exactly the mutated replay's RPC shape.

## (D) Refactor harness shape

Replay: read cassette bytes -> verify_cassette_bytes (drift gate) -> Cassette::from_json_bytes -> CassetteReplayTransport::new -> as_alloy_provider() -> run_aave_update_on_db(LedgerDb::open_for_writes(path).db(), chain, market_id, Some(to_block), chunk_size, provider, cancel, NoProgress, false, None, false, None) over a temp DB seeded exactly as the recorder (activate_aave_market_on_conn market row + warm-boot POOL/POOL_CONFIGURATOR/PRICE_ORACLE rows + the span's reserve assets, cursor = window cursor). Provider injection already exists (run_aave_update_on_db takes the pre-opened handle; the transport is the injected provider). Chunk loop runs one Transaction per chunk; the ledger accumulates all chunks' statements in order. Assert: (1) report.chunks_committed == expected; (2) ledger_golden_json(records) byte-equal to the committed statement-ledger golden; (3) dump_tables_golden_json(conn, DUMP_TABLES) byte-equal to the db-dump golden; (4) ServedSnapshot.served == expected round-trip literal (the N+1 tripwire); (5) the three transition probes: boundary (W2 revision value), read-your-own-writes (W1/W2 later-tx discount path), deprecation (W2 GHO clear). REGENERATE_SQL_GOLDENS=1 regenerates through the same writer for byte-identical drift.

## (E) Feasibility + order

Endpoints:
- ETHEREUM_ARCHIVE_NODE_HTTP_URI = http://localhost:8545/ => curl HTTP 000, no listener. UNUSABLE.
- lb.drpc.live/ethereum/`<key>`: JSON-RPC 200 for eth_blockNumber; historical eth_call/eth_getStorageAt OK; but eth_getLogs errors: "ranges over 10000 blocks are not supported on free plan" (even a 100-block range over a busy contract) AND "Unknown state. First available state is 1" for pre-recent blocks. INSUFFICIENT for historical getLogs.
- Known public endpoints blocked/limited: publicnode 403, ankr needs key, 1rpc/drpc.org/blastapi 403 (Cloudflare 1010), merkle 429, flashbots 504, securerpc/payload DNS.
- WORKING (unauthenticated): https://gateway.tenderly.co/public/mainnet — full-history getLogs (400k blocks/req OK), historical eth_call, eth_getBalance. Use this unless the manager supplies an archive key.
dRPC key from tests.env: AnKobTqXZUNgunS9ObwnNVyyfCj-whMR8b2mLtu1AWF8.

Cost: all windows are small spans (5-9 blocks) => seconds per window once warm-seeded. W2's 96-log scaled getLogs ~0.4s. No recording window is unrecordable, EXCEPT the literal "two distinct upgrade blocks in one window": tightest pair 18,777,806 & 18,870,593 (92,787 blocks, ~1.4M scaled logs / ~16.8 MB just for scaled logs, plus 47 chunks x 6 getLogs) and 22839362 & 23088584 (249,222 blocks). PREREQUISITE MISSING: an archive endpoint that serves multi-100k-block sparse getLogs at acceptable cost AND a policy for ~GBs of cassette; NO synthetic substitution. Substituted with W4 (multi-upgrade in ONE chunk), which exercises the same two-Upgraded-events-one-chunk code path.

Recommended record order: W5 (cheapest, control) -> W1 -> W3 -> W4 -> W2 (deprecation, largest). Regenerate goldens after each.

Recording prerequisite (all windows): the recorder's warm-boot substrate resolution (resolve_aave_substrate) scans the span's POOL logs for reserve candidates; the upgrade windows' Pool logs are sparse (W1: ~2 logs, no reserves), so it errors with "no market reserves found". Prerequisite: seed warm-boot reserve rows from the CHAIN's current getReserveData set (67 reserves) instead of the span's Pool logs, or widen the resolution window; otherwise every window fails at substrate resolution before recording.
