//! The chunk's RESOLUTION phase: the SQL-free read + reduce stage that turns
//! one chunk's candidate transactions into ordered canonical deltas.
//!
//! ChunkResolver consumes the chunk's ChunkSeed and its candidate evidence, and
//! performs every read the reduction needs through the seed and the hash-pinned
//! exact read views. It holds no Connection and issues no SQL: the seed is the
//! complete relational starting point, and a fact it does not hold is an
//! IncompleteChunkSeed refusal rather than a deferred query (ADR-105 D4).
//!
//! The resolver owns the chunk's two read memos — the block-pinned RevisionMemo
//! (shared with the reserve-identity acquisition that ran before the seed) and
//! the ParentAnchors map the pre-block discount observation reads through — so
//! one chunk issues one read per distinct (implementation, selector, anchor) and
//! per distinct parent position.

#![expect(clippy::doc_markdown)]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use degenbot_core::canonical_head::TxRef;
use degenbot_db::ScaledTokenPosition;
use degenbot_rpc::chain_evidence::BlockReads;
use degenbot_rpc::provider::AlloyProvider;

use super::evidence::CandidateTransaction;
use super::seed::ChunkSeed;
use super::substrate::ChunkSubstrate;
use super::{ConfigDispatchError, RunError};
use crate::config_dispatch::{ChunkContext, ChunkSpan, ParentAnchors, RevisionMemo};
use crate::projection::{CanonicalDelta, DeltaEffect, PositionSubject, ProjectionView};
use crate::transaction_processor::{MarketIdentity, OperationReducer, ProcessTxError};

/// The per-chunk resolution owner: the read memos the chunk's exact reads
/// share, plus the two stages that run before and around the seed.
#[derive(Debug, Default)]
pub(crate) struct ChunkResolver {
    memo: RevisionMemo,
    parent_anchors: ParentAnchors,
}

impl ChunkResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The chunk's block-pinned revision memo (populated by the reserve
    /// identity acquisition, consumed by the dispatch's own revision reads).
    pub(crate) fn memo_mut(&mut self) -> &mut RevisionMemo {
        &mut self.memo
    }

    /// Resolve one chunk: every per-transaction RPC and every relational read
    /// happens here, BEFORE the chunk's write transaction opens, and every
    /// relational fact comes from the seed.
    ///
    /// # Errors
    ///
    /// A dispatch/parse refusal, or an IncompleteChunkSeed when resolution names
    /// a fact the seed does not hold. A successful resolution leaves the chunk's
    /// deltas as the only product that crosses into persistence.
    #[expect(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn resolve(
        &mut self,
        provider: &AlloyProvider,
        seed: ChunkSeed,
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        candidates: &[CandidateTransaction<'_>],
        chunk_start: u64,
        chunk_end: u64,
    ) -> Result<ChunkResolution, RunError> {
        let mut view = ProjectionView::new();
        let mut substrate = seed.into_substrate();
        let mut touched_user_addresses: HashSet<Address> = HashSet::new();
        let mut chunk_deltas: Vec<CanonicalDelta> = Vec::new();
        let mut decode_compute_time = Duration::ZERO;
        let chunk_span = ChunkSpan::new(chunk_start, chunk_end);
        let memo = std::mem::take(&mut self.memo);
        let mut ctx = ChunkContext::for_chunk_with_memo(
            &chunk_span,
            provider.clone(),
            market_id,
            chain_id,
            pool_address,
            memo,
            &mut substrate,
        );
        ctx.substrate_mut()
            .observe_seed_generations(&mut view, market_id, chain_id);
        for candidate in candidates {
            let group = &candidate.group;
            let block_number = group.block_number;
            let tx_logs: Vec<&alloy::rpc::types::Log> = group.logs.clone();
            ctx.begin_transaction(*candidate.evidence.tx());
            for log in &group.logs {
                let topics = log.topics();
                if let Some(t1) = topics.get(1) {
                    touched_user_addresses.insert(Address::from_slice(&t1.as_slice()[12..]));
                }
                if let Some(t2) = topics.get(2) {
                    touched_user_addresses.insert(Address::from_slice(&t2.as_slice()[12..]));
                }
            }
            let compute_span = tracing::info_span!(
                "degenbot.updater.aave.compute",
                tx.block = block_number,
                rpc.round_trips = tracing::field::Empty,
            );
            let compute_rt_start = degenbot_rpc::provider::rpc_round_trips();
            let compute_guard = compute_span.enter();
            let compute_started = Instant::now();
            let dispatch = ctx
                .dispatch_transaction(&mut view, &tx_logs)
                .await
                .map_err(map_dispatch_error)?;
            let discounts = dispatch.discounts;
            let config_deltas = dispatch.deltas;
            chunk_deltas.extend(config_deltas.iter().cloned());
            let identity = MarketIdentity {
                market_id,
                chain_id,
                pool_address: ctx.pool_address(),
                gho_token_address: dispatch.gho_underlying_address,
                gho_vtoken_address: dispatch.gho_vtoken_address,
            };
            let deltas = OperationReducer::new(
                identity,
                *candidate.evidence.tx(),
                &discounts,
                ctx.substrate_mut(),
                &mut view,
            )
            .reduce_transaction(&candidate.evidence, &config_deltas)
            .map_err(|error| {
                report_parse_failure(*candidate.evidence.tx(), &error);
                map_process_error(error)
            })?;
            decode_compute_time += compute_started.elapsed();
            emit_transaction_trace(&view, &deltas, block_number, *candidate.evidence.tx());
            drop(compute_guard);
            compute_span.record(
                "rpc.round_trips",
                degenbot_rpc::provider::rpc_round_trips().saturating_sub(compute_rt_start),
            );
            chunk_deltas.extend(deltas.iter().cloned());
            for delta in &deltas {
                if let DeltaEffect::GhoRefreshDiscount { position_id } = delta.effect() {
                    let signal = delta.clone();
                    let anchor = *signal.source().tx().anchor();
                    let parent = self.parent_anchors.of(provider, anchor).await?;
                    resolve_discount_refresh(
                        &BlockReads::at(provider, parent),
                        &signal,
                        *position_id,
                        &view,
                        ctx.substrate_mut(),
                        &mut chunk_deltas,
                    )
                    .await?;
                }
            }
        }
        self.memo = ctx.take_memo();
        self.parent_anchors = ctx.take_parent_anchors();
        drop(ctx);
        let checkpoint = if crate::projection::checkpoint_due(chunk_deltas.len()) {
            Some(crate::projection::CheckpointState::capture(
                &view, &substrate,
            ))
        } else {
            None
        };
        Ok(ChunkResolution {
            deltas: chunk_deltas,
            substrate,
            touched_user_addresses,
            decode_compute_time,
            checkpoint,
        })
    }
}

/// One chunk's resolved write product: the ordered canonical deltas, the
/// substrate overlay resolution advanced (persistence applies against it), the
/// touched users, and the resolution's own stage time.
pub(super) struct ChunkResolution {
    pub(super) deltas: Vec<CanonicalDelta>,
    pub(super) substrate: ChunkSubstrate,
    pub(super) touched_user_addresses: HashSet<Address>,
    pub(super) decode_compute_time: Duration,
    /// The view's resume state when the cadence scheduled a checkpoint for this
    /// chunk's projection unit (ADR-105 D8, D9).
    pub(super) checkpoint: Option<crate::projection::CheckpointState>,
}

/// The `AAVE-TXTRACE` target: the closed `aave` domain, so an operator enables
/// this per-transaction trace without enabling any other debug stream.
const TXTRACE_TARGET: &str = degenbot_core::telemetry::domain::AAVE;

/// The positions a transaction's deltas moved, deduplicated in table order.
fn traced_subjects(deltas: &[CanonicalDelta]) -> Vec<PositionSubject> {
    let mut subjects: Vec<PositionSubject> = Vec::new();
    let mut push = |subject: PositionSubject| {
        if !subjects.contains(&subject) {
            subjects.push(subject);
        }
    };
    for delta in deltas {
        match delta.effect() {
            DeltaEffect::ScaledBalance { subject, .. } => push(*subject),
            DeltaEffect::ScaledTransfer { from, to, .. } => {
                push(*from);
                if let Some(to) = to {
                    push(*to);
                }
            }
            DeltaEffect::DebtPositionReset { position_id, .. } => push(PositionSubject {
                position: ScaledTokenPosition::Debt,
                position_id: *position_id,
            }),
            DeltaEffect::GhoRefreshDiscount { .. }
            | DeltaEffect::StkAaveBalanceObserved { .. }
            | DeltaEffect::GhoDiscountRefreshed { .. }
            | DeltaEffect::Configuration(_) => {}
        }
    }
    subjects.sort_unstable_by_key(|subject| (subject.position.table(), subject.position_id));
    subjects
}

/// Emit one `AAVE-TXTRACE` line per position the transaction moved, reading the
/// projection's POST-delta state: reduction advanced the view through every
/// delta the transaction emitted, so a position moved twice in one transaction
/// reports its settled end-of-transaction `(balance, last_index)`.
///
/// The gate is the diagnostic's OWN target, so enabling unrelated debug (a SQL
/// layer, another domain) never runs this scan.
fn emit_transaction_trace(
    view: &ProjectionView,
    deltas: &[CanonicalDelta],
    block_number: u64,
    tx: TxRef,
) {
    if !tracing::enabled!(target: TXTRACE_TARGET, tracing::Level::DEBUG) {
        return;
    }
    let tx_hash = alloy::hex::encode(tx.hash());
    for subject in traced_subjects(deltas) {
        let Some((balance, index)) = view.position_state(subject) else {
            continue;
        };
        degenbot_core::diag!(
            domain = aave,
            block = block_number,
            tx = %tx_hash,
            kind = %subject.position.table(),
            pos = subject.position_id,
            bal = %balance,
            idx = %index.map_or_else(String::new, |index| index.to_string()),
            "AAVE-TXTRACE"
        );
    }
}

/// Report a reduction refusal as one bounded `AAVE-PARSE-FAIL` event carrying the
/// exact block and transaction coordinates of the refused transaction: the
/// differential-narrowing consumer needs the position even when the error text
/// states only the operation's own refusal.
fn report_parse_failure(tx: TxRef, error: &ProcessTxError) {
    let anchor = tx.anchor();
    degenbot_core::op_error!(
        domain = aave,
        block = anchor.number(),
        block_hash = %alloy::hex::encode(anchor.hash()),
        tx = %alloy::hex::encode(tx.hash()),
        tx_index = tx.index(),
        err = %error,
        "AAVE-PARSE-FAIL"
    );
}

/// Recompute the users' GHO discounts the transaction's debt movements require
/// (the (C) refresh): the debt state is the view's post-transaction state and
/// the stkAAVE balance is the projection's. The settled values travel as their
/// own deltas, so persistence writes them without recomputing.
async fn resolve_discount_refresh(
    reads: &BlockReads<'_>,
    signal: &CanonicalDelta,
    position_id: i64,
    view: &ProjectionView,
    substrate: &mut ChunkSubstrate,
    chunk_deltas: &mut Vec<CanonicalDelta>,
) -> Result<(), RunError> {
    let discount_token = substrate
        .gho_asset()
        .and_then(|gho| gho.v_gho_discount_token)
        .and_then(|address| address.parse::<Address>().ok());
    let Some(discount_token) = discount_token else {
        return Ok(());
    };
    let user_id = substrate.position_user(degenbot_db::ScaledTokenPosition::Debt, position_id)?;
    let user_address = substrate.user_address(user_id)?;
    let (scaled_balance, last_index) = view
        .require_position_state(PositionSubject {
            position: degenbot_db::ScaledTokenPosition::Debt,
            position_id,
        })
        .map_err(|error| RunError::ProcessTx(ProcessTxError::Projection(error)))?;
    let projected = substrate.user_stk_aave_balance(&user_address)?;
    let stk_aave_balance = if let Some(Some(balance)) = projected {
        balance
    } else {
        let address: Address = user_address.parse().map_err(|_| {
            RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                "bad user_address in refresh: {user_address}"
            )))
        })?;
        let ret = reads
            .call(
                discount_token,
                crate::config_dispatch::encode_single_address_call("balanceOf(address)", &address),
            )
            .await
            .map_err(|error| RunError::ConfigDispatch(ConfigDispatchError::from(error)))?;
        let balance =
            crate::config_dispatch::require_word0_u256(&ret, "balanceOf(address)", &discount_token)
                .map_err(|error| {
                    RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                        "balanceOf for user {user_address}: {error}"
                    )))
                })?;
        substrate.record_user_stk_aave_balance_by_id(user_id, Some(balance));
        chunk_deltas.push(CanonicalDelta::new(
            signal.source(),
            None,
            DeltaEffect::StkAaveBalanceObserved { user_id, balance },
        ));
        balance
    };
    let debt_balance = crate::ray_mul(scaled_balance, last_index.unwrap_or_default(), None)
        .map_err(|error| {
            RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                "ray_mul overflow in refresh: {error}"
            )))
        })?;
    let rate = crate::gho_processor::calculate_gho_discount_rate(debt_balance, stk_aave_balance)
        .map_err(|error| {
            RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                "calculate_gho_discount_rate: {error}"
            )))
        })?;
    let gho_discount = crate::config_dispatch::discount_to_i64(rate);
    substrate.record_user_gho_discount_by_id(user_id, gho_discount);
    chunk_deltas.push(CanonicalDelta::new(
        signal.source(),
        None,
        DeltaEffect::GhoDiscountRefreshed {
            user_id,
            gho_discount,
        },
    ));
    Ok(())
}

/// Map a configuration-dispatch refusal onto the run's error.
fn map_dispatch_error(error: ConfigDispatchError) -> RunError {
    match error {
        ConfigDispatchError::IncompleteSeed(seed) => RunError::IncompleteChunkSeed(Box::new(seed)),
        ConfigDispatchError::Projection(error) => match error {
            crate::projection::ProjectionError::IncompleteSeed(seed) => {
                RunError::IncompleteChunkSeed(Box::new(seed))
            }
            other => RunError::ConfigDispatch(ConfigDispatchError::Projection(other)),
        },
        other => RunError::ConfigDispatch(other),
    }
}

/// Map a parse refusal onto the run's error.
fn map_process_error(error: ProcessTxError) -> RunError {
    match error {
        ProcessTxError::IncompleteSeed(seed) => RunError::IncompleteChunkSeed(Box::new(seed)),
        ProcessTxError::Projection(crate::projection::ProjectionError::IncompleteSeed(seed)) => {
            RunError::IncompleteChunkSeed(Box::new(seed))
        }
        other => RunError::ProcessTx(other),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U256};
    use degenbot_core::canonical_head::LogRef;
    use degenbot_db::sql_ledger::LedgerDb;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    /// The statement ledger is ONE process-global slot, and cargo runs this
    /// binary's tests as threads of one process: a one-mint-chunk test holds this
    /// guard for its whole ledger session instead of racing a sibling into
    /// `LedgerArmed`. A panic inside one test poisons the mutex; a sibling still
    /// runs rather than failing for the first one's abort.
    fn ledger_session() -> std::sync::MutexGuard<'static, ()> {
        static LEDGER_SESSION: Mutex<()> = Mutex::new(());
        LEDGER_SESSION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The one-mint chunk's fixture: a REAL candidate set (a scaled-token mint on
    /// this market's aToken and a collateral-configuration change on its
    /// underlying) over the production seed, resolved while the connection guard
    /// stays in the test's hand.
    #[expect(clippy::too_many_lines)]
    fn with_one_mint_chunk<R>(
        run: impl for<'a> FnOnce(
            &LedgerDb,
            &mut ChunkResolver,
            &AlloyProvider,
            ChunkSeed,
            &[CandidateTransaction<'a>],
        ) -> R,
    ) -> R {
        use degenbot_core::canonical_head::ChainAnchor;
        use degenbot_rpc::tx_evidence::ReceiptTransaction;
        use rusqlite::params;
        let _session = ledger_session();
        let (ledger, _state) = LedgerDb::open_for_writes(Path::new(":memory:")).unwrap();
        let a_token = Address::repeat_byte(0x32);
        let underlying = Address::repeat_byte(0x31);
        let user = Address::repeat_byte(0x41);
        {
            let conn = ledger.db().lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'm', 1, 99)",
                [],
            )
            .unwrap();
            for (id, address) in [
                (1_i64, underlying),
                (2, a_token),
                (3, Address::repeat_byte(0x33)),
            ] {
                conn.execute(
                    "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                    params![
                        id,
                        degenbot_core::address_utils::address_to_checksum_string(&address)
                    ],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO aave_v3_assets \
                (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                 v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                 borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '0', '0')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
             VALUES (1, 'POOL', ?1, 1)",
                params![degenbot_core::address_utils::address_to_checksum_string(
                    &Address::repeat_byte(0x11)
                )],
            )
            .unwrap();
        }
        let block_hash = alloy::primitives::B256::repeat_byte(0xaa);
        let parent_hash = alloy::primitives::B256::repeat_byte(0x99);
        let tx_hash = alloy::primitives::B256::repeat_byte(0xbb);
        let word = |value: alloy::primitives::U256| value.to_be_bytes::<32>();
        let ray = alloy::primitives::U256::from(10).pow(alloy::primitives::U256::from(27));
        let mut mint_data = Vec::new();
        mint_data.extend_from_slice(&word(alloy::primitives::U256::from(1_000)));
        mint_data.extend_from_slice(&word(alloy::primitives::U256::ZERO));
        mint_data.extend_from_slice(&word(ray));
        let mut config_data = Vec::new();
        config_data.extend_from_slice(&word(alloy::primitives::U256::from(8_000)));
        config_data.extend_from_slice(&word(alloy::primitives::U256::from(8_500)));
        config_data.extend_from_slice(&word(alloy::primitives::U256::from(1_050)));
        let mint = alloy::rpc::types::Log {
            inner: alloy::primitives::Log::new_unchecked(
                a_token,
                vec![
                    degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC,
                    alloy::primitives::B256::left_padding_from(user.as_slice()),
                    alloy::primitives::B256::left_padding_from(user.as_slice()),
                ],
                mint_data.into(),
            ),
            block_hash: Some(block_hash),
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: Some(tx_hash),
            transaction_index: Some(0),
            log_index: Some(7),
            removed: false,
        };
        let configuration = alloy::rpc::types::Log {
            inner: alloy::primitives::Log::new_unchecked(
                Address::repeat_byte(0x11),
                vec![
                degenbot_decoders::aave_event_decoder::AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC,
                alloy::primitives::B256::left_padding_from(underlying.as_slice()),
            ],
                config_data.into(),
            ),
            block_hash: Some(block_hash),
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: Some(tx_hash),
            transaction_index: Some(0),
            log_index: Some(9),
            removed: false,
        };
        let logs = vec![mint, configuration];
        let log_refs: Vec<&alloy::rpc::types::Log> = logs.iter().collect();
        // The collateral-configuration dispatch reads the reserve's price source
        // from the pool at the event's own block (the only chain read this chunk
        // makes; every relational fact is the seed's).
        let configuration_selector: [u8; 4] =
            alloy::primitives::keccak256(b"getConfiguration(address)")[..4]
                .try_into()
                .expect("a selector is four bytes");
        let chain = crate::updater::test_chain::ScriptedChain::new().answer(
            Address::repeat_byte(0x11),
            configuration_selector,
            crate::updater::test_chain::Answer::Return(alloy::primitives::Bytes::copy_from_slice(
                &alloy::primitives::B256::ZERO.0,
            )),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let conn = ledger.db().lock();
        let seed = ChunkSeed::load(&conn, 1, 1, &log_refs, &[]).expect("the seed loads");
        let groups =
            crate::updater::run::process::group_logs_by_tx(&logs).expect("the logs are one group");
        let anchor =
            ChainAnchor::new(1, 100, block_hash, parent_hash).expect("the anchor is complete");
        let tx = groups[0]
            .tx_ref(anchor)
            .expect("the group names its transaction");
        let receipt =
            ReceiptTransaction::new(tx, logs.clone()).expect("the receipt binds the logs");
        let evidence =
            crate::updater::evidence::bind_transaction_evidence(&log_refs, receipt, None)
                .expect("a receipt-only transaction binds");
        let candidates = vec![super::super::evidence::CandidateTransaction {
            group: groups.into_iter().next().unwrap(),
            evidence,
        }];
        let mut resolver = ChunkResolver::new();
        run(&ledger, &mut resolver, &provider, seed, &candidates)
    }

    /// Resolution is a function of the seed and the exact read views: the real
    /// chunk resolves to a non-empty delta stream while the connection stays in
    /// the test's own hand. Every relational fact those paths read (the asset
    /// classification, the user and its position, the pool revision) comes from
    /// the production seed, so the statement ledger records nothing between the
    /// seed load and the resolution's return. A deferred lookup would be a
    /// statement here, or a deadlock against the guard.
    #[test]
    fn a_real_chunk_resolves_without_sql() {
        let (resolution, before, after) =
            with_one_mint_chunk(|ledger, resolver, provider, seed, candidates| {
                let before = ledger.records().unwrap().len();
                let resolution = degenbot_core::runtime::get_runtime()
                    .block_on(resolver.resolve(
                        provider,
                        seed,
                        1,
                        1,
                        Address::repeat_byte(0x11),
                        candidates,
                        100,
                        100,
                    ))
                    .expect("a seeded chunk resolves without a connection");
                let after = ledger.records().unwrap().len();
                (resolution, before, after)
            });
        assert_eq!(
            after, before,
            "resolution issued no SQL: the guard stayed in the test's hand"
        );
        assert!(
            resolution.deltas.iter().any(|delta| matches!(
                delta.effect(),
                crate::projection::DeltaEffect::ScaledBalance { .. }
            )),
            "the mint reduced to a scaled-balance delta: the operations path ran"
        );
        assert!(
            resolution.deltas.iter().any(|delta| matches!(
                delta.effect(),
                crate::projection::DeltaEffect::Configuration(_)
            )),
            "the configuration change reduced to a configuration delta: the config path ran"
        );
    }

    /// The transaction the diagnostic tests state their coordinates under.
    fn trace_fixture_tx(index: u64) -> TxRef {
        let anchor = degenbot_core::canonical_head::ChainAnchor::new(
            1,
            100,
            alloy::primitives::B256::repeat_byte(0xaa),
            alloy::primitives::B256::repeat_byte(0x99),
        )
        .expect("the fixture anchor is complete");
        TxRef::new(anchor, alloy::primitives::B256::repeat_byte(0xbb), index)
            .expect("the fixture transaction names its position")
    }

    /// One captured diagnostic event: its level, target, and rendered fields.
    #[derive(Debug)]
    struct CapturedEvent {
        level: Level,
        target: String,
        fields: Vec<(String, String)>,
    }

    impl CapturedEvent {
        fn message(&self) -> &str {
            self.field("message")
        }

        fn field(&self, name: &str) -> &str {
            self.fields
                .iter()
                .find(|(field, _)| field == name)
                .map_or("", |(_, value)| value.as_str())
        }
    }

    struct FieldCapture<'a>(&'a mut Vec<(String, String)>);

    impl Visit for FieldCapture<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
    }

    /// A subscriber that records ONLY `target`'s debug-or-worse events, so a test
    /// can prove both the emitted payload and the stream's target scoping.
    struct TargetCapture {
        target: &'static str,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl Subscriber for TargetCapture {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == self.target && *metadata.level() <= Level::DEBUG
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = Vec::new();
            event.record(&mut FieldCapture(&mut fields));
            self.events.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                fields,
            });
        }
    }

    /// Run `emit` under a subscriber that enables `target` alone.
    fn capturing(target: &'static str, emit: impl FnOnce()) -> Vec<CapturedEvent> {
        let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::default();
        let subscriber = TargetCapture {
            target,
            events: Arc::clone(&events),
        };
        tracing::subscriber::with_default(subscriber, emit);
        Arc::try_unwrap(events)
            .expect("the capture is the sole owner")
            .into_inner()
            .expect("the capture lock is unpoisoned")
    }

    /// One collateral position seeded at 1,000 and moved twice inside ONE
    /// transaction: the trace must state the settled end-of-transaction state,
    /// never the seed or the first delta's intermediate value.
    #[test]
    fn txtrace_reports_a_positions_same_transaction_updates() {
        let tx = trace_fixture_tx(3);
        let subject = PositionSubject {
            position: ScaledTokenPosition::Collateral,
            position_id: 7,
        };
        let mut view = ProjectionView::new();
        view.seed_position(subject, (U256::from(1_000), Some(U256::from(1))));
        let deltas = vec![
            CanonicalDelta::new(
                LogRef::new(tx, 0),
                None,
                DeltaEffect::ScaledBalance {
                    subject,
                    balance_delta: I256::try_from(500_i64).expect("500 fits a signed word"),
                    index: U256::from(2),
                },
            ),
            CanonicalDelta::new(
                LogRef::new(tx, 1),
                None,
                DeltaEffect::ScaledBalance {
                    subject,
                    balance_delta: I256::try_from(-400_i64).expect("-400 fits a signed word"),
                    index: U256::from(3),
                },
            ),
        ];
        for delta in &deltas {
            view.apply(delta)
                .expect("the fixture deltas advance the view");
        }

        let events = capturing(TXTRACE_TARGET, || {
            emit_transaction_trace(&view, &deltas, 100, tx);
        });

        assert_eq!(events.len(), 1, "one line for one moved position");
        let trace = &events[0];
        assert_eq!(trace.message(), "AAVE-TXTRACE");
        assert_eq!(trace.field("kind"), "aave_v3_collateral_positions");
        assert_eq!(trace.field("pos"), "7");
        assert_eq!(
            trace.field("bal"),
            "1100",
            "the settled state (1000 + 500 - 400), not the seed's 1000 or the first delta's 1500"
        );
        assert_eq!(trace.field("idx"), "3");
    }

    /// The trace reads the projection's POST-delta state for a real chunk: the
    /// mint's scaled balance is what the transaction established, not the zero
    /// balance the seed read.
    #[test]
    fn txtrace_reports_the_state_the_transaction_settled() {
        let (events, deltas) =
            with_one_mint_chunk(|_ledger, resolver, provider, seed, candidates| {
                let capture = Arc::new(Mutex::new(Vec::new()));
                let subscriber = TargetCapture {
                    target: TXTRACE_TARGET,
                    events: Arc::clone(&capture),
                };
                let resolution = tracing::subscriber::with_default(subscriber, || {
                    degenbot_core::runtime::get_runtime()
                        .block_on(resolver.resolve(
                            provider,
                            seed,
                            1,
                            1,
                            Address::repeat_byte(0x11),
                            candidates,
                            100,
                            100,
                        ))
                        .expect("a seeded chunk resolves without a connection")
                });
                let events = Arc::try_unwrap(capture)
                    .expect("the capture is the sole owner")
                    .into_inner()
                    .expect("the capture lock is unpoisoned");
                (events, resolution.deltas)
            });

        let mint = deltas
            .iter()
            .find_map(|delta| match delta.effect() {
                DeltaEffect::ScaledBalance { balance_delta, .. } => Some(*balance_delta),
                _ => None,
            })
            .expect("the mint reduced to a scaled-balance delta");
        assert_eq!(events.len(), 1, "one line for the one moved position");
        let trace = &events[0];
        assert_eq!(trace.message(), "AAVE-TXTRACE");
        assert_eq!(trace.level, Level::DEBUG);
        assert_eq!(trace.target, TXTRACE_TARGET);
        assert_eq!(trace.field("block"), "100");
        assert_eq!(
            trace.field("bal"),
            mint.to_string(),
            "the trace states the delta's settled balance, not the seed's zero"
        );
    }

    /// An operator who enabled debug for an unrelated target (a SQL layer) must
    /// not cause the aave trace to run: the gate is the diagnostic's own target.
    #[test]
    fn txtrace_stays_dark_when_only_an_unrelated_target_is_enabled() {
        let tx = trace_fixture_tx(0);
        let subject = PositionSubject {
            position: ScaledTokenPosition::Debt,
            position_id: 9,
        };
        let mut view = ProjectionView::new();
        view.seed_position(subject, (U256::from(1), Some(U256::from(1))));
        let delta = CanonicalDelta::new(
            LogRef::new(tx, 0),
            None,
            DeltaEffect::DebtPositionReset {
                position_id: 9,
                index: U256::from(2),
            },
        );
        view.apply(&delta)
            .expect("the fixture delta advances the view");
        let deltas = vec![delta];

        let events = capturing("degenbot::db", || {
            emit_transaction_trace(&view, &deltas, 100, tx);
        });
        assert!(
            events.is_empty(),
            "another target's debug level never runs the aave trace"
        );
    }

    /// A parse refusal states the exact block and transaction coordinates as
    /// structured fields, without a stderr write.
    #[test]
    fn parse_failures_state_the_exact_block_and_transaction() {
        let tx = trace_fixture_tx(3);
        let error = ProcessTxError::InvalidOperation(
            "Expected at least 1 collateral event (burn or transfer)".to_string(),
        );

        let events = capturing(TXTRACE_TARGET, || {
            report_parse_failure(tx, &error);
        });

        assert_eq!(events.len(), 1, "one bounded event per refusal");
        let failure = &events[0];
        assert_eq!(failure.level, Level::ERROR);
        assert_eq!(failure.target, TXTRACE_TARGET);
        assert_eq!(failure.message(), "AAVE-PARSE-FAIL");
        assert_eq!(failure.field("block"), "100");
        assert_eq!(
            failure.field("block_hash"),
            alloy::hex::encode(tx.anchor().hash())
        );
        assert_eq!(failure.field("tx"), alloy::hex::encode(tx.hash()));
        assert_eq!(failure.field("tx_index"), "3");
        assert!(
            failure.field("err").contains("collateral event"),
            "the refusal payload rides the event: {}",
            failure.field("err")
        );
    }
}
