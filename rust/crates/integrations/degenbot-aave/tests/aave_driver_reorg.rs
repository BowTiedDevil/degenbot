//! The driver's reorg gate: a committed cursor that the chain no longer holds
//! must recover through the REAL run path, not only through the recovery seam
//! (ADR-105 D9).
//!
//! Run 1 commits a chunk on branch A. The ledger is then rebuilt on branch B —
//! the same window, a different block at the fork height — and run 2 is the
//! production `run_aave_update_on_db` call: its startup fence reads the cursor's
//! own header, pays for the session window's header chain only because that
//! header is gone, restores the shared ancestor's checkpoint, and resumes at
//! ancestor + 1. Every assertion below is about the real loop: the recovered
//! cursor is the next commit's expected parent, the abandoned branch's journal
//! row is gone, and the final exact cursor names branch B.
#![expect(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{run_aave_update_on_db, NoProgress, RunSpec, VerifyPolicy};
use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};
use degenbot_db::{
    read_latest_checkpoint_on_conn, read_projection_cursor_on_conn, write_projection_unit_on_conn,
    CursorAnchor, DegenbotDb, DeltaJournalEntry, LogCoordinate, ProjectionCheckpoint,
    ProjectionUnit,
};
use degenbot_rpc::cassette::{
    entry_digest, entry_key, Cassette, CassetteEntry, CassetteProvenance, CassetteResponse,
    CassetteSpan,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;

mod common;

const CHAIN_ID: i64 = 1;
const MARKET_ID: i64 = 1;
const MARKET_NAME: &str = "Aave Driver Reorg Market";
const PROVIDER: &str = "0x2f39d218133afab8f2b819b1066c7e434ad94e9e";
const POOL_OLD: &str = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2";
const POOL_NEW: &str = "0x1111111111111111111111111111111111111111";
const CONFIGURATOR: &str = "0x64b761d848206f447fe2dd461b0c635ec39ebb27";

/// The seeded cursor's block, on both branches' shared prefix.
const SEED_BLOCK: u64 = 100;
/// The height the fork replaces: block 105's hash differs between the branches.
const FORK_AT: u64 = 105;
const RUN1_END: u64 = 109;
const RUN2_END: u64 = 119;

const BRANCH_A: u64 = 0xa1;
const BRANCH_B: u64 = 0xb2;

const ADDRESS_PROVIDER_TOPICS: [&str; 6] = [
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7",
    "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465",
    "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d",
    "0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627",
    "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd",
    "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
];
const CONFIGURATOR_PASS_TOPICS: [&str; 5] = [
    "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb",
    "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f",
    "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264",
    "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995",
    "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098",
];
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
const ORACLE_PASS_TOPIC: &str =
    "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1";
const DISCOUNT_PASS_TOPICS: [&str; 2] = [
    "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef",
    "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529",
];
const POOL_ID_TOPIC: &str = "0x504f4f4c00000000000000000000000000000000000000000000000000000000";
const ADDRESS_SET_TOPIC: &str =
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7";

/// A block hash on the branches' shared prefix: identical on both branches below
/// the fork, so a journal entry written there is still the common ancestor.
fn shared_hash(number: u64) -> String {
    format!("0x{:064x}", 0x5a5a_0000_0000_0000_u64 + number)
}

/// A block hash on one branch. Below the fork height both branches serve the
/// shared hash; at and above it, the branch tag makes the two chains distinct.
fn branch_hash(branch: u64, number: u64) -> String {
    if number < FORK_AT {
        shared_hash(number)
    } else {
        format!("0x{:064x}", (branch << 48) | 0xa1a1_0000_0000_0000 | number)
    }
}

fn header(branch: u64, number: u64) -> serde_json::Value {
    serde_json::json!({
        "number": number.to_string(),
        "hash": branch_hash(branch, number),
        "parentHash": branch_hash(branch, number.saturating_sub(1)),
    })
}

fn rotation_log(branch: u64, block: u64) -> serde_json::Value {
    let word = |address: &str| format!("0x000000000000000000000000{}", &address["0x".len()..]);
    let tx_material = [branch_hash(branch, block), format!("tx{block}")].concat();
    let tx_hash = alloy::primitives::keccak256(tx_material.as_bytes());
    serde_json::json!({
        "address": PROVIDER.to_lowercase(),
        "blockHash": branch_hash(branch, block),
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": "0x",
        "logIndex": "0",
        "removed": false,
        "topics": [
            ADDRESS_SET_TOPIC,
            POOL_ID_TOPIC,
            word(POOL_OLD),
            word(POOL_NEW),
        ],
        "transactionHash": format!("{tx_hash:#x}"),
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

/// One branch's ledger for one window: every header the run can read (the
/// cursor fence, the session window, the terminal height, a candidate's block),
/// the six fetch passes the tracking policy issues, and the rotation event's
/// receipt.
fn branch_cassette(branch: u64, from: u64, to: u64, fork_at: u64) -> CassetteReplayTransport {
    let mut entries = std::collections::BTreeMap::new();
    for number in 90..=to {
        let response = CassetteResponse::Success {
            result: header(branch, number),
        };
        entries.insert(
            entry_key(
                "eth_getBlockByNumber",
                &serde_json::json!([format!("0x{number:x}"), false]),
            ),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    }
    let chain = CassetteResponse::Success {
        result: serde_json::json!("0x1"),
    };
    entries.insert(
        entry_key("eth_chainId", &serde_json::Value::Null),
        CassetteEntry {
            digest: entry_digest(&chain),
            response: chain,
        },
    );

    let mut add = |address: Option<&str>,
                   topics: serde_json::Value,
                   window: (u64, u64),
                   result: serde_json::Value| {
        let params =
            serde_json::Value::Array(vec![chunk_filter(address, topics, window.0, window.1)]);
        let response = CassetteResponse::Success { result };
        entries.insert(
            entry_key("eth_getLogs", &params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    };

    let window = (from, to);
    add(
        Some(&PROVIDER.to_lowercase()),
        serde_json::json!([ADDRESS_PROVIDER_TOPICS.to_vec()]),
        window,
        serde_json::json!([rotation_log(branch, fork_at)]),
    );
    add(
        Some(&CONFIGURATOR.to_lowercase()),
        serde_json::json!([CONFIGURATOR_PASS_TOPICS.to_vec()]),
        window,
        serde_json::json!([]),
    );
    add(
        Some(&POOL_OLD.to_lowercase()),
        serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        window,
        serde_json::json!([]),
    );
    add(
        Some(&POOL_NEW.to_lowercase()),
        serde_json::json!([POOL_PASS_TOPICS.to_vec()]),
        window,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([ORACLE_PASS_TOPIC]),
        window,
        serde_json::json!([]),
    );
    add(
        None,
        serde_json::json!([DISCOUNT_PASS_TOPICS.to_vec()]),
        window,
        serde_json::json!([]),
    );

    let evidence = common::candidate_evidence_entries(&entries, |block| header(branch, block));
    entries.extend(evidence);

    CassetteReplayTransport::new(Cassette {
        schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
        chain_id: 1,
        provenance: CassetteProvenance {
            source: "test".to_string(),
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            span: CassetteSpan {
                from_block: from,
                to_block: to,
            },
        },
        entries,
    })
}

fn contract_rows() -> [(&'static str, &'static str); 3] {
    [
        ("POOL_ADDRESS_PROVIDER", PROVIDER),
        ("POOL", POOL_OLD),
        ("POOL_CONFIGURATOR", CONFIGURATOR),
    ]
}

/// A warm-boot substrate with one committed projection unit at the shared
/// block: the market's cursor, journal, and checkpoint all sit where both
/// branches agree, so a reorg to the fork height has a retained ancestor.
fn seeded_db() -> DegenbotDb {
    let (db, _) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, 99)",
            rusqlite::params![CHAIN_ID, MARKET_NAME],
        )
        .unwrap();
        for (name, address) in contract_rows() {
            let checksummed =
                degenbot_core::address_utils::address_to_checksum_string(&address.parse().unwrap());
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                MARKET_ID,
                name,
                &checksummed,
                Some(1),
            )
            .unwrap();
        }
    }
    let contracts: Vec<degenbot_db::CheckpointContract> = contract_rows()
        .iter()
        .map(|(name, address)| degenbot_db::CheckpointContract {
            name: (*name).to_string(),
            address: degenbot_core::address_utils::address_to_checksum_string(
                &address.parse().unwrap(),
            ),
            revision: Some(1),
        })
        .collect();
    let anchor = ChainAnchor::new(
        1,
        SEED_BLOCK,
        shared_hash(SEED_BLOCK).parse().unwrap(),
        shared_hash(SEED_BLOCK - 1).parse().unwrap(),
    )
    .unwrap();
    let tx = alloy::primitives::keccak256(format!("seed{SEED_BLOCK}").as_bytes());
    let position = LogCoordinate::new(
        anchor,
        LogRef::new(TxRef::new(anchor, tx, 0).unwrap(), 0)
            .tx()
            .hash(),
        0,
        1,
    );
    let entries = [DeltaJournalEntry {
        position,
        generation: None,
        encoding: format!("test-delta@{SEED_BLOCK}"),
    }];
    let mut conn = db.lock();
    let tx = conn.transaction().unwrap();
    write_projection_unit_on_conn(
        &tx,
        &ProjectionUnit {
            market_id: MARKET_ID,
            chain_id: 1,
            encoding_version: degenbot_aave::projection::DELTA_ENCODING_VERSION,
            entries: &entries,
            range_end_block: SEED_BLOCK,
            anchor: CursorAnchor::First {
                replaces_numeric_cursor: Some(99),
            },
            checkpoint: Some(ProjectionCheckpoint {
                encoding_version: degenbot_aave::projection::CHECKPOINT_ENCODING_VERSION,
                generations: &[],
                positions: &[],
                contracts: &contracts,
                gho: None,
            }),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    conn.execute(
        "UPDATE aave_v3_markets SET last_update_block = ?2 WHERE id = ?1",
        rusqlite::params![MARKET_ID, i64::try_from(SEED_BLOCK).unwrap()],
    )
    .unwrap();
    drop(conn);
    db
}

fn run(
    db: &DegenbotDb,
    transport: &CassetteReplayTransport,
    to_block: u64,
    chunk_size: u64,
) -> degenbot_aave::updater::AaveUpdateReport {
    run_aave_update_on_db(
        db,
        RunSpec {
            chain_id: CHAIN_ID,
            market_id: MARKET_ID,
            to_block: Some(to_block),
            chunk_size,
            max_block_range: 10_000,
            provider: transport.as_alloy_provider(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        },
    )
    .unwrap_or_else(|error| panic!("the run must commit through the recovered cursor: {error}"))
}

/// The activation position the market's surviving lineage names for the Pool
/// endpoint. The chunk's reduction journals the rotation's generation at the
/// block the rotation was emitted in, and the unit's checkpoint states the
/// lineage the reducing view held — so this reads the generation a RESUME would
/// continue under, not a row an abandoned branch left behind.
fn restored_generation_names_the_winning_branch(db: &DegenbotDb) -> String {
    let conn = db.lock();
    let checkpoint = read_latest_checkpoint_on_conn(&conn, MARKET_ID)
        .unwrap()
        .expect("the committed unit checkpointed its cursor");
    let entity = degenbot_aave::projection::GenerationEntity::Contract {
        market_id: MARKET_ID,
        role: degenbot_aave::projection::ContractRole::Pool,
    }
    .encode();
    let mut activations: Vec<String> = checkpoint
        .generations
        .iter()
        .filter(|generation| generation.entity == entity)
        .filter_map(|generation| match generation.activation {
            degenbot_db::GenerationActivation::Event(at) => Some(at.anchor().hash().to_string()),
            degenbot_db::GenerationActivation::Seed => None,
        })
        .collect();
    assert_eq!(
        activations.len(),
        1,
        "the Pool endpoint's lineage names exactly one event activation: {activations:?}"
    );
    activations.remove(0)
}

fn pool_address(db: &DegenbotDb) -> String {
    db.lock()
        .query_row(
            "SELECT address FROM aave_v3_contracts WHERE market_id = ?1 AND name = 'POOL'",
            rusqlite::params![MARKET_ID],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_startup_reorg_recovers_through_the_driver_and_commits_on_the_new_branch() {
    let db = seeded_db();
    let first = run(
        &db,
        &branch_cassette(BRANCH_A, SEED_BLOCK + 1, RUN1_END, FORK_AT),
        RUN1_END,
        10,
    );
    assert_eq!(first.chunks_committed, 1);
    let abandoned = read_projection_cursor_on_conn(&db.lock(), MARKET_ID)
        .unwrap()
        .expect("the first run commits a cursor");
    assert_eq!(abandoned.position.anchor().number(), FORK_AT);
    assert_eq!(
        abandoned.position.anchor().hash().to_string(),
        branch_hash(BRANCH_A, FORK_AT),
        "the abandoned cursor names branch A's block"
    );
    assert_eq!(
        pool_address(&db),
        degenbot_core::address_utils::address_to_checksum_string(&POOL_NEW.parse().unwrap()),
        "branch A committed the rotation"
    );

    let second = run(
        &db,
        &branch_cassette(BRANCH_B, SEED_BLOCK + 1, RUN2_END, FORK_AT),
        RUN2_END,
        20,
    );
    assert_eq!(
        second.chunks_committed, 1,
        "the resumed run re-processes the replaced range as one chunk"
    );
    assert!(
        second.total_events_applied >= 1,
        "branch B's rotation event is applied after the recovery"
    );

    let recovered = read_projection_cursor_on_conn(&db.lock(), MARKET_ID)
        .unwrap()
        .expect("the run leaves an exact cursor");
    assert_eq!(
        recovered.position.anchor().hash().to_string(),
        branch_hash(BRANCH_B, FORK_AT),
        "the final exact cursor names branch B"
    );
    assert_eq!(
        recovered.position.anchor().parent_hash().to_string(),
        branch_hash(BRANCH_B, FORK_AT - 1),
        "the new block links to the shared prefix"
    );
    assert_eq!(recovered.range_end_block, RUN2_END);

    let conn = db.lock();
    let (abandoned_rows, last_update): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM aave_v3_delta_journal WHERE market_id = ?1 \
                     AND block_hash = ?2), \
                    (SELECT last_update_block FROM aave_v3_markets WHERE id = ?1)",
            rusqlite::params![MARKET_ID, branch_hash(BRANCH_A, FORK_AT)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        abandoned_rows, 0,
        "the abandoned branch's journal row is pruned with the rewind"
    );
    assert_eq!(
        last_update,
        i64::try_from(RUN2_END).unwrap(),
        "the numeric cursor advances to the new head"
    );
    drop(conn);
    assert_eq!(
        pool_address(&db),
        degenbot_core::address_utils::address_to_checksum_string(&POOL_NEW.parse().unwrap())
    );

    // Branch B ACTIVATES the Pool endpoint's generation: the rotation delta
    // carries the generation its reduction consumed, and the surviving lineage
    // names branch B's block. Recovery rewound the abandoned checkpoint (which
    // named branch A's activation) to the shared ancestor before the replay.
    assert_eq!(
        restored_generation_names_the_winning_branch(&db),
        branch_hash(BRANCH_B, FORK_AT),
        "recovery selected branch B's generation"
    );
    assert_ne!(
        restored_generation_names_the_winning_branch(&db),
        branch_hash(BRANCH_A, FORK_AT),
        "the surviving generation must not name the abandoned branch"
    );
}

/// The branch-A negative control: the SAME gate, on a market that never reorged,
/// names branch A's activation. The gate above therefore passes only because
/// recovery rewound the abandoned generation away — had recovery selected branch
/// A's generation, that assertion would have failed exactly as this one would.
#[test]
fn restored_generation_names_the_winning_branch_is_discriminating() {
    let db = seeded_db();
    let first = run(
        &db,
        &branch_cassette(BRANCH_A, SEED_BLOCK + 1, RUN1_END, FORK_AT),
        RUN1_END,
        10,
    );
    assert_eq!(first.chunks_committed, 1);
    assert_eq!(
        restored_generation_names_the_winning_branch(&db),
        branch_hash(BRANCH_A, FORK_AT),
        "a market on branch A names branch A's activation"
    );
    assert_ne!(
        restored_generation_names_the_winning_branch(&db),
        branch_hash(BRANCH_B, FORK_AT),
        "the gate cannot pass by accepting any activation"
    );
}
