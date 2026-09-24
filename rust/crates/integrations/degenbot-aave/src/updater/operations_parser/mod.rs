//! Aave V3 transaction operations parser — the per-Ethereum-tx stateful
//! grouping engine over the chunk's decoded receipt logs.
//!
//! Given a `&[&Log]` slice (the tx's RPC-fetched receipt logs — decoded
//! in-place via [`degenbot_decoders::aave_event_decoder::decode_aave_log`])
//! plus a borrowed chunk-tx `&Connection` (the §3.4 atomicity invariant —
//! every get/lookup runs on the caller's single chunk Transaction), the parser
//! matches Pool events (Supply/Borrow/Repay/Withdraw/MintToTreasury/Deficit)
//! to their constituent `ScaledToken` events (aToken/vToken mint, burn, and
//! transfer, plus plain ERC20 transfer) by user + amount +/- a pool-revision
//! ray-flooring tolerance ([`TransactionOperationsParser::amounts_match`]) and
//! emits typed [`Operation`]s.
//!
//! # Module layout
//!
//! - this module: the parser struct + entry-point types, the matching helpers,
//!   the `parse()` scaffold, the per-pool-event dispatch, and the `ParseError`
//!   conversions.
//! - `scaled_tokens`: the four `_decode_*_event` wrappers
//!   (Mint/Burn/BalanceTransfer/Transfer → [`ScaledTokenEvent`] with the
//!   emitter-address classification) + the token-address classifiers.
//! - `standard`: the Supply/Withdraw/Borrow/Repay/RepayWithAtokens builders
//!   + their `_find_*` / token-resolution helpers.
//! - `post_loop`: the post-loop builders (MintToTreasury/Deficit/
//!   DeficitCoverage/InterestAccrual/Transfer).
//! - `pool_events`: the anchor extraction + the pool-event dispatcher.
//! - `liquidation`: the `LiquidationCall` engine (pattern detection + the
//!   `SINGLE`/`COMBINED_BURN`/`SEPARATE_BURNS` debt-burn collector + the collateral
//!   collector).
//! - `validation`: the per-`Operation` validators.
//! - `util`: shared free helpers (log-index/address codecs, event cloning,
//!   the tolerance comparison, the burn/mint pairing predicates).
//!
//! # The plumbing-equivalence caveat (escalation trigger)
//!
//! The reference pipeline is 4-stage: `parser.parse(events)` →
//! `ScaledEventEnricher.enrich(scaled_event, operation)` →
//! `EnrichedScaledTokenEvent` → `token_processor._process_*_with_match`.
//! `ScaledTokenProcessor` already subsumes the enrichment layer (they route
//! through the same `TokenMath` rounding). This parser does NOT call
//! `ScaledTokenProcessor` — the matching uses raw
//! `value - balance_increase` / `value + balance_increase`; the
//! `ScaledTokenProcessor` is the apply-path concern. If the standard-builder
//! tests reveal a divergence between the raw-arithmetic matching here and
//! `ScaledTokenProcessor::process_*` on an edge branch, escalate (don't paper
//! over).

use crate::operations::{
    Operation, OperationType, ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType,
    SCALED_AMOUNT_POOL_REVISION, TOKEN_AMOUNT_MATCH_TOLERANCE,
};
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_db::DegenbotDb;
use degenbot_decoders::aave_event_decoder::{
    self, AaveV3Erc20TransferEvent, AaveV3ScaledTokenBalanceTransferEvent,
    AaveV3ScaledTokenBurnEvent, AaveV3ScaledTokenMintEvent, DecodedAaveEvent,
};

use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};

mod liquidation;
mod pool_events;
mod post_loop;
mod scaled_tokens;
mod standard;
mod util;
mod validation;

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests;

pub use util::amounts_match_with_tolerance;
pub(crate) use util::{addr_to_hex, log_idx_value, parse_address};
use util::{is_erc20_transfer_log, is_minted_to_treasury_log};

use pool_events::extract_pool_events;

// ── the parser struct ─────────────────────────────────────────────────────

/// The Aave V3 per-tx operations parser. Holds the per-tx
/// context (`market_id`, `pool_address`, `treasury_address`, the pre-resolved
/// GHO token/vToken addresses) + a borrowed chunk-tx `&Connection`. The
/// orchestrator constructs one fresh per Ethereum tx.
///
/// # Lifetime
///
/// `'a` ties the `&Connection` borrow the parser resolves address→id through
/// (the caller's chunk Transaction). The parser's output `Operation`s borrow
/// the input `&'b [&&'b Log]` slice independently — the parser struct itself
/// doesn't borrow the logs.
#[derive(Debug)]
#[expect(clippy::module_name_repetitions)]
pub struct TransactionOperationsParser<'a> {
    /// `aave_v3_markets.id`.
    pub market_id: i64,
    /// `aave_v3_markets.chain_id` (for the `aave_gho_tokens` JOIN).
    pub chain_id: i64,
    /// The Pool contract address (for the `MintToTreasury`
    /// `caller_address == pool_address` test).
    pub pool_address: Address,
    /// The treasury address (for `MintToTreasury` test DP3-style fallbacks;
    /// None if the market doesn't expose one).
    pub treasury_address: Option<Address>,
    /// The GHO token (underlying) address — None for non-GHO markets.
    pub gho_token_address: Option<Address>,
    /// The GHO vToken address — None for non-GHO markets. The parser's
    /// `_decode_transfer_event` uses this to classify a Transfer on the GHO
    /// vToken contract as a `GhoDebtTransfer`.
    pub gho_vtoken_address: Option<Address>,
    /// The borrowed `&Connection` — every substrate lookup runs on this
    /// (the §3.4 invariant).
    pub conn: &'a rusqlite::Connection,
    /// The Pool contract revision resolved at parse-start (DP4). Read once
    /// via [`DegenbotDb::lookup_pool_revision_on_conn`]; mid-tx `PoolUpdated`
    /// config events are the orchestrator's concern.
    pub pool_revision: u32,
}

/// Errors raised by the parser — the §3.4 atomicity invariant surfaces these
/// to the caller's chunk-tx loop, which rolls back the whole chunk on any
/// `Err`. The failure modes are assertion/value errors + the
/// `[cold]` look-up `MissingRow` errors.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A `pool_event` had no matching `scaled_token` event (the Python's
    /// `assert x is not None` — surfaces as `Err` not panic so the chunk
    /// rolls back cleanly).
    #[error("matching scaled-token event not found: {0}")]
    NoMatch(String),
    /// A substrate lookup failed (a SELECT error, a decode failure — e.g. the
    /// market has no aToken for the reserve the Supply event named).
    #[error("substrate lookup failed: {0}")]
    Substrate(String),
    /// An RPC-style `ray_div` math error from the `MintToTreasury` v8 branch
    /// (DP3).
    #[error("ray-div math error: {0}")]
    RayMath(#[from] crate::WadRayError),
}

// ── the entry-point types ─────────────────────────────────────────────────

/// The parser's output.
/// Borrows the source logs for the lifetime of the input slice.
#[derive(Debug)]
pub struct TransactionOperations<'a> {
    /// The tx hash (raw bytes). Set by the caller.
    pub tx_hash: [u8; 32],
    /// The block number.
    pub block_number: u64,
    /// The parsed operations (in the order the parser produced them —
    /// pool-event-dispatched first, then the phase-4 post-loop appends).
    pub operations: Vec<Operation<'a>>,
    /// Logs that weren't matched to any Operation.
    pub unassigned_events: Vec<&'a Log>,
}

// ── the parser impl ───────────────────────────────────────────────────────

impl<'a> TransactionOperationsParser<'a> {
    /// Construct a parser with the per-tx context pre-resolved by the caller
    /// (the orchestrator does the GHO-token / treasury resolution
    /// before instantiation; the parser doesn't RPC).
    /// # Errors
    /// Returns [`ParseError::Substrate`] if the pool-revision lookup fails.
    #[expect(clippy::similar_names)] // gho_token vs gho_vtoken is intrinsic to the domain
    pub fn new(
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        treasury_address: Option<Address>,
        gho_token_address: Option<Address>,
        gho_vtoken_address: Option<Address>,
        conn: &'a rusqlite::Connection,
    ) -> Result<Self, ParseError> {
        let pool_revision = DegenbotDb::lookup_pool_revision_on_conn(conn, market_id, "POOL")?
            .ok_or_else(|| {
                ParseError::Substrate(format!(
                    "POOL contract revision missing for market_id={market_id}"
                ))
            })?;
        Ok(Self {
            market_id,
            chain_id,
            pool_address,
            treasury_address,
            gho_token_address,
            gho_vtoken_address,
            conn,
            pool_revision,
        })
    }

    // ── the matching fns (no `&self` — pure helpers, exposed for tests) ──

    /// The §4.2-critical tolerance gate. Pool revision ≥
    /// [`SCALED_AMOUNT_POOL_REVISION`] → allow ±[`TOKEN_AMOUNT_MATCH_TOLERANCE`]
    /// wei; otherwise exact match.
    #[must_use]
    pub fn amounts_match(calculated: U256, expected: U256, pool_revision: u32) -> bool {
        if pool_revision >= SCALED_AMOUNT_POOL_REVISION {
            amounts_match_with_tolerance(calculated, expected, TOKEN_AMOUNT_MATCH_TOLERANCE)
        } else {
            calculated == expected
        }
    }

    /// Whether two transfer types are compatible (ERC20 Transfer ↔ `BalanceTransfer`
    /// pairing — the cross-token discrimination for the `_find_matching_balance_transfer`
    /// helper).
    #[must_use]
    pub fn are_compatible_transfer_types(
        ev1: ScaledTokenEventType,
        ev2: ScaledTokenEventType,
    ) -> bool {
        use ScaledTokenEventType::{
            CollateralTransfer, DebtTransfer, Erc20CollateralTransfer, Erc20DebtTransfer,
        };
        let collateral_pair = (CollateralTransfer, Erc20CollateralTransfer);
        let debt_pair = (DebtTransfer, Erc20DebtTransfer);
        let pair = (ev1, ev2);
        let pair_rev = (ev2, ev1);
        pair == collateral_pair
            || pair == debt_pair
            || pair_rev == collateral_pair
            || pair_rev == debt_pair
    }

    // ── the `parse()` scaffold ──

    /// Parse the tx's logs into [`TransactionOperations`]. The entry point
    /// the orchestrator calls per-tx inside the chunk-tx loop.
    ///
    /// # Errors
    /// Returns [`ParseError`] on any builder-matching failure (the caller
    /// rolls back the chunk-tx). Validator-level assertion failures (the
    /// `_validate_*` fns) are routed to `Operation.validation_errors`
    /// (non-fatal — the caller can decide; the Python's
    /// `TransactionOperations.validate` is the strict top-level pass).
    #[expect(
        clippy::missing_panics_doc,
        clippy::too_many_lines,
        clippy::panic_in_result_fn
    )] // parse() is intrinsic — §4.2-drift mirror
    pub fn parse(
        &self,
        events: &'a [&'a Log],
        tx_hash: [u8; 32],
    ) -> Result<TransactionOperations<'a>, ParseError> {
        assert!(
            !events.is_empty(),
            "parser requires at least one event (callers pre-filter empty logs)"
        );
        let block_number = events.first().and_then(|l| l.block_number).unwrap_or(0);

        // Step 1: identify pool events (anchors). .
        let pool_events = extract_pool_events(events);
        // Step 2: decode scaled-token events (mint/burn/balance_transfer/transfer).
        let mut scaled_events: Vec<ScaledTokenEvent<'a>> = Vec::new();
        for ev in events {
            if let Some(decoded) = aave_event_decoder::decode_aave_log(ev) {
                match decoded {
                    DecodedAaveEvent::ScaledTokenMint(m) => {
                        scaled_events.push(self.decode_mint_event(ev, &m));
                    }
                    DecodedAaveEvent::ScaledTokenBurn(b) => {
                        scaled_events.push(self.decode_burn_event(ev, &b));
                    }
                    DecodedAaveEvent::ScaledTokenBalanceTransfer(bt) => {
                        scaled_events.push(self.decode_balance_transfer_event(ev, &bt));
                    }
                    DecodedAaveEvent::Erc20Transfer(t) => {
                        if let Some(s) = self.decode_transfer_event(ev, &t) {
                            scaled_events.push(s);
                        }
                    }
                    _ => {} // ExpectedAbsent: pool events are handled separately in step 1.
                }
            }
        }
        scaled_events.sort_by_key(|e| e.log_index);

        // Step 3: group into operations.
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut assigned_log_indices: HashSet<u64> = HashSet::new();
        let mut next_op_id: u32 = 0;

        for pool_event in pool_events {
            let op = self.create_operation_from_pool_event(
                next_op_id,
                pool_event,
                &scaled_events,
                events,
                &mut assigned_log_indices,
            )?;
            assigned_log_indices.extend(op.event_log_indices());
            operations.push(op);
            next_op_id += 1;
        }

        // Step 4b: MintToTreasury (mirrors `_create_mint_to_treasury_operations`).
        let minted_to_treasury_events: Vec<&Log> = events
            .iter()
            .copied()
            .filter(|l| is_minted_to_treasury_log(l))
            .collect();
        let mint_to_treasury_ops = self.create_mint_to_treasury_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
            &minted_to_treasury_events,
        );
        assigned_log_indices.extend(
            mint_to_treasury_ops
                .iter()
                .flat_map(|op| op.scaled_events.iter().map(|ev| ev.log_index)),
        );
        operations.extend(mint_to_treasury_ops);

        // Step 4c: DeficitCoverage (BalanceTransfer + Burn pairs).
        let deficit_coverage_ops = self.create_deficit_coverage_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
        );
        assigned_log_indices.extend(
            deficit_coverage_ops
                .iter()
                .flat_map(|op| op.scaled_events.iter().map(|ev| ev.log_index)),
        );
        operations.extend(deficit_coverage_ops);

        // Step 4d: InterestAccrual.
        let interest_accrual_ops = self.create_interest_accrual_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
        );
        assigned_log_indices.extend(interest_accrual_ops.iter().flat_map(|op| {
            op.scaled_events
                .iter()
                .map(|ev| ev.log_index)
                .chain(op.transfer_events.iter().map(|l| log_idx_value(l)))
        }));
        operations.extend(interest_accrual_ops);

        // Step 4e: Transfer (ERC20 Transfer leftover handling).
        let transfer_ops = self.create_transfer_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
        );
        assigned_log_indices.extend(transfer_ops.iter().flat_map(|op| {
            op.scaled_events
                .iter()
                .map(|ev| ev.log_index)
                .chain(op.transfer_events.iter().map(|l| log_idx_value(l)))
                .chain(op.balance_transfer_events.iter().map(|l| log_idx_value(l)))
        }));
        operations.extend(transfer_ops);

        // Step 4f: unassigned events (preserve the Python's ERC20-Transfer filter).
        let unassigned_events = events
            .iter()
            .copied()
            .filter(|l| {
                let idx = log_idx_value(l);
                !assigned_log_indices.contains(&idx) && !is_erc20_transfer_log(l)
            })
            .collect();

        // Step 5: validators (per-op).
        for op in &mut operations {
            Self::validate_operation(op);
        }

        Ok(TransactionOperations {
            tx_hash,
            block_number,
            operations,
            unassigned_events,
        })
    }
}
// ── free helpers (top-level + ParseError conversions) ─────────────────────

/// `From<DbError>` for `ParseError` (the substrate-lookup failures surface as
/// `ParseError::Substrate`).
impl From<degenbot_db::DbError> for ParseError {
    fn from(e: degenbot_db::DbError) -> Self {
        ParseError::Substrate(e.to_string())
    }
}

/// `From<rusqlite::Error>` for `ParseError` (the ad-hoc SELECTs the parser
/// does in `get_a_token_for_asset` + `classify_token_type`).
impl From<rusqlite::Error> for ParseError {
    fn from(e: rusqlite::Error) -> Self {
        ParseError::Substrate(format!("sqlite: {e}"))
    }
}
