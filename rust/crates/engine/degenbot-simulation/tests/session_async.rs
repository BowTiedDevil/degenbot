//! The async sequence verb at the public session seam
//! (`Evm2SimSession::run_sequence_async`): a delayed async database proves the
//! verb really suspends the ordered sequence on its reads, and a worked
//! sequence's facts are asserted independently of the sync verb AND against it,
//! so the two drivers cannot drift.
//!
//! The fixture contract is the sync session tests' dual-mode bytecode extended
//! with a log-emitting branch: empty calldata stores `0x2A` at its own slot 5;
//! non-empty calldata commits a V2 `Swap` log plus a non-swap log, then returns
//! slot 5's current value.

#![expect(clippy::expect_used)]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::r#async::{AsyncDatabase, AsyncDb};
use ::evm2::evm::{AccountInfo, DynDatabase, InMemoryDB};
use ::evm2::interpreter::Word;
use alloy::primitives::{address, keccak256, Address, Bytes, TxKind, B256, U256};
use degenbot_simulation::sim::evm2::build_block_evm2;
use degenbot_simulation::sim::evm2::session::{
    Evm2CaptureMode, Evm2SimError, Evm2SimRun, Evm2SimSequence, Evm2SimSession, Evm2SimStep,
};
use degenbot_simulation::sim::RunVerdict;

const SENDER: Address = address!("0x3333333333333333333333333333333333333333");
const DUAL: Address = address!("0x4444444444444444444444444444444444444444");
const COLD: Address = address!("0x5555555555555555555555555555555555555555");

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_GWEI: u128 = 1_000_000_000;
const SENDER_START: u64 = 1_000_000_000_000_000_000;

/// The never-touched account's seeded balance: the figure a cold read reports,
/// and the pre-transfer figure an overlay read must exceed.
const COLD_START: u64 = 3_000_000_000_000_000_000;

/// The value a sequence step transfers to `COLD`.
const VALUE: u64 = 500_000_000_000_000_000;

/// The fixture contract's write slot, and the value its write branch stores.
const SLOT: u8 = 5;
const STORED: u8 = 0x2A;

/// The V2 `Swap` amounts the emit branch commits: 500 of token1 paid in, 1000
/// of token0 paid out — the signed-delta convention the capture reports.
const AMOUNT1_IN: u16 = 500;
const AMOUNT0_OUT: u16 = 1_000;

/// The jump target of the calldata-empty branch (its `JUMPDEST`).
const EMIT_BRANCH: u8 = 0x0A;

/// `keccak256("Swap(address,uint256,uint256,uint256,uint256,address)")`,
/// derived here rather than imported from the decoder table: the expected swap
/// is an independent fact, not a restatement of the capture's own constant.
fn v2_swap_topic() -> B256 {
    keccak256("Swap(address,uint256,uint256,uint256,uint256,address)")
}

/// The non-swap log's topic — `keccak256("Ping()")` — so a captured log that is
/// not a swap is observable as such.
fn ping_topic() -> B256 {
    keccak256("Ping()")
}

/// The two-branch fixture contract (see the module doc).
fn fixture_code() -> Bytes {
    let mut code: Vec<u8> = Vec::new();
    // CALLDATASIZE; PUSH1 <emit branch>; JUMPI (calldata non-empty -> emit).
    code.extend_from_slice(&[0x36, 0x60, EMIT_BRANCH, 0x57]);
    // Write branch: SSTORE slot 5 = 0x2A; STOP.
    code.extend_from_slice(&[0x60, STORED, 0x60, SLOT, 0x55, 0x00]);
    assert_eq!(
        code.len(),
        usize::from(EMIT_BRANCH),
        "the jump target must land on the JUMPDEST below"
    );
    // JUMPDEST, then mem[0x20] = amount1_in (word 0, amount0_in = 0, stays
    // zero).
    code.push(0x5B);
    code.push(0x61);
    code.extend_from_slice(&AMOUNT1_IN.to_be_bytes());
    code.extend_from_slice(&[0x60, 0x20, 0x52]);
    // mem[0x40] = amount0_out (word 3, amount1_out = 0, stays zero).
    code.push(0x61);
    code.extend_from_slice(&AMOUNT0_OUT.to_be_bytes());
    code.extend_from_slice(&[0x60, 0x40, 0x52]);
    // LOG3(0, 0x80, ..): topics pushed deepest-first — `to`, then the indexed
    // `sender`, then the signature. The emitting address is this contract.
    code.push(0x7F);
    code.extend_from_slice(DUAL.into_word().as_slice());
    code.push(0x7F);
    code.extend_from_slice(SENDER.into_word().as_slice());
    code.push(0x7F);
    code.extend_from_slice(v2_swap_topic().as_slice());
    code.extend_from_slice(&[0x60, 0x80, 0x60, 0x00, 0xA3]);
    // A non-swap log: LOG1(0, 0, Ping()).
    code.push(0x7F);
    code.extend_from_slice(ping_topic().as_slice());
    code.extend_from_slice(&[0x60, 0x00, 0x60, 0x00, 0xA1]);
    // Return slot 5 (the write branch's value, when an earlier step wrote it).
    code.extend_from_slice(&[0x60, SLOT, 0x54]); // PUSH1 slot; SLOAD
    code.extend_from_slice(&[0x60, 0x00, 0x52]); // MSTORE 0
    code.extend_from_slice(&[0x60, 0x20, 0x60, 0x00, 0xF3]); // RETURN 32 bytes
    Bytes::from(code)
}

/// The fixture state both drivers read: a funded sender and the fixture code.
fn seeded() -> InMemoryDB {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &SENDER,
        AccountInfo::default().with_balance(U256::from(SENDER_START)),
    );
    db.insert_account_info(
        &COLD,
        AccountInfo::default().with_balance(U256::from(COLD_START)),
    );
    db.insert_account_info(
        &DUAL,
        AccountInfo::default().with_code(Bytecode::new_raw(fixture_code())),
    );
    db
}

/// An envelope to the fixture contract (empty calldata writes, non-empty
/// calldata emits and reads).
fn fixture_tx(data: &[u8]) -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(DUAL),
            input: Bytes::copy_from_slice(data),
            gas_limit: 200_000,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// An envelope that transfers `value` wei to a codeless account.
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

/// The envelope the engine refuses (a gas limit below the intrinsic floor).
fn refused_tx() -> ::evm2::ethereum::RecoveredTxEnvelope {
    use ::evm2::ethereum::TxEnvelope;
    use alloy::consensus::transaction::Recovered;
    use alloy::consensus::TxLegacy;
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(DUAL),
            input: Bytes::copy_from_slice(&[0x00]),
            gas_limit: 100,
            gas_price: BASE_FEE_GWEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// The worked sequence both drivers run: a write step, the balance it left
/// behind, a captured step (logs + a swap + a read of the accumulated write),
/// the balance that step left behind, and a third account's balance.
fn worked_sequence() -> Vec<Evm2SimStep> {
    vec![
        Evm2SimStep::Transact(fixture_tx(&[])),
        Evm2SimStep::ReadNativeBalance(SENDER),
        Evm2SimStep::Inspect(fixture_tx(&[0x00])),
        Evm2SimStep::ReadNativeBalance(SENDER),
        Evm2SimStep::ReadNativeBalance(DUAL),
    ]
}

/// A future that reports `Pending` once before completing — the delay that
/// proves the EVM fiber suspended instead of blocking the task.
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

/// The async database every read of the async driver goes through, delayed by
/// one suspension per read.
struct DelayedDb {
    inner: InMemoryDB,
    suspensions: Arc<AtomicUsize>,
}

impl DelayedDb {
    fn new(inner: InMemoryDB, suspensions: Arc<AtomicUsize>) -> Self {
        Self { inner, suspensions }
    }

    /// Suspend once, counting the suspension.
    async fn delay(&self) {
        self.suspensions.fetch_add(1, Ordering::Relaxed);
        YieldOnce { yielded: false }.await;
    }
}

impl AsyncDatabase for DelayedDb {
    type Error = ::evm2::DatabaseError;

    fn is_fatal(error: &Self::Error) -> bool {
        error.is_fatal()
    }

    async fn get_account(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.delay().await;
        DynDatabase::get_account(&mut self.inner, &address)
    }

    async fn get_code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.delay().await;
        DynDatabase::get_code_by_hash(&mut self.inner, &code_hash)
    }

    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        self.delay().await;
        DynDatabase::get_storage(&mut self.inner, &address, &key)
    }

    async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
        self.delay().await;
        DynDatabase::get_block_hash(&mut self.inner, &number)
    }
}

/// The 32-byte word a call returned (big-endian, right-aligned).
fn returned_word(output: &Bytes) -> U256 {
    U256::from_be_slice(&output[output.len().saturating_sub(32)..])
}

/// The worked sequence's step facts, asserted independently of either run: the
/// write's slot/value, the accumulated read, and the gas each step cost the
/// sender.
fn assert_step_facts(sequence: &Evm2SimSequence) {
    let fee = U256::from(BASE_FEE_GWEI);
    assert_eq!(sequence.calls.len(), 5, "every step's result is returned");
    assert_eq!(
        sequence.storage_writes,
        vec![(DUAL, U256::from(SLOT), U256::from(STORED))],
        "the settled state carries exactly the write branch's SSTORE"
    );
    assert_eq!(sequence.calls[0].verdict, RunVerdict::Success);
    assert_eq!(sequence.calls[0].native_balance, None);
    assert!(
        sequence.calls[0].gas_used > 21_000,
        "the write step paid intrinsic gas plus the SSTORE"
    );
    assert_eq!(sequence.calls[2].verdict, RunVerdict::Success);
    assert_eq!(
        returned_word(&sequence.calls[2].output),
        U256::from(STORED),
        "the captured step reads the write step's accumulated slot"
    );

    // The sender's balance moves by exactly the gas each step burned.
    let gas_after_write = U256::from(sequence.calls[0].gas_used);
    let gas_after_captured = U256::from(sequence.calls[2].gas_used);
    assert_eq!(
        sequence.calls[1].native_balance,
        Some(U256::from(SENDER_START) - gas_after_write * fee),
        "the first balance step observes the write step's gas payment"
    );
    assert_eq!(sequence.calls[1].gas_used, 0);
    assert_eq!(sequence.calls[1].output, Bytes::new());
    assert_eq!(
        sequence.calls[3].native_balance,
        Some(U256::from(SENDER_START) - (gas_after_write + gas_after_captured) * fee),
        "the second balance step observes both steps' gas payments"
    );
    assert_eq!(
        sequence.calls[4].native_balance,
        Some(U256::ZERO),
        "the fixture contract was never funded"
    );
}

/// The worked sequence's capture facts, asserted independently of either run:
/// two committed logs (one of them a V2 swap), the swap's decoded amounts, and
/// the access list the `Seam` mechanism drains.
fn assert_capture_facts(sequence: &Evm2SimSequence) {
    assert_eq!(sequence.capture.log_count, 2, "both committed logs count");
    assert_eq!(sequence.capture.logs.len(), 2);
    assert_eq!(sequence.capture.logs[0].address, DUAL);
    assert_eq!(sequence.capture.logs[0].topics().len(), 3);
    assert_eq!(sequence.capture.logs[0].topics()[0], v2_swap_topic());
    assert_eq!(
        sequence.capture.logs[0].topics()[1],
        SENDER.into_word(),
        "the swap's indexed sender"
    );
    assert_eq!(
        sequence.capture.logs[0].topics()[2],
        DUAL.into_word(),
        "the swap's indexed recipient"
    );
    assert_eq!(
        sequence.capture.logs[0].data.data.as_ref(),
        [
            U256::ZERO.to_be_bytes::<32>(),
            U256::from(AMOUNT1_IN).to_be_bytes::<32>(),
            U256::from(AMOUNT0_OUT).to_be_bytes::<32>(),
            U256::ZERO.to_be_bytes::<32>(),
        ]
        .concat()
        .as_slice(),
        "the swap's in/out amounts"
    );
    assert_eq!(sequence.capture.logs[1].topics(), [ping_topic()]);

    assert_eq!(sequence.capture.swaps.len(), 1);
    let swap = &sequence.capture.swaps[0];
    assert_eq!(swap.emitter, DUAL);
    assert_eq!(
        swap.amount0,
        alloy::primitives::I256::try_from(i64::from(AMOUNT0_OUT)).expect("amount fits i256"),
        "token0 was received (positive delta)"
    );
    assert_eq!(
        swap.amount1,
        -alloy::primitives::I256::try_from(i64::from(AMOUNT1_IN)).expect("amount fits i256"),
        "token1 was paid in (negative delta)"
    );
    assert!(
        sequence.capture.reverted_swaps.is_empty(),
        "the seam capture has no reverted-frame source"
    );
    assert!(
        sequence.capture.trace.frames.is_empty(),
        "the seam capture has no call-trace source"
    );
    assert!(
        sequence
            .capture
            .access_list
            .0
            .iter()
            .any(|item| item.address == DUAL
                && item.storage_keys.contains(&B256::from(U256::from(SLOT)))),
        "the captured step's slot reached the drained access list"
    );
}

/// One step's settled result, field by field — the per-step parity claim, with
/// the step index in every failure message.
fn assert_step_parity(sync_run: &Evm2SimRun, async_run: &Evm2SimRun, index: usize) {
    assert_eq!(sync_run.verdict, async_run.verdict, "step {index}: verdict");
    assert_eq!(
        sync_run.gas_used, async_run.gas_used,
        "step {index}: gas used"
    );
    assert_eq!(sync_run.output, async_run.output, "step {index}: output");
    assert_eq!(
        sync_run.native_balance, async_run.native_balance,
        "step {index}: native balance read"
    );
    let mut sync_writes = sync_run.storage_writes.clone();
    let mut async_writes = async_run.storage_writes.clone();
    sync_writes.sort();
    async_writes.sort();
    assert_eq!(sync_writes, async_writes, "step {index}: storage writes");
}

/// The two drivers' settled sequences, compared field by field.
fn assert_driver_parity(sync: &Evm2SimSequence, async_sequence: &Evm2SimSequence) {
    assert_eq!(sync.calls.len(), async_sequence.calls.len());
    for (index, (sync_run, async_run)) in sync
        .calls
        .iter()
        .zip(async_sequence.calls.iter())
        .enumerate()
    {
        assert_step_parity(sync_run, async_run, index);
    }
    assert_eq!(
        sync.storage_writes, async_sequence.storage_writes,
        "the accumulated writes are the same"
    );
    assert_eq!(
        sync.capture.logs, async_sequence.capture.logs,
        "the captured logs are the same"
    );
    assert_eq!(
        sync.capture.swaps, async_sequence.capture.swaps,
        "the captured swaps are the same"
    );
    assert_eq!(
        sync.capture.log_count, async_sequence.capture.log_count,
        "the log-hook count is the same"
    );
    assert_eq!(
        sync.capture.access_list, async_sequence.capture.access_list,
        "the drained access list is the same"
    );
}

/// The worked sequence settles identically on both drivers, and both agree with
/// facts asserted independently of either run (see [`assert_step_facts`] and
/// [`assert_capture_facts`]).
#[tokio::test]
async fn the_async_verb_settles_the_sync_result() {
    let steps = worked_sequence();
    let sync = {
        let mut evm = build_block_evm2(seeded(), BASE_FEE_GWEI, BLOCK, TIMESTAMP);
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence(&steps, Evm2CaptureMode::Seam)
            .expect("the sync sequence settles")
    };

    let suspensions = Arc::new(AtomicUsize::new(0));
    let async_sequence = {
        let mut evm = build_block_evm2(
            AsyncDb::new(DelayedDb::new(seeded(), Arc::clone(&suspensions))),
            BASE_FEE_GWEI,
            BLOCK,
            TIMESTAMP,
        );
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence_async(&steps, Evm2CaptureMode::Seam)
            .await
            .expect("the async sequence settles")
    };

    assert!(
        suspensions.load(Ordering::Relaxed) > 0,
        "the async verb suspended the sequence on its database reads"
    );
    assert_step_facts(&sync);
    assert_step_facts(&async_sequence);
    assert_capture_facts(&sync);
    assert_capture_facts(&async_sequence);
    assert_driver_parity(&sync, &async_sequence);
}

/// A refused step reports the same step index (and the same refusal) on both
/// drivers.
#[tokio::test]
async fn both_drivers_report_the_same_refused_step_index() {
    let steps = vec![
        Evm2SimStep::Transact(fixture_tx(&[])),
        Evm2SimStep::Inspect(refused_tx()),
    ];
    let sync_refusal = {
        let mut evm = build_block_evm2(seeded(), BASE_FEE_GWEI, BLOCK, TIMESTAMP);
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence(&steps, Evm2CaptureMode::Seam)
            .expect_err("a refused step fails the sync sequence")
    };
    let async_refusal = {
        let mut evm = build_block_evm2(
            AsyncDb::new(DelayedDb::new(seeded(), Arc::new(AtomicUsize::new(0)))),
            BASE_FEE_GWEI,
            BLOCK,
            TIMESTAMP,
        );
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence_async(&steps, Evm2CaptureMode::Seam)
            .await
            .expect_err("a refused step fails the async sequence")
    };

    assert_eq!(
        sync_refusal.step_index(),
        Some(1),
        "the error carries the refusing step's index"
    );
    assert_eq!(
        async_refusal.step_index(),
        Some(1),
        "the async verb maps the refusal the same way"
    );
    assert_eq!(
        sync_refusal.to_string(),
        async_refusal.to_string(),
        "the same refusal reads the same on both drivers"
    );
}

/// A balance read of an account the sequence's state has never seen is a cold
/// database round trip: on the async verb it SUSPENDS the sequence — the first
/// poll of the returned future is `Pending`, before any transaction step runs —
/// and resolves to the database's own figure. The figure is asserted from the
/// seeded fact rather than from either run, and the synchronous verb (which
/// blocks for the same read) must agree field by field.
#[tokio::test]
async fn a_cold_balance_read_suspends_and_matches_the_sync_value() {
    let steps = [Evm2SimStep::ReadNativeBalance(COLD)];

    let sync = {
        let mut evm = build_block_evm2(seeded(), BASE_FEE_GWEI, BLOCK, TIMESTAMP);
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence(&steps, Evm2CaptureMode::Seam)
            .expect("the synchronous read forwards to the database")
    };
    assert_eq!(
        sync.calls[0].native_balance,
        Some(U256::from(COLD_START)),
        "the synchronous read reports the database's own figure"
    );

    let suspensions = Arc::new(AtomicUsize::new(0));
    let mut evm = build_block_evm2(
        AsyncDb::new(DelayedDb::new(seeded(), Arc::clone(&suspensions))),
        BASE_FEE_GWEI,
        BLOCK,
        TIMESTAMP,
    );
    let mut session = Evm2SimSession::new(&mut evm);
    let mut future = std::pin::pin!(session.run_sequence_async(&steps, Evm2CaptureMode::Seam));
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    assert!(
        matches!(future.as_mut().poll(&mut cx), Poll::Pending),
        "the cold balance-read step suspends the sequence on its database read"
    );
    let async_sequence = future.await.expect("the async balance read resolves");

    assert!(
        suspensions.load(Ordering::Relaxed) > 0,
        "the read really reached the async database"
    );
    assert_eq!(
        async_sequence.calls[0].native_balance,
        Some(U256::from(COLD_START)),
        "the suspended read resolves to the database's own figure"
    );
    assert_driver_parity(&sync, &async_sequence);
}

/// A balance read after an earlier step moved value observes the ACCUMULATING
/// accepted overlay, not the database it falls through to: the credited account
/// reads the seeded figure plus the transfer on both drivers, while the seeded
/// database still holds only the pre-transfer figure (the sequence's writes died
/// with its throwaway EVM). Overlay precedence is the property under test — a
/// read answered from the bare database would report the stale figure here.
#[tokio::test]
async fn a_balance_read_after_a_value_move_observes_the_accepted_overlay() {
    let steps = [
        Evm2SimStep::ReadNativeBalance(COLD),
        Evm2SimStep::Transact(value_tx(COLD, VALUE)),
        Evm2SimStep::ReadNativeBalance(COLD),
    ];

    let sync = {
        let mut evm = build_block_evm2(seeded(), BASE_FEE_GWEI, BLOCK, TIMESTAMP);
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence(&steps, Evm2CaptureMode::Seam)
            .expect("the synchronous sequence settles")
    };
    let async_sequence = {
        let mut evm = build_block_evm2(
            AsyncDb::new(DelayedDb::new(seeded(), Arc::new(AtomicUsize::new(0)))),
            BASE_FEE_GWEI,
            BLOCK,
            TIMESTAMP,
        );
        let mut session = Evm2SimSession::new(&mut evm);
        session
            .run_sequence_async(&steps, Evm2CaptureMode::Seam)
            .await
            .expect("the async sequence settles")
    };

    let seeded_figure = Some(U256::from(COLD_START));
    let credited = Some(U256::from(COLD_START) + U256::from(VALUE));
    assert_eq!(
        sync.calls[0].native_balance, seeded_figure,
        "the cold read reports the database's figure"
    );
    assert_eq!(
        sync.calls[2].native_balance, credited,
        "the synchronous read observes the earlier step's transfer"
    );
    assert_eq!(
        async_sequence.calls[0].native_balance, seeded_figure,
        "the async cold read reports the database's figure"
    );
    assert_eq!(
        async_sequence.calls[2].native_balance, credited,
        "the async read observes the earlier step's transfer"
    );

    // The database itself never moved: the credited figure came from the overlay.
    let mut probe = seeded();
    let database_figure = DynDatabase::get_account(&mut probe, &COLD)
        .expect("the seeded database answers")
        .map(|info| info.balance);
    assert_eq!(
        database_figure, seeded_figure,
        "a sequence's writes never reach the shared database"
    );
    assert_ne!(
        async_sequence.calls[2].native_balance, database_figure,
        "the async read answered from the accepted overlay, not the bare database"
    );
    assert_driver_parity(&sync, &async_sequence);
}

/// The fiber entrypoint the balance step rides, pinned directly: the same
/// untracked account-info read that fails outside a fiber on an `AsyncDb`-backed
/// stack answers on the entrypoint, suspending first. The vendored crate's own
/// in-crate test modules are not compiled here (`rust/vendor/README.md`: test
/// targets off, our parity suites are the behavioral gate), so this suite is
/// where the entrypoint's suspension is executable.
#[tokio::test]
async fn the_account_info_async_entrypoint_suspends_on_a_cold_read() {
    let suspensions = Arc::new(AtomicUsize::new(0));
    let mut evm = build_block_evm2(
        AsyncDb::new(DelayedDb::new(seeded(), Arc::clone(&suspensions))),
        BASE_FEE_GWEI,
        BLOCK,
        TIMESTAMP,
    );

    // The entrypoint's future holds the EVM borrow until it drops, so the
    // suspension and its resolution share one scope.
    let info = {
        let mut future = std::pin::pin!(evm.read_account_info_async(COLD));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(
            matches!(future.as_mut().poll(&mut cx), Poll::Pending),
            "the cold account read suspends the fiber"
        );
        future
            .await
            .expect("the fibered read resolves")
            .expect("the seeded account exists")
    };
    assert_eq!(info.balance, U256::from(COLD_START));
    assert!(
        suspensions.load(Ordering::Relaxed) > 0,
        "the read reached the async database"
    );

    // A synchronous read of a cold account cannot poll the adapter at all: the
    // fiber, not a warmed cache entry, is what makes a cold read answerable.
    // (The fibered read above warmed its own account's cache entry, so the
    // contrast uses an account nothing has read.)
    let untouched = Address::repeat_byte(0x99);
    let refusal = evm
        .read_account_info(&untouched)
        .expect_err("a synchronous read cannot suspend on an async database");
    assert!(
        refusal.to_string().contains("async fiber"),
        "unexpected refusal: {refusal}"
    );
}

/// `Inspector` capture is refused explicitly: evm2's async entrypoint has no
/// inspector counterpart, so honoring the mode would mean silently capturing by
/// another mechanism. Nothing runs, so the session's view is untouched.
#[tokio::test]
async fn the_async_verb_refuses_the_inspector_capture_mode() {
    let mut evm = build_block_evm2(seeded(), BASE_FEE_GWEI, BLOCK, TIMESTAMP);
    let mut session = Evm2SimSession::new(&mut evm);
    let refusal = session
        .run_sequence_async(
            &[Evm2SimStep::Inspect(fixture_tx(&[0x00]))],
            Evm2CaptureMode::Inspector,
        )
        .await
        .expect_err("the async verb cannot carry an inspector");
    assert!(
        matches!(
            refusal,
            Evm2SimError::AsyncInspectorUnsupported {
                mode: Evm2CaptureMode::Inspector
            }
        ),
        "unexpected refusal: {refusal}"
    );
    assert_eq!(
        refusal.step_index(),
        None,
        "the refusal is the verb's, not a step's"
    );

    // The refusal ran nothing: the follow-up read still observes the pre-state.
    let after = session
        .run_sequence(
            &[Evm2SimStep::Transact(fixture_tx(&[0x00]))],
            Evm2CaptureMode::Seam,
        )
        .expect("the follow-up sequence settles");
    assert_eq!(
        returned_word(&after.calls[0].output),
        U256::ZERO,
        "the refused async call left no state behind"
    );
}
