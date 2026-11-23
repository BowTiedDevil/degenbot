//! The discount-token-generation replay gate: an actual
//! `Some(T0) -> Some(T1)` discount-token replacement must (a) fetch EVERY
//! generation's stkAAVE log stream over the replacement chunk (the committed
//! token + each `DiscountTokenUpdated` introduction, intermediate
//! generations included), (b) resolve the post-dispatch token for the
//! same-tx discount refresh, and (c) invalidate the cached
//! `stk_aave_balance` values the old generation seeded, inside the chunk's
//! own transaction, so the next read/backfill reseeds from the new ledger.
//! The initial `None -> stkAAVE` set carries no balance invalidation and
//! one uncovered supplemental fetch for the introduced token (the writer's
//! old-token read adds no invalidation write on that path). The ledger
//! answers ONLY the filters the tracking policy issues — a stale-generation
//! run misses the ledger loudly, never silently advancing the cursor over a
//! mixed-generation chunk.

#![expect(clippy::unwrap_used, clippy::panic)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use degenbot_aave::updater::{
    run_aave_update_on_db, AaveUpdateReport, NoProgress, RunSpec, VerifyPolicy,
};
use degenbot_db::DegenbotDb;
use degenbot_rpc::cassette::{
    entry_digest, entry_key, Cassette, CassetteEntry, CassetteProvenance, CassetteResponse,
    CassetteSpan,
};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;

mod common;
use serde_json::{json, Value};

/// The chain the fixtures run on.
const CHAIN_ID: i64 = 1;
/// The market name the seed stamps.
const MARKET_NAME: &str = "Aave Discount Token Generation Market";
/// The `POOL_ADDRESS_PROVIDER` (unchanged by the rotation).
const PROVIDER: &str = "0x2f39d218133afab8f2b819b1066c7e434ad94e9e";
/// The `POOL` (unchanged).
const POOL: &str = "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2";
/// The `POOL_CONFIGURATOR` (unchanged).
const CONFIGURATOR: &str = "0x64b761d848206f447fe2dd461b0c635ec39ebb27";
/// The GHO underlying token (the chain-wide discount row's parent).
const GHO: &str = "0x40d16fc0246ad3160ccc09b8d0d3a2cdd286ae77";
/// The GHO vToken — the `DiscountTokenUpdated` emitter the dispatch's
/// emitter guard scopes to the chain's GHO row.
const VTOKEN: &str = "0x3050e2f0b4db4a08e0cbb8ce4225eb1c3ffbc159";
/// The GHO aToken — the seeded reserve asset's other scaled token.
const ATOKEN: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The committed (old) discount token generation.
const T0: &str = "0x4da27a545dcf524758366dd7361f6931ed2df65b";
/// The replacement (new) discount token generation.
const T1: &str = "0x2222222222222222222222222222222222222222";
/// A second replacement — the intermediate-generation case.
const T2: &str = "0x3333333333333333333333333333333333333333";
/// The touched user (the reseed + refresh backfill target).
const USER: &str = "0x4242424242424242424242424242424242424242";

/// The chunk window `[100, 109]`.
const CHUNK_START: u64 = 100;
const CHUNK_END: u64 = 109;

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
/// `keccak256("DiscountTokenUpdated(address,address)")` — the discount pass's
/// second topic set.
const DISCOUNT_TOKEN_UPDATED_TOPIC: &str =
    "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529";
/// `keccak256("Transfer(address,address,uint256)")`.
const ERC20_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
/// `keccak256("Mint(address,address,uint256,uint256,uint256)")`.
const AAVE_MINT_TOPIC: &str = "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196";
/// `keccak256("Redeem(address,address,uint256)")` — the stkAAVE pass.
const AAVE_REDEEM_TOPIC: &str =
    "0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc";
/// `keccak256("Staked(address,address,uint256)")`.
const AAVE_STAKED_TOPIC: &str =
    "0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc";
/// `balanceOf(address)`'s selector (the backfill/refresh account read).
const BALANCE_OF_SELECTOR: &str = "0x70a08231";

const TX_A: &str = "0xa17567fa201a78b66c43e6ab178559f8c1d5d308fe944c0bd2c39b5e585097dc";
const TX_B: &str = "0xb17567fa201a78b66c43e6ab178559f8c1d5d308fe944c0bd2c39b5e585097dc";

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

/// A warm-boot substrate: the market cursor at 99, the three endpoint rows,
/// and the chain's GHO row linked to its vToken with `v_gho_discount_token`
/// set to `discount_token` (`None` = the cold-boot pre-initialization
/// state).
fn warm_boot_db(discount_token: Option<&str>) -> DegenbotDb {
    let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
    {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, ?1, ?2, 1, 99)",
            rusqlite::params![CHAIN_ID, MARKET_NAME],
        )
        .unwrap();
        for (name, address) in [
            ("POOL_ADDRESS_PROVIDER", PROVIDER),
            ("POOL", POOL),
            ("POOL_CONFIGURATOR", CONFIGURATOR),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn,
                1,
                name,
                &checksum(address),
                Some(1),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO erc20_tokens (id, chain, address) VALUES (1, ?1, ?2), (2, ?1, ?3)",
            rusqlite::params![CHAIN_ID, checksum(GHO), checksum(VTOKEN)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, v_gho_discount_token) \
             VALUES (1, 1, 2, ?1)",
            rusqlite::params![discount_token.map(checksum)],
        )
        .unwrap();
    }
    db
}

/// Seed the GHO reserve asset (aToken + vToken, revision 1 — the discount
/// pre-pass's `v_token_revision` read + the parser's GHO classification).
fn seed_gho_reserve_asset(db: &DegenbotDb) {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO erc20_tokens (id, chain, address) VALUES (3, ?1, ?2)",
        rusqlite::params![CHAIN_ID, checksum(ATOKEN)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, \
         a_token_revision, v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
         borrow_index, borrow_rate) VALUES (1, 1, 1, 3, 1, 2, 1, '0', '0', '0', '0')",
        [],
    )
    .unwrap();
}

fn seed_user(db: &DegenbotDb, gho_discount: i64, stk_aave_balance: Option<&str>) {
    let conn = db.lock();
    conn.execute(
        "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, \
         stk_aave_balance, isolation_mode_debt) VALUES (1, 1, ?1, 0, ?2, ?3, '0')",
        rusqlite::params![checksum(USER), gho_discount, stk_aave_balance],
    )
    .unwrap();
}

/// The synthetic captures' block identity: one hash per height, each naming the
/// previous height as its parent, so the exact-position evidence a projection
/// journals is coherent (a block hash names one block, ADR-105 D1).
fn synthetic_block_hash(number: u64) -> String {
    format!("0x{number:064x}")
}

/// The `DiscountTokenUpdated(old, new)` log — emitted by the GHO vToken.
fn discount_token_updated_log(tx: &str, block: u64, log_index: u64, old: &str, new: &str) -> Value {
    json!({
        "address": VTOKEN.to_lowercase(),
        "blockHash": synthetic_block_hash(block),
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": "0x",
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [DISCOUNT_TOKEN_UPDATED_TOPIC, word(old), word(new)],
        "transactionHash": tx,
        "transactionIndex": "0",
    })
}

/// The stkAAVE `Transfer` mint leg (`0x0 -> USER`) the introduced
/// generation's supplemental pass fetches.
fn stk_aave_mint_transfer_log(
    tx: &str,
    block: u64,
    log_index: u64,
    token: &str,
    amount: u64,
) -> Value {
    json!({
        "address": token.to_lowercase(),
        "blockHash": synthetic_block_hash(block),
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": format!("0x{}", data_word(amount)),
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [
            ERC20_TRANSFER_TOPIC,
            word("0x0000000000000000000000000000000000000000"),
            word(USER),
        ],
        "transactionHash": tx,
        "transactionIndex": "0",
    })
}

/// The GHO vToken `Mint` (the pure interest-accrual shape: `value ==
/// balance_increase` → a zero scaled delta, a live `should_refresh_discount`
/// signal).
fn gho_vtoken_mint_log(tx: &str, block: u64, log_index: u64) -> Value {
    json!({
        "address": VTOKEN.to_lowercase(),
        "blockHash": synthetic_block_hash(block),
        "blockNumber": block.to_string(),
        "blockTimestamp": "0",
        "data": format!("0x{}{}{}", data_word(1_000), data_word(1_000), data_word(1_000_000_000)),
        "logIndex": log_index.to_string(),
        "removed": false,
        "topics": [
            AAVE_MINT_TOPIC,
            word("0x1111111111111111111111111111111111111111"),
            word(USER),
        ],
        "transactionHash": tx,
        "transactionIndex": "0",
    })
}

fn chunk_filter(address: &Value, topics: &[Value], from_block: u64, to_block: u64) -> Value {
    let mut filter = serde_json::Map::new();
    if !address.is_null() {
        filter.insert("address".to_string(), address.clone());
    }
    filter.insert("fromBlock".to_string(), json!(from_block.to_string()));
    filter.insert("toBlock".to_string(), json!(to_block.to_string()));
    filter.insert("topics".to_string(), json!(topics));
    Value::Object(filter)
}

/// The replay ledger builder: one entry per distinct wire request.
struct Ledger {
    entries: std::collections::BTreeMap<String, CassetteEntry>,
    from_block: u64,
    to_block: u64,
}

impl Ledger {
    fn new(from_block: u64, to_block: u64) -> Self {
        Self {
            entries: std::collections::BTreeMap::new(),
            from_block,
            to_block,
        }
    }

    fn add_get_logs(&mut self, addresses: &[&str], topics: &[Vec<String>], result: Value) {
        // The wire shape follows alloy's `Filter`: a single address is a
        // bare string, a multi-address OR-set is an array; the same
        // single-option rule holds per topic position.
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
        let params = json!([chunk_filter(
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

    /// The backfill/refresh `balanceOf(user)` `eth_call` on `token` at
    /// `block - 1` (the pre-block balance contract).
    fn add_balance_of(&mut self, token: &str, user: &str, block: u64, balance: u64) {
        let input = format!(
            "{BALANCE_OF_SELECTOR}000000000000000000000000{}",
            &user[2..]
        );
        let params = json!([
            {"input": input, "to": token.to_lowercase()},
            (block - 1).to_string(),
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

    /// The scaled-token pass's six topic sets.
    fn scaled_topics() -> Vec<Vec<String>> {
        vec![vec![
            "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956".to_string(),
            "0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b".to_string(),
            "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666".to_string(),
            "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90".to_string(),
            AAVE_MINT_TOPIC.to_string(),
            ERC20_TRANSFER_TOPIC.to_string(),
        ]]
    }

    /// The stkAAVE pass's three topic sets (`Redeem`/`Staked`/`Transfer`).
    fn stk_aave_topics() -> Vec<Vec<String>> {
        vec![vec![
            AAVE_REDEEM_TOPIC.to_string(),
            AAVE_STAKED_TOPIC.to_string(),
            ERC20_TRANSFER_TOPIC.to_string(),
        ]]
    }

    /// The main pass set every chunk issues: the five unconditioned passes,
    /// the scaled-token pass when `scaled` carries a result (an empty token
    /// set short-circuits without a request), the chain-wide discount pass
    /// with its `discount` result (the pass that fetches
    /// `DiscountTokenUpdated` events), and the committed generation's stkAAVE
    /// pass when `stk_aave` carries a result (`None` on the cold-boot
    /// pre-initialization state).
    fn add_main_passes(&mut self, scaled: Option<Value>, discount: Value, stk_aave: Option<Value>) {
        self.add_get_logs(
            &[PROVIDER],
            &[ADDRESS_PROVIDER_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(
            &[CONFIGURATOR],
            &[CONFIGURATOR_PASS_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(
            &[POOL],
            &[POOL_PASS_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            json!([]),
        );
        self.add_get_logs(&[], &[vec![ORACLE_PASS_TOPIC.to_string()]], json!([]));
        self.add_get_logs(
            &[],
            &[DISCOUNT_PASS_TOPICS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()],
            discount,
        );
        if let Some(result) = scaled {
            self.add_get_logs(&[ATOKEN, VTOKEN], &Self::scaled_topics(), result);
        }
        if let Some(result) = stk_aave {
            self.add_get_logs(&[T0], &Self::stk_aave_topics(), result);
        }
    }

    fn stk_aave_pass(&mut self, token: &str, result: Value) {
        self.add_get_logs(&[token], &Self::stk_aave_topics(), result);
    }

    /// The run's terminal evidence: the endpoint's chain id and the header of
    /// the block the span closes on. A run resolves both before it fetches
    /// (ADR-105 D1), so a recorded capture carries them; the synthetic fixtures
    /// serve one block hash for every height, so the header names that hash.
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
        let header = CassetteResponse::Success {
            result: json!({
                "number": self.to_block.to_string(),
                "hash": synthetic_block_hash(self.to_block),
                "parentHash": synthetic_block_hash(self.to_block.saturating_sub(1)),
            }),
        };
        let params = json!([format!("0x{:x}", self.to_block), false]);
        self.entries.insert(
            entry_key("eth_getBlockByNumber", &params),
            CassetteEntry {
                digest: entry_digest(&header),
                response: header,
            },
        );
    }

    fn into_transport(mut self) -> CassetteReplayTransport {
        self.add_terminal_anchor();
        let evidence = common::candidate_evidence_entries(&self.entries, |block| {
            json!({
                "number": block.to_string(),
                "hash": synthetic_block_hash(block),
                "parentHash": synthetic_block_hash(block.saturating_sub(1)),
            })
        });
        self.entries.extend(evidence);
        CassetteReplayTransport::new(Cassette {
            schema: degenbot_rpc::cassette::CASSETTE_SCHEMA_V1.to_string(),
            chain_id: 1,
            provenance: CassetteProvenance {
                source: "test".to_string(),
                recorded_at: "1970-01-01T00:00:00Z".to_string(),
                span: CassetteSpan {
                    from_block: self.from_block,
                    to_block: self.to_block,
                },
            },
            entries: self.entries,
        })
    }
}

fn run_update(db: &DegenbotDb, transport: &CassetteReplayTransport) -> AaveUpdateReport {
    run_aave_update_on_db(
        db,
        RunSpec {
            chain_id: CHAIN_ID,
            market_id: 1,
            to_block: Some(CHUNK_END),
            chunk_size: 10,
            max_block_range: 10_000,
            provider: transport.as_alloy_provider(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(NoProgress),
            verify: VerifyPolicy::default(),
            max_chunks: None,
        },
    )
    .unwrap_or_else(|e| panic!("the run must commit: {e}"))
}

fn committed_discount_token(db: &DegenbotDb) -> Option<String> {
    let conn = db.lock();
    conn.query_row(
        "SELECT v_gho_discount_token FROM aave_gho_tokens WHERE id = 1",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .unwrap()
}

fn user_stk_aave_balance(db: &DegenbotDb) -> Option<String> {
    let conn = db.lock();
    conn.query_row(
        "SELECT stk_aave_balance FROM aave_v3_users WHERE id = 1",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .unwrap()
}

/// An actual `Some(T0) -> Some(T1)` replacement must fetch BOTH
/// generations' stkAAVE streams over the replacement chunk: the committed
/// T0 main pass plus the uncovered T1 supplemental pass. A stale-generation
/// run never requests the T1 pass — the unserved ledger entry + the
/// request-count assertion are the loud red.
#[test]
fn discount_token_replacement_fetches_old_and_new_token_streams() {
    let db = warm_boot_db(Some(T0));
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        None,
        json!([discount_token_updated_log(TX_A, 100, 0, T0, T1)]),
        Some(json!([])),
    );
    ledger.stk_aave_pass(T0, json!([]));
    ledger.stk_aave_pass(T1, json!([]));
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    assert_eq!(report.to_block, CHUNK_END);
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 12,
        "five main passes + the committed T0 pass + the ONE uncovered \
         generation (T1) + the terminal anchor's chain-id and header reads \
         + the candidate acquisition (chain binding, block header, complete \
         receipt) — a committed-token insert would re-request T0"
    );
    assert_eq!(
        snapshot.per_method.get("eth_getLogs").map(|m| m.served),
        Some(7)
    );
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T1).as_str()),
        "the replacement committed"
    );
}

/// Multiple `DiscountTokenUpdated` events in ONE chunk (T0 -> T1 -> T2)
/// must reach EVERY generation's stream: the committed T0 through the main
/// pass + both introduced tokens through uncovered supplemental passes, the
/// intermediate T1 included — each generation's ledger is reachable only
/// through its own stream.
#[test]
fn multiple_discount_token_updates_fetch_every_generation() {
    let db = warm_boot_db(Some(T0));
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        None,
        json!([
            discount_token_updated_log(TX_A, 100, 0, T0, T1),
            discount_token_updated_log(TX_B, 101, 0, T1, T2),
        ]),
        Some(json!([])),
    );
    ledger.stk_aave_pass(T0, json!([]));
    ledger.stk_aave_pass(T1, json!([]));
    ledger.stk_aave_pass(T2, json!([]));
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 15,
        "five main passes + the committed T0 pass + the TWO uncovered \
         generations (T1, T2) + the terminal anchor's chain-id and header reads \
         + the candidate acquisition (chain binding, block headers, complete \
         receipts)"
    );
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T2).as_str()),
        "the final generation committed — the cursor never advanced over a \
         mixed-generation state"
    );
}

/// The initial `None -> stkAAVE` set keeps its current behavior: ONE
/// uncovered supplemental pass for the introduced token and NO balance
/// invalidation — the pre-existing non-NULL balance stays the backfill-free
/// apply baseline.
#[test]
fn initial_set_none_to_some_matches_current_behavior() {
    let db = warm_boot_db(None);
    seed_user(&db, 0, Some("777"));
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        None,
        json!([discount_token_updated_log(
            TX_A,
            100,
            0,
            "0x0000000000000000000000000000000000000000",
            T1
        )]),
        None,
    );
    ledger.stk_aave_pass(T1, json!([stk_aave_mint_transfer_log(TX_A, 100, 1, T1, 5)]));
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T1).as_str())
    );
    assert_eq!(
        user_stk_aave_balance(&db).as_deref(),
        Some("782"),
        "the initial-set Transfer applied against the seeded balance — no \
         invalidation on a None -> some set"
    );
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 11,
        "five main passes + the introduced T1 pass + the terminal anchor's \
         chain-id and header reads + the candidate acquisition (chain binding, \
         block header, complete receipt) — the current wire shape"
    );
}

/// An actual replacement must reseed the affected cached balances from the
/// NEW ledger: the chunk's own transaction invalidates the T0-denominated
/// cache, the introduced T1 `Transfer` triggers the NULL backfill through
/// the NEW token's `balanceOf` (the recorded `eth_call` answers only a
/// T1-targeted call — a stale-generation backfill misses the ledger
/// loudly), and the final cached value is the T1 answer plus the applied
/// delta.
#[test]
fn discount_token_replacement_reseeds_balances_from_the_new_ledger() {
    let db = warm_boot_db(Some(T0));
    seed_user(&db, 0, Some("500"));
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        None,
        json!([discount_token_updated_log(TX_A, 100, 0, T0, T1)]),
        Some(json!([])),
    );
    ledger.stk_aave_pass(T0, json!([]));
    ledger.stk_aave_pass(
        T1,
        json!([stk_aave_mint_transfer_log(TX_B, 101, 0, T1, 100)]),
    );
    ledger.add_balance_of(T1, USER, 101, 1_000);
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T1).as_str())
    );
    assert_eq!(
        user_stk_aave_balance(&db).as_deref(),
        Some("1100"),
        "the T0 cache (500) was invalidated + reseeded from the NEW ledger \
         (balanceOf(T1) = 1000) before the T1 Transfer applied (+100)"
    );
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 15,
        "five main passes + committed T0 + the uncovered T1 pass + the T1 \
         balanceOf backfill call + the terminal anchor's chain-id and header \
         reads + the candidate acquisition (chain binding, block headers, \
         complete receipts)"
    );
}

/// A same-tx rotation + GHO debt action: the post-apply discount refresh
/// must read the POST-dispatch token — the recorded `balanceOf` answers
/// only a NEW-contract (T1) call. A tx-start snapshot refresh targets T0
/// and misses the ledger loudly.
#[test]
fn same_tx_rotation_refresh_uses_new_contract() {
    let db = warm_boot_db(Some(T0));
    seed_gho_reserve_asset(&db);
    seed_user(&db, 10, None);
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        Some(json!([gho_vtoken_mint_log(TX_A, 100, 1)])),
        json!([discount_token_updated_log(TX_A, 100, 0, T0, T1)]),
        Some(json!([])),
    );
    ledger.stk_aave_pass(T0, json!([]));
    ledger.stk_aave_pass(T1, json!([]));
    // The refresh's block is the mint tx's block (100) — the call reads
    // block 99.
    ledger.add_balance_of(T1, USER, 100, 750);
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T1).as_str())
    );
    assert_eq!(
        user_stk_aave_balance(&db).as_deref(),
        Some("750"),
        "the refresh's NULL backfill read the NEW contract's ledger — the \
         recorded eth_call answers only a T1-targeted balanceOf"
    );
    let snapshot = transport.served_snapshot();
    assert_eq!(snapshot.requests, 14);
    assert_eq!(
        snapshot.per_method.get("eth_call").map(|m| m.served),
        Some(1),
        "the refresh backfill hit the NEW token's balanceOf"
    );
}

/// A no-op `DiscountTokenUpdated(T0, T0)` is not a generation change: the
/// committed token is already the main pass's coverage, so the supplemental
/// plan must stay EMPTY (no redundant request), the cached balance is
/// preserved (the writer invalidates only on an actual address change), and
/// the run's request shape is byte-identical to a no-event chunk.
#[test]
fn unchanged_discount_token_preserves_cached_balances_and_wire_shape() {
    let db = warm_boot_db(Some(T0));
    seed_user(&db, 0, Some("321"));
    let mut ledger = Ledger::new(CHUNK_START, CHUNK_END);
    ledger.add_main_passes(
        None,
        json!([discount_token_updated_log(TX_A, 100, 0, T0, T0)]),
        Some(json!([])),
    );
    let transport = ledger.into_transport();

    let report = run_update(&db, &transport);

    assert_eq!(report.chunks_committed, 1);
    assert_eq!(
        committed_discount_token(&db).as_deref(),
        Some(checksum(T0).as_str()),
        "the no-op rotation left the committed generation unchanged"
    );
    assert_eq!(
        user_stk_aave_balance(&db).as_deref(),
        Some("321"),
        "the cached balance is preserved — no invalidation without an          actual generation change"
    );
    let snapshot = transport.served_snapshot();
    assert_eq!(
        snapshot.requests, 11,
        "five main passes + the committed T0 pass + the terminal anchor's chain-id \
         and header reads + the candidate acquisition (chain binding, block \
         header, complete receipt) — the supplemental plan is empty (every \
         generation already covered)"
    );
}
