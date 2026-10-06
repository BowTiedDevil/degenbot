//! Generate the wave-3 aave-side adversarial golden captures for the aave
//! updater: node-free, deterministic, through the recorder's own artifact
//! writers; the replay suite (`wave3_capture_replay`) consumes the committed
//! cassette bytes and SQL goldens.
//!
//! Three scenarios on the ScratchEvm/frame-replay seam
//! ([`ScratchDriver`](degenbot_simulation::capture::ScratchDriver)) with the
//! scripted actor contracts (`degenbot_simulation::capture::actor` — the aave
//! sibling of the wave-2 `V3CaptureHarness` precedent). Each drives the REAL
//! `run_aave_update` chunk loop over a recording transport — the flushed
//! cassette is the recorder's canonical writer's bytes, exactly what a live
//! recording produces — then replays the committed cassette bytes through the
//! statement-ledger wrapper for the SQL goldens (the same writers
//! `aave_config_cassette_replay.rs` gates with).
//!
//! # The scenarios
//!
//! 1. `w3_aave_cross_tx_fact_dependence` — the ChunkSubstrate overlay case
//!    (Perf C's write-overlay contract, adversarial): tx N's ops parser
//!    creates the user + GHO debt position (the paired Pool `Repay` + vToken
//!    `Burn` apply), and tx N+1's discount/config path READS that row
//!    in-chunk. The recorded RPC shape is the QR7QVT rt-gate shape: 6 getLogs
//!    passes + exactly ONE `getDiscountPercent` `eth_call` (the first-seen
//!    user's path-#2 RPC, pinned at tx N's block). The replay must serve 7 —
//!    a fact phase that consulted the COMMITTED DB instead of the chunk's own
//!    applies would re-issue the call at tx N+1's block (9 on a live node; a
//!    loud fixture gap in replay — demonstrated by the replay suite's probe).
//! 2. `w3_aave_upgraded_boundary_revision_memo` — the mid-chunk `Upgraded`
//!    boundary (BEKQVL's RevisionMemo contract): two same-implementation
//!    `DEBT_TOKEN_REVISION()` probes inside one chunk with the second
//!    proxy's `Upgraded` between them. The memo's block lane must make the
//!    post-upgrade probe a REAL recorded RPC (two distinct cassette entries,
//!    one per block tag) — mutating the recorded post-upgrade answer must go
//!    loud in replay, never silently serve the pre-upgrade memo value.
//! 3. `w3_aave_discount_revival_fixture` — the named discount-revival
//!    counterfactual: the GHO vToken's revision is 1 (< the deprecation
//!    revision 4), so the discount pre-pass's path-#2 RPC shape is recorded
//!    end-to-end (a first-seen borrower's `getDiscountPercent`, then the
//!    in-chunk-created row's DB-cache reuse) and QR7QVT's option-(a) fact-run
//!    shape stays executable if the discount path ever revives on the live
//!    chain. This is a FIXTURE, not live behavior — the provenance source
//!    string says so.
//!
//! # Determinism
//!
//! The whole generation runs TWICE and both passes must be byte-identical
//! (asserted here; the corpus `--check` + the replay suite re-prove it from
//! disk). Block hashes are `keccak256(block_number)`, tx hashes
//! `keccak256(block || tx_index)`; the actor frames' logs ride the same
//! deterministic coordinate scheme as wave 2.
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \
//!         --features degenbot/sql-ledger \
//!         --example generate_aave_adversarial_captures -- \
//!         --check tests/fixtures/cassettes/wave3 tests/fixtures/sql_goldens/wave3
//!
//! `just check-aave-captures` is this exact invocation as a recipe. The
//! `sql-ledger` passthrough is REQUIRED (the `[[example]]` entry's
//! required-features — without the feature the `degenbot_db::sql_ledger`
//! import fails E0432). Default mode (re)writes the two fixture homes;
//! `--check` regenerates and exits 1 on any byte drift against the committed
//! artifacts.

// Run-once diagnostic example: stdout/stderr reports ARE its interface (the
// generate_evm_oracle_captures precedent), and the two-pass determinism gate
// asserts by panic — the same scoping the committed examples carry.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::transports::Transport as _;
use degenbot::aave::updater::{
    run_aave_update, run_aave_update_on_db, AaveUpdateReport, NoProgress,
};
use degenbot::db::DegenbotDb;
use degenbot::rpc::cassette::{
    rfc3339_utc, Cassette, CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use degenbot_simulation::capture::{
    scripted_actor_creation_code, scripted_coordinator_creation_code, ActorBranch, ScratchDriver,
    ScriptedLog,
};
use tempfile::TempDir;

/// The chain every scenario serves (mainnet-shaped; the wave-2 convention).
const CHAIN_ID: u64 = 1;
/// Deterministic block-timestamp base: block `b` serves `TS_BASE + b`.
const TS_BASE: u64 = 1_700_000_000;
/// The actor-deploy block (the frames' coordinates are scenario constants
/// from here on; the chunk spans start at this block).
const DEPLOY_BLOCK: u64 = 1000;
/// Actor-deploy/call frame gas (the tier-3 harnesses' documented budget).
const FRAME_GAS: u64 = 16_700_000;
/// The chunk span's end for every scenario (the frames land at 1001/1002).
const SPAN_TO: u64 = 1002;

// ── event topics (the decoder crate's constants are the source of truth;
//    the actors' emitted topics must match the fetch filters + decoders
//    exactly — an off-by-one topic would decode-skip, and this generator's
//    recorded-entry assertions make that loud) ─────────────────────────────

fn aave_repay_topic() -> B256 {
    alloy::primitives::b256!("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
}

fn aave_burn_topic() -> B256 {
    alloy::primitives::b256!("0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90")
}

fn aave_mint_topic() -> B256 {
    alloy::primitives::b256!("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")
}

fn aave_borrow_topic() -> B256 {
    alloy::primitives::b256!("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0")
}

fn aave_upgraded_topic() -> B256 {
    alloy::primitives::b256!("0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b")
}

fn erc20_transfer_topic() -> B256 {
    alloy::primitives::b256!("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
}

/// The GHO-discount deprecation revision (the config dispatch's constant):
/// the revival fixture's vToken revision (1) must sit BELOW it for the
/// discount path to be live on the fixture chain. A static assertion here
/// keeps the fixture's premise load-bearing if the constant moves.
const GHO_DISCOUNT_DEPRECATION_REVISION: u32 = 4;

/// The seeded aToken/vToken revision (the recorder's `AAVE_SEED_TOKEN_REVISION`
/// convention): era-accurate for the discount-era fixture chain.
const SEED_TOKEN_REVISION: i64 = 1;

/// The seeded POOL contract revision (chain-verified for the QR7QVT corpus
/// era; drives the parser's scaled-amount tolerance gate).
const SEED_POOL_REVISION: i64 = 11;
/// The seeded POOL_CONFIGURATOR revision (the recorder's seed shape).
const SEED_CONFIGURATOR_REVISION: i64 = 8;

/// RAY (1e27) — the index every first event carries, so the scaled-amount
/// math reduces to the raw amounts (deterministic literals).
fn ray() -> U256 {
    U256::from(1_000_000_000_000_000_000_000_000_000u128)
}

/// 1.05e27 — the revival fixture's second index (the 5% index movement that
/// makes the accrual event's balance-increase non-zero).
fn idx2() -> U256 {
    U256::from(1_050_000_000_000_000_000_000_000_000u128)
}

fn eth18(whole: u64) -> U256 {
    U256::from(whole) * U256::from(10).pow(U256::from(18))
}

fn word(value: U256) -> [u8; 32] {
    value.to_be_bytes::<32>()
}

fn addr_word(address: &Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(address.as_slice());
    w
}

// ── pinned chain literals (independent constants: the free addresses are
//    scenario-fixed; the actor addresses are CREATE-derived from the frame
//    sequence and pinned — a frame-order change fails the pin loudly) ──────

/// The GHO underlying token (a free literal: no actor needed — it only names
/// a topic word + the erc20/gho rows).
const W3_GHO: &str = "0x4040404040404040404040404040404040404040";
/// The GHO aToken (free literal; no logs from it in these scenarios).
const W3_ATOKEN: &str = "0x4141414141414141414141414141414141414141";
/// The address-provider/configurator/oracle contract rows (free literals;
/// their getLogs passes serve empty answers — no actors needed).
const W3_ADDRESS_PROVIDER: &str = "0x7070707070707070707070707070707070707070";
const W3_CONFIGURATOR: &str = "0x7272727272727272727272727272727272727272";
const W3_PRICE_ORACLE: &str = "0x7474747474747474747474747474747474747474";
/// The scenario-2 pool row (its getLogs pass serves an empty answer — no
/// pool events in that scenario, so no actor either).
const W3_S2_POOL: &str = "0x5050505050505050505050505050505050505050";
/// Scenario-2 asset literals (underlying/aToken pairs; the vTokens are the
/// deployed actors).
const W3_S2_UNDERLYING_1: &str = "0x5151515151515151515151515151515151515151";
const W3_S2_ATOKEN_1: &str = "0x5252525252525252525252525252525252525252";
const W3_S2_UNDERLYING_2: &str = "0x5353535353535353535353535353535353535353";
const W3_S2_ATOKEN_2: &str = "0x5454545454545454545454545454545454545454";
/// The repeat-burn user (scenario 1) and the revival fixture's borrower
/// (scenario 3) — free address literals.
const W3_S1_USER: &str = "0xa1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const W3_S3_USER: &str = "0xb2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

/// The CREATE-derived actor addresses (pinned literals — the replay suite
/// seeds the same addresses; a frame-sequence change fails these pins loudly
/// instead of silently re-seeding a different chain).
const ACTOR_PINS: &[(&str, &str)] = &[
    (
        "s1.pool_actor",
        "0xBd770416a3345F91E4B34576cb804a576fa48EB1",
    ),
    (
        "s1.vtoken_actor",
        "0x5a443704dd4B594B382c22a083e2BD3090A6feF3",
    ),
    (
        "s1.coordinator",
        "0x47e9Fbef8C83A1714F1951F142132E6e90F5fa5D",
    ),
    (
        "s2.impl_actor",
        "0xBd770416a3345F91E4B34576cb804a576fa48EB1",
    ),
    (
        "s2.proxy_1_actor",
        "0x5a443704dd4B594B382c22a083e2BD3090A6feF3",
    ),
    (
        "s2.proxy_2_actor",
        "0x47e9Fbef8C83A1714F1951F142132E6e90F5fa5D",
    ),
    (
        "s3.pool_actor",
        "0xBd770416a3345F91E4B34576cb804a576fa48EB1",
    ),
    (
        "s3.vtoken_actor",
        "0x5a443704dd4B594B382c22a083e2BD3090A6feF3",
    ),
    (
        "s3.coordinator",
        "0x47e9Fbef8C83A1714F1951F142132E6e90F5fa5D",
    ),
];

/// The pinned address for one actor label (zero = intentionally unpinned).
fn actor_pin(label: &str) -> Address {
    let raw = ACTOR_PINS
        .iter()
        .find(|(name, _)| *name == label)
        .unwrap_or_else(|| panic!("unknown actor pin label {label}"))
        .1;
    raw.parse().expect("pinned actor literal parses")
}

/// Cross-check one resolved deploy address against its pin (loud on drift;
/// an unpinned zero literal fails with the resolved value to copy in).
fn assert_pin(label: &str, resolved: Address) {
    let pinned = actor_pin(label);
    if pinned.is_zero() {
        panic!(
            "UNPINNED actor address {label}: resolved {resolved} — pin it in ACTOR_PINS \
             (and mirror it in the replay suite's seed literals)"
        );
    }
    assert_eq!(
        pinned, resolved,
        "pinned {label} address drifted: pinned {pinned}, resolved {resolved} — the frame \
         sequence moved; re-pin both the generator and the replay suite"
    );
}

/// The getDiscountPercent answer the fixture chain serves (basis points —
/// 2000 = 20.00%): the recorded path-#2 RPC's answer for both the cross-tx
/// capture and the revival fixture.
fn discount_answer_word() -> [u8; 32] {
    word(U256::from(2_000u64))
}

/// The `DEBT_TOKEN_REVISION()` answer the scenario-2 implementation actor
/// serves. Both same-impl probes read this value — the recorded answers are
/// chain-truthful (one implementation, one revision constant); the two
/// cassette entries differ by BLOCK TAG, which is exactly the memo
/// contract's block lane. The static assertion pins the fixture premise:
/// the recorded revision sits below the deprecation revision.
const _: () = assert!(
    (GHO_DISCOUNT_DEPRECATION_REVISION as u64) > 1,
    "the revival fixture's premise (DEBT_TOKEN_REVISION 1 < deprecation revision 4) moved"
);

fn impl_revision_word() -> [u8; 32] {
    word(U256::from(2u64))
}

// ── the scripted actors ──────────────────────────────────────────────────

/// Scenario 1: the pool actor emits the paired `Repay` (GHO reserve) and the
/// vToken actor the paired `Burn` — the same logs every call, so the two tx
/// frames differ only by block/tx-hash (the overlay case needs exactly that:
/// the SAME user acting across txs). The vToken actor's fallback word is the
/// `getDiscountPercent` answer (the discount pre-pass's RPC surface targets
/// the GHO vToken).
fn s1_actors(user: Address, gho: Address) -> (Vec<u8>, Vec<u8>) {
    let repay_amount = eth18(105);
    let pool_actor = scripted_actor_creation_code(
        &[ActorBranch::new(
            1,
            vec![ScriptedLog::new(
                vec![
                    aave_repay_topic(),
                    B256::from(addr_word(&gho)),
                    B256::from(addr_word(&user)),
                    B256::from(addr_word(&user)),
                ],
                {
                    let mut d = Vec::with_capacity(64);
                    d.extend_from_slice(&word(repay_amount));
                    d.extend_from_slice(&word(U256::ZERO)); // useATokens = false
                    d
                },
            )],
            word(U256::ONE),
        )],
        word(U256::ZERO),
    )
    .expect("scenario-1 pool actor assembles");
    let v_token_actor = scripted_actor_creation_code(
        &[ActorBranch::new(
            1,
            vec![ScriptedLog::new(
                vec![
                    aave_burn_topic(),
                    B256::from(addr_word(&user)), // from
                    B256::from(addr_word(&user)), // target
                ],
                {
                    let mut d = Vec::with_capacity(96);
                    d.extend_from_slice(&word(repay_amount)); // value (pairing: value + 0 == repay)
                    d.extend_from_slice(&word(U256::ZERO)); // balanceIncrease
                    d.extend_from_slice(&word(ray())); // index
                    d
                },
            )],
            discount_answer_word(),
        )],
        discount_answer_word(),
    )
    .expect("scenario-1 vToken actor assembles");
    (pool_actor, v_token_actor)
}

/// Scenario 2: two proxy actors (each emits `Upgraded(impl)` on its branch)
/// + the implementation actor (a pure fallback responder: the revision word).
fn s2_actors(impl_address: Address) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let upgraded_log = || {
        ScriptedLog::new(
            vec![aave_upgraded_topic(), B256::from(addr_word(&impl_address))],
            Vec::new(),
        )
    };
    let proxy_1 = scripted_actor_creation_code(
        &[ActorBranch::new(1, vec![upgraded_log()], word(U256::ONE))],
        word(U256::ZERO),
    )
    .expect("scenario-2 proxy-1 actor assembles");
    let proxy_2 = scripted_actor_creation_code(
        &[ActorBranch::new(1, vec![upgraded_log()], word(U256::ONE))],
        word(U256::ZERO),
    )
    .expect("scenario-2 proxy-2 actor assembles");
    // The implementation: no branches — EVERY call (the memo's
    // `DEBT_TOKEN_REVISION()` reads at both blocks) falls through to the
    // revision word.
    let implementation = scripted_actor_creation_code(&[] as &[ActorBranch], impl_revision_word())
        .expect("scenario-2 implementation actor assembles");
    (proxy_1, proxy_2, implementation)
}

/// Scenario 3: the pool actor emits the paired `Borrow`; the vToken actor's
/// branch 0x01 emits the paired `Mint` + the ERC20 `Transfer(0→user)` the
/// borrow matcher asserts exactly one of; branch 0x02 emits the
/// pure-interest accrual Mint (value == balanceIncrease, moved index) the
/// fixture's second tx drives; the fallback answers `getDiscountPercent`.
fn s3_actors(user: Address, gho: Address) -> (Vec<u8>, Vec<u8>) {
    let borrow_amount = eth18(100);
    let pool_actor = scripted_actor_creation_code(
        &[ActorBranch::new(
            1,
            vec![ScriptedLog::new(
                vec![
                    aave_borrow_topic(),
                    B256::from(addr_word(&gho)),
                    B256::from(addr_word(&user)),
                    B256::from(word(U256::ZERO)), // referralCode 0
                ],
                {
                    let mut d = Vec::with_capacity(128);
                    d.extend_from_slice(&addr_word(&user)); // user
                    d.extend_from_slice(&word(borrow_amount)); // amount
                    d.extend_from_slice(&word(U256::from(2u64))); // variable rate mode
                    d.extend_from_slice(&word(U256::ZERO)); // borrowRate
                    d
                },
            )],
            word(U256::ONE),
        )],
        word(U256::ZERO),
    )
    .expect("scenario-3 pool actor assembles");
    let mint_log = |value: U256, balance_increase: U256, index: U256| {
        ScriptedLog::new(
            vec![
                aave_mint_topic(),
                B256::from(addr_word(&user)), // caller
                B256::from(addr_word(&user)), // onBehalfOf
            ],
            {
                let mut d = Vec::with_capacity(96);
                d.extend_from_slice(&word(value));
                d.extend_from_slice(&word(balance_increase));
                d.extend_from_slice(&word(index));
                d
            },
        )
    };
    let transfer_log = ScriptedLog::new(
        vec![
            erc20_transfer_topic(),
            B256::from(addr_word(&Address::ZERO)),
            B256::from(addr_word(&user)),
        ],
        word(borrow_amount).to_vec(),
    );
    let v_token_actor = scripted_actor_creation_code(
        &[
            ActorBranch::new(
                1,
                vec![mint_log(borrow_amount, U256::ZERO, ray()), transfer_log],
                discount_answer_word(),
            ),
            ActorBranch::new(
                2,
                vec![mint_log(eth18(5), eth18(5), idx2())],
                discount_answer_word(),
            ),
        ],
        discount_answer_word(),
    )
    .expect("scenario-3 vToken actor assembles");
    (pool_actor, v_token_actor)
}

// ── the seed (the replay-suite substrate shape, per scenario) ────────────

/// One reserve asset's seed row set: `(underlying, a_token, v_token)` plus
/// the GHO link flag (the FK that makes the discount pre-pass resolve the
/// GHO vToken).
struct ReserveSeed {
    underlying: String,
    a_token: String,
    v_token: String,
    gho_link: bool,
}

/// Seed the harness DB the recorder's `--kind aave-run` flow builds (the
/// `aave_config_cassette_replay` seeding shape): the market row at the
/// bootstrap block, the warm-boot contract rows with their revisions, the
/// span cursor, and the reserve assets via the `ReserveInitialized`
/// substrate. Returns `(db path, market id)`.
fn seeded_db(
    dir: &Path,
    file_name: &str,
    pool: &str,
    reserves: &[ReserveSeed],
    span_from: u64,
) -> (PathBuf, i64) {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).expect("seed db opens");
    let market_id = {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO aave_v3_markets (chain_id, name, active, last_update_block) \
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![1i64, "Aave Ethereum Market", 16_291_070i64],
        )
        .expect("market row inserts");
        let market_id = conn.last_insert_rowid();

        // The warm-boot contract rows (bootstrap_pool_contracts no-ops on
        // both POOL + POOL_CONFIGURATOR being present — no RPC).
        for (name, address, revision) in [
            ("POOL_ADDRESS_PROVIDER", W3_ADDRESS_PROVIDER, None),
            ("POOL", pool, Some(SEED_POOL_REVISION)),
            (
                "POOL_CONFIGURATOR",
                W3_CONFIGURATOR,
                Some(SEED_CONFIGURATOR_REVISION),
            ),
            ("PRICE_ORACLE", W3_PRICE_ORACLE, None),
        ] {
            DegenbotDb::apply_contract_inserted_if_absent_on_conn(
                &conn, market_id, name, address, revision,
            )
            .expect("contract row inserts");
        }

        // The span cursor.
        DegenbotDb::set_market_last_update_block_on_conn(
            &conn,
            market_id,
            i64::try_from(span_from - 1).expect("span cursor fits i64"),
        )
        .expect("cursor stamps");

        // The reserve assets (the GHO reserve carries the gho_link FK).
        for reserve in reserves {
            let (underlying_name, underlying_symbol, underlying_decimals) = if reserve.gho_link {
                (Some("Gho Token"), Some("GHO"), Some(18))
            } else {
                (None, None, None)
            };
            let underlying_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                &reserve.underlying,
                underlying_name,
                underlying_symbol,
                underlying_decimals,
            )
            .expect("underlying row inserts");
            let a_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                &reserve.a_token,
                None,
                None,
                None,
            )
            .expect("aToken row inserts");
            let v_token_id = DegenbotDb::get_or_create_erc20_token_on_conn(
                &conn,
                1,
                &reserve.v_token,
                None,
                None,
                None,
            )
            .expect("vToken row inserts");
            let gho_link = if reserve.gho_link {
                Some(
                    DegenbotDb::get_or_create_gho_token_on_conn(&conn, 1, &reserve.underlying)
                        .expect("gho row inserts"),
                )
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
            .expect("asset row inserts");
        }
        market_id
    };
    (path, market_id)
}

/// The tables one Aave chunk apply touches, in the dump's fixed
/// (alphabetical) order (the `aave_config_cassette_replay` list).
const DUMP_TABLES: &[&str] = &[
    "aave_gho_tokens",
    "aave_v3_asset_configs",
    "aave_v3_assets",
    "aave_v3_collateral_positions",
    "aave_v3_contracts",
    "aave_v3_debt_positions",
    "aave_v3_emode_categories",
    "aave_v3_markets",
    "aave_v3_user_collateral_configs",
    "aave_v3_users",
    "erc20_tokens",
];

// ── the recording run + the SQL golden pass (the wave-2 shape) ───────────

/// One scenario's generation products.
struct ScenarioArtifacts {
    name: &'static str,
    cassette: Vec<u8>,
    ledger: String,
    dump: String,
}

/// Run the REAL chunk loop over the recording transport, one chunk covering
/// the whole span (the QR7QVT recording shape: the run resolves its window
/// from the seeded cursor, so no ancillary block-tag re-issue).
fn run_recorded_chunk(
    db_path: &Path,
    market_id: i64,
    provider: degenbot::rpc::provider::AlloyProvider,
) -> AaveUpdateReport {
    run_aave_update(
        db_path,
        1,
        market_id,
        Some(SPAN_TO),
        SPAN_TO - DEPLOY_BLOCK + 1,
        provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .expect("the recording run of the scenario chunk must commit cleanly")
}

/// The SQL golden pass: replay the committed cassette BYTES through the
/// ledger (the same per-chunk shape the recording run used).
fn golden_pass(
    cassette_bytes: &[u8],
    seed: impl FnOnce(&Path, &str) -> (PathBuf, i64),
) -> (String, String) {
    let cassette = Cassette::from_json_bytes(cassette_bytes).expect("reparse");
    let replay_provider = CassetteReplayTransport::new(cassette).as_alloy_provider();
    let dir = TempDir::new().expect("golden temp dir");
    let (db2, market_id) = seed(dir.path(), "golden.db");
    let (ledger, _state) = LedgerDb::open_for_writes(&db2).expect("ledger opens");
    run_aave_update_on_db(
        ledger.db(),
        1,
        market_id,
        Some(SPAN_TO),
        SPAN_TO - DEPLOY_BLOCK + 1,
        replay_provider,
        Arc::new(AtomicBool::new(false)),
        Arc::new(NoProgress),
        false,
        None,
        false,
        None,
    )
    .expect("the golden replay of the scenario chunk must commit cleanly");
    let records = ledger.records().expect("the capture session is armed");
    let ledger_json = ledger_golden_json(&records);
    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES).expect("dump");
    drop(conn);
    (ledger_json, dump_json)
}

/// The scenario's provenance — fully deterministic (the pinned source string,
/// the epoch-anchored capture time; no wall clock). The scenario name rides
/// the source string.
fn provenance(name: &'static str, source: &'static str) -> CassetteProvenance {
    CassetteProvenance {
        source: format!("{source} [{name}]"),
        recorded_at: rfc3339_utc(0),
        span: CassetteSpan {
            from_block: DEPLOY_BLOCK,
            to_block: SPAN_TO,
        },
    }
}

/// The keccak-256 label for one artifact (deterministic content hash for the
/// sign-off table; the drift gates re-verify the bytes themselves).
fn content_hash(bytes: &[u8]) -> String {
    format!(
        "keccak:0x{}",
        alloy::primitives::hex::encode(keccak256(bytes))
    )
}

/// The scenario-1 recording pass: drive the actors, run the chunk loop over
/// the recording transport, flush the cassette, replay for the SQL goldens.
fn generate_s1() -> ScenarioArtifacts {
    let name = "w3_aave_cross_tx_fact_dependence";
    let user: Address = W3_S1_USER.parse().expect("user literal");
    let gho: Address = W3_GHO.parse().expect("gho literal");
    let (pool_creation, vtoken_creation) = s1_actors(user, gho);

    let mut driver = ScratchDriver::new(CHAIN_ID, DEPLOY_BLOCK, TS_BASE);
    let pool_actor = driver
        .deploy_frame(Bytes::from(pool_creation), FRAME_GAS)
        .expect("pool actor deploys");
    assert_pin("s1.pool_actor", pool_actor);
    let v_token_actor = driver
        .deploy_frame(Bytes::from(vtoken_creation), FRAME_GAS)
        .expect("vToken actor deploys");
    assert_pin("s1.vtoken_actor", v_token_actor);
    let coordinator = driver
        .deploy_frame(
            Bytes::from(
                scripted_coordinator_creation_code(
                    &[(pool_actor, 1), (v_token_actor, 1)],
                    word(U256::ONE),
                )
                .expect("coordinator assembles"),
            ),
            FRAME_GAS,
        )
        .expect("coordinator deploys");
    assert_pin("s1.coordinator", coordinator);

    // The two tx frames: the SAME coordinator call at two blocks — tx N
    // (first-seen user, the path-#2 RPC) and tx N+1 (the overlay read).
    for block in [1001, 1002] {
        driver.advance_to(block).expect("head advances");
        driver
            .call_frame(coordinator, Bytes::from(vec![0u8; 1]), FRAME_GAS)
            .unwrap_or_else(|e| panic!("scenario-1 coordinator frame at {block}: {e}"));
    }
    // The recording run over the seeded harness DB (one chunk [1000..1002]).
    let dir = TempDir::new().expect("temp dir");
    let reserves = |v_token: &str| {
        vec![ReserveSeed {
            underlying: W3_GHO.to_string(),
            a_token: W3_ATOKEN.to_string(),
            v_token: v_token.to_string(),
            gho_link: true,
        }]
    };
    let (db_path, market_id) = seeded_db(
        dir.path(),
        "record.db",
        &pool_actor.to_checksum(None),
        &reserves(&v_token_actor.to_checksum(None)),
        DEPLOY_BLOCK,
    );
    let recorder = RecordingTransport::new(driver.chain().clone().boxed());
    let provider = recorder.as_alloy_provider();
    let report = run_recorded_chunk(&db_path, market_id, provider);
    println!(
        "  {name}: {} events applied, {} chunks",
        report.total_events_applied, report.chunks_committed
    );

    let cassette = recorder.cassette(
        CHAIN_ID,
        provenance(
            name,
            "degenbot-scratchevm/wave3 (scripted aave actors, fixture EVM)",
        ),
    );
    let cassette_bytes = cassette.canonical_bytes().expect("canonical bytes");
    let (ledger, dump) = golden_pass(&cassette_bytes, |dir, file| {
        seeded_db(
            dir,
            file,
            &pool_actor.to_checksum(None),
            &reserves(&v_token_actor.to_checksum(None)),
            DEPLOY_BLOCK,
        )
    });
    ScenarioArtifacts {
        name,
        cassette: cassette_bytes,
        ledger,
        dump,
    }
}

/// The scenario-2 recording pass: the two proxy actors' `Upgraded` frames,
/// the implementation actor's memo reads, the chunk loop, the goldens. The
/// implementation deploys FIRST (its resolved address names the `Upgraded`
/// event payloads — its own creation code needs no address), then the two
/// proxies, then one direct proxy call per tx (the two same-impl revision
/// probes with the second `Upgraded` between them).
fn generate_s2() -> ScenarioArtifacts {
    let name = "w3_aave_upgraded_boundary_revision_memo";
    let mut driver = ScratchDriver::new(CHAIN_ID, DEPLOY_BLOCK, TS_BASE);
    let impl_actor = driver
        .deploy_frame(
            Bytes::from(
                scripted_actor_creation_code(&[] as &[ActorBranch], impl_revision_word())
                    .expect("impl actor assembles"),
            ),
            FRAME_GAS,
        )
        .expect("impl actor deploys");
    assert_pin("s2.impl_actor", impl_actor);
    let (proxy1_creation, proxy2_creation, _impl_creation) = s2_actors(impl_actor);
    let proxy_1 = driver
        .deploy_frame(Bytes::from(proxy1_creation), FRAME_GAS)
        .expect("proxy-1 actor deploys");
    assert_pin("s2.proxy_1_actor", proxy_1);
    let proxy_2 = driver
        .deploy_frame(Bytes::from(proxy2_creation), FRAME_GAS)
        .expect("proxy-2 actor deploys");
    assert_pin("s2.proxy_2_actor", proxy_2);

    // tx N (block 1001): proxy 1 upgrades to the implementation — the FIRST
    // same-impl revision probe (the memo inserts at this block).
    driver.advance_to(1001).expect("head advances");
    driver
        .call_frame(proxy_1, Bytes::from(vec![0u8; 1]), FRAME_GAS)
        .expect("scenario-2 proxy-1 frame");
    // tx N+1 (block 1002): proxy 2 upgrades to the SAME implementation — the
    // SECOND same-impl revision probe sits AFTER the `Upgraded`; the memo's
    // block lane must make it a real recorded RPC at this block.
    driver.advance_to(1002).expect("head advances");
    driver
        .call_frame(proxy_2, Bytes::from(vec![0u8; 1]), FRAME_GAS)
        .expect("scenario-2 proxy-2 frame");

    // The recording run over the seeded harness DB (one chunk [1000..1002];
    // two assets whose vTokens are the proxies — no GHO row, the discount
    // path is inert here).
    let dir = TempDir::new().expect("temp dir");
    let reserves = |p1: &str, p2: &str| {
        vec![
            ReserveSeed {
                underlying: W3_S2_UNDERLYING_1.to_string(),
                a_token: W3_S2_ATOKEN_1.to_string(),
                v_token: p1.to_string(),
                gho_link: false,
            },
            ReserveSeed {
                underlying: W3_S2_UNDERLYING_2.to_string(),
                a_token: W3_S2_ATOKEN_2.to_string(),
                v_token: p2.to_string(),
                gho_link: false,
            },
        ]
    };
    let (db_path, market_id) = seeded_db(
        dir.path(),
        "record.db",
        W3_S2_POOL,
        &reserves(&proxy_1.to_checksum(None), &proxy_2.to_checksum(None)),
        DEPLOY_BLOCK,
    );
    let recorder = RecordingTransport::new(driver.chain().clone().boxed());
    let provider = recorder.as_alloy_provider();
    let report = run_recorded_chunk(&db_path, market_id, provider);
    println!(
        "  {name}: {} events applied, {} chunks",
        report.total_events_applied, report.chunks_committed
    );

    let cassette = recorder.cassette(
        CHAIN_ID,
        provenance(
            name,
            "degenbot-scratchevm/wave3 (scripted aave actors, fixture EVM)",
        ),
    );
    let cassette_bytes = cassette.canonical_bytes().expect("canonical bytes");
    let (ledger, dump) = golden_pass(&cassette_bytes, |dir, file| {
        seeded_db(
            dir,
            file,
            W3_S2_POOL,
            &reserves(&proxy_1.to_checksum(None), &proxy_2.to_checksum(None)),
            DEPLOY_BLOCK,
        )
    });
    ScenarioArtifacts {
        name,
        cassette: cassette_bytes,
        ledger,
        dump,
    }
}

/// The scenario-3 recording pass (the revival fixture): tx N borrows
/// (first-seen user → the path-#2 `getDiscountPercent` RPC, recorded) and tx
/// N+1 accrues (the DB-cache read of the row tx N's apply created — the
/// overlay reuse on the mint side), then the chunk loop + goldens.
fn generate_s3() -> ScenarioArtifacts {
    let name = "w3_aave_discount_revival_fixture";
    let user: Address = W3_S3_USER.parse().expect("user literal");
    let gho: Address = W3_GHO.parse().expect("gho literal");
    let (pool_creation, vtoken_creation) = s3_actors(user, gho);

    let mut driver = ScratchDriver::new(CHAIN_ID, DEPLOY_BLOCK, TS_BASE);
    let pool_actor = driver
        .deploy_frame(Bytes::from(pool_creation), FRAME_GAS)
        .expect("pool actor deploys");
    assert_pin("s3.pool_actor", pool_actor);
    let v_token_actor = driver
        .deploy_frame(Bytes::from(vtoken_creation), FRAME_GAS)
        .expect("vToken actor deploys");
    assert_pin("s3.vtoken_actor", v_token_actor);
    let coordinator = driver
        .deploy_frame(
            Bytes::from(
                scripted_coordinator_creation_code(
                    &[(pool_actor, 1), (v_token_actor, 1)],
                    word(U256::ONE),
                )
                .expect("coordinator assembles"),
            ),
            FRAME_GAS,
        )
        .expect("coordinator deploys");
    assert_pin("s3.coordinator", coordinator);

    // tx N (block 1001): Borrow + Mint + Transfer (the paired GhoBorrow op —
    // the ops parser creates the user + GHO debt position; the pre-pass
    // issues the path-#2 `getDiscountPercent` RPC for the first-seen user).
    driver.advance_to(1001).expect("head advances");
    driver
        .call_frame(coordinator, Bytes::from(vec![0u8; 1]), FRAME_GAS)
        .expect("scenario-3 borrow frame");
    // tx N+1 (block 1002): the pure-interest accrual Mint (value ==
    // balanceIncrease, moved index) — the pre-pass reads the row tx N's
    // apply created (path #1, no RPC).
    driver.advance_to(1002).expect("head advances");
    driver
        .call_frame(v_token_actor, Bytes::from(vec![0u8; 2]), FRAME_GAS)
        .expect("scenario-3 accrual frame");

    // The recording run over the seeded harness DB (one chunk [1000..1002]).
    let dir = TempDir::new().expect("temp dir");
    let reserves = |v_token: &str| {
        vec![ReserveSeed {
            underlying: W3_GHO.to_string(),
            a_token: W3_ATOKEN.to_string(),
            v_token: v_token.to_string(),
            gho_link: true,
        }]
    };
    let (db_path, market_id) = seeded_db(
        dir.path(),
        "record.db",
        &pool_actor.to_checksum(None),
        &reserves(&v_token_actor.to_checksum(None)),
        DEPLOY_BLOCK,
    );
    let recorder = RecordingTransport::new(driver.chain().clone().boxed());
    let provider = recorder.as_alloy_provider();
    let report = run_recorded_chunk(&db_path, market_id, provider);
    println!(
        "  {name}: {} events applied, {} chunks",
        report.total_events_applied, report.chunks_committed
    );

    let cassette = recorder.cassette(
        CHAIN_ID,
        provenance(
            name,
            "degenbot-scratchevm/wave3 DISCOUNT-REVIVAL FIXTURE (counterfactual: the fixture \
             chain's GHO vToken `DEBT_TOKEN_REVISION()` = 1 sits below the deprecation \
             revision 4, so the discount pre-pass's path-#2 RPC shape is recorded end-to-end \
             and the QR7QVT option-(a) fact-run shape stays executable if the discount path \
             ever revives; NOT live behavior on the current chain)",
        ),
    );
    let cassette_bytes = cassette.canonical_bytes().expect("canonical bytes");
    let (ledger, dump) = golden_pass(&cassette_bytes, |dir, file| {
        seeded_db(
            dir,
            file,
            &pool_actor.to_checksum(None),
            &reserves(&v_token_actor.to_checksum(None)),
            DEPLOY_BLOCK,
        )
    });
    ScenarioArtifacts {
        name,
        cassette: cassette_bytes,
        ledger,
        dump,
    }
}

fn generate_all() -> Vec<ScenarioArtifacts> {
    vec![generate_s1(), generate_s2(), generate_s3()]
}

fn main() -> std::process::ExitCode {
    let check = std::env::args().any(|arg| arg == "--check");
    println!("wave-3 aave adversarial capture generator ");

    println!("pass 1 ...");
    let pass1 = generate_all();
    println!("pass 2 (determinism gate) ...");
    let pass2 = generate_all();
    for (a, b) in pass1.iter().zip(&pass2) {
        assert_eq!(a.name, b.name);
        if a.cassette != b.cassette {
            let pos = a
                .cassette
                .iter()
                .zip(b.cassette.iter())
                .position(|(x, y)| x != y)
                .unwrap_or(a.cassette.len());
            panic!(
                "{}: cassette not byte-identical across passes (first diff at byte {pos}) — a \
                 nondeterministic generator is a defect (ADR-068 D3)",
                a.name
            );
        }
        assert_eq!(
            a.ledger, b.ledger,
            "{}: statement ledger not byte-identical",
            a.name
        );
        assert_eq!(a.dump, b.dump, "{}: db dump not byte-identical", a.name);
    }
    println!("determinism: two passes byte-identical across all scenarios");

    for artifacts in &pass1 {
        println!(
            "  {}\n    cassette {} ({} bytes)\n    ledger   {} ({} bytes)\n    dump     {} ({} bytes)",
            artifacts.name,
            content_hash(&artifacts.cassette),
            artifacts.cassette.len(),
            content_hash(artifacts.ledger.as_bytes()),
            artifacts.ledger.len(),
            content_hash(artifacts.dump.as_bytes()),
            artifacts.dump.len(),
        );
    }

    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let cassette_home = home.join("tests/fixtures/cassettes/wave3");
    let golden_home = home.join("tests/fixtures/sql_goldens/wave3");

    if check {
        let mut drifted = 0usize;
        for artifacts in &pass1 {
            let checks = [
                (
                    cassette_home.join(format!("{}.json", artifacts.name)),
                    artifacts.cassette.as_slice(),
                ),
                (
                    golden_home.join(format!("{}.statement-ledger.json", artifacts.name)),
                    artifacts.ledger.as_bytes(),
                ),
                (
                    golden_home.join(format!("{}.db-dump.json", artifacts.name)),
                    artifacts.dump.as_bytes(),
                ),
            ];
            for (path, produced) in checks {
                match std::fs::read(&path) {
                    Ok(committed) if committed == produced => {}
                    Ok(_committed) => {
                        eprintln!("DRIFT: {} (regenerated bytes differ)", path.display());
                        drifted += 1;
                    }
                    Err(e) => {
                        eprintln!("MISSING: {} ({e})", path.display());
                        drifted += 1;
                    }
                }
            }
        }
        if drifted == 0 {
            println!("--check: all committed wave-3 artifacts byte-identical to regeneration");
            std::process::ExitCode::SUCCESS
        } else {
            eprintln!("--check: {drifted} artifact(s) drifted");
            std::process::ExitCode::FAILURE
        }
    } else {
        std::fs::create_dir_all(&cassette_home).expect("create cassette home");
        std::fs::create_dir_all(&golden_home).expect("create golden home");
        for artifacts in &pass1 {
            std::fs::write(
                cassette_home.join(format!("{}.json", artifacts.name)),
                &artifacts.cassette,
            )
            .expect("write cassette");
            std::fs::write(
                golden_home.join(format!("{}.statement-ledger.json", artifacts.name)),
                &artifacts.ledger,
            )
            .expect("write ledger golden");
            std::fs::write(
                golden_home.join(format!("{}.db-dump.json", artifacts.name)),
                &artifacts.dump,
            )
            .expect("write dump golden");
        }
        println!(
            "wrote {} scenario(s) under {} + {}",
            pass1.len(),
            cassette_home.display(),
            golden_home.display()
        );
        std::process::ExitCode::SUCCESS
    }
}
