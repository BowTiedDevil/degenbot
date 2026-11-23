//! The per-tx apply dispatch glue — the bridge from parsed `Operation`s to
//! substrate [`AaveChunkEvent`] variants the orchestrator batches +
//! `apply_aave_chunk_writes_on_conn` stamps.
//!
//! # One rounding precedence
//!
//! The scaled-token pipeline has exactly one rounding source: the enrich stage
//! reads the Pool event's `raw_amount` and divides it through the token
//! revision's on-chain `TokenMath` row (`enricher_collateral_mint` /
//! `enricher_debt_mint` / `enricher_collateral_burn` / `enricher_debt_burn`),
//! handing the result to the processors as
//! `ScaledTokenEventData.scaled_amount`. A [`ScaledTokenProcessor`] entry point
//! stamps the delta's sign and passes the index through; it never divides, so
//! no dispatch path can re-round an amount on its way to the position row. The
//! parity-gate cross-check is the final arbiter.
//!
//! # Dispatch surface
//!
//! Every parsed `Operation` kind has an apply path: the 5 standard pool-event
//! operations (Supply/Withdraw/Borrow/Repay/RepayWithAtokens), the standalone
//! `BalanceTransfer`, the InterestAccrual/InterestOnly paths, the liquidation
//! apply (`OperationType::Liquidation`/`GhoLiquidation`, sized by the
//! `LiquidationPatternContext` aggregation), the GHO-discount machinery (the
//! `discount_updates_by_log_index` map + `get_effective_discount_at_log_index`
//! path + the stkAAVE pre-processing), the `DeficitCoverage` paired-event
//! apply, and the `MintToTreasury` v8 `ray_div` branch.
//!
//! [`ProcessTxError::Deferred`] therefore marks no unfinished feature. It fires
//! only on internal-shape exhaustiveness: an `Operation` missing a field the
//! parser guarantees (`pool_event`, `minted_to_treasury_amount`), or a
//! `ScaledTokenEventType` reaching an arm its calling dispatch already
//! excluded.

use crate::gho_processor::{GhoProcessorError, UnifiedGhoProcessor};
use crate::operations::{
    LiquidationPattern, LiquidationPatternContext, Operation, OperationType, ScaledTokenEvent,
    ScaledTokenEventType,
};
use crate::operations_parser::{
    addr_to_hex, log_idx_value, parse_address, TransactionOperationsParser,
};
use crate::processors::{
    enricher_collateral_burn, enricher_collateral_mint, enricher_debt_burn, enricher_debt_mint,
    ProcessorError, ScaledTokenEventData, ScaledTokenProcessor,
};
use crate::ray_div;
use crate::run::AaveChunkEvent;
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_db::{DegenbotDb, ScaledTokenPosition};
use rusqlite::Connection;

use super::evidence::TransactionEvidence;
use super::run::substrate::ChunkSubstrate;
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Errors from `process_transaction`.
#[derive(Debug, Error)]
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
    /// A `UnifiedGhoProcessor` failure (ray-math / percentage-math overflow /
    /// delta overflow).
    #[error("GHO processor error: {0}")]
    GhoProcessor(#[from] GhoProcessorError),
    /// A ray-math failure from the enrichment's `ray_div`.
    #[error("ray-math error: {0}")]
    RayMath(#[from] crate::WadRayError),
    /// An internal-shape violation: an `Operation` missing a field the parser
    /// guarantees, or a `ScaledTokenEventType` reaching a dispatch arm its
    /// caller already excluded. No feature path returns it.
    #[error("deferred: {0}")]
    Deferred(String),
}

/// The per-tx entry point the orchestrator calls: parses the tx logs into
/// `Operation`s + dispatches each Operation's constituent scaled-token events to
/// [`AaveChunkEvent`] variants (the enricher + processor pipeline). Returns
/// the accumulated events for the orchestrator to batch + stamp via
/// `apply_aave_chunk_writes_on_conn`.
///
/// The `conn` is the caller's chunk `Transaction`'s borrowed `&Connection`
/// (every substrate lookup goes through it — the chunk-atomicity invariant).
///
/// # Errors
///
/// Returns [`ProcessTxError`] on any parse / substrate / processor / ray-math
/// failure, or [`ProcessTxError::Deferred`] when an `Operation` reaches the
/// dispatch missing a field the parser guarantees.
///
/// `discounts` is the orchestrator-pre-fetched GHO discount snapshot (the
/// `raw_call` for users-not-in-DB discounts is the driver-shell's
/// concern). Keyed by user address → the discount percent (basis
/// points, `10_000 == 100.00%`) in effect at the tx's start.
/// [`GhoDiscountContext`] resolves the EFFECTIVE discount per event (adjusting
/// for in-tx `DISCOUNT_PERCENT_UPDATED` events).
#[expect(
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::implicit_hasher
)]
pub fn process_transaction(
    market_id: i64,
    chain_id: i64,
    pool_address: Address,
    gho_token_address: Option<Address>,
    gho_vtoken_address: Option<Address>,
    conn: &Connection,
    evidence: &TransactionEvidence,
    discounts: &HashMap<Address, U256>,
    substrate: &mut ChunkSubstrate,
) -> Result<Vec<AaveChunkEvent>, ProcessTxError> {
    let mut parser = TransactionOperationsParser::new(
        market_id,
        chain_id,
        pool_address,
        gho_token_address,
        gho_vtoken_address,
        conn,
        &mut *substrate,
    )?;
    let tx_logs: Vec<&Log> = evidence.logs();
    let parsed = parser.parse_evidence(evidence, evidence.tx().hash().0)?;

    // Build the GHO-discount context once per tx (the
    // `discount_updates_by_log_index` map + the `bad_debt_users` set): the
    // DISCOUNT_PERCENT_UPDATED scan + the DEFICIT_CREATED scan.
    let gho_ctx = GhoDiscountContext::new(&tx_logs, discounts, gho_vtoken_address, pool_address);

    // Build the per-tx liquidation-pattern context (the port of
    // `_preprocess_liquidation_aggregates` → `detect_liquidation_patterns`).
    // Drives the COMBINED_BURN aggregated-burn + Mint-skip behavior in
    // `dispatch_liquidation` (N liquidations share 1 burn).
    let mut liq_patterns =
        build_liquidation_patterns(conn, market_id, &parsed.operations, substrate);

    // Sort the parsed operations by pool_event logIndex (or minimum
    // scaled_event log_index for the no-pool-event operations — INTEREST_
    // ACCRUAL / MintToTreasury) — the apply order the DB rows are stamped in.
    let mut sorted_ops: Vec<&Operation> = parsed.operations.iter().collect();
    sorted_ops.sort_by_key(|op| operation_sort_key(op));

    // The per-tx running-state map for GHO debt positions: every event in one
    // tx threads the running balance/index instead of re-reading the pre-tx
    // snapshot. Per-tx scope (the run-loop's
    // `apply_chunk_events_on_conn` flushes the events Vec right after this
    // fn returns, so the next tx's reads are fresh).
    let mut gho_running_state: HashMap<i64, (U256, U256)> = HashMap::new();

    let mut events: Vec<AaveChunkEvent> = Vec::new();
    for op in &sorted_ops {
        dispatch_operation(
            op,
            market_id,
            conn,
            gho_vtoken_address,
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut gho_running_state,
            substrate,
        )?;
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

/// Build the per-tx liquidation-pattern context. Groups the tx's liquidation
/// ops by
/// `(user, debt_v_token)`, attaches the matched debt burns (by vToken emitter
/// address), and computes the SINGLE / `COMBINED_BURN` / `SEPARATE_BURNS` pattern
/// per group.
///
/// The `debt_v_token` for each `LiquidationCall` is resolved from the call's
/// indexed `debtAsset` (topic 2) via the asset substrate (underlying → vToken
/// mirrors `_get_v_token_for_asset`). The matched burns' `debt_v_token` is
/// their vToken emitter (`ev.token_address`), which equals the
/// `LiquidationCall`'s resolved vToken — so burns attach to the same group.
///
/// Non-liquidation ops + liquidation ops with an unresolvable vToken are
/// skipped. Infallible
/// (a substrate miss degrades to `None` → no aggregation → the per-op path
/// runs unchanged).
fn build_liquidation_patterns(
    conn: &Connection,
    market_id: i64,
    operations: &[Operation<'_>],
    substrate: &mut ChunkSubstrate,
) -> LiquidationPatternContext {
    let mut ctx = LiquidationPatternContext::default();
    // 1. Group liquidations by (user, debt_v_token); record each call's
    //    `(operation_id, debt_to_cover, pool_event_log_index)`.
    for op in operations {
        if !matches!(
            op.operation_type,
            OperationType::Liquidation | OperationType::GhoLiquidation
        ) {
            continue;
        }
        let Some(pool_event) = op.pool_event else {
            continue;
        };
        let topics = pool_event.topics();
        if topics.len() < 4 {
            continue;
        }
        let user = Address::from_word(topics[3]);
        let debt_asset = Address::from_word(topics[2]);
        let Some(row) = substrate
            .lookup_asset_row(conn, market_id, "underlying", &addr_to_hex(debt_asset))
            .ok()
            .flatten()
        else {
            continue;
        };
        let Some(v_token) = parse_address(&row.v_token_address) else {
            continue;
        };
        let key = (user, v_token);
        let group = ctx.groups.entry(key).or_default();
        group.liquidations.push((
            op.operation_id,
            op.debt_to_cover.unwrap_or(U256::ZERO),
            log_idx_value(pool_event),
        ));
        // 2. Attach the matched debt burns carried by THIS op's
        //    `scaled_events` (the parser already matched them via
        //    `collect_debt_burns`; the burn's vToken emitter IS the group's
        //    `debt_v_token`). SKIP a burn whose `(user, v_token)` key is
        //    NOT already an existing liquidation group. This is critical: a
        //    standalone (non-liquidation) repay burn, or a burn whose vToken
        //    doesn't match the LC's debt vToken, must NOT create a spurious
        //    0-liquidation group — such a group classifies as `CombinedBurn`
        //    with `total_debt_to_cover() == 0`, and `dispatch_liquidation`'s
        //    COMBINED_BURN override would size the burn by 0 (the GHO
        //    debt-burn zeroing bug). Dedup by log_index — the per-op iteration
        //    can only ever see each burn once (the parser attaches it to a
        //    single op — `COMBINED_BURN` position 0).
        for ev in &op.scaled_events {
            if matches!(
                ev.event_type,
                ScaledTokenEventType::DebtBurn | ScaledTokenEventType::GhoDebtBurn
            ) {
                let burn_key = (ev.user_address, ev.token_address);
                let Some(g) = ctx.groups.get_mut(&burn_key) else {
                    continue;
                };
                if !g.burn_events.iter().any(|(li, _)| *li == ev.log_index) {
                    g.burn_events.push((ev.log_index, ev.amount));
                }
            }
        }
    }
    // 3. Detect the pattern per group (`LiquidationGroup::detect_pattern`).
    for (key, group) in &ctx.groups {
        ctx.patterns.insert(*key, group.detect_pattern());
    }
    ctx
}

/// Dispatch a single Operation's scaled-token events → [`AaveChunkEvent`]
/// variants — the per-event-type dispatch loop.
#[expect(clippy::too_many_arguments)]
fn dispatch_operation(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    gho_vtoken_address: Option<Address>,
    gho_ctx: &GhoDiscountContext,
    liq_patterns: &mut LiquidationPatternContext,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    match op.operation_type {
        OperationType::Supply
        | OperationType::Withdraw
        | OperationType::Borrow
        | OperationType::Repay
        | OperationType::RepayWithAtokens => {
            dispatch_standard(op, market_id, conn, events, substrate)
        }
        OperationType::BalanceTransfer => {
            dispatch_balance_transfer(op, market_id, conn, events, substrate)
        }
        // Standalone interest accrual (amount == balance_increase). Delta = 0
        // (only the index updates — the apply fn reconciles `last_index`).
        // The Operation carries the scaled events; pass them through as
        // zero-delta mints/burns so the position's `last_index` advances.
        OperationType::InterestAccrual => dispatch_interest_accrual(
            op,
            market_id,
            conn,
            gho_vtoken_address,
            gho_ctx,
            events,
            gho_running_state,
            substrate,
        ),
        OperationType::Liquidation => dispatch_liquidation(
            op,
            market_id,
            conn,
            gho_ctx,
            liq_patterns,
            events,
            gho_running_state,
            substrate,
        ),
        OperationType::GhoLiquidation => dispatch_gho_liquidation(
            op,
            market_id,
            conn,
            gho_vtoken_address,
            gho_ctx,
            events,
            gho_running_state,
            substrate,
        ),
        OperationType::GhoBorrow | OperationType::GhoRepay | OperationType::GhoFlashLoan => {
            dispatch_gho_standard(
                op,
                market_id,
                conn,
                gho_vtoken_address,
                gho_ctx,
                events,
                gho_running_state,
                substrate,
            )
        }
        OperationType::DeficitCoverage => {
            dispatch_deficit_coverage(op, market_id, conn, events, substrate)
        }
        OperationType::MintToTreasury => {
            dispatch_mint_to_treasury(op, market_id, conn, events, substrate)
        }
        OperationType::StkAaveTransfer => {
            // Pre-processed by the orchestrator (the stkAAVE transfers run
            // BEFORE GHO operations for the discount computation); no apply
            // here. The orchestrator constructs `AaveChunkEvent::StkAaveStaked`
            // / `StkAaveRedeem` directly from the decoded ERC20 Transfer.
            Ok(())
        }
        OperationType::Unknown => {
            // DEFICIT_CREATED stays UNKNOWN to avoid interfering with
            // liquidation processing downstream — no apply, no event.
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
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let pool_event = op.pool_event.ok_or_else(|| {
        ProcessTxError::Deferred(format!(
            "standard op {:?} has no pool_event — enricher cannot extract raw_amount",
            op.operation_type
        ))
    })?;
    // Sort scaled_events by logIndex (mirror of `_process_operation`).
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        // Use the per-event-type raw-amount extractor (the standard path
        // resolves to word 0; the liquidation path, handled in
        // `dispatch_liquidation`, resolves per event type).
        let ev_raw = extract_raw_amount_for_event(pool_event, ev, op);
        let chunk_event =
            build_scaled_event_chunk_event(ev, op, ev_raw, market_id, conn, substrate)?;
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

/// Extract data word 1 (bytes 32..64) from a Pool event's `data` field. This
/// is the `liquidatedCollateralAmount` for `LiquidationCall` (the collateral-
/// extraction path of `RawAmountExtractor::extract_liquidation_collateral`).
fn extract_pool_amount_word1(pool_event: &Log) -> U256 {
    let data = pool_event.data().data.as_ref();
    if data.len() < 64 {
        return U256::ZERO;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[32..64]);
    U256::from_be_bytes::<32>(buf)
}

/// The per-event-type raw-amount extractor for an Operation (mirrors
/// `EnrichmentContext::extract_pool_amount`). For standard operations
/// (Supply/Withdraw/Borrow/Repay), the `raw_amount` is data word 0. For
/// `LiquidationCall`, the `raw_amount` depends on the scaled event's type:
/// debt burn/transfer → word 0 (`debtToCover`); collateral burn/transfer →
/// word 1 (`liquidatedCollateralAmount`). This is the
/// `extract_liquidation_debt` / `extract_liquidation_collateral` special
/// path.
#[expect(clippy::match_same_arms)] // the debt-burn + fallback arms both return word0 (the LiquidationCall word0 = debtToCover)
fn extract_raw_amount_for_event(pool_event: &Log, ev: &ScaledTokenEvent, op: &Operation) -> U256 {
    let is_liquidation = op.operation_type == OperationType::Liquidation
        || op.operation_type == OperationType::GhoLiquidation;
    if is_liquidation {
        match ev.event_type {
            ScaledTokenEventType::DebtBurn
            | ScaledTokenEventType::DebtTransfer
            | ScaledTokenEventType::Erc20DebtTransfer => extract_pool_amount_word0(pool_event),
            ScaledTokenEventType::CollateralMint
            | ScaledTokenEventType::CollateralBurn
            | ScaledTokenEventType::CollateralTransfer
            | ScaledTokenEventType::Erc20CollateralTransfer => {
                extract_pool_amount_word1(pool_event)
            }
            _ => extract_pool_amount_word0(pool_event),
        }
    } else {
        // the per-event selector: Supply + Borrow encode `user`
        // (address) at data word 0, with `amount` at word 1; Repay, RepayWithAtokens,
        // and Withdraw encode `amount` at word 0. Using word 0 uniformly here
        // produced SENDER balances equal to the user ADDRESS interpreted as
        // a U256 (the "~10^69 oddity").
        match op.operation_type {
            OperationType::Supply | OperationType::Borrow | OperationType::GhoBorrow => {
                extract_pool_amount_word1(pool_event)
            }
            OperationType::Repay
            | OperationType::RepayWithAtokens
            | OperationType::Withdraw
            | OperationType::GhoRepay => extract_pool_amount_word0(pool_event),
            _ => extract_pool_amount_word0(pool_event),
        }
    }
}

/// Build an [`AaveChunkEvent`] from a single scaled-token event (the
/// Mint-exceeds → Burn edge + the standard Mint/Burn/Transfer paths). The
/// `raw_amount` is the Pool event's amount (the enricher's `raw_amount`);
/// the `scaled_amount` is computed as `ray_div(raw_amount, index,
/// strategy)` from the Pool word — never `scaled_amount: None`, which would
/// let the processor recompute from `value ± balance_increase`.
#[expect(clippy::too_many_lines)]
fn build_scaled_event_chunk_event(
    ev: &ScaledTokenEvent,
    op: &Operation,
    raw_amount: U256,
    market_id: i64,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
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
                "non-scaled-token event_type {:?} in standard op",
                ev.event_type
            )));
        }
    };
    let asset = substrate
        .lookup_asset_row(conn, market_id, token_type, &token_addr_str)?
        .ok_or_else(|| {
            ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                "no asset for token {token_addr_str} ({token_type}) in market {market_id}"
            )))
        })?;

    let balance_increase = ev.balance_increase.unwrap_or_default();
    let index = ev.index.unwrap_or_default();

    // DEBUG: per-event trace (env-gated). Emits the dispatch inputs so the
    // Mint-vs-Burn variant + the enriched `raw_amount` (+ `value`,
    // `balance_increase`, `index`) can be audited per event — narrows the
    // exact rounding/raw_amount site for a divergent position. Keys via
    // `log_index` (cross-ref the per-tx balance trace).
    degenbot_core::diag!(
        domain = aave,
        li = ev.log_index,
        op = ?op.operation_type,
        ev = ?ev.event_type,
        user = %alloy::hex::encode(ev.user_address),
        raw = %raw_amount,
        value = %ev.amount,
        balinc = %balance_increase,
        idx = %index,
        "AAVE-EVTRACE"
    );

    // The processor only stamps the delta's sign — the rounding arrives in the
    // enriched `scaled_amount` each arm below computes from the token
    // revision's TokenMath row — so the side is the whole difference.
    let (position, processor) = if token_type == "a_token" {
        (
            ScaledTokenPosition::Collateral,
            ScaledTokenProcessor::collateral(),
        )
    } else {
        (ScaledTokenPosition::Debt, ScaledTokenProcessor::debt())
    };

    match ev.event_type {
        ScaledTokenEventType::CollateralMint
        | ScaledTokenEventType::DebtMint
        | ScaledTokenEventType::GhoDebtMint => {
            // DEBT-REPAY interest-exceeds edge: a `DebtMint` event inside a
            // `Repay`/`RepayWithAtokens` op where accrued interest >
            // repayAmount — the vToken contract emits a Mint whose NET effect
            // is burning the scaled repayment amount (Aave V3 `_burnScaled`:
            // the Mint-of-the-payed-interest falsifies the simple `value <
            // balance_increase` reading). The dispatch does NOT route this
            // through `process_mint_event`: it decodes `repayAmount` from the
            // Repay pool event, computes the burn's scaled amount as
            // `ray_div_floor(repayAmount, index)` for V4/V5, and dispatches a
            // `DebtBurn` sized by that value. The Pool word is the amount
            // that couples to chain, because the event's own `value` is rounded
            // relative to `balance_increase - repayAmount` by ±1 wei — at block
            // 23_093_579 (`repayAmount = 10_000_000`, `balance_increase =
            // 461_000_000`, Mint `value = 451_000_001`) `balance_increase -
            // value` floor-divides to 8_437_165 where the Pool word floor-divides
            // to 8_437_166; one wei per repay accumulates until the final full
            // repay underflows the scaled balance.
            let is_debt_repay_mint_edge = token_type == "v_token"
                && matches!(ev.event_type, ScaledTokenEventType::DebtMint)
                && matches!(
                    op.operation_type,
                    OperationType::Repay
                        | OperationType::RepayWithAtokens
                        | OperationType::Liquidation
                        | OperationType::GhoLiquidation
                        | OperationType::GhoRepay
                )
                && ev.amount < balance_increase;
            // The interest-exceeds edge for aToken: a WITHDRAW, a
            // REPAY_WITH_ATOKENS or a liquidation where
            // `amount < balance_increase` — the Mint is emitted as an effective
            // Burn, sized by the Pool's own word for that operation.
            let is_collateral_interest_exceeds_edge = matches!(
                op.operation_type,
                OperationType::Withdraw
                    | OperationType::RepayWithAtokens
                    | OperationType::Liquidation
                    | OperationType::GhoLiquidation
            ) && ev.amount < balance_increase;
            if is_debt_repay_mint_edge {
                // Treat the Mint as a DebtBurn, sized by the Pool word: the
                // enriched `scaled_amount = ray_div(raw_amount, index, FLOOR)` —
                // the burn rides the Pool word.
                //
                // `enricher_debt_burn(v_token_revision)` is the on-chain
                // TokenMath row for the burn leg (`ray_div_floor` at rev ≥ 4);
                // rev ≤ 3 divides `HALF_UP` (`HalfUpRoundingMath`).
                let scaled_amount = ray_div(
                    raw_amount,
                    index,
                    enricher_debt_burn(asset.v_token_revision).into(),
                )?;
                let event_data = ScaledTokenEventData {
                    value: ev.amount,
                    balance_increase,
                    index,
                    scaled_amount: Some(scaled_amount),
                };
                let result = processor.process_debt_burn(&event_data, None)?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    position,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                    substrate,
                )?;
                Ok(AaveChunkEvent::ScaledTokenBurn {
                    position,
                    position_id,
                    balance_delta: result.balance_delta,
                    new_index: result.new_index,
                })
            } else if is_collateral_interest_exceeds_edge {
                // Treat the Mint as a Burn, sized by the Pool word through the
                // COLLATERAL BURN TokenMath row (`enricher_collateral_burn` =
                // `ExplicitRoundingMath.get_collateral_burn_scaled_amount =
                // ray_div_ceil`, rev ≥ 4). The enriched amount rides the event
                // datum, which is the burn path's precedence too: this route and
                // the `CollateralBurn` route below compute the identical delta,
                // so the `scaled_delta` argument stays `None` here.
                let scaled_amount = ray_div(
                    raw_amount,
                    index,
                    enricher_collateral_burn(asset.a_token_revision).into(),
                )?;
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
                    substrate,
                )?;
                Ok(AaveChunkEvent::ScaledTokenBurn {
                    position,
                    position_id,
                    balance_delta: result.balance_delta,
                    new_index: result.new_index,
                })
            } else {
                // Standard mint path: aToken → `process_collateral_mint`,
                // vToken → `process_debt_mint`.
                //
                // The rounding is the token revision's on-chain TokenMath row.
                // The collateral-mint row is FLOOR at rev ≥ 4 (Pool rev_9
                // `getATokenMintScaledAmount = rayDivFloor`), so a shared
                // mint/burn `HALF_UP` division lands +1 wei above chain whenever
                // `(raw_amount * RAY) % index > index // 2` — the pos-173339
                // divergence at block 23_089_231.
                let enricher_mode = if token_type == "v_token" {
                    enricher_debt_mint(asset.v_token_revision)
                } else {
                    enricher_collateral_mint(asset.a_token_revision)
                };
                let scaled_amount = ray_div(raw_amount, index, enricher_mode.into())?;
                let event_data = ScaledTokenEventData {
                    value: ev.amount,
                    balance_increase,
                    index,
                    scaled_amount: Some(scaled_amount),
                };
                let result = if token_type == "v_token" {
                    processor.process_debt_mint(&event_data)
                } else {
                    processor.process_collateral_mint(&event_data)
                }?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    position,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                    substrate,
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
            // Each side negates the enriched amount, so the arm only selects
            // which TokenMath row computed it: the debt-burn row divides FLOOR
            // (`get_debt_burn_scaled_amount`) and the collateral-burn row CEIL
            // (`get_collateral_burn_scaled_amount`) at rev ≥ 4; both divide
            // `HALF_UP` below that.
            let enricher_mode = if token_type == "v_token" {
                enricher_debt_burn(asset.v_token_revision)
            } else {
                enricher_collateral_burn(asset.a_token_revision)
            };
            let scaled_amount = ray_div(raw_amount, index, enricher_mode.into())?;
            let event_data = ScaledTokenEventData {
                value: ev.amount,
                balance_increase,
                index,
                scaled_amount: Some(scaled_amount),
            };
            let result = if token_type == "v_token" {
                // The enriched amount is the Pool word — `debtToCover` for a
                // `LiquidationCall`, `repayAmount` for a `Repay` — divided by the
                // debt-burn row. It rides the event, so
                // every dispatch path (standard repay, non-bad-debt liquidation,
                // deficit coverage, balance transfer, interest accrual) reaches
                // the processor with one precedence, and no operation type can
                // route an event to a `value + balance_increase` recompute.
                processor.process_debt_burn(&event_data, None)
            } else {
                processor.process_collateral_burn(&event_data, None)
            }?;
            let position_id = resolve_position_id(
                conn,
                market_id,
                position,
                ev.user_address,
                asset.id,
                &asset.underlying_token_address,
                substrate,
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
            // The `from` position is always resolved — the SENDER side is
            // written unconditionally (this matches the 2
            // pre-existing 0-address collateral rows on 0x83F2 / 0xD533 in
            // the recorded gold + Rust). The `to` position is resolved ONLY
            // when `to_addr != ZERO_ADDRESS` (the recorded-gold guard skips
            // the recipient leg for a zero target). Burn-to-zero legs
            // (`Transfer(from=user, to=0x0, amount)` = dust from a direct
            // aToken `transfer()` drop to the burn address) must NOT create
            // a 0x0 collateral row.
            let from_addr = ev.from_address.unwrap_or(ev.user_address);
            let to_addr = ev.target_address.unwrap_or_default();
            let from_position_id = resolve_position_id(
                conn,
                market_id,
                ScaledTokenPosition::Collateral,
                from_addr,
                asset.id,
                &asset.underlying_token_address,
                substrate,
            )?;
            let to_position_id = if to_addr == Address::ZERO {
                None
            } else {
                Some(resolve_position_id(
                    conn,
                    market_id,
                    ScaledTokenPosition::Collateral,
                    to_addr,
                    asset.id,
                    &asset.underlying_token_address,
                    substrate,
                )?)
            };
            Ok(AaveChunkEvent::ScaledTokenTransfer {
                from_position_id,
                to_position_id,
                scaled_amount: ev.amount,
                transfer_index: index,
            })
        }
        _ => Err(ProcessTxError::Deferred(format!(
            "event_type {:?} not handled in standard op",
            ev.event_type
        ))),
    }
}

/// Dispatch a standalone `BalanceTransfer` operation (no Pool event). The
/// scaled event in `op.scaled_events` is the ERC20 `Transfer` (no index).
/// Paired `BalanceTransfer` event(s) on `op.balance_transfer_events` carry
/// the SCALED balance being moved (+ the liquidity index) — those are the
/// authoritative inputs for credit/debit + `last_index` advancement
/// The BT's SCALED amount and index are the only valid inputs: dispatching
/// the ERC20's UNDERLYING amount with `transfer_index=0` debits more than the
/// credited scaled balance whenever the liquidity index is not 1, and the
/// uint128-negative guard rejects it.
fn dispatch_balance_transfer(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        // Find the paired BalanceTransfer event for this ERC20 transfer
        // (matched by token address + from + to — mirrors
        // `_match_paired_balance_transfer`). `create_transfer_operations` pairs
        // at most one BT per ERC20, so the first match wins.
        let bt_pair =
            op.balance_transfer_events
                .iter()
                .find_map(|bt_log| match decode_balance_transfer_log(bt_log) {
                    Some((bt_from, bt_to, bt_token, bt_value, bt_index))
                        if bt_token == ev.token_address
                            && bt_from == ev.from_address.unwrap_or(ev.user_address)
                            && bt_to == ev.target_address.unwrap_or_default() =>
                    {
                        Some((bt_value, bt_index))
                    }
                    _ => None,
                });
        let raw_amount = ev.amount;
        let transfer_index = match bt_pair {
            Some((_, idx)) => idx,
            None => ev.index.unwrap_or_default(),
        };
        let chunk_event =
            build_scaled_event_chunk_event(ev, op, raw_amount, market_id, conn, substrate)?;
        let chunk_event =
            override_transfer_with_paired_bt(chunk_event, bt_pair, raw_amount, transfer_index);
        events.push(chunk_event);
    }
    Ok(())
}

/// If `build_scaled_event_chunk_event` returned a `ScaledTokenTransfer` chunk
/// event for the ERC20 `Transfer` event (carrying the ERC20's UNDERLYING amount
/// as `scaled_amount`), override it with the paired `BalanceTransfer` event's
/// scaled amount (the actual scaled balance moved). For Liquidation paths
/// (handled by `dispatch_liquidation`, not here) the override is a no-op.
fn override_transfer_with_paired_bt(
    event: AaveChunkEvent,
    bt_pair: Option<(U256, U256)>,
    fallback_amount: U256,
    fallback_index: U256,
) -> AaveChunkEvent {
    let AaveChunkEvent::ScaledTokenTransfer {
        from_position_id,
        to_position_id,
        scaled_amount: _,
        transfer_index: _,
    } = event
    else {
        return event;
    };
    let (scaled_amount, transfer_index) = match bt_pair {
        Some((value, index)) => (value, index),
        None => (fallback_amount, fallback_index),
    };
    AaveChunkEvent::ScaledTokenTransfer {
        from_position_id,
        to_position_id,
        scaled_amount,
        transfer_index,
    }
}

/// Decode an aToken `BalanceTransfer(address indexed from, address indexed to,
/// uint256 value, uint256 index)` log into its raw fields.
/// Returns `(from, to, token_address, value, index)`.
fn decode_balance_transfer_log(log: &Log) -> Option<(Address, Address, Address, U256, U256)> {
    let topics = log.topics();
    if topics.len() < 3 {
        return None;
    }
    let from = Address::from_slice(&topics[1].as_slice()[12..]);
    let to = Address::from_slice(&topics[2].as_slice()[12..]);
    let token = log.address();
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    let value = U256::from_be_bytes::<32>(buf);
    buf.copy_from_slice(&data[32..64]);
    let index = U256::from_be_bytes::<32>(buf);
    Some((from, to, token, value, index))
}
#[expect(clippy::too_many_arguments)] // the substrate cache rides the existing arg list
/// Dispatch an `InterestAccrual` operation (amount == `balance_increase` →
/// delta = 0; only the position's `last_index` advances). The enricher passes
/// `scaled_amount: Some(0)` (the accrued-interest-only path — the balance
/// doesn't change, but the index does).
fn dispatch_interest_accrual(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    gho_vtoken_address: Option<Address>,
    gho_ctx: &GhoDiscountContext,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    // ALL GHO vToken Mints — including interest accrual + the
    // discount "dust mints" — route through the GHO discount processor.
    // The per-event dispatch does NOT branch on operation_type (only on
    // event_type). So a
    // `GhoDebtMint` inside an `InterestAccrual` op takes the SAME path as one
    // inside a `GhoBorrow` op.
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let is_gho = gho_vtoken_address.is_some_and(|addr| addr == ev.token_address)
            || matches!(
                ev.event_type,
                ScaledTokenEventType::GhoDebtMint
                    | ScaledTokenEventType::GhoDebtBurn
                    | ScaledTokenEventType::GhoDebtTransfer
            );
        if is_gho {
            // Interest accrual does not change the scaled balance, so the
            // interest-accrual enricher sets `scaled_amount = 0` for ALL
            // events. This matters for the
            // BORROW branch of `process_gho_debt_mint` (hit by dust mints where
            // `balance_increase == 0 < value`): V5+ uses `scaled_amount` there,
            // so it MUST be 0 (NOT `ray_div(raw_amount, index)`
            // which `build_gho_chunk_event` would compute for pool-event ops).
            // The discount math (delta for V1-V3 / pure-interest-accrual) is
            // verified byte-exact vs the Python gold at 18M.
            let (chunk_event, refresh) = build_gho_chunk_event(
                ev,
                op,
                op.pool_event,
                Some(U256::ZERO),
                gho_ctx,
                market_id,
                conn,
                gho_running_state,
                substrate,
            )?;
            events.push(chunk_event);
            if let Some(refresh_ev) = refresh {
                events.push(refresh_ev);
            }
            continue;
        }
        let token_addr_str = addr_to_hex(ev.token_address);
        let token_type = match ev.event_type {
            ScaledTokenEventType::CollateralMint | ScaledTokenEventType::CollateralBurn => {
                "a_token"
            }
            ScaledTokenEventType::DebtMint | ScaledTokenEventType::DebtBurn => "v_token",
            _ => {
                return Err(ProcessTxError::Deferred(format!(
                    "interest-accrual event_type {:?}",
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
            let r = ScaledTokenProcessor::collateral().process_collateral_mint(&event_data)?;
            (ScaledTokenPosition::Collateral, r)
        } else {
            let r = ScaledTokenProcessor::debt().process_debt_mint(&event_data)?;
            (ScaledTokenPosition::Debt, r)
        };
        let position_id = resolve_position_id(
            conn,
            market_id,
            position,
            ev.user_address,
            asset.id,
            &asset.underlying_token_address,
            substrate,
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

// ── the GHO-discount-lookup machinery ─────────────────────────────────────

/// The per-tx GHO-discount context. Computed once at the top of
/// [`process_transaction`] — its pre-pass:
/// - scans `tx_logs` for `DISCOUNT_PERCENT_UPDATED` events → builds the
///   `updates_by_log_index` map (per-user, sorted by log index).
/// - scans `tx_logs` for `DEFICIT_CREATED` events → builds the `bad_debt_users`
///   set (the bad-debt-liquidation signal).
/// - holds the orchestrator-pre-fetched `discounts` snapshot (the
///   `raw_call` for users-not-in-DB is the driver-shell's concern).
///
/// The `effective_discount(user, log_index, vtoken_rev)` resolves the discount
/// in effect at a given log index (the `get_effective_discount_at_log_index`
/// fn — the most-recent `DISCOUNT_PERCENT_UPDATED` before `log_index`, else the
/// snapshot default). For revisions without discount support (V4+), returns 0.
///
/// # The bad-debt mechanism
///
/// The override keys off a `DEFICIT_CREATED` event for the user in `tx_logs` —
/// the Pool's own record that it wrote the shortfall off. It does NOT consult
/// `user_liquidation_count`: that count says how often the user was liquidated,
/// not whether this liquidation produced a deficit, and the two disagree on a
/// repeat liquidation. A `DEFICIT_CREATED` in the tx means the contract burned
/// the ENTIRE remaining debt.
#[derive(Debug)]
pub struct GhoDiscountContext<'a> {
    /// `(user, log_index, old_discount)` — from `DISCOUNT_PERCENT_UPDATED`
    /// events, per-user, sorted ascending by log index.
    updates_by_log_index: HashMap<Address, Vec<(u64, U256)>>,
    /// The orchestrator-pre-fetched GHO discount snapshot. Keyed by user
    /// address → the discount percent (basis points) in effect at the tx's
    /// start. Users not in the map default to 0 (no discount).
    discounts: &'a HashMap<Address, U256>,
    /// Users with a `DEFICIT_CREATED` event in this tx (the bad-debt set —
    /// `_is_bad_debt_liquidation`).
    bad_debt_users: HashSet<Address>,
    /// The GHO vToken address (the `is_gho` discriminator). `None` when the
    /// market has no GHO asset.
    gho_vtoken_address: Option<Address>,
}

impl<'a> GhoDiscountContext<'a> {
    /// Construct the context by scanning `tx_logs` once: the
    /// `DISCOUNT_PERCENT_UPDATED` scan plus the `DEFICIT_CREATED` scan the
    /// bad-debt-liquidation verdict reads.
    ///
    /// Each scan is scoped to the contract that emits it for THIS market: the GHO
    /// vToken reports a user's discount and the Pool reports its own deficit. A
    /// receipt carries the whole transaction, so another deployment emitting the
    /// same signature names the same user but is another market's discount or
    /// write-off, never this one's.
    #[must_use]
    pub fn new(
        tx_logs: &[&Log],
        discounts: &'a HashMap<Address, U256>,
        gho_vtoken_address: Option<Address>,
        pool_address: Address,
    ) -> Self {
        use degenbot_decoders::aave_event_decoder::{
            AAVE_DEFICIT_CREATED_TOPIC, AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
        };
        let mut updates_by_log_index: HashMap<Address, Vec<(u64, U256)>> = HashMap::new();
        let mut bad_debt_users: HashSet<Address> = HashSet::new();
        for log in tx_logs {
            let topics = log.topics();
            if topics.is_empty() {
                continue;
            }
            if topics[0] == AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC
                && gho_vtoken_address == Some(log.address())
                && topics.len() >= 2
            {
                // DiscountPercentUpdated(user indexed, oldDiscountPercent).
                // topics[1] = the user; data word 0 = the OLD discount.
                let user = topic_to_address(topics[1]);
                let old_discount = extract_pool_amount_word0(log);
                updates_by_log_index
                    .entry(user)
                    .or_default()
                    .push((log.log_index.unwrap_or(0), old_discount));
            } else if topics[0] == AAVE_DEFICIT_CREATED_TOPIC
                && log.address() == pool_address
                && topics.len() >= 2
            {
                // DeficitCreated(user indexed, debtAsset indexed, amount).
                // topics[1] = the user whose debt is written off.
                let user = topic_to_address(topics[1]);
                bad_debt_users.insert(user);
            }
        }
        // Sort each user's updates ascending by log index.
        for v in updates_by_log_index.values_mut() {
            v.sort_by_key(|(idx, _)| *idx);
        }
        Self {
            updates_by_log_index,
            discounts,
            bad_debt_users,
            gho_vtoken_address,
        }
    }

    /// `true` if the GHO discount mechanism is active at `vtoken_rev` (V1/V2/V3).
    /// V4+ deprecated the discount → the effective discount is always 0.
    #[must_use]
    fn discount_supported(vtoken_rev: u32) -> bool {
        crate::gho_processor::gho_discount_strategy(vtoken_rev).supports_discount
    }

    /// Resolve the discount percent in effect at `log_index` for `user` on a
    /// GHO vToken at `vtoken_rev` (the most-recent in-tx update before
    /// `log_index`, else the snapshot default). Returns 0 for V4+ (no discount).
    #[must_use]
    pub fn effective_discount(&self, user: Address, log_index: u64, vtoken_rev: u32) -> U256 {
        if !Self::discount_supported(vtoken_rev) {
            return U256::ZERO;
        }
        let default = self.discounts.get(&user).copied().unwrap_or(U256::ZERO);
        self.get_effective_discount_at_log_index(user, log_index, default)
    }

    /// The most-recent `DISCOUNT_PERCENT_UPDATED` before `log_index`, else the
    /// `default`. Mirrors `TransactionContext::get_effective_discount_at_log_index`.
    #[must_use]
    fn get_effective_discount_at_log_index(
        &self,
        user: Address,
        log_index: u64,
        default_discount: U256,
    ) -> U256 {
        let Some(updates) = self.updates_by_log_index.get(&user) else {
            return default_discount;
        };
        let mut effective = default_discount;
        for &(update_log_index, old_discount) in updates {
            if update_log_index < log_index {
                effective = old_discount;
            } else {
                break;
            }
        }
        effective
    }

    /// `true` if `user` has a `DEFICIT_CREATED` event in this tx (the bad-debt
    /// signal). Mirrors `_is_bad_debt_liquidation`.
    #[must_use]
    pub fn is_bad_debt(&self, user: Address) -> bool {
        self.bad_debt_users.contains(&user)
    }

    /// `true` if `token_address` is the GHO vToken.
    #[must_use]
    pub fn is_gho_vtoken(&self, token_address: Address) -> bool {
        self.gho_vtoken_address
            .is_some_and(|addr| addr == token_address)
    }
}

/// Extract a 20-byte `Address` from a 32-byte topic (right-aligned — the
/// Solidity `address` is left-padded to 32 bytes in an indexed topic).
fn topic_to_address(topic: alloy::primitives::B256) -> Address {
    let bytes = topic.0;
    Address::from_slice(&bytes[12..])
}
#[expect(clippy::too_many_arguments)] // the substrate cache rides the existing arg list
/// Dispatch the GHO Borrow/Repay/FlashLoan operations. Mirrors the GHO branch
/// of `_process_debt_mint_with_match` / `_process_debt_burn_with_match`. Each
/// GHO scaled event is routed through the [`UnifiedGhoProcessor`] (NOT the
/// standard `ScaledTokenProcessor::debt_*` — the GHO processor carries the
/// discount surface + the V4-ROUNDING divergence). The `actual_repay_amount`
/// is threaded through from a paired `Repay` pool event for `GhoRepay` (the
/// 1-wei rounding-error avoidance param).
fn dispatch_gho_standard(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    gho_vtoken_address: Option<Address>,
    gho_ctx: &GhoDiscountContext,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let pool_event = op.pool_event;
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        // Only GHO vToken events route through the GHO processor. Non-GHO
        // events (e.g. a paired collateral aToken event in a FlashLoan) fall
        // back to the standard builder.
        let is_gho = gho_vtoken_address.is_some_and(|addr| addr == ev.token_address)
            || matches!(
                ev.event_type,
                ScaledTokenEventType::GhoDebtMint
                    | ScaledTokenEventType::GhoDebtBurn
                    | ScaledTokenEventType::GhoDebtTransfer
            );
        if !is_gho {
            // A non-GHO scaled event within a GHO operation (e.g. the
            // collateral leg of a GHO FlashLoan) → standard builder.
            let raw = pool_event.map_or(ev.amount, extract_pool_amount_word0);
            let chunk_event =
                build_scaled_event_chunk_event(ev, op, raw, market_id, conn, substrate)?;
            events.push(chunk_event);
            continue;
        }
        let (chunk_event, refresh) = build_gho_chunk_event(
            ev,
            op,
            pool_event,
            None,
            gho_ctx,
            market_id,
            conn,
            gho_running_state,
            substrate,
        )?;
        events.push(chunk_event);
        if let Some(refresh_ev) = refresh {
            events.push(refresh_ev);
        }
    }
    Ok(())
}
/// The single bad-debt reset owner shared by both liquidation dispatch
/// paths: emit the `DebtPositionReset` AND synchronize the per-tx GHO
/// running state, so reset emission and the tuple a later same-tx GHO event
/// reads cannot diverge (the apply buffer lands only after
/// `process_transaction` returns — the map is the only fresh state).
fn emit_bad_debt_reset(
    position_id: i64,
    new_index: U256,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
) {
    events.push(AaveChunkEvent::DebtPositionReset {
        position_id,
        new_index,
    });
    gho_running_state.insert(position_id, (U256::ZERO, new_index));
}

#[expect(clippy::too_many_arguments)] // the substrate cache rides the existing arg list
/// Dispatch the GHO `LiquidationCall`. Mirrors the GHO branch of
/// `_process_debt_burn_with_match` (the bad-debt override) + the GHO branch of
/// `_process_debt_mint_with_match` (the `COMBINED_BURN` Mint-skip for the
/// aggregated-burn pattern). The collateral leg goes through the standard
/// builder (aToken); the GHO debt leg goes through [`UnifiedGhoProcessor`].
fn dispatch_gho_liquidation(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    gho_vtoken_address: Option<Address>,
    gho_ctx: &GhoDiscountContext,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let pool_event = op.pool_event.ok_or_else(|| {
        ProcessTxError::Deferred(format!(
            "GHO liquidation op {:?} has no pool_event (LiquidationCall missing)",
            op.operation_type
        ))
    })?;
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let is_gho = gho_vtoken_address.is_some_and(|addr| addr == ev.token_address)
            || matches!(
                ev.event_type,
                ScaledTokenEventType::GhoDebtMint
                    | ScaledTokenEventType::GhoDebtBurn
                    | ScaledTokenEventType::GhoDebtTransfer
            );
        if is_gho {
            // The bad-debt check: a `DEFICIT_CREATED` event for the user in
            // this tx means the contract burned the ENTIRE remaining GHO debt
            // (not just `debtToCover`) → set the position balance to 0 + advance
            // `last_index`. The delta-based apply may be off by 1 wei here: the
            // contract zeroes a balance, the apply derives a delta.
            if gho_ctx.is_bad_debt(ev.user_address) {
                let asset = lookup_gho_debt_asset(ev, market_id, conn, substrate)?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    ScaledTokenPosition::Debt,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                    substrate,
                )?;
                emit_bad_debt_reset(
                    position_id,
                    ev.index.unwrap_or_default(),
                    events,
                    gho_running_state,
                );
                continue;
            }
            let (chunk_event, refresh) = build_gho_chunk_event(
                ev,
                op,
                Some(pool_event),
                None,
                gho_ctx,
                market_id,
                conn,
                gho_running_state,
                substrate,
            )?;
            events.push(chunk_event);
            if let Some(refresh_ev) = refresh {
                events.push(refresh_ev);
            }
        } else {
            // Non-GHO event within a GHO liquidation: either the aToken
            // collateral leg, OR a stranded non-GHO vToken `DebtBurn` (the
            // `is_gho` guard above excludes only the GHO vToken). Mirror
            // `dispatch_liquidation`'s bad-debt seam: a stranded non-GHO
            // debt burn whose user had a `DEFICIT_CREATED` in this tx is a
            // bad-debt write-off → the contract burns the ENTIRE remaining
            // debt; zero the position + advance `last_index` (Python's
            // `_process_debt_burn_with_match` bad-debt override). This guard
            // MUST precede `build_scaled_event_chunk_event` so the debt-burn
            // BURN arm's `Some(scaled_delta)` (the enriched `debtToCover`)
            // never reaches a bad-debt position — that sentinel `debtToCover`
            // over-burn was the GhoLiquidation crash.
            let is_debt = matches!(
                ev.event_type,
                ScaledTokenEventType::DebtBurn
                    | ScaledTokenEventType::DebtTransfer
                    | ScaledTokenEventType::Erc20DebtTransfer
            );
            if is_debt && gho_ctx.is_bad_debt(ev.user_address) {
                let token_addr_str = addr_to_hex(ev.token_address);
                let asset = DegenbotDb::lookup_asset_by_token_address_on_conn(
                    conn,
                    market_id,
                    &token_addr_str,
                    "v_token",
                )?
                .ok_or_else(|| {
                    ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                        "no vToken asset for token {token_addr_str} in market {market_id}"
                    )))
                })?;
                let position_id = resolve_position_id(
                    conn,
                    market_id,
                    ScaledTokenPosition::Debt,
                    ev.user_address,
                    asset.id,
                    &asset.underlying_token_address,
                    substrate,
                )?;
                emit_bad_debt_reset(
                    position_id,
                    ev.index.unwrap_or_default(),
                    events,
                    gho_running_state,
                );
                continue;
            }
            // The collateral leg (or a non-bad-debt stranded debt burn) →
            // standard builder.
            let ev_raw = extract_raw_amount_for_event(pool_event, ev, op);
            let chunk_event =
                build_scaled_event_chunk_event(ev, op, ev_raw, market_id, conn, substrate)?;
            events.push(chunk_event);
        }
    }
    Ok(())
}
/// Build a GHO debt `AaveChunkEvent` from a single GHO scaled-token event via
/// the [`UnifiedGhoProcessor`]. Mirrors the GHO branch of
/// `_process_debt_mint_with_match` / `_process_debt_burn_with_match`. Resolves
/// the effective discount + threads the `actual_repay_amount` (for `GhoRepay`).
#[expect(clippy::too_many_arguments, clippy::too_many_lines)]
fn build_gho_chunk_event(
    ev: &ScaledTokenEvent,
    op: &Operation,
    pool_event: Option<&Log>,
    scaled_amount_override: Option<U256>,
    gho_ctx: &GhoDiscountContext,
    market_id: i64,
    conn: &Connection,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(AaveChunkEvent, Option<AaveChunkEvent>), ProcessTxError> {
    let asset = lookup_gho_debt_asset(ev, market_id, conn, substrate)?;
    let processor = UnifiedGhoProcessor::new(asset.v_token_revision);
    let balance_increase = ev.balance_increase.unwrap_or_default();
    let index = ev.index.unwrap_or_default();
    // The enricher's `scaled_amount`. For pool-event ops (Borrow/Repay/
    // Liquidation) `None` → compute `ray_div(raw_amount, index, strategy)`
    // (the strategy is the per-revision GHO mint/burn rounding). For
    // interest-accrual ops the caller passes `Some(0)` — Python's
    // interest-accrual enricher sets `scaled_amount = 0` always, + the BORROW
    // branch (dust mints) uses it for V5+.
    let scaled_amount = if let Some(s) = scaled_amount_override {
        Some(s)
    } else {
        let raw_amount =
            pool_event.map_or(ev.amount, |pe| extract_raw_amount_for_event(pe, ev, op));
        let strat = crate::gho_processor::gho_strategy(asset.v_token_revision);
        // The rounding mode derives from the operation's shape, never from
        // the event magnitudes:
        //
        //   • GhoBorrow → the MINT strategy row (CEIL for V5)
        //   • GhoRepay → the BURN strategy row (FLOOR for V5)
        //   • the other GHO-carrying ops → event-type-based (MINT for
        //     GhoDebtMint, BURN for GhoDebtBurn)
        //
        // The heuristic
        // `if ev.amount >= balance_increase { MINT } else { BURN }` reads a GHO
        // REPAY (GhoDebtBurn) whose `Burn.value > Burn.balanceIncrease`
        // (typical when the user repays the principal, not just interest) as a
        // MINT, so it CEILs and burns 1 wei more than the FLOOR row per burn;
        // across several GHO Burns in one chunk that accumulates into a wei
        // underflow at the final full repay — the uint128-negative guard
        // blocks it.
        //
        // The deriving `ray_div` call uses the chosen
        // per-op per-event rounding mode. Note the dispatch_gho_liquidation
        // path already handles bad-debt via `DebtPositionReset` (before reaching
        // this compute).
        let rounding_mode = match op.operation_type {
            OperationType::GhoBorrow => strat.mint,
            OperationType::GhoRepay => strat.burn,
            _ => {
                if ev.event_type == ScaledTokenEventType::GhoDebtMint {
                    strat.mint
                } else {
                    strat.burn
                }
            }
        };
        Some(ray_div(raw_amount, index, rounding_mode.into())?)
    };
    let event_data = ScaledTokenEventData {
        value: ev.amount,
        balance_increase,
        index,
        scaled_amount,
    };
    // For GhoRepay, thread the actual repay amount from the paired Repay pool
    // event (the 1-wei rounding-error avoidance param).
    let actual_repay_amount = if op.operation_type == OperationType::GhoRepay {
        pool_event.map(extract_pool_amount_word0)
    } else {
        None
    };
    let position_id = resolve_position_id(
        conn,
        market_id,
        ScaledTokenPosition::Debt,
        ev.user_address,
        asset.id,
        &asset.underlying_token_address,
        substrate,
    )?;
    // Read the position's actual prev balance + last_index — the GHO
    // processor's `accrue_debt_on_action` + `get_discounted_balance` NEED
    // them to compute the discount-scaled amount (the standard
    // `ScaledTokenProcessor` does not — GHO is the exception). Mirrors the
    // Python's read of `debt_position.balance` / `.last_index`.
    //
    // THE PER-TX RUNNING STATE:
    // when a single tx has multiple GHO events for the same position
    // (e.g., borrow + accrued-interest Mint + zero-noop Mint in the same
    // `process_transaction`), each event MUST thread the running
    // `prev_balance` + `prev_index` — reading the DB pre-tx state on every
    // event re-applies the `accrue_debt_on_action` `discount_scaled` burn
    // once per event, because `balance_increase` looks non-zero against a
    // stale `prev_index` — the map stands in for the immediate
    // read-your-own-writes balance the apply buffer lands.
    //
    // The map is per-tx (the run-loop's `apply_chunk_events_on_conn` lands
    // the buffer right after `process_transaction` returns, so the next tx's
    // reads are fresh). The map subs ONLY for the GHO debt paths (the
    // `build_gho_chunk_event` callers) — the standard `dispatch_standard`
    // path uses `ScaledTokenProcessor` which does NOT consult `prev_balance`
    // (no discount math); it has no per-tx staleness.
    let (prev_balance, prev_index) =
        if let Some(&(balance, idx)) = gho_running_state.get(&position_id) {
            (balance, idx)
        } else {
            let (balance, index_opt) = substrate.lookup_position_balance_index(
                conn,
                ScaledTokenPosition::Debt,
                position_id,
            )?;
            let idx = index_opt.unwrap_or(index);
            gho_running_state.insert(position_id, (balance, idx));
            (balance, idx)
        };
    let effective_discount =
        gho_ctx.effective_discount(ev.user_address, ev.log_index, asset.v_token_revision);

    let (balance_delta, new_index, chunk_event, refresh) = match ev.event_type {
        ScaledTokenEventType::GhoDebtMint => {
            let result = processor.process_gho_debt_mint(
                &event_data,
                prev_balance,
                prev_index,
                effective_discount,
                actual_repay_amount,
            )?;
            let refresh = result
                .should_refresh_discount
                .then_some(AaveChunkEvent::GhoRefreshDiscount { position_id });
            let chunk_event = AaveChunkEvent::ScaledTokenMint {
                position: ScaledTokenPosition::Debt,
                position_id,
                balance_delta: result.balance_delta,
                new_index: result.new_index,
            };
            (result.balance_delta, result.new_index, chunk_event, refresh)
        }
        ScaledTokenEventType::GhoDebtBurn | ScaledTokenEventType::GhoDebtTransfer => {
            let result = processor.process_gho_debt_burn(
                &event_data,
                prev_balance,
                prev_index,
                effective_discount,
            )?;
            let refresh = result
                .should_refresh_discount
                .then_some(AaveChunkEvent::GhoRefreshDiscount { position_id });
            let chunk_event = AaveChunkEvent::ScaledTokenBurn {
                position: ScaledTokenPosition::Debt,
                position_id,
                balance_delta: result.balance_delta,
                new_index: result.new_index,
            };
            (result.balance_delta, result.new_index, chunk_event, refresh)
        }
        _ => {
            return Err(ProcessTxError::Deferred(format!(
                "GHO event_type {:?} not a GHO debt event",
                ev.event_type
            )));
        }
    };

    // Update the per-tx running state (mirrors
    // `apply_scaled_token_balance_delta_on_conn`); the I256-signed delta may
    // be negative (a burn or a discount_scaled-burning BORROW edge case).
    let new_balance = if balance_delta.is_negative() {
        let abs = U256::try_from(-balance_delta).unwrap_or(U256::MAX);
        prev_balance.saturating_sub(abs)
    } else {
        let abs = U256::try_from(balance_delta).unwrap_or(U256::MAX);
        prev_balance.saturating_add(abs)
    };
    let new_index_for_map = new_index.max(prev_index);
    gho_running_state.insert(position_id, (new_balance, new_index_for_map));

    Ok((chunk_event, refresh))
}

/// Look up the GHO debt asset for a scaled event's emitter (the vToken).
/// Returns the [`AssetRow`] (id + revisions + underlying address).
fn lookup_gho_debt_asset(
    ev: &ScaledTokenEvent,
    market_id: i64,
    conn: &Connection,
    substrate: &mut ChunkSubstrate,
) -> Result<degenbot_db::AssetRow, ProcessTxError> {
    let token_addr_str = addr_to_hex(ev.token_address);
    substrate
        .lookup_asset_row(conn, market_id, "v_token", &token_addr_str)?
        .ok_or_else(|| {
            ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                "no GHO vToken asset for token {token_addr_str} in market {market_id}"
            )))
        })
}

/// Dispatch the non-GHO `LiquidationCall`. Checks the bad-debt override FIRST:
/// a `DEFICIT_CREATED` event for the user in this tx means the contract burned
/// the ENTIRE remaining debt (`user_liquidation_count` is NOT consulted) → set
/// the position balance to 0 + advance `last_index`. Else, the per-event-type
/// raw-amount extractor (debt → word 0, collateral → word 1).
///
/// The bad-debt reset applies to BOTH GHO and non-GHO debt burns: a GHO debt
/// burn attached to a non-GHO `LiquidationCall` (via `collect_debt_burns`'
/// single-liquidation "collect all burns" branch) never reaches
/// `dispatch_gho_liquidation`, so the reset here is the only zeroing path.
#[expect(clippy::too_many_arguments)]
fn dispatch_liquidation(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    gho_ctx: &GhoDiscountContext,
    liq_patterns: &mut LiquidationPatternContext,
    events: &mut Vec<AaveChunkEvent>,
    gho_running_state: &mut HashMap<i64, (U256, U256)>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let pool_event = op.pool_event.ok_or_else(|| {
        ProcessTxError::Deferred(format!(
            "liquidation op {:?} has no pool_event (LiquidationCall missing)",
            op.operation_type
        ))
    })?;
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        // COMBINED_BURN: N liquidations of the same
        // `(user, debt_v_token)` share 1 burn. The aggregated burn — sized by
        // the group's `total_debt_to_cover` (handled below for `DebtBurn`) —
        // covers ALL debt reduction; the debt Mint events for this group are
        // SKIPPED entirely (the `COMBINED_BURN` early-return).
        if matches!(
            ev.event_type,
            ScaledTokenEventType::DebtMint | ScaledTokenEventType::GhoDebtMint
        ) && liq_patterns.get_pattern(ev.user_address, ev.token_address)
            == Some(LiquidationPattern::CombinedBurn)
        {
            continue;
        }
        // The bad-debt check applies only to debt events (the collateral leg
        // is unaffected — bad-debt writes off the DEBT, not the collateral).
        // The bad-debt reset applies to both GHO and non-GHO tokens for ANY
        // debt burn in a Liquidation/GhoLiquidation op. A GHO debt burn attached to a
        // NON-GHO LiquidationCall (the parser's `collect_debt_burns`
        // single-liquidation branch attaches ALL of the user's debt burns) MUST
        // be reset here: it never reaches `dispatch_gho_liquidation` (the op
        // is `Liquidation`, not `GhoLiquidation`), so there is no GHO-path
        // double-apply to guard against. An `is_gho` guard plus omitting
        // `GhoDebtBurn` from `is_debt` lets the GHO burn fall through to
        // `build_scaled_event_chunk_event` with `raw_amount` = the non-GHO
        // LiquidationCall's `debtToCover` (a dust value) → a wrongly-sized
        // burn that leaves the 1.28e15 GHO debt un-burnt.
        let is_debt = matches!(
            ev.event_type,
            ScaledTokenEventType::DebtBurn
                | ScaledTokenEventType::DebtTransfer
                | ScaledTokenEventType::Erc20DebtTransfer
                | ScaledTokenEventType::GhoDebtBurn
                | ScaledTokenEventType::GhoDebtTransfer
        );
        if is_debt && gho_ctx.is_bad_debt(ev.user_address) {
            let token_addr_str = addr_to_hex(ev.token_address);
            let asset = DegenbotDb::lookup_asset_by_token_address_on_conn(
                conn,
                market_id,
                &token_addr_str,
                "v_token",
            )?
            .ok_or_else(|| {
                ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                    "no vToken asset for token {token_addr_str} in market {market_id}"
                )))
            })?;
            let position_id = resolve_position_id(
                conn,
                market_id,
                ScaledTokenPosition::Debt,
                ev.user_address,
                asset.id,
                &asset.underlying_token_address,
                substrate,
            )?;
            emit_bad_debt_reset(
                position_id,
                ev.index.unwrap_or_default(),
                events,
                gho_running_state,
            );
            continue;
        }
        // The per-event-type raw-amount extractor resolves debt burn/transfer
        // → word 0, collateral burn/transfer → word 1.
        let mut ev_raw = extract_raw_amount_for_event(pool_event, ev, op);
        // COMBINED_BURN aggregation: size the single shared
        // debt burn by the group's aggregate `debt_to_cover` across ALL its
        // liquidations — NOT this single LiquidationCall's `debtToCover` —
        // rounded by the debt-burn rounding row.
        // `mark_processed` guards double-application; the parser attached the
        // shared burn to exactly one op (position 0), so it fires once.
        if is_debt
            && matches!(ev.event_type, ScaledTokenEventType::DebtBurn)
            && liq_patterns.get_pattern(ev.user_address, ev.token_address)
                == Some(LiquidationPattern::CombinedBurn)
        {
            if let Some(group) = liq_patterns.get_group(ev.user_address, ev.token_address) {
                if !liq_patterns.is_processed(ev.user_address, ev.token_address) {
                    ev_raw = group.total_debt_to_cover();
                    liq_patterns.mark_processed(ev.user_address, ev.token_address);
                }
            }
        }
        let chunk_event =
            build_scaled_event_chunk_event(ev, op, ev_raw, market_id, conn, substrate)?;
        events.push(chunk_event);
    }
    Ok(())
}

/// Dispatch the `DeficitCoverage` paired-event apply (Umbrella protocol):
/// process transfer events first (credit the user's collateral position),
/// then burn events (debit, including accrued interest). The burn's
/// `scaled_amount` is computed directly from the burn's `amount` + `index` —
/// the deficit-coverage burn includes interest accrued between the transfer
/// + the burn within the same tx, so the enriched path must be skipped.
fn dispatch_deficit_coverage(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    // Two-pass: transfers first (credit), then burns (debit). The BT-variant
    // `CollateralTransfer` is in `op.balance_transfer_events` and is skipped;
    // only the unscaled ERC20 `Erc20CollateralTransfer` is processed, paired
    // with its `BalanceTransfer` log, using the BT's SCALED value — NOT the
    // unscaled ERC20 amount.
    //
    // The two transfer variants carry the same logical delta at different
    // scales (`CollateralTransfer` (BT, scaled value) +
    // `Erc20CollateralTransfer` (ERC20 Transfer, UNSCALED value)), so
    // dispatching both with `scaled_amount = ev.amount` applies
    // `unscaled + scaled` — when the unscaled amount exceeds the position's
    // scaled balance the uint128-negative guard fires (the balance is uint128
    // on-chain — see AToken.rev_1.sol:1714, linearity checked; this guard is
    // CORRECT).
    //
    // The dispatch:
    //   - SKIP the `CollateralTransfer` (BT) variant entirely (the BT log is
    //     also in `op.balance_transfer_events`, paired with the ERC20 Transfer
    //     that handles the delta).
    //   - For the `Erc20CollateralTransfer` event, pair with the BT log via
    //     `op.balance_transfer_events` (mirrors `dispatch_balance_transfer`'s
    //     `find_map` lookup — same matching `bt_token + bt_from + bt_to`),
    //     then `override_transfer_with_paired_bt` to override scaled_amount
    //     with the BT's scaled value + transfer_index with the BT's index.
    //   - The deficit-coverage Burn still goes through the burn path below.
    for ev in &scaled {
        if matches!(
            ev.event_type,
            ScaledTokenEventType::CollateralTransfer
                | ScaledTokenEventType::Erc20CollateralTransfer
        ) {
            // The `CollateralTransfer` (BT) variant — it's already in
            // `op.balance_transfer_events` (the orchestrator's `create_deficit_coverage_operations`
            // inserts `bt_events` exactly from the BT-variant events). The
            // paired ERC20 Transfer handles the delta via the BT pair below.
            if ev.event_type == ScaledTokenEventType::CollateralTransfer {
                continue;
            }
            // The `Erc20CollateralTransfer` (ERC20 Transfer, unscaled) — pair
            // with the BT log in `op.balance_transfer_events` (same matching
            // as `dispatch_balance_transfer`'s `find_map` lookup — `bt_token +
            // bt_from + bt_to`; the BT log is the DeficitCoverage's
            // middle-inserted paired BT).
            let bt_pair = op.balance_transfer_events.iter().find_map(|bt_log| {
                match decode_balance_transfer_log(bt_log) {
                    Some((bt_from, bt_to, bt_token, bt_value, bt_index))
                        if bt_token == ev.token_address
                            && bt_from == ev.from_address.unwrap_or(ev.user_address)
                            && bt_to == ev.target_address.unwrap_or_default() =>
                    {
                        Some((bt_value, bt_index))
                    }
                    _ => None,
                }
            });
            let raw_amount = ev.amount;
            let transfer_index = match bt_pair {
                Some((_, idx)) => idx,
                None => ev.index.unwrap_or_default(),
            };
            let chunk_event =
                build_scaled_event_chunk_event(ev, op, raw_amount, market_id, conn, substrate)?;
            let chunk_event =
                override_transfer_with_paired_bt(chunk_event, bt_pair, raw_amount, transfer_index);
            events.push(chunk_event);
        }
    }
    for ev in &scaled {
        if matches!(ev.event_type, ScaledTokenEventType::CollateralBurn) {
            // The deficit-coverage burn: compute `scaled_amount` directly from
            // the burn's `amount` (the raw value) + `index` (the collateral-burn
            // strategy). Skip enrichment validation — the burn includes interest
            // accrued between the transfer + the burn.
            let chunk_event =
                build_scaled_event_chunk_event(ev, op, ev.amount, market_id, conn, substrate)?;
            events.push(chunk_event);
        }
    }
    Ok(())
}

/// Dispatch the `MintToTreasury` operation. Mirrors
/// `_calculate_mint_to_treasury_scaled_amount` — the
/// `PoolMath::underlying_to_scaled_collateral` revision split:
/// - rev >= 9: `ray_div_ceil(amountMinted, liquidity_index)` (reverse of `ray_mul_floor`).
/// - rev <= 8: `ray_div(amountMinted, liquidity_index, HALF_UP)` (reverse of `ray_mul` half-up).
///   The `amountMinted` field is on `op.minted_to_treasury_amount`.
pub(crate) fn dispatch_mint_to_treasury(
    op: &Operation,
    market_id: i64,
    conn: &Connection,
    events: &mut Vec<AaveChunkEvent>,
    substrate: &mut ChunkSubstrate,
) -> Result<(), ProcessTxError> {
    let minted_amount = op.minted_to_treasury_amount.ok_or_else(|| {
        ProcessTxError::Deferred(
            "MintToTreasury op has no `minted_to_treasury_amount` (DP3 field)".into(),
        )
    })?;
    let mut scaled: Vec<&ScaledTokenEvent> = op.scaled_events.iter().collect();
    scaled.sort_by_key(|e| e.log_index);
    for ev in &scaled {
        let token_addr_str = addr_to_hex(ev.token_address);
        let asset = substrate
            .lookup_asset_row(conn, market_id, "a_token", &token_addr_str)?
            .ok_or_else(|| {
                ProcessTxError::Substrate(degenbot_db::DbError::Decode(format!(
                    "no aToken asset for {token_addr_str} in market {market_id}"
                )))
            })?;
        let index = ev.index.unwrap_or_default();
        let balance_increase = ev.balance_increase.unwrap_or_default();
        // The revision split: rev >= 9 → CEIL, rev <= 8 → HALF_UP.
        let scaled_amount = if op.pool_revision >= 9 {
            crate::ray_div_ceil(minted_amount, index)?
        } else {
            ray_div(minted_amount, index, Some(crate::RayRounding::HalfUp))?
        };
        let processor = ScaledTokenProcessor::collateral();
        let event_data = ScaledTokenEventData {
            value: ev.amount,
            balance_increase,
            index,
            scaled_amount: Some(scaled_amount),
        };
        let result = processor.process_collateral_mint(&event_data)?;
        let position_id = resolve_position_id(
            conn,
            market_id,
            ScaledTokenPosition::Collateral,
            ev.user_address,
            asset.id,
            &asset.underlying_token_address,
            substrate,
        )?;
        events.push(AaveChunkEvent::ScaledTokenMint {
            position: ScaledTokenPosition::Collateral,
            position_id,
            balance_delta: result.balance_delta,
            new_index: result.new_index,
        });
    }
    Ok(())
}

/// Resolve a `position_id` via `get_or_create_*_position_on_conn`. First
/// resolves the `user_id` via `get_or_create_user_on_conn`.
fn resolve_position_id(
    conn: &Connection,
    market_id: i64,
    position: ScaledTokenPosition,
    user_address: Address,
    asset_id: i64,
    _underlying_address: &str,
    substrate: &mut ChunkSubstrate,
) -> Result<i64, ProcessTxError> {
    let user_addr_str = addr_to_hex(user_address);
    let user_id = substrate.user_id_or_create(
        conn,
        market_id,
        &user_addr_str,
        0, // gho_discount — effective discount resolution lives in the GHO-discount context
    )?;
    let position_id = match position {
        ScaledTokenPosition::Collateral => substrate.position_id_or_create(
            conn,
            ScaledTokenPosition::Collateral,
            user_id,
            asset_id,
        )?,
        ScaledTokenPosition::Debt => {
            substrate.position_id_or_create(conn, ScaledTokenPosition::Debt, user_id, asset_id)?
        }
    };
    Ok(position_id)
}

#[expect(clippy::expect_used, clippy::panic)]
#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::too_many_lines)]
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
    /// This is the enricher's `raw_amount` extraction — the dispatch must
    /// hand the processor the Pool word, never `scaled_amount: None`.
    #[test]
    fn extract_pool_amount_word0_reads_data_word_0() {
        // data = [amount=0x1000 (4096), padding...] → word 0 = 4096.
        use degenbot_decoders::aave_event_decoder::AAVE_SUPPLY_TOPIC;
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
                AAVE_SUPPLY_TOPIC,
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
        use degenbot_decoders::aave_event_decoder::AAVE_SUPPLY_TOPIC;
        let pool_log = make_log(
            5,
            Address::ZERO,
            vec![AAVE_SUPPLY_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
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

    /// `extract_pool_amount_word1` reads data word 1 (bytes 32..64) — the
    /// `liquidatedCollateralAmount` for `LiquidationCall`. This is the
    /// collateral-extraction path of `RawAmountExtractor::extract_liquidation_
    /// collateral`.
    #[test]
    fn extract_pool_amount_word1_reads_data_word_1() {
        use degenbot_decoders::aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC;
        // LiquidationCall data = [debtToCover=1000, liquidatedCollateralAmount=500, liquidator, receiveAToken].
        let debt_to_cover = U256::from(1_000u64);
        let liquidated_collateral = U256::from(500u64);
        let mut data = vec![0u8; 128];
        debt_to_cover
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[i] = *b;
            });
        liquidated_collateral
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[32 + i] = *b;
            });
        let log = make_log(
            0,
            Address::from([0xAA; 20]),
            vec![
                AAVE_LIQUIDATION_CALL_TOPIC,
                B256::left_padding_from(Address::from([0x01; 20]).as_slice()),
                B256::left_padding_from(Address::from([0x02; 20]).as_slice()),
                B256::left_padding_from(Address::from([0x03; 20]).as_slice()),
            ],
            Bytes::from(data),
        );
        assert_eq!(extract_pool_amount_word0(&log), U256::from(1_000u64));
        assert_eq!(extract_pool_amount_word1(&log), U256::from(500u64));
    }

    /// `OperationType::MintToTreasury` scaled-amount calculation — the
    /// revision split. This is
    /// the parity-gate zero-drift surface for the `MintToTreasury` path: rev >= 9 →
    /// CEIL, rev <= 8 → `HALF_UP`.
    #[test]
    fn mint_to_treasury_scaled_amount_revision_split() {
        use crate::RAY;
        // amountMinted = 1000 (underlying), liquidity_index = 3 RAY.
        // rev 9+: ray_div_ceil(1000, 3*RAY) — 1000/3 rounds up.
        let minted = U256::from(1_000u64);
        let index = U256::from(3u64) * RAY;
        let v9 = crate::ray_div_ceil(minted, index).unwrap();
        let v8 = ray_div(minted, index, Some(crate::RayRounding::HalfUp)).unwrap();
        // 1000/3 = 333.33... → ceil = 334, half_up = 333.
        assert_eq!(v9, U256::from(334u64));
        assert_eq!(v8, U256::from(333u64));
        assert_ne!(v9, v8, "the rev-9 CEIL vs rev-8 HALF_UP split must differ");
    }

    // ── GhoDiscountContext ─────────────────────────────────────────────

    /// Build a synthetic `DISCOUNT_PERCENT_UPDATED` log.
    /// The market's GHO vToken for the discount-context fixtures: the contract
    /// that reports a user's discount.
    fn fixture_gho_vtoken() -> Address {
        Address::from([0xAB; 20])
    }

    /// The market's Pool for the discount-context fixtures: the contract that
    /// reports its own deficit.
    fn fixture_pool() -> Address {
        Address::from([0x99; 20])
    }

    fn make_discount_updated_log(idx: u64, user: Address, old_discount: u64) -> Log {
        use degenbot_decoders::aave_event_decoder::AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC;
        let mut data = vec![0u8; 32];
        let val = U256::from(old_discount);
        val.to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[i] = *b;
            });
        let mut user_topic = [0u8; 32];
        user_topic[12..].copy_from_slice(user.as_slice());
        make_log(
            idx,
            fixture_gho_vtoken(),
            vec![AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC, B256::from(user_topic)],
            Bytes::from(data),
        )
    }

    /// Build a synthetic `DEFICIT_CREATED` log for a user.
    fn make_deficit_created_log(idx: u64, user: Address) -> Log {
        let mut user_topic = [0u8; 32];
        user_topic[12..].copy_from_slice(user.as_slice());
        let mut data = vec![0u8; 32];
        U256::from(1_000u64)
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[i] = *b;
            });
        make_log(
            idx,
            fixture_pool(),
            vec![
                degenbot_decoders::aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC,
                B256::from(user_topic),
            ],
            Bytes::from(data),
        )
    }

    #[test]
    fn gho_discount_context_no_discount_events_returns_default() {
        let discounts = HashMap::new();
        let user = Address::from([0x42; 20]);
        let logs: Vec<&Log> = Vec::new();
        let ctx = GhoDiscountContext::new(&logs, &discounts, None, fixture_pool());
        // V1 (discount supported): default is 0 (no entry in discounts map).
        assert_eq!(ctx.effective_discount(user, 0, 1), U256::ZERO);
        // V4 (no discount support): always 0.
        assert_eq!(ctx.effective_discount(user, 0, 4), U256::ZERO);
        assert!(!ctx.is_bad_debt(user));
        assert!(!ctx.is_gho_vtoken(Address::ZERO));
    }

    #[test]
    fn gho_discount_context_snapshot_discount_used_for_v2() {
        // V2 (discount supported): a user in the `discounts` snapshot → uses it.
        let mut discounts = HashMap::new();
        let user = Address::from([0x42; 20]);
        discounts.insert(user, U256::from(3_000u64)); // 30% discount
        let logs: Vec<&Log> = Vec::new();
        let ctx = GhoDiscountContext::new(
            &logs,
            &discounts,
            Some(fixture_gho_vtoken()),
            fixture_pool(),
        );
        assert_eq!(ctx.effective_discount(user, 50, 2), U256::from(3_000u64));
    }

    #[test]
    fn gho_discount_context_resolves_effective_at_log_index() {
        // Two DISCOUNT_PERCENT_UPDATED events → the effective discount at a given
        // log index is the OLD value of the most-recent update BEFORE it.
        let user = Address::from([0x42; 20]);
        let log1 = make_discount_updated_log(10, user, 1_000); // old was 1000
        let log2 = make_discount_updated_log(30, user, 2_000); // old was 2000
        let log_refs: Vec<&Log> = vec![&log1, &log2];
        let mut discounts = HashMap::new();
        discounts.insert(user, U256::from(5_000u64)); // snapshot default
        let ctx = GhoDiscountContext::new(
            &log_refs,
            &discounts,
            Some(fixture_gho_vtoken()),
            fixture_pool(),
        );
        // Before log1 (idx=10): default (5000).
        assert_eq!(ctx.effective_discount(user, 5, 2), U256::from(5_000u64));
        // After log1 but before log2 (idx 20): the OLD value of log1 = 1000.
        assert_eq!(ctx.effective_discount(user, 20, 2), U256::from(1_000u64));
        // After log2 (idx 40): the OLD value of log2 = 2000.
        assert_eq!(ctx.effective_discount(user, 40, 2), U256::from(2_000u64));
        // V4+ → always 0 regardless of the updates.
        assert_eq!(ctx.effective_discount(user, 40, 4), U256::ZERO);
    }

    #[test]
    fn gho_discount_context_detects_bad_debt_users() {
        let user = Address::from([0x42; 20]);
        let other = Address::from([0x99; 20]);
        let deficit_log = make_deficit_created_log(5, user);
        let other_deficit = make_deficit_created_log(7, other);
        let logs: Vec<&Log> = vec![&deficit_log, &other_deficit];
        let discounts = HashMap::new();
        let ctx = GhoDiscountContext::new(&logs, &discounts, None, fixture_pool());
        assert!(
            ctx.is_bad_debt(user),
            "user with DEFICIT_CREATED is bad debt"
        );
        assert!(
            ctx.is_bad_debt(other),
            "other user with DEFICIT_CREATED is bad debt"
        );
        assert!(
            !ctx.is_bad_debt(Address::from([0x11; 20])),
            "user without DEFICIT_CREATED is not bad debt"
        );
    }

    #[test]
    fn gho_discount_context_is_gho_vtoken_match() {
        let gho_vtoken = Address::from([0xAB; 20]);
        let discounts = HashMap::new();
        let logs: Vec<&Log> = Vec::new();
        let ctx = GhoDiscountContext::new(&logs, &discounts, Some(gho_vtoken), fixture_pool());
        assert!(ctx.is_gho_vtoken(gho_vtoken));
        assert!(!ctx.is_gho_vtoken(Address::from([0xCD; 20])));
    }

    // ── GHO interest-accrual dispatch ──────────────────────────

    /// Seed an in-memory DB with a market + a GHO debt asset (V4, no
    /// discount) whose vToken erc20 is `0xvtoken`. Returns (db, `vtoken_addr`).
    fn fresh_db_with_gho_debt_asset() -> (DegenbotDb, Address) {
        use degenbot_core::address_utils::address_to_checksum_string;
        use rusqlite::params;
        let (db, _state) = DegenbotDb::open_in_memory_for_writes().unwrap();
        // A real 20-byte vToken address (stored as its EIP-55 checksum so
        // `lookup_asset_by_token_address_on_conn` matches `addr_to_hex`).
        let vtoken = Address::from([0xAB; 20]);
        let vtoken_str = address_to_checksum_string(&vtoken);
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) \
                 VALUES (1, 1, 'mainnet', 1, NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES \
                 (1, 1, '0xgho'), (2, 1, ?1)",
                params![vtoken_str],
            )
            .unwrap();
            // GHO debt asset: underlying=1 (GHO), vToken=2, v_token_revision=4
            // (V4 — no discount; exercises the no-discount interest-accrual
            // branch of UnifiedGhoProcessor).
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 2, 4, 2, 4, '0', '0', '0', '0')",
                [],
            )
            .unwrap();
        }
        (db, vtoken)
    }

    /// Build a synthetic `GhoDebtMint` scaled event (the GHO vToken's `Mint`
    /// for pure interest accrual: `value == balance_increase`).
    fn make_gho_debt_mint_interest_event(
        log_idx: u64,
        vtoken: Address,
        user: Address,
        amount: U256,
        index: U256,
    ) -> ScaledTokenEvent<'static> {
        // `decoded` is unread by dispatch_interest_accrual / build_gho_chunk_event
        // (they consume the flat fields); pass a Mint variant for shape parity.
        let decoded = crate::operations::ScaledTokenEventData::Mint {
            caller: user,
            on_behalf_of: user,
            value: amount,
            balance_increase: amount,
            index,
        };
        let log = Box::leak(Box::new(make_log(
            log_idx,
            vtoken,
            vec![
                degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        )));
        ScaledTokenEvent {
            log,
            decoded,
            event_type: ScaledTokenEventType::GhoDebtMint,
            token_address: vtoken,
            user_address: user,
            caller_address: Some(user),
            from_address: None,
            target_address: None,
            amount,
            balance_increase: Some(amount),
            index: Some(index),
            log_index: log_idx,
        }
    }

    /// `dispatch_interest_accrual` must route a `GhoDebtMint`
    /// through the GHO discount processor (`build_gho_chunk_event` →
    /// `process_gho_debt_mint`) instead of returning `Err(Deferred)`.
    ///
    /// The coldboot→18M drive crashed at block 17699521 on this routing gap
    /// (a `GhoDebtMint` refused as an interest-accrual event). The byte-exact
    /// GHO interest-accrual math (discount, dust mints) is verified end-to-end
    /// vs the Python gold at 18M; this unit test pins the DISPATCH routing (no
    /// deferral + a `ScaledTokenMint` chunk event produced).
    #[test]
    fn dispatch_interest_accrual_routes_gho_debt_mint_through_gho_processor() {
        let (db, vtoken) = fresh_db_with_gho_debt_asset();
        let user = Address::from([0x42; 20]);
        let amount = U256::from(1_000u64);
        let index = U256::from(1_000_000_000u64); // a non-trivial borrow index
        let ev = make_gho_debt_mint_interest_event(7, vtoken, user, amount, index);
        let op = Operation {
            operation_id: 1,
            operation_type: OperationType::InterestAccrual,
            pool_revision: 9,
            pool_event: None,
            scaled_events: vec![ev],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };
        let conn = db.lock();
        let discounts = HashMap::new();
        let logs: Vec<&Log> = Vec::new();
        let gho_ctx = GhoDiscountContext::new(&logs, &discounts, Some(vtoken), fixture_pool());
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        let mut gho_running_state: HashMap<i64, (U256, U256)> = HashMap::new();
        dispatch_interest_accrual(
            &op,
            1,
            &conn,
            Some(vtoken),
            &gho_ctx,
            &mut events,
            &mut gho_running_state,
            &mut ChunkSubstrate::lazy(1),
        )
        .expect("GHO interest accrual must not defer");
        assert_eq!(
            events.len(),
            1,
            "the single GhoDebtMint must produce one chunk event"
        );
        match &events[0] {
            AaveChunkEvent::ScaledTokenMint { position, .. } => {
                assert_eq!(
                    *position,
                    ScaledTokenPosition::Debt,
                    "GHO interest accrual → debt position"
                );
            }
            other => panic!("expected ScaledTokenMint, got {other:?}"),
        }
    }

    /// `dispatch_deficit_coverage`: the `CollateralTransfer` (BT
    /// variant, with `.index` set) is SKIPPED (the BT log is in
    /// `op.balance_transfer_events`, paired with the `Erc20CollateralTransfer`
    /// that handles the delta), and the `Erc20CollateralTransfer` event is
    /// paired with the BT log via `decode_balance_transfer_log` lookup
    /// (`dispatch_balance_transfer`'s `find_map` +
    /// `override_transfer_with_paired_bt`) — its `scaled_amount` is the BT's
    /// SCALED value, NOT the unscaled ERC20 Transfer amount.
    ///
    /// Dispatching both the `Erc20CollateralTransfer` (unscaled `ev.amount`)
    /// and the `CollateralTransfer` (BT, scaled `ev.amount`) as `chunk_events`
    /// double-bills the position: the unscaled amount `168401963` over-debits
    /// the scaled balance `148831960` and the uint128-negative guard rejects
    /// it (on-chain `UserState.balance` is uint128, `AToken.rev_1.sol:1712`).
    #[test]
    fn dispatch_deficit_coverage_pairs_erc20_transfer_with_paired_balance_transfer() {
        use crate::RAY;
        use degenbot_core::address_utils::address_to_checksum_string;
        use degenbot_decoders::aave_event_decoder::AAVE_BALANCE_TRANSFER_TOPIC;
        use degenbot_decoders::aave_event_decoder::AAVE_BURN_TOPIC;
        use rusqlite::params;
        let db = fresh_db_with_asset();
        let a_token = Address::from([0x98; 20]);
        let a_token_str = address_to_checksum_string(&a_token);
        {
            let conn = db.lock();
            // Replace the seeded asset row (which has aToken at '0xatoken' —
            // a non-hex placeholder) with a real 20-byte aToken address so
            // `lookup_asset_by_token_address_on_conn` matches `addr_to_hex`.
            conn.execute("DELETE FROM aave_v3_assets WHERE id=1", [])
                .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (4, 1, ?1)",
                params![a_token_str],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (1, 1, 1, 4, 1, 3, 1, ?1, '0', '0', '0')",
                params![RAY.to_string()],
            )
            .unwrap();
        }
        let user = Address::from([0x53; 20]);
        let recipient = Address::from([0xD4; 20]);
        let index = U256::from(1_131_490_601_199_816u64) * RAY; // a real liquidity index
                                                                // The unscaled ERC20 Transfer amount (= the underlying deposit transfer).
                                                                // 168401963 / index ≈ 148831959 — the unscaled amount EXCEEDS the
                                                                // user's scaled balance (148831960) by the difference between the
                                                                // unscaled + the scaled: `168401963 - 148831959 = 19_570_004` → the
                                                                // uint128 guard fires.
        let unscaled_amount = U256::from(168_401_963u64);
        let bt_scaled = U256::from(148_831_959u64);

        // 1) Build the ERC20 Transfer event log (idx 214): it carries
        // `ev.amount = unscaled_amount`, the unscaled leg the pairing must
        // override.
        // The Transfer log has 3 indexed topics + 32-byte data. (For the
        // Erc20CollateralTransfer variant the parser does NOT read the
        // topics' order precedence, but constructs the `ScaledTokenEvent`
        // with the decoded fields.)
        let transfer_log = Box::leak(Box::new(make_log(
            214,
            a_token,
            vec![
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(recipient.as_slice()),
            ],
            Bytes::from({
                let mut d = vec![0u8; 32];
                unscaled_amount
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[i] = *b);
                d
            }),
        )));
        let erc20_xfer = ScaledTokenEvent {
            log: transfer_log,
            decoded: crate::operations::ScaledTokenEventData::Transfer {
                from: user,
                to: recipient,
                value: unscaled_amount,
            },
            event_type: ScaledTokenEventType::Erc20CollateralTransfer,
            token_address: a_token,
            user_address: user,
            caller_address: None,
            from_address: Some(user),
            target_address: Some(recipient),
            amount: unscaled_amount,
            balance_increase: None,
            index: None,
            log_index: 214,
        };

        // 2) Build the BT event log (idx 216) — this is the
        // `CollateralTransfer` BT-variant that holds the SCALED amount. It's
        // also the log in `op.balance_transfer_events` (the orchestrator's
        // `create_deficit_coverage_operations` inserts the BT-variant at
        // `paired_events[1]` + pushes its `&Log` into `bt_events`).
        let bt_value = bt_scaled;
        let bt_log = Box::leak(Box::new(make_log(
            216,
            a_token,
            vec![
                AAVE_BALANCE_TRANSFER_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(recipient.as_slice()),
            ],
            Bytes::from({
                let mut d = vec![0u8; 64];
                bt_value
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[i] = *b);
                index
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[32 + i] = *b);
                d
            }),
        )));
        let bt_evt = ScaledTokenEvent {
            log: bt_log,
            decoded: crate::operations::ScaledTokenEventData::BalanceTransfer {
                from: user,
                to: recipient,
                value: bt_value,
                index,
            },
            event_type: ScaledTokenEventType::CollateralTransfer,
            token_address: a_token,
            user_address: user,
            caller_address: None,
            from_address: Some(user),
            target_address: Some(recipient),
            amount: bt_value,
            balance_increase: Some(U256::ZERO),
            index: Some(index),
            log_index: 216,
        };

        // 3) Build the Burn event (deficit-coverage burn) — the second-pass
        // burn handler produces a `ScaledTokenBurn` chunk_event (NOT a transfer),
        // so it doesn't affect the transfer-leg count assertion.
        // we build a complete Burn log so `build_scaled_event_chunk_event`'s
        // collateral-burn arm can compute a scaled amount (via `ray_div`).
        let burn_amount = U256::from(100u64);
        let burn_log = Box::leak(Box::new(make_log(
            217,
            a_token,
            vec![
                AAVE_BURN_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::from({
                let mut d = vec![0u8; 96];
                burn_amount
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[i] = *b);
                U256::ZERO
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[32 + i] = *b);
                index
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[64 + i] = *b);
                d
            }),
        )));
        let burn_evt = ScaledTokenEvent {
            log: burn_log,
            decoded: crate::operations::ScaledTokenEventData::Burn {
                from: user,
                target: user,
                value: burn_amount,
                balance_increase: U256::ZERO,
                index,
            },
            event_type: ScaledTokenEventType::CollateralBurn,
            token_address: a_token,
            user_address: user,
            caller_address: None,
            from_address: Some(user),
            target_address: Some(user),
            amount: burn_amount,
            balance_increase: Some(U256::ZERO),
            index: Some(index),
            log_index: 217,
        };

        // 4) Construct the DeficitCoverage op — mirrors the orchestrator's
        // `create_deficit_coverage_operations` `paired_events = [erc20, BT, burn]`
        // ordering + `balance_transfer_events = [BT log]`.
        let op = Operation {
            operation_id: 1,
            operation_type: OperationType::DeficitCoverage,
            pool_revision: 9,
            pool_event: None,
            scaled_events: vec![erc20_xfer, bt_evt, burn_evt],
            transfer_events: Vec::new(),
            balance_transfer_events: vec![bt_log],
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };

        let conn = db.lock();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        dispatch_deficit_coverage(&op, 1, &conn, &mut events, &mut ChunkSubstrate::lazy(1))
            .expect("DeficitCoverage dispatch must not defer");

        // Expectation: exactly TWO chunk_events — ONE `ScaledTokenTransfer`
        // (the ERC20 paired with the BT — the BT-supplied scaled amount),
        // the other `ScaledTokenBurn` (the deficit-coverage burn). The
        // `CollateralTransfer` (BT) VARIANT is SKIPPED (the BT log is in
        // `op.balance_transfer_events`, paired with the ERC20 Transfer).
        // A THIRD event (= ERC20 + BT + Burn) would double-bill the user via
        // the BT chunk_event.
        assert_eq!(
            events.len(),
            2,
            "DeficitCoverage must emit exactly 2 chunk_events (the ERC20 paired with the BT + the deficit burn)"
        );

        let transfer_chunk = events
            .iter()
            .find(|e| matches!(e, AaveChunkEvent::ScaledTokenTransfer { .. }))
            .expect("a ScaledTokenTransfer chunk_event must be present");
        match transfer_chunk {
            AaveChunkEvent::ScaledTokenTransfer {
                scaled_amount,
                transfer_index,
                ..
            } => {
                assert_eq!(
                    *scaled_amount, bt_scaled,
                    "the ERC20 Transfer must be paired with the BT — scaled_amount must be the BT's scaled value (148831959), not the unscaled ERC20 amount (168401963)"
                );
                assert_eq!(
                    *transfer_index, index,
                    "transfer_index must be the BT's liquidity index (NOT the zero `ev.index.unwrap_or_default()` fallback)"
                );
            }
            other => panic!("expected ScaledTokenTransfer, got {other:?}"),
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AaveChunkEvent::ScaledTokenBurn { .. })),
            "the deficit-coverage burn chunk_event must be present"
        );
    }

    // ── COMBINED_BURN (N liquidations share 1 burn) ──────────────────────
    // A Balancer flash-loan liquidation with 2 LiquidationCalls on the same
    // (user, USDC debt) produces exactly 1 vUSDC DebtBurn (value = liq#2's
    // debtToCover) + 1 repay-path DebtMint (balInc > value). Python detects
    // `LiquidationPattern.COMBINED_BURN`, sizes the single burn by the group's
    // `total_debt_to_cover` (sum), and skips the Mint entirely. Sizing the
    // burn by each op's LiquidationCall word0 under-burns, and applying the
    // DebtMint as a standard borrow over-mints: the pair diverges from chain
    // `scaledBalanceOf` by 262_371_189_504.

    /// Build a synthetic `LiquidationCall` log.
    fn make_liquidation_call_log(
        idx: u64,
        collateral_asset: Address,
        debt_asset: Address,
        user: Address,
        debt_to_cover: U256,
    ) -> Log {
        let mut data = vec![0u8; 128];
        debt_to_cover
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[i] = *b;
            });
        make_log(
            idx,
            Address::from([0x99; 20]), // the Pool address (irrelevant here)
            vec![
                degenbot_decoders::aave_event_decoder::AAVE_LIQUIDATION_CALL_TOPIC,
                B256::left_padding_from(collateral_asset.as_slice()),
                B256::left_padding_from(debt_asset.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::from(data),
        )
    }

    /// Build a synthetic vToken `Burn` scaled event (the `COMBINED_BURN`'s
    /// shared debt burn).
    fn make_debt_burn_event(
        log_idx: u64,
        vtoken: Address,
        user: Address,
        amount: U256,
        index: U256,
    ) -> ScaledTokenEvent<'static> {
        let decoded = crate::operations::ScaledTokenEventData::Burn {
            from: user,
            target: Address::ZERO,
            value: amount,
            balance_increase: U256::ZERO,
            index,
        };
        let log = Box::leak(Box::new(make_log(
            log_idx,
            vtoken,
            vec![
                degenbot_decoders::aave_event_decoder::AAVE_BURN_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(Address::ZERO.as_slice()),
            ],
            Bytes::default(),
        )));
        ScaledTokenEvent {
            log,
            decoded,
            event_type: ScaledTokenEventType::DebtBurn,
            token_address: vtoken,
            user_address: user,
            caller_address: None,
            from_address: Some(user),
            target_address: Some(Address::ZERO),
            amount,
            balance_increase: Some(U256::ZERO),
            index: Some(index),
            log_index: log_idx,
        }
    }

    /// Build a synthetic vToken `Mint` scaled event (the repay-path mint:
    /// `balance_increase > value` → on-chain `_burnScaled` truth).
    fn make_debt_mint_repay_event(
        log_idx: u64,
        vtoken: Address,
        user: Address,
        value: U256,
        balance_increase: U256,
        index: U256,
    ) -> ScaledTokenEvent<'static> {
        let decoded = crate::operations::ScaledTokenEventData::Mint {
            caller: user,
            on_behalf_of: user,
            value,
            balance_increase,
            index,
        };
        let log = Box::leak(Box::new(make_log(
            log_idx,
            vtoken,
            vec![
                degenbot_decoders::aave_event_decoder::AAVE_MINT_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        )));
        ScaledTokenEvent {
            log,
            decoded,
            event_type: ScaledTokenEventType::DebtMint,
            token_address: vtoken,
            user_address: user,
            caller_address: Some(user),
            from_address: None,
            target_address: None,
            amount: value,
            balance_increase: Some(balance_increase),
            index: Some(index),
            log_index: log_idx,
        }
    }

    /// `build_liquidation_patterns` detects `COMBINED_BURN` for the
    /// 2-liquidations/1-burn group, and `dispatch_liquidation` (a) sizes the
    /// single shared burn by the group's aggregate `total_debt_to_cover`, and
    /// (b) skips the `DebtMint` entirely.
    ///
    /// Sizing the burn by liq#1's `debtToCover` (`47_005_978`, ~42 scaled — vs
    /// the aggregate `292_585_135_322`) and applying the `DebtMint` once per op
    /// as a positive borrow delta diverges by `262_371_189_504`
    /// (`rayDiv(292_538_129_344, idx)`).
    #[test]
    fn dispatch_liquidation_combined_burn_aggregates_debt_and_skips_mint() {
        use crate::{ray_div, RayRounding};
        use degenbot_core::address_utils::address_to_checksum_string;
        use rusqlite::params;

        let db = fresh_db_with_asset();
        let vtoken = Address::from([0x72; 20]);
        let vtoken_str = address_to_checksum_string(&vtoken);
        let underlying = Address::from([0xA0; 20]);
        let underlying_str = address_to_checksum_string(&underlying);
        // Rewrite the seeded erc20 rows (id 1=underlying, id 3=vToken) to real
        // hex addresses so the dispatch resolves the debt position by
        // `lookup_asset_by_token_address_on_conn` + the pattern builder by
        // `lookup_asset_by_underlying_address_on_conn`.
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 1",
                params![underlying_str],
            )
            .unwrap();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 3",
                params![vtoken_str],
            )
            .unwrap();
        }

        let user = Address::from([0x4A; 20]);
        let index = U256::from(1_114_798_937_791u64).saturating_mul(U256::from(1_000_000_000u64));
        let dtc1 = U256::from(47_005_978u64);
        let dtc2 = U256::from(292_538_129_344u64);
        let total = dtc1 + dtc2;

        // Two LiquidationCall logs (collateral assets are irrelevant to the
        // debt leg; debtAsset = the underlying, user = the liquidated user).
        let liq1 = Box::leak(Box::new(make_liquidation_call_log(
            17,
            Address::from([0xC0; 20]),
            underlying,
            user,
            dtc1,
        )));
        let liq2 = Box::leak(Box::new(make_liquidation_call_log(
            30,
            Address::from([0xC1; 20]),
            underlying,
            user,
            dtc2,
        )));

        // The shared DebtBurn (idx 19) + the repay-path DebtMint (idx 6).
        let burn = make_debt_burn_event(19, vtoken, user, dtc2, index);
        let mint = make_debt_mint_repay_event(
            6,
            vtoken,
            user,
            U256::from(352_195_531u64),
            U256::from(399_201_509u64),
            index,
        );

        // Op #0 (liq#1) carries the shared burn (COMBINED_BURN position 0) +
        // the mint; Op #1 (liq#2) carries only the mint — mirrors the parser's
        // `collect_debt_burns` COMBINED_BURN assignment.
        let op0 = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq1),
            scaled_events: vec![burn.clone(), mint.clone()],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(dtc1),
            validation_errors: Vec::new(),
        };
        let op1 = Operation {
            operation_id: 1,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq2),
            scaled_events: vec![mint.clone()],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(dtc2),
            validation_errors: Vec::new(),
        };
        let operations = vec![op0.clone(), op1.clone()];

        // The pattern context detects COMBINED_BURN.
        let conn = db.lock();
        let mut liq_patterns =
            build_liquidation_patterns(&conn, 1, &operations, &mut ChunkSubstrate::lazy(1));
        assert_eq!(
            liq_patterns.get_pattern(user, vtoken),
            Some(LiquidationPattern::CombinedBurn),
            "2 liquidations sharing 1 burn must detect COMBINED_BURN",
        );
        assert_eq!(
            liq_patterns
                .get_group(user, vtoken)
                .unwrap()
                .total_debt_to_cover(),
            total,
            "the group's total_debt_to_cover must be the sum of both debtToCovers",
        );

        // Dispatch sizes the single burn by the aggregate +
        // skips the mint (0 mints across both ops).
        let discounts = HashMap::new();
        let logs: Vec<&Log> = Vec::new();
        let gho_ctx = GhoDiscountContext::new(&logs, &discounts, None, fixture_pool());
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        dispatch_liquidation(
            &op0,
            1,
            &conn,
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut HashMap::new(),
            &mut ChunkSubstrate::lazy(1),
        )
        .expect("op0 dispatch");
        dispatch_liquidation(
            &op1,
            1,
            &conn,
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut HashMap::new(),
            &mut ChunkSubstrate::lazy(1),
        )
        .expect("op1 dispatch");

        let debt_burns: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AaveChunkEvent::ScaledTokenBurn { position, .. } if *position == ScaledTokenPosition::Debt))
            .collect();
        let debt_mints: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AaveChunkEvent::ScaledTokenMint { position, .. } if *position == ScaledTokenPosition::Debt))
            .collect();
        assert_eq!(
            debt_burns.len(),
            1,
            "the shared burn must fire exactly once"
        );
        assert_eq!(
            debt_mints.len(),
            0,
            "the COMBINED_BURN Mint must be skipped"
        );

        // The burn's delta = -ray_div(total, index, HalfUp) — the aggregate
        // (rev 1 → `enricher_debt_burn` = HalfUp).
        let expected_scaled = ray_div(total, index, Some(RayRounding::HalfUp)).unwrap();
        match debt_burns[0] {
            AaveChunkEvent::ScaledTokenBurn {
                balance_delta,
                new_index,
                ..
            } => {
                // balance_delta is negative I256; the magnitude equals the
                // aggregate scaled burn (HalfUp for rev 1).
                let actual_magnitude = if balance_delta.is_negative() {
                    U256::try_from(-*balance_delta).unwrap()
                } else {
                    U256::try_from(*balance_delta).unwrap()
                };
                assert_eq!(
                    actual_magnitude, expected_scaled,
                    "the burn must be sized by total_debt_to_cover ({total}), not liq#1's debtToCover ({dtc1})",
                );
                assert_eq!(*new_index, index);
            }
            other => panic!("expected ScaledTokenBurn, got {other:?}"),
        }
    }

    // A debt burn whose vToken does NOT match any LiquidationCall's debt
    // vToken — e.g. a cross-asset burn attached to a liquidation op, or a
    // standalone (non-liquidation) repay — must NOT be classified as
    // `COMBINED_BURN`. Grouping it under `entry().or_default()` creates a
    // spurious 0-liquidation + 1-burn group; `detect_pattern` then returns
    // `CombinedBurn` with `total_debt_to_cover() == 0`, and
    // `dispatch_liquidation`'s COMBINED_BURN override sizes the burn by 0 —
    // zeroing a debt balance chain expects ~1.28e15 on — a burn whose key
    // has no liquidation group must NOT create one.
    #[test]
    fn build_liquidation_patterns_does_not_classify_standalone_burn_as_combined() {
        use degenbot_core::address_utils::address_to_checksum_string;
        use rusqlite::params;

        let db = fresh_db_with_asset();
        // WBTC underlying + vToken (the LC's debt asset) — group key A.
        let wbtc_underlying = Address::from([0xA0; 20]);
        let wbtc_vtoken = Address::from([0x72; 20]);
        // GHO vToken (the cross-asset burn's emitter) — must NOT form a group.
        let gho_vtoken = Address::from([0x99; 20]);
        let user = Address::from([0x4A; 20]);
        let index = U256::from(1_114_798_937_791u64).saturating_mul(U256::from(1_000_000_000u64));

        // Seed BOTH vToken rows so `build_liquidation_patterns` can resolve
        // the LC's debt vToken (1=underlying, 3=vToken already seeded by
        // fresh_db_with_asset — rewrite them). Add a second asset (GHO) row
        // so the GHO burn's emitter address is registered (parity with a real
        // market; the step-2 lookup uses the in-memory map, but we keep the DB
        // realistic).
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 1",
                params![address_to_checksum_string(&wbtc_underlying)],
            )
            .unwrap();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 3",
                params![address_to_checksum_string(&wbtc_vtoken)],
            )
            .unwrap();
        }

        let dtc = U256::from(47_005_978u64);
        // WBTC-debt LiquidationCall (the real group).
        let liq = Box::leak(Box::new(make_liquidation_call_log(
            17,
            Address::from([0xC0; 20]),
            wbtc_underlying,
            user,
            dtc,
        )));
        // A cross-asset GHO debt burn riding in the same op (the parser's
        // COMBINED_BURN position-0 assignment can attach a same-user burn of a
        // different vToken). Its vToken (0x99..) != the LC's debt vToken
        // (0x72..), so it must NOT create a (user, 0x99..) group.
        let gho_burn = make_debt_burn_event(42, gho_vtoken, user, U256::from(1_000_000u64), index);
        let op = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq),
            scaled_events: vec![gho_burn],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(dtc),
            validation_errors: Vec::new(),
        };
        let conn = db.lock();
        let liq_patterns = build_liquidation_patterns(
            &conn,
            1,
            std::slice::from_ref(&op),
            &mut ChunkSubstrate::lazy(1),
        );
        // The WBTC group exists (1 liquidation, 1 burn would be attached if it
        // matched) — but the GHO burn must NOT have created a group.
        assert_eq!(
            liq_patterns.get_pattern(user, wbtc_vtoken),
            Some(LiquidationPattern::Single),
            "the WBTC-CALL group (1 liq, 0 burns) must detect Single",
        );
        assert_eq!(
            liq_patterns.get_pattern(user, gho_vtoken),
            None,
            "a cross-asset burn with no matching LC must not create a pattern",
        );
        assert!(
            liq_patterns.get_group(user, gho_vtoken).is_none(),
            "a cross-asset burn must not create a (0-liq) group",
        );
    }

    // ── Bad-debt reset for a GHO debt burn in a NON-GHO liquidation ─────
    // A `batchLiquidate` (N LiquidationCalls, all debtAsset=WBTC) can carry a
    // user's GHO-debt vToken Burn attached to a WBTC-LC op: the parser's
    // `collect_debt_burns` single-liquidation branch ("collect ALL debt burns,
    // no asset filter") attaches it because the user has exactly 1
    // liquidation. The GHO burn's real value is 1.28e15 (the FULL GHO debt —
    // a bad-debt write-off: the contract emits DEFICIT_CREATED, on-chain
    // scaledBalanceOf drops to 0).
    //
    // The bad-debt reset applies FIRST (balance=0) to ANY debt burn in a
    // Liquidation/GhoLiquidation op when the tx carries the user's
    // `DEFICIT_CREATED`.
    //
    // Gating the reset on `is_debt` EXCLUDES `GhoDebtBurn` (the set is
    // {DebtBurn, DebtTransfer, Erc20DebtTransfer}), and an `is_gho` guard
    // excludes the GHO vToken (deferring to `dispatch_gho_liquidation` — a
    // path NEVER reached for a GHO burn attached to a NON-GHO LC). The GHO
    // burn then falls through to `build_scaled_event_chunk_event` with
    // `raw_amount` = the WBTC LiquidationCall's word0 `debtToCover` (a dust
    // value, 142 wei) → `ray_div(142, gho_index)` = 126 residual, and the
    // 1.28e15 GHO debt is never burned.
    #[test]
    fn dispatch_liquidation_resets_gho_debt_burn_on_bad_debt() {
        use degenbot_core::address_utils::address_to_checksum_string;
        use degenbot_decoders::aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC;
        use rusqlite::params;

        let db = fresh_db_with_asset();
        // Asset 1 (seeded): rewrite to WBTC underlying (0xA0) / vToken (0x72).
        let wbtc_underlying = Address::from([0xA0; 20]);
        let wbtc_vtoken = Address::from([0x72; 20]);
        // GHO asset: a NEW row (id 2) with underlying 0xA1 + vToken 0x99
        // (the GHO debt vToken whose burn we test). erc20 ids 4 (GHO
        // underlying) + 5 (GHO vToken) — fresh ids that don't collide with the
        // seeded 1/2/3.
        let gho_underlying = Address::from([0xA1; 20]);
        let gho_vtoken = Address::from([0x99; 20]);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 1",
                params![address_to_checksum_string(&wbtc_underlying)],
            )
            .unwrap();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 3",
                params![address_to_checksum_string(&wbtc_vtoken)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (4, 1, ?1), (5, 1, ?2), (6, 1, ?3)",
                params![
                    address_to_checksum_string(&gho_underlying),
                    address_to_checksum_string(&gho_vtoken),
                    address_to_checksum_string(&Address::from([0x77; 20])),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (2, 1, 4, 6, 1, 5, 1, '0', '0', '0', '0')",
                [],
            )
            .unwrap();
        }

        let user = Address::from([0x4A; 20]);
        let index = U256::from(1_122_921_836u64).saturating_mul(U256::from(1_000_000_000u64));
        // The WBTC LiquidationCall (debtAsset=WBTC underlying, user). Its
        // word0 = debtToCover = a dust value (142) — mirrors the real tx's
        // LC which liquidated a near-zero WBTC debt.
        let liq = Box::leak(Box::new(make_liquidation_call_log(
            665,
            Address::from([0xC0; 20]),
            wbtc_underlying,
            user,
            U256::from(142u64),
        )));
        // The GHO debt burn attached to this op: real value 1.28e15 (the
        // FULL GHO debt — a bad-debt write-off).
        let gho_burn_value = U256::from(1_282_800_003_371_564u64);
        let gho_burn = make_debt_burn_event(661, gho_vtoken, user, gho_burn_value, index);
        // Mutate the event_type to GhoDebtBurn (make_debt_burn_event builds
        // DebtBurn; the GHO vToken's burn is a GhoDebtBurn).
        let mut gho_burn = gho_burn;
        gho_burn.event_type = ScaledTokenEventType::GhoDebtBurn;

        let op = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq),
            scaled_events: vec![gho_burn],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(U256::from(142u64)),
            validation_errors: Vec::new(),
        };

        // Build a GhoDiscountContext that flags `user` as bad-debt via a
        // DEFICIT_CREATED log (topic1 = user).
        let deficit_log = make_log(
            666,
            fixture_pool(),
            vec![
                AAVE_DEFICIT_CREATED_TOPIC,
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        );
        let logs: Vec<&Log> = vec![&deficit_log];
        let discounts = HashMap::new();
        let gho_ctx = GhoDiscountContext::new(&logs, &discounts, Some(gho_vtoken), fixture_pool());
        assert!(
            gho_ctx.is_bad_debt(user),
            "fixture: the DEFICIT_CREATED log must flag the user as bad-debt",
        );

        let conn = db.lock();
        let mut liq_patterns = LiquidationPatternContext::default();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        dispatch_liquidation(
            &op,
            1,
            &conn,
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut HashMap::new(),
            &mut ChunkSubstrate::lazy(1),
        )
        .expect("dispatch");

        // A DebtPositionReset must be emitted for the GHO debt position (the
        // bad-debt write-off zeroes the balance). Without the reset the GHO
        // burn emits as a wrongly-sized ScaledTokenBurn instead
        // (delta = ray_div(142, index) = 126, leaving 1.28e15).
        let reset = events.iter().find_map(|e| match e {
            AaveChunkEvent::DebtPositionReset {
                position_id,
                new_index,
            } => Some((*position_id, *new_index)),
            _ => None,
        });
        let (reset_pid, reset_idx) =
            reset.expect("the GHO debt burn must trigger a DebtPositionReset on bad-debt");
        assert_eq!(
            reset_idx, index,
            "the reset must advance last_index to the burn's index"
        );
        // The reset's position_id must be the GHO vToken's debt position.
        let gho_user_id =
            DegenbotDb::get_or_create_user_on_conn(&conn, 1, &address_to_checksum_string(&user), 0)
                .unwrap();
        let gho_debt_pid =
            DegenbotDb::get_or_create_debt_position_on_conn(&conn, gho_user_id, 2).unwrap();
        assert_eq!(
            reset_pid, gho_debt_pid,
            "the reset must target the GHO vToken's debt position"
        );
        // And no wrongly-sized ScaledTokenBurn for the GHO position.
        assert!(
            !events.iter().any(|e| matches!(e, AaveChunkEvent::ScaledTokenBurn { position, .. } if *position == ScaledTokenPosition::Debt)),
            "the GHO bad-debt burn must NOT also emit a ScaledTokenBurn (would double-apply)",
        );
    }

    // ── bad-debt reset ↔ GHO running-state synchronization ───────────────
    // A `DEFICIT_CREATED` bad-debt reset emitted by `dispatch_liquidation`
    // (a NON-GHO `LiquidationCall` carrying an attached GHO debt burn — the
    // parser's `collect_debt_burns` single-liquidation branch) must
    // synchronize the per-tx `gho_running_state` map, exactly like
    // `dispatch_gho_liquidation`'s own reset does. The apply buffer lands
    // only after `process_transaction` returns, so a later-sorted GHO
    // operation for the same position in the same tx reads the map — if the
    // reset didn't write it, the later event computes from the pre-reset
    // (balance, index) tuple: the discount accrual (`accrue_debt_on_action`)
    // burns a share off the pre-reset balance the contract already wrote
    // off.

    /// WBTC liquidation asset + GHO vToken asset (revision 1 — the discount
    /// surface is active) + a user whose GHO debt position exists in the DB,
    /// mirroring `dispatch_liquidation_resets_gho_debt_burn_on_bad_debt`'s
    /// fixture. Returns (db, `gho_vtoken`, user, gho debt position id).
    fn bad_debt_running_state_fixture() -> (DegenbotDb, Address, Address, i64) {
        use degenbot_core::address_utils::address_to_checksum_string;
        use rusqlite::params;

        let db = fresh_db_with_asset();
        let gho_underlying = Address::from([0xA1; 20]);
        let gho_vtoken = Address::from([0x99; 20]);
        let user = Address::from([0x4A; 20]);
        let gho_pid = {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 1",
                params![address_to_checksum_string(&Address::from([0xA0; 20]))],
            )
            .unwrap();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 3",
                params![address_to_checksum_string(&Address::from([0x72; 20]))],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (4, 1, ?1), (5, 1, ?2), (6, 1, ?3)",
                params![
                    address_to_checksum_string(&gho_underlying),
                    address_to_checksum_string(&gho_vtoken),
                    address_to_checksum_string(&Address::from([0x77; 20])),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO aave_v3_assets \
                    (id, market_id, underlying_asset_id, a_token_id, a_token_revision, \
                     v_token_id, v_token_revision, liquidity_index, liquidity_rate, \
                     borrow_index, borrow_rate) \
                 VALUES (2, 1, 4, 6, 1, 5, 1, '0', '0', '0', '0')",
                [],
            )
            .unwrap();
            let user_id = DegenbotDb::get_or_create_user_on_conn(
                &conn,
                1,
                &address_to_checksum_string(&user),
                0,
            )
            .unwrap();
            DegenbotDb::get_or_create_debt_position_on_conn(&conn, user_id, 2).unwrap()
        };
        (db, gho_vtoken, user, gho_pid)
    }

    // One tx: a WBTC `LiquidationCall` whose attached GHO debt burn is a
    // bad-debt write-off (`DEFICIT_CREATED`), then a later-sorted GHO borrow
    // for the same position. The reset must seed the running-state map with
    // (0, reset_index); the borrow's discount accrual is a pure function of
    // the tuple it reads — from the reset tuple the mint delta is exactly
    // `ray_div(value, index)`, from the pre-reset tuple it loses
    // `discount_scaled` (here 100_000 scaled wei).
    #[test]
    fn bad_debt_reset_in_non_gho_liquidation_syncs_running_state_for_later_gho_event() {
        use alloy::primitives::I256;
        use degenbot_decoders::aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC;

        let (db, gho_vtoken, user, gho_pid) = bad_debt_running_state_fixture();

        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let pre_reset_index = ray;
        let reset_index = U256::from(3u8) * ray / U256::from(2u8);
        let later_index = U256::from(2u8) * ray;
        let pre_reset_balance = U256::from(1_000_000u64);
        let discount_percent = U256::from(2_000u64);
        let mint_value = U256::from(1_000u64) * U256::from(10u8).pow(U256::from(18u8));

        let liq = Box::leak(Box::new(make_liquidation_call_log(
            665,
            Address::from([0xC0; 20]),
            Address::from([0xA0; 20]),
            user,
            U256::from(142u64),
        )));
        let mut gho_burn = make_debt_burn_event(
            661,
            gho_vtoken,
            user,
            U256::from(1_282_800_003_371_564u64),
            reset_index,
        );
        gho_burn.event_type = ScaledTokenEventType::GhoDebtBurn;
        let op_liq = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq),
            scaled_events: vec![gho_burn],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(U256::from(142u64)),
            validation_errors: Vec::new(),
        };

        let mut gho_mint =
            make_debt_mint_repay_event(670, gho_vtoken, user, mint_value, U256::ZERO, later_index);
        gho_mint.event_type = ScaledTokenEventType::GhoDebtMint;
        let op_borrow = Operation {
            operation_id: 1,
            operation_type: OperationType::GhoBorrow,
            pool_revision: 9,
            pool_event: None,
            scaled_events: vec![gho_mint],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };

        let deficit_log = make_log(
            666,
            fixture_pool(),
            vec![
                AAVE_DEFICIT_CREATED_TOPIC,
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        );
        let logs: Vec<&Log> = vec![&deficit_log];
        let mut discounts = HashMap::new();
        discounts.insert(user, discount_percent);
        let gho_ctx = GhoDiscountContext::new(&logs, &discounts, Some(gho_vtoken), fixture_pool());
        assert!(
            gho_ctx.is_bad_debt(user),
            "fixture: the DEFICIT_CREATED log must flag the user as bad-debt",
        );

        let conn = db.lock();
        let mut substrate = ChunkSubstrate::lazy(1);
        // The pre-tx snapshot: the DB row holds the tuple as of the reset
        // event (balance before the write-off, index before the burn) because
        // events apply only after `process_transaction` returns — the
        // running-state map is the only state a later same-tx GHO op reads.
        substrate.record_position_write(
            ScaledTokenPosition::Debt,
            gho_pid,
            pre_reset_balance,
            Some(pre_reset_index),
        );

        let mut liq_patterns = LiquidationPatternContext::default();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        let mut gho_running_state: HashMap<i64, (U256, U256)> = HashMap::new();
        dispatch_operation(
            &op_liq,
            1,
            &conn,
            Some(gho_vtoken),
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut gho_running_state,
            &mut substrate,
        )
        .expect("liquidation dispatch");
        dispatch_operation(
            &op_borrow,
            1,
            &conn,
            Some(gho_vtoken),
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut gho_running_state,
            &mut substrate,
        )
        .expect("GHO borrow dispatch");

        let (reset_pid, reset_idx) = events
            .iter()
            .find_map(|e| match e {
                AaveChunkEvent::DebtPositionReset {
                    position_id,
                    new_index,
                } => Some((*position_id, *new_index)),
                _ => None,
            })
            .expect("bad-debt GHO burn must emit a DebtPositionReset");
        assert_eq!(
            reset_pid, gho_pid,
            "the reset targets the GHO debt position"
        );
        assert_eq!(reset_idx, reset_index, "the reset advances last_index");

        let mint_delta = events
            .iter()
            .find_map(|e| match e {
                AaveChunkEvent::ScaledTokenMint {
                    position: ScaledTokenPosition::Debt,
                    position_id,
                    balance_delta,
                    ..
                } if *position_id == gho_pid => Some(*balance_delta),
                _ => None,
            })
            .expect("the later GHO borrow emits a debt ScaledTokenMint");
        let expected_from_zero = I256::try_from(500_000_000_000_000_000_000u128).unwrap();
        assert_eq!(
            mint_delta, expected_from_zero,
            "the later GHO borrow must compute from the reset zero balance + reset index — a delta short of ray_div(value, index) means it read the pre-reset tuple",
        );
        assert_eq!(
            gho_running_state.get(&gho_pid),
            Some(&(U256::from(500_000_000_000_000_000_000u128), later_index)),
            "the running state must reflect the zero-based computation",
        );
    }

    // The burn variant: the later-sorted GHO event is a repayment burn. From
    // the reset tuple the V1 burn delta is exactly -amount_scaled (the
    // discount accrual is zero against a zero balance); off the pre-reset
    // tuple it burns an extra `discount_scaled` share.
    #[test]
    fn second_gho_event_after_bad_debt_reset_reads_zero_balance() {
        use alloy::primitives::I256;
        use degenbot_decoders::aave_event_decoder::AAVE_DEFICIT_CREATED_TOPIC;

        let (db, gho_vtoken, user, gho_pid) = bad_debt_running_state_fixture();

        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let pre_reset_index = ray;
        let reset_index = U256::from(3u8) * ray / U256::from(2u8);
        let later_index = U256::from(2u8) * ray;
        let pre_reset_balance = U256::from(1_000_000u64);
        let discount_percent = U256::from(2_000u64);
        let burn_value = U256::from(200u64) * U256::from(10u8).pow(U256::from(18u8));

        let liq = Box::leak(Box::new(make_liquidation_call_log(
            665,
            Address::from([0xC0; 20]),
            Address::from([0xA0; 20]),
            user,
            U256::from(142u64),
        )));
        let mut gho_burn = make_debt_burn_event(
            661,
            gho_vtoken,
            user,
            U256::from(1_282_800_003_371_564u64),
            reset_index,
        );
        gho_burn.event_type = ScaledTokenEventType::GhoDebtBurn;
        let op_liq = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 9,
            pool_event: Some(liq),
            scaled_events: vec![gho_burn],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(U256::from(142u64)),
            validation_errors: Vec::new(),
        };

        let mut later_burn = make_debt_burn_event(670, gho_vtoken, user, burn_value, later_index);
        later_burn.event_type = ScaledTokenEventType::GhoDebtBurn;
        let op_repay = Operation {
            operation_id: 1,
            operation_type: OperationType::GhoRepay,
            pool_revision: 9,
            pool_event: None,
            scaled_events: vec![later_burn],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };

        let deficit_log = make_log(
            666,
            fixture_pool(),
            vec![
                AAVE_DEFICIT_CREATED_TOPIC,
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        );
        let logs: Vec<&Log> = vec![&deficit_log];
        let mut discounts = HashMap::new();
        discounts.insert(user, discount_percent);
        let gho_ctx = GhoDiscountContext::new(&logs, &discounts, Some(gho_vtoken), fixture_pool());
        assert!(
            gho_ctx.is_bad_debt(user),
            "fixture: the DEFICIT_CREATED log must flag the user as bad-debt",
        );

        let conn = db.lock();
        let mut substrate = ChunkSubstrate::lazy(1);
        substrate.record_position_write(
            ScaledTokenPosition::Debt,
            gho_pid,
            pre_reset_balance,
            Some(pre_reset_index),
        );

        let mut liq_patterns = LiquidationPatternContext::default();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        let mut gho_running_state: HashMap<i64, (U256, U256)> = HashMap::new();
        dispatch_operation(
            &op_liq,
            1,
            &conn,
            Some(gho_vtoken),
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut gho_running_state,
            &mut substrate,
        )
        .expect("liquidation dispatch");
        dispatch_operation(
            &op_repay,
            1,
            &conn,
            Some(gho_vtoken),
            &gho_ctx,
            &mut liq_patterns,
            &mut events,
            &mut gho_running_state,
            &mut substrate,
        )
        .expect("GHO repay dispatch");

        let burn_delta = events
            .iter()
            .find_map(|e| match e {
                AaveChunkEvent::ScaledTokenBurn {
                    position: ScaledTokenPosition::Debt,
                    position_id,
                    balance_delta,
                    ..
                } if *position_id == gho_pid => Some(*balance_delta),
                _ => None,
            })
            .expect("the later GHO repay emits a debt ScaledTokenBurn");
        let expected_from_zero = -I256::try_from(100_000_000_000_000_000_000u128).unwrap();
        assert_eq!(
            burn_delta, expected_from_zero,
            "the second GHO event must read the reset zero balance — an extra discount share in the burn means it read the pre-reset tuple",
        );
    }

    // ── DebtMint in a Liquidation (interest exceeds debtToCover) ─────────
    // A `LiquidationCall` whose `_burnScaled` emits BOTH a `Mint` (value =
    // balanceIncrease - debtToCover, when interest > the liquidated debt) AND
    // a `Burn` (value = debtToCover). The Mint is a NET BURN (the on-chain
    // `scaledBalanceOf` DROPS at the Mint's block) — Aave V3 `_burnScaled`
    // mints the interest-remainder token then the actual burn subtracts the
    // principal scaled amount.
    //
    // A `DebtMint` in an op of type `Liquidation | GhoLiquidation | Repay |
    // RepayWithAtokens | GhoRepay` is
    // a burn sized by the Pool event's `debtToCover`/`repayAmount` (decoded
    // from the Pool event word0), FLOOR-rounded for rev ≥4. The Rust
    // `is_debt_repay_mint_edge` gate must include
    // `Liquidation`: a `DebtMint` outside it falls to the standard-mint
    // REPAY-arm, which derives the amount from `balance_increase - value`
    // (= `bi - (bi - debtToCover)` = debtToCover on paper — but the on-chain
    // Mint `value` field is rounded relative to debtToCover by ±1 wei).
    // Floor-dividing `bi - value` (= 496_463_174) instead of `debtToCover`
    // (= 496_463_175) burns 1 wei too small, leaving the DB 1 wei too high.
    #[test]
    fn dispatch_liquidation_treats_debt_mint_as_burn_via_debt_to_cover() {
        use rusqlite::params;
        let db = fresh_db_with_asset();
        // Asset 1: vToken rev 5 (V5 ExplicitRoundingMath — mint=CEIL, burn=FLOOR).
        let underlying = Address::from([0xA0; 20]);
        let vtoken = Address::from([0x72; 20]);
        let user = Address::from([0x4B; 20]);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 1",
                params![degenbot_core::address_utils::address_to_checksum_string(
                    &underlying
                )],
            )
            .unwrap();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 3",
                params![degenbot_core::address_utils::address_to_checksum_string(
                    &vtoken
                )],
            )
            .unwrap();
            conn.execute(
                "UPDATE aave_v3_assets SET v_token_revision = 5 WHERE id = 1",
                [],
            )
            .unwrap();
        }
        // Fixture values (USDC debt vToken rev 5): index + debtToCover chosen
        // so that floor(debtToCover / idx) = 408_261_084 (the on-chain Mint
        // effect) while floor((balance_increase - value) / idx) = 408_261_083
        // (the standard-mint REPAY-arm's under-burn by 1).
        let index = U256::from_str_radix("3ede331d09b14470cc739a4", 16).unwrap();
        let debt_to_cover = U256::from(496_463_175u64);
        let balance_increase = U256::from(666_026_406u64);
        let mint_value = balance_increase - debt_to_cover + U256::from(1u64); // rounded +1 vs bi-debtToCover
                                                                              // Mint (value < balance_increase → the interest-exceeds case).
        let mint =
            make_debt_mint_repay_event(661, vtoken, user, mint_value, balance_increase, index);
        let collat = Address::from([0xC0; 20]);
        let liq = Box::leak(Box::new(make_liquidation_call_log(
            665,
            collat,
            underlying,
            user,
            debt_to_cover,
        )));
        let op = Operation {
            operation_id: 0,
            operation_type: OperationType::Liquidation,
            pool_revision: 10,
            pool_event: Some(liq),
            scaled_events: vec![mint],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: Some(debt_to_cover),
            validation_errors: Vec::new(),
        };
        let conn = db.lock();
        let mut liq_patterns = LiquidationPatternContext::default();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        dispatch_liquidation(
            &op,
            1,
            &conn,
            &GhoDiscountContext::new(&[], &HashMap::new(), None, fixture_pool()),
            &mut liq_patterns,
            &mut events,
            &mut HashMap::new(),
            &mut ChunkSubstrate::lazy(1),
        )
        .expect("dispatch");
        // The Mint must be dispatched as a BURN sized by debtToCover (= raw_amount
        // = the LiquidationCall word0 = 496_463_175), floor-divided → 408_261_084.
        // NOT the standard-mint path which would floor-divide (balance_increase -
        // value) = 496_463_174 → 408_261_083 (1 too small → DB +1 too high).
        let debt_burn = events.iter().find_map(|e| match e {
            AaveChunkEvent::ScaledTokenBurn {
                position,
                balance_delta,
                ..
            } if *position == ScaledTokenPosition::Debt => Some(*balance_delta),
            _ => None,
        });
        let burn_delta = debt_burn.expect("the liquidation DebtMint (value < balance_increase) must emit a ScaledTokenBurn sized by debtToCover");
        // The burn delta must be -floor(debtToCover/idx) = -408_261_084,
        // NOT the standard-mint -floor((bi-value)/idx) = -408_261_083.
        assert!(
            burn_delta.is_negative(),
            "the Mint-as-burn delta must be negative"
        );
        let burn_magnitude = U256::try_from(-burn_delta).unwrap();
        assert_eq!(
            burn_magnitude,
            U256::from(408_261_084u64),
            "the burn magnitude must be floor(debtToCover/idx) = 408_261_084, NOT the standard-mint floor((bi-value)/idx) = 408_261_083",
        );
        // And it must NOT be a ScaledTokenMint (the standard borrow-mint path).
        assert!(
            !events.iter().any(|e| matches!(e, AaveChunkEvent::ScaledTokenMint { position, .. } if *position == ScaledTokenPosition::Debt)),
            "the liquidation DebtMint must NOT be dispatched as a borrow-mint (ScaledTokenMint)",
        );
    }

    // ── The aToken interest-exceeds edge (Mint dispatched as a Burn) ───────

    /// A `Withdraw` whose aToken `Mint` carries `amount < balance_increase` is
    /// a net burn, sized by the enriched Pool word (the `withdrawAmount`)
    /// through the collateral-burn `TokenMath` row — never by a
    /// `value + balance_increase` recompute, which the fixture makes differ by
    /// several orders of magnitude. The delta is the same whether the enriched
    /// amount arrives through the event datum (as this dispatch hands it over)
    /// or through the `scaled_delta` override.
    #[test]
    fn dispatch_withdraw_interest_exceeds_edge_sizes_the_burn_from_the_pool_word() {
        use alloy::primitives::I256;
        use degenbot_core::address_utils::address_to_checksum_string;
        use degenbot_decoders::aave_event_decoder::{AAVE_MINT_TOPIC, AAVE_WITHDRAW_TOPIC};
        use rusqlite::params;

        let db = fresh_db_with_asset();
        let a_token = Address::from([0x98; 20]);
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE erc20_tokens SET address = ?1 WHERE id = 2",
                params![address_to_checksum_string(&a_token)],
            )
            .unwrap();
            // Rev 4: the collateral-burn TokenMath row divides CEIL.
            conn.execute(
                "UPDATE aave_v3_assets SET a_token_revision = 4 WHERE id = 1",
                [],
            )
            .unwrap();
        }

        let user = Address::from([0x53; 20]);
        let index = U256::from(1_113_456_789_000_000_000_000_000_000u128);
        let withdraw_amount = U256::from(1_000_000u64);
        let value = U256::from(100u64);
        let balance_increase = U256::from(5_000_000u64);

        // The Pool's `Withdraw` log: data word 0 = the withdrawn amount (the
        // enricher's `raw_amount` for a Withdraw op).
        let pool_event = Box::leak(Box::new(make_log(
            10,
            Address::from([0x99; 20]),
            vec![
                AAVE_WITHDRAW_TOPIC,
                B256::left_padding_from(Address::ZERO.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::from({
                let mut d = vec![0u8; 64];
                withdraw_amount
                    .to_be_bytes::<32>()
                    .iter()
                    .enumerate()
                    .for_each(|(i, b)| d[i] = *b);
                d
            }),
        )));

        // The aToken `Mint` whose interest exceeds the withdrawn amount.
        let mint_log = Box::leak(Box::new(make_log(
            11,
            a_token,
            vec![
                AAVE_MINT_TOPIC,
                B256::left_padding_from(user.as_slice()),
                B256::left_padding_from(user.as_slice()),
            ],
            Bytes::default(),
        )));
        let mint_ev = ScaledTokenEvent {
            log: mint_log,
            decoded: crate::operations::ScaledTokenEventData::Mint {
                caller: user,
                on_behalf_of: user,
                value,
                balance_increase,
                index,
            },
            event_type: ScaledTokenEventType::CollateralMint,
            token_address: a_token,
            user_address: user,
            caller_address: Some(user),
            from_address: None,
            target_address: None,
            amount: value,
            balance_increase: Some(balance_increase),
            index: Some(index),
            log_index: 11,
        };

        let op = Operation {
            operation_id: 0,
            operation_type: OperationType::Withdraw,
            pool_revision: 9,
            pool_event: Some(pool_event),
            scaled_events: vec![mint_ev],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };

        let enriched = crate::ray_div_ceil(withdraw_amount, index).unwrap();
        let recompute = crate::ray_div(
            value + balance_increase,
            index,
            Some(crate::RayRounding::Ceil),
        )
        .unwrap();
        assert_ne!(
            enriched, recompute,
            "fixture: the Pool word and the value + balance_increase recompute disagree"
        );
        let expected = -I256::try_from(enriched).unwrap();

        let conn = db.lock();
        let mut events: Vec<AaveChunkEvent> = Vec::new();
        dispatch_standard(&op, 1, &conn, &mut events, &mut ChunkSubstrate::lazy(1))
            .expect("dispatch");
        assert_eq!(events.len(), 1, "the single Mint must emit one chunk event");
        match &events[0] {
            AaveChunkEvent::ScaledTokenBurn {
                balance_delta,
                new_index,
                ..
            } => {
                assert_eq!(
                    *balance_delta, expected,
                    "the burn is sized by the enriched Pool word, not the recompute"
                );
                assert_eq!(*new_index, index);
            }
            other => panic!("expected ScaledTokenBurn, got {other:?}"),
        }

        // The override route is the same precedence.
        let event_data = ScaledTokenEventData {
            value,
            balance_increase,
            index,
            scaled_amount: Some(enriched),
        };
        assert_eq!(
            ScaledTokenProcessor::collateral()
                .process_collateral_burn(&event_data, Some(enriched))
                .unwrap()
                .balance_delta,
            expected,
            "the scaled_delta route and the event datum compute the identical delta"
        );
    }
    use degenbot_decoders::aave_event_decoder::{
        AAVE_DEFICIT_CREATED_TOPIC, AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
    };

    /// A reserve's Pool contract for the discount-context fixtures.
    fn pool() -> Address {
        Address::repeat_byte(0x99)
    }

    /// The market's GHO variable-debt token for the discount-context fixtures.
    fn gho_vtoken() -> Address {
        Address::repeat_byte(0x6A)
    }

    /// A contract that is not this market's: another deployment's Pool or GHO
    /// token emits the same signatures.
    fn foreign() -> Address {
        Address::repeat_byte(0x77)
    }

    fn user() -> Address {
        Address::repeat_byte(0xA0)
    }

    fn topic_of(address: Address) -> B256 {
        B256::left_padding_from(address.as_slice())
    }

    fn event_log(
        log_index: u64,
        emitter: Address,
        signature: B256,
        indexed: &[Address],
        words: &[U256],
    ) -> Log {
        let mut topics = vec![signature];
        topics.extend(indexed.iter().map(|address| topic_of(*address)));
        let mut data = Vec::with_capacity(32 * words.len());
        for word in words {
            data.extend_from_slice(&word.to_be_bytes::<32>());
        }
        Log {
            inner: AlloyLog::new_unchecked(emitter, topics, Bytes::from(data)),
            block_hash: None,
            block_number: Some(100),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        }
    }

    fn discount_update(log_index: u64, emitter: Address, old_percent: u64) -> Log {
        event_log(
            log_index,
            emitter,
            AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
            &[user()],
            &[U256::from(old_percent), U256::from(0)],
        )
    }

    fn deficit_created(log_index: u64, emitter: Address) -> Log {
        event_log(
            log_index,
            emitter,
            AAVE_DEFICIT_CREATED_TOPIC,
            &[user(), Address::repeat_byte(0x01)],
            &[U256::from(1)],
        )
    }

    fn effective_discount(logs: &[Log], snapshot: u64, at_log_index: u64) -> U256 {
        let discounts: HashMap<Address, U256> =
            [(user(), U256::from(snapshot))].into_iter().collect();
        let refs: Vec<&Log> = logs.iter().collect();
        let context = GhoDiscountContext::new(&refs, &discounts, Some(gho_vtoken()), pool());
        context.effective_discount(user(), at_log_index, 1)
    }

    fn is_bad_debt(logs: &[Log]) -> bool {
        let discounts: HashMap<Address, U256> = HashMap::new();
        let refs: Vec<&Log> = logs.iter().collect();
        let context = GhoDiscountContext::new(&refs, &discounts, Some(gho_vtoken()), pool());
        context.is_bad_debt(user())
    }

    /// Control: the GHO vToken's own `DiscountPercentUpdated` is the market's
    /// discount report, so its old percent is the discount in effect after it.
    #[test]
    fn the_gho_vtoken_discount_update_is_read() {
        let logs = [discount_update(10, gho_vtoken(), 200)];

        assert_eq!(
            effective_discount(&logs, 50, 20),
            U256::from(200),
            "the GHO vToken's report becomes the effective discount"
        );
    }

    /// A receipt carries the whole transaction, so another deployment's GHO token
    /// can emit the same signature. Its report is not this market's discount.
    #[test]
    fn a_foreign_discount_update_is_ignored() {
        let logs = [
            discount_update(10, gho_vtoken(), 200),
            discount_update(15, foreign(), 999),
        ];

        assert_eq!(
            effective_discount(&logs, 50, 20),
            U256::from(200),
            "only the market's own GHO vToken reports its discount"
        );
    }

    /// A foreign report is not a fallback either: with no market report, the
    /// pre-transaction snapshot stands.
    #[test]
    fn a_foreign_discount_update_never_becomes_the_default() {
        let logs = [discount_update(10, foreign(), 999)];

        assert_eq!(
            effective_discount(&logs, 50, 20),
            U256::from(50),
            "the snapshot is the only default"
        );
    }

    /// Control: the Pool's own `DeficitCreated` is the bad-debt signal.
    #[test]
    fn the_pool_deficit_created_marks_bad_debt() {
        let logs = [deficit_created(10, pool())];

        assert!(
            is_bad_debt(&logs),
            "the Pool's report marks the user bad debt"
        );
    }

    /// A foreign contract's `DeficitCreated` names the same user but is another
    /// deployment's write-off, so it never marks this market's user bad debt.
    #[test]
    fn a_foreign_deficit_created_does_not_mark_bad_debt() {
        let logs = [deficit_created(10, foreign())];

        assert!(
            !is_bad_debt(&logs),
            "only this market's Pool reports its own deficit"
        );
    }
}
