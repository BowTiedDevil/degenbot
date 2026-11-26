//! The transaction-scoped resolution sequencer (ADR-105 D4).
//!
//! One `TransactionResolution` owns one log-transaction's whole resolution
//! sequence: entering the transaction's read position, emitting the exact-block
//! observations its configuration resolution made, projecting each
//! configuration effect onto the chunk substrate (so a later event of the same
//! transaction resolves against it), reducing the configuration and operation
//! families against the advancing `ProjectionView` in canonical event order,
//! sealing the generations a replacement displaced, repointing the POOL at a
//! rotation, resolving the follow-up requests the transaction's journaled
//! signals raise, and emitting one ordered delta stream. Nothing here spans
//! transactions, so no chunk-wide phase can reorder work across transaction
//! boundaries.
//!
//! The sequencer holds no `Connection` and issues no SQL: every relational fact
//! is the chunk seed's, every chain read is a hash-pinned `BlockReads` at either
//! the transaction's own anchor or its proven parent, and persistence is
//! downstream.
//!
//! A `ResolutionRequest` is the sequencer's one follow-up seam. A journaled
//! signal states what happened; the request states what resolution still owes
//! and carries every input composition must not re-derive — the GHO discount
//! refresh carries the triggering effect's own `U256` index, never an optional
//! index or a zero default.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::{LogRef, TxRef};
use degenbot_db::ScaledTokenPosition;
use degenbot_rpc::chain_evidence::BlockReads;
use degenbot_rpc::provider::AlloyProvider;
use futures_util::stream::{self, StreamExt};

use super::evidence::CandidateTransaction;
use super::{ConfigDispatchError, RunError};
use crate::config_dispatch::{resolve_config_log, ChunkContext, ParentAnchors};
use crate::projection::{
    reduce_configuration, CanonicalDelta, DeltaEffect, PositionSubject, ProjectionView,
};
use crate::transaction_processor::{MarketIdentity, OperationReducer, ProcessTxError};

/// One follow-up resolution a transaction's journaled signal requires.
///
/// The journaled signal stays what it is: a `CanonicalDelta` whose effect and
/// encoding are unchanged. The request is the resolution that signal obliges,
/// stated with its own inputs, so the sequencer never infers a trigger's facts
/// from the view's settled end-of-transaction state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolutionRequest {
    /// Recompute one user's GHO discount from the position the transaction's
    /// debt movements settled. `index` is the triggering effect's own index —
    /// the `U256` that effect reconciled its movement to — so composition never
    /// receives an optional index or a zero default.
    RefreshGhoDiscount {
        /// The journaled signal's exact position; the recomputation's own
        /// deltas travel at it.
        source: LogRef,
        /// The debt position the triggering movement settled.
        position_id: i64,
        /// The triggering effect's index.
        index: U256,
    },
}

/// One transaction's reduction: its ordered canonical deltas and the follow-up
/// resolutions its journaled signals raised, in the same order.
#[derive(Debug, Default)]
pub(crate) struct TransactionReduction {
    pub(crate) deltas: Vec<CanonicalDelta>,
    pub(crate) requests: Vec<ResolutionRequest>,
}

/// One transaction's resolution product: the ordered canonical deltas and the
/// resolution stage time the chunk reports (the configuration dispatch and the
/// operation reduction; the follow-up reads are measured separately, as they
/// always were).
pub(crate) struct ResolvedTransaction {
    pub(crate) deltas: Vec<CanonicalDelta>,
    pub(crate) decode_compute_time: Duration,
}

/// The transaction-scoped sequencer: one transaction's resolution, from its read
/// position to its ordered deltas.
pub(crate) struct TransactionResolution<'a, 'span> {
    ctx: &'a mut ChunkContext<'span>,
    view: &'a mut ProjectionView,
    market_id: i64,
    chain_id: i64,
    tx: TxRef,
    deltas: Vec<CanonicalDelta>,
}

impl<'a, 'span> TransactionResolution<'a, 'span> {
    /// Open one transaction's sequence: the exact transaction every read and
    /// every emitted delta of this sequence rides.
    pub(crate) fn for_transaction(
        ctx: &'a mut ChunkContext<'span>,
        view: &'a mut ProjectionView,
        market_id: i64,
        chain_id: i64,
        tx: TxRef,
    ) -> Self {
        ctx.begin_transaction(tx);
        Self {
            ctx,
            view,
            market_id,
            chain_id,
            tx,
            deltas: Vec::new(),
        }
    }

    /// The configuration stage: resolve every configuration event of the
    /// transaction and apply it at its own position, in canonical log order.
    ///
    /// Per event: the exact-block observations it resolved are recorded on the
    /// substrate and emitted first, the effect is projected onto the substrate
    /// (so a later event's resolution reads this one's overlay), the generation
    /// it displaced is sealed, the POOL endpoint is repointed at a rotation, and
    /// the effect is reduced into its positioned delta carrying the generation it
    /// names.
    ///
    /// The deltas are NOT advanced onto the view here: the operation reduction
    /// advances configuration and operation deltas together in one canonical
    /// event order, so an operation ahead of a configuration event never reads
    /// the state that event establishes.
    ///
    /// # Errors
    ///
    /// A resolution refusal (an exact read or a seed hole), a substrate
    /// projection failure, or the projection refusal of an effect the view
    /// cannot interpret under the generation it names.
    pub(crate) async fn apply_configuration(
        &mut self,
        tx_logs: &[&Log],
    ) -> Result<Vec<CanonicalDelta>, RunError> {
        let mut deltas: Vec<CanonicalDelta> = Vec::new();
        for log in tx_logs {
            let Some(event) = resolve_config_log(self.ctx, log)
                .await
                .map_err(map_dispatch_error)?
            else {
                continue;
            };
            for (user_id, balance) in &event.observations {
                self.ctx
                    .substrate_mut()
                    .record_user_stk_aave_balance_by_id(*user_id, Some(*balance));
                deltas.push(CanonicalDelta::new(
                    event.source,
                    None,
                    DeltaEffect::StkAaveBalanceObserved {
                        user_id: *user_id,
                        balance: *balance,
                    },
                ));
            }
            event
                .effect
                .project(self.ctx.substrate_mut())
                .map_err(config_projection_error)?;
            if let Some(superseded) = event.superseded {
                self.view.seal_generation(superseded);
            }
            self.ctx
                .repoint_pool(&event.effect)
                .map_err(config_substrate_error)?;
            deltas.push(
                reduce_configuration(self.view, event.effect, event.source, event.generation)
                    .map_err(config_projection_error)?,
            );
        }
        Ok(deltas)
    }

    /// Resolve one candidate transaction end to end.
    ///
    /// # Errors
    ///
    /// A dispatch refusal (an exact read, a seed hole, a projection refusal), a
    /// reduction refusal (mapped onto the run's taxonomy after the
    /// `AAVE-PARSE-FAIL` diagnostic states its coordinates), or a follow-up read
    /// the exact parent position cannot serve.
    pub(super) async fn resolve(
        mut self,
        provider: &AlloyProvider,
        parent_anchors: &mut ParentAnchors,
        candidate: &CandidateTransaction<'_>,
    ) -> Result<ResolvedTransaction, RunError> {
        let tx_logs: Vec<&Log> = candidate.group.logs.clone();
        let compute_span = tracing::info_span!(
            "degenbot.updater.aave.compute",
            tx.block = self.tx.anchor().number(),
            rpc.round_trips = tracing::field::Empty,
        );
        let compute_rt_start = degenbot_rpc::provider::rpc_round_trips();
        let compute_guard = compute_span.enter();
        let compute_started = Instant::now();
        let prelude = self
            .ctx
            .begin_dispatch(&tx_logs)
            .await
            .map_err(map_dispatch_error)?;
        let configuration_deltas = self.apply_configuration(&tx_logs).await?;
        self.deltas.extend(configuration_deltas.iter().cloned());
        let identity = self.ctx.resolve_operation_identity();
        let market = MarketIdentity {
            market_id: self.market_id,
            chain_id: self.chain_id,
            pool_address: self.ctx.pool_address(),
            gho_token_address: identity.gho_underlying_address,
            gho_vtoken_address: identity.gho_vtoken_address,
        };
        let reduction = OperationReducer::new(
            market,
            self.tx,
            &prelude.discounts,
            self.ctx.substrate_mut(),
            self.view,
        )
        .reduce_transaction(&candidate.evidence, &configuration_deltas)
        .map_err(|error| {
            report_parse_failure(self.tx, &error);
            map_process_error(error)
        })?;
        let decode_compute_time = compute_started.elapsed();
        drop(compute_guard);
        compute_span.record(
            "rpc.round_trips",
            degenbot_rpc::provider::rpc_round_trips().saturating_sub(compute_rt_start),
        );
        self.deltas.extend(reduction.deltas);
        self.resolve_requests(reduction.requests, provider, parent_anchors)
            .await?;
        Ok(ResolvedTransaction {
            deltas: self.deltas,
            decode_compute_time,
        })
    }

    /// Resolve the follow-up requests one transaction's journaled signals raised.
    ///
    /// This is the ONE GHO-discount refresh: the lookup each request obliges, the
    /// bounded prefetch of the independent parent-block reads those lookups need,
    /// the `ray_mul` -> `calculate_gho_discount_rate` -> `discount_to_i64`
    /// composition, and the ordered observation/refresh emission. Production and
    /// the refresh tests drive this same body.
    ///
    /// Every request is planned, prefetched, and applied inside the transaction
    /// that raised it, and the plan reads the `ProjectionView` before any of this
    /// transaction's reads lands. A prefetched balance is therefore consumed only
    /// with the event-boundary view its own transaction settled: no plan reads a
    /// later transaction's state, and no later transaction can consume this one's
    /// read.
    ///
    /// # Errors
    ///
    /// A parent-anchor refusal, a seed hole for the position or user a request
    /// names, a projection refusal for a position the view never resolved, an
    /// exact read failure, a short `balanceOf` return, or a composition the pure
    /// calculator refuses.
    async fn resolve_requests(
        &mut self,
        requests: Vec<ResolutionRequest>,
        provider: &AlloyProvider,
        parent_anchors: &mut ParentAnchors,
    ) -> Result<(), RunError> {
        if requests.is_empty() {
            return Ok(());
        }
        // The parent position is resolved before the discount-token check:
        // every refresh signal has always paid (and memoized) it, so a market
        // without a discount token keeps the same read shape.
        let parent = parent_anchors.of(provider, *self.tx.anchor()).await?;
        let Some(discount_token) = self.discount_token() else {
            return Ok(());
        };
        let reads = BlockReads::at(provider, parent);
        let mut plans: Vec<RefreshPlan> = Vec::with_capacity(requests.len());
        let mut read_users: Vec<Address> = Vec::new();
        let mut read_slots: HashMap<Address, usize> = HashMap::new();
        for request in requests {
            let ResolutionRequest::RefreshGhoDiscount {
                source,
                position_id,
                index,
            } = request;
            let user_id = self
                .ctx
                .substrate_mut()
                .position_user(ScaledTokenPosition::Debt, position_id)?;
            let user_address = self.ctx.substrate_mut().user_address(user_id)?;
            let (scaled_balance, _) = self
                .view
                .require_position_state(PositionSubject {
                    position: ScaledTokenPosition::Debt,
                    position_id,
                })
                .map_err(|error| RunError::ProcessTx(ProcessTxError::Projection(error)))?;
            let projected = self
                .ctx
                .substrate_mut()
                .user_stk_aave_balance(&user_address)?;
            let stk_aave = if let Some(Some(balance)) = projected {
                StkAaveSettlement::Projected(balance)
            } else {
                let address: Address = user_address.parse().map_err(|_| {
                    RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                        "bad user_address in refresh: {user_address}"
                    )))
                })?;
                let slot = *read_slots.entry(address).or_insert_with(|| {
                    let slot = read_users.len();
                    read_users.push(address);
                    slot
                });
                StkAaveSettlement::ParentRead(slot)
            };
            plans.push(RefreshPlan {
                source,
                user_id,
                scaled_balance,
                index,
                stk_aave,
            });
        }
        let mut prefetched =
            prefetch_stk_aave_balances(&reads, discount_token, &read_users).await?;
        for plan in plans {
            self.apply_refresh(plan, &mut prefetched)?;
        }
        Ok(())
    }

    /// The canonical GHO-discount refresh: compose one settled request into the
    /// user's discount and emit the deltas it settles.
    ///
    /// The debt balance is the view's post-apply scaled balance at the triggering
    /// effect's own `U256` index (`ray_mul`), the rate is the pure
    /// `calculate_gho_discount_rate`, and the stored value is that rate narrowed
    /// by `discount_to_i64`. A parent-block balance the prefetch read is recorded
    /// on the substrate and emitted as its own observation before the discount
    /// that consumed it, so persistence writes exactly what composition read.
    ///
    /// # Errors
    ///
    /// A seed hole for the user the position names, a `ray_mul` overflow, or a
    /// rate the pure calculator refuses.
    fn apply_refresh(
        &mut self,
        plan: RefreshPlan,
        prefetched: &mut PrefetchedBalances,
    ) -> Result<(), RunError> {
        let RefreshPlan {
            source,
            user_id,
            scaled_balance,
            index,
            stk_aave,
        } = plan;
        let stk_aave_balance = match stk_aave {
            StkAaveSettlement::Projected(balance) => balance,
            StkAaveSettlement::ParentRead(slot) => {
                let (balance, newly_observed) = prefetched.consume(slot)?;
                if newly_observed {
                    self.ctx
                        .substrate_mut()
                        .record_user_stk_aave_balance_by_id(user_id, Some(balance));
                    self.deltas.push(CanonicalDelta::new(
                        source,
                        None,
                        DeltaEffect::StkAaveBalanceObserved { user_id, balance },
                    ));
                }
                balance
            }
        };
        let debt_balance = crate::ray_mul(scaled_balance, index, None).map_err(|error| {
            RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                "ray_mul overflow in refresh: {error}"
            )))
        })?;
        let rate =
            crate::gho_processor::calculate_gho_discount_rate(debt_balance, stk_aave_balance)
                .map_err(|error| {
                    RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                        "calculate_gho_discount_rate: {error}"
                    )))
                })?;
        let gho_discount = crate::config_dispatch::discount_to_i64(rate);
        self.ctx
            .substrate_mut()
            .record_user_gho_discount_by_id(user_id, gho_discount);
        self.deltas.push(CanonicalDelta::new(
            source,
            None,
            DeltaEffect::GhoDiscountRefreshed {
                user_id,
                gho_discount,
            },
        ));
        Ok(())
    }

    /// The discount token the chunk's GHO asset names, if any. A row without one
    /// leaves the refresh with nothing to read against, as it always has.
    fn discount_token(&mut self) -> Option<Address> {
        self.ctx
            .gho_asset()
            .and_then(|gho| gho.v_gho_discount_token)
            .and_then(|address| address.parse::<Address>().ok())
    }
}

/// The bounded width of one refresh's independent read pass.
const MAX_CONCURRENT_REFRESH_READS: usize = 8;

/// One request's settled lookup, taken at its own event boundary before any of
/// the transaction's reads lands.
struct RefreshPlan {
    source: LogRef,
    user_id: i64,
    scaled_balance: U256,
    index: U256,
    stk_aave: StkAaveSettlement,
}

/// Where one request's stkAAVE balance comes from.
enum StkAaveSettlement {
    /// The substrate already projects the user's balance: no read is owed.
    Projected(U256),
    /// The transaction's prefetched parent-block read at `slot`.
    ParentRead(usize),
}

/// One prefetched parent-block stkAAVE balance, with the observation its first
/// consumer owns.
struct PrefetchedBalance {
    balance: U256,
    observed: bool,
}

/// The parent-block stkAAVE balances one transaction prefetched, one per distinct
/// user, in the order the reads were issued.
#[derive(Default)]
struct PrefetchedBalances(Vec<PrefetchedBalance>);

impl PrefetchedBalances {
    /// The balance a request's slot holds, and whether this request is the one
    /// that must emit the observation. Two requests naming one user share the
    /// read — the value is the same parent-block balance — so only its first
    /// consumer observes it, exactly as the sequential pass emitted it once.
    ///
    /// # Errors
    ///
    /// A slot the prefetch never filled, which no plan can name.
    fn consume(&mut self, slot: usize) -> Result<(U256, bool), ConfigDispatchError> {
        let Some(entry) = self.0.get_mut(slot) else {
            return Err(ConfigDispatchError::DecodeShape(format!(
                "refresh read slot {slot} holds no prefetched balance"
            )));
        };
        let newly_observed = !entry.observed;
        entry.observed = true;
        Ok((entry.balance, newly_observed))
    }
}

/// Prefetch one transaction's independent stkAAVE `balanceOf` reads at its exact
/// parent block.
///
/// Each read is a distinct user against one target at one anchor, and no read's
/// answer gates another, so the concurrent pass issues exactly the requests one
/// sequential loop would — one hash-pinned `eth_call` per distinct user, the
/// corpus shape the recorder already carries — with at most
/// [`MAX_CONCURRENT_REFRESH_READS`] in flight. Results stay in the users' own
/// order, so the caller applies them in canonical request order. A fresh user in
/// a transaction with one refresh therefore pays one read and has nothing to
/// batch.
///
/// # Errors
///
/// The failed read's own class, or the short-word refusal naming the read, its
/// target, and the user it was issued for. A failing read refuses the pass before
/// any of the transaction's refreshes apply.
async fn prefetch_stk_aave_balances(
    reads: &BlockReads<'_>,
    token: Address,
    users: &[Address],
) -> Result<PrefetchedBalances, ConfigDispatchError> {
    let calls = users.iter().map(|user| async move {
        reads
            .call(
                token,
                crate::config_dispatch::encode_single_address_call("balanceOf(address)", user),
            )
            .await
    });
    let returns = stream::iter(calls)
        .buffered(MAX_CONCURRENT_REFRESH_READS)
        .collect::<Vec<_>>()
        .await;
    let mut balances = Vec::with_capacity(returns.len());
    for (user, ret) in users.iter().zip(returns) {
        let ret = ret.map_err(ConfigDispatchError::from)?;
        let balance =
            crate::config_dispatch::require_word0_u256(&ret, "balanceOf(address)", &token)
                .map_err(|error| {
                    ConfigDispatchError::DecodeShape(format!("balanceOf for user {user}: {error}"))
                })?;
        balances.push(PrefetchedBalance {
            balance,
            observed: false,
        });
    }
    Ok(PrefetchedBalances(balances))
}
/// A configuration effect the projection refuses, as the run's error.
fn config_projection_error(error: crate::projection::ProjectionError) -> RunError {
    RunError::ConfigDispatch(ConfigDispatchError::Projection(error))
}

/// A substrate projection failure, as the run's error.
fn config_substrate_error(error: degenbot_db::DbError) -> RunError {
    RunError::ConfigDispatch(ConfigDispatchError::Substrate(error))
}

/// Report a reduction refusal as one bounded `AAVE-PARSE-FAIL` event carrying
/// the exact block and transaction coordinates of the refused transaction.
pub(super) fn report_parse_failure(tx: TxRef, error: &ProcessTxError) {
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

/// Map a reduction refusal onto the run's error.
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
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config_dispatch::ChunkSpan;
    use crate::updater::run::substrate::ChunkSubstrate;
    use crate::updater::test_chain::{
        Answer, Issued, ScriptedChain, HEAD_HASH, HEAD_NUMBER, HEAD_PARENT,
    };
    use alloy::primitives::{Bytes, B256};
    use degenbot_core::canonical_head::ChainAnchor;
    use degenbot_db::DegenbotDb;
    use rusqlite::params;
    use std::future::Future;
    use std::path::Path;
    use std::task::{Context, Waker};

    const MARKET_ID: i64 = 1;
    const CHAIN_ID: i64 = 1;

    fn whole_range_span() -> &'static ChunkSpan {
        use std::sync::OnceLock;
        static SPAN: OnceLock<ChunkSpan> = OnceLock::new();
        SPAN.get_or_init(|| ChunkSpan::new(0, u64::MAX))
    }

    fn ray() -> U256 {
        U256::from(10).pow(U256::from(27))
    }

    fn checksum(address: Address) -> String {
        degenbot_core::address_utils::address_to_checksum_string(&address)
    }

    fn balance_selector() -> [u8; 4] {
        alloy::primitives::keccak256("balanceOf(address)").0[..4]
            .try_into()
            .expect("a selector is four bytes")
    }

    /// The fixture transaction at the scripted head block: the one read position
    /// the scripted chain answers pinned calls at, in addition to its parent.
    fn head_tx() -> TxRef {
        let anchor = ChainAnchor::new(1, HEAD_NUMBER, HEAD_HASH, HEAD_PARENT)
            .expect("the fixture anchor is complete");
        TxRef::new(anchor, B256::repeat_byte(0x77), 0)
            .expect("the fixture transaction names itself")
    }

    /// One fixture transaction at `block` whose parent is `parent`.
    fn tx_at(block: u64, hash: B256, parent: B256) -> TxRef {
        let anchor = ChainAnchor::new(1, block, hash, parent).expect("the anchor is complete");
        TxRef::new(anchor, B256::from(U256::from(block)), 0)
            .expect("the transaction names its position")
    }

    fn refresh_request(
        tx: TxRef,
        log_index: u64,
        position_id: i64,
        index: U256,
    ) -> ResolutionRequest {
        ResolutionRequest::RefreshGhoDiscount {
            source: LogRef::new(tx, log_index),
            position_id,
            index,
        }
    }

    /// One refresh fixture: a market with its GHO row, one debt position per user,
    /// and the discount token every read targets.
    struct RefreshFixture {
        substrate: ChunkSubstrate,
        users: Vec<(Address, i64)>,
        positions: Vec<i64>,
        discount_token: Address,
    }

    fn refresh_fixture(users: &[Address]) -> RefreshFixture {
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let conn = db.lock();
        let discount_token = Address::repeat_byte(0xd1);
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
             VALUES (1, 1, 'm', 1, NULL)",
            [],
        )
        .unwrap();
        for (id, address) in [
            (1_i64, Address::repeat_byte(0x31)),
            (2, Address::repeat_byte(0x32)),
            (3, Address::repeat_byte(0x33)),
            (10, Address::repeat_byte(0x10)),
            (11, discount_token),
        ] {
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?1, 1, ?2)",
                params![id, checksum(address)],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO aave_v3_assets \
             (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
              v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
              borrow_index, borrow_rate) \
             VALUES (1, 1, 1, 2, 1, 3, 2, '0', '0', '0', '0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, v_gho_discount_token) \
             VALUES (1, 10, 3, ?1)",
            params![checksum(discount_token)],
        )
        .unwrap();
        let mut identities: Vec<(Address, i64, i64)> = Vec::new();
        for user in users {
            conn.execute(
                "INSERT INTO aave_v3_users \
                 (market_id, address, e_mode, gho_discount, isolation_mode_debt) \
                 VALUES (1, ?1, 0, 0, '0')",
                params![checksum(*user)],
            )
            .unwrap();
            let user_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) \
                 VALUES (?1, 1, '0', '1000000000000000000000000000')",
                params![user_id],
            )
            .unwrap();
            identities.push((*user, user_id, conn.last_insert_rowid()));
        }
        let substrate = crate::updater::run::seed::ChunkSeed::for_tests_users(&conn, 1, 1, users)
            .into_substrate();
        drop(conn);
        RefreshFixture {
            substrate,
            users: identities
                .iter()
                .map(|(user, id, _)| (*user, *id))
                .collect(),
            positions: identities.iter().map(|(_, _, id)| *id).collect(),
            discount_token,
        }
    }

    /// Seed the projection view with the states the transaction settled: each
    /// position's post-apply scaled balance at the index its movement carried.
    fn seed_positions(view: &mut ProjectionView, settled: &[(i64, U256, U256)]) {
        for (position_id, scaled_balance, index) in settled {
            view.seed_position(
                PositionSubject {
                    position: ScaledTokenPosition::Debt,
                    position_id: *position_id,
                },
                (*scaled_balance, Some(*index)),
            );
        }
    }

    /// Drive one transaction's refresh pass at its own event boundary.
    fn refresh_in_boundary(
        substrate: &mut ChunkSubstrate,
        view: &mut ProjectionView,
        provider: &AlloyProvider,
        tx: TxRef,
        requests: Vec<ResolutionRequest>,
    ) -> Result<Vec<CanonicalDelta>, RunError> {
        let mut ctx = ChunkContext::for_chunk(
            whole_range_span(),
            provider.clone(),
            MARKET_ID,
            CHAIN_ID,
            Address::ZERO,
            substrate,
        );
        let mut resolution =
            TransactionResolution::for_transaction(&mut ctx, view, MARKET_ID, CHAIN_ID, tx);
        let mut anchors = ParentAnchors::new();
        degenbot_core::runtime::get_runtime().block_on(resolution.resolve_requests(
            requests,
            provider,
            &mut anchors,
        ))?;
        Ok(resolution.deltas.clone())
    }

    /// The discount the pure composition yields for one settled refresh.
    fn expected_discount(scaled_balance: U256, index: U256, stk_aave_balance: U256) -> i64 {
        let debt_balance = crate::ray_mul(scaled_balance, index, None).unwrap();
        let rate =
            crate::gho_processor::calculate_gho_discount_rate(debt_balance, stk_aave_balance)
                .unwrap();
        crate::config_dispatch::discount_to_i64(rate)
    }

    fn observed_users(deltas: &[CanonicalDelta]) -> Vec<i64> {
        deltas
            .iter()
            .filter_map(|delta| match delta.effect() {
                DeltaEffect::StkAaveBalanceObserved { user_id, .. } => Some(*user_id),
                _ => None,
            })
            .collect()
    }

    fn refreshed(deltas: &[CanonicalDelta]) -> Vec<(i64, i64)> {
        deltas
            .iter()
            .filter_map(|delta| match delta.effect() {
                DeltaEffect::GhoDiscountRefreshed {
                    user_id,
                    gho_discount,
                } => Some((*user_id, *gho_discount)),
                _ => None,
            })
            .collect()
    }

    /// The hash one issued `eth_call` pinned, as the wire carried it.
    fn pinned_hash(issued: &Issued) -> Option<String> {
        issued
            .block_id()?
            .get("blockHash")?
            .as_str()
            .map(str::to_string)
    }

    /// The user address one issued `balanceOf(address)` call names.
    fn read_user(issued: &Issued) -> Option<Address> {
        let data = issued.params.get(0)?.get("data")?.as_str()?;
        let word = data.get(10..)?;
        format!("0x{}", word.get(24..)?).parse().ok()
    }

    fn stk_aave_answer(balance: U256) -> Answer {
        Answer::Return(Bytes::from(balance.to_be_bytes::<32>().to_vec()))
    }

    /// The canonical refresh: the settled debt position and the parent-block
    /// stkAAVE read compose into the observation the read settled and the
    /// recomputed discount that consumed it, in that order, at the signal's own
    /// position.
    #[test]
    fn refresh_composes_the_settled_inputs_into_observation_then_discount() {
        let user = Address::repeat_byte(0x41);
        let mut fixture = refresh_fixture(&[user]);
        let (user_id, position_id) = (fixture.users[0].1, fixture.positions[0]);
        let stk_aave = U256::from(500_000_000_000_000_000_000_u128);
        let index = ray();
        let chain = ScriptedChain::new().answer(
            fixture.discount_token,
            balance_selector(),
            stk_aave_answer(stk_aave),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled = [(position_id, U256::from(1_000_u64), index)];
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();

        let deltas = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![refresh_request(tx, 7, position_id, index)],
        )
        .expect("a settled refresh composes");

        assert_eq!(
            deltas.len(),
            2,
            "the read's observation and the discount it settled"
        );
        assert!(matches!(
            deltas[0].effect(),
            DeltaEffect::StkAaveBalanceObserved { user_id: observed, balance }
                if *observed == user_id && *balance == stk_aave
        ));
        let expected = expected_discount(U256::from(1_000_u64), index, stk_aave);
        assert!(matches!(
            deltas[1].effect(),
            DeltaEffect::GhoDiscountRefreshed { user_id: refreshed_id, gho_discount }
                if *refreshed_id == user_id && *gho_discount == expected
        ));
        assert_eq!(
            &deltas[1].source(),
            &LogRef::new(tx, 7),
            "the recomputed discount travels at the signal's own position"
        );

        let reads = chain.calls_to(fixture.discount_token, balance_selector());
        assert_eq!(reads.len(), 1, "one distinct user, one parent-block read");
        assert_eq!(
            pinned_hash(&reads[0]),
            Some(format!("{HEAD_PARENT}")),
            "the read is the transaction's exact parent anchor"
        );
        assert_eq!(read_user(&reads[0]), Some(user));

        assert_eq!(
            fixture
                .substrate
                .user_gho_discount(&checksum(user))
                .unwrap(),
            Some(expected),
            "the settled discount is what persistence writes"
        );
        assert_eq!(
            fixture
                .substrate
                .user_stk_aave_balance(&checksum(user))
                .unwrap(),
            Some(Some(stk_aave)),
            "the read is projected, so no later transaction re-reads it"
        );
    }

    /// A user whose stkAAVE balance the substrate already projects owes no read:
    /// the refresh settles from the projection alone.
    #[test]
    fn refresh_uses_the_projected_balance_without_a_chain_read() {
        let user = Address::repeat_byte(0x42);
        let mut fixture = refresh_fixture(&[user]);
        let (user_id, position_id) = (fixture.users[0].1, fixture.positions[0]);
        let stk_aave = U256::from(250_000_000_000_000_000_000_u128);
        fixture
            .substrate
            .record_user_stk_aave_balance_by_id(user_id, Some(stk_aave));
        let chain = ScriptedChain::new();
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled = [(position_id, U256::from(2_000_u64), ray())];
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();

        let deltas = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![refresh_request(tx, 3, position_id, ray())],
        )
        .expect("a projected refresh composes");

        assert!(
            observed_users(&deltas).is_empty(),
            "no read, no observation"
        );
        assert_eq!(
            refreshed(&deltas),
            vec![(
                user_id,
                expected_discount(U256::from(2_000_u64), ray(), stk_aave)
            )]
        );
        assert!(
            chain.reads().is_empty(),
            "the refresh issued no call: the projection answered it"
        );
        assert!(
            !chain.issued().is_empty(),
            "the parent position is still resolved (and memoized) for every refresh"
        );
    }

    /// A short `balanceOf` word is malformed evidence, never a zero balance: the
    /// refresh refuses naming the read and its target, and no discount is
    /// recorded.
    #[test]
    fn refresh_refuses_a_short_balance_return_naming_read_and_target() {
        let user = Address::repeat_byte(0x43);
        let mut fixture = refresh_fixture(&[user]);
        let position_id = fixture.positions[0];
        let discount_token = fixture.discount_token;
        let chain = ScriptedChain::new().answer(
            discount_token,
            balance_selector(),
            Answer::Return(Bytes::from_static(&[0x01])),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled = [(position_id, U256::from(1_000_u64), ray())];
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();

        let error = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![refresh_request(tx, 1, position_id, ray())],
        )
        .expect_err("a short balanceOf return must abort the refresh, never cache zero");

        match error {
            RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(message)) => {
                assert!(message.contains("balanceOf"), "names the read: {message}");
                assert!(
                    message.contains(&checksum(discount_token)),
                    "names the target: {message}"
                );
            }
            other => panic!("expected DecodeShape, got {other:?}"),
        }
        assert_eq!(
            fixture
                .substrate
                .user_gho_discount(&checksum(user))
                .unwrap(),
            Some(0),
            "the refused refresh recorded no discount"
        );
    }

    /// A position the seed never resolved is a seed hole, refused before any read.
    #[test]
    fn refresh_refuses_a_position_the_seed_never_resolved() {
        let user = Address::repeat_byte(0x44);
        let mut fixture = refresh_fixture(&[user]);
        let position_id = fixture.positions[0];
        let chain = ScriptedChain::new();
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let mut view = ProjectionView::new();
        seed_positions(
            &mut view,
            &[(position_id + 1_000, U256::from(1_u64), ray())],
        );
        let tx = head_tx();

        let error = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![refresh_request(tx, 0, position_id + 1_000, ray())],
        )
        .expect_err("an unknown position cannot refresh");

        assert!(
            matches!(error, RunError::IncompleteChunkSeed(_)),
            "a position outside the seed is a seed hole, got {error:?}"
        );
        assert!(
            chain
                .calls_to(fixture.discount_token, balance_selector())
                .is_empty(),
            "a refused lookup reads nothing"
        );
    }

    /// A debt position the view never resolved is a projection refusal: the
    /// refresh cannot read a balance for a position whose settlement it does not
    /// hold.
    #[test]
    fn refresh_refuses_a_position_the_view_never_settled() {
        let user = Address::repeat_byte(0x45);
        let mut fixture = refresh_fixture(&[user]);
        let position_id = fixture.positions[0];
        let chain = ScriptedChain::new();
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let mut view = ProjectionView::new();
        let tx = head_tx();

        let error = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![refresh_request(tx, 0, position_id, ray())],
        )
        .expect_err("a position with no settled state cannot refresh");

        assert!(
            matches!(
                error,
                RunError::ProcessTx(ProcessTxError::Projection(
                    crate::projection::ProjectionError::UnresolvedPosition { .. }
                ))
            ),
            "an unsettled position is the projection's refusal, got {error:?}"
        );
    }

    /// Two triggers on one user inside one transaction share the parent-block
    /// read: one observation, one read, and both recomputed discounts in request
    /// order.
    #[test]
    fn one_transactions_two_triggers_share_one_parent_block_read() {
        let user = Address::repeat_byte(0x46);
        let mut fixture = refresh_fixture(&[user]);
        let (user_id, position_id) = (fixture.users[0].1, fixture.positions[0]);
        let stk_aave = U256::from(400_000_000_000_000_000_000_u128);
        let chain = ScriptedChain::new().answer(
            fixture.discount_token,
            balance_selector(),
            stk_aave_answer(stk_aave),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled = [(position_id, U256::from(3_000_u64), ray())];
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();

        let deltas = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            tx,
            vec![
                refresh_request(tx, 2, position_id, ray()),
                refresh_request(tx, 5, position_id, ray()),
            ],
        )
        .expect("both triggers resolve");

        assert_eq!(
            observed_users(&deltas),
            vec![user_id],
            "one read, one observation"
        );
        let expected = expected_discount(U256::from(3_000_u64), ray(), stk_aave);
        assert_eq!(
            refreshed(&deltas),
            vec![(user_id, expected), (user_id, expected)],
            "both triggers recompute at their own positions"
        );
        assert_eq!(
            deltas
                .iter()
                .map(CanonicalDelta::source)
                .collect::<Vec<_>>(),
            vec![LogRef::new(tx, 2), LogRef::new(tx, 2), LogRef::new(tx, 5)],
            "observation then discount per trigger, in request order"
        );
        assert_eq!(
            chain
                .calls_to(fixture.discount_token, balance_selector())
                .len(),
            1,
            "the second trigger reuses the first one's read"
        );
    }

    /// A synthetic multi-refresh transaction applies its prefetched balances in
    /// canonical request order: one observation and one recomputed discount per
    /// user, interleaved in the order the requests were raised.
    #[test]
    fn multi_user_refresh_applies_its_prefetch_in_canonical_request_order() {
        let users = vec![
            Address::repeat_byte(0x51),
            Address::repeat_byte(0x52),
            Address::repeat_byte(0x53),
        ];
        let mut fixture = refresh_fixture(&users);
        let user_ids: Vec<i64> = fixture.users.iter().map(|(_, id)| *id).collect();
        let positions = fixture.positions.clone();
        let stk_aave = U256::from(600_000_000_000_000_000_000_u128);
        let chain = ScriptedChain::new().answer(
            fixture.discount_token,
            balance_selector(),
            stk_aave_answer(stk_aave),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled: Vec<(i64, U256, U256)> = positions
            .iter()
            .enumerate()
            .map(|(i, position_id)| (*position_id, U256::from(1_000_u64 * (i as u64 + 1)), ray()))
            .collect();
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();
        let requests: Vec<ResolutionRequest> = positions
            .iter()
            .enumerate()
            .map(|(i, position_id)| refresh_request(tx, i as u64, *position_id, ray()))
            .collect();

        let deltas =
            refresh_in_boundary(&mut fixture.substrate, &mut view, &provider, tx, requests)
                .expect("a multi-user refresh resolves");

        assert_eq!(observed_users(&deltas), user_ids, "canonical request order");
        let expected: Vec<(i64, i64)> = settled
            .iter()
            .zip(&user_ids)
            .map(|((_, scaled, index), user_id)| {
                (*user_id, expected_discount(*scaled, *index, stk_aave))
            })
            .collect();
        assert_eq!(refreshed(&deltas), expected);
        assert_eq!(
            deltas.len(),
            2 * positions.len(),
            "each user's observation precedes its discount"
        );
        for (pair, user_id) in deltas.chunks(2).zip(&user_ids) {
            assert!(matches!(
                pair[0].effect(),
                DeltaEffect::StkAaveBalanceObserved { user_id: observed, .. } if observed == user_id
            ));
            assert!(matches!(
                pair[1].effect(),
                DeltaEffect::GhoDiscountRefreshed { user_id: settled, .. } if settled == user_id
            ));
        }
        let reads = chain.calls_to(fixture.discount_token, balance_selector());
        assert_eq!(
            reads.iter().filter_map(read_user).collect::<Vec<_>>(),
            users,
            "one read per distinct user, issued in request order"
        );
    }

    /// The prefetch's width is capped: a stalled chain sees at most
    /// [`MAX_CONCURRENT_REFRESH_READS`] of one transaction's reads in flight,
    /// whatever the number of unobserved users.
    #[test]
    fn refresh_prefetch_caps_its_concurrency_at_the_bounded_width() {
        let users: Vec<Address> = (1..=MAX_CONCURRENT_REFRESH_READS + 2)
            .map(|byte| Address::repeat_byte(u8::try_from(byte).unwrap()))
            .collect();
        let fixture = refresh_fixture(&users);
        let positions = fixture.positions.clone();
        let discount_token = fixture.discount_token;
        let chain = ScriptedChain::new().stall_calls();
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let settled: Vec<(i64, U256, U256)> = positions
            .iter()
            .map(|position_id| (*position_id, U256::from(1_000_u64), ray()))
            .collect();
        let mut view = ProjectionView::new();
        seed_positions(&mut view, &settled);
        let tx = head_tx();
        let requests: Vec<ResolutionRequest> = positions
            .iter()
            .enumerate()
            .map(|(i, position_id)| refresh_request(tx, i as u64, *position_id, ray()))
            .collect();

        let mut substrate = fixture.substrate;
        let mut ctx = ChunkContext::for_chunk(
            whole_range_span(),
            provider.clone(),
            MARKET_ID,
            CHAIN_ID,
            Address::ZERO,
            &mut substrate,
        );
        let mut resolution =
            TransactionResolution::for_transaction(&mut ctx, &mut view, MARKET_ID, CHAIN_ID, tx);
        let mut anchors = ParentAnchors::new();
        let mut pass =
            std::pin::pin!(resolution.resolve_requests(requests, &provider, &mut anchors));
        let waker = Waker::noop();
        degenbot_core::runtime::get_runtime().block_on(async {
            let mut cx = Context::from_waker(waker);
            for _ in 0..64 {
                assert!(
                    !pass.as_mut().poll(&mut cx).is_ready(),
                    "a stalled prefetch must never complete"
                );
            }
        });

        assert_eq!(
            chain.in_flight(),
            MAX_CONCURRENT_REFRESH_READS,
            "the prefetch holds the capped wave and no more"
        );
        let reads = chain.calls_to(discount_token, balance_selector());
        assert_eq!(
            reads.iter().filter_map(read_user).collect::<Vec<_>>(),
            users[..MAX_CONCURRENT_REFRESH_READS].to_vec(),
            "the capped wave is the request order's prefix"
        );
        assert!(
            reads
                .iter()
                .all(|read| pinned_hash(read) == Some(format!("{HEAD_PARENT}"))),
            "every prefetched read pins the transaction's parent anchor"
        );
        assert!(
            chain.in_flight() < users.len(),
            "the request count exceeds the width that reached the node"
        );
    }

    /// Two transactions whose refreshes need reads each consume only the read
    /// their own event boundary issued: the earlier transaction's discount is
    /// the composition of its settled view, never the later transaction's.
    #[test]
    fn interleaved_refreshes_cannot_consume_a_later_transactions_view() {
        let users = vec![Address::repeat_byte(0x61), Address::repeat_byte(0x62)];
        let mut fixture = refresh_fixture(&users);
        let positions = fixture.positions.clone();
        let user_ids: Vec<i64> = fixture.users.iter().map(|(_, id)| *id).collect();
        let stk_aave = U256::from(1_000_000_000_000_000_000_u128);
        let chain = ScriptedChain::new().answer(
            fixture.discount_token,
            balance_selector(),
            stk_aave_answer(stk_aave),
        );
        let provider = degenbot_core::runtime::get_runtime().block_on(chain.provider());
        let first = tx_at(HEAD_NUMBER, HEAD_HASH, HEAD_PARENT);
        let second = tx_at(HEAD_NUMBER + 1, B256::repeat_byte(0xee), HEAD_HASH);
        let mut view = ProjectionView::new();

        let first_settled = U256::from(1_000_000_000_000_000_000_000_u128);
        seed_positions(&mut view, &[(positions[0], first_settled, ray())]);
        let first_deltas = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            first,
            vec![refresh_request(first, 0, positions[0], ray())],
        )
        .expect("the first transaction's refresh resolves");

        let second_settled = U256::from(100_000_000_000_000_000_000_000_u128);
        seed_positions(&mut view, &[(positions[1], second_settled, ray())]);
        let second_deltas = refresh_in_boundary(
            &mut fixture.substrate,
            &mut view,
            &provider,
            second,
            vec![refresh_request(second, 0, positions[1], ray())],
        )
        .expect("the second transaction's refresh resolves");

        assert_eq!(
            refreshed(&first_deltas),
            vec![(
                user_ids[0],
                expected_discount(first_settled, ray(), stk_aave)
            )],
            "the first transaction settles against its own event boundary"
        );
        assert_eq!(
            refreshed(&second_deltas),
            vec![(
                user_ids[1],
                expected_discount(second_settled, ray(), stk_aave)
            )],
            "the later transaction settles against its own"
        );
        assert_ne!(
            refreshed(&first_deltas)[0].1,
            expected_discount(second_settled, ray(), stk_aave),
            "the first transaction's discount is not the later view's composition"
        );
        assert_eq!(
            observed_users(&first_deltas),
            vec![user_ids[0]],
            "each transaction observes only the user its own triggers name"
        );
        assert_eq!(observed_users(&second_deltas), vec![user_ids[1]]);

        let reads = chain.calls_to(fixture.discount_token, balance_selector());
        assert_eq!(
            reads.len(),
            2,
            "one read per transaction, no sharing across"
        );
        assert_eq!(
            pinned_hash(&reads[0]),
            Some(format!("{HEAD_PARENT}")),
            "the first transaction reads its own parent"
        );
        assert_eq!(
            pinned_hash(&reads[1]),
            Some(format!("{HEAD_HASH}")),
            "the second transaction reads its own parent, never the first's"
        );
    }
}
