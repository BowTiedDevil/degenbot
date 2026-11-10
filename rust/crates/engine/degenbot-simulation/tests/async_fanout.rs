//! Public-seam tests for the bounded async candidate fan-out
//! (`degenbot_simulation::sim::evm2::async_fanout`).
//!
//! Two seams are exercised: one slot's own reuse contract (a slot carries a
//! candidate's writes, logs, transient storage, and override rows — and none of
//! them may survive into the next candidate installed over it), and the
//! scheduler's behaviour (a bounded active-slot set, results keyed by input
//! index under reversed completion, isolated refusals, and identity with the
//! indexed sequential execution of the same candidates).

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture or assertion that cannot proceed must fail loudly, not return a value"
)]

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use ::evm2::evm::r#async::{AsyncDatabase, AsyncDb};
use ::evm2::evm::{AccountInfo, DynDatabase, InMemoryDB};
use ::evm2::interpreter::Word;
use alloy::primitives::{address, keccak256, Address, Bytes, TxKind, B256, U256};
use alloy::providers::RootProvider;
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::json_rpc::{
    RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
};
use alloy::transports::{TransportError, TransportFut};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::async_fanout::{
    AsyncEvmFanout, AsyncFanoutSlot, CandidateStateSource, DrainPolicy, FanoutEngineConfig,
    FanoutError, FanoutLimits, FanoutOutcome, PreparedCandidate, ReadHubDb, ReadHubDbError,
};
use degenbot_simulation::sim::evm2::handle_block_env;
use degenbot_simulation::sim::evm2::read_hub::ReadHub;
use degenbot_simulation::sim::evm2::session::{
    Evm2CaptureMode, Evm2SimError, Evm2SimSequence, Evm2SimStep,
};
use degenbot_simulation::sim::{DeclaredCode, ReadAnchor, RunVerdict, SimulationOverrideParams};
use serde_json::value::to_raw_value;
use serde_json::{json, Value};

const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");
const OWNER_A: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const OWNER_B: Address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;
const SENDER_START_WEI: u64 = 1_000_000_000_000_000_000;

/// The slot both candidates' write steps touch.
const SLOT: u8 = 5;

/// The value candidate A's step stores at [`SLOT`].
const STORED_BY_A: u8 = 0x99;

/// The value slot A's database seeded at [`SLOT`] (the figure A's write
/// replaces in the accepted overlay).
const SEEDED_A: u8 = 0x2A;

/// The value slot B's database seeds at [`SLOT`] — B's read must report this,
/// never A's write and never A's database's value.
const SEEDED_B: u8 = 0x11;

/// A transient-storage key/value A's parked state leaves behind.
const TRANSIENT_KEY: U256 = U256::from_limbs([0, 0, 0, 0]);
const TRANSIENT_VALUE: u64 = 7;

/// The whole-ETH funding [`SimulationOverrideParams`] installs for an owner.
const OWNER_FUND_ETH: u64 = 100;

/// One slot's block env.
fn block_env() -> ::evm2::env::BlockEnvExt {
    handle_block_env(BASE_FEE_GWEI, BLOCK, TIMESTAMP)
}

/// A runtime that stores `value` at [`SLOT`] and stops — the write a candidate's
/// step leaves in whichever state layer accepted it.
fn store_runtime(value: u8) -> Bytes {
    Bytes::from(vec![0x60, value, 0x60, SLOT, 0x55, 0x00])
}

/// An envelope calling `to` with `data`.
fn call_tx(to: Address, data: &[u8]) -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(to),
            input: Bytes::copy_from_slice(data),
            gas_limit: 200_000,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// A funded sender with no other state.
fn funded_sender() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &SENDER,
        AccountInfo::default().with_balance(U256::from(SENDER_START_WEI)),
    );
    db
}

/// Candidate A's initial state: the sender funded, [`DUAL`] absent from the
/// account map, and [`SEEDED_A`] sitting at [`DUAL`]'s [`SLOT`].
fn candidate_a_db() -> InMemoryDB {
    let mut db = funded_sender();
    db.insert_account_storage(&DUAL, &Word::from(SLOT), &Word::from(SEEDED_A));
    db
}

/// Candidate B's initial state: the sender funded, [`SEEDED_B`] at [`DUAL`]'s
/// [`SLOT`], and no account entry for [`DUAL`] (so A's declared code override is
/// observable as absent).
fn candidate_b_db() -> InMemoryDB {
    let mut db = funded_sender();
    db.insert_account_storage(&DUAL, &Word::from(SLOT), &Word::from(SEEDED_B));
    db
}

/// Candidate A's overrides: its own owner funding plus a declared code override
/// at [`DUAL`] that stores [`STORED_BY_A`].
fn candidate_a_overrides() -> SimulationOverrideParams {
    SimulationOverrideParams {
        owner: OWNER_A,
        code: Some(DeclaredCode {
            address: DUAL,
            runtime: store_runtime(STORED_BY_A),
        }),
        settlement: None,
    }
}

/// Candidate B's overrides: only its own owner funding — no declared code, so
/// B's DB (which carries no [`DUAL`] account) is the whole story.
fn candidate_b_overrides() -> SimulationOverrideParams {
    SimulationOverrideParams {
        owner: OWNER_B,
        code: None,
        settlement: None,
    }
}

/// The owner funding an override installs.
fn owner_funding() -> U256 {
    U256::from(OWNER_FUND_ETH) * U256::from(1_000_000_000_000_000_000_u64)
}

/// The account read through the slot's accepted overlay and database.
fn account_balance(slot: &mut AsyncFanoutSlot<'_>, address: Address) -> Option<U256> {
    slot.evm_mut()
        .read_account_info(&address)
        .expect("the in-memory database answers an account read")
        .map(|info| info.balance)
}

/// The storage word read through the slot's accepted overlay and database.
fn read_slot_word(slot: &mut AsyncFanoutSlot<'_>, address: Address) -> U256 {
    slot.evm_mut()
        .state_mut()
        .storage_slot_untracked(&address, &Word::from(SLOT))
        .expect("the in-memory database answers a storage read")
}

/// A reused slot retains its engine configuration and nothing its previous
/// candidate produced: the accepted writes, the committed logs, the transient
/// storage, and the previous candidate's override rows are all gone, while the
/// newly installed database and overrides are the ones the next candidate sees.
#[test]
fn same_slot_reuse_clears_the_previous_candidates_state() {
    let config = FanoutEngineConfig::production(block_env());
    let mut slot = AsyncFanoutSlot::build(0, &config, candidate_a_db(), &candidate_a_overrides());
    assert_eq!(slot.installs(), 1);
    assert_eq!(slot.worker(), 0);
    assert_eq!(
        read_slot_word(&mut slot, DUAL),
        U256::from(SEEDED_A),
        "the slot starts from candidate A's own database"
    );

    // Candidate A's work on the slot: a committed transaction that accepted a
    // write, plus the scratch a parked candidate leaves behind (a committed
    // log and a transient-storage word).
    let _settled = slot
        .evm_mut()
        .transact(&call_tx(DUAL, &[]))
        .expect("candidate A's write step executes")
        .commit();
    slot.evm_mut()
        .state_mut()
        .logs_mut()
        .push(alloy::primitives::Log::default());
    slot.evm_mut()
        .state_mut()
        .tstore(&DUAL, &TRANSIENT_KEY, &Word::from(TRANSIENT_VALUE));

    assert_eq!(
        read_slot_word(&mut slot, DUAL),
        U256::from(STORED_BY_A),
        "candidate A's accepted write is visible in the slot"
    );
    assert_eq!(
        account_balance(&mut slot, OWNER_A),
        Some(owner_funding()),
        "candidate A's owner funding is installed"
    );
    assert_eq!(slot.evm_mut().state_mut().logs().len(), 1);
    assert_eq!(
        slot.evm_mut().state_mut().tload(&DUAL, &TRANSIENT_KEY),
        Word::from(TRANSIENT_VALUE)
    );

    // Candidate B is installed over the same slot.
    slot.install(candidate_b_db(), &candidate_b_overrides());
    assert_eq!(slot.installs(), 2);

    assert_eq!(
        read_slot_word(&mut slot, DUAL),
        U256::from(SEEDED_B),
        "the accepted write is gone and candidate B's database is installed"
    );
    assert!(
        slot.evm_mut().state_mut().logs().is_empty(),
        "the previous candidate's committed logs are cleared"
    );
    assert!(
        slot.evm_mut().state_mut().journal().is_empty(),
        "the previous candidate's journal is cleared"
    );
    assert_eq!(
        slot.evm_mut().state_mut().tload(&DUAL, &TRANSIENT_KEY),
        Word::ZERO,
        "the previous candidate's transient storage is cleared"
    );
    assert_eq!(
        account_balance(&mut slot, OWNER_A),
        None,
        "candidate A's owner funding does not survive into candidate B"
    );
    assert_eq!(
        account_balance(&mut slot, DUAL),
        None,
        "candidate A's declared code override does not survive into candidate B"
    );
    assert_eq!(
        account_balance(&mut slot, OWNER_B),
        Some(owner_funding()),
        "candidate B's own overrides are the ones installed"
    );
    assert_eq!(
        slot.config().spec(),
        config.spec(),
        "the slot retains the engine configuration it was built from"
    );
}

/// The limits split the slot budget over the funded lanes and never leave a lane
/// without one.
#[test]
fn limits_split_the_slot_budget_over_the_funded_lanes() {
    let limits = FanoutLimits::new(3, 8);
    assert_eq!(limits.effective_workers(), 3);
    assert_eq!(
        (0..3)
            .map(|worker| limits.lane_capacity(worker))
            .sum::<usize>(),
        8,
        "the lanes own exactly the slot budget"
    );
    assert_eq!(
        (0..3)
            .map(|worker| limits.lane_capacity(worker))
            .collect::<Vec<_>>(),
        vec![3, 3, 2]
    );

    let starved = FanoutLimits::new(5, 2);
    assert_eq!(
        starved.effective_workers(),
        2,
        "a lane needs a slot to exist"
    );
    assert_eq!(starved.lane_capacity(2), 0);
    assert_eq!(FanoutLimits::new(0, 0).slots(), 1, "zero collapses to one");
}

/// The lane structure, the slot bound, and the reuse count, measured.
#[tokio::test]
async fn the_active_slot_bound_holds_and_queued_candidates_allocate_nothing() {
    let candidates: Vec<PreparedCandidate> = (0..6).map(transfer_candidate).collect();
    let source = SeededSource::new(vec![2; 6]);
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(2, 2), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("the fan-out settles every candidate");

    assert_eq!(outcome.stats.workers, 2, "two lanes carry the fan-out");
    assert_eq!(outcome.stats.slots, 2, "the slot budget the run was given");
    assert_eq!(
        outcome.stats.slots_created, 2,
        "six candidates, two EVMs: a queued candidate allocates no slot"
    );
    assert_eq!(
        outcome.stats.installs, 6,
        "each admission installs; the excess over slots_created is reuse"
    );
    assert!(
        outcome.stats.peak_active_slots > 0,
        "the fan-out ran, so at least one slot was active"
    );
    assert!(
        outcome.stats.peak_active_slots <= outcome.stats.slots,
        "at most the slot budget is ever in flight, peaked at {}",
        outcome.stats.peak_active_slots
    );
    assert_eq!(outcome.stats.completion_order.len(), 6);
    for index in 0..6 {
        assert_candidate_identity(&outcome, index);
    }
}

/// One worker, three slots: a lane carrying more than one EVM is what makes the
/// suspended-candidate fan-out possible at all.
#[tokio::test]
async fn one_worker_carries_several_slots() {
    let candidates: Vec<PreparedCandidate> = (0..3).map(transfer_candidate).collect();
    let source = SeededSource::new(vec![1, 1, 1]);
    let reads = source.read_log();
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(1, 3), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("the fan-out settles every candidate");

    assert_eq!(outcome.stats.workers, 1, "one lane");
    assert_eq!(
        outcome.stats.peak_active_slots, 3,
        "the one lane holds three slots at once, so one EVM per worker is insufficient"
    );
    assert_eq!(outcome.stats.slots_created, 3);
    let first_reads: Vec<usize> = reads
        .lock()
        .expect("the read log is not poisoned")
        .iter()
        .copied()
        .take(3)
        .collect();
    assert_eq!(
        first_reads,
        vec![0, 1, 2],
        "all three candidates registered a read before any of them settled"
    );
    for index in 0..3 {
        assert_candidate_identity(&outcome, index);
    }
}

/// Completion order is the schedule's, not the result order's: the slowest
/// candidate is index 0 and the results still come back by index.
#[tokio::test]
async fn results_return_by_input_index_under_reversed_completion() {
    let candidates: Vec<PreparedCandidate> = (0..4).map(transfer_candidate).collect();
    // Candidate `index` needs `(4 - index) * 2` suspensions per read, so index 0
    // finishes last and index 3 first.
    let source = SeededSource::new(vec![8, 6, 4, 2]);
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(1, 4), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("the fan-out settles every candidate");

    assert_eq!(
        outcome.stats.completion_order,
        vec![3, 2, 1, 0],
        "the delays complete the candidates in reverse input order"
    );
    for index in 0..4 {
        assert_candidate_identity(&outcome, index);
    }
}

/// A refused candidate refuses only itself: the neighbours in the same fan-out
/// settle, and each refusal keeps its own class and step index.
#[tokio::test]
async fn per_candidate_errors_stay_isolated() {
    let candidates = vec![
        transfer_candidate(0),
        transfer_candidate(1).with_mode(Evm2CaptureMode::Inspector),
        PreparedCandidate::new(
            vec![
                Evm2SimStep::Transact(value_tx(recipient(2), transferred(2))),
                Evm2SimStep::Transact(refused_tx()),
            ],
            settlement_overrides(),
        ),
    ];
    let source = SeededSource::new(vec![0, 0, 0]);
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(2, 3), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("a refused candidate does not fail the fan-out");

    assert_candidate_identity(&outcome, 0);
    assert_eq!(
        outcome
            .refusals()
            .map(|(index, _)| index)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "exactly the two refused candidates are refused"
    );
    assert!(
        matches!(
            outcome.outcomes[1].refusal(),
            Some(Evm2SimError::AsyncInspectorUnsupported {
                mode: Evm2CaptureMode::Inspector
            })
        ),
        "candidate 1 keeps its own capture-mode refusal"
    );
    let step_refusal = outcome.outcomes[2]
        .refusal()
        .expect("candidate 2 was refused");
    assert_eq!(
        step_refusal.step_index(),
        Some(1),
        "candidate 2 keeps its own refusing step"
    );
}

/// A fan-out smaller than the slot budget occupies exactly the slots it needs.
#[tokio::test]
async fn a_small_job_occupies_one_slot() {
    let candidates = vec![transfer_candidate(0)];
    let source = SeededSource::new(vec![1]);
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(2, 4), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("the single candidate settles");

    assert_eq!(outcome.stats.slots_created, 1);
    assert_eq!(outcome.stats.peak_active_slots, 1);
    assert_eq!(outcome.stats.installs, 1);
    assert_eq!(outcome.stats.completion_order, vec![0]);
    assert_candidate_identity(&outcome, 0);
}

/// The concurrent fan-out and the one-slot sequential driver settle the same
/// facts per candidate, over a database whose reads park on the read hub: the
/// fan-out's idle rounds are what settle them, and the results are the
/// sequential ones — under either drain ownership.
#[tokio::test]
async fn the_fanout_settles_the_indexed_sequential_result() {
    for policy in [DrainPolicy::SessionWide, DrainPolicy::PerLane] {
        indexed_sequential_result_holds(policy).await;
    }
}

/// One drain policy's concurrent fan-out, against the one-slot sequential driver.
async fn indexed_sequential_result_holds(policy: DrainPolicy) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(
        &provider,
        ReadAnchor::identified(BLOCK, B256::repeat_byte(0xaa)),
    );
    let candidates: Vec<PreparedCandidate> = (0..3).map(hub_candidate).collect();
    let source = HubSource {
        hub: &hub,
        provider: &provider,
    };

    let concurrent = AsyncEvmFanout::new(FanoutLimits::new(3, 3), fanout_config())
        .with_drain_policy(policy)
        .run(&candidates, &source, Some(&hub))
        .await
        .expect("the concurrent fan-out settles every candidate");
    assert!(
        concurrent.stats.rounds_drained >= 1,
        "the parked cold reads were settled by drained rounds"
    );
    let stats = hub.stats();
    assert!(stats.rounds >= 1, "the hub settled at least one round");

    let sequential = AsyncEvmFanout::new(FanoutLimits::new(1, 1), fanout_config())
        .run(&candidates, &source, Some(&hub))
        .await
        .expect("the sequential driver settles every candidate");
    assert_eq!(
        sequential.stats.slots_created, 1,
        "the sequential driver reuses one slot"
    );
    assert_eq!(sequential.stats.completion_order, vec![0, 1, 2]);

    for (index, (fast, slow)) in concurrent
        .outcomes
        .iter()
        .zip(sequential.outcomes.iter())
        .enumerate()
    {
        let fast = fast.sequence().expect("the concurrent candidate settled");
        let slow = slow.sequence().expect("the sequential candidate settled");
        assert_sequence_identity(fast, slow, index);
        assert_eq!(
            fast.calls[1].output,
            served_storage_word(),
            "candidate {index}: the captured step returns the hub's slot value"
        );
        assert_eq!(
            fast.calls[2].native_balance,
            Some(U256::from(transferred(index))),
            "candidate {index}: the transfer it made is its own"
        );
    }
}

/// One lane's pass registers every parked candidate's read before it drains, so
/// candidates sharing a read identity coalesce onto ONE fetch by construction —
/// the single-flight rule's own evidence, not a thread-start race.
#[tokio::test]
async fn concurrent_candidates_coalesce_onto_one_fetch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hub = ReadHub::new(
        &provider,
        ReadAnchor::identified(BLOCK, B256::repeat_byte(0xaa)),
    );
    let candidates: Vec<PreparedCandidate> = (0..2).map(hub_candidate).collect();
    let source = HubSource {
        hub: &hub,
        provider: &provider,
    };

    let outcome = AsyncEvmFanout::new(FanoutLimits::new(1, 2), fanout_config())
        .run(&candidates, &source, Some(&hub))
        .await
        .expect("both candidates settle");

    let stats = hub.stats();
    assert!(
        stats.coalesced >= 1,
        "the second candidate joined the first one's queued identity, got {stats:?}"
    );
    for index in 0..2 {
        let sequence = outcome
            .sequence(index)
            .unwrap_or_else(|| panic!("candidate {index} settled"));
        assert_eq!(
            sequence.calls[1].output,
            served_storage_word(),
            "candidate {index}: the captured step returns the hub's slot value"
        );
        assert_eq!(
            sequence.calls[2].native_balance,
            Some(U256::from(transferred(index))),
            "candidate {index}: the transfer it made is its own"
        );
    }
}

/// The value one unit of a candidate's transfer moves (1e15 wei) — candidate
/// `index` transfers `(index + 1)` units, so its result names its own index.
const VALUE_UNIT: u64 = 1_000_000_000_000_000;

/// The slot value the stub chain serves and the fixture contract returns.
const SERVED_STORAGE: u8 = 0x2A;

/// Which candidates made which reads, in registration order — the schedule's
/// interleaving evidence.
type ReadLog = Arc<Mutex<Vec<usize>>>;

/// The fan-out's engine configuration for the test block.
fn fanout_config() -> FanoutEngineConfig {
    FanoutEngineConfig::production(block_env())
}

/// Candidate `index`'s transfer recipient: a distinct codeless account.
fn recipient(index: usize) -> Address {
    Address::with_last_byte(0x40 + u8::try_from(index).expect("the fixture has few candidates"))
}

/// The wei candidate `index` transfers.
fn transferred(index: usize) -> u64 {
    VALUE_UNIT * (u64::try_from(index).expect("the fixture has few candidates") + 1)
}

/// An envelope transferring `value` wei to `to`.
fn value_tx(to: Address, value: u64) -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(to),
            value: U256::from(value),
            gas_limit: 200_000,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// An envelope the engine refuses (a gas limit under the intrinsic floor).
fn refused_tx() -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(DUAL),
            input: Bytes::from_static(&[0x00]),
            gas_limit: 100,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// `keccak256("Ping()")` — the fixture contract's log topic.
fn ping_topic() -> B256 {
    keccak256("Ping()")
}

/// The runtime declared at [`DUAL`] for the hub-backed candidates: load
/// [`SLOT`] (a cold database read through the hub), emit one log (a committed
/// capture fact), and return the loaded word.
fn emit_and_read_runtime() -> Bytes {
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x60, SLOT, 0x54]); // SLOAD slot 5
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // MSTORE 0
    code.push(0x7F); // PUSH32 topic
    code.extend_from_slice(ping_topic().as_slice());
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0xA1]); // LOG1(0, 0)
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xF3]); // RETURN 32 bytes
    Bytes::from(code)
}

/// A runtime that loads [`SLOT`] and returns it — the read candidate B makes
/// over the slot it shares with candidate A.
fn read_slot_runtime() -> Bytes {
    Bytes::from(vec![
        0x60, SLOT, 0x54, // SLOAD slot 5
        0x60, 0x00, 0x52, // MSTORE 0
        0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN 32 bytes
    ])
}

/// The 32-byte word a call returned (big-endian, right-aligned).
fn returned_word(output: &Bytes) -> U256 {
    U256::from_be_slice(&output[output.len().saturating_sub(32)..])
}

/// The 32-byte word the fixture contract returns: the hub's slot value.
fn served_storage_word() -> Bytes {
    Bytes::from(U256::from(SERVED_STORAGE).to_be_bytes::<32>().to_vec())
}

/// Overrides for the hub-backed candidates: an owner funding plus the fixture
/// runtime declared at [`DUAL`].
fn hub_overrides() -> SimulationOverrideParams {
    SimulationOverrideParams {
        owner: OWNER_A,
        code: Some(DeclaredCode {
            address: DUAL,
            runtime: emit_and_read_runtime(),
        }),
        settlement: None,
    }
}

/// Overrides that install nothing but an owner funding.
fn settlement_overrides() -> SimulationOverrideParams {
    SimulationOverrideParams {
        owner: OWNER_B,
        code: None,
        settlement: None,
    }
}

/// One candidate's work: transfer its own amount, then read the balance it left.
fn transfer_candidate(index: usize) -> PreparedCandidate {
    PreparedCandidate::new(
        vec![
            Evm2SimStep::Transact(value_tx(recipient(index), transferred(index))),
            Evm2SimStep::ReadNativeBalance(recipient(index)),
        ],
        settlement_overrides(),
    )
}

/// One hub-backed candidate's work: transfer its own amount, run the fixture
/// contract (a cold storage read + a log through the hub), then read the balance
/// it left.
fn hub_candidate(index: usize) -> PreparedCandidate {
    PreparedCandidate::new(
        vec![
            Evm2SimStep::Transact(value_tx(recipient(index), transferred(index))),
            Evm2SimStep::Inspect(call_tx(DUAL, &[])),
            Evm2SimStep::ReadNativeBalance(recipient(index)),
        ],
        hub_overrides(),
    )
}

/// A future that reports `Pending` once (self-waking) before completing — one
/// unit of a read delay.
#[derive(Default)]
struct YieldOnce {
    yielded: bool,
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.yielded {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Suspend `count` times, waking after each.
async fn yield_times(count: usize) {
    for _ in 0..count {
        YieldOnce::default().await;
    }
}

/// An in-memory database that suspends (and logs its candidate) on every read —
/// the test's control over completion order.
struct DelayedDb {
    inner: InMemoryDB,
    delays: usize,
    candidate: usize,
    log: ReadLog,
}

impl DelayedDb {
    /// Record the read, then suspend `delays` times.
    async fn note(&mut self) {
        self.log
            .lock()
            .expect("the read log is not poisoned")
            .push(self.candidate);
        yield_times(self.delays).await;
    }
}

impl AsyncDatabase for DelayedDb {
    type Error = ::evm2::DatabaseError;

    fn is_fatal(error: &Self::Error) -> bool {
        error.is_fatal()
    }

    async fn get_account(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.note().await;
        DynDatabase::get_account(&mut self.inner, &address)
    }

    async fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
        self.note().await;
        DynDatabase::get_code_by_hash(&mut self.inner, &code_hash)
    }

    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        self.note().await;
        DynDatabase::get_storage(&mut self.inner, &address, &key)
    }

    async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
        self.note().await;
        DynDatabase::get_block_hash(&mut self.inner, &number)
    }
}

/// The per-candidate state source: a funded sender over a delayed database whose
/// delay the candidate index selects.
struct SeededSource {
    delays: Vec<usize>,
    log: ReadLog,
}

impl SeededSource {
    fn new(delays: Vec<usize>) -> Self {
        Self {
            delays,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn read_log(&self) -> ReadLog {
        Arc::clone(&self.log)
    }
}

impl CandidateStateSource<'static> for SeededSource {
    fn database(&self, candidate: usize) -> Box<dyn DynDatabase + 'static> {
        Box::new(AsyncDb::new(DelayedDb {
            inner: funded_sender(),
            delays: self.delays.get(candidate).copied().unwrap_or(0),
            candidate,
            log: Arc::clone(&self.log),
        }))
    }
}

/// The per-candidate state source over the read hub: every candidate's database
/// is the hub adapter, so a cold read parks until a round is drained.
struct HubSource<'h, 'p> {
    hub: &'h ReadHub,
    provider: &'p AlloyProvider,
}

impl<'h> CandidateStateSource<'h> for HubSource<'h, '_> {
    fn database(&self, _candidate: usize) -> Box<dyn DynDatabase + 'h> {
        Box::new(AsyncDb::new(ReadHubDb::new(self.hub, self.provider)))
    }
}

/// A chain stub serving the hub's read shapes and recording dispatched packets.
#[derive(Debug, Clone, Default)]
struct ChainStub {
    packets: Arc<Mutex<Vec<Vec<String>>>>,
    /// How long a dispatched packet takes to answer — a round's latency, so a
    /// round can still be in flight while another lane looks for work.
    delay: Duration,
    /// Raised the moment a packet reaches the transport: a round is in flight,
    /// which a held-back read waits for before it registers.
    dispatched: Option<Arc<FirstDispatch>>,
}

impl ChainStub {
    fn serving() -> Self {
        Self::default()
    }

    /// The same stub, answering each packet after `delay`.
    fn slow(delay: Duration) -> Self {
        Self {
            delay,
            ..Self::default()
        }
    }

    /// The same stub, raising `dispatched` as each packet reaches it.
    fn announcing(mut self, dispatched: Arc<FirstDispatch>) -> Self {
        self.dispatched = Some(dispatched);
        self
    }

    fn provider(&self) -> AlloyProvider {
        let client = ClientBuilder::default().transport(self.clone(), true);
        let root: RootProvider = RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }

    /// A dispatched request's answer.
    fn answer(req: &SerializedRequest) -> Response {
        let result = match req.method() {
            "eth_getBalance" => {
                if Self::address_param(req) == SENDER {
                    json!("0xde0b6b3a7640000")
                } else {
                    json!("0x0")
                }
            }
            "eth_getTransactionCount" => json!("0x0"),
            "eth_getCode" => json!("0x"),
            "eth_getStorageAt" => json!(format!("{SERVED_STORAGE:#x}")),
            other => panic!("the fan-out's hub reads must not issue {other}"),
        };
        Response {
            id: req.id().clone(),
            payload: ResponsePayload::Success(
                to_raw_value(&result).expect("the stub answer is JSON"),
            ),
        }
    }

    /// The address a member request names.
    fn address_param(req: &SerializedRequest) -> Address {
        let params: Value =
            serde_json::from_str(req.params().expect("a stub member carries params").get())
                .expect("member params are JSON");
        params
            .get(0)
            .and_then(Value::as_str)
            .expect("the address is the first param")
            .parse()
            .expect("the member address parses")
    }
}

impl tower::Service<RequestPacket> for ChainStub {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let stub = self.clone();
        Box::pin(async move {
            if let Some(dispatched) = &stub.dispatched {
                dispatched.raise();
            }
            if !stub.delay.is_zero() {
                std::thread::sleep(stub.delay);
            }
            let recorded: Vec<String> = match &req {
                RequestPacket::Single(one) => vec![one.method().to_string()],
                RequestPacket::Batch(many) => {
                    many.iter().map(|one| one.method().to_string()).collect()
                }
            };
            stub.packets
                .lock()
                .expect("the packet log is not poisoned")
                .push(recorded);
            Ok::<ResponsePacket, TransportError>(match req {
                RequestPacket::Single(one) => ResponsePacket::Single(ChainStub::answer(&one)),
                RequestPacket::Batch(many) => {
                    ResponsePacket::Batch(many.iter().map(ChainStub::answer).collect())
                }
            })
        })
    }
}

/// Candidate `index` settled, and its facts are its own.
fn assert_candidate_identity(outcome: &FanoutOutcome, index: usize) {
    let Some(sequence) = outcome.sequence(index) else {
        panic!("candidate {index} settled");
    };
    assert_eq!(
        sequence.calls.len(),
        2,
        "candidate {index}: both steps reported"
    );
    assert_eq!(
        sequence.calls[0].verdict,
        RunVerdict::Success,
        "candidate {index}: the transfer step succeeded"
    );
    assert_eq!(
        sequence.calls[1].native_balance,
        Some(U256::from(transferred(index))),
        "candidate {index}'s result is its own, whatever order it completed in"
    );
}

/// Two runs' sequences for one candidate, compared field by field.
fn assert_sequence_identity(
    concurrent: &Evm2SimSequence,
    sequential: &Evm2SimSequence,
    index: usize,
) {
    assert_eq!(
        concurrent.calls.len(),
        sequential.calls.len(),
        "candidate {index}: step count"
    );
    for (step, (fast, slow)) in concurrent.calls.iter().zip(&sequential.calls).enumerate() {
        assert_eq!(
            fast.verdict, slow.verdict,
            "candidate {index} step {step}: verdict"
        );
        assert_eq!(
            fast.gas_used, slow.gas_used,
            "candidate {index} step {step}: gas used"
        );
        assert_eq!(
            fast.output, slow.output,
            "candidate {index} step {step}: output"
        );
        assert_eq!(
            fast.native_balance, slow.native_balance,
            "candidate {index} step {step}: native balance"
        );
        assert_eq!(
            fast.storage_writes, slow.storage_writes,
            "candidate {index} step {step}: storage writes"
        );
    }
    assert_eq!(
        concurrent.storage_writes, sequential.storage_writes,
        "candidate {index}: accumulated writes"
    );
    assert_eq!(
        concurrent.capture.logs, sequential.capture.logs,
        "candidate {index}: captured logs"
    );
    assert_eq!(
        concurrent.capture.access_list, sequential.capture.access_list,
        "candidate {index}: drained access list"
    );
}

/// Two candidates forced onto one slot (one lane, one slot) share the engine
/// configuration and nothing else: candidate A's SSTORE is A's alone, and B reads
/// the database value its own installation put there.
#[tokio::test]
async fn two_candidates_on_one_slot_share_no_writes() {
    let candidates = vec![
        PreparedCandidate::new(
            vec![Evm2SimStep::Transact(call_tx(DUAL, &[]))],
            SimulationOverrideParams {
                owner: OWNER_A,
                code: Some(DeclaredCode {
                    address: DUAL,
                    runtime: store_runtime(STORED_BY_A),
                }),
                settlement: None,
            },
        ),
        PreparedCandidate::new(
            vec![Evm2SimStep::Transact(call_tx(DUAL, &[]))],
            SimulationOverrideParams {
                owner: OWNER_B,
                code: Some(DeclaredCode {
                    address: DUAL,
                    runtime: read_slot_runtime(),
                }),
                settlement: None,
            },
        ),
    ];
    let source = SeededSource::new(vec![0, 0]);
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(1, 1), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("both candidates settle");

    assert_eq!(
        outcome.stats.slots_created, 1,
        "one slot carried both candidates"
    );
    assert_eq!(outcome.stats.installs, 2, "the second candidate reused it");

    let first = outcome.sequence(0).expect("candidate A settled");
    assert_eq!(
        first.storage_writes,
        vec![(DUAL, U256::from(SLOT), U256::from(STORED_BY_A))],
        "candidate A's write is its own settled result"
    );
    let second = outcome.sequence(1).expect("candidate B settled");
    assert!(
        second.storage_writes.is_empty(),
        "candidate B wrote nothing"
    );
    assert_eq!(
        returned_word(&second.calls[0].output),
        U256::ZERO,
        "candidate B reads the database's word, never candidate A's write"
    );
}

/// Nothing can wake a parked candidate forever: the driver refuses to spin and
/// reports the candidates still waiting instead of parking the caller — under
/// either drain ownership.
#[tokio::test]
async fn a_candidate_no_round_can_settle_is_reported_as_stalled() {
    for policy in [DrainPolicy::SessionWide, DrainPolicy::PerLane] {
        let candidates = vec![transfer_candidate(0)];
        let outcome = AsyncEvmFanout::new(FanoutLimits::new(1, 1), fanout_config())
            .with_drain_policy(policy)
            .run(&candidates, &ParkingSource, None)
            .await;
        let Err(refusal) = outcome else {
            panic!("the run reports the stall under {policy:?}");
        };
        assert_eq!(
            refusal,
            FanoutError::Stalled { waiting: vec![0] },
            "the stalled candidate is named"
        );
    }
}

/// The state source whose reads never resolve and never wake.
struct ParkingSource;

impl CandidateStateSource<'static> for ParkingSource {
    fn database(&self, _candidate: usize) -> Box<dyn DynDatabase + 'static> {
        Box::new(AsyncDb::new(ParkingDb))
    }
}

/// A database whose every read parks forever without waking the driver.
struct ParkingDb;

impl AsyncDatabase for ParkingDb {
    type Error = ::evm2::DatabaseError;

    async fn get_account(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        std::future::pending::<Result<Option<AccountInfo>, Self::Error>>().await
    }

    async fn get_code_by_hash(
        &mut self,
        _code_hash: B256,
    ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
        std::future::pending::<Result<::evm2::bytecode::Bytecode, Self::Error>>().await
    }

    async fn get_storage(&mut self, _address: Address, _key: Word) -> Result<Word, Self::Error> {
        std::future::pending::<Result<Word, Self::Error>>().await
    }

    async fn get_block_hash(&mut self, _number: Word) -> Result<B256, Self::Error> {
        std::future::pending::<Result<B256, Self::Error>>().await
    }
}

/// Candidate admissions, as `(candidate, thread)` — the scheduler's own evidence
/// that a lane is a thread.
type AdmissionLog = Arc<Mutex<Vec<(usize, std::thread::ThreadId)>>>;

/// A rendezvous `parties` reads must reach at the same time.
///
/// A lane set that shares one thread cannot satisfy it: the first party parks
/// the thread the second one needs, so the wait runs into its deadline. A
/// timed-out rendezvous is recorded as failed, so a later party cannot satisfy it
/// by arriving after the first one gave up.
struct Rendezvous {
    parties: usize,
    state: Mutex<RendezvousState>,
    released: Condvar,
}

/// How many parties have arrived, and whether the rendezvous already failed.
struct RendezvousState {
    arrived: usize,
    failed: bool,
}

impl Rendezvous {
    /// A rendezvous `parties` reads must reach together.
    fn new(parties: usize) -> Self {
        Self {
            parties,
            state: Mutex::new(RendezvousState {
                arrived: 0,
                failed: false,
            }),
            released: Condvar::new(),
        }
    }

    /// The wait a party tolerates before the rendezvous counts as unmet.
    const BUDGET: Duration = Duration::from_secs(5);

    /// Wait for the other parties, reporting whether they arrived in time.
    fn meet(&self) -> bool {
        let mut state = self.state.lock().expect("the rendezvous is not poisoned");
        if state.failed {
            return false;
        }
        state.arrived += 1;
        if state.arrived >= self.parties {
            self.released.notify_all();
            return true;
        }
        loop {
            let (next, timeout) = self
                .released
                .wait_timeout(state, Self::BUDGET)
                .expect("the rendezvous is not poisoned");
            state = next;
            if state.failed {
                return false;
            }
            if state.arrived >= self.parties {
                return true;
            }
            if timeout.timed_out() {
                state.failed = true;
                self.released.notify_all();
                return false;
            }
        }
    }
}

/// The per-candidate state source that records each admission's thread and holds
/// the first two candidates at a rendezvous on their first read.
struct ThreadedSource {
    admissions: AdmissionLog,
    rendezvous: Arc<Rendezvous>,
    met: Arc<AtomicBool>,
}

impl ThreadedSource {
    /// A source whose first two candidates must be in flight together.
    fn new() -> Self {
        Self {
            admissions: Arc::new(Mutex::new(Vec::new())),
            rendezvous: Arc::new(Rendezvous::new(2)),
            met: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl CandidateStateSource<'static> for ThreadedSource {
    fn database(&self, candidate: usize) -> Box<dyn DynDatabase + 'static> {
        self.admissions
            .lock()
            .expect("the admission log is not poisoned")
            .push((candidate, std::thread::current().id()));
        Box::new(AsyncDb::new(RendezvousDb {
            inner: funded_sender(),
            party: candidate < 2,
            rendezvous: Arc::clone(&self.rendezvous),
            met: Arc::clone(&self.met),
            met_once: false,
        }))
    }
}

/// A database that holds its first read at the rendezvous.
struct RendezvousDb {
    inner: InMemoryDB,
    /// Whether this candidate is a rendezvous party.
    party: bool,
    rendezvous: Arc<Rendezvous>,
    met: Arc<AtomicBool>,
    met_once: bool,
}

impl RendezvousDb {
    /// Meet the other party before the first read answers.
    fn meet(&mut self) {
        if !self.party || self.met_once {
            return;
        }
        self.met_once = true;
        if self.rendezvous.meet() {
            self.met.store(true, Ordering::SeqCst);
        }
    }
}

impl AsyncDatabase for RendezvousDb {
    type Error = ::evm2::DatabaseError;

    fn is_fatal(error: &Self::Error) -> bool {
        error.is_fatal()
    }

    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + '_ {
        self.meet();
        std::future::ready(DynDatabase::get_account(&mut self.inner, &address))
    }

    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<::evm2::bytecode::Bytecode, Self::Error>> + '_ {
        self.meet();
        std::future::ready(DynDatabase::get_code_by_hash(&mut self.inner, &code_hash))
    }

    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + '_ {
        self.meet();
        std::future::ready(DynDatabase::get_storage(&mut self.inner, &address, &key))
    }

    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<B256, Self::Error>> + '_ {
        self.meet();
        std::future::ready(DynDatabase::get_block_hash(&mut self.inner, &number))
    }
}

/// W > 1 lanes are W OS threads, not W interleavings of one thread: three
/// candidates over three lanes are admitted on three distinct threads (none of
/// them the thread awaiting the fan-out), and two of them must be in flight at
/// the same time to get past their rendezvous.
#[tokio::test]
async fn workers_run_candidates_on_distinct_os_threads() {
    let candidates: Vec<PreparedCandidate> = (0..3).map(transfer_candidate).collect();
    let source = ThreadedSource::new();
    let outcome = AsyncEvmFanout::new(FanoutLimits::new(3, 3), fanout_config())
        .run(&candidates, &source, None)
        .await
        .expect("the fan-out settles every candidate");

    assert_eq!(outcome.stats.workers, 3, "three lanes carry the fan-out");
    let admissions: Vec<(usize, std::thread::ThreadId)> = source
        .admissions
        .lock()
        .expect("the admission log is not poisoned")
        .clone();
    let threads: HashSet<std::thread::ThreadId> =
        admissions.iter().map(|(_, thread)| *thread).collect();
    assert_eq!(
        threads.len(),
        3,
        "each candidate was admitted on its own OS thread, got {admissions:?}"
    );
    assert!(
        !threads.contains(&std::thread::current().id()),
        "no lane ran on the thread awaiting the fan-out, got {admissions:?}"
    );
    assert!(
        source.met.load(Ordering::SeqCst),
        "two lanes were in flight at the same time; a lane set sharing one thread parks the first \
         party until its deadline instead"
    );
    for index in 0..3 {
        assert_candidate_identity(&outcome, index);
    }
}

/// The fan-out this test drives: three lanes over a total budget of four slots.
const HUB_WORKERS: usize = 3;
const HUB_SLOTS: usize = 4;

/// How long one hub round takes to answer in this test — long enough that one
/// lane's round is still in flight while the others look for work.
const ROUND_DELAY: Duration = Duration::from_millis(25);

/// One candidate's hub stack under a gate: the FIRST read waits at the gate
/// before it registers, so every lane has a candidate parked on the hub before
/// any lane can settle a round.
struct GatedHubDb<'h> {
    inner: ReadHubDb<'h>,
    gate: Arc<Rendezvous>,
    met: Arc<AtomicUsize>,
    arrived: bool,
}

impl<'h> GatedHubDb<'h> {
    fn new(inner: ReadHubDb<'h>, gate: Arc<Rendezvous>, met: Arc<AtomicUsize>) -> Self {
        Self {
            inner,
            gate,
            met,
            arrived: false,
        }
    }

    /// Meet the other lanes before the first read registers.
    fn gate(&mut self) {
        if self.arrived {
            return;
        }
        self.arrived = true;
        if self.gate.meet() {
            self.met.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl AsyncDatabase for GatedHubDb<'_> {
    type Error = ReadHubDbError;

    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send + '_ {
        self.gate();
        self.inner.get_account(address)
    }

    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<::evm2::bytecode::Bytecode, Self::Error>> + Send + '_ {
        self.gate();
        self.inner.get_code_by_hash(code_hash)
    }

    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + Send + '_ {
        self.gate();
        self.inner.get_storage(address, key)
    }

    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send + '_ {
        self.gate();
        self.inner.get_block_hash(number)
    }
}

/// The per-candidate state source over the gated hub stack.
struct GatedHubSource<'h, 'p> {
    hub: &'h ReadHub,
    provider: &'p AlloyProvider,
    gate: Arc<Rendezvous>,
    met: Arc<AtomicUsize>,
}

impl<'h> CandidateStateSource<'h> for GatedHubSource<'h, '_> {
    fn database(&self, _candidate: usize) -> Box<dyn DynDatabase + 'h> {
        Box::new(AsyncDb::new(GatedHubDb::new(
            ReadHubDb::new(self.hub, self.provider),
            Arc::clone(&self.gate),
            Arc::clone(&self.met),
        )))
    }
}

/// W lanes and F total slots over the real read hub, with every lane parked on the
/// hub before the first round: one lane's round is in flight while the others find
/// an empty drain, and the run still completes. A lane that counted a neighbour's
/// round as "nothing can settle" would spin through [`MAX_IDLE_ROUNDS`] and report
/// a false [`FanoutError::Stalled`] while the reads were being served — under
/// either drain ownership.
#[tokio::test]
async fn overlapping_hub_reads_across_workers_do_not_false_stall() {
    for policy in [DrainPolicy::SessionWide, DrainPolicy::PerLane] {
        overlapping_reads_do_not_false_stall(policy).await;
    }
}

/// One drain policy's overlapping hub reads across workers.
async fn overlapping_reads_do_not_false_stall(policy: DrainPolicy) {
    let stub = ChainStub::slow(ROUND_DELAY);
    let provider = stub.provider();
    let hub = ReadHub::new(
        &provider,
        ReadAnchor::identified(BLOCK, B256::repeat_byte(0xaa)),
    );
    let candidates: Vec<PreparedCandidate> = (0..8).map(hub_candidate).collect();
    let met = Arc::new(AtomicUsize::new(0));
    let source = GatedHubSource {
        hub: &hub,
        provider: &provider,
        gate: Arc::new(Rendezvous::new(HUB_WORKERS)),
        met: Arc::clone(&met),
    };

    let outcome = AsyncEvmFanout::new(FanoutLimits::new(HUB_WORKERS, HUB_SLOTS), fanout_config())
        .with_drain_policy(policy)
        .run(&candidates, &source, Some(&hub))
        .await
        .expect("overlapping hub reads across workers must not report a false stall");

    assert!(
        met.load(Ordering::SeqCst) >= HUB_WORKERS,
        "all {HUB_WORKERS} lanes reached the gate, so they had candidates parked on the hub at \
         the same time; {} met it",
        met.load(Ordering::SeqCst)
    );
    assert_eq!(outcome.outcomes.len(), 8, "every candidate settled");
    assert_eq!(
        outcome.refusals().count(),
        0,
        "no candidate was refused by the read lane"
    );
    assert!(
        outcome.stats.slots_created <= HUB_SLOTS,
        "the TOTAL slot bound holds, built {}",
        outcome.stats.slots_created
    );
    assert!(
        outcome.stats.peak_active_slots <= HUB_SLOTS,
        "at most F candidates were ever in flight, peaked at {}",
        outcome.stats.peak_active_slots
    );
    assert_eq!(
        outcome.stats.peak_active_slots, HUB_SLOTS,
        "the fan-out filled its slot budget across the lanes"
    );
    assert!(
        outcome.stats.rounds_drained >= 1,
        "the parked cold reads were settled by drained rounds"
    );
    for index in 0..8 {
        let sequence = outcome
            .sequence(index)
            .unwrap_or_else(|| panic!("candidate {index} settled"));
        assert_eq!(
            sequence.calls[1].output,
            served_storage_word(),
            "candidate {index}: the captured step returns the hub's slot value"
        );
        assert_eq!(
            sequence.calls[2].native_balance,
            Some(U256::from(transferred(index))),
            "candidate {index}: the transfer it made is its own"
        );
    }
}

/// The moment a round is in flight: raised when the stub's transport receives
/// the round's packet, while the modeled round trip is still ahead of it.
#[derive(Debug, Default)]
struct FirstDispatch {
    raised: Mutex<bool>,
    announced: Condvar,
}

impl FirstDispatch {
    /// Announce the dispatch. Idempotent: every packet announces the same fact.
    fn raise(&self) {
        let mut raised = self
            .raised
            .lock()
            .expect("the dispatch flag is not poisoned");
        *raised = true;
        self.announced.notify_all();
    }

    /// Block until a round is in flight, giving up after a bounded wait so a
    /// broken ordering fails the assertions instead of hanging the suite.
    fn wait(&self) {
        let _outcome = self
            .announced
            .wait_timeout_while(
                self.raised
                    .lock()
                    .expect("the dispatch flag is not poisoned"),
                Rendezvous::BUDGET,
                |raised| !*raised,
            )
            .expect("the dispatch flag is not poisoned");
    }
}

/// One candidate's hub stack held back until a neighbour's round is in flight:
/// its reads register only once another round already owns the identities, so
/// this lane's own drain is the empty one the stall guard must coordinate.
struct HeldBackDb<'h> {
    inner: ReadHubDb<'h>,
    dispatch: Arc<FirstDispatch>,
    held: bool,
}

impl<'h> HeldBackDb<'h> {
    fn new(inner: ReadHubDb<'h>, dispatch: Arc<FirstDispatch>) -> Self {
        Self {
            inner,
            dispatch,
            held: false,
        }
    }

    /// Hold the FIRST read only: the walk's later reads follow its answers.
    fn hold(&mut self) {
        if self.held {
            return;
        }
        self.held = true;
        self.dispatch.wait();
    }
}

impl AsyncDatabase for HeldBackDb<'_> {
    type Error = ReadHubDbError;

    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send + '_ {
        self.hold();
        self.inner.get_account(address)
    }

    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<::evm2::bytecode::Bytecode, Self::Error>> + Send + '_ {
        self.hold();
        self.inner.get_code_by_hash(code_hash)
    }

    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + Send + '_ {
        self.hold();
        self.inner.get_storage(address, key)
    }

    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send + '_ {
        self.hold();
        self.inner.get_block_hash(number)
    }
}

/// The per-candidate state source whose candidate 1 is held back: both
/// candidates walk the same read identities, and candidate 1's first read
/// registers only after candidate 0's round is in flight.
struct OwnedReadSource<'h, 'p> {
    hub: &'h ReadHub,
    provider: &'p AlloyProvider,
    dispatch: Arc<FirstDispatch>,
}

impl<'h> CandidateStateSource<'h> for OwnedReadSource<'h, '_> {
    fn database(&self, candidate: usize) -> Box<dyn DynDatabase + 'h> {
        let inner = ReadHubDb::new(self.hub, self.provider);
        if candidate == 0 {
            Box::new(AsyncDb::new(inner))
        } else {
            Box::new(AsyncDb::new(HeldBackDb::new(
                inner,
                Arc::clone(&self.dispatch),
            )))
        }
    }
}

/// Candidate `index`'s work over the recipient BOTH candidates share: the
/// transfer values differ per candidate, but every read identity is common, so
/// one round can own both candidates' reads.
fn shared_read_candidate(index: usize) -> PreparedCandidate {
    PreparedCandidate::new(
        vec![
            Evm2SimStep::Transact(value_tx(recipient(0), transferred(index))),
            Evm2SimStep::ReadNativeBalance(recipient(0)),
        ],
        settlement_overrides(),
    )
}

/// Two lanes over one hub, both candidates walking the same read identities,
/// with candidate 1's first read registering only after candidate 0's round is
/// in flight. That neighbour's round therefore owns candidate 1's read: the
/// lane must wait for it (never spin into a false stall, never fetch the
/// identity a second time), and both candidates must settle with their own
/// results.
#[tokio::test]
async fn a_lane_whose_reads_a_neighbours_round_owns_waits_and_shares_one_fetch() {
    let dispatch = Arc::new(FirstDispatch::default());
    let stub = ChainStub::slow(ROUND_DELAY).announcing(Arc::clone(&dispatch));
    let provider = stub.provider();
    let hub = ReadHub::new(
        &provider,
        ReadAnchor::identified(BLOCK, B256::repeat_byte(0xaa)),
    );
    let candidates = vec![shared_read_candidate(0), shared_read_candidate(1)];
    let source = OwnedReadSource {
        hub: &hub,
        provider: &provider,
        dispatch,
    };

    let outcome = AsyncEvmFanout::new(FanoutLimits::new(2, 2), fanout_config())
        .with_drain_policy(DrainPolicy::PerLane)
        .run(&candidates, &source, Some(&hub))
        .await
        .expect("a lane whose read a neighbour's round owns waits instead of stalling");

    assert!(
        outcome.stats.slots_created <= 2,
        "the TOTAL slot bound holds, built {}",
        outcome.stats.slots_created
    );
    assert!(
        outcome.stats.peak_active_slots <= 2,
        "at most F candidates were ever in flight, peaked at {}",
        outcome.stats.peak_active_slots
    );
    for index in 0..2 {
        assert_candidate_identity(&outcome, index);
    }
    let stats = hub.stats();
    assert!(
        stats.coalesced >= 1,
        "the held-back read joined the neighbour's identity, got {stats:?}"
    );
    let fetched = stub
        .packets
        .lock()
        .expect("the packet log is not poisoned")
        .iter()
        .flatten()
        .filter(|method| *method == "eth_getBalance")
        .count();
    assert_eq!(
        fetched,
        usize::try_from(stats.account_members).expect("the member count fits a usize"),
        "each dispatched identity was fetched exactly once, whatever lane drained it"
    );
}

/// The stall guard's sensitivity: the same two-lane fixture with the
/// coordinated wait removed. Candidate 1's read is owned by candidate 0's
/// round, so its drains report empty while the read is being served — counted
/// as idle, that is the false [`FanoutError::Stalled`] the guard exists to
/// prevent, and the unguarded run must reproduce it.
#[tokio::test]
async fn uncoordinated_drains_reproduce_the_false_stall_the_guard_prevents() {
    let dispatch = Arc::new(FirstDispatch::default());
    let stub = ChainStub::slow(ROUND_DELAY).announcing(Arc::clone(&dispatch));
    let provider = stub.provider();
    let hub = ReadHub::new(
        &provider,
        ReadAnchor::identified(BLOCK, B256::repeat_byte(0xaa)),
    );
    let candidates = vec![shared_read_candidate(0), shared_read_candidate(1)];
    let source = OwnedReadSource {
        hub: &hub,
        provider: &provider,
        dispatch,
    };

    let outcome = AsyncEvmFanout::new(FanoutLimits::new(2, 2), fanout_config())
        .with_drain_policy(DrainPolicy::PerLaneUnguarded)
        .run(&candidates, &source, Some(&hub))
        .await;

    let Err(FanoutError::Stalled { waiting }) = outcome else {
        panic!("without the coordinated wait the neighbour's round reads as a stall");
    };
    assert_eq!(
        waiting,
        vec![1],
        "the false stall names the lane whose read the neighbour's round owns"
    );
}
