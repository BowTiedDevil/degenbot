//! Aave V3 transaction operations parser — the per-Ethereum-tx stateful
//! grouping engine over the chunk's decoded receipt logs.
//!
//! Given a `&[&Log]` slice (the tx's RPC-fetched receipt logs — decoded
//! in-place via [`degenbot_decoders::aave_event_decoder::decode_aave_log`])
//! plus a borrowed chunk-tx `&Connection` (the chunk-atomicity invariant —
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
//!   the tolerance comparison, the per-log ownership classification).
//!
//! # Raw-arithmetic matching (escalation trigger)
//!
//! `ScaledTokenProcessor` subsumes the enrichment layer (both route through
//! the same rounding). This parser does NOT call `ScaledTokenProcessor` —
//! the matching uses raw `value - balance_increase` /
//! `value + balance_increase`; the `ScaledTokenProcessor` is the apply-path
//! concern. If the standard-builder tests reveal a divergence between the
//! raw-arithmetic matching here and `ScaledTokenProcessor::process_*` on an
//! edge branch, escalate (don't paper over).

use super::run::substrate::ChunkSubstrate;
use crate::operations::{
    Operation, OperationType, ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType,
    TokenType, SCALED_AMOUNT_POOL_REVISION, TOKEN_AMOUNT_MATCH_TOLERANCE,
};
use crate::updater::evidence::TransactionEvidence;
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_decoders::aave_event_decoder::{
    self, AaveV3Erc20TransferEvent, AaveV3ScaledTokenBalanceTransferEvent,
    AaveV3ScaledTokenBurnEvent, AaveV3ScaledTokenMintEvent, DecodedAaveEvent,
};

use std::collections::{BTreeMap, HashMap, HashSet};

mod liquidation;
pub mod ownership;
mod pool_events;
mod post_loop;
mod scaled_tokens;
mod standard;
mod util;
mod validation;

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

#[cfg(test)]
mod receipt_repro;

use ownership::DecodedReceiptLog;
pub use ownership::{
    LogOwner, NonOperationRole, OutOfScope, OwnershipCensus, OwnershipRefusal, ReceiptOwnership,
};
pub use util::amounts_match_with_tolerance;
pub(crate) use util::{addr_to_hex, log_idx_value, parse_address};
use util::{is_minted_to_treasury_log, EventOwnership};

use liquidation::CallOwnership;
use pool_events::extract_pool_events;

// ── the parser struct ─────────────────────────────────────────────────────

/// The Aave V3 per-tx operations parser. Holds the per-tx
/// context (`market_id`, `pool_address`, the pre-resolved
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
pub struct TransactionOperationsParser<'a, 's> {
    /// `aave_v3_markets.id`.
    pub market_id: i64,
    /// `aave_v3_markets.chain_id` (for the `aave_gho_tokens` JOIN).
    pub chain_id: i64,
    /// The Pool contract address (for the `MintToTreasury`
    /// `caller_address == pool_address` test).
    pub pool_address: Address,
    /// The GHO token (underlying) address — None for non-GHO markets.
    pub gho_token_address: Option<Address>,
    /// The GHO vToken address — None for non-GHO markets. The parser's
    /// `_decode_transfer_event` uses this to classify a Transfer on the GHO
    /// vToken contract as a `GhoDebtTransfer`.
    pub gho_vtoken_address: Option<Address>,
    /// The chunk substrate cache — the parse-time lookups + the
    /// POOL-revision read ride its prefetch + write-overlay (the
    /// chunk-atomicity
    /// read-your-own-writes contract lives in `run/substrate.rs`). The `'s`
    /// lifetime is INDEPENDENT of `'a` (the conn/log borrow): the substrate
    /// reborrow ends when the parser drops, so the run loop can keep using
    /// the cache for the apply + the liquidation patterns.
    pub(crate) substrate: &'s mut ChunkSubstrate,
    /// The Pool contract revision resolved at parse-start. Served from
    /// the chunk substrate cache (the per-parse read behind it); mid-tx `PoolUpdated`
    /// config events are the orchestrator's concern.
    pub pool_revision: u32,
    /// The log borrow's lifetime appears only in the parse entry points, so the
    /// struct carries it as a marker rather than a field.
    marker: std::marker::PhantomData<&'a ()>,
}

/// Errors raised by the parser — the chunk-atomicity invariant surfaces these
/// to the caller's chunk-tx loop, which rolls back the whole chunk on any
/// `Err`. The failure modes are assertion/value errors + the
/// `[cold]` look-up `MissingRow` errors.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A `pool_event` had no matching `scaled_token` event — surfaces as
    /// `Err` not panic so the chunk rolls back cleanly.
    #[error("matching scaled-token event not found: {0}")]
    NoMatch(String),
    /// A substrate lookup failed (a SELECT error, a decode failure — e.g. the
    /// market has no aToken for the reserve the Supply event named).
    #[error("substrate lookup failed: {0}")]
    Substrate(String),
    /// An RPC-style `ray_div` math error from the `MintToTreasury` v8 branch.
    #[error("ray-div math error: {0}")]
    RayMath(#[from] crate::WadRayError),
    /// The transaction's receipt could not be given exactly one owner per log:
    /// two operations claim one log, an operation claims a log the receipt
    /// settled as a role, an Aave log ended the parse unowned, or a market
    /// contract emitted a log the decoder cannot name (ADR-105 D5).
    #[error("receipt ownership refused: {0}")]
    Ownership(#[from] OwnershipRefusal),
    /// The operation at `log_index` is named by a CALL (a liquidation's
    /// collateral leg), and this parse carries no call evidence — a receipt
    /// cannot name the sub-call that emitted the leg (ADR-105 D2).
    #[error("log {log_index} requires call evidence, and this parse carries none")]
    MissingCallEvidence {
        /// The log whose owner is named by a call.
        log_index: u64,
    },
    /// A relational fact the chunk's seed does not hold: the seed is the
    /// complete read position, so a miss refuses (ADR-105 D4).
    #[error("{0}")]
    IncompleteSeed(#[from] crate::updater::run::seed::IncompleteChunkSeed),
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
    /// The transaction's receipt with exactly one owner per log: an operation, a
    /// recognized non-operation role, or `OutOfScope` (ADR-105 D5). The parse
    /// refuses to return until that census is total.
    pub ownership: ReceiptOwnership,
}

// ── the parser impl ───────────────────────────────────────────────────────

impl<'a, 's> TransactionOperationsParser<'a, 's> {
    /// Construct a parser with the per-tx context pre-resolved by the caller
    /// (the orchestrator does the GHO-token resolution before instantiation;
    /// the parser doesn't RPC).
    /// # Errors
    /// Returns [`ParseError::Substrate`] if the pool-revision lookup fails.
    #[expect(clippy::similar_names)] // gho_token vs gho_vtoken is intrinsic to the domain
    pub fn new(
        market_id: i64,
        chain_id: i64,
        pool_address: Address,
        gho_token_address: Option<Address>,
        gho_vtoken_address: Option<Address>,
        substrate: &'s mut ChunkSubstrate,
    ) -> Result<Self, ParseError> {
        let pool_revision = substrate.pool_revision("POOL")?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "POOL contract revision missing for market_id={market_id}"
            ))
        })?;
        Ok(Self {
            market_id,
            chain_id,
            pool_address,
            gho_token_address,
            gho_vtoken_address,
            substrate,
            pool_revision,
            marker: std::marker::PhantomData,
        })
    }

    // ── the matching fns (no `&self` — pure helpers, exposed for tests) ──

    /// The parity-critical tolerance gate. Pool revision ≥
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

    /// Whether two transfer-report types describe the same balance movement
    /// (the index-less ERC20 report ↔ the index-bearing `BalanceTransfer`
    /// report). The ownership classification uses it to pair a movement's two
    /// reports.
    #[must_use]
    pub fn are_compatible_transfer_types(
        ev1: ScaledTokenEventType,
        ev2: ScaledTokenEventType,
    ) -> bool {
        util::are_compatible_transfer_types(ev1, ev2)
    }

    /// The emitters this market tracks as its own tokens.
    ///
    /// A token event from an emitter outside this set is another market's series
    /// or a reserve underlying: the receipt settles it as out of scope, and it
    /// never becomes an operation source (ADR-105 D5).
    ///
    /// The prefetched asset rows answer the census in memory. Only a cache with no
    /// prefetched rows (the lazy cache) has to probe the token tables, and its
    /// answers are cached for the chunk exactly as the per-log decode already
    /// cached them.
    ///
    /// # Errors
    ///
    /// A token-table lookup failure on the probing path surfaces as
    /// [`ParseError::Substrate`].
    fn market_tokens(
        &mut self,
        decoded: &[DecodedReceiptLog<'_>],
    ) -> Result<BTreeMap<Address, TokenType>, ParseError> {
        let mut tokens: BTreeMap<Address, TokenType> =
            self.substrate.market_token_census().into_iter().collect();
        if let Some(gho_vtoken) = self.gho_vtoken_address {
            tokens.insert(gho_vtoken, TokenType::Vtoken);
        }
        if self.substrate.has_prefetched_assets() {
            return Ok(tokens);
        }
        for event in decoded.iter().filter_map(DecodedReceiptLog::decoded) {
            let emitter = match event {
                DecodedAaveEvent::ScaledTokenMint(ev) => ev.token_address,
                DecodedAaveEvent::ScaledTokenBurn(ev) => ev.token_address,
                DecodedAaveEvent::ScaledTokenBalanceTransfer(ev) => ev.token_address,
                DecodedAaveEvent::Erc20Transfer(ev) => ev.token_address,
                _ => continue,
            };
            if tokens.contains_key(&emitter) {
                continue;
            }
            if let Some(kind) = self.classify_token_type(emitter)? {
                tokens.insert(emitter, kind);
            }
        }
        Ok(tokens)
    }

    // ── the `parse()` scaffold ──

    /// Parse the tx's logs into [`TransactionOperations`], receipt-only.
    ///
    /// This is the receipt-only entry point: the logs ARE the evidence, so an
    /// operation whose legs a CALL names (a liquidation's collateral sub-call)
    /// is refused with [`ParseError::MissingCallEvidence`] rather than
    /// attributed by a log window. The orchestrator uses
    /// [`Self::parse_evidence`], which carries the transaction's bound evidence.
    ///
    /// # Errors
    /// Returns [`ParseError`] on any builder-matching failure (the caller
    /// rolls back the chunk-tx), on a receipt that cannot be owned (ADR-105 D5),
    /// and on call-backed operations this parse carries no call evidence for.
    /// Validator-level assertion failures are routed to
    /// `Operation.validation_errors` (non-fatal — the caller can decide).
    pub fn parse(
        &mut self,
        events: &'a [&'a Log],
        tx_hash: [u8; 32],
    ) -> Result<TransactionOperations<'a>, ParseError> {
        self.parse_inner(events, None, tx_hash)
    }

    /// Parse the transaction its bound evidence describes (ADR-105 D2, D5).
    ///
    /// The complete receipt is the log set — every log of it receives one
    /// owner — and, when the evidence carries the transaction's call trace, the
    /// call that emitted each receipt log, which is what names a liquidation's
    /// collateral legs. A receipt-only transaction is parsed from its receipt and
    /// never acquires a trace or a call path.
    ///
    /// # Errors
    ///
    /// Every [`Self::parse`] refusal, and the ownership refusals of the complete
    /// receipt.
    pub fn parse_evidence(
        &mut self,
        evidence: &'a TransactionEvidence,
        tx_hash: [u8; 32],
    ) -> Result<TransactionOperations<'a>, ParseError> {
        let events: Vec<&'a Log> = evidence.logs();
        let calls = match evidence {
            TransactionEvidence::ReceiptOnly(_) => None,
            TransactionEvidence::TraceBacked(joined) => Some(CallOwnership::of(joined)),
        };
        self.parse_inner(&events, calls.as_ref(), tx_hash)
    }

    #[expect(clippy::too_many_lines, clippy::panic_in_result_fn)] // parse() is intrinsic — parity-drift mirror
    fn parse_inner(
        &mut self,
        events: &[&'a Log],
        calls: Option<&CallOwnership>,
        tx_hash: [u8; 32],
    ) -> Result<TransactionOperations<'a>, ParseError> {
        assert!(
            !events.is_empty(),
            "parser requires at least one event (callers pre-filter empty logs)"
        );
        let block_number = events.first().and_then(|l| l.block_number).unwrap_or(0);

        // Step 1: identify pool events (anchors). .
        let pool_events = extract_pool_events(events, self.pool_address);
        // Step 2: decode every log ONCE — the receipt's ownership census and the
        // market's scaled events read the same decode. A token event emitted by a
        // series this market does not track is another market's (or an
        // underlying's) movement: out of scope, never an operation source.
        let decoded: Vec<DecodedReceiptLog<'a>> = events
            .iter()
            .map(|ev| DecodedReceiptLog::new(ev, aave_event_decoder::decode_aave_log(ev)))
            .collect();
        let tokens = self.market_tokens(&decoded)?;
        let mut scaled_events: Vec<ScaledTokenEvent<'a>> = Vec::new();
        for receipt_log in &decoded {
            match receipt_log.decoded() {
                Some(DecodedAaveEvent::ScaledTokenMint(m))
                    if tokens.contains_key(&m.token_address) =>
                {
                    scaled_events.push(self.decode_mint_event(receipt_log.log(), m)?);
                }
                Some(DecodedAaveEvent::ScaledTokenBurn(b))
                    if tokens.contains_key(&b.token_address) =>
                {
                    scaled_events.push(self.decode_burn_event(receipt_log.log(), b)?);
                }
                Some(DecodedAaveEvent::ScaledTokenBalanceTransfer(bt))
                    if tokens.contains_key(&bt.token_address) =>
                {
                    scaled_events.push(self.decode_balance_transfer_event(receipt_log.log(), bt)?);
                }
                Some(DecodedAaveEvent::Erc20Transfer(t))
                    if tokens.contains_key(&t.token_address) =>
                {
                    if let Some(s) = self.decode_transfer_event(receipt_log.log(), t)? {
                        scaled_events.push(s);
                    }
                }
                _ => {} // pool and other Aave events are classified by the receipt census.
            }
        }
        scaled_events.sort_by_key(|e| e.log_index);

        // Step 2b: classify each decoded log's ownership ONCE — the semantic
        // Mint/Burn carrying a delta, a zero-address companion of one, or a
        // movement report (index-less ERC20 carrier + scaled `BalanceTransfer`).
        // Builders consume these dispositions instead of re-deriving pairing
        // rules or coordinating through the claim set.
        let ownership = EventOwnership::classify(&scaled_events);
        // Step 2c: the receipt's total census — every log is an operation's, a
        // recognized role, or deliberately out of scope, before any operation
        // exists. The parse then claims into it and closes it.
        let mut receipt = ReceiptOwnership::classify(
            &decoded,
            &scaled_events,
            &ownership,
            &tokens,
            self.pool_address,
        )?;

        // Step 3: group into operations. The claim set starts with every log the
        // receipt already settled as a terminal role: a builder that consults it
        // skips those logs by construction, and a builder that would claim one
        // is refused when the ledger is closed.
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut assigned_log_indices: HashSet<u64> = events
            .iter()
            .map(|event| log_idx_value(event))
            .filter(|log_index| receipt.is_terminal_role(*log_index))
            .collect();
        let mut next_op_id: u32 = 0;

        for pool_event in pool_events {
            let op = self.create_operation_from_pool_event(
                next_op_id,
                pool_event,
                &scaled_events,
                &ownership,
                events,
                calls,
                &mut assigned_log_indices,
            )?;
            op.claim_into(&mut assigned_log_indices);
            operations.push(op);
            next_op_id += 1;
        }

        // Step 4b: MintToTreasury (mirrors `_create_mint_to_treasury_operations`).
        let minted_to_treasury_events: Vec<&Log> = events
            .iter()
            .copied()
            .filter(|l| l.address() == self.pool_address && is_minted_to_treasury_log(l))
            .collect();
        let mint_to_treasury_ops = self.create_mint_to_treasury_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
            &minted_to_treasury_events,
        )?;
        for op in &mint_to_treasury_ops {
            op.claim_into(&mut assigned_log_indices);
        }
        operations.extend(mint_to_treasury_ops);

        // Step 4c: DeficitCoverage (BalanceTransfer + Burn pairs).
        let deficit_coverage_ops = self.create_deficit_coverage_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
        );
        for op in &deficit_coverage_ops {
            op.claim_into(&mut assigned_log_indices);
        }
        operations.extend(deficit_coverage_ops);

        // Step 4d: InterestAccrual.
        let interest_accrual_ops = self.create_interest_accrual_operations(
            &scaled_events,
            &mut assigned_log_indices,
            &mut next_op_id,
        );
        for op in &interest_accrual_ops {
            op.claim_into(&mut assigned_log_indices);
        }
        operations.extend(interest_accrual_ops);

        // Step 4e: standalone transfer reports (leftover handling).
        let transfer_ops = Self::create_transfer_operations(
            self.pool_revision,
            &scaled_events,
            &ownership,
            &assigned_log_indices,
            &mut next_op_id,
        );
        for op in &transfer_ops {
            op.claim_into(&mut assigned_log_indices);
        }
        operations.extend(transfer_ops);

        // Step 5: every operation claims the logs it consumed, then the receipt
        // must be owned exactly once: a log two operations claim, a log an
        // operation claims that the receipt settled as a role, and a log still
        // expecting an owner are all refusals (ADR-105 D5).
        for op in &operations {
            receipt.claim_operation(op)?;
        }
        receipt.conclude()?;

        // Step 6: validators (per-op).
        for op in &mut operations {
            Self::validate_operation(op);
        }

        Ok(TransactionOperations {
            tx_hash,
            block_number,
            operations,
            ownership: receipt,
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
