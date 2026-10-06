//! Generate the wave-2 EVM-oracle golden captures (the wave-2 capture program, ADR-068
//! approach C) — node-free, deterministic, through the recorder's own
//! artifact writers.
//!
//! Each scenario drives the REAL canonical `UniswapV3Pool` (via the committed
//! `V3CaptureHarness` artifact — real deploy / initialize / mint / burn /
//! swap frames, real `PoolCreated`/`Mint`/`Burn`/`Swap` events) against a
//! [`ScratchDriver`](`degenbot_simulation::capture::ScratchChain`) chain, then
//! runs the REAL `run_pool_update` chunk loop over a recording transport —
//! the flushed cassette is the recorder's canonical writer's bytes, exactly
//! what a live recording produces. The SQL goldens are then replayed from the
//! committed cassette bytes through the statement-ledger wrapper — the same
//! writers `sql_golden_replay.rs` gates with.
//!
//! Determinism is the acceptance gate: the whole generation runs TWICE and
//! both passes must be byte-identical (asserted here; the corpus `--check`
//! + the replay suites re-prove it from disk).
//!
//! Usage:
//!
//!     cargo run --locked --manifest-path rust/Cargo.toml -p degenbot \\
//!         --features degenbot/sql-ledger \\
//!         --example generate_evm_oracle_captures -- \\
//!         --check tests/fixtures/cassettes/wave2 tests/fixtures/sql_goldens/wave2
//!
//! `just check-evm-captures` is this exact invocation as a recipe. The
//! `sql-ledger` passthrough is REQUIRED (the `[[example]]` entry's
//! required-features — without the feature the `degenbot_db::sql_ledger`
//! import fails E0432).
//!
//! Default mode (re)writes `tests/fixtures/cassettes/wave2/` +
//! `tests/fixtures/sql_goldens/wave2/`. `--check` regenerates into a temp
//! home and exits 1 on any byte drift against the committed artifacts.

// Run-once diagnostic example: stdout/stderr reports ARE its interface (the
// record_updater_cassette precedent), and the two-pass determinism gate
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

use alloy::primitives::{keccak256, Address, Bytes, U256};
use degenbot::db::DegenbotDb;
use degenbot::pool_updater::{run_pool_update, run_pool_update_on_db, NoProgress};
use degenbot::rpc::cassette::{
    rfc3339_utc, Cassette, CassetteProvenance, CassetteSpan, RecordingTransport,
};
use degenbot::rpc::provider::LogFilter;
use degenbot_db::sql_ledger::{dump_tables_golden_json, ledger_golden_json, LedgerDb};
use degenbot_rpc::cassette_replay::CassetteReplayTransport;
use degenbot_simulation::capture::{
    encode_capture_announce_pool, encode_capture_burn, encode_capture_initialize,
    encode_capture_mint, encode_capture_pool_getter, encode_capture_setup_pool,
    encode_capture_swap, load_capture_harness_creation_bytecode, log_emitter_initcode,
    ScratchDriver,
};
use tempfile::TempDir;

/// The chain every scenario serves (mainnet-shaped; `eth_chainId` answers
/// `0x1` — the odd-digit quantity the precision rule decimalizes exactly).
const CHAIN_ID: u64 = 1;
/// Deterministic block-timestamp base: block `b` serves `TS_BASE + b` — no
/// wall clock anywhere in the artifacts.
const TS_BASE: u64 = 1_700_000_000;
/// The capture harness deploys at this deterministic block (the frames'
/// coordinates are scenario constants from here on).
const DEPLOY_BLOCK: u64 = 1000;
/// Harness-deploy gas (the tier-3 harnesses' documented budget).
const DEPLOY_GAS: u64 = 16_700_000;

/// The deterministic factory address the exchange rows seed with: the capture
/// harness's own address for pool scenarios (it IS the factory role), and the
/// same pinned address for the pool-less scenario (whose creation fetch must
/// replay against the identical seeded exchange). An independent literal: it
/// changes only if the harness artifact is rebuilt.
const WAVE2_FACTORY: &str = "0xbd770416a3345f91e4b34576cb804a576fa48eb1";

/// `sqrtPriceX96` for tick 0 (independent literal: 2^96).
const SQRT_RATIO_AT_TICK_0: U256 = U256::from_limbs([
    0x0000_0000_0000_0000,
    0x0000_0000_0001_0000, // 2^32 → 2^(64+32) = 2^96
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
]);
/// `MIN_SQRT_RATIO + 1` — the canonical zeroForOne swap price bound.
const MIN_SQRT_RATIO_PLUS_1: U256 = U256::from_limbs([4_295_128_741, 0, 0, 0]);

/// One pool frame the scenario drives through the REAL pool contract.
#[derive(Clone, Copy)]
enum Frame {
    /// Real `pool.mint` (the harness pays via the mint callback).
    Mint {
        block: u64,
        lower: i32,
        upper: i32,
        amount: u128,
    },
    /// Real `pool.burn` (the harness owns the position).
    Burn {
        block: u64,
        lower: i32,
        upper: i32,
        amount: u128,
    },
    /// Real `pool.swap` (zeroForOne; the harness pays via the swap callback).
    Swap { block: u64, amount: u128 },
}

/// One hand-rolled log emitter: a raw `LOGn` with the given topics + data,
/// emitted by real EVM execution during the emitter's deploy frame.
struct Emitter {
    block: u64,
    topics: Vec<alloy::primitives::B256>,
    data: Vec<u8>,
}

/// A wave-2 scenario.
struct Scenario {
    name: &'static str,
    /// The pool the harness deploys: (fee, tickSpacing). `None` = no pool
    /// (the hand-rolled-log scenario runs the updater over emitter logs).
    pool: Option<(u32, i32)>,
    frames: Vec<Frame>,
    emitters: Vec<Emitter>,
    /// The updater run's chunks (inclusive spans) — the frames execute
    /// incrementally so each chunk's verification reads state AT its end
    /// block (the driver publishes snapshots per executed block).
    chunks: Vec<(u64, u64)>,
    verify_chunk: bool,
    /// A generator-issued topic-less getLogs window recorded alongside the
    /// updater's own fetch surface (the raw emitter-log ride).
    raw_log_window: Option<(u64, u64)>,
}

/// The V3 `Mint` topic (independent constant — the hand-rolled logs must
/// match the updater's real fetch filter to ride its getLogs answer).
fn v3_mint_topic() -> alloy::primitives::B256 {
    alloy::primitives::b256!("0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde")
}

/// An arbitrary deterministic topic (the emitter logs that must NOT match the
/// updater's mint/burn filter — they ride only the raw window).
fn arbitrary_topic(seed: u8) -> alloy::primitives::B256 {
    let mut word = [0u8; 32];
    word[0] = 0xde;
    word[1] = 0xad;
    word[31] = seed;
    alloy::primitives::B256::from(word)
}

/// A hand-rolled V3 `Mint` EVENT as a real `LOG4`: full 4-topic + 128-byte
/// data payload (owner/ticks indexed, sender/amount/amount0/amount1 in data),
/// executed by real EVM `LOG` opcodes. The canonical way to feed the apply's
/// zero-delta (skipped-write) branch: the real pool's `mint` hard-rejects
/// `amount = 0` (`require(amount > 0)`), so a zero-amount Mint event can only
/// reach the updater from a non-pool producer — exactly the case the branch
/// exists for.
fn zero_mint_log_emitter(
    block: u64,
    owner_seed: u8,
    lower: i32,
    upper: i32,
    amount: u128,
) -> Emitter {
    let sig = v3_mint_topic();
    let mut owner = [0u8; 32];
    owner[31] = owner_seed;
    let tick_word = |tick: i32| -> [u8; 32] {
        alloy::primitives::aliases::I256::try_from(tick)
            .expect("tick fits i256")
            .into_raw()
            .to_be_bytes::<32>()
    };
    let mut sender = [0u8; 32];
    sender[12..32].copy_from_slice(&Address::from([0x5eu8; 20]).into_array());
    let mut data = Vec::with_capacity(128);
    data.extend_from_slice(&sender);
    data.extend_from_slice(&{
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&amount.to_be_bytes());
        w
    });
    data.extend_from_slice(&[0u8; 32]); // amount0 = 0
    data.extend_from_slice(&[0u8; 32]); // amount1 = 0
    Emitter {
        block,
        topics: vec![
            sig,
            alloy::primitives::B256::from(owner),
            alloy::primitives::B256::from(tick_word(lower)),
            alloy::primitives::B256::from(tick_word(upper)),
        ],
        data,
    }
}

/// The five wave-2 scenarios (the wave-2 capture program body + the manager's deliverables).
fn scenarios() -> Vec<Scenario> {
    vec![
        // (a) cross-chunk reorg window over the flip-flip-then-swap motif on a
        // dense CL pool: PoolCreated + flip #1 in chunk 1, flip #2 + the swap
        // crossing in chunk 2 — chunk 2's read pass + marker logic must
        // survive on chunk 1's committed map.
        Scenario {
            name: "w2_cl_cross_chunk_flip_flip_swap",
            pool: Some((500, 10)),
            frames: vec![
                Frame::Mint {
                    block: 1001,
                    lower: -100,
                    upper: 100,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Mint {
                    block: 1002,
                    lower: -200,
                    upper: -100,
                    amount: 500_000_000_000_000,
                },
                Frame::Swap {
                    block: 1003,
                    amount: 1_000_000_000_000_000_000,
                },
            ],
            emitters: vec![],
            chunks: vec![(DEPLOY_BLOCK, 1001), (1002, 1003), (1004, 1004)],
            verify_chunk: true,
            raw_log_window: None,
        },
        // (b) the 0x5a17f7ce-family (V3 `mint`) bitmap counterexample: three
        // positions whose boundary ticks are ALL multiples of the 100 tick
        // spacing and ALL live in bitmap word 0 (compressed 1..=4) — a greedy
        // search that halts at the first set bit (p = k·100) misses the rest.
        // Pinned so the search fix is provably exact by replay.
        Scenario {
            name: "w2_bitmap_5a17f7ce_early_termination",
            pool: Some((3000, 100)),
            frames: vec![
                Frame::Mint {
                    block: 1001,
                    lower: 100,
                    upper: 200,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Mint {
                    block: 1002,
                    lower: 200,
                    upper: 300,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Mint {
                    block: 1003,
                    lower: 300,
                    upper: 400,
                    amount: 1_000_000_000_000_000,
                },
            ],
            emitters: vec![],
            chunks: vec![(DEPLOY_BLOCK, 1004)],
            verify_chunk: true,
            raw_log_window: None,
        },
        // (c) short-hex / non-minimal hex DATA fields (the cassette precision
        // rule): real `LOG` opcodes carrying `0x00`, `0x00000000`,
        // `0x06fdde03`, `0x0000` — plus a hand-rolled log that MATCHES the
        // updater's Mint filter but truncates the Mint data (the decoders'
        // documented skip path). The odd-digit quantity case rides every
        // cassette's `eth_chainId` answer (`0x1`).
        Scenario {
            name: "w2_short_hex_hand_rolled_logs",
            pool: None,
            frames: vec![],
            emitters: vec![
                Emitter {
                    block: 1000,
                    topics: vec![v3_mint_topic()],
                    data: vec![0x00],
                },
                Emitter {
                    block: 1000,
                    topics: vec![
                        v3_mint_topic(),
                        arbitrary_topic(1),
                        arbitrary_topic(2),
                        arbitrary_topic(3),
                    ],
                    data: vec![0x00, 0x00, 0x00, 0x00],
                },
                Emitter {
                    block: 1001,
                    topics: vec![arbitrary_topic(4)],
                    data: vec![0x01],
                },
                Emitter {
                    block: 1001,
                    topics: vec![arbitrary_topic(5)],
                    data: vec![0x06, 0xfd, 0xde, 0x03],
                },
                Emitter {
                    block: 1002,
                    topics: vec![arbitrary_topic(6)],
                    data: vec![0x00, 0x00],
                },
            ],
            chunks: vec![(DEPLOY_BLOCK, 1004)],
            verify_chunk: false,
            raw_log_window: Some((DEPLOY_BLOCK, 1002)),
        },
        // (d) the delta-persist branches (Perf B): a real zero-amount Mint
        // (the pool emits the event; the apply's skipped-write branch) and a
        // real full drain (chunk 2's complement-delete fires with a NON-empty
        // drained set — the empty-live-set delete-all branch the corpus
        // cannot produce, where a chunk-new pool's drained set is empty).
        Scenario {
            name: "w2_drained_pool_zero_mint",
            pool: Some((500, 10)),
            frames: vec![
                Frame::Mint {
                    block: 1001,
                    lower: -100,
                    upper: 100,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Burn {
                    block: 1003,
                    lower: -100,
                    upper: 100,
                    amount: 1_000_000_000_000_000,
                },
            ],
            emitters: vec![zero_mint_log_emitter(1002, 0x77, -100, 100, 0)],
            chunks: vec![(DEPLOY_BLOCK, 1001), (1002, 1003), (1004, 1004)],
            verify_chunk: true,
            raw_log_window: None,
        },
        // (5, ergo body) negative/odd tick values through the ABI encoder: an
        // ODD positive spacing (5) with mints at NEGATIVE ticks — the Mint
        // calldata, the Mint event topics, the verification's `ticks(int24)`
        // args (negative, sign-extended) and `tickBitmap(int16)` words
        // (negative: -1000/5 = -200 -> word -1) all ride the sign-extension
        // law end-to-end. (A NEGATIVE spacing pool was tried first: the real
        // pool's constructor accepts it and mints succeed, but the pool's own
        // compressed-tick invariants invert (lower > upper in compressed
        // space) and a plain down-swap reverts LiquidityMath 'LS' — recorded
        // as a FINDING in the task sign-off, not silently fixed here.)
        Scenario {
            name: "w2_odd_spacing_negative_ticks_sign_extension",
            pool: Some((500, 5)),
            frames: vec![
                Frame::Mint {
                    block: 1001,
                    lower: -1000,
                    upper: -500,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Mint {
                    block: 1002,
                    lower: 500,
                    upper: 1000,
                    amount: 1_000_000_000_000_000,
                },
                Frame::Swap {
                    block: 1003,
                    amount: 1_000_000_000_000_000_000,
                },
            ],
            emitters: vec![],
            chunks: vec![(DEPLOY_BLOCK, 1004)],
            verify_chunk: true,
            raw_log_window: None,
        },
    ]
}

/// The scenario's executed products: the deterministic harness address (the
/// exchange row's factory) and the pending frame schedule (block-ordered).
struct ChainFacts {
    harness: Address,
}

/// One executable step: a real pool frame or a hand-rolled log emitter's
/// deploy frame, pinned to its block.
enum Step {
    Frame(Frame),
    Emitter(Emitter),
}

/// The prologue (deploy + setupPool + initialize + the REAL `PoolCreated`
/// announcement) executes at [`DEPLOY_BLOCK`] — a pure function of the
/// scenario. No clock, no entropy.
fn drive_prologue(
    scenario: &Scenario,
    artifacts_dir: &Path,
) -> Result<(ScratchDriver, ChainFacts, Vec<Step>), String> {
    let mut driver = ScratchDriver::new(CHAIN_ID, DEPLOY_BLOCK, TS_BASE);
    let mut steps: Vec<Step> = Vec::new();
    let mut facts = ChainFacts {
        harness: WAVE2_FACTORY.parse().expect("pinned factory literal"),
    };

    if let Some((fee, spacing)) = scenario.pool {
        // Deploy the REAL capture harness (creation bytecode + ctor args).
        let creation = load_capture_harness_creation_bytecode(artifacts_dir)?;
        let mut init_code = creation;
        init_code.extend_from_slice(&capture_harness_ctor_args(fee, spacing));
        facts.harness = driver
            .deploy_frame(Bytes::from(init_code), DEPLOY_GAS)
            .map_err(|e| format!("harness deploy: {e}"))?;

        // Real pool deployment (the deferred CREATE gets full forwarded gas).
        driver
            .call_frame(facts.harness, encode_capture_setup_pool(), DEPLOY_GAS)
            .map_err(|e| format!("setupPool: {e}"))?;

        // Resolve the pool address through the harness getter (view).
        let out = driver
            .call_view(facts.harness, encode_capture_pool_getter(), 2_000_000)
            .map_err(|e| format!("pool(): {e}"))?;
        let pool = Address::from_slice(&out.as_ref()[12..32]);

        // Real initialize (slot0 at the pinned price) + the REAL PoolCreated
        // announcement the creation fetch filters on.
        driver
            .call_frame(
                pool,
                encode_capture_initialize(SQRT_RATIO_AT_TICK_0),
                5_000_000,
            )
            .map_err(|e| format!("initialize: {e}"))?;
        driver
            .call_frame(facts.harness, encode_capture_announce_pool(), 2_000_000)
            .map_err(|e| format!("announcePool: {e}"))?;
    }

    for frame in &scenario.frames {
        steps.push(Step::Frame(*frame));
    }
    for emitter in &scenario.emitters {
        steps.push(Step::Emitter(Emitter {
            block: emitter.block,
            topics: emitter.topics.clone(),
            data: emitter.data.clone(),
        }));
    }
    // Block-ordered schedule (the frame order within a block stays the
    // scenario's declared order — frames before emitters of the same block
    // would be nondeterministic otherwise; none of the scenarios mix them).
    steps.sort_by_key(|step| match step {
        Step::Frame(frame) => match frame {
            Frame::Mint { block, .. } | Frame::Burn { block, .. } | Frame::Swap { block, .. } => {
                (*block, 0u8)
            }
        },
        Step::Emitter(emitter) => (emitter.block, 1u8),
    });
    Ok((driver, facts, steps))
}

/// Execute every pending step whose block is `<= to` (frames land in their
/// block; the driver publishes the committed state per block).
fn execute_until(
    driver: &mut ScratchDriver,
    facts: &ChainFacts,
    pending: &mut Vec<Step>,
    to: u64,
) -> Result<(), String> {
    let mut executed = 0usize;
    for step in pending.iter() {
        let block = match step {
            Step::Frame(
                Frame::Mint { block, .. } | Frame::Burn { block, .. } | Frame::Swap { block, .. },
            )
            | Step::Emitter(Emitter { block, .. }) => *block,
        };
        if block > to {
            break;
        }
        executed += 1;
        driver.advance_to(block)?;
        match step {
            Step::Frame(Frame::Mint {
                lower,
                upper,
                amount,
                ..
            }) => {
                driver
                    .call_frame(
                        facts.harness,
                        encode_capture_mint(*lower, *upper, *amount),
                        DEPLOY_GAS,
                    )
                    .map_err(|e| format!("mint frame at {block}: {e}"))?;
            }
            Step::Frame(Frame::Burn {
                lower,
                upper,
                amount,
                ..
            }) => {
                driver
                    .call_frame(
                        facts.harness,
                        encode_capture_burn(*lower, *upper, *amount),
                        DEPLOY_GAS,
                    )
                    .map_err(|e| format!("burn frame at {block}: {e}"))?;
            }
            Step::Frame(Frame::Swap { amount, .. }) => {
                let amount = alloy::primitives::aliases::I256::try_from(*amount)
                    .expect("swap amount fits i256");
                driver
                    .call_frame(
                        facts.harness,
                        encode_capture_swap(true, amount, MIN_SQRT_RATIO_PLUS_1),
                        DEPLOY_GAS,
                    )
                    .map_err(|e| format!("swap frame at {block}: {e}"))?;
            }
            Step::Emitter(emitter) => {
                let code = log_emitter_initcode(&emitter.topics, &emitter.data)?;
                driver
                    .deploy_frame(Bytes::from(code), 500_000)
                    .map_err(|e| format!("emitter deploy at {block}: {e}"))?;
            }
        }
    }
    pending.drain(..executed);
    Ok(())
}

/// `(uint24 fee, int24 tickSpacing)` constructor args (the CL harness ABI).
fn capture_harness_ctor_args(fee: u32, tick_spacing: i32) -> Vec<u8> {
    let mut args = Vec::with_capacity(64);
    let mut fee_word = [0u8; 32];
    // uint24: the low three bytes of the u32 big-endian form.
    fee_word[29..].copy_from_slice(&fee.to_be_bytes()[1..4]);
    args.extend_from_slice(&fee_word);
    args.extend_from_slice(
        &alloy::primitives::aliases::I256::try_from(tick_spacing)
            .expect("spacing fits i256")
            .into_raw()
            .to_be_bytes::<32>(),
    );
    args
}

/// A temp DB with the scenario's ACTIVE `uniswap_v3` exchange stamped at
/// `from - 1` (the replay-suite seeding shape; the factory is the harness —
/// a deterministic address).
fn seeded_db(dir: &Path, harness: Address, from: u64, file_name: &str) -> std::path::PathBuf {
    let path = dir.join(file_name);
    let (db, _state) = DegenbotDb::open_for_writes(&path).unwrap();
    let exchange = db
        .upsert_exchange(
            i64::try_from(CHAIN_ID).unwrap(),
            "uniswap_v3",
            harness,
            None,
        )
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE exchanges SET active = 1, last_update_block = ?1 WHERE id = ?2",
        rusqlite::params![i64::try_from(from - 1).unwrap(), exchange.id,],
    )
    .unwrap();
    path
}

/// The recorder's provenance — fully deterministic (the pinned source string,
/// the epoch-anchored capture time; no wall clock).
fn provenance(scenario: &Scenario) -> CassetteProvenance {
    CassetteProvenance {
        source: "degenbot-scratchevm/wave2 (V3CaptureHarness, fixture EVM)".to_string(),
        recorded_at: rfc3339_utc(0),
        span: CassetteSpan {
            from_block: scenario.chunks[0].0,
            to_block: scenario.chunks[scenario.chunks.len() - 1].1,
        },
    }
}

/// The tables one pool-chunk apply touches (the replay suite's dump list).
const DUMP_TABLES: &[&str] = &[
    "erc20_tokens",
    "exchanges",
    "initialization_maps",
    "liquidity_positions",
    "managed_pool_initialization_maps",
    "managed_pool_liquidity_positions",
    "managed_pools",
    "pools",
    "uniswap_v2_pools",
    "uniswap_v3_pools",
    "uniswap_v4_pools",
];

/// Generate ONE scenario's artifacts (cassette bytes + SQL golden strings):
/// the chunk runs interleave with the frame execution so each chunk's
/// verification reads state AT that chunk's end block.
fn generate_scenario(
    scenario: &Scenario,
    artifacts_dir: &Path,
) -> Result<(Vec<u8>, String, String), String> {
    use alloy::transports::Transport as _;

    let (mut driver, facts, mut pending) = drive_prologue(scenario, artifacts_dir)?;

    // The recording run: the REAL chunk loop over the scratch chain through
    // the recorder — the cassette is the recorder's canonical writer's bytes.
    let recorder = RecordingTransport::new(driver.chain().clone().boxed());
    let provider = recorder.as_alloy_provider();

    // The ancillary round trips the live recorder captures too (driven
    // through the recording seam, ONE block_on on the shared runtime — the
    // example thread carries no ambient tokio context).
    degenbot::core::runtime::get_runtime()
        .block_on(async {
            let chain_id = provider.get_chain_id().await.map_err(|e| e.to_string())?;
            if chain_id != CHAIN_ID {
                return Err(format!("served chain id {chain_id} != {CHAIN_ID}"));
            }
            let head_served = provider
                .get_block_number()
                .await
                .map_err(|e| e.to_string())?;
            if head_served != DEPLOY_BLOCK {
                return Err(format!(
                    "initial served head {head_served} != deploy block {DEPLOY_BLOCK}"
                ));
            }
            Ok(())
        })
        .map_err(|e| format!("{}: ancillary: {e}", scenario.name))?;

    let dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let db_path = seeded_db(dir.path(), facts.harness, scenario.chunks[0].0, "record.db");
    for (from, to) in &scenario.chunks {
        execute_until(&mut driver, &facts, &mut pending, *to)?;
        driver.advance_to(*to)?;
        run_pool_update(
            &db_path,
            i64::try_from(CHAIN_ID).unwrap(),
            Some(*to),
            to - from + 1,
            provider.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(NoProgress),
            scenario.verify_chunk,
            None,
            false,
        )
        .map_err(|e| {
            format!(
                "{}: the recording run of chunk [{from}..{to}] must commit cleanly: {e}",
                scenario.name
            )
        })?;
    }
    if !pending.is_empty() {
        return Err(format!(
            "{}: {} scenario steps never executed (blocks beyond the chunks)",
            scenario.name,
            pending.len()
        ));
    }

    // The generator's own raw-log window (the topic-less getLogs ride) —
    // recorded by the same transport BEFORE the cassette flush.
    if let Some((from, to)) = scenario.raw_log_window {
        let filter = LogFilter::new(from, to, None, None)
            .map_err(|e| format!("{}: raw log filter: {e}", scenario.name))?;
        let _ = degenbot::core::runtime::get_runtime()
            .block_on(provider.get_logs(&filter))
            .map_err(|e| format!("{}: raw log window: {e}", scenario.name))?;
    }
    drop(driver);

    let cassette = recorder.cassette(CHAIN_ID, provenance(scenario));
    let cassette_bytes = cassette
        .canonical_bytes()
        .map_err(|e| format!("{}: canonical bytes: {e}", scenario.name))?;

    // ── the SQL golden pass: replay the cassette BYTES through the ledger,
    // chunk by chunk (the same per-chunk shape the recording run used). ──
    let cassette = Cassette::from_json_bytes(&cassette_bytes)
        .map_err(|e| format!("{}: reparse: {e}", scenario.name))?;
    let replay_provider = CassetteReplayTransport::new(cassette).as_alloy_provider();
    let dir2 = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
    let db2 = seeded_db(
        dir2.path(),
        facts.harness,
        scenario.chunks[0].0,
        "golden.db",
    );
    let (ledger, _state) = LedgerDb::open_for_writes(&db2)
        .map_err(|e| format!("{}: ledger open: {e}", scenario.name))?;
    for (from, to) in &scenario.chunks {
        run_pool_update_on_db(
            ledger.db(),
            i64::try_from(CHAIN_ID).unwrap(),
            Some(*to),
            to - from + 1,
            replay_provider.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(NoProgress),
            scenario.verify_chunk,
            None,
            false,
        )
        .map_err(|e| {
            format!(
                "{}: the golden replay of chunk [{from}..{to}] must commit cleanly: {e}",
                scenario.name
            )
        })?;
    }
    let records = ledger
        .records()
        .map_err(|e| format!("{}: ledger records: {e}", scenario.name))?;
    let ledger_json = ledger_golden_json(&records);
    let conn = ledger.db().lock();
    let dump_json = dump_tables_golden_json(&conn, DUMP_TABLES)
        .map_err(|e| format!("{}: dump: {e}", scenario.name))?;
    drop(conn);

    Ok((cassette_bytes, ledger_json, dump_json))
}

/// The artifact homes (repo-root-relative, the recorder corpus layout's
/// per-kind homes with a wave-2 scenario subdirectory).
fn fixtures_home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../tests/fixtures")
}

fn cassette_home(home: &Path) -> PathBuf {
    home.join("cassettes/wave2")
}

fn golden_home(home: &Path) -> PathBuf {
    home.join("sql_goldens/wave2")
}

/// Generate EVERY scenario and return `(name, cassette_bytes, ledger, dump)`.
/// One generated scenario's artifacts: (name, cassette bytes, ledger golden, dump golden).
type ScenarioArtifacts = (String, Vec<u8>, String, String);

fn generate_all(artifacts_dir: &Path) -> Result<Vec<ScenarioArtifacts>, String> {
    let mut out = Vec::new();
    for scenario in scenarios() {
        let (cassette, ledger, dump) = generate_scenario(&scenario, artifacts_dir)?;
        println!(
            "  {:<44} cassette {} bytes, {} ledger statements",
            scenario.name,
            cassette.len(),
            ledger.matches("\\n    {").count() + ledger.matches("\\n  {").count(),
        );
        out.push((scenario.name.to_string(), cassette, ledger, dump));
    }
    Ok(out)
}

/// The keccak-256 label for one artifact (deterministic content hash for the
/// sign-off table; the drift gates re-verify the bytes themselves).
fn content_hash(bytes: &[u8]) -> String {
    format!(
        "keccak:0x{}",
        alloy::primitives::hex::encode(keccak256(bytes))
    )
}

fn main() -> std::process::ExitCode {
    let check = std::env::args().any(|arg| arg == "--check");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let artifacts_dir = repo.join("tier3-oracle/artifacts");

    println!("wave-2 EVM-oracle capture generator ");

    // ── the determinism gate: generate TWICE, require byte-identical ──
    println!("pass 1 ...");
    let pass1 = match generate_all(&artifacts_dir) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("GENERATION FAILED: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("pass 2 (determinism gate) ...");
    let pass2 = match generate_all(&artifacts_dir) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("GENERATION FAILED (pass 2): {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    for ((n1, c1, l1, d1), (n2, c2, l2, d2)) in pass1.iter().zip(&pass2) {
        assert_eq!(n1, n2);
        if c1 != c2 {
            let pos = c1
                .iter()
                .zip(c2.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(c1.len());
            panic!(
                "{n1}: cassette not byte-identical across passes (first diff at byte {pos}) — a nondeterministic generator is a defect (ADR-068 D3)",
            );
        }
        assert_eq!(
            l1, l2,
            "{n1}: statement ledger not byte-identical across runs"
        );
        assert_eq!(d1, d2, "{n1}: db dump not byte-identical across runs");
    }
    println!("determinism: two passes byte-identical across all scenarios");

    if check {
        // ── the generator's own drift gate: the regenerated bytes must equal
        // the committed artifacts byte-for-byte.
        let home = fixtures_home();
        let mut drifted = 0usize;
        for (name, cassette, ledger, dump) in &pass1 {
            let checks = [
                (
                    cassette_home(&home).join(format!("{name}.json")),
                    cassette.as_slice(),
                ),
                (
                    golden_home(&home).join(format!("{name}.statement-ledger.json")),
                    ledger.as_bytes(),
                ),
                (
                    golden_home(&home).join(format!("{name}.db-dump.json")),
                    dump.as_bytes(),
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
            println!("--check: all committed wave-2 artifacts byte-identical to regeneration");
            std::process::ExitCode::SUCCESS
        } else {
            eprintln!("--check: {drifted} artifact(s) drifted");
            std::process::ExitCode::FAILURE
        }
    } else {
        // ── write mode ──
        let home = fixtures_home();
        let chome = cassette_home(&home);
        let ghome = golden_home(&home);
        std::fs::create_dir_all(&chome).expect("create cassette home");
        std::fs::create_dir_all(&ghome).expect("create golden home");
        for (name, cassette, ledger, dump) in &pass1 {
            let cpath = chome.join(format!("{name}.json"));
            std::fs::write(&cpath, cassette).expect("write cassette");
            let lpath = ghome.join(format!("{name}.statement-ledger.json"));
            std::fs::write(&lpath, ledger).expect("write ledger golden");
            let dpath = ghome.join(format!("{name}.db-dump.json"));
            std::fs::write(&dpath, dump).expect("write dump golden");
            println!(
                "  {name}\n    cassette {} ({} bytes)\n    ledger   {} ({} bytes)\n    dump     {} ({} bytes)",
                content_hash(cassette),
                cassette.len(),
                content_hash(ledger.as_bytes()),
                ledger.len(),
                content_hash(dump.as_bytes()),
                dump.len(),
            );
        }
        println!(
            "wrote {} scenario(s) under {} + {}",
            pass1.len(),
            chome.display(),
            ghome.display()
        );
        std::process::ExitCode::SUCCESS
    }
}
