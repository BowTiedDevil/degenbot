#![expect(clippy::expect_used)]

//! The caller-owned EARLY projection, exercised from an EXTERNAL crate.
//!
//! `project_early_admission` is the pre-admitted transport's one public
//! construction and the caller-owned half of the pre-simulation policy: it
//! applies the non-stamping suppression read, divergence, fee-on-transfer, the
//! caller's thin floor, the caller's input-order cap, and the PRE-SELECTION
//! staleness gate, then reports every drop beside the row it dropped. This
//! target is an external consumer, so a name, accessor, or re-export that stops
//! resolving is a compile error here rather than a silent break for a
//! `cargo add degenbot-arbitrage` caller.

use std::sync::{Arc, Mutex};

use alloy::primitives::aliases::U112;
use alloy::primitives::{address, Address, B256};

use degenbot_arbitrage::{
    project_early_admission, AdmittedCandidate, DispatchCandidate, EarlyAdmissionPolicy, EarlyDrop,
    EarlyDropReason, EarlyProjection, FeeOnTransferRegistry, PoolDivergence, PoolDivergenceKey,
    SolveStep, MAX_SIMULATE_CONCURRENT,
};
use degenbot_core::fee::FeeRatio;
use degenbot_execution::{CandidateId, DeploymentId, HopInfo, PathInfo, V2HopInfo};
use degenbot_pools::{DexVariant, RegisterV2PoolParams};
use degenbot_submission::{PathSuppression, PATH_SUPPRESS_RETRY_INTERVAL, PATH_SUPPRESS_THRESHOLD};
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const OTHER: Address = address!("1111111111111111111111111111111111111111");

/// The fixture pools — one flaggable, one clean.
const POOL_A: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const POOL_B: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

/// The retry clock these tests run at: past the retry interval, so a benched
/// candidate is due.
const DUE_BLOCK: u64 = 1_000;

fn deployment_id() -> DeploymentId {
    DeploymentId::new(1)
}

fn candidate_id(path_id: u64) -> CandidateId {
    CandidateId::new(path_id, deployment_id())
}

fn step(state_nonce: u64) -> SolveStep {
    SolveStep {
        output: 1_100_000_000_000_000_000u128,
        consumed_input: 1_000_000_000_000_000_000u128,
        state_nonce,
    }
}

/// One V2 hop from `token_in` to `token_out` through `pool` — the fixtures vary
/// the pool and the input token independently, so a stage's drop can be
/// attributed to that stage alone.
fn single_v2_hop_over(pool: Address, token_in: Address, token_out: Address) -> PathInfo {
    PathInfo::new(vec![HopInfo::V2(V2HopInfo {
        pool_address: pool,
        token0_address: token_in,
        token1_address: token_out,
        fee: 30,
        zfo: true,
    })])
}

fn two_v2_hops() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: POOL_A,
            token0_address: WETH,
            token1_address: OTHER,
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: POOL_B,
            token0_address: OTHER,
            token1_address: WETH,
            fee: 30,
            zfo: true,
        }),
    ])
}

/// A two-hop candidate through both fixture pools.
fn candidate(path_id: u64, opt_input: u128, profit: u128) -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: candidate_id(path_id),
        optimal_input: opt_input,
        engine_profit: profit,
        steps: Box::new([step(0), step(0)]),
        solve_block: 100,
        path_info: two_v2_hops(),
        prepared: None,
    }
}

/// One one-hop candidate through `pool` from `token_in`, with its own margin
/// (`optimal_input` = `1_000_000`, so `profit` reads as basis points).
fn projected_candidate(
    path_id: u64,
    pool: Address,
    token_in: Address,
    profit: u128,
) -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: candidate_id(path_id),
        optimal_input: 1_000_000,
        engine_profit: profit,
        steps: Box::new([step(0)]),
        solve_block: 100,
        path_info: single_v2_hop_over(pool, token_in, OTHER),
        prepared: None,
    }
}

/// A one-hop candidate routing through `pool`, carrying `state_nonce` on its
/// single step (the staleness gate's snapshot).
fn candidate_through_pool(path_id: u64, pool: Address, state_nonce: u64) -> DispatchCandidate {
    DispatchCandidate {
        candidate_id: candidate_id(path_id),
        optimal_input: 1_000_000_000_000_000_000u128,
        engine_profit: 1_000,
        steps: Box::new([step(state_nonce)]),
        solve_block: 100,
        path_info: single_v2_hop_over(pool, WETH, OTHER),
        prepared: None,
    }
}

/// The three policy registries, fresh per test.
struct Registries {
    suppression: Arc<Mutex<PathSuppression>>,
    divergence: Arc<Mutex<PoolDivergence>>,
    fee_on_transfer: Arc<Mutex<FeeOnTransferRegistry>>,
}

fn registries() -> Registries {
    Registries {
        suppression: Arc::new(Mutex::new(PathSuppression::new())),
        divergence: Arc::new(Mutex::new(PoolDivergence::new())),
        fee_on_transfer: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
    }
}

/// Bench `path_id` in the suppression ledger: `PATH_SUPPRESS_THRESHOLD`
/// consecutive failures.
fn bench(ledger: &Registries, path_id: u64) {
    let mut suppression = ledger.suppression.lock().expect("suppression ledger");
    for _ in 0..PATH_SUPPRESS_THRESHOLD {
        suppression.record_failure(candidate_id(path_id));
    }
    assert!(
        suppression.is_in_suppressed_set(candidate_id(path_id)),
        "fixture benches the candidate"
    );
}

/// Poison `registry`: a std `Mutex` takes poison only when a panic unwinds while
/// its guard is live.
fn poison_under_guard<T>(registry: &Arc<Mutex<T>>) {
    let handle = Arc::clone(registry);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = handle.lock().expect("registry lock");
        // The panic source is a genuinely fallible parse: an unwrapped `None`
        // literal is the `unnecessary_literal_unwrap` lint, and the workspace
        // denies `panic!`.
        let _: u8 = "not a number".parse().expect("the deliberate panic source");
    }));
    assert!(registry.is_poisoned(), "fixture poisons the registry");
}

/// A `BotState` with each pool in `pools` registered as a V2 pair.
fn bot_state_with(pools: &[Address]) -> BotState {
    let mut core = BotState::new();
    for pool in pools {
        core.register_v2_pool(&RegisterV2PoolParams {
            address: *pool,
            token0: WETH,
            token1: OTHER,
            reserve0: U112::from(1_000_000u64),
            reserve1: U112::from(2_000_000u64),
            fee_token0: FeeRatio::new(997, 1_000).expect("valid fee"),
            fee_token1: FeeRatio::new(997, 1_000).expect("valid fee"),
            factory: Address::ZERO,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
            update_block: 1,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
        })
        .expect("the fixture pool registers once");
    }
    core
}

/// The live `state_nonce` of `pool` — the value a fresh candidate snapshots.
fn live_nonce(core: &BotState, pool: Address) -> u64 {
    let pid = core.pool_id_by_address(&pool).expect("the fixture pool");
    core.pool_state_nonce(pid)
}

/// One early projection, by the caller's rules: the suppression ledger is
/// locked by the CALLER (the projection can only read it), the cap is the
/// caller's own count, and `state` is the pre-selection staleness gate's sample
/// source.
fn project_with_state(
    ledger: &Registries,
    state: Option<&StateLock<BotState>>,
    block: u64,
    min_profit_margin_bps: u64,
    max_candidates: usize,
    candidates: Vec<DispatchCandidate>,
) -> EarlyProjection {
    let suppression = ledger.suppression.lock().expect("suppression ledger");
    project_early_admission(
        candidates,
        &EarlyAdmissionPolicy {
            suppression: &suppression,
            divergence: &ledger.divergence,
            fee_on_transfer: &ledger.fee_on_transfer,
            bot_state: state,
            current_block: block,
            min_profit_margin_bps,
            max_candidates,
        },
    )
}

/// The same projection with no state owner: the staleness gate is skipped.
fn project(
    ledger: &Registries,
    block: u64,
    min_profit_margin_bps: u64,
    max_candidates: usize,
    candidates: Vec<DispatchCandidate>,
) -> EarlyProjection {
    project_with_state(
        ledger,
        None,
        block,
        min_profit_margin_bps,
        max_candidates,
        candidates,
    )
}

/// The projection's survivor path ids, in transported order.
fn projected_ids(projection: &EarlyProjection) -> Vec<u64> {
    projection
        .admitted()
        .iter()
        .map(|admitted| admitted.candidate().candidate_id.path_id())
        .collect()
}

/// The projection's reported reasons, in report order.
fn projected_reasons(projection: &EarlyProjection) -> Vec<EarlyDropReason> {
    projection.drops().iter().map(EarlyDrop::reason).collect()
}

/// The projection's reported drop path ids, in report order.
fn dropped_ids(projection: &EarlyProjection) -> Vec<u64> {
    projection
        .drops()
        .iter()
        .map(|drop| drop.candidate().candidate_id.path_id())
        .collect()
}

/// The transported set, as the caller hands it to the pre-admitted entry.
fn transport(projection: EarlyProjection) -> Vec<AdmittedCandidate> {
    projection.into_admitted()
}

// -- stage order and provenance ---------------------------------------------

/// Stage order and drop provenance: one row per stage drops against its OWN
/// row, in pipeline order, and only the clean row is transported.
#[test]
fn the_projection_reports_each_stage_in_pipeline_order() {
    let ledger = registries();
    bench(&ledger, 1);
    ledger
        .divergence
        .lock()
        .expect("divergence memo")
        .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
    {
        let mut registry = ledger.fee_on_transfer.lock().expect("fot registry");
        registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
        registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
    }
    let rows = vec![
        projected_candidate(1, POOL_B, OTHER, 100_000), // benched, not due
        projected_candidate(2, POOL_A, OTHER, 100_000), // flagged pool
        projected_candidate(3, POOL_B, WETH, 100_000),  // confirmed token
        projected_candidate(4, POOL_B, OTHER, 100),     // 1 bps at a 50 bps floor
        projected_candidate(5, POOL_B, OTHER, 100_000), // clean
    ];
    // One block inside the retry interval (the ledger's sentinel retry is 0).
    let projection = project(&ledger, PATH_SUPPRESS_RETRY_INTERVAL - 1, 50, 50, rows);

    assert_eq!(
        projected_reasons(&projection),
        vec![
            EarlyDropReason::Suppressed,
            EarlyDropReason::DivergentPool,
            EarlyDropReason::FeeOnTransfer,
            EarlyDropReason::ThinMargin,
        ],
        "suppression, divergence, fee-on-transfer, thin margin — in that order"
    );
    assert_eq!(
        dropped_ids(&projection),
        vec![1, 2, 3, 4],
        "each report carries the row its own stage dropped"
    );
    assert_eq!(projected_ids(&projection), vec![5]);
    assert_eq!(
        ledger
            .divergence
            .lock()
            .expect("divergence memo")
            .total_divergent_dropped(),
        1,
        "the early projection feeds the same lifetime tally the leaf does"
    );
    assert_eq!(
        ledger
            .fee_on_transfer
            .lock()
            .expect("fot registry")
            .total_fot_dropped(),
        1
    );
}

/// The suppression read is the NON-STAMPING projection: a retry-due candidate
/// is admitted here and its retry stays unspent for the leaf.
#[test]
fn the_projection_admits_a_retry_due_candidate_without_spending_its_retry() {
    let ledger = registries();
    bench(&ledger, 9);

    let projection = project(
        &ledger,
        DUE_BLOCK,
        0,
        50,
        vec![projected_candidate(9, POOL_B, OTHER, 100_000)],
    );
    assert_eq!(projected_ids(&projection), vec![9]);
    assert!(projection.drops().is_empty());

    // One block later the projection of the ledger (no stamp, no retry spent)
    // still admits: the projection wrote no `last_retry_block`.
    let mut ledger_guard = ledger.suppression.lock().expect("suppression ledger");
    assert!(
        !ledger_guard.is_suppressed_without_stamp(candidate_id(9), DUE_BLOCK + 1),
        "the early read spent no retry"
    );
    // Contrast: the STAMPING read does spend it, leaving the same block
    // suppressed one block later.
    assert!(!ledger_guard.is_suppressed(candidate_id(9), DUE_BLOCK));
    assert!(
        ledger_guard.is_suppressed_without_stamp(candidate_id(9), DUE_BLOCK + 1),
        "the leaf's stamp is what consumes the interval"
    );
}

// -- the thin floor and the identity-keyed attribution -----------------------

/// The thin-margin duplicate quirk, preserved exactly: a thin row whose
/// identity a SURVIVING twin carries is dropped but earns no report, because
/// the identity-keyed attribution that emits those records never attributed a
/// drop whose identity survived.
#[test]
fn a_thin_row_whose_twin_survives_is_dropped_without_a_report() {
    let ledger = registries();
    let projection = project(
        &ledger,
        100,
        50,
        50,
        vec![
            projected_candidate(7, POOL_B, OTHER, 1),      // 0 bps
            projected_candidate(7, POOL_B, OTHER, 10_000), // 100 bps
        ],
    );

    assert_eq!(projected_ids(&projection), vec![7]);
    assert_eq!(
        projection.admitted()[0].candidate().engine_profit,
        10_000,
        "the fat twin is what rides"
    );
    assert!(
        projection.drops().is_empty(),
        "the thin twin shares a kept identity, so it earns no report"
    );
}

/// Two thin rows on ONE identity are two reports — each row is judged on its
/// own margin, so no survivor hides either drop.
#[test]
fn two_thin_duplicates_of_one_identity_are_two_reports() {
    let ledger = registries();
    let projection = project(
        &ledger,
        100,
        50,
        50,
        vec![
            projected_candidate(7, POOL_B, OTHER, 1),
            projected_candidate(7, POOL_B, OTHER, 2),
        ],
    );

    assert!(projection.admitted().is_empty());
    assert_eq!(
        projected_reasons(&projection),
        vec![EarlyDropReason::ThinMargin, EarlyDropReason::ThinMargin]
    );
    assert_eq!(dropped_ids(&projection), vec![7, 7]);
}

// -- the caller's cap -------------------------------------------------------

/// The cap is the caller's count clamped to the fan-out ceiling and applied
/// LAST, in INPUT order, reporting the tail it dropped.
#[test]
fn the_projection_caps_in_input_order_and_reports_the_tail() {
    let ledger = registries();
    let ceiling = u64::try_from(MAX_SIMULATE_CONCURRENT).expect("the cap fits a u64");
    let over = ceiling + 10;
    let rows: Vec<DispatchCandidate> = (1..=over)
        .map(|path_id| projected_candidate(path_id, POOL_B, OTHER, 100_000))
        .collect();

    // A caller count ABOVE the ceiling still clamps to it.
    let projection = project(&ledger, 100, 0, MAX_SIMULATE_CONCURRENT + 100, rows);
    assert_eq!(
        projected_ids(&projection),
        (1..=ceiling).collect::<Vec<u64>>(),
        "the cap keeps the input prefix"
    );
    assert_eq!(
        projected_reasons(&projection),
        vec![EarlyDropReason::OverCap; 10]
    );
    assert_eq!(
        dropped_ids(&projection),
        (ceiling + 1..=over).collect::<Vec<u64>>(),
        "the reported tail is the input-order tail"
    );
    assert_eq!(
        ledger
            .divergence
            .lock()
            .expect("divergence memo")
            .total_divergent_dropped(),
        0,
        "a capped drop is not a divergence drop"
    );
}

// -- the transport ----------------------------------------------------------

/// The survivors ARE the pre-admitted transport: the complete candidate rides,
/// so the leaf needs no ID-keyed side table.
#[test]
fn the_projection_transports_the_complete_candidate() {
    let ledger = registries();
    let projection = project(&ledger, DUE_BLOCK, 0, 50, vec![candidate(7, 1_234, 5_678)]);

    assert_eq!(projection.admitted().len(), 1);
    let carried = projection.admitted()[0].candidate();
    assert_eq!(carried.candidate_id, candidate_id(7));
    assert_eq!(carried.optimal_input, 1_234);
    assert_eq!(carried.engine_profit, 5_678);
    assert_eq!(carried.solve_block, 100);
    assert_eq!(carried.steps.len(), 2);
    assert_eq!(carried.path_info.hops.len(), 2);
    assert!(
        carried.prepared.is_none(),
        "the target-selection walk attaches the encoding, not the projection"
    );

    // The transported set is what the pre-admitted entry is handed, and the
    // accessor still reads the same candidate through it.
    let admitted = transport(projection);
    assert_eq!(admitted.len(), 1);
    let carried = admitted[0].candidate();
    assert_eq!(carried.candidate_id, candidate_id(7));
    assert_eq!(carried.optimal_input, 1_234);
    assert_eq!(carried.expected_outputs_vec().len(), 2);
}

// -- the PRE-SELECTION staleness gate --------------------------------------

/// The PRE-SELECTION staleness gate is part of the shared projection: a row
/// whose hop nonce turned over is reported as `Stale` and never transported,
/// while its fresh sibling rides.
#[test]
fn the_projection_drops_a_stale_row_at_the_pre_selection_position() {
    let ledger = registries();
    let core = bot_state_with(&[POOL_A, POOL_B]);
    let fresh_a = live_nonce(&core, POOL_A);
    let fresh_b = live_nonce(&core, POOL_B);
    let state = Arc::new(StateLock::new(core));

    let projection = project_with_state(
        &ledger,
        Some(&state),
        100,
        0,
        50,
        vec![
            candidate_through_pool(1, POOL_A, fresh_a + 1), // advanced
            candidate_through_pool(2, POOL_B, fresh_b),     // current
        ],
    );

    assert_eq!(projected_ids(&projection), vec![2]);
    assert_eq!(projected_reasons(&projection), vec![EarlyDropReason::Stale]);
    assert_eq!(
        dropped_ids(&projection),
        vec![1],
        "the report carries the row the gate dropped"
    );
}

/// The caller's cap runs BEFORE the gate, in input order: with cap 1 over
/// `[stale, fresh]` the STALE row keeps the slot, the fresh row is the cap's own
/// drop, and the gate then empties the transport — a stale row frees no slot for
/// the sibling it starved.
#[test]
fn the_projection_caps_before_it_gates_staleness() {
    let ledger = registries();
    let core = bot_state_with(&[POOL_A, POOL_B]);
    let fresh_a = live_nonce(&core, POOL_A);
    let fresh_b = live_nonce(&core, POOL_B);
    let state = Arc::new(StateLock::new(core));

    let projection = project_with_state(
        &ledger,
        Some(&state),
        100,
        0,
        1,
        vec![
            candidate_through_pool(1, POOL_A, fresh_a + 1), // stale, keeps the slot
            candidate_through_pool(2, POOL_B, fresh_b),     // fresh, past the cap
        ],
    );

    assert!(projection.admitted().is_empty(), "nobody rides");
    assert_eq!(
        projected_reasons(&projection),
        vec![EarlyDropReason::OverCap, EarlyDropReason::Stale],
        "the cap reports its input-order tail first, the gate runs after it"
    );
    assert_eq!(
        dropped_ids(&projection),
        vec![2, 1],
        "the capped sibling is reported before the stale row"
    );
}

/// No state owner, no gate: the projection transports the stale row. This is
/// the UNGATED control — it proves the stale evidence has exactly one
/// pre-selection home, so a caller that forgets the state owner gets the
/// ungated behavior rather than a silent second gate.
#[test]
fn the_projection_without_a_state_owner_transports_a_stale_row() {
    let ledger = registries();
    let core = bot_state_with(&[POOL_A]);
    let stale_nonce = live_nonce(&core, POOL_A) + 1;

    let projection = project(
        &ledger,
        100,
        0,
        50,
        vec![candidate_through_pool(1, POOL_A, stale_nonce)],
    );

    assert_eq!(
        projected_ids(&projection),
        vec![1],
        "no owner means no pre-selection gate"
    );
    assert!(projection.drops().is_empty());

    // The same row IS stale, and the projection says so the moment it has the
    // owner: the gate a caller skips is the one it runs.
    let state = Arc::new(StateLock::new(core));
    let gated = project_with_state(
        &ledger,
        Some(&state),
        100,
        0,
        50,
        vec![candidate_through_pool(1, POOL_A, stale_nonce)],
    );
    assert!(gated.admitted().is_empty());
    assert_eq!(projected_reasons(&gated), vec![EarlyDropReason::Stale]);
}

// -- the poisoned-registry recovery ----------------------------------------

/// The projection READS a poisoned registry (the caller's frame keeps
/// simulating) rather than treating it as "nothing flagged". The authoritative
/// passes keep their loud failure on the same state — their panic unit tests
/// live beside the passes.
#[test]
fn the_projection_recovers_poisoned_registries() {
    let ledger = registries();
    ledger
        .divergence
        .lock()
        .expect("divergence memo")
        .record_divergence(PoolDivergenceKey::V2(POOL_A), 100);
    {
        let mut registry = ledger.fee_on_transfer.lock().expect("fot registry");
        registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_A), 100);
        registry.record_suspicion(WETH, PoolDivergenceKey::V2(POOL_B), 100);
    }
    poison_under_guard(&ledger.divergence);
    poison_under_guard(&ledger.fee_on_transfer);

    let projection = project(
        &ledger,
        100,
        0,
        50,
        vec![
            projected_candidate(1, POOL_A, OTHER, 100_000),
            projected_candidate(2, POOL_B, WETH, 100_000),
            projected_candidate(3, POOL_B, OTHER, 100_000),
        ],
    );
    assert_eq!(
        projected_reasons(&projection),
        vec![
            EarlyDropReason::DivergentPool,
            EarlyDropReason::FeeOnTransfer
        ],
        "the poisoned memos are still READ, not treated as empty"
    );
    assert_eq!(projected_ids(&projection), vec![3]);
}
