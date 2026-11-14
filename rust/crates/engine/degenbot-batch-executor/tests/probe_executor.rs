#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "the fixture drives known-valid values and asserts on the typed product"
)]

//! The core probe executor over the REAL stages.
//!
//! These suites drive the ordinary assembly, production encoding, and EVM2
//! simulation through [`ProbeExecutor`] over the crate's offline RPC transport —
//! so the reuse is real evidence, not a claim about a stub:
//!
//! * the probe configuration names NO submit-side handle at all (no signer, no
//!   nonce lane, no relay fan-out, no dispatcher, no receipt probe — the
//!   [`SimRuntime`] it is built from has no such field), so non-submission is a
//!   missing field rather than a dry-run flag or a refused enqueue;
//! * the caller's own amount vector produces the fixture's PROFITABLE receipt
//!   (the injected build's `execute` stub), the same dollar figure the
//!   production lane reports for the same vector;
//! * the production encoder's joined row comes back as planning evidence
//!   (`execute_calldata`, target, pool set) with `submit` unset on every record,
//!   because the probe type has no submit stage to fill it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::{address, Address, Bytes, U256};

use degenbot_arbitrage::{
    CmdExecutorBinding, FeeOnTransferRegistry, PoolDivergence, SimulationDeployment,
    TargetSelection,
};
use degenbot_batch_executor::{
    derive_path_pools, AssemblyVerdict, BatchWork, ExecutorConfig, PathResolver, ProbeCapital,
    ProbeConfig, ProbeExecutor, RawResult, SimRuntime, SimulateVerdict,
};
use degenbot_execution::{CandidateId, HopInfo, PathInfo, V2HopInfo};
use degenbot_executor::composers::EncodeOptions;
use degenbot_submission::PathSuppression;
use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::BotState;

mod support;

use support::mock_provider;

/// The operator account every simulated transaction is sent from (the
/// binding's owner — a probe has no key, so this is the only authority it can
/// name).
const ANVIL_ADDRESS: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

/// The built-in settlement executor's address (the declared build's target).
const EXECUTOR: Address = address!("0x543c7ef4f2368a9411c94a055e7236e6dc6f99d5");

const WETH: Address = address!("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const POOL_MANAGER: Address = address!("0x000000000004444c5dc75cb358380d2e3de08a90");

const PATH_ID: u64 = 7;
const BLOCK: u64 = 100;
const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000;
const HOP: u128 = OPTIMAL_INPUT;

/// The declared build's receipt gross (the fixture's `execute` stub returns the
/// two receipt words — ADR-098 D7).
const BUILTIN_GROSS: U256 = U256::from_limbs([3_000_000_000_000_000, 0, 0, 0]);

/// A WETH → token → WETH round trip: the built-in command grammar encodes this
/// shape.
fn v2_path() -> PathInfo {
    PathInfo::new(vec![
        HopInfo::V2(V2HopInfo {
            pool_address: address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token0_address: WETH,
            token1_address: address!("0x3333333333333333333333333333333333333333"),
            fee: 30,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: address!("0xcccccccccccccccccccccccccccccccccccccccc"),
            token0_address: address!("0x3333333333333333333333333333333333333333"),
            token1_address: WETH,
            fee: 30,
            zfo: true,
        }),
    ])
}

/// The declared `execute` stub: return `(gross_profit, 0)` as two ABI words.
fn receipt_stub(gross: U256) -> Bytes {
    let mut code = Vec::with_capacity(76);
    code.push(0x7f); // PUSH32 gross_profit
    code.extend_from_slice(&gross.to_be_bytes::<32>());
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0x00 MSTORE
    code.push(0x7f); // PUSH32 bribe_paid
    code.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
    code.extend_from_slice(&[0x60, 0x20, 0x52]); // PUSH1 0x20 MSTORE
    code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xf3]); // PUSH1 0x40 PUSH1 0x00 RETURN
    Bytes::from(code)
}

struct MapResolver(HashMap<u64, PathInfo>);

impl PathResolver for MapResolver {
    fn resolve(&self, path_id: u64) -> Option<PathInfo> {
        self.0.get(&path_id).cloned()
    }
}

/// The caller's amount vector as work: ONE raw row, no payload rows (a probe
/// has no engine-side inline simulation to serve).
fn work(block: u64) -> BatchWork {
    BatchWork {
        rows: vec![RawResult {
            path_id: PATH_ID,
            optimal_input: OPTIMAL_INPUT,
            profit: 1,
            hop_outputs: vec![HOP, HOP],
            consumed_inputs: vec![HOP, HOP],
            solve_block: block,
            state_nonces: vec![0, 0],
        }],
        payloads: Vec::new(),
        current_block: block,
        base_fee_next: 1_000_000_000,
        block_timestamp: 0,
        block_priority_fees: None,
    }
}

/// The probe executor over the offline transport, built from a [`SimRuntime`]
/// that carries NO submit-side handle: there is no signer to sign with, no nonce
/// lane to lease, no relay list to fan out, and no dispatcher to claim pools.
///
/// The fixture has no way to construct a submission-capable executor from these
/// values, which is exactly the property under test: non-submission here is a
/// MISSING field, not a policy flag, a dry-run stance, or an entry that refuses.
async fn probe_executor() -> ProbeExecutor {
    probe_executor_with(ProbeCapital::Production).await
}

/// The same probe executor at an explicit capital posture: a probe is built per
/// posture, and a probe posture composes a real capital plan for the paths it
/// simulates.
async fn probe_executor_with(capital: ProbeCapital) -> ProbeExecutor {
    probe_executor_over(capital, HashMap::from([(PATH_ID, v2_path())])).await
}

/// The same probe executor over an explicit resolver projection: a probe whose
/// rows this engine cannot resolve is a shape the capital pre-flight must SKIP,
/// so the fixture needs a resolver that knows fewer paths than the work names.
async fn probe_executor_over(
    capital: ProbeCapital,
    paths: HashMap<u64, PathInfo>,
) -> ProbeExecutor {
    let mut verdict = degenbot_config::BotConfig::default();
    // The built-in target runs a DECLARED build in this fixture: its `execute`
    // stub returns a receipt, so the probe's outcome is the real sim's own
    // assessment rather than an absent-receipt refusal.
    verdict.simulation.inject_executor_code = true;
    let runtime = SimRuntime {
        max_candidates: 50,
        use_v4_batch: false,
        probe_capital: capital,
        resolver: Arc::new(MapResolver(paths)),
        suppression: Arc::new(Mutex::new(PathSuppression::new())),
        divergence: Arc::new(Mutex::new(PoolDivergence::new())),
        fot: Arc::new(Mutex::new(FeeOnTransferRegistry::new())),
        provider: Arc::new(mock_provider()),
        binding: CmdExecutorBinding::new(
            ANVIL_ADDRESS,
            EXECUTOR,
            WETH,
            POOL_MANAGER,
            EncodeOptions::default(),
        ),
        additional_deployments: Vec::<Arc<SimulationDeployment>>::new(),
        target_selection: TargetSelection::AllTargets,
        injected_address: Some(EXECUTOR),
        runtime_bytecode: receipt_stub(BUILTIN_GROSS),
        bot_state: Some(Arc::new(StateLock::new(BotState::new()))),
        warm_cache: Some(degenbot_simulation::WarmCodeCacheInner::shared_default()),
    };
    ProbeExecutor::new(
        ProbeConfig::from_verdict(&verdict, runtime)
            .expect("the fixture's one deployment composes a valid registry"),
    )
    .await
    .expect("this fixture's cache carries no BAL opt-in, so no capability probe can refuse")
}

/// The ordinary stages answer a probe with the fixture's PROFITABLE receipt, and
/// the call carries no submit verdict anywhere: the executor was built with no
/// submit-side handle at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_probe_returns_the_real_profitable_simulation_without_a_submit_lane() {
    let executor = probe_executor().await;
    let outcome = executor.probe(work(BLOCK)).await.expect("the probe ran");

    assert_eq!(outcome.records.len(), 1, "one raw row, one record");
    let record = &outcome.records[0];
    assert_eq!(
        record.candidate_id,
        CandidateId::new(PATH_ID, ExecutorConfig::BUILTIN_DEPLOYMENT_ID)
    );
    assert_eq!(record.assembly.label(), "assembled");
    let Some(SimulateVerdict::Profitable(receipt)) = &record.simulate else {
        panic!("the injected build's receipt is the real sim's own profit: {record:?}")
    };
    assert_eq!(receipt.gross_profit, BUILTIN_GROSS);
    assert!(
        receipt.net_profit <= receipt.gross_profit,
        "net is gross minus the simulated gas cost"
    );
    assert!(
        record.submit.is_none(),
        "the probe executor has no submit stage to fill the slot"
    );

    // The production encoder ran on the caller's own path: the same pool set the
    // path derivation names, and calldata targeting the deployment's contract.
    assert_eq!(outcome.encoded.len(), 1);
    let encoded = &outcome.encoded[0];
    assert_eq!(encoded.candidate_id, record.candidate_id);
    assert_eq!(encoded.execution_target, EXECUTOR);
    assert_eq!(encoded.path_pools, derive_path_pools(&v2_path().hops));
    assert!(
        encoded.execute_calldata.len() > 4,
        "the production grammar's selector plus arguments"
    );
    assert_eq!(encoded.gross_profit, BUILTIN_GROSS);
}

/// The one-path probe contract's typed refusal: a probe posture whose path
/// touches a token this build has no verified layout for comes back as
/// `ProbeError::CapitalPlan` naming the asset — its own value, not a stage
/// detail, and not a per-candidate failure. Nothing was simulated, so there is no
/// record to report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_probe_refuses_an_unverified_token_as_its_own_typed_planning_answer() {
    for capital in [ProbeCapital::ZeroBalance, ProbeCapital::SelfFund] {
        let executor = probe_executor_with(capital).await;
        let refusal = executor
            .probe(work(BLOCK))
            .await
            .expect_err("the fixture's lab token has no verified layout");
        assert_eq!(
            refusal,
            degenbot_batch_executor::ProbeError::CapitalPlan(
                degenbot_simulation::CapitalPlanError::UnverifiedLayout {
                    asset: degenbot_simulation::CapitalAsset::Erc20 {
                        token: address!("0x3333333333333333333333333333333333333333"),
                    },
                }
            ),
            "{capital:?}: the refusal is typed and names the asset"
        );
        assert!(
            refusal.to_string().contains("no verified storage layout"),
            "{capital:?}: the message states the gap: {refusal}"
        );
    }
}

/// A row the engine cannot RESOLVE, and an EMPTY batch, are never misreported as
/// a planning refusal: the typed capital pre-flight has nothing to compose, so it
/// skips and the ordinary stage answer survives.
///
/// This is the distinction the pre-flight exists to preserve — an unresolvable
/// path is the assembly's `SkipResolveMiss` answer (or, for a malformed row, its
/// own corruption refusal), never `CapitalPlanError::NoPaths`, which would
/// replace the real verdict with a planning one that names no asset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unresolvable_row_is_left_to_the_stage_answer_not_refused_as_no_paths() {
    // A probe posture (not production) over a resolver that knows no path at all.
    let executor = probe_executor_over(ProbeCapital::ZeroBalance, HashMap::new()).await;

    let outcome = executor
        .probe(work(BLOCK))
        .await
        .expect("the stage answers for the unresolvable row; the pre-flight refuses nothing");
    assert_eq!(
        outcome.records.len(),
        1,
        "the row keeps its assembly record"
    );
    assert_eq!(
        outcome.records[0].assembly,
        AssemblyVerdict::SkipResolveMiss,
        "the ordinary resolve-miss answer survives, not a NoPaths planning refusal"
    );

    // An empty batch composes no plan either — and reports no refusal.
    let mut empty = work(BLOCK);
    empty.rows.clear();
    let outcome = executor
        .probe(empty)
        .await
        .expect("an empty batch has no capital work to refuse");
    assert!(outcome.records.is_empty(), "nothing was assembled");
}

/// Two concurrent probes over the real stages each read their OWN record: the
/// correlation is per call, and a multi-threaded runtime interleaves them freely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_probes_over_the_real_stages_each_read_their_own_record() {
    let executor = probe_executor().await;
    let (first, second) =
        tokio::join!(executor.probe(work(BLOCK)), executor.probe(work(BLOCK + 1)));
    let first = first.expect("the first call's outcome");
    let second = second.expect("the second call's outcome");
    assert_eq!(first.records.len(), 1);
    assert_eq!(second.records.len(), 1);
    assert_eq!(first.records[0].block, BLOCK);
    assert_eq!(second.records[0].block, BLOCK + 1);
    for outcome in [&first, &second] {
        assert!(
            matches!(
                outcome.records[0].simulate,
                Some(SimulateVerdict::Profitable(_))
            ),
            "both calls ran the real sim to a profitable receipt"
        );
        assert!(outcome.records[0].submit.is_none());
        assert_eq!(outcome.records[0].candidate_id.path_id(), PATH_ID);
        assert_eq!(
            outcome.encoded.len(),
            1,
            "each call has its own encoded row"
        );
    }
}
