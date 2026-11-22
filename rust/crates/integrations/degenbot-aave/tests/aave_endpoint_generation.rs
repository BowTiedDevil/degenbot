//! The endpoint-generation replay gate: a committed `AddressSet` POOL
//! rotation must repoint the NEXT chunk's `eth_getLogs` Pool pass at the new
//! endpoint. The run drives the REAL two-chunk loop over a warm-boot market:
//! chunk 1 carries the rotation event (emitted by the `POOL_ADDRESS_PROVIDER`
//! and applied through the ordinary `ContractAddressChanged` apply), chunk 2
//! must fetch the Pool pass from the NEW address. The ledger answers ONLY the
//! filters the tracking policy issues — a chunk 2 Pool pass keyed at the
//! stale (pre-rotation) address is a loud fixture miss, never a silent cursor
//! advance over a stale endpoint generation.
#![expect(clippy::unwrap_used, clippy::panic)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update_on_db, NoProgress, RunSpec, VerifyPolicy};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{
    entry_digest, entry_key, Cassette, CassetteEntry, CassetteProvenance, CassetteResponse,
    CassetteSpan,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;

/// The chain the fixture runs on.
const CHAIN_ID: i64 = 1;
/// The market name the seed stamps.
const MARKET_NAME: &str = "Aave Endpoint Generation Market";
/// The `POOL_ADDRESS_PROVIDER` (the rotation event's emitter — unchanged by
/// the rotation).
const PROVIDER: &str = "0x2f39d218133afab8f2b819b1066c7e434ad94e9e";
/// The pre-rotation `POOL` endpoint (chunk 1's Pool pass target).
const POOL_OLD: &str = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2";
/// The post-rotation `POOL` endpoint (chunk 2's Pool pass target).
const POOL_NEW: &str = "0x1111111111111111111111111111111111111111";
/// The `POOL_CONFIGURATOR` (unchanged by the rotation).
const CONFIGURATOR: &str = "0x64b761d848206f447fe2dd461b0c635ec39ebb27";

/// The chunk-1 window `[100, 109]` and the chunk-2 window `[110, 119]`.
const CHUNK1_START: u64 = 100;
const CHUNK_END: u64 = 119;

/// The event topics `fetch_address_provider_logs` unions (one nested
/// position).
const ADDRESS_PROVIDER_TOPICS: [&str; 6] = [
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7",
    "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465",
    "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d",
    "0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627",
    "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd",
    "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
];
/// The five topic sets `fetch_reserve_init_logs` unions.
const CONFIGURATOR_PASS_TOPICS: [&str; 5] = [
    "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb",
    "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f",
    "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264",
    "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995",
    "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098",
];
/// The eleven topic sets `fetch_pool_logs` unions.
const POOL_PASS_TOPICS: [&str; 11] = [
    "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2",
    "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61",
    "0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699",
    "0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7",
    "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd",
    "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a",
    "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051",
    "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",
    "0xbfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de",
    "0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84",
    "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286",
];
/// The single topic `fetch_oracle_logs` unions (discovery mode here: no
/// `PRICE_ORACLE` row exists, so no address filter).
const ORACLE_PASS_TOPIC: &str =
    "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1";
/// The two topic sets `fetch_discount_config_logs` unions.
const DISCOUNT_PASS_TOPICS: [&str; 2] = [
    "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef",
    "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529",
];

/// The `POOL` right-padded ASCII bytes32 id (the `AddressSet` topic 1).
const POOL_ID_TOPIC: &str = "0x504f4f4c00000000000000000000000000000000000000000000000000000000";
/// `keccak256("AddressSet(bytes32,address,address)")` — the same constant
/// `fetch_address_provider_logs` unions and the decoder dispatches on.
const ADDRESS_SET_TOPIC: &str =
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7";

/// A warm-boot substrate: the market cursor at 99 plus the three endpoint
/// rows (no `PRICE_ORACLE` — the oracle pass stays in chain-wide discovery,
/// which a rotation does not repoint).
fn warm_boot_db() -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, 99)",
            rusqlite::params![CHAIN_ID, MARKET_NAME],
        )
        .unwrap();
        let market_id = conn.last_insert_rowid();
        for (name, address) in [
            ("POOL_ADDRESS_PROVIDER", PROVIDER),
            ("POOL", POOL_OLD),
            ("POOL_CONFIGURATOR", CONFIGURATOR),
        ] {
            // The substrate parses + checksum-validates every row address on
            // read; seed the EIP-55 form the DB convention carries.
            let checksummed =
                degenbot_core::address_utils::address_to_checksum_string(&address.parse().unwrap());
            // The ops parser reads the POOL row's revision per transaction —
            // seed the warm-boot revision the substrate expects.
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                market_id,
                name,
                &checksummed,
                Some(1),
            )
            .unwrap();
        }
    }
    db
}

fn address_set_rotation_log(block: u64, log_index: u64) -> serde_json::Value {
    let word = |addr: &str| format!("0x000000000000000000000000{}", &addr["0x".len()..]);
    serde_json::json!({
        "address": PROVIDER.to_lowercase(),
        "blockHash": "0xc698c93cdbe7ab12221088ec36e9ee86f85e11048876814b84cb0a5d558d3314",
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": "0x",
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [
            ADDRESS_SET_TOPIC,
            POOL_ID_TOPIC,
            word(POOL_OLD),
            word(POOL_NEW),
        ],
        "transactionHash": "0xa17567fa201a78b66c43e6ab178559f8c1d5d308fe944c0bd2c39b5e585097dc",
        "transactionIndex": "0",
    })
}

fn chunk_filter(
    address: Option<&str>,
    topics: serde_json::Value,
    from_block: u64,
    to_block: u64,
) -> serde_json::Value {
    let mut filter = serde_json::Map::new();
    if let Some(address) = address {
        filter.insert("address".to_string(), serde_json::json!(address));
    }
    filter.insert(
        "fromBlock".to_string(),
        serde_json::json!(from_block.to_string()),
    );
    filter.insert(
        "toBlock".to_string(),
        serde_json::json!(to_block.to_string()),
    );
    filter.insert("topics".to_string(), topics);
    serde_json::Value::Object(filter)
}

/// The two-chunk replay ledger. Chunk 1 `[100, 109]`: the provider pass
/// carries the `AddressSet` POOL rotation, every other pass (and the
/// same-chunk supplemental Pool pass keyed at the NEW endpoint) is empty.
/// Chunk 2 `[110, 119]`: the Pool pass is keyed at the NEW endpoint — the
/// entry a stale-generation run never requests, and the one a correct run
/// MUST. Nothing else reaches the wire (empty scaled-token + stkAAVE passes
/// short-circuit; no `PRICE_ORACLE` row keeps the oracle pass in discovery).
#[expect(clippy::too_many_lines)] // one entry per pass per chunk, naturally linear
fn two_chunk_rotation_cassette() -> CassetteReplayTransport {
    let c1_end = CHUNK1_START + 9;
    let c2_start = c1_end + 1;
    let span = CassetteSpan {
        from_block: CHUNK1_START,
        to_block: CHUNK_END,
    };
    let mut entries = std::collections::BTreeMap::new();
    let mut add = |address: Option<&str>,
                   topics: serde_json::Value,
                   from: u64,
                   to: u64,
                   result: serde_json::Value| {
        let params = serde_json::Value::Array(vec![chunk_filter(address, topics, from, to)]);
        let response = CassetteResponse::Success { result };
        entries.insert(
            entry_key("eth_getLogs", &params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    };

    // ── chunk 1 ──
    add(
        Some(&PROVIDER.to_lowercase()),
        serde_json::json!([ADDRESS_PROVIDER_TOPICS.to_vec()]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([address_set_rotation_log(CHUNK1_START, 0)]),
    );
    add(
        Some(&CONFIGURATOR.to_lowercase()),
        serde_json::json!([CONFIGURATOR_PASS_TOPICS.to_vec()]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([]),
    );
    add(
        Some(&POOL_OLD.to_lowercase()),
        serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([]),
    );
    // The same-chunk supplemental pass keyed at the NEW endpoint (the
    // rotation's own chunk still gets one replacement-generation fetch).
    add(
        Some(&POOL_NEW.to_lowercase()),
        serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([ORACLE_PASS_TOPIC]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([DISCOUNT_PASS_TOPICS.to_vec()]),
        CHUNK1_START,
        c1_end,
        serde_json::json!([]),
    );

    // ── chunk 2 — the Pool pass MUST target the rotated endpoint ──
    add(
        Some(&PROVIDER.to_lowercase()),
        serde_json::json!([ADDRESS_PROVIDER_TOPICS.to_vec()]),
        c2_start,
        CHUNK_END,
        serde_json::json!([]),
    );
    add(
        Some(&CONFIGURATOR.to_lowercase()),
        serde_json::json!([CONFIGURATOR_PASS_TOPICS.to_vec()]),
        c2_start,
        CHUNK_END,
        serde_json::json!([]),
    );
    add(
        Some(&POOL_NEW.to_lowercase()),
        serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        c2_start,
        CHUNK_END,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([ORACLE_PASS_TOPIC]),
        c2_start,
        CHUNK_END,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([DISCOUNT_PASS_TOPICS.to_vec()]),
        c2_start,
        CHUNK_END,
        serde_json::json!([]),
    );

    CassetteReplayTransport::new(Cassette {
        schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
        chain_id: 1,
        provenance: CassetteProvenance {
            source: "test".to_string(),
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            span,
        },
        entries,
    })
}

/// A committed `AddressSet` POOL rotation must repoint the NEXT chunk's
/// fetch plan: chunk 2's `eth_getLogs` Pool pass targets the NEW endpoint
/// and the run commits both chunks (the cursor advances only through correct
/// tracking — a stale-generation chunk 2 requests the OLD address, misses the
/// ledger loudly, and errors instead of silently advancing).
#[test]
fn address_set_pool_rotation_fetches_new_pool_logs_next_chunk() {
    let db = warm_boot_db();
    let transport = two_chunk_rotation_cassette();
    let provider = transport.as_alloy_provider();
    let result = run_aave_update_on_db(
        &db,
        RunSpec {
            chain_id: CHAIN_ID,
            market_id: 1,
            to_block: Some(CHUNK_END),
            chunk_size: 10,
            max_block_range: 10_000,
            provider,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        },
    );
    let report = result.unwrap_or_else(|e| {
        panic!(
            "the two-chunk run must commit: {e} — a stale-generation chunk 2 would have \
             requested the OLD pool address and missed the ledger"
        )
    });
    assert_eq!(report.chunks_committed, 2);
    assert_eq!(report.to_block, CHUNK_END);

    // Every ledger entry served exactly once: the request set IS the tracked
    // wire surface, including chunk 2's new-address Pool pass.
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 11,
        "eleven filters issued, none repeated"
    );
    assert_eq!(
        snapshot.per_method.get("eth_getLogs").map(|m| m.served),
        Some(11),
        "chunk 2's Pool pass served from the NEW endpoint (a stale request would be a loud miss)"
    );

    // The committed row carries the rotation; the cursor advanced across both
    // chunks.
    let conn = db.lock();
    let (pool_address, stamp): (String, Option<i64>) = conn
        .query_row(
            "SELECT address, (SELECT last_update_block FROM aave_v3_markets WHERE id = 1) \
             FROM aave_v3_contracts WHERE market_id = 1 AND name = 'POOL'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(pool_address, POOL_NEW, "the rotation committed");
    assert_eq!(
        stamp,
        Some(i64::try_from(CHUNK_END).unwrap()),
        "the cursor advanced across both chunks — never silently over stale tracking"
    );
}
