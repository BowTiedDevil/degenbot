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

use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::{LogRef, TxRef};
use degenbot_db::ScaledTokenPosition;
use degenbot_rpc::chain_evidence::BlockReads;
use degenbot_rpc::provider::AlloyProvider;

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
        for request in reduction.requests {
            self.resolve_request(request, provider, parent_anchors)
                .await?;
        }
        Ok(ResolvedTransaction {
            deltas: self.deltas,
            decode_compute_time,
        })
    }

    /// Resolve one follow-up request the transaction's journaled signals raised.
    async fn resolve_request(
        &mut self,
        request: ResolutionRequest,
        provider: &AlloyProvider,
        parent_anchors: &mut ParentAnchors,
    ) -> Result<(), RunError> {
        match request {
            ResolutionRequest::RefreshGhoDiscount {
                source,
                position_id,
                index,
            } => {
                let child = *source.tx().anchor();
                let parent = parent_anchors.of(provider, child).await?;
                self.refresh_gho_discount(
                    &BlockReads::at(provider, parent),
                    source,
                    position_id,
                    index,
                )
                .await
            }
        }
    }

    /// Recompute one user's GHO discount (the (C) refresh): the debt balance is
    /// the view's post-transaction scaled balance at the triggering effect's
    /// index, and the stkAAVE balance is the projection's — or, when the user
    /// has never been observed, one `balanceOf` at the transaction's exact
    /// parent block, emitted as its own observation delta before the recomputed
    /// discount. The settled values travel as deltas, so persistence writes them
    /// without recomputing.
    async fn refresh_gho_discount(
        &mut self,
        reads: &BlockReads<'_>,
        source: LogRef,
        position_id: i64,
        index: U256,
    ) -> Result<(), RunError> {
        let discount_token = self
            .ctx
            .gho_asset()
            .and_then(|gho| gho.v_gho_discount_token)
            .and_then(|address| address.parse::<Address>().ok());
        let Some(discount_token) = discount_token else {
            return Ok(());
        };
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
                    crate::config_dispatch::encode_single_address_call(
                        "balanceOf(address)",
                        &address,
                    ),
                )
                .await
                .map_err(|error| RunError::ConfigDispatch(ConfigDispatchError::from(error)))?;
            let balance = crate::config_dispatch::require_word0_u256(
                &ret,
                "balanceOf(address)",
                &discount_token,
            )
            .map_err(|error| {
                RunError::ConfigDispatch(ConfigDispatchError::DecodeShape(format!(
                    "balanceOf for user {user_address}: {error}"
                )))
            })?;
            self.ctx
                .substrate_mut()
                .record_user_stk_aave_balance_by_id(user_id, Some(balance));
            self.deltas.push(CanonicalDelta::new(
                source,
                None,
                DeltaEffect::StkAaveBalanceObserved { user_id, balance },
            ));
            balance
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
