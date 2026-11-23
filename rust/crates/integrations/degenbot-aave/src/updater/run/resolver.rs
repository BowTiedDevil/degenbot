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
use degenbot_rpc::chain_evidence::BlockReads;
use degenbot_rpc::provider::AlloyProvider;

use super::evidence::CandidateTransaction;
use super::seed::ChunkSeed;
use super::substrate::ChunkSubstrate;
use super::{ConfigDispatchError, RunError};
use crate::config_dispatch::{ChunkContext, ChunkSpan, ParentAnchors, RevisionMemo};
use crate::projection::{CanonicalDelta, DeltaEffect, ProjectionView};
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
        let mut pending_refreshes: Vec<(CanonicalDelta, i64)> = Vec::new();
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
                #[expect(clippy::print_stderr)]
                {
                    eprintln!(
                        "AAVE-PARSE-FAIL block={block_number} tx=0x{} err={error}",
                        alloy::hex::encode(group.tx_hash)
                    );
                }
                map_process_error(error)
            })?;
            decode_compute_time += compute_started.elapsed();
            drop(compute_guard);
            compute_span.record(
                "rpc.round_trips",
                degenbot_rpc::provider::rpc_round_trips().saturating_sub(compute_rt_start),
            );
            chunk_deltas.extend(deltas.iter().cloned());
            for delta in &deltas {
                if let DeltaEffect::GhoRefreshDiscount { position_id } = delta.effect() {
                    pending_refreshes.push((delta.clone(), *position_id));
                }
            }
        }
        self.memo = ctx.take_memo();
        self.parent_anchors = ctx.take_parent_anchors();
        drop(ctx);
        for (signal, position_id) in pending_refreshes {
            let anchor = *signal.source().tx().anchor();
            let parent = self.parent_anchors.of(provider, anchor).await?;
            resolve_discount_refresh(
                &BlockReads::at(provider, parent),
                &signal,
                position_id,
                &view,
                &mut substrate,
                &mut chunk_deltas,
            )
            .await?;
        }
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
/// projection the resolution left behind (persistence reads its overlay), the
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
    use crate::projection::PositionSubject;
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
    use degenbot_db::sql_ledger::LedgerDb;
    use std::path::Path;

    /// Resolution is a function of the seed and the exact read views: a chunk with
    /// REAL candidate logs — a scaled-token mint on this market's aToken and a
    /// collateral-configuration change on its underlying — resolves to a non-empty
    /// delta stream while the connection stays in the test's own hand. Every
    /// relational fact those paths read (the asset classification, the user and its
    /// position, the pool revision) comes from the production seed, so the statement
    /// ledger records nothing between the seed load and the resolution's return. A
    /// deferred lookup would be a statement here, or a deadlock against the guard.
    #[test]
    #[expect(clippy::too_many_lines)]
    fn a_real_chunk_resolves_without_sql() {
        use degenbot_core::canonical_head::ChainAnchor;
        use degenbot_rpc::tx_evidence::ReceiptTransaction;
        use rusqlite::params;
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
        let before = ledger.records().unwrap().len();
        let resolution = degenbot_core::runtime::get_runtime()
            .block_on(ChunkResolver::new().resolve(
                &provider,
                seed,
                1,
                1,
                Address::repeat_byte(0x11),
                &candidates,
                100,
                100,
            ))
            .expect("a seeded chunk resolves without a connection");
        let after = ledger.records().unwrap().len();
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
}
