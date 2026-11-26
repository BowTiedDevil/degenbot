//! The three-path projection equivalence suite (ADR-105 D7, D8, D9).
//!
//! Each captured fixture is driven through three DISTINCT starting artifacts and
//! must land the same canonical delta stream and the same relational
//! position/configuration/discount rows:
//!
//! - **incremental** — the fixture's numeric `last_update_block` cursor, the
//!   production backfill entry;
//! - **seed-restore** — an exact `ProjectionCursor` + `ProjectionCheckpoint`
//!   the fixture states: a committed, small resume artifact carrying the
//!   reserve identities, the contract endpoints and revisions, the GHO link,
//!   and the journaled generations — never a historical market export;
//! - **journal replay** — the post-run journal rows, decoded through
//!   [`CanonicalDelta::decode`] and persisted through the ONE delta writer on a
//!   market restored at the fixture's cursor, with no cassette and no chain.
//!
//! The fixtures are the two real config/update cassettes, the real GHO
//! first-mint cassette, and the explicitly synthetic two-refresh fixture (its
//! block hashes are generated and its ledger is built in code; only the two
//! block heights and the two transaction hashes are real).
//!
//! The resume artifact is itself a golden: every run derives it from the
//! fixture's seed rows and the resolved identity space, and the committed file
//! must match byte-for-byte (`REGENERATE_PROJECTION_CHECKPOINTS=1` rewrites
//! it). Its identity family is the one fact a replay cannot re-derive: the
//! users and position rows resolution settles OUTSIDE the chunk transaction
//! (ADR-105 D4), so the fixture states them and the equivalence run proves the
//! journal alone carries every state value on top of them.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use alloy::primitives::{keccak256, I256};
use degenbot_aave::projection::JournaledDelta;
use degenbot_aave::updater::{
    apply_aave_chunk_writes_on_conn, journal_entries, AaveChunkProgress, AaveUpdateReport,
    CanonicalDelta, DeltaEffect, ProgressSink, RunError, RunSpec, VerifyPolicy,
    CHECKPOINT_ENCODING_VERSION, DELTA_ENCODING_VERSION,
};
use degenbot_aave::AaveMarketWriter;
use degenbot_core::canonical_head::{ChainAnchor, LogRef, TxRef};
use degenbot_db::sql_ledger::{dump_tables_golden_json, restore_aave_seed_json};
use degenbot_db::{
    read_journal_on_conn, read_latest_checkpoint_on_conn, write_projection_unit_on_conn,
    CheckpointContract, CheckpointGho, CursorAnchor, DegenbotDb, DeltaJournalEntry,
    ProjectionCheckpoint, ProjectionCheckpointRow, ProjectionUnit,
};
use degenbot_rpc::cassette::{
    entry_digest, entry_key, read_artifact, verify_cassette_bytes, Cassette, CassetteEntry,
    CassetteResponse,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use degenbot_rpc::replay_surface::{
    ReplayCapture, ReplaySurface, AAVE_CONFIG_CAPTURE, AAVE_UPDATE_CAPTURE,
};
use serde_json::{json, Value};
use tempfile::TempDir;

mod common;

const CHAIN_ID: i64 = 1;
const MARKET_ID: i64 = 1;

/// The market-scoped relational families the three paths must agree on: the
/// settlement rows (positions), the configuration rows (reserves, collateral
/// configs, endpoints, e-mode categories), and the discount state (the chain's
/// GHO row and the users' discount columns). The projection tables are excluded
/// by design: each path reaches the cursor through its own evidence.
const RELATIONAL_TABLES: &[&str] = &[
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
    "erc20_tokens",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// The four named fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fixture {
    /// The real update-era cassette (`aave_update_chunk_26130440-26130445`).
    Update,
    /// The real config-era cassette (`aave_config_chunk_19091050-19091059`).
    Config,
    /// The real GHO first-mint cassette (`aave_gho_first_mint_17699253`).
    FirstMint,
    /// The explicitly synthetic two-refresh fixture.
    Synthetic,
}

impl Fixture {
    const ALL: [Self; 4] = [Self::Update, Self::Config, Self::FirstMint, Self::Synthetic];

    const fn id(self) -> &'static str {
        match self {
            Self::Update => "aave_update_chunk_26130440-26130445",
            Self::Config => "aave_config_chunk_19091050-19091059",
            Self::FirstMint => "aave_gho_first_mint_17699253",
            Self::Synthetic => "aave_discount_refresh_synthetic",
        }
    }

    /// The captured span (`from`, `to`) the fixture drives.
    const fn span(self) -> (u64, u64) {
        match self {
            Self::Update => (26_130_440, 26_130_445),
            Self::Config => (19_091_050, 19_091_059),
            Self::FirstMint => (17_699_253, 17_699_253),
            Self::Synthetic => (17_699_253, 17_701_049),
        }
    }

    /// The committed resume artifact (the seed-restore path's starting state).
    fn resume_path(self) -> PathBuf {
        repo_root().join(format!(
            "tests/fixtures/aave_projection/{}.resume.json",
            self.id()
        ))
    }

    /// The fixture's substrate, seeded exactly as the capture's recorder built
    /// it. Each path gets its own file: the paths must not share a market.
    fn prepare(self, dir: &Path, name: &str) -> (DegenbotDb, PathBuf) {
        let path = dir.join(format!("{name}.db"));
        let db = match self {
            Self::Update => seed_real_market(&path, self.span().0, UPDATE_RESERVES),
            Self::Config => seed_real_market(&path, self.span().0, CONFIG_RESERVES),
            Self::FirstMint => seed_first_mint(&path),
            Self::Synthetic => seed_synthetic(&path),
        };
        (db, path)
    }

    /// The fixture's replay ledger.
    fn cassette(self) -> Cassette {
        match self {
            Self::Update => committed_cassette(AAVE_UPDATE_CAPTURE),
            Self::Config => committed_cassette(AAVE_CONFIG_CAPTURE),
            Self::FirstMint => committed_cassette("aave_gho_first_mint_17699253"),
            Self::Synthetic => synthetic_cassette(),
        }
    }
}

// ── the real cassettes' substrate ─────────────────────────────────────────

/// The Aave V3 Ethereum bootstrap block (`activate_aave_market`'s fresh-seed
/// stamp; both real config/update captures were seeded with it).
const AAVE_BOOTSTRAP_BLOCK: i64 = 16_291_070;

const POOL_ADDRESS_PROVIDER: &str = "0x2f39d218133AFaB8F2B819B1066c7E434Ad94E9e";
const POOL: &str = "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2";
const POOL_REVISION: i64 = 11;
const POOL_CONFIGURATOR: &str = "0x64b761D848206f447Fe2dd461b0c635Ec39EbB27";
const CONFIGURATOR_REVISION: i64 = 8;
const PRICE_ORACLE: &str = "0x54586bE62E3c3580375aE3723C145253060Ca0C2";

const GHO: &str = "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f";
const GHO_NAME: &str = "Gho Token";
const GHO_SYMBOL: &str = "GHO";
const GHO_DECIMALS: i64 = 18;

/// The config-era span's reserve assets as `(underlying, a_token, v_token)`,
/// in the recorder's candidate order (address-ascending by underlying: GHO,
/// WETH, USDT).
const CONFIG_RESERVES: [(&str, &str, &str); 3] = [
    (
        "0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f",
        "0x00907f9921424583e7ffBfEdf84F92B7B2Be4977",
        "0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B",
    ),
    (
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8",
        "0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE",
    ),
    (
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        "0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a",
        "0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8",
    ),
];

/// The update-era span's reserve assets as `(underlying, a_token, v_token)`,
/// in the recorder's candidate order (address-ascending by underlying: USDC,
/// WETH, USDT). The aToken/vToken set IS the run's scaled-token OR-set, so the
/// seed must match the recorder's exactly or the pass's getLogs filter misses
/// its recorded entry (a loud fixture gap, by design).
const UPDATE_RESERVES: [(&str, &str, &str); 3] = [
    (
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
        "0x72E95b8931767C79bA4EeE721354d6E99a61D004",
    ),
    (
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "0x4d5F47FA6A74757f35C14fD3a6Ef8E3C9BC514E8",
        "0xeA51d7853EEFb32b6ee06b1C12E6dcCA88Be0fFE",
    ),
    (
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        "0x23878914EFE38d27C4D67Ab83ed1b93A74D4086a",
        "0x6df1C1E379bC5a00a7b4C6e67A203333772f45A8",
    ),
];

/// Seeded aToken/vToken revision (the recorder's `AAVE_SEED_TOKEN_REVISION`).
const SEED_TOKEN_REVISION: i64 = 1;

/// The committed statement-ledger-era seed both real config/update captures
/// read: `activate_aave_market`'s fresh-seed substrate, the warm-boot contract
/// rows with their chain-verified revisions, the span cursor, and the span's
/// reserves (the GHO reserve carrying the vToken link).
fn seed_real_market(path: &Path, span_from: u64, reserves: [(&str, &str, &str); 3]) -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    let conn = db.lock();
    conn.execute(
        "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block)          VALUES (?1, ?2, 1, ?3)",
        rusqlite::params![1i64, "Aave Ethereum Market", AAVE_BOOTSTRAP_BLOCK],
    )
    .unwrap();
    let market_id = conn.last_insert_rowid();
    DegenbotDb::apply_contract_inserted_if_absent_on_conn(
        &conn,
        market_id,
        "POOL_ADDRESS_PROVIDER",
        POOL_ADDRESS_PROVIDER,
        None,
    )
    .unwrap();
    DegenbotDb::get_or_create_erc20_token_on_conn(
        &conn,
        1,
        GHO,
        Some(GHO_NAME),
        Some(GHO_SYMBOL),
        Some(GHO_DECIMALS),
    )
    .unwrap();
    DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, GHO).unwrap();
    for (name, address, revision) in [
        ("POOL", POOL, Some(POOL_REVISION)),
        (
            "POOL_CONFIGURATOR",
            POOL_CONFIGURATOR,
            Some(CONFIGURATOR_REVISION),
        ),
        ("PRICE_ORACLE", PRICE_ORACLE, None),
    ] {
        DegenbotDb::apply_contract_inserted_if_absent_on_conn(
            &conn, market_id, name, address, revision,
        )
        .unwrap();
    }
    DegenbotDb::set_market_last_update_block_on_conn(
        &conn,
        market_id,
        i64::try_from(span_from - 1).unwrap(),
    )
    .unwrap();
    for (underlying, a_token, v_token) in reserves {
        let underlying_id =
            DegenbotDb::get_or_create_erc20_token_on_conn(&conn, 1, underlying, None, None, None)
                .unwrap();
        let a_token_id =
            DegenbotDb::get_or_create_erc20_token_on_conn(&conn, 1, a_token, None, None, None)
                .unwrap();
        let v_token_id =
            DegenbotDb::get_or_create_erc20_token_on_conn(&conn, 1, v_token, None, None, None)
                .unwrap();
        let gho_link = if underlying == GHO {
            Some(DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, GHO).unwrap())
        } else {
            None
        };
        DegenbotDb::apply_reserve_initialized_on_conn(
            &conn,
            market_id,
            underlying_id,
            a_token_id,
            SEED_TOKEN_REVISION,
            v_token_id,
            SEED_TOKEN_REVISION,
            None,
            gho_link,
        )
        .unwrap();
    }
    drop(conn);
    db
}

/// The first-mint capture's committed replay seed (the recorder's
/// `--seed-out`), restored through the case-preserving codec.
fn seed_first_mint(path: &Path) -> DegenbotDb {
    let seed_path =
        repo_root().join("tests/fixtures/cassettes/aave_gho_first_mint_17699253.seed.json");
    let seed = std::fs::read_to_string(&seed_path)
        .unwrap_or_else(|e| panic!("the committed seed {} must exist: {e}", seed_path.display()));
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    {
        let conn = db.lock();
        restore_aave_seed_json(&conn, &seed)
            .expect("the committed seed restores through the case-preserving codec");
    }
    db
}

/// The capture's committed cassette, verified through the drift gate.
fn committed_cassette(capture_id: &str) -> Cassette {
    let surface = ReplaySurface::load_committed(&repo_root())
        .expect("the committed replay-surface authority must load");
    let capture: ReplayCapture = surface
        .capture(capture_id)
        .unwrap_or_else(|error| panic!("the authority governs capture {capture_id}: {error}"))
        .clone();
    let path = repo_root().join(&capture.cassette);
    let bytes = read_artifact(&path)
        .unwrap_or_else(|e| panic!("capture {}: cassette {}: {e}", capture.id, path.display()));
    verify_cassette_bytes(&bytes).expect("the committed cassette must pass the drift gate");
    Cassette::from_json_bytes(&bytes).unwrap()
}
// ── the explicitly synthetic two-refresh fixture ───────────────────────────
//
// Rebuilt in code, exactly as its committed delta-stream golden was recorded:
// one chunk over two GHO vToken mints, each in its own transaction, whose
// refresh reads the committed stkAAVE balance at the second transaction's
// parent. The block hashes are generated (`synthetic_block_hash`); only the two
// block heights and the two transaction hashes are real.

const SYNTHETIC_MARKET_NAME: &str = "Aave Discount Token Generation Market";
const SYNTHETIC_PROVIDER: &str = "0x2f39d218133afab8f2b819b1066c7e434ad94e9e";
const SYNTHETIC_POOL: &str = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2";
const SYNTHETIC_CONFIGURATOR: &str = "0x64b761d848206f447fe2dd461b0c635ec39ebb27";
const SYNTHETIC_GHO: &str = "0x40d16fc0246ad3160ccc09b8d0d3a2cdd286ae77";
const SYNTHETIC_VTOKEN: &str = "0x3050e2f0b4db4a08e0cbb8ce4225eb1c3ffbc159";
const SYNTHETIC_ATOKEN: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SYNTHETIC_T0: &str = "0x4da27a545dcf524758366dd7361f6931ed2df65b";
const SYNTHETIC_USER: &str = "0x4242424242424242424242424242424242424242";

const SYNTHETIC_EARLY_BLOCK: u64 = 17_699_253;
const SYNTHETIC_LATE_BLOCK: u64 = 17_701_049;
const SYNTHETIC_EARLY_TX: &str =
    "0x97858017903ffbe793a4c5789293f170719f5a9d918db52e31e7f74e1129dd75";
const SYNTHETIC_LATE_TX: &str =
    "0x2d99b1e744cd78185373f40938646647a696321ed030e720ff13b75265b1bbfe";

const SYNTHETIC_ADDRESS_PROVIDER_TOPICS: [&str; 6] = [
    "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7",
    "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465",
    "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d",
    "0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627",
    "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd",
    "0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478",
];
const SYNTHETIC_CONFIGURATOR_TOPICS: [&str; 5] = [
    "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb",
    "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f",
    "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264",
    "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995",
    "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098",
];
const SYNTHETIC_POOL_TOPICS: [&str; 11] = [
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
const SYNTHETIC_ORACLE_TOPIC: &str =
    "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1";
const SYNTHETIC_DISCOUNT_TOPICS: [&str; 2] = [
    "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef",
    "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529",
];
const SYNTHETIC_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const SYNTHETIC_MINT_TOPIC: &str =
    "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196";
const SYNTHETIC_REDEEM_TOPIC: &str =
    "0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc";
const SYNTHETIC_STAKED_TOPIC: &str =
    "0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc";
const SYNTHETIC_BALANCE_OF_SELECTOR: &str = "0x70a08231";

fn checksum(addr: &str) -> String {
    degenbot_core::address_utils::address_to_checksum_string(&addr.parse().unwrap())
}

/// The right-padded address word of a log topic.
fn word(addr: &str) -> String {
    format!("0x000000000000000000000000{}", &addr[2..])
}

fn data_word(n: u64) -> String {
    format!("{n:064x}")
}

/// The synthetic captures' block identity: one hash per height, each naming the
/// previous height as its parent, so the exact-position evidence the projection
/// journals is coherent (a block hash names one block, ADR-105 D1).
fn synthetic_block_hash(number: u64) -> String {
    format!("0x{number:064x}")
}

fn seed_synthetic(path: &Path) -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_for_writes(path).unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block)              VALUES (1, ?1, ?2, 1, 99)",
            rusqlite::params![CHAIN_ID, SYNTHETIC_MARKET_NAME],
        )
        .unwrap();
        for (name, address) in [
            ("POOL_ADDRESS_PROVIDER", SYNTHETIC_PROVIDER),
            ("POOL", SYNTHETIC_POOL),
            ("POOL_CONFIGURATOR", SYNTHETIC_CONFIGURATOR),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                MARKET_ID,
                name,
                &checksum(address),
                Some(1),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, ?1, ?2), (2, ?1, ?3), \
             (3, ?1, ?4)",
            rusqlite::params![
                CHAIN_ID,
                checksum(SYNTHETIC_GHO),
                checksum(SYNTHETIC_VTOKEN),
                checksum(SYNTHETIC_ATOKEN)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, v_gho_discount_token) \
             VALUES (1, 1, 2, ?1)",
            rusqlite::params![checksum(SYNTHETIC_T0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
             a_token_revision, v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
             borrow_index, borrow_rate) VALUES (1, 1, 1, 3, 1, 2, 1, '0', '0', '0', '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
             stk_aave_balance, isolation_mode_debt) VALUES (1, 1, ?1, 0, 0, NULL, '0')",
            rusqlite::params![checksum(SYNTHETIC_USER)],
        )
        .unwrap();
        conn.execute(
            "UPDATE aave_v3_markets SET last_update_block = ?1 WHERE id = 1",
            rusqlite::params![i64::try_from(SYNTHETIC_EARLY_BLOCK - 1).unwrap()],
        )
        .unwrap();
    }
    db
}

/// The GHO vToken `Mint` (the pure interest-accrual shape: `value ==
/// balance_increase` → a zero scaled delta, a live `should_refresh_discount`
/// signal).
fn synthetic_mint_log(tx: &str, block: u64, log_index: u64) -> Value {
    json!({
        "address": SYNTHETIC_VTOKEN.to_lowercase(),
        "blockHash": synthetic_block_hash(block),
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": format!("0x{}{}{}", data_word(1_000), data_word(1_000), data_word(1_000_000_000)),
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [
            SYNTHETIC_MINT_TOPIC,
            word("0x1111111111111111111111111111111111111111"),
            word(SYNTHETIC_USER),
        ],
        "transactionHash": tx,
        "transactionIndex": "0",
    })
}

fn synthetic_filter(address: &Value, topics: &[Value], from_block: u64, to_block: u64) -> Value {
    let mut filter = serde_json::Map::new();
    if !address.is_null() {
        filter.insert("address".to_string(), address.clone());
    }
    filter.insert("fromBlock".to_string(), json!(from_block.to_string()));
    filter.insert("toBlock".to_string(), json!(to_block.to_string()));
    filter.insert("topics".to_string(), json!(topics));
    Value::Object(filter)
}

/// The synthetic ledger builder: one entry per distinct wire request.
struct SyntheticLedger {
    entries: std::collections::BTreeMap<String, CassetteEntry>,
    from_block: u64,
    to_block: u64,
}

impl SyntheticLedger {
    fn new(from_block: u64, to_block: u64) -> Self {
        Self {
            entries: std::collections::BTreeMap::new(),
            from_block,
            to_block,
        }
    }

    fn add_get_logs(&mut self, addresses: &[&str], topics: &[Vec<String>], result: Value) {
        let address: Value = match addresses.len() {
            0 => Value::Null,
            1 => json!(addresses[0].to_lowercase()),
            _ => json!(addresses
                .iter()
                .map(|a| a.to_lowercase())
                .collect::<Vec<_>>()),
        };
        let wire_topics: Vec<Value> = topics
            .iter()
            .map(|position| match position.len() {
                0 => json!([]),
                1 => json!(position[0].to_lowercase()),
                _ => json!(position
                    .iter()
                    .map(|t| t.to_lowercase())
                    .collect::<Vec<_>>()),
            })
            .collect();
        let params = json!([synthetic_filter(
            &address,
            &wire_topics,
            self.from_block,
            self.to_block
        )]);
        let response = CassetteResponse::Success { result };
        self.entries.insert(
            entry_key("eth_getLogs", &params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    }

    /// The refresh's `balanceOf(user)` `eth_call` on `token` at `block - 1`
    /// (the pre-block balance contract).
    fn add_balance_of(&mut self, token: &str, user: &str, block: u64, balance: u64) {
        let input = format!(
            "{SYNTHETIC_BALANCE_OF_SELECTOR}000000000000000000000000{}",
            &user[2..]
        );
        let params = json!([
            {"data": input, "to": token.to_lowercase()},
            {
                "blockHash": synthetic_block_hash(block - 1),
                "requireCanonical": true,
            },
        ]);
        let response = CassetteResponse::Success {
            result: json!(format!("0x{:064x}", balance)),
        };
        self.entries.insert(
            entry_key("eth_call", &params),
            CassetteEntry {
                digest: entry_digest(&response),
                response,
            },
        );
    }

    fn scaled_topics() -> Vec<Vec<String>> {
        vec![vec![
            "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956".to_string(),
            "0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b".to_string(),
            "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666".to_string(),
            "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90".to_string(),
            SYNTHETIC_MINT_TOPIC.to_string(),
            SYNTHETIC_TRANSFER_TOPIC.to_string(),
        ]]
    }

    fn stk_aave_topics() -> Vec<Vec<String>> {
        vec![vec![
            SYNTHETIC_REDEEM_TOPIC.to_string(),
            SYNTHETIC_STAKED_TOPIC.to_string(),
            SYNTHETIC_TRANSFER_TOPIC.to_string(),
        ]]
    }

    /// The main pass set every chunk issues: the five unconditioned passes, the
    /// scaled-token pass, the chain-wide discount pass, and the committed
    /// generation's stkAAVE pass.
    fn add_main_passes(&mut self, scaled: Option<Value>, discount: Value, stk_aave: Option<Value>) {
        self.add_get_logs(
            &[SYNTHETIC_PROVIDER],
            &[SYNTHETIC_ADDRESS_PROVIDER_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(
            &[SYNTHETIC_CONFIGURATOR],
            &[SYNTHETIC_CONFIGURATOR_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(
            &[SYNTHETIC_POOL],
            &[SYNTHETIC_POOL_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(&[], &[vec![SYNTHETIC_ORACLE_TOPIC.to_string()]], json!([]));
        self.add_get_logs(
            &[],
            &[SYNTHETIC_DISCOUNT_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            discount,
        );
        if let Some(result) = scaled {
            self.add_get_logs(
                &[SYNTHETIC_ATOKEN, SYNTHETIC_VTOKEN],
                &Self::scaled_topics(),
                result,
            );
        }
        if let Some(result) = stk_aave {
            self.add_get_logs(&[SYNTHETIC_T0], &Self::stk_aave_topics(), result);
        }
    }

    /// The run's terminal evidence: the endpoint's chain id and the headers of
    /// the block the span closes on and its parent (the pre-block stkAAVE seed
    /// read position).
    fn add_terminal_anchor(&mut self) {
        let chain = CassetteResponse::Success {
            result: json!("0x1"),
        };
        self.entries.insert(
            entry_key("eth_chainId", &Value::Null),
            CassetteEntry {
                digest: entry_digest(&chain),
                response: chain,
            },
        );
        for number in [
            self.to_block,
            self.to_block.saturating_sub(1),
            self.from_block.saturating_sub(1),
        ] {
            let header = CassetteResponse::Success {
                result: json!({
                    "number": number.to_string(),
                    "hash": synthetic_block_hash(number),
                    "parentHash": synthetic_block_hash(number.saturating_sub(1)),
                }),
            };
            let params = json!([format!("0x{number:x}"), false]);
            self.entries.insert(
                entry_key("eth_getBlockByNumber", &params),
                CassetteEntry {
                    digest: entry_digest(&header),
                    response: header,
                },
            );
        }
    }

    fn into_cassette(mut self) -> Cassette {
        self.add_terminal_anchor();
        let evidence = common::candidate_evidence_entries(&self.entries, |block| {
            json!({
                "number": block.to_string(),
                "hash": synthetic_block_hash(block),
                "parentHash": synthetic_block_hash(block.saturating_sub(1)),
            })
        });
        self.entries.extend(evidence);
        Cassette {
            schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: degenbot_rpc::cassette::CassetteProvenance {
                source: "test".to_string(),
                recorded_at: "1970-01-01T00:00:00Z".to_string(),
                span: degenbot_rpc::cassette::CassetteSpan {
                    from_block: self.from_block,
                    to_block: self.to_block,
                },
            },
            entries: self.entries,
        }
    }
}

/// The synthetic two-refresh ledger: one chunk over both mints, with the
/// refresh balance reads pinned at each mint's parent.
fn synthetic_cassette() -> Cassette {
    let mut ledger = SyntheticLedger::new(SYNTHETIC_EARLY_BLOCK, SYNTHETIC_LATE_BLOCK);
    ledger.add_main_passes(
        Some(json!([
            synthetic_mint_log(SYNTHETIC_EARLY_TX, SYNTHETIC_EARLY_BLOCK, 339),
            synthetic_mint_log(SYNTHETIC_LATE_TX, SYNTHETIC_LATE_BLOCK, 271),
        ])),
        json!([]),
        Some(json!([])),
    );
    ledger.add_balance_of(SYNTHETIC_T0, SYNTHETIC_USER, SYNTHETIC_EARLY_BLOCK, 0);
    ledger.add_balance_of(
        SYNTHETIC_T0,
        SYNTHETIC_USER,
        SYNTHETIC_LATE_BLOCK,
        909_764_547_577_110_203,
    );
    ledger.into_cassette()
}
// ── the committed resume artifact ─────────────────────────────────────────

/// The fixture's resume state: the exact pre-span cursor, the checkpoint
/// payload's semantic families (the reserve identities, the contract endpoints
/// and revisions, and the GHO link), and the identity space resolution settles
/// outside the chunk transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resume {
    fixture: String,
    provenance: String,
    number: u64,
    hash: String,
    parent_hash: String,
    tx_hash: String,
    tx_index: u64,
    log_index: u64,
    contracts: Vec<CheckpointContract>,
    gho: Option<CheckpointGho>,
    users: Vec<ResumeUser>,
    positions: Vec<ResumePosition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeUser {
    id: i64,
    address: String,
    gho_discount: i64,
    stk_aave_balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumePosition {
    table: String,
    id: i64,
    user_id: i64,
    asset_id: i64,
}

/// A value whose every object states its keys in ascending order, so the
/// serialized artifact is canonical independently of the `serde_json` map
/// backend the build happened to unify: the default `BTreeMap` backend sorts on
/// its own, while the `preserve_order` backend (the simulation and fixture-tools
/// crates) keeps INSERTION order — which would otherwise reorder this file's
/// field list and drift every committed artifact. Sorting by construction makes
/// one byte sequence for both.
fn canonical_json(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonical_json(value));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        other => other,
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a resume artifact states {key}: {value}"))
        .to_string()
}

fn number(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("a resume artifact states {key}: {value}"))
}

fn integer(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("a resume artifact states {key}: {value}"))
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => panic!("a resume artifact states {key} as text or null: {other}"),
    }
}

impl Resume {
    fn to_json(&self) -> String {
        let contracts: Vec<Value> = self
            .contracts
            .iter()
            .map(|contract| {
                json!({
                    "name": contract.name,
                    "address": contract.address,
                    "revision": contract.revision,
                })
            })
            .collect();
        let gho = self.gho.as_ref().map(|gho| {
            json!({
                "gho_id": gho.gho_id,
                "token_id": gho.token_id,
                "v_token_id": gho.v_token_id,
                "gho_token_address": gho.gho_token_address,
                "v_token_address": gho.v_token_address,
                "discount_rate_strategy": gho.discount_rate_strategy,
                "discount_token": gho.discount_token,
            })
        });
        let users: Vec<Value> = self
            .users
            .iter()
            .map(|user| {
                json!({
                    "id": user.id,
                    "address": user.address,
                    "gho_discount": user.gho_discount,
                    "stk_aave_balance": user.stk_aave_balance,
                })
            })
            .collect();
        let positions: Vec<Value> = self
            .positions
            .iter()
            .map(|position| {
                json!({
                    "table": position.table,
                    "id": position.id,
                    "user_id": position.user_id,
                    "asset_id": position.asset_id,
                })
            })
            .collect();
        let value = json!({
            "fixture": self.fixture,
            "provenance": self.provenance,
            "cursor": {
                "number": self.number,
                "hash": self.hash,
                "parent_hash": self.parent_hash,
                "tx_hash": self.tx_hash,
                "tx_index": self.tx_index,
                "log_index": self.log_index,
            },
            "checkpoint": {
                "encoding_version": CHECKPOINT_ENCODING_VERSION,
                "generations": [],
                "positions": [],
                "contracts": contracts,
                "gho": gho,
            },
            "identities": { "users": users, "positions": positions },
        });
        format!(
            "{}\n",
            serde_json::to_string_pretty(&canonical_json(value))
                .expect("the resume artifact serializes")
        )
    }

    fn load(path: &Path) -> Self {
        let text_value = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "the committed resume artifact {} must exist: {e}",
                path.display()
            )
        });
        let value: Value = serde_json::from_str(&text_value).expect("a resume artifact is JSON");
        let cursor = &value["cursor"];
        let checkpoint = &value["checkpoint"];
        let identities = &value["identities"];
        let contracts = checkpoint["contracts"]
            .as_array()
            .expect("a resume artifact states its contract endpoints")
            .iter()
            .map(|contract| CheckpointContract {
                name: text(contract, "name"),
                address: text(contract, "address"),
                revision: contract
                    .get("revision")
                    .and_then(Value::as_u64)
                    .map(|revision| u32::try_from(revision).expect("a revision fits u32")),
            })
            .collect();
        let gho = match &checkpoint["gho"] {
            Value::Null => None,
            gho => Some(CheckpointGho {
                gho_id: integer(gho, "gho_id"),
                token_id: integer(gho, "token_id"),
                v_token_id: gho.get("v_token_id").and_then(Value::as_i64),
                gho_token_address: text(gho, "gho_token_address"),
                v_token_address: optional_text(gho, "v_token_address"),
                discount_rate_strategy: optional_text(gho, "discount_rate_strategy"),
                discount_token: optional_text(gho, "discount_token"),
            }),
        };
        let users = identities["users"]
            .as_array()
            .expect("a resume artifact states its users")
            .iter()
            .map(|user| ResumeUser {
                id: integer(user, "id"),
                address: text(user, "address"),
                gho_discount: integer(user, "gho_discount"),
                stk_aave_balance: optional_text(user, "stk_aave_balance"),
            })
            .collect();
        let positions = identities["positions"]
            .as_array()
            .expect("a resume artifact states its positions")
            .iter()
            .map(|position| ResumePosition {
                table: text(position, "table"),
                id: integer(position, "id"),
                user_id: integer(position, "user_id"),
                asset_id: integer(position, "asset_id"),
            })
            .collect();
        Self {
            fixture: text(&value, "fixture"),
            provenance: text(&value, "provenance"),
            number: number(cursor, "number"),
            hash: text(cursor, "hash"),
            parent_hash: text(cursor, "parent_hash"),
            tx_hash: text(cursor, "tx_hash"),
            tx_index: number(cursor, "tx_index"),
            log_index: number(cursor, "log_index"),
            contracts,
            gho,
            users,
            positions,
        }
    }
}

fn header_key(number: u64) -> String {
    entry_key("eth_getBlockByNumber", &json!([number.to_string(), false]))
}

/// A recorded header answer in the recorder's canonical form.
fn recorded_header(number: u64, hash: &str, parent_hash: &str) -> CassetteEntry {
    let response = CassetteResponse::Success {
        result: json!({
            "number": number.to_string(),
            "hash": hash,
            "parentHash": parent_hash,
        }),
    };
    CassetteEntry {
        digest: entry_digest(&response),
        response,
    }
}

fn pre_span_header(cassette: &Cassette, number: u64) -> Option<Value> {
    let entry = cassette.entries.get(&header_key(number))?;
    let CassetteResponse::Success { result } = &entry.response else {
        panic!("the recorded header at {number} is an answer");
    };
    Some(result.clone())
}

fn hex_of(hash: impl AsRef<[u8]>) -> String {
    format!("0x{}", alloy::hex::encode(hash))
}

/// Derive the fixture's resume artifact: the pre-span cursor its ledger can
/// name, the substrate's contract/GHO families, and the identity space the
/// resolution settled.
fn derive_resume(
    fixture: Fixture,
    substrate: &DegenbotDb,
    identities: &DegenbotDb,
    cassette: &Cassette,
) -> Resume {
    let (number, hash, parent_hash, provenance) = resume_cursor(fixture, cassette);
    let (contracts, gho) = substrate_families(substrate);
    Resume {
        fixture: fixture.id().to_string(),
        provenance,
        number,
        hash,
        parent_hash,
        tx_hash: hex_of(keccak256(format!("{}:resume-tx:{number}", fixture.id()))),
        tx_index: 0,
        log_index: 0,
        contracts,
        gho,
        users: identity_users(identities),
        positions: identity_positions(identities),
    }
}

/// The pre-span cursor the fixture's ledger can name: the recording's own
/// header where the corpus carries one, a fixture-synthesized identity where it
/// does not (the numeric-cursor backfill never read that height).
fn resume_cursor(fixture: Fixture, cassette: &Cassette) -> (u64, String, String, String) {
    let number = fixture.span().0 - 1;
    let recorded = pre_span_header(cassette, number);
    recorded.map_or_else(
        || {
            let hash = hex_of(keccak256(format!("{}:resume:{number}", fixture.id())));
            let parent = hex_of(keccak256(format!("{}:resume:{}", fixture.id(), number - 1)));
            (
                number,
                hash,
                parent,
                "fixture-synthesized: the capture records no header at this height \
                 (the numeric-cursor run never read one)"
                    .to_string(),
            )
        },
        |header| {
            (
                number,
                header["hash"]
                    .as_str()
                    .expect("a recorded header states its hash")
                    .to_string(),
                header["parentHash"]
                    .as_str()
                    .expect("a recorded header states its parent")
                    .to_string(),
                "recorded: the capture's own pre-span header".to_string(),
            )
        },
    )
}

/// The substrate's configuration families at the fixture's cursor: the contract
/// endpoints with their revisions, and the chain's GHO row.
fn substrate_families(substrate: &DegenbotDb) -> (Vec<CheckpointContract>, Option<CheckpointGho>) {
    let contracts = {
        let conn = substrate.lock();
        let mut statement = conn
            .prepare(
                "SELECT name, address, revision FROM aave_v3_contracts \
                 WHERE market_id = ?1 ORDER BY name",
            )
            .unwrap();
        statement
            .query_map(rusqlite::params![MARKET_ID], |row| {
                Ok(CheckpointContract {
                    name: row.get(0)?,
                    address: row.get(1)?,
                    revision: row
                        .get::<_, Option<i64>>(2)?
                        .map(|revision| u32::try_from(revision).unwrap_or(0)),
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let gho = substrate
        .fetch_aave_gho_asset(CHAIN_ID)
        .unwrap()
        .map(|gho| CheckpointGho {
            gho_id: gho.id,
            token_id: gho.token_id,
            v_token_id: gho.v_token_id,
            gho_token_address: gho.gho_token_address,
            v_token_address: gho.v_token_address,
            discount_rate_strategy: gho.v_gho_discount_rate_strategy,
            discount_token: gho.v_gho_discount_token,
        });
    (contracts, gho)
}

/// The users resolution settled, with the discount state it left them at — the
/// transaction-start state the journal's deltas then move.
fn identity_users(identities: &DegenbotDb) -> Vec<ResumeUser> {
    let conn = identities.lock();
    let mut statement = conn
        .prepare(
            "SELECT id, address, gho_discount, stk_aave_balance FROM aave_v3_users \
             WHERE market_id = ?1 ORDER BY id",
        )
        .unwrap();
    statement
        .query_map(rusqlite::params![MARKET_ID], |row| {
            Ok(ResumeUser {
                id: row.get(0)?,
                address: row.get(1)?,
                gho_discount: row.get(2)?,
                stk_aave_balance: row.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The position rows resolution settled (at zero) and the ids the journaled
/// deltas name.
fn identity_positions(identities: &DegenbotDb) -> Vec<ResumePosition> {
    let conn = identities.lock();
    let mut out: Vec<ResumePosition> = Vec::new();
    for (table, name) in [
        ("aave_v3_collateral_positions", "collateral"),
        ("aave_v3_debt_positions", "debt"),
    ] {
        let mut statement = conn
            .prepare(&format!(
                "SELECT id, user_id, asset_id FROM {table} ORDER BY id"
            ))
            .unwrap();
        out.extend(
            statement
                .query_map([], |row| {
                    Ok(ResumePosition {
                        table: name.to_string(),
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        asset_id: row.get(2)?,
                    })
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    out
}

/// Write the fixture's exact cursor + checkpoint into a freshly seeded market:
/// the seed-restore path's one starting artifact.
fn write_restored_market(db: &DegenbotDb, resume: &Resume) {
    let numeric_claim = resume.number - 1;
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE aave_v3_markets SET last_update_block = ?1 WHERE id = ?2",
            rusqlite::params![i64::try_from(numeric_claim).unwrap(), MARKET_ID],
        )
        .unwrap();
    }
    let anchor = ChainAnchor::new(
        u64::try_from(CHAIN_ID).unwrap(),
        resume.number,
        resume
            .hash
            .parse()
            .expect("the resume cursor names a block hash"),
        resume
            .parent_hash
            .parse()
            .expect("the resume cursor names its parent"),
    )
    .unwrap();
    let coordinate = degenbot_db::LogCoordinate::new(
        anchor,
        resume
            .tx_hash
            .parse()
            .expect("the resume artifact names a transaction"),
        resume.tx_index,
        resume.log_index,
    );
    let entries = [DeltaJournalEntry {
        position: coordinate,
        generation: None,
        encoding: format!("fixture-resume@{}", resume.number),
    }];
    let checkpoint = ProjectionCheckpoint {
        encoding_version: CHECKPOINT_ENCODING_VERSION,
        generations: &[],
        positions: &[],
        contracts: &resume.contracts,
        gho: resume.gho.as_ref(),
    };
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        write_projection_unit_on_conn(
            &tx,
            &ProjectionUnit {
                market_id: MARKET_ID,
                chain_id: CHAIN_ID,
                encoding_version: DELTA_ENCODING_VERSION,
                entries: &entries,
                range_end_block: resume.number,
                anchor: CursorAnchor::First {
                    replaces_numeric_cursor: Some(numeric_claim),
                },
                checkpoint: Some(checkpoint),
            },
        )
        .expect("the fixture's resume unit establishes its exact cursor");
        tx.commit().unwrap();
    }
    let conn = db.lock();
    conn.execute(
        "UPDATE aave_v3_markets SET last_update_block = ?1 WHERE id = ?2",
        rusqlite::params![i64::try_from(resume.number).unwrap(), MARKET_ID],
    )
    .unwrap();
}

/// Materialize the identity space resolution settles outside the chunk
/// transaction, exactly as the fixture states it (the ids the journaled
/// deltas name).
fn materialize_identities(db: &DegenbotDb, resume: &Resume) {
    let conn = db.lock();
    for user in &resume.users {
        conn.execute(
            "INSERT OR IGNORE INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
             stk_aave_balance, isolation_mode_debt) VALUES (?1, ?2, ?3, 0, ?4, ?5, '0')",
            rusqlite::params![
                user.id,
                MARKET_ID,
                user.address,
                user.gho_discount,
                user.stk_aave_balance
            ],
        )
        .unwrap();
    }
    for position in &resume.positions {
        let table = match position.table.as_str() {
            "collateral" => "aave_v3_collateral_positions",
            "debt" => "aave_v3_debt_positions",
            other => panic!("a resume artifact names a position relation: {other}"),
        };
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {table} (id, user_id, asset_id, balance, last_index) \
                 VALUES (?1, ?2, ?3, '0', NULL)"
            ),
            rusqlite::params![position.id, position.user_id, position.asset_id],
        )
        .unwrap();
    }
}
// ── the three paths ───────────────────────────────────────────────────────

#[derive(Default)]
struct Collector(Mutex<Vec<AaveChunkProgress>>);

impl ProgressSink for Collector {
    fn report_chunk(&self, progress: &AaveChunkProgress) {
        self.0.lock().unwrap().push(progress.clone());
    }
}

impl Collector {
    fn take(&self) -> Vec<AaveChunkProgress> {
        self.0.lock().unwrap().clone()
    }
}

fn run_span(
    db: &DegenbotDb,
    db_path: &Path,
    cassette: &Cassette,
    fixture: Fixture,
    verify: VerifyPolicy,
) -> (
    Result<AaveUpdateReport, RunError>,
    Vec<AaveChunkProgress>,
    CassetteReplayTransport,
) {
    let transport = CassetteReplayTransport::new(cassette.clone());
    let sink = Arc::new(Collector::default());
    let (from, to) = fixture.span();
    let result = AaveMarketWriter::acquire(db_path, MARKET_ID)
        .unwrap()
        .update_through(
            db,
            RunSpec {
                chain_id: CHAIN_ID,
                market_id: MARKET_ID,
                to_block: Some(to),
                chunk_size: to - from + 1,
                max_block_range: 10_000,
                provider: transport.as_alloy_provider(),
                cancel: Arc::new(AtomicBool::new(false)),
                progress: sink.clone(),
                verify,
                max_chunks: None,
            },
        );
    let chunks = sink.take();
    (result, chunks, transport)
}

fn header_reads(transport: &CassetteReplayTransport) -> u64 {
    transport
        .served_snapshot()
        .per_method
        .get("eth_getBlockByNumber")
        .map_or(0, |method| method.served)
}

struct Outcome {
    deltas: Vec<CanonicalDelta>,
    checkpoint: ProjectionCheckpointRow,
    rows: String,
}

impl Outcome {
    fn of(db: &DegenbotDb, chunks: &[AaveChunkProgress]) -> Self {
        let deltas: Vec<CanonicalDelta> = chunks
            .iter()
            .flat_map(|chunk| chunk.deltas.iter().cloned())
            .collect();
        let checkpoint = {
            let conn = db.lock();
            read_latest_checkpoint_on_conn(&conn, MARKET_ID)
                .expect("the checkpoint read")
                .expect("a unit that journals a delta checkpoints its cursor")
        };
        Self {
            deltas,
            checkpoint,
            rows: relational_snapshot(db),
        }
    }
}

/// The fixture's market-scoped relational rows: positions, configuration, and
/// discount state.
fn relational_snapshot(db: &DegenbotDb) -> String {
    let conn = db.lock();
    dump_tables_golden_json(&conn, RELATIONAL_TABLES).expect("dump the relational families")
}

/// The restore path's ledger: the fixture's cassette, extended with the
/// pre-span header the restored cursor names where the recording carries none.
fn cassette_with_pre_span_header(
    fixture: Fixture,
    cassette: &Cassette,
    resume: &Resume,
) -> Cassette {
    let mut cassette = cassette.clone();
    let key = header_key(resume.number);
    if let Some(entry) = cassette.entries.get(&key) {
        let CassetteResponse::Success { result } = &entry.response else {
            panic!(
                "{}: the recorded pre-span header is an answer",
                fixture.id()
            );
        };
        assert_eq!(
            result["hash"].as_str(),
            Some(resume.hash.as_str()),
            "{}: the resume cursor must name the recording's own pre-span header",
            fixture.id()
        );
        return cassette;
    }
    cassette.entries.insert(
        key,
        recorded_header(resume.number, &resume.hash, &resume.parent_hash),
    );
    cassette
}

/// A restored market's checkpoint states the fixture's contract endpoints, GHO
/// link, and the cursor the fixture names — before any chunk runs.
fn assert_restored_market_states_the_fixture(db: &DegenbotDb, resume: &Resume) {
    let conn = db.lock();
    let row = read_latest_checkpoint_on_conn(&conn, MARKET_ID)
        .expect("the checkpoint read")
        .expect("the fixture's unit checkpointed its cursor");
    assert_eq!(row.encoding_version, CHECKPOINT_ENCODING_VERSION);
    assert_eq!(row.position.anchor().number(), resume.number);
    assert_eq!(row.position.anchor().hash().to_string(), resume.hash);
    assert_eq!(row.contracts, resume.contracts);
    assert_eq!(row.gho, resume.gho);
    let cursor = degenbot_db::read_projection_cursor_on_conn(&conn, MARKET_ID)
        .expect("the cursor read")
        .expect("the fixture's unit established the exact cursor");
    assert_eq!(cursor.position, row.position);
    assert_eq!(cursor.range_end_block, resume.number);
}

fn log_ref(coordinate: degenbot_db::LogCoordinate) -> LogRef {
    let anchor = coordinate.anchor();
    let tx = TxRef::new(anchor, coordinate.tx_hash(), coordinate.tx_index())
        .expect("a journaled coordinate names a real transaction position");
    LogRef::new(tx, coordinate.log_index())
}

/// A delta's exact source position, in the report's own words.
fn position_text(source: LogRef) -> String {
    format!(
        "chain {} block {} ({}) tx {} #{} log {}",
        source.tx().anchor().chain_id(),
        source.tx().anchor().number(),
        hex_of(*source.tx().anchor().hash()),
        hex_of(*source.tx().hash()),
        source.tx().index(),
        source.index()
    )
}

/// A decoded journal row — the refusal names the row's exact source position.
fn decode_journal_entry(entry: &DeltaJournalEntry) -> Result<CanonicalDelta, String> {
    let source = log_ref(entry.position);
    CanonicalDelta::decode(&JournaledDelta {
        source,
        generation: entry.generation.clone(),
        market_id: MARKET_ID,
        encoding: &entry.encoding,
        encoding_version: DELTA_ENCODING_VERSION,
    })
    .map_err(|error| {
        format!(
            "the journal row at {} does not decode: {error}",
            position_text(source)
        )
    })
}

/// The journal-replay path: decode the fixture's journal rows and persist them
/// through the ONE delta writer on the market restored at the fixture's cursor.
fn replay_journal(
    db: &DegenbotDb,
    fixture: Fixture,
    written: &[DeltaJournalEntry],
) -> Vec<CanonicalDelta> {
    let decoded: Vec<CanonicalDelta> = written
        .iter()
        .map(|entry| decode_journal_entry(entry).expect("the journal row decodes"))
        .collect();
    {
        let mut conn = db.lock();
        let tx = conn.transaction().unwrap();
        apply_aave_chunk_writes_on_conn(&tx, MARKET_ID, &decoded, fixture.span().1)
            .expect("the replayed journal applies through the chunk's one writer");
        tx.commit().unwrap();
    }
    decoded
}

/// The journal-replay path's starting artifact: the market's post-run RETAINED
/// journal rows, read back from storage, prefixed by the rows retention pruned
/// away (taken from the unit's own record — the rows it dropped, never
/// different ones). Retention drops evidence outside the reorg window, it never
/// rewrites it.
fn post_run_journal(db: &DegenbotDb, written: &[DeltaJournalEntry]) -> Vec<DeltaJournalEntry> {
    let conn = db.lock();
    let retained = read_journal_on_conn(&conn, MARKET_ID).expect("the journal read");
    assert!(
        retained.len() <= written.len(),
        "the retained journal ({} rows) cannot exceed the written one ({} rows)",
        retained.len(),
        written.len()
    );
    let offset = written.len() - retained.len();
    let mut journal: Vec<DeltaJournalEntry> = written[..offset].to_vec();
    for (entry, row) in written[offset..].iter().zip(&retained) {
        assert_eq!(row.encoding_version, DELTA_ENCODING_VERSION);
        assert_eq!(
            entry.position, row.position,
            "the retained row states the position the unit wrote"
        );
        assert_eq!(entry.encoding, row.encoding);
        assert_eq!(entry.generation, row.generation);
        journal.push(DeltaJournalEntry {
            position: row.position,
            generation: row.generation.clone(),
            encoding: row.encoding.clone(),
        });
    }
    journal
}

/// Why two paths' canonical delta streams disagree, naming the exact source
/// position of the first differing delta.
struct DeltaStreamMismatch {
    index: usize,
    note: &'static str,
    expected: String,
    produced: String,
    expected_source: String,
    produced_source: String,
}

impl std::fmt::Display for DeltaStreamMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: delta #{} differs\n  expected {}: {}\n  produced {}: {}",
            self.note,
            self.index,
            self.expected_source,
            self.expected,
            self.produced_source,
            self.produced
        )
    }
}

/// Compare two paths' canonical delta streams byte-for-byte, reporting the
/// exact source position of the first disagreement.
fn compare_streams(
    expected: &[CanonicalDelta],
    produced: &[CanonicalDelta],
) -> Result<(), DeltaStreamMismatch> {
    let shared = expected.len().min(produced.len());
    for index in 0..shared {
        let (left, right) = (&expected[index], &produced[index]);
        if left.source() != right.source() {
            return Err(DeltaStreamMismatch {
                index,
                note: "the paths journaled different positions at one ordinal",
                expected: left.encode(),
                produced: right.encode(),
                expected_source: position_text(left.source()),
                produced_source: position_text(right.source()),
            });
        }
        if left.encode() != right.encode() {
            return Err(DeltaStreamMismatch {
                index,
                note: "the paths encoded one position's effect differently",
                expected: left.encode(),
                produced: right.encode(),
                expected_source: position_text(left.source()),
                produced_source: position_text(right.source()),
            });
        }
    }
    if expected.len() != produced.len() {
        let index = shared;
        let (expected_encoding, produced_encoding) = (
            expected
                .get(index)
                .map_or_else(String::new, CanonicalDelta::encode),
            produced
                .get(index)
                .map_or_else(String::new, CanonicalDelta::encode),
        );
        return Err(DeltaStreamMismatch {
            index,
            note: "the paths' streams differ in length",
            expected: expected_encoding,
            produced: produced_encoding,
            expected_source: expected.get(index).map_or_else(
                || "stream end".to_string(),
                |delta| position_text(delta.source()),
            ),
            produced_source: produced.get(index).map_or_else(
                || "stream end".to_string(),
                |delta| position_text(delta.source()),
            ),
        });
    }
    Ok(())
}

fn regen_requested() -> bool {
    std::env::var("REGENERATE_PROJECTION_CHECKPOINTS").is_ok_and(|value| value != "0")
}

fn check_or_regen(name: &str, produced: &str, path: &Path) {
    if regen_requested() {
        std::fs::write(path, produced)
            .unwrap_or_else(|e| panic!("write the regenerated {name} at {}: {e}", path.display()));
        eprintln!("regenerated {name} at {}", path.display());
    } else {
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("the committed {name} {} must exist: {e}", path.display()));
        assert_eq!(
            produced, committed,
            "{name} drift: the derived resume artifact must be byte-identical to \
             the committed one; re-run with REGENERATE_PROJECTION_CHECKPOINTS=1 \
             after reviewing the change"
        );
    }
}

// ── the equivalence gate ──────────────────────────────────────────────────

#[expect(clippy::too_many_lines)]
fn assert_three_paths_agree(fixture: Fixture) {
    let dir = TempDir::new().unwrap();
    let cassette = fixture.cassette();

    // The fixture's substrate (pre-span rows) and the identity space
    // resolution settles OUTSIDE the chunk transaction: the pre-commit
    // verification reads a balance the offline corpora never recorded, so the
    // chunk's transaction never opens and nothing but the identities exists.
    let (substrate, _) = fixture.prepare(dir.path(), "substrate");
    let (identities, identities_path) = fixture.prepare(dir.path(), "identities");
    let (refused, refused_chunks, _) = run_span(
        &identities,
        &identities_path,
        &cassette,
        fixture,
        VerifyPolicy {
            verify_chunk: true,
            ..VerifyPolicy::default()
        },
    );
    assert!(
        refused.is_err(),
        "{}: the identity pass must refuse before the chunk's transaction",
        fixture.id()
    );
    assert!(
        refused_chunks.is_empty(),
        "{}: a refused chunk resolves but never reports a commit",
        fixture.id()
    );

    // The committed resume artifact, derived and drift-gated.
    let derived = derive_resume(fixture, &substrate, &identities, &cassette);
    assert_eq!(derived.fixture, fixture.id());
    check_or_regen(
        &format!("{} resume artifact", fixture.id()),
        &derived.to_json(),
        &fixture.resume_path(),
    );
    let resume = Resume::load(&fixture.resume_path());
    assert_eq!(resume, derived);
    assert_eq!(
        resume.number,
        fixture.span().0 - 1,
        "{}: the resume cursor sits immediately before the span",
        fixture.id()
    );

    // (A) incremental: the fixture's numeric cursor.
    let (incremental_market, incremental_path) = fixture.prepare(dir.path(), "incremental");
    let (result, chunks, transport) = run_span(
        &incremental_market,
        &incremental_path,
        &cassette,
        fixture,
        VerifyPolicy::default(),
    );
    result.unwrap_or_else(|error| panic!("{}: the incremental run commits: {error}", fixture.id()));
    let incremental = Outcome::of(&incremental_market, &chunks);
    assert!(
        !incremental.deltas.is_empty(),
        "{}: the fixture reduces at least one canonical delta",
        fixture.id()
    );
    let incremental_headers = header_reads(&transport);

    // (B) seed-restore: the fixture's exact ProjectionCursor + checkpoint.
    let (restore_market, restore_path) = fixture.prepare(dir.path(), "restore");
    write_restored_market(&restore_market, &resume);
    assert_restored_market_states_the_fixture(&restore_market, &resume);
    let restore_cassette = cassette_with_pre_span_header(fixture, &cassette, &resume);
    let (result, chunks, transport) = run_span(
        &restore_market,
        &restore_path,
        &restore_cassette,
        fixture,
        VerifyPolicy::default(),
    );
    result
        .unwrap_or_else(|error| panic!("{}: the seed-restore run commits: {error}", fixture.id()));
    let restored = Outcome::of(&restore_market, &chunks);
    assert_eq!(
        header_reads(&transport),
        incremental_headers + 1,
        "{}: the restored cursor pays for exactly its one recovery-fence header read",
        fixture.id()
    );

    // (C) journal replay: the post-run journal rows, decoded and written.
    let (replay_market, _) = fixture.prepare(dir.path(), "replay");
    write_restored_market(&replay_market, &resume);
    materialize_identities(&replay_market, &resume);
    let journal = post_run_journal(&incremental_market, &journal_entries(&incremental.deltas));
    let replayed = replay_journal(&replay_market, fixture, &journal);
    let replay_rows = relational_snapshot(&replay_market);

    // The streams are byte-identical, delta for delta, at every named fixture.
    compare_streams(&incremental.deltas, &restored.deltas).unwrap_or_else(|mismatch| {
        panic!("{}: incremental vs seed-restore {mismatch}", fixture.id())
    });
    compare_streams(&incremental.deltas, &replayed).unwrap_or_else(|mismatch| {
        panic!("{}: incremental vs journal-replay {mismatch}", fixture.id())
    });

    // The relational position / configuration / discount rows are identical.
    assert_eq!(
        incremental.rows,
        restored.rows,
        "{}: the restored market settles the same relational rows",
        fixture.id()
    );
    assert_eq!(
        incremental.rows,
        replay_rows,
        "{}: the replayed journal settles the same relational rows",
        fixture.id()
    );

    // The checkpoints the two running paths commit state the same resume
    // facts at the same position (the journal sequence differs by the
    // fixture's own resume unit).
    assert_eq!(
        restored.checkpoint.encoding_version,
        incremental.checkpoint.encoding_version
    );
    assert_eq!(
        restored.checkpoint.position,
        incremental.checkpoint.position
    );
    assert_eq!(
        restored.checkpoint.range_end_block,
        incremental.checkpoint.range_end_block
    );
    assert_eq!(
        restored.checkpoint.generations,
        incremental.checkpoint.generations
    );
    assert_eq!(
        restored.checkpoint.positions,
        incremental.checkpoint.positions
    );
    assert_eq!(restored.checkpoint.assets, incremental.checkpoint.assets);
    assert_eq!(
        restored.checkpoint.asset_configs,
        incremental.checkpoint.asset_configs
    );
    assert_eq!(
        restored.checkpoint.contracts,
        incremental.checkpoint.contracts
    );
    assert_eq!(restored.checkpoint.gho, incremental.checkpoint.gho);
    assert_eq!(restored.checkpoint.users, incremental.checkpoint.users);
    assert_eq!(
        restored.checkpoint.user_collaterals,
        incremental.checkpoint.user_collaterals
    );
    assert_eq!(
        restored.checkpoint.emode_categories,
        incremental.checkpoint.emode_categories
    );
}

#[test]
fn the_update_cassette_paths_agree() {
    assert_three_paths_agree(Fixture::Update);
}

#[test]
fn the_config_cassette_paths_agree() {
    assert_three_paths_agree(Fixture::Config);
}

#[test]
fn the_first_mint_cassette_paths_agree() {
    assert_three_paths_agree(Fixture::FirstMint);
}

#[test]
fn the_synthetic_two_refresh_fixture_paths_agree() {
    assert_three_paths_agree(Fixture::Synthetic);
}

#[test]
fn every_named_fixture_has_a_committed_resume_artifact() {
    for fixture in Fixture::ALL {
        let resume = Resume::load(&fixture.resume_path());
        assert_eq!(resume.fixture, fixture.id());
        assert_eq!(resume.number, fixture.span().0 - 1);
        assert!(
            !resume.contracts.is_empty(),
            "{}: a resume artifact states the market's contract endpoints",
            fixture.id()
        );
        assert!(
            !resume.users.is_empty() && !resume.positions.is_empty(),
            "{}: a resume artifact states the identity space the journal names",
            fixture.id()
        );
    }
}

/// The resume artifact's bytes are CANONICAL and backend-independent: the
/// committed file is reproduced from its own loaded content, and every object
/// states its keys in ascending order at every depth. A build that unifies
/// `serde_json`'s `preserve_order` backend must produce the same bytes as one
/// that does not — a top-level-only or backend-default order would drift here.
#[test]
fn a_committed_resume_artifact_round_trips_through_the_canonical_serializer() {
    for fixture in Fixture::ALL {
        let path = fixture.resume_path();
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: the committed artifact reads: {e}", path.display()));
        let resume = Resume::load(&path);
        assert_eq!(
            resume.to_json(),
            committed,
            "{}: the artifact round-trips byte-identically through the canonical serializer",
            fixture.id()
        );

        let text = resume.to_json();
        let ordered = |keys: &[&str]| -> Vec<usize> {
            keys.iter()
                .map(|key| {
                    text.find(key)
                        .unwrap_or_else(|| panic!("{}: the artifact states {key}", fixture.id()))
                })
                .collect()
        };
        let ascending = |positions: Vec<usize>| positions.windows(2).all(|pair| pair[0] < pair[1]);
        assert!(
            ascending(ordered(&[
                "\"checkpoint\":",
                "\"cursor\":",
                "\"fixture\":",
                "\"identities\":",
                "\"provenance\":",
            ])),
            "{}: the artifact's top-level keys are canonical",
            fixture.id()
        );
        assert!(
            ascending(ordered(&[
                "\"contracts\":",
                "\"encoding_version\":",
                "\"generations\":",
                "\"gho\":",
            ])),
            "{}: the checkpoint's own keys are canonical",
            fixture.id()
        );
        assert!(
            ascending(ordered(&[
                "\"hash\":",
                "\"log_index\":",
                "\"number\":",
                "\"parent_hash\":",
                "\"tx_hash\":",
                "\"tx_index\":",
            ])),
            "{}: the cursor's own keys are canonical",
            fixture.id()
        );
        let row_keys_ascend = |row: &str, keys: &[&str]| -> bool {
            let positions: Vec<usize> = keys
                .iter()
                .map(|key| {
                    row.find(key)
                        .unwrap_or_else(|| panic!("{}: a row states {key}", fixture.id()))
                })
                .collect();
            positions.windows(2).all(|pair| pair[0] < pair[1])
        };
        let identities_start = text
            .find("\"identities\": {")
            .unwrap_or_else(|| panic!("{}: the artifact states its identities", fixture.id()));
        let identities = &text[identities_start..];
        assert!(
            row_keys_ascend(
                identities,
                &["\"asset_id\":", "\"id\":", "\"table\":", "\"user_id\":"],
            ),
            "{}: the position rows state their keys canonically",
            fixture.id()
        );
        let users_start = identities
            .find("\"users\": [")
            .unwrap_or_else(|| panic!("{}: the artifact states its users", fixture.id()));
        assert!(
            row_keys_ascend(
                &identities[users_start..],
                &[
                    "\"address\":",
                    "\"gho_discount\":",
                    "\"id\":",
                    "\"stk_aave_balance\":",
                ],
            ),
            "{}: the user rows state their keys canonically",
            fixture.id()
        );
    }
}

/// The deliberate one-delta mutation: a stream that differs at exactly one
/// delta is refused, and the refusal names that delta's exact source position
/// (its block hash and its log index), so a fault is attributed rather than
/// merely detected.
#[test]
fn a_one_delta_mutation_fails_with_its_exact_source_position() {
    let dir = TempDir::new().unwrap();
    let fixture = Fixture::Update;
    let (db, db_path) = fixture.prepare(dir.path(), "mutation");
    let (result, chunks, _) = run_span(
        &db,
        &db_path,
        &fixture.cassette(),
        fixture,
        VerifyPolicy::default(),
    );
    result.expect("the incremental run commits");
    let deltas: Vec<CanonicalDelta> = chunks
        .iter()
        .flat_map(|chunk| chunk.deltas.iter().cloned())
        .collect();

    let target = deltas
        .iter()
        .position(|delta| matches!(delta.effect(), DeltaEffect::ScaledBalance { .. }))
        .expect("the span reduces a scaled-balance delta");
    let source = deltas[target].source();
    let DeltaEffect::ScaledBalance {
        subject,
        balance_delta,
        index,
    } = deltas[target].effect().clone()
    else {
        unreachable!("selected above");
    };
    let mut mutated = deltas.clone();
    mutated[target] = CanonicalDelta::new(
        source,
        deltas[target].generation(),
        DeltaEffect::ScaledBalance {
            subject,
            balance_delta: balance_delta + I256::try_from(1_i64).unwrap(),
            index,
        },
    );

    let mismatch = compare_streams(&deltas, &mutated)
        .expect_err("a stream with one mutated delta cannot agree");
    assert_eq!(
        mismatch.index, target,
        "the refusal names the delta's ordinal"
    );
    assert_eq!(
        mismatch.expected_source, mismatch.produced_source,
        "the mutation moved the effect, not the position"
    );
    assert!(
        mismatch
            .produced_source
            .contains(&hex_of(*source.tx().hash())),
        "the refusal names the mutated delta's transaction: {mismatch}"
    );
    assert!(
        mismatch
            .produced_source
            .contains(&format!("log {}", source.index())),
        "the refusal names the mutated delta's block-wide log index: {mismatch}"
    );

    // The same single mutation, delivered through the journal's own encoding,
    // is refused at that row's exact source position.
    let written = journal_entries(&deltas);
    let mut mutated_row = written[target].clone();
    mutated_row.encoding = mutated_row.encoding.replace(" effect=", " effectx=");
    let error = decode_journal_entry(&mutated_row)
        .expect_err("a journal row whose encoding disagrees with the vocabulary is refused");
    assert!(
        error.contains(&position_text(source)),
        "the decode refusal names the row's exact source position: {error}"
    );
}
