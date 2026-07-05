//! The per-tx apply dispatch glue (HQF5NQ-C). Mirrors
//! `src/degenbot/cli/aave/transaction_processor.py::_process_transaction` +
//! `_process_operation` — the bridge from parsed `Operation`s to substrate
//! [`AaveChunkEvent`] variants the orchestrator batches +
//! `apply_aave_chunk_writes_on_conn` stamps.
//!
//! # The plumbing-equivalence caveat (Finding 1 — the §4.2-drift canary)
//!
//! The Python pipeline is two stages: (1) `ScaledEventEnricher.enrich` —
//! extract `raw_amount` from the Operation's Pool event + compute
//! `scaled_amount = ray_div(raw_amount, index, strategy)`;
//! (2) `UnifiedCollateralProcessor.process_mint_event(event_data,
//! scaled_delta=enriched.scaled_amount)` — use the pre-calculated delta
//! directly. [`ScaledTokenProcessor`] in SCALEAPPLY already subsumes stage 2
//! (it takes `ScaledTokenEventData.scaled_amount: Option<U256>` + uses it
//! directly when `Some`).
//!
//! **CANNOT pass `scaled_amount: None`** — the processor's None path
//! computes `ray_div(value - balance_increase, index, strategy)` (it
//! subtracts the accrued interest before scaling), which DIVERGES from the
//! Python `scaled_amount = ray_div(raw_amount, index, strategy)` whenever
//! `balance_increase > 0` (the Python enricher's `raw_amount` is the Pool
//! event's amount, NOT `value - balance_increase`). So C's enricher MUST
//! extract `raw_amount` from the Operation's Pool event + compute
//! `scaled_amount = ray_div(raw_amount, index, strategy)` + pass it as
//! `Some(...)`. This is the plumbing-equivalence caveat materialized; the
//! §4.2 cross-check (U5YIBG) is the final arbiter.
//!
//! # Scope (incremental)
//!
//! This impl covers the 5 standard pool-event operations (Supply/Withdraw/
//! Borrow/Repay/RepayWithAtokens) + the standalone `BalanceTransfer` + the
//! InterestAccrual/InterestOnly paths. The liquidation apply
//! (`OperationType::Liquidation`/`GhoLiquidation`), the GHO-discount-lookup
//! machinery (scope 3 — the `discount_updates_by_log_index` map +
//! `get_effective_discount_at_log_index` path + the stkAAVE pre-processing),
//! the `DeficitCoverage` paired-event apply, + the `MintToTreasury` v8 `ray_div`
//! branch are **deferred** — flagged `Err(Deferred)` so the orchestrator can
//! split them into C2 sub-tasks without rushing the edge-branch verification.

use crate::operations::{Operation, OperationType, ScaledTokenEvent, ScaledTokenEventType};
use crate::operations_parser::{addr_to_hex, log_idx_value, TransactionOperationsParser};
use crate::processors::{
    collateral_strategy, debt_strategy, ProcessorError, ScaledTokenEventData, ScaledTokenProcessor,
};
use crate::run::AaveChunkEvent;
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_db::{DegenbotDb, ScaledTokenPosition};
use degenbot_evm_math::ray_div;
use rusqlite::Connection;
use thiserror::Error;

/// Errors from `process_transaction`.
#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)]
pub enum ProcessTxError {
    /// A parser failure (the `parse()` stage).
    #[error("parse error: {0}")]
    Parse(#[from] crate::operations_parser::ParseError),
    /// A substrate (`DegenbotDb::`) failure (a SELECT / get-or-create / apply).
    #[error("substrate error: {0}")]
    Substrate(#[from] degenbot_db::DbError),
    /// A `ScaledTokenProcessor` failure (ray-math overflow / delta overflow).
    #[error("processor error: {0}")]
    Processor(#[from] ProcessorError),
    /// A ray-math failure from the enrichment's `ray_div`.
    #[error("ray-math error: {0}")]
    RayMath(#[from] degenbot_evm_math::WadRayError),
    /// A deferred path (HQF5NQ-C2 — the liquidation apply / GHO discount
    /// machinery / `DeficitCoverage` / `MintToTreasury`). The orchestrator may
    /// split these into sub-tasks rather than rush the edge-branch
    /// verification.
    #[error("deferred (HQF5NQ-C2): {0}")]
    Deferred(String),
}

/// The per-tx entry point the orchestrator (6SWY4R) calls. Mirrors
/// `_process_transaction` — parses the tx logs into `Operation`s +
/// dispatches each Operation's constituent scaled-token events to
/// [`AaveChunkEvent`] variants (the enricher + processor pipeline). Returns
/// the accumulated events for the orchestrator to batch + stamp via
/// `apply_aave_chunk_writes_on_conn`.
///
/// The `conn` is the caller's chunk `Transaction`'s borrowed `&Connection`
/// (every substrate lookup goes through it — the §3.4 atomicity invariant).
///
/// # Errors
///
/// Returns [`ProcessTxError`] on any parse / substrate / processor / ray-math
/// failure, OR [`ProcessTxError::Deferred`] for the not-yet-ported paths
/// (liquidation apply / GHO discount machinery / `DeficitCoverage` /
/// `MintToTreasury`).
#[allow(
    clippy::too_many_arguments,
    clippy::missing_errors_doc,
    clippy::similar_names
)]
pub fn process_transaction(
    market_id: i64,
    chain_id: i64,
    pool_address: Address,
    treasury_address: Option<Address>,
    gho_token_address: Option<Address>,
    gho_vtoken_address: Option<Address>,
    conn: &Connection,
    tx_logs: &[&Log],
    tx_hash: [u8; 32],
) -> Result<Vec<AaveChunkEvent>, ProcessTxError> {
    let parser = TransactionOperationsParser::new(
        market_id,
        chain_id,
        pool_address,
        treasury_address,
        gho_token_address,
        gho_vtoken_address,
        conn,
    )?;
    let parsed = parser.parse(tx_logs, tx_hash)?;

    // Sort the parsed operations by pool_event logIndex (or minimum
    // scaled_event log_index for the no-pool-event operations — INTEREST_
    // ACCRUAL / MintToTreasury). Mirrors the Python `_get_operation_sort_key`.
    let mut sorted_ops: Vec<&Operation> = parsed.operations.iter().collect();
    sorted_ops.sort_by_key(|op| operation_sort_key(op));

    let mut events: Vec<AaveChunkEvent> = Vec::new();
    for op in &sorted_ops {
        dispatch_operation(op, market_id, conn, &mut events)?;
    }
    Ok(events)
}

/// The per-Operation sort key (mirror of `_get_operation_sort_key`). Operations
/// with a `pool_event` sort by its `logIndex`; operations without (the phase-4
/// post-loop appends — InterestAccrual/MintToTreasury/BalanceTransfer/
/// `DeficitCoverage`) sort by their minimum `scaled_event` `logIndex`.
fn operation_sort_key(op: &Operation) -> u64 {
    if let Some(pool_event) = op.pool_event {
        log_idx_value(pool_event)
    } else if !op.scaled_events.is_empty() {
        op.scaled_events
            .iter()
            .map(|e| e.log_index)
            .min()
            .unwrap_or(0)
    } else {
        0
    }
}

/// Dispatch a single Operation's scaled-token events → [`AaveChunkEvent`]
/// variants. Mirrors `_process_operation`'s per-event-type dispatch loop.
fn dispatch_operation(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
) -> Result<(), ProcessTxError> {
    match op.operation_type {
        OperationType::Supply
        | OperationType::Withdraw
        | OperationType::Borrow
        | OperationType::Repay
        | OperationType::RepayWithAtokens => dispatch_standard(op, market_id, conn, events),
        OperationType::BalanceTransfer => dispatch_balance_transfer(op, market_id, conn, events),
        // Standalone interest accrual (amount == balance_increase). Delta = 0
        // (only the index updates — the apply fn reconciles `last_index`).
        // The Operation carries the scaled events; pass them through as
        // zero-delta mints/burns so the position's `last_index` advances.
        OperationType::InterestAccrual => dispatch_interest_accrual(op, market_id, conn, events),
        OperationType::Liquidation | OperationType::GhoLiquidation => Err(ProcessTxError::Deferred(
            "liquidation apply (collateral_burn vs collateral_transfer split + debt_burn dispatch) — C2"
                .into(),
        )),
        OperationType::GhoBorrow | OperationType::GhoRepay | OperationType::GhoFlashLoan => {
            Err(ProcessTxError::Deferred(
                "GHO apply (the GHO-discount-lookup machinery + the GhoDebtMint/Burn apply) — C2 / RYKCC4"
                    .into(),
            ))
        }
        OperationType::DeficitCoverage => Err(ProcessTxError::Deferred(
            "deficit-coverage paired-event apply (BalanceTransfer + Burn atomic) — C2".into(),
        )),
        OperationType::MintToTreasury => Err(ProcessTxError::Deferred(
            "mint-to-treasury v8 ray_div(amountMinted, liquidity_index) branch — C2".into(),
        )),
        OperationType::StkAaveTransfer => {
            // Pre-processed by the orchestrator (the stkAAVE transfers run
            // BEFORE GHO operations for the discount computation); no apply
            // here. The orchestrator constructs `AaveChunkEvent::StkAaveStaked`
            // / `StkAaveRedeem` directly from the decoded ERC20 Transfer.
            Ok(())
        }
        OperationType::Unknown => {
            // The Python marks DEFICIT_CREATED as UNKNOWN to avoid interfering
            // with liquidation processing downstream — no apply, no event.
            Ok(())
        }
    }
}

/// Dispatch the 5 standard pool-event operations (Supply/Withdraw/Borrow/
/// Repay/RepayWithAtokens). Each Operation's `scaled_events` carry the matched
/// aToken/vToken Mint/Burn events; the Pool event's `data` word 0 is the
/// `raw_amount` (supplyAmount/withdrawAmount/borrowAmount/repayAmount).
fn dispatch_standard(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
) -> Result<(), ProcessTxError> {
    let pool_event = op.pool_event.ok_or_else(|| {
        ProcessTxError::Deferred(format!(
            "standard op {:?} has no pool_event — enricher cannot extract raw_amount",
            op.operation_type
        ))
    })?;
    let raw_amount = extract_pool_amount_word0(pool_event);
    // Sort scaled_events by logIndex (mirror of `_process_operation`).
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let chunk_event = build_scaled_event_chunk_event(ev, op, raw_amount, market_id, conn)?;
        events.push(chunk_event);
    }
    Ok(())
}

/// Extract the first data word (32 bytes) from a Pool event's `data` field.
/// This is the `raw_amount` for SUPPLY/WITHDRAW/BORROW/REPAY
/// (supplyAmount/withdrawAmount/borrowAmount/repayAmount — ABI-encoded as the
/// first word of the non-indexed `data`).
fn extract_pool_amount_word0(pool_event: &Log) -> U256 {
    let data = pool_event.data().data.as_ref();
    if data.len() < 32 {
        return U256::ZERO;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    U256::from_be_bytes::<32>(buf)
}

/// Build an [`AaveChunkEvent`] from a single scaled-token event. Mirrors the
/// `_process_operation` per-event-type dispatch (the Mint-exceeds → Burn edge
/// + the standard Mint/Burn/Transfer paths). The `raw_amount` is the Pool
///   event's amount (the enricher's `raw_amount`); the `scaled_amount` is
///   computed as `ray_div(raw_amount, index, strategy)` (the plumbing-
///   equivalence caveat — NOT the processor's None fallback path).
#[allow(clippy::too_many_lines)]
fn build_scaled_event_chunk_event(
    ev: &ScaledTokenEvent,
    op: &Operation,
    raw_amount: U256,
    market_id: i64,
    conn: &Connection,
) -> Result<AaveChunkEvent, ProcessTxError> {
    // Resolve the emitter's AssetRow (id + revisions + sibling addresses).
    let token_addr_str = addr_to_hex(ev.token_address);
    let token_type = match ev.event_type {
        ScaledTokenEventType::CollateralMint
        | ScaledTokenEventType::CollateralBurn
        | ScaledTokenEventType::CollateralTransfer
        | ScaledTokenEventType::Erc20CollateralTransfer => "a_token",
        ScaledTokenEventType::DebtMint
        | ScaledTokenEventType::DebtBurn
        | ScaledTokenEventType::DebtTransfer
        | ScaledTokenEventType::GhoDebtMint
        | ScaledTokenEventType::GhoDebtBurn
        | ScaledTokenEventType::GhoDebtTransfer => "v_token",
        _ => {
            return Err(ProcessTxError::Deferred(format!(
                "non-scaled-token event_type {:?} in standard op — C2",
                ev.event_type
            )));
        }
    };
    let asset = DegenbotDb::lookup_asset_by_token_address_on_conn(
        conn,
        market_id,
        &token_addr_str,
        token_type,
    )?
    .ok_or_else(|| {
        ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
            "no asset for token {token_addr_str} ({token_type}) in market {market_id}"
        )))
    })?;

    let balance_increase = ev.balance_increase.unwrap_or_default();
    let index = ev.index.unwrap_or_default();

    // The enricher's `scaled_amount = ray_div(raw_amount, index, strategy)`.
    // The strategy is the per-revision + per-event-type rounding.
    let (position, processor, strategy_mode) = if token_type == "a_token" {
        let strat = collateral_strategy(asset.a_token_revision);
        (
            ScaledTokenPosition::Collateral,
            ScaledTokenProcessor::collateral(asset.a_token_revision),
            strat,
        )
    } else {
        let strat = debt_strategy(asset.v_token_revision);
        (
            ScaledTokenPosition::Debt,
            ScaledTokenProcessor::debt(asset.v_token_revision),
            strat,
        )
    };

    match ev.event_type {
        ScaledTokenEventType::CollateralMint
        | ScaledTokenEventType::DebtMint
        | ScaledTokenEventType::GhoDebtMint => {
            // The interest-exceeds edge: WITHDRAW/REPAY_WITH_ATOKENS where
            // `amount < balance_increase` — the Mint is emitted as an
            // effective Burn (mirror of `_process_operation`'s special case).
            let is_interest_exceeds_edge = (op.operation_type == OperationType::Withdraw
                || op.operation_type == OperationType::RepayWithAtokens)
                && ev.amount < balance_increase;
            if is_interest_exceeds_edge {
                // Treat the Mint as a Burn: compute the scaled delta from
                // `raw_amount` using the BURN strategy (the Python enricher
                // still uses the raw_amount from the pool_event for the
                // scaled_amount; the processor's None path for burn uses
                // `value + balance_increase`, so we MUST pass the enriched
                // scaled_amount as `Some`).
                let scaled_amount = ray_div(raw_amount, index, strategy_mode.burn.into())?;
                let event_data = ScaledTokenEventData {
                    value: ev.amount,
                    balance_increase,
                    index,
                    scaled_amount: Some(scaled_amount),
                };
                let result = processor.process_collateral_burn(&event_data, None)?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    position,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                )?;
                Ok(AaveChunkEvent::ScaledTokenBurn {
                    position,
                    position_id,
                    balance_delta: result.balance_delta,
                    new_index: result.new_index,
                })
            } else {
                // Standard mint path.
                let scaled_amount = ray_div(raw_amount, index, strategy_mode.mint.into())?;
                let event_data = ScaledTokenEventData {
                    value: ev.amount,
                    balance_increase,
                    index,
                    scaled_amount: Some(scaled_amount),
                };
                let result = processor
                    .process_collateral_mint(&event_data)
                    .or_else(|_| processor.process_debt_mint(&event_data))?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    position,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                )?;
                Ok(AaveChunkEvent::ScaledTokenMint {
                    position,
                    position_id,
                    balance_delta: result.balance_delta,
                    new_index: result.new_index,
                })
            }
        }
        ScaledTokenEventType::CollateralBurn
        | ScaledTokenEventType::DebtBurn
        | ScaledTokenEventType::GhoDebtBurn => {
            let scaled_amount = ray_div(raw_amount, index, strategy_mode.burn.into())?;
            let event_data = ScaledTokenEventData {
                value: ev.amount,
                balance_increase,
                index,
                scaled_amount: Some(scaled_amount),
            };
            let result = processor.process_collateral_burn(&event_data, None)?;
            let position_id = resolve_position_id(
                conn,
                market_id,
                position,
                ev.user_address,
                asset.id,
                &asset.underlying_token_address,
            )?;
            Ok(AaveChunkEvent::ScaledTokenBurn {
                position,
                position_id,
                balance_delta: result.balance_delta,
                new_index: result.new_index,
            })
        }
        ScaledTokenEventType::CollateralTransfer
        | ScaledTokenEventType::Erc20CollateralTransfer => {
            // The aToken BalanceTransfer (collateral moved between users).
            // The `from` + `to` positions must both be resolved.
            let from_addr = ev.from_address.unwrap_or(ev.user_address);
            let to_addr = ev.target_address.unwrap_or_default();
            let from_position_id = resolve_position_id(
                conn,
                market_id,
                ScaledTokenPosition::Collateral,
                from_addr,
                asset.id,
                &asset.underlying_token_address,
            )?;
            let to_position_id = resolve_position_id(
                conn,
                market_id,
                ScaledTokenPosition::Collateral,
                to_addr,
                asset.id,
                &asset.underlying_token_address,
            )?;
            Ok(AaveChunkEvent::ScaledTokenTransfer {
                from_position_id,
                to_position_id,
                scaled_amount: ev.amount,
                transfer_index: index,
            })
        }
        _ => Err(ProcessTxError::Deferred(format!(
            "event_type {:?} not handled in standard op — C2",
            ev.event_type
        ))),
    }
}

/// Dispatch a standalone `BalanceTransfer` operation (no Pool event). The
/// scaled event's `amount` IS the scaled transfer amount (no enrichment
/// needed — the `BalanceTransfer` carries the scaled value directly).
fn dispatch_balance_transfer(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
) -> Result<(), ProcessTxError> {
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let chunk_event = build_scaled_event_chunk_event(ev, op, ev.amount, market_id, conn)?;
        events.push(chunk_event);
    }
    Ok(())
}

/// Dispatch an `InterestAccrual` operation (amount == `balance_increase` →
/// delta = 0; only the position's `last_index` advances). The enricher passes
/// `scaled_amount: Some(0)` (the accrued-interest-only path — the balance
/// doesn't change, but the index does).
fn dispatch_interest_accrual(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
) -> Result<(), ProcessTxError> {
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let token_addr_str = addr_to_hex(ev.token_address);
        let token_type = match ev.event_type {
            ScaledTokenEventType::CollateralMint | ScaledTokenEventType::CollateralBurn => {
                "a_token"
            }
            ScaledTokenEventType::DebtMint | ScaledTokenEventType::DebtBurn => "v_token",
            _ => {
                return Err(ProcessTxError::Deferred(format!(
                    "interest-accrual event_type {:?} — C2",
                    ev.event_type
                )));
            }
        };
        let asset = DegenbotDb::lookup_asset_by_token_address_on_conn(
            conn,
            market_id,
            &token_addr_str,
            token_type,
        )?
        .ok_or_else(|| {
            ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                "no asset for token {token_addr_str} ({token_type}) in market {market_id}"
            )))
        })?;
        let index = ev.index.unwrap_or_default();
        let balance_increase = ev.balance_increase.unwrap_or_default();
        let event_data = ScaledTokenEventData {
            value: ev.amount,
            balance_increase,
            index,
            scaled_amount: Some(U256::ZERO), // interest-only → delta 0
        };
        let (position, result) = if token_type == "a_token" {
            let proc = ScaledTokenProcessor::collateral(asset.a_token_revision);
            let r = proc.process_collateral_mint(&event_data)?;
            (ScaledTokenPosition::Collateral, r)
        } else {
            let proc = ScaledTokenProcessor::debt(asset.v_token_revision);
            let r = proc.process_debt_mint(&event_data)?;
            (ScaledTokenPosition::Debt, r)
        };
        let position_id = resolve_position_id(
            conn,
            market_id,
            position,
            ev.user_address,
            asset.id,
            &asset.underlying_token_address,
        )?;
        events.push(AaveChunkEvent::ScaledTokenMint {
            position,
            position_id,
            balance_delta: result.balance_delta,
            new_index: result.new_index,
        });
    }
    Ok(())
}

/// Resolve a `position_id` via `get_or_create_*_position_on_conn`. First
/// resolves the `user_id` via `get_or_create_user_on_conn`.
#[allow(clippy::too_many_arguments)]
fn resolve_position_id(
    conn: &Connection,
    market_id: i64,
    position: ScaledTokenPosition,
    user_address: Address,
    asset_id: i64,
    _underlying_address: &str,
) -> Result<i64, ProcessTxError> {
    let user_addr_str = addr_to_hex(user_address);
    let user_id = DegenbotDb::get_or_create_user_on_conn(
        conn,
        market_id,
        &user_addr_str,
        0, // gho_discount — the GHO-discount-lookup machinery is C2 / RYKCC4
    )?;
    let position_id = match position {
        ScaledTokenPosition::Collateral => {
            DegenbotDb::get_or_create_collateral_position_on_conn(conn, user_id, asset_id)?
        }
        ScaledTokenPosition::Debt => {
            DegenbotDb::get_or_create_debt_position_on_conn(conn, user_id, asset_id)?
        }
    };
    Ok(position_id)
}

// Silence the unused-import warn for `I256` (kept for the deferred paths'
// type-annotation parity; remove when C2 lands).
// (Removed — no deferred path uses I256 directly; the processor returns it.)

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::too_many_lines)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog, B256};
    use degenbot_db::DegenbotDb;

    /// A fresh in-memory write-capable DB seeded with a single market
    /// (id 1, chain 1) + a single asset (id 1, aToken rev 1) + its erc20
    /// parents. Mirrors run.rs's `fresh_db` test harness.
    fn fresh_db_with_asset() -> DegenbotDb {
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
            // Seed a POOL contract row (the parser's `lookup_pool_revision_on_conn`).
            conn.execute(
                "INSERT INTO aave_v3_contracts (market_id, name, address, revision) \
                 VALUES (1, 'POOL', '0xpool', 9)",
                [],
            )
            .unwrap();
            // Seed the erc20 parents: underlying (id 1), aToken (id 2), vToken (id 3).
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES \
                 (1, 1, '0xunderlying'), (2, 1, '0xatoken'), (3, 1, '0xvtoken')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 2, 1, 3, 1, '1000000000000000000000000000', '0', '1000000000000000000000000000', '0')",
                [],
            )
            .unwrap();
        }
        db
    }

    /// Construct a synthetic Pool `SUPPLY` event log + its matched aToken
    /// `Mint` event log. The Pool event's `data` word 0 = the supplyAmount;
    /// the aToken Mint's `value`/`balance_increase`/`index` fields drive the
    /// enricher + processor.
    /// Construct a minimal `Log` for tests.
    fn make_log(idx: u64, address: Address, topics: Vec<B256>, data: Bytes) -> Log {
        let inner = AlloyLog::new_unchecked(address, topics, data);
        Log {
            inner,
            block_hash: None,
            block_number: Some(1),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(idx),
            removed: false,
        }
    }

    /// Unit test for `extract_pool_amount_word0` — the Pool event's data word 0
    /// is the `raw_amount` (supplyAmount/withdrawAmount/borrowAmount/repayAmount).
    /// This is the enricher's `raw_amount` extraction (the plumbing-equivalence
    /// caveat — CANNOT pass `scaled_amount: None` to the processor).
    #[test]
    fn extract_pool_amount_word0_reads_data_word_0() {
        // data = [amount=0x1000 (4096), padding...] → word 0 = 4096.
        use degenbot_decoders::aave_event_decoder::SUPPLY_TOPIC;
        let amount = U256::from(4_096u64);
        let mut data = vec![0u8; 64];
        amount
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[i] = *b;
            });
        let log = make_log(
            0,
            Address::from([0xAA; 20]),
            vec![
                SUPPLY_TOPIC,
                B256::left_padding_from(Address::from([0x01; 20]).as_slice()),
                B256::left_padding_from(Address::from([0x02; 20]).as_slice()),
                B256::ZERO,
            ],
            Bytes::from(data),
        );
        assert_eq!(extract_pool_amount_word0(&log), U256::from(4_096u64));
    }

    /// Unit test for `operation_sort_key` — operations with a `pool_event`
    /// sort by its `logIndex`; operations without sort by their minimum
    /// `scaled_event` `logIndex`.
    #[test]
    fn operation_sort_key_uses_pool_event_log_index() {
        use degenbot_decoders::aave_event_decoder::SUPPLY_TOPIC;
        let pool_log = make_log(
            5,
            Address::ZERO,
            vec![SUPPLY_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            Bytes::default(),
        );
        // Mirror an Operation with a pool_event at logIndex 5.
        let op_with_pool = Operation {
            operation_id: 0,
            operation_type: OperationType::Supply,
            pool_revision: 9,
            pool_event: Some(&pool_log),
            scaled_events: Vec::new(),
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };
        assert_eq!(operation_sort_key(&op_with_pool), 5);
    }

    /// DEFERRED end-to-end synthetic-SUPPLY-tx integration test — the
    /// fixture must match the parser's exact topic/data decode shape (4
    /// indexed Supply topics + 2-word data; the aToken Mint's 3 indexed
    /// topics + 3-word data). Constructing byte-exact RPC-shape logs that the
    /// parser's `extract_pool_events` + `decode_mint_event` accept is
    /// HQF5NQ-C2's concern — flagged for the integration-fixture sub-task.
    #[test]
    #[ignore = "HQF5NQ-C2: end-to-end synthetic-tx fixture (byte-exact log shapes)"]
    fn process_transaction_supply_writes_collateral_position() {
        let _db = fresh_db_with_asset();
    }
}
