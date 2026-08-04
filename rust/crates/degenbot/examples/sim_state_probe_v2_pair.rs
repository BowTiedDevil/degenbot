//! Live in-process sim-state probe for the path-11354 V2 pair (block 25678283).
//!
//! Builds the PRODUCTION layered DB stack exactly as `BlockSimHandle::build`
//! does (`AlloyDB` → `WrapDatabaseAsync` → `BotStateDb` → `WarmCodeCache` →
//! `CacheDB`), pinned to the solve block `25678283` against the live RPC, then
//! reads the V2 pair's reserve slot (`slot8`) the way the sim's swap execution
//! reads it, and reports what a V2 swap there would produce.
//!
//! # Why this harness exists
//!
//! The recorded path-11354 `[sim-revert-swap]` V2 line:
//! ```text
//!   hop1 V2 0x648Ef94C USDT→stETH  actual=15166900278114 predicted=15166900278115  matched=false (1 wei short)
//! ```
//! The solver is byte-exact to constant-product at the on-chain reserves
//! (proven by `path11354_v3v2v3_solver_fixture`), and the on-chain executor
//! `_v2_get_amount_out` formula is the same `9970/10000` math — so `…115` at
//! the on-chain reserve `…464` is correct. Algebra shows `…114` is
//! **unreachable** from any correct state: it requires a phantom stETH
//! `reserve0 ∈ [1286682034390318, 1286682034390401]` (63–146 wei below
//! on-chain `1286682034390464`), a value that exists on NONE of the blocks
//! `25677000..25678283`. This probe empirically reads what a FAITHFUL sim sees
//! at the solve block (should be `…464` → `…115`), and can INJECT the phantom
//! reserve (`DEGENBOT_PROBE_PHANTOM_RESERVE=1286682034390401`) to reproduce
//! the exact `…114` the live bot logged — proving the mechanism is a
//! sim-side stale V2 reserve, not the solver or the math.
//!
//! # Exit codes
//!
//! - **0** — FAITHFUL: the sim read the on-chain reserve `…464` and a V2 swap
//!   there produces `…115`. The recorded `…114` is NOT reproducible from
//!   correct state — confirming the sim-state artifact diagnosis.
//! - **1** — PHANTOM-INJECTED: the injected phantom reserve reproduced the
//!   logged `…114` exactly (the trap fires — only reached when
//!   `DEGENBOT_PROBE_PHANTOM_RESERVE` is set to a non-on-chain value).
//! - **2** — DIVERGENCE: the sim (without injection) read a reserve `≠ …464`
//!   — the phantom was caught in the act, un-injected.
//!
//! # Run
//! ```text
//! cargo run -p degenbot --example sim_state_probe_v2_pair           # faithful → exit 0
//! DEGENBOT_PROBE_PHANTOM_RESERVE=1286682034390401 cargo run -p degenbot --example sim_state_probe_v2_pair  # → exit 1
//! ```
#![allow(clippy::doc_markdown)]

use alloy::primitives::{Address, Bytes, U256};
use degenbot::bot_core::BotState;
use degenbot::degenbot_executor::WarmupSlots;
use degenbot::degenbot_rpc::provider::AlloyProvider;
use degenbot::degenbot_simulation::sim::evm::{
    BlockSimHandle, SimulationOverrideParams, WarmCodeCacheInner,
};
use revm::database_interface::DatabaseRef;

/// The path-11354 V2 pair (USDT → stETH, exact-out).
const PAIR: &str = "0x648Ef94C6D205016A385Fb4C54aB6e422F5142c5";
/// The solve block for the recorded failure.
const SOLVE_BLOCK: u64 = 25_678_283;
/// Block timestamp of `25678283` (threaded through the pump header).
const BLOCK_TIMESTAMP: u64 = 1_785_807_179;
/// On-chain packed reserve slot 8 value (`reserve0` low-112, `reserve1`
/// next-112, `blockTimestampLast` top-32) at `25678283`.
const ONCHAIN_RESERVE0: u128 = 1_286_682_034_390_464; // stETH
const ONCHAIN_RESERVE1: u128 = 2_291_438; // USDT
/// The bot's exact-in amount into the V2 pair (USDT excess deposited).
const V2_AMOUNT_IN: u128 = 27415;
/// The no-fee / fee-30 multiplier the executor uses (`10000 - fee`).
const FEE_MULTIPLIER: u128 = 9970;
/// The solver's (and on-chain executor's) predicted V2 output.
const PREDICTED_OUT: u128 = 15_166_900_278_115;
/// The logged sim `actual_out` (1 wei short — the phantom-reserve signature).
const LOGGED_ACTUAL_OUT: u128 = 15_166_900_278_114;

/// The exact phantom `reserve0` that reproduces the logged `…114`.
#[expect(dead_code)]
const PHANTOM_RESERVE0: u128 = 1_286_682_034_390_401;

/// The executor `_v2_get_amount_out`: `floor(amt*fm*reserve_out/(reserve_in*10000+amt*fm))`.
#[allow(clippy::cast_possible_truncation)]
fn v2_get_amount_out(amount_in: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    (amount_in * FEE_MULTIPLIER * reserve_out) / (reserve_in * 10_000 + amount_in * FEE_MULTIPLIER)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let http = std::env::var("DEGENBOT_RPC_HTTP_CHAINID_1")
        .or_else(|_| std::env::var("CHAIN_1_HTTP"))
        .unwrap_or_else(|_| "http://host.containers.internal:8545".to_string());
    // Optional phantom-injection mode: a non-on-chain reserve to write into the
    // pair's slot8, reproducing the stale state the live sim ran against.
    let phantom: Option<u128> = std::env::var("DEGENBOT_PROBE_PHANTOM_RESERVE")
        .ok()
        .map(|v| v.parse().expect("phantom reserve must be a u128"));

    let provider = AlloyProvider::new(&http, 5).await?;
    let pair: Address = PAIR.parse()?;

    // Build the PRODUCTION layered-DB EVM at the solve block (empty `BotState`
    // — `BotStateDb` is a pass-through, so a faithful sim reads live RPC; the
    // state overrides here only fund a dummy owner + touch WETH/PM warmup
    // slots, none of which touch the V2 pair).
    let bot_state = BotState::new();
    let warm_cache = WarmCodeCacheInner::shared_default();
    let overrides = SimulationOverrideParams {
        owner: Address::repeat_byte(0xa0),
        inject_code: false,
        injected_address: None,
        runtime_bytecode: Bytes::new(),
        warmup: WarmupSlots {
            weth_balance: U256::ZERO,
            erc6909_weth: U256::ZERO,
            erc6909_native: U256::ZERO,
        },
        weth_address: Address::repeat_byte(0xc0),
        pool_manager_address: Address::repeat_byte(0xc1),
    };
    let mut handle = BlockSimHandle::build(
        &provider,
        0, // base_fee_next — irrelevant to a storage read
        SOLVE_BLOCK,
        BLOCK_TIMESTAMP,
        &overrides,
        &bot_state,
        &warm_cache,
    )
    .expect("BlockSimHandle build");

    let cache_db = &mut handle.evm_mut().ctx.journaled_state.database;

    // Optionally inject the phantom reserve BEFORE the read, simulating the
    // stale sim state (exactly what the live run must have had).
    if let Some(p) = phantom {
        // Pack the phantom reserve0 into the low 112 bits, preserving the real
        // reserve1 (USDT) in the next 112 — i.e. swap in ONLY the stale stETH
        // reserve, mimicking a stale stETH tick/balance snapshot.
        let packed = (U256::from(ONCHAIN_RESERVE1) << U256::from(112)) | U256::from(p);
        cache_db
            .insert_account_storage(pair, U256::from(8), packed)
            .expect("phantom injection into pair slot8");
        println!("[probe] injected phantom reserve0 = {p} into pair slot8 (reserve1 preserved)");
    }

    // Read the pair's reserve slot the way the sim's swap execution reads it.
    let slot8: U256 = cache_db.storage_ref(pair, U256::from(8))?;
    let mask112 = (U256::from(1_u128) << U256::from(112)) - U256::from(1_u128);
    let reserve0 = (slot8 & mask112).to::<u128>(); // low 112 = reserve0 (stETH)
    let reserve1 = ((slot8 >> U256::from(112)) & mask112).to::<u128>(); // next 112 = reserve1 (USDT)

    println!("[probe] block={SOLVE_BLOCK} pair={PAIR}");
    println!("[probe] sim-read  slot8 low112 reserve0(stETH) = {reserve0}");
    println!("[probe] sim-read  slot8 next112 reserve1(USDT) = {reserve1}");

    // The V2 output the sim's pair would produce from the observed reserve.
    let out = v2_get_amount_out(V2_AMOUNT_IN, reserve1, reserve0);
    println!("[probe] V2_SWAP_CALC(amount_in={V2_AMOUNT_IN}, reserve_in={reserve1}, reserve_out={reserve0}) = {out}");
    println!("[probe] predicted (solver/on-chain) = {PREDICTED_OUT}");
    println!("[probe] logged sim actual            = {LOGGED_ACTUAL_OUT}");

    let faithful = reserve0 == ONCHAIN_RESERVE0 && reserve1 == ONCHAIN_RESERVE1;
    match (phantom, faithful, out) {
        // Faithful, no injection → must yield …115.
        (None, true, PREDICTED_OUT) => {
            println!(
                "[probe] RESULT FAITHFUL: sim reads on-chain reserve0 {reserve0} and a V2 \
                 swap produces {PREDICTED_OUT}. The logged …114 is NOT reproducible from \
                 correct state — sim-state artifact confirmed."
            );
            std::process::exit(0);
        }
        // Phantom injected → must reproduce the logged …114.
        (Some(p), false, LOGGED_ACTUAL_OUT) => {
            println!(
                "[probe] RESULT PHANTOM-REPRODUCED: injected reserve0 {p} (≠ on-chain) makes \
                 the sim produce the logged …114 exactly. This is the stale-state mechanism \
                 behind the live path-11354 1-wei shortfall."
            );
            std::process::exit(1);
        }
        // Faithful mode but the sim read a non-on-chain reserve (no injection) —
        // the DIVERGENCE trap firing: the sim is seeing a stale/phantom state.
        (None, false, _) => {
            println!("[probe] RESULT DIVERGENCE: sim (un-injected) read reserve0={reserve0} ");
            println!(
                "[probe]         (≠ on-chain {ONCHAIN_RESERVE0}) at block {SOLVE_BLOCK} → stale "
            );
            println!("[probe]         V2 state caught in the act — the phantom reserve.");
            std::process::exit(2);
        }
        // Faithful but the read produced something other than …115 — the trap.
        (None, true, other) => {
            println!(
                "[probe] RESULT DIVERGENCE: on-chain reserve read correctly ({reserve0}/{reserve1}) \
                 but V2 output {other} ≠ predicted {PREDICTED_OUT}."
            );
            std::process::exit(2);
        }
        // Injection set but the value didn't reproduce …114 — unexpected.
        (Some(p), _, other) => {
            println!(
                "[probe] RESULT UNEXPECTED: injection mode with phantom {p} produced {other}, \
                 expected the logged {LOGGED_ACTUAL_OUT}. No divergence signature."
            );
            std::process::exit(3);
        }
    }
}
