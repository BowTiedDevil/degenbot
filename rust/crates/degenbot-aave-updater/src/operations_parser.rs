//! Aave V3 transaction operations parser — port of
//! `src/degenbot/cli/aave/operations_parser.py::TransactionOperationsParser`.
//!
//! The parser is a **per-Ethereum-tx stateful grouping engine**: given a
//! `&[&Log]` slice (the tx's RPC-fetched receipt logs — decoded in-place via
//! [`degenbot_decoders::aave_event_decoder::decode_aave_log`]) + a borrowed
//! chunk-tx `&Connection` (the §3.4 atomicity invariant — every get/lookup
//! runs on the caller's single chunk Transaction), it matches Pool events
//! (Supply/Borrow/Repay/Withdraw/MintToTreasury/Deficit) to their constituent
//! `ScaledToken` events (aToken/vToken Mint/Burn/Transfer + plain ERC20
//! Transfer) by user + amount +/- a pool-revision ray-flooring tolerance
//! ([`Self::amounts_match`]) + emits typed [`Operation`]s.
//!
//! # What this file owns (HQF5NQ-A)
//!
//! - the per-tx [`TransactionOperationsParser`] struct (mirrors the Python's
//!   `__init__` — `market_id` + `pool_address` + `treasury_address` + the
//!   pre-resolved GHO token/vToken addresses + the `&Connection` borrow).
//! - the matching helpers: [`Self::amounts_match`]
//!   (the §4.2-critical tolerance gate),
//!   [`Self::are_compatible_transfer_types`], the 4 `_decode_*_event`
//!   wrappers (Mint/Burn/BalanceTransfer/Transfer → [`ScaledTokenEvent`] with
//!   the emitter-address classification via
//!   [`DegenbotDb::lookup_asset_by_token_address_on_conn`] +
//!   [`DegenbotDb::lookup_asset_id_by_token_address_on_conn`]).
//! - the `parse()` scaffold (extract `pool_events` + extract `scaled_events` + the
//!   per-pool-event dispatch loop + the 5-phase post-loop append).
//! - the 9 standard per-operation builders (Supply/Withdraw/Borrow/Repay/
//!   RepayWithAtokens/MintToTreasury/Deficit/DeficitCoverage/InterestAccrual/
//!   Transfer). The `LiquidationCall` builder is a STUB delegated to HQF5NQ-B
//!   — it returns an Unknown operation so the `parse()` scaffold doesn't crash
//!   on a tx with liquidations (B will replace the stub).
//! - the per-Operation validators (mirror `operations_parser.py::_validate_*`
//!   — strict assert-on-violation, fills `validation_errors`).
//!
//! # What this file does NOT own
//!
//! - the liquidation engine (`_create_liquidation_operation` +
//!   `_collect_debt_burns` `SINGLE/COMBINED_BURN/SEPARATE_BURNS` pattern
//!   detection + `_analyze_liquidation_scenarios`) → HQF5NQ-B (this file
//!   provides a stub builder so multi-liquidation txs don't panic in A's
//!   unit tests; B replaces).
//! - the apply dispatch glue (`process_transaction` entry point +
//!   `Operation`→`AaveChunkEvent` variant construction + the GHO-discount
//!   machinery) → HQF5NQ-C.
//! - the `TransactionOperations.validate()` strict pass — currently per-Op
//!   validator calls fill `validation_errors`; the top-level "no unassigned
//!   required events / no ambiguous assignments" pass lives in C (it
//!   requires the dispatch glue's id-resolution state).
//!
//! # The plumbing-equivalence caveat (escalation trigger — Finding 1)
//!
//! The Python pipeline is 4-stage: `parser.parse(events)` →
//! `ScaledEventEnricher.enrich(scaled_event, operation)` →
//! `EnrichedScaledTokenEvent` → `token_processor._process_*_with_match`.
//! SCALEAPPLY's `ScaledTokenProcessor` already subsumes the enrichment layer
//! (they route through the same `TokenMath` rounding). **This file does NOT
//! call `ScaledTokenProcessor`** — the matching in A uses raw
//! `value - balance_increase` / `value + balance_increase` (the §4.2-Python
//! matching heuristic); `ScaledTokenProcessor` is C's concern on the apply
//! path. The plumbing-equivalence caveat is an A-tests + C-dispatch concern:
//! if the standard-builder tests reveal a divergence between the
//! raw-arithmetic matching here + `ScaledTokenProcessor::process_*` on an
//! edge branch, escalate (don't paper over).

use crate::operations::{
    Operation, OperationType, ScaledTokenEvent, ScaledTokenEventData, ScaledTokenEventType,
    SCALED_AMOUNT_POOL_REVISION, TOKEN_AMOUNT_MATCH_TOLERANCE,
};
use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_db::DegenbotDb;
use degenbot_decoders::aave_event_decoder::{
    self, DecodedAaveEvent, Erc20TransferEvent, ScaledTokenBalanceTransferEvent,
    ScaledTokenBurnEvent, ScaledTokenMintEvent,
};
use degenbot_evm_math::RayRounding;
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};

// ── the parser struct ─────────────────────────────────────────────────────

/// The Aave V3 per-tx operations parser (port of
/// `operations_parser.py::TransactionOperationsParser`). Holds the per-tx
/// context (`market_id`, `pool_address`, `treasury_address`, the pre-resolved
/// GHO token/vToken addresses) + a borrowed chunk-tx `&Connection`. The
/// orchestrator (6SWY4R) constructs one fresh per Ethereum tx.
///
/// # Lifetime
///
/// `'a` ties the `&Connection` borrow the parser resolves address→id through
/// (the caller's chunk Transaction). The parser's output `Operation`s borrow
/// the input `&'b [&&'b Log]` slice independently — the parser struct itself
/// doesn't borrow the logs.
#[derive(Debug)]
#[allow(clippy::module_name_repetitions)]
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
    /// vToken contract as a `GhoDebtTransfer` (matches
    /// `operations_parser.py:_decode_transfer_event`).
    pub gho_vtoken_address: Option<Address>,
    /// The borrowed `&Connection` — every substrate lookup runs on this
    /// (the §3.4 invariant).
    pub conn: &'a rusqlite::Connection,
    /// The Pool contract revision resolved at parse-start (DP4). Read once
    /// via [`DegenbotDb::lookup_pool_revision_on_conn`]; mid-tx `PoolUpdated`
    /// config events are the orchestrator's (6SWY4R) concern.
    pub pool_revision: u32,
}

/// Errors raised by the parser — the §3.4 atomicity invariant surfaces these
/// to the caller's chunk-tx loop, which rolls back the whole chunk on any
/// `Err`. Mirrors the Python's `AssertionError`/`ValueError` raises + the
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
    RayMath(#[from] degenbot_evm_math::WadRayError),
}

// ── the entry-point types ─────────────────────────────────────────────────

/// The parser's output (mirror of `operations.py::TransactionOperations`).
/// Borrows the source logs for the lifetime of the input slice.
#[derive(Debug)]
#[allow(clippy::module_name_repetitions)]
pub struct TransactionOperations<'a> {
    /// The tx hash (raw bytes). Set by the caller.
    pub tx_hash: [u8; 32],
    /// The block number.
    pub block_number: u64,
    /// The parsed operations (in the order the parser produced them —
    /// pool-event-dispatched first, then the phase-4 post-loop appends).
    pub operations: Vec<Operation<'a>>,
    /// Logs that weren't matched to any Operation (mirror of
    /// `TransactionOperations.unassigned_events`).
    pub unassigned_events: Vec<&'a Log>,
}

// ── the parser impl ───────────────────────────────────────────────────────

impl<'a> TransactionOperationsParser<'a> {
    /// Construct a parser with the per-tx context pre-resolved by the caller
    /// (the orchestrator — 6SWY4R — does the GHO-token / treasury resolution
    /// before instantiation; the parser doesn't RPC).
    /// # Errors
    /// Returns [`ParseError::Substrate`] if the pool-revision lookup fails.
    #[allow(clippy::similar_names)] // gho_token vs gho_vtoken is intrinsic to the domain
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

    /// Mirrors `operations_parser.py::TransactionOperationsParser::_amounts_match`:
    /// the §4.2-critical tolerance gate. Pool revision ≥
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

    /// Mirrors `_are_compatible_transfer_types` (ERC20 Transfer ↔ `BalanceTransfer`
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

    // ── the decode wrappers (mirror `_decode_mint/burn/balance_transfer/transfer_event`) ──

    /// Mirrors `_decode_mint_event`. Decodes a `ScaledTokenMint` log via
    /// ECFB5C + classifies by emitter-address → [`ScaledTokenEventType`]
    /// (`GhoDebtMint` if the emitter is `gho_vtoken_address`; `CollateralMint` if
    /// aToken; `DebtMint` if vToken).
    fn decode_mint_event(&self, log: &'a Log, ev: &ScaledTokenMintEvent) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "mint");
        ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Mint {
                caller: ev.caller,
                on_behalf_of: ev.on_behalf_of,
                value: ev.value,
                balance_increase: ev.balance_increase,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.on_behalf_of,
            caller_address: Some(ev.caller),
            from_address: None,
            target_address: None,
            amount: ev.value,
            balance_increase: Some(ev.balance_increase),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        }
    }

    /// Mirrors `_decode_burn_event`. Decodes a `ScaledTokenBurn` log +
    /// classifies by emitter-address (`GhoDebtBurn` / `CollateralBurn` / `DebtBurn`).
    fn decode_burn_event(&self, log: &'a Log, ev: &ScaledTokenBurnEvent) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_mint_burn(token_address, "burn");
        ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Burn {
                from: ev.from,
                target: ev.target,
                value: ev.value,
                balance_increase: ev.balance_increase,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.target),
            amount: ev.value,
            balance_increase: Some(ev.balance_increase),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        }
    }

    /// Mirrors `_decode_balance_transfer_event`. Decodes a
    /// `ScaledTokenBalanceTransfer` log + classifies by emitter-address
    /// (`CollateralTransfer` / `DebtTransfer` / `GhoDebtTransfer`). NB: GHO vToken
    /// doesn't emit `BalanceTransfer` in practice (the GHO mechanism uses plain
    /// ERC20 Transfer for the user→user movement) — but the classification
    /// path covers it defensively.
    fn decode_balance_transfer_event(
        &self,
        log: &'a Log,
        ev: &ScaledTokenBalanceTransferEvent,
    ) -> ScaledTokenEvent<'a> {
        let token_address = ev.token_address;
        let event_type = self.classify_transfer(token_address);
        // BalanceTransfer has balance_increase = 0 (mirrors Python).
        ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::BalanceTransfer {
                from: ev.from,
                to: ev.to,
                value: ev.value,
                index: ev.index,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.to),
            amount: ev.value,
            balance_increase: Some(U256::ZERO),
            index: Some(ev.index),
            log_index: log_idx_value(log),
        }
    }

    /// Mirrors `_decode_transfer_event`. Decodes a plain ERC20 `Transfer`
    /// + classifies: GHO vToken → `GhoDebtTransfer`; aToken →
    ///   `Erc20CollateralTransfer`; vToken → `Erc20DebtTransfer`;
    ///   the GHO-discount-token → `DiscountTransfer`; otherwise `None` (the log is
    ///   for an unrelated contract — the parser skips it).
    fn decode_transfer_event(
        &self,
        log: &'a Log,
        ev: &Erc20TransferEvent,
    ) -> Option<ScaledTokenEvent<'a>> {
        let token_address = ev.token_address;
        let event_type = if self.gho_vtoken_address == Some(token_address) {
            ScaledTokenEventType::GhoDebtTransfer
        } else {
            self.classify_token_type(token_address)?
        };
        Some(ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Transfer {
                from: ev.from,
                to: ev.to,
                value: ev.value,
            },
            event_type,
            token_address,
            user_address: ev.from,
            caller_address: None,
            from_address: Some(ev.from),
            target_address: Some(ev.to),
            amount: ev.value,
            balance_increase: None,
            index: None,
            log_index: log_idx_value(log),
        })
    }

    /// Classify a Mint/Burn event's emitter token address. Mirrors
    /// `_get_event_type_for_token`. Returns the matching
    /// [`ScaledTokenEventType`]; the caller's match-arm on the
    /// `event_category` ("mint"/"burn") maps to `CollateralMint` / `DebtMint` /
    /// `GhoDebtMint` (and the corresponding burn variants).
    ///
    /// # Panics
    /// Panics if the emitter is neither GHO-vToken, a known aToken, nor a
    /// known vToken for this market (mirror of the Python's `raise ValueError`
    /// on unexpected `token_type`). The caller should pre-filter via
    /// `classify_token_type` if a non-panic is needed.
    fn classify_mint_burn(
        &self,
        token_address: Address,
        event_category: &str,
    ) -> ScaledTokenEventType {
        if self.gho_vtoken_address == Some(token_address) {
            return match event_category {
                "mint" => ScaledTokenEventType::GhoDebtMint,
                "burn" => ScaledTokenEventType::GhoDebtBurn,
                _ => unreachable!("event_category is mint|burn"),
            };
        }
        let token_type = self.classify_token_type(token_address).unwrap_or_else(|| {
            panic!(
                "unexpected token at {token_address} for market {}",
                self.market_id
            )
        });
        match (token_type, event_category) {
            (ScaledTokenEventType::CollateralMint |
ScaledTokenEventType::CollateralTransfer |
ScaledTokenEventType::Erc20CollateralTransfer, "mint") => ScaledTokenEventType::CollateralMint,
            (ScaledTokenEventType::CollateralBurn |
ScaledTokenEventType::CollateralTransfer |
ScaledTokenEventType::Erc20CollateralTransfer, "burn") => ScaledTokenEventType::CollateralBurn,
            (ScaledTokenEventType::DebtMint | ScaledTokenEventType::DebtTransfer |
ScaledTokenEventType::Erc20DebtTransfer, "mint") => ScaledTokenEventType::DebtMint,
            (ScaledTokenEventType::DebtBurn | ScaledTokenEventType::DebtTransfer |
ScaledTokenEventType::Erc20DebtTransfer, "burn") => ScaledTokenEventType::DebtBurn,
            // classify_token_type returns a transfer variant on aToken/vToken;
            // re-derive here (the Python's helper). The conversion is
            // aToken → CollateralMint/Burn, vToken → DebtMint/Burn.
            _ => panic!("classify_token_type returned a non-token-type variant for token at {token_address} (event_category={event_category})"),
        }
    }

    /// Classify a `BalanceTransfer` event's emitter → `CollateralTransfer` /
    /// `DebtTransfer` / `GhoDebtTransfer`. Mirrors the Python's transfer-branch
    /// of `_get_event_type_for_token`.
    fn classify_transfer(&self, token_address: Address) -> ScaledTokenEventType {
        if self.gho_vtoken_address == Some(token_address) {
            return ScaledTokenEventType::GhoDebtTransfer;
        }
        let token_type = self.classify_token_type(token_address).unwrap_or_else(|| {
            panic!(
                "unexpected token at {token_address} for market {}",
                self.market_id
            )
        });
        // classify_token_type returned a transfer variant — re-derive.
        match token_type {
            ScaledTokenEventType::DebtTransfer | ScaledTokenEventType::Erc20DebtTransfer => {
                ScaledTokenEventType::DebtTransfer
            }
            _ => ScaledTokenEventType::CollateralTransfer,
        }
    }

    /// Classify a token-address by which asset-table column matches (aToken /
    /// vToken / GHO-discount-token). Mirrors `_get_token_type`. Returns `None`
    /// if the address isn't any of those (the parser skips the log).
    ///
    /// Returns a [`ScaledTokenEventType`] *transfer variant* as the
    /// discriminator (the caller's match-arm maps mint/burn variants as
    /// needed). This is a slight overloading of the enum but matches the
    /// Python's three-way classification surface.
    fn classify_token_type(&self, token_address: Address) -> Option<ScaledTokenEventType> {
        let addr_hex = addr_to_hex(token_address);
        // Try aToken first (mirror Python order).
        if DegenbotDb::lookup_asset_id_by_token_address_on_conn(
            self.conn,
            self.market_id,
            &addr_hex,
            "a_token",
        )
        .ok()
        .flatten()
        .is_some()
        {
            return Some(ScaledTokenEventType::Erc20CollateralTransfer);
        }
        if DegenbotDb::lookup_asset_id_by_token_address_on_conn(
            self.conn,
            self.market_id,
            &addr_hex,
            "v_token",
        )
        .ok()
        .flatten()
        .is_some()
        {
            return Some(ScaledTokenEventType::Erc20DebtTransfer);
        }
        // GHO-discount-token check (the v_gho_discount_token column on
        // aave_gho_tokens; only set if the discount mechanism is active).
        if self
            .conn
            .query_row(
                "SELECT g.v_gho_discount_token FROM aave_gho_tokens g
                 JOIN erc20_tokens t ON t.id = g.token_id
                 WHERE t.chain = ?1 AND g.v_gho_discount_token IS NOT NULL",
                rusqlite::params![self.chain_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
            .is_some_and(|s| s.eq_ignore_ascii_case(&addr_hex))
        {
            return Some(ScaledTokenEventType::DiscountTransfer);
        }
        None
    }

    // ── the `parse()` scaffold (mirror `operations_parser.py::parse`) ──

    /// Parse the tx's logs into [`TransactionOperations`]. The entry point
    /// the orchestrator (6SWY4R) calls per-tx inside the chunk-tx loop.
    ///
    /// # Errors
    /// Returns [`ParseError`] on any builder-matching failure (the caller
    /// rolls back the chunk-tx). Validator-level assertion failures (the
    /// `_validate_*` fns) are routed to `Operation.validation_errors`
    /// (non-fatal — the caller can decide; the Python's
    /// `TransactionOperations.validate` is the strict top-level pass + is HQF5NQ-C).
    #[allow(clippy::missing_panics_doc, clippy::too_many_lines)] // parse() is intrinsic — §4.2-drift mirror
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

        // Step 1: identify pool events (anchors). Mirrors `_extract_pool_events`.
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
                    _ => {} // pool events handled separately in step 1
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

    // ── the per-pool-event dispatch (`_create_operation_from_pool_event`) ──

    #[allow(clippy::too_many_arguments)]
    fn create_operation_from_pool_event(
        &self,
        operation_id: u32,
        pool_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        all_events: &'a [&'a Log],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let topic = pool_event
            .topics()
            .first()
            .copied()
            .ok_or_else(|| ParseError::Substrate("pool_event has no topic".into()))?;
        // Match by topic — use the decoder's topic constants to dispatch.
        if topic == aave_event_decoder::SUPPLY_TOPIC {
            self.create_supply_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::WITHDRAW_TOPIC {
            self.create_withdraw_operation(
                operation_id,
                pool_event,
                scaled_events,
                assigned_indices,
            )
        } else if topic == aave_event_decoder::BORROW_TOPIC {
            self.create_borrow_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::REPAY_TOPIC {
            self.create_repay_operation(operation_id, pool_event, scaled_events, assigned_indices)
        } else if topic == aave_event_decoder::LIQUIDATION_CALL_TOPIC {
            // HQF5NQ-B: the real liquidation engine (the
            // SINGLE/COMBINED_BURN/SEPARATE_BURNS pattern detection + the
            // `_analyze_liquidation_scenarios` / `_analyze_user_liquidation_
            // count` pre-analysis dicts over `all_events`).
            self.create_liquidation_operation(
                operation_id,
                pool_event,
                scaled_events,
                all_events,
                assigned_indices,
            )
        } else if topic == aave_event_decoder::DEFICIT_CREATED_TOPIC {
            Ok(self.create_deficit_operation(operation_id, pool_event))
        } else {
            Err(ParseError::Substrate(format!(
                "unexpected pool-event topic {topic}"
            )))
        }
    }

    // ── the 9 standard builders ─────────────────────────────────────────

    /// Mirrors `_create_supply_operation`. Pool Supply → match `CollateralMint`
    /// by `onBehalfOf` + amount (value - `balance_increase` vs `supply_amount`).
    fn create_supply_operation(
        &self,
        operation_id: u32,
        supply_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_supply_pool_event(supply_event)
            .ok_or_else(|| ParseError::Substrate("malformed Supply event".into()))?;
        // Resolve the expected aToken address.
        let expected_a_token = self
            .get_a_token_for_asset(decoded.reserve)?
            .ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no aToken for reserve {} in market {}",
                    decoded.reserve, self.market_id
                ))
            })?;

        // Find the matching CollateralMint event.
        let mut collateral_mint: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint {
                continue;
            }
            if ev.token_address != expected_a_token {
                continue;
            }
            if ev.user_address != decoded.on_behalf_of {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_principal = ev.amount - balance_increase;
            if !Self::amounts_match(calculated_principal, decoded.amount, self.pool_revision) {
                continue;
            }
            collateral_mint = Some(ev);
            break;
        }
        let collateral_mint = collateral_mint.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "SUPPLY at log={} missing CollateralMint",
                log_idx_value(supply_event)
            ))
        })?;

        // Look for matching Transfer event (mint-from-zero on the same aToken).
        let mut transfer_events: Vec<&'a Log> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.from_address != Some(Address::ZERO) {
                continue;
            }
            if ev.target_address != Some(decoded.on_behalf_of) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralTransfer
                && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
            {
                continue;
            }
            if ev.amount == collateral_mint.amount {
                transfer_events.push(ev.log);
                break;
            }
        }
        assert_eq!(transfer_events.len(), 1, "SUPPLY expects 1 Transfer");

        assigned_log_idx_for_event(assigned_indices, collateral_mint);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }

        Ok(Operation {
            operation_id,
            operation_type: OperationType::Supply,
            pool_revision: self.pool_revision,
            pool_event: Some(supply_event),
            scaled_events: vec![clone_scaled_event(collateral_mint)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Mirrors `_create_withdraw_operation`. Pool Withdraw → match `CollateralBurn`
    /// by user + amount (value + `balance_increase` vs `withdraw_amount`). The
    /// "interest-exceeds-withdrawal → Mint instead of Burn" branch is the
    /// §4.2-drift edge — verify plumbing equivalence.
    #[allow(clippy::too_many_lines)] // mirror's body intrinsic — §4.2-drift match has 6 branches
    fn create_withdraw_operation(
        &self,
        operation_id: u32,
        withdraw_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_withdraw_pool_event(withdraw_event)
            .ok_or_else(|| ParseError::Substrate("malformed Withdraw event".into()))?;
        let expected_a_token = self
            .get_a_token_for_asset(decoded.reserve)?
            .ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no aToken for reserve {} in market {}",
                    decoded.reserve, self.market_id
                ))
            })?;

        // First try a CollateralBurn.
        let mut collateral_burn: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralBurn {
                continue;
            }
            if ev.token_address != expected_a_token {
                continue;
            }
            if ev.user_address != decoded.user {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_burn = ev.amount + balance_increase;
            if !Self::amounts_match(calculated_burn, decoded.amount, self.pool_revision) {
                continue;
            }
            collateral_burn = Some(ev);
            break;
        }

        // Fallback: "interest exceeds withdrawal → Mint" branch.
        let mut interest_mint: Option<&ScaledTokenEvent<'a>> = None;
        if collateral_burn.is_none() {
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index) {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralMint {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                interest_mint = Some(ev);
                break;
            }
        }
        if collateral_burn.is_none() && interest_mint.is_none() {
            return Err(ParseError::NoMatch(format!(
                "WITHDRAW at log={} missing CollateralBurn + interest-Mint",
                log_idx_value(withdraw_event)
            )));
        }

        // Find the matching Transfer event (mint → from-zero; burn → to-zero).
        let mut transfer_event: Option<&'a Log> = None;
        if let Some(interest_mint_ev) = interest_mint {
            // Mint → Transfer (CreditTransfer) from any addr — Python `_create_withdraw_operation`
            // interest_mint-branch: search any (CollateralTransfer / ERC20_COLLATERAL_TRANSFER).
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index)
                    || ev.log_index == interest_mint_ev.log_index
                {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                transfer_event = Some(ev.log);
                break;
            }
        } else if let Some(burn) = collateral_burn {
            // Burn → Transfer (Transfer-to-zero).
            for ev in scaled_events {
                if assigned_indices.contains(&ev.log_index) || ev.log_index == burn.log_index {
                    continue;
                }
                if ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if ev.token_address != expected_a_token {
                    continue;
                }
                if ev.target_address != Some(Address::ZERO) {
                    continue;
                }
                transfer_event = Some(ev.log);
                break;
            }
        }
        let transfer_event = transfer_event.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "WITHDRAW at log={} missing transfer event",
                log_idx_value(withdraw_event)
            ))
        })?;

        let scaled_token_events: Vec<ScaledTokenEvent<'a>> = if let Some(im) = interest_mint {
            vec![clone_scaled_event(im)]
        } else {
            vec![clone_scaled_event(collateral_burn.unwrap())]
        };

        // Mark assignments.
        for ev in &scaled_token_events {
            assigned_indices.insert(ev.log_index);
        }
        assigned_indices.insert(log_idx_value(transfer_event));

        Ok(Operation {
            operation_id,
            operation_type: OperationType::Withdraw,
            pool_revision: self.pool_revision,
            pool_event: Some(withdraw_event),
            scaled_events: scaled_token_events,
            transfer_events: vec![transfer_event],
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Mirrors `_create_borrow_operation`. Pool Borrow → match `DebtMint` (or
    /// `GhoDebtMint` if the reserve is the GHO token). Includes GHO-BORROW
    /// detection.
    fn create_borrow_operation(
        &self,
        operation_id: u32,
        borrow_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_borrow_pool_event(borrow_event)
            .ok_or_else(|| ParseError::Substrate("malformed Borrow event".into()))?;
        let is_gho = self.gho_token_address == Some(decoded.reserve);

        let expected_event_type = if is_gho {
            ScaledTokenEventType::GhoDebtMint
        } else {
            ScaledTokenEventType::DebtMint
        };

        let mut debt_mint: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.user_address != decoded.on_behalf_of {
                continue;
            }
            if ev.event_type != expected_event_type {
                continue;
            }
            let balance_increase = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated_borrow = ev.amount - balance_increase;
            if !Self::amounts_match(calculated_borrow, decoded.amount, self.pool_revision) {
                continue;
            }
            debt_mint = Some(ev);
            break;
        }
        let debt_mint = debt_mint.ok_or_else(|| {
            ParseError::NoMatch(format!(
                "BORROW at log={} missing DebtMint",
                log_idx_value(borrow_event)
            ))
        })?;

        // Look for matching Transfer event from ZERO_ADDRESS.
        let mut transfer_events: Vec<&'a Log> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.from_address != Some(Address::ZERO) {
                continue;
            }
            if ev.target_address != Some(decoded.on_behalf_of) {
                continue;
            }
            if ev.amount != debt_mint.amount {
                continue;
            }
            transfer_events.push(ev.log);
            break;
        }
        assert_eq!(transfer_events.len(), 1, "BORROW expects 1 Transfer");

        let op_type = if is_gho {
            OperationType::GhoBorrow
        } else {
            OperationType::Borrow
        };
        assigned_log_idx_for_event(assigned_indices, debt_mint);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }

        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(borrow_event),
            scaled_events: vec![clone_scaled_event(debt_mint)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Mirrors `_create_repay_operation`. Pool Repay → match `DebtBurn` (or
    /// `GhoDebtBurn`), dispatching to `_create_repay_with_atokens_operation` if
    /// `useATokens=true`.
    fn create_repay_operation(
        &self,
        operation_id: u32,
        repay_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let decoded = decode_repay_pool_event(repay_event)
            .ok_or_else(|| ParseError::Substrate("malformed Repay event".into()))?;
        let is_gho = self.gho_token_address == Some(decoded.reserve);
        if decoded.use_a_tokens {
            assert!(!is_gho, "REPAY_WITH_ATOKENS for GHO is impossible");
            return self.create_repay_with_atokens_operation(
                operation_id,
                repay_event,
                decoded.reserve,
                decoded.user,
                decoded.amount,
                scaled_events,
                assigned_indices,
            );
        }
        // Standard REPAY path.
        let principal_repay_event = self.find_principal_repay_event(
            decoded.amount,
            is_gho,
            scaled_events,
            assigned_indices,
        )?;
        // Find debt Transfer-to-zero for the matched principal.
        let transfer_events = self.find_debt_transfer_to_zero(
            decoded.user,
            principal_repay_event.amount,
            scaled_events,
            assigned_indices,
        );
        let op_type = if is_gho {
            OperationType::GhoRepay
        } else {
            OperationType::Repay
        };
        assigned_log_idx_for_event(assigned_indices, principal_repay_event);
        for l in &transfer_events {
            assigned_indices.insert(log_idx_value(l));
        }
        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(repay_event),
            scaled_events: vec![clone_scaled_event_from_ref(principal_repay_event)],
            transfer_events,
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Mirrors `_create_repay_with_atokens_operation`. _`find_principal_repay_event`
    /// + _`find_collateral_adjustment_event` (the paired vToken-Burn +
    ///   aToken-Transfer matching). GHO repayment with aTokens is impossible
    ///   (asserted in caller).
    #[allow(clippy::too_many_arguments)]
    fn create_repay_with_atokens_operation(
        &self,
        operation_id: u32,
        repay_event: &'a Log,
        reserve: Address,
        user: Address,
        repay_amount: U256,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let principal_repay_event =
            self.find_principal_repay_event(repay_amount, false, scaled_events, assigned_indices)?;
        let collateral_adjustment_event = self.find_collateral_adjustment_event(
            user,
            reserve,
            repay_amount,
            scaled_events,
            assigned_indices,
        )?;
        assigned_log_idx_for_event(assigned_indices, principal_repay_event);
        assigned_log_idx_for_event(assigned_indices, collateral_adjustment_event);
        Ok(Operation {
            operation_id,
            operation_type: OperationType::RepayWithAtokens,
            pool_revision: self.pool_revision,
            pool_event: Some(repay_event),
            scaled_events: vec![
                clone_scaled_event_from_ref(principal_repay_event),
                clone_scaled_event_from_ref(collateral_adjustment_event),
            ],
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        })
    }

    /// Mirrors `_create_mint_to_treasury_operations`. The v8-vs-v9+ `ray_div`
    /// subtlety (DP3): for `pool_revision` <= 8, `amountMinted` is in underlying
    /// units → apply `ray_div(amountMinted, liquidity_index, HALF_UP)` to
    /// derive the scaled amount; for `pool_revision` >= 9, `amountMinted`
    /// equals the scaled amount directly.
    #[allow(clippy::too_many_lines)] // mirror's body intrinsic — phased loop
    fn create_mint_to_treasury_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
        minted_to_treasury_events: &[&'a Log],
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint {
                continue;
            }
            // Only CollateralMint where caller == Pool (mintToTreasury indicator).
            if ev.caller_address != Some(self.pool_address) {
                continue;
            }
            // Skip pure-interest accrual (amount == balance_increase).
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            if ev.amount == bal_inc {
                // Pure-accrual Mint — not a MintToTreasury.
                assigned_indices.insert(ev.log_index);
                continue;
            }

            // Determine the underlying asset (the Mint event's aToken → asset row).
            let asset_row = DegenbotDb::lookup_asset_by_token_address_on_conn(
                self.conn,
                self.market_id,
                &addr_to_hex(ev.token_address),
                "a_token",
            )
            .ok()
            .flatten();
            let underlying_addr = asset_row
                .as_ref()
                .and_then(|a| parse_address(&a.underlying_token_address))
                .unwrap_or(Address::ZERO);

            // Extract the amountMinted from the MintedToTreasury log matching
            // this underlying asset.
            let mut minted_amount: Option<U256> = None;
            for mt_ev in minted_to_treasury_events {
                if let Some(DecodedAaveEvent::MintedToTreasury(m)) =
                    aave_event_decoder::decode_aave_log(mt_ev)
                {
                    if m.reserve == underlying_addr {
                        minted_amount = Some(m.amount_minted);
                        break;
                    }
                }
            }

            // DP3: for pool_revision <= 8, ray_div the amountMinted by the
            // event's index (the aToken's liquidity_index at emit-time).
            // For pool_revision >= 9, the amountMinted is already in scaled units.
            let resolved = if let Some(raw_amount) = minted_amount {
                if self.pool_revision < SCALED_AMOUNT_POOL_REVISION {
                    // Use the Mint event's index (the LiquidityIndex at emit-time).
                    let idx = ev.index.unwrap_or(U256::ZERO);
                    if idx.is_zero() {
                        Some(raw_amount)
                    } else {
                        degenbot_evm_math::wad_ray_math::ray_div(
                            raw_amount,
                            idx,
                            Some(RayRounding::HalfUp),
                        )
                        .ok()
                    }
                } else {
                    Some(raw_amount)
                }
            } else {
                None
            };

            assigned_indices.insert(ev.log_index);
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: OperationType::MintToTreasury,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events: Vec::new(),
                balance_transfer_events: Vec::new(),
                minted_to_treasury_amount: resolved,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
        }
        operations
    }

    /// Mirrors `_create_deficit_operation`. `DEFICIT_CREATED` → Unknown
    /// (placeholder awaiting downstream liquidation matching). The Python
    /// deliberately uses UNKNOWN so `DEFICIT_CREATED` doesn't interfere with
    /// liquidation processing downstream.
    fn create_deficit_operation(&self, operation_id: u32, deficit_event: &'a Log) -> Operation<'a> {
        Operation {
            operation_id,
            operation_type: OperationType::Unknown,
            pool_revision: self.pool_revision,
            pool_event: Some(deficit_event),
            scaled_events: Vec::new(),
            transfer_events: Vec::new(),
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        }
    }

    /// Mirrors `_create_deficit_coverage_operations`. Deficit-coverage
    /// `BalanceTransfer` + Burn pair (phase 4c). ERC20-Transfer +
    /// `BalanceTransfer` + Burn triplet matching (the §4.2-drift knife-edge
    /// per DP6 — kept in A; escalate to B if gnarly).
    #[allow(clippy::too_many_lines)] // phased loop body intrinsic
    fn create_deficit_coverage_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();

        // Find all unassigned BalanceTransfers (aToken-side, including ERC20).
        let balance_transfers: Vec<&ScaledTokenEvent<'a>> = scaled_events
            .iter()
            .filter(|ev| {
                (ev.event_type == ScaledTokenEventType::CollateralTransfer
                    || ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer)
                    && !assigned_indices.contains(&ev.log_index)
                    && !local_assigned.contains(&ev.log_index)
            })
            .collect();

        for bt_ev in balance_transfers {
            // For each BalanceTransfer, look for a paired CollateralBurn.
            let bt_target = bt_ev.target_address.unwrap_or(Address::ZERO);
            let paired_burn: Option<&ScaledTokenEvent<'a>> = scaled_events.iter().find(|burn_ev| {
                if assigned_indices.contains(&burn_ev.log_index)
                    || local_assigned.contains(&burn_ev.log_index)
                {
                    return false;
                }
                if burn_ev.event_type != ScaledTokenEventType::CollateralBurn {
                    return false;
                }
                if burn_ev.user_address != bt_target {
                    return false;
                }
                burn_ev.token_address == bt_ev.token_address
            });

            // DeficitCoverage: the bt_ev is required to be ERC20_COLLATERAL_TRANSFER
            // for the triplet middle-insertion. If paired_burn is Some, build the op.
            let paired = if let Some(burn) = paired_burn {
                let mut paired_events: Vec<ScaledTokenEvent<'a>> =
                    vec![clone_scaled_event(bt_ev), clone_scaled_event(burn)];
                // The Python requires bt_ev.event_type == ERC20_COLLATERAL_TRANSFER
                // here; an additional look for a matching BalanceTransfer (with index
                // field) for the same transfer — inserted between transfer + burn.
                let mut bt_events: Vec<&'a Log> = Vec::new();
                if bt_ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer {
                    for other_ev in scaled_events {
                        if assigned_indices.contains(&other_ev.log_index)
                            || local_assigned.contains(&other_ev.log_index)
                        {
                            continue;
                        }
                        if other_ev.event_type != ScaledTokenEventType::CollateralTransfer {
                            continue;
                        }
                        if other_ev.from_address != bt_ev.from_address {
                            continue;
                        }
                        paired_events.insert(1, clone_scaled_event(other_ev));
                        bt_events.push(other_ev.log);
                        local_assigned.insert(other_ev.log_index);
                        break;
                    }
                }
                let op_id = *next_op_id;
                *next_op_id += 1;
                assigned_indices.insert(bt_ev.log_index);
                assigned_indices.insert(burn.log_index);
                operations.push(Operation {
                    operation_id: op_id,
                    operation_type: OperationType::DeficitCoverage,
                    pool_revision: self.pool_revision,
                    pool_event: None,
                    scaled_events: paired_events,
                    transfer_events: Vec::new(),
                    balance_transfer_events: bt_events,
                    minted_to_treasury_amount: None,
                    debt_to_cover: None,
                    validation_errors: Vec::new(),
                });
                true
            } else {
                false
            };
            // Only mark the bt_ev assigned when it was actually paired into
            // a DeficitCoverage op. Unpaired transfers (no paired burn — the
            // normal aToken-transfer case, NOT an Umbrella DeficitCoverage)
            // must fall through to create_transfer_operations as standalone
            // BalanceTransfer ops (YUPSIB: previously the unconditional
            // `local_assigned.insert` stole them → the recipient's credit
            // never landed → their Withdraw's Burn went negative).
            //
            // NB: the paired bt_ev is already directly inserted into
            // `assigned_indices` above (so the extend below flushing the
            // inner-loop BalanceTransfer companions suffices).
            if paired {
                local_assigned.insert(bt_ev.log_index);
            }
        }

        // Merge local_assigned back into assigned_indices (mirrors Python's
        // `assigned_indices.update(local_assigned)`).
        assigned_indices.extend(local_assigned.iter().copied());
        operations
    }

    /// Mirrors `_create_interest_accrual_operations`. Unassigned Mint events
    /// (with `amount == balance_increase` or small balanceIncrease) → pure
    /// interest accrual operations. Includes dust mints from discounts.
    #[allow(clippy::too_many_lines)] // phased loop + paired Transfer check
    fn create_interest_accrual_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) || local_assigned.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralMint
                && ev.event_type != ScaledTokenEventType::DebtMint
                && ev.event_type != ScaledTokenEventType::GhoDebtMint
            {
                continue;
            }
            // Find matching Transfer event (from ZERO_ADDRESS to this user).
            let mut transfer_events: Vec<&'a Log> = Vec::new();
            for transfer_ev in scaled_events {
                let from_zero = transfer_ev.from_address == Some(Address::ZERO);
                let Some(target) = transfer_ev.target_address else {
                    continue;
                };
                if transfer_ev.event_type != ScaledTokenEventType::CollateralTransfer
                    && transfer_ev.event_type != ScaledTokenEventType::Erc20CollateralTransfer
                {
                    continue;
                }
                if !from_zero || target != ev.user_address {
                    continue;
                }
                if transfer_ev.token_address != ev.token_address {
                    continue;
                }
                if assigned_indices.contains(&transfer_ev.log_index)
                    || local_assigned.contains(&transfer_ev.log_index)
                {
                    continue;
                }
                if transfer_ev.amount <= ev.amount {
                    transfer_events.push(transfer_ev.log);
                    local_assigned.insert(transfer_ev.log_index);
                    break;
                }
            }
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: OperationType::InterestAccrual,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events,
                balance_transfer_events: Vec::new(),
                minted_to_treasury_amount: None,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
            local_assigned.insert(ev.log_index);
        }
        operations
    }

    /// Mirrors `_create_transfer_operations` + `_process_erc20_transfers`.
    /// Unassigned transfer events → standalone `BALANCE_TRANSFER` (or
    /// `STKAAVE_TRANSFER`) operations. Pairs ERC20 Transfer + `BalanceTransfer`
    /// for the same movement.
    #[allow(clippy::too_many_lines)] // phased loop + pairing
    fn create_transfer_operations(
        &self,
        scaled_events: &[ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        next_op_id: &mut u32,
    ) -> Vec<Operation<'a>> {
        let mut operations: Vec<Operation<'a>> = Vec::new();
        let mut local_assigned: HashSet<u64> = HashSet::new();
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) || local_assigned.contains(&ev.log_index) {
                continue;
            }
            let is_transfer = matches!(
                ev.event_type,
                ScaledTokenEventType::CollateralTransfer
                    | ScaledTokenEventType::DebtTransfer
                    | ScaledTokenEventType::DiscountTransfer
                    | ScaledTokenEventType::Erc20CollateralTransfer
                    | ScaledTokenEventType::Erc20DebtTransfer
                    | ScaledTokenEventType::GhoDebtTransfer
            );
            if !is_transfer {
                continue;
            }
            assert!(ev.index.is_none(), "Transfer events have no index");
            // Skip transfers to/from zero that are part of mints/burns.
            if ev.target_address == Some(Address::ZERO)
                && is_part_of_burn(ev, scaled_events, &mut local_assigned)
            {
                continue;
            }
            if ev.from_address == Some(Address::ZERO)
                && is_part_of_mint(ev, scaled_events, &mut local_assigned)
            {
                continue;
            }
            // Find matching BalanceTransfer for the same movement.
            let mut balance_transfer_events: Vec<&'a Log> = Vec::new();
            for bt_ev in scaled_events {
                if assigned_indices.contains(&bt_ev.log_index)
                    || local_assigned.contains(&bt_ev.log_index)
                {
                    continue;
                }
                if bt_ev.index.is_none() {
                    continue;
                }
                if bt_ev.from_address != ev.from_address {
                    continue;
                }
                if bt_ev.target_address != ev.target_address {
                    continue;
                }
                if bt_ev.token_address != ev.token_address {
                    continue;
                }
                if !Self::are_compatible_transfer_types(ev.event_type, bt_ev.event_type) {
                    continue;
                }
                local_assigned.insert(bt_ev.log_index);
                balance_transfer_events.push(bt_ev.log);
                break;
            }
            let op_type = if ev.event_type == ScaledTokenEventType::DiscountTransfer {
                OperationType::StkAaveTransfer
            } else {
                OperationType::BalanceTransfer
            };
            let op_id = *next_op_id;
            *next_op_id += 1;
            operations.push(Operation {
                operation_id: op_id,
                operation_type: op_type,
                pool_revision: self.pool_revision,
                pool_event: None,
                scaled_events: vec![clone_scaled_event(ev)],
                transfer_events: Vec::new(),
                balance_transfer_events,
                minted_to_treasury_amount: None,
                debt_to_cover: None,
                validation_errors: Vec::new(),
            });
        }
        assigned_indices.extend(local_assigned.iter().copied());
        operations
    }

    // ── the `_find_*` helpers (mirror the Python) ──────────────────────

    /// Mirrors `_find_principal_repay_event`. For REPAY: match either Burn
    /// (amount + `balance_increase` == `repay_amount`) or Mint (`balance_increase`
    /// - amount == `repay_amount` — interest > repayment path).
    #[allow(clippy::unused_self)] // keep &self cadence — the future B-version (layer helpers) will need it
    fn find_principal_repay_event<'b>(
        &self,
        repay_amount: U256,
        is_gho: bool,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Result<&'b ScaledTokenEvent<'a>, ParseError>
    where
        'a: 'b,
    {
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            let valid_types = if is_gho {
                (
                    ScaledTokenEventType::GhoDebtBurn,
                    ScaledTokenEventType::GhoDebtMint,
                )
            } else {
                (
                    ScaledTokenEventType::DebtBurn,
                    ScaledTokenEventType::DebtMint,
                )
            };
            if ev.event_type != valid_types.0 && ev.event_type != valid_types.1 {
                continue;
            }
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            let calculated = if ev.event_type == valid_types.0
                || ev.event_type == ScaledTokenEventType::GhoDebtBurn
            {
                // Burn: amount + balance_increase.
                ev.amount + bal_inc
            } else {
                // Mint: balance_increase - amount.
                bal_inc - ev.amount
            };
            if !Self::amounts_match(calculated, repay_amount, self.pool_revision) {
                continue;
            }
            return Ok(ev);
        }
        Err(ParseError::NoMatch(
            "no matching principal repay event".into(),
        ))
    }

    /// Mirrors `_find_collateral_adjustment_event` (`REPAY_WITH_ATOKENS` paired
    /// vToken-Burn + aToken-Transfer matching). Both Burn + Mint branches
    /// (the interest-exceeds-repayment edge).
    #[allow(clippy::too_many_arguments)]
    fn find_collateral_adjustment_event<'b>(
        &self,
        user: Address,
        reserve: Address,
        expected_amount: U256,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Result<&'b ScaledTokenEvent<'a>, ParseError>
    where
        'a: 'b,
    {
        let expected_a_token = self.get_a_token_for_asset(reserve)?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "no aToken for reserve {reserve} in market {}",
                self.market_id
            ))
        })?;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::CollateralBurn
                && ev.event_type != ScaledTokenEventType::CollateralMint
            {
                continue;
            }
            if ev.user_address != user {
                continue;
            }
            let _ = expected_a_token; // Boundary: aToken-address match could be
                                      // added if multiple collateral tokens exist for a single user;
                                      // matching by `user` + amount is the Python's primary match
                                      // (it also doesn't filter by token contract explicitly here).
            let bal_inc = ev.balance_increase.unwrap_or(U256::ZERO);
            let adjustment = if ev.event_type == ScaledTokenEventType::CollateralMint {
                bal_inc - ev.amount
            } else {
                ev.amount + bal_inc
            };
            if !Self::amounts_match(adjustment, expected_amount, self.pool_revision) {
                continue;
            }
            return Ok(ev);
        }
        Err(ParseError::NoMatch(
            "no matching collateral adjustment event".into(),
        ))
    }

    /// Mirrors `_find_debt_transfer_to_zero`. Find debt transfer event to
    /// zero address matching the principal burn amount. Returns 0 or 1 events.
    #[allow(clippy::unused_self)] // mirror's symmetric API to siblings
    fn find_debt_transfer_to_zero<'b>(
        &self,
        user: Address,
        amount: U256,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &HashSet<u64>,
    ) -> Vec<&'a Log>
    where
        'a: 'b,
    {
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            if ev.event_type != ScaledTokenEventType::DebtTransfer
                && ev.event_type != ScaledTokenEventType::Erc20DebtTransfer
                && ev.event_type != ScaledTokenEventType::GhoDebtTransfer
            {
                continue;
            }
            if ev.from_address != Some(user) {
                continue;
            }
            if ev.amount != amount {
                continue;
            }
            return vec![ev.log];
        }
        Vec::new()
    }

    /// Mirrors Python `_get_a_token_for_asset`. Resolves the aToken address
    /// for an underlying asset (None if not a market asset). Uses the
    /// [`DegenbotDb::lookup_asset_by_underlying_address_on_conn`] substrate
    /// (the §3 surface — no ad-hoc SQL JOINs in the parser).
    fn get_a_token_for_asset(&self, underlying: Address) -> Result<Option<Address>, ParseError> {
        let row = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
            self.conn,
            self.market_id,
            &addr_to_hex(underlying),
        )?;
        Ok(row.and_then(|a| parse_address(&a.a_token_address)))
    }

    /// Mirrors Python `_get_v_token_for_asset`. Resolves the vToken address
    /// for an underlying debt asset (None if not a market asset). Required by
    /// `_create_liquidation_operation`'s `SEPARATE_BURNS` pattern detection.
    fn get_v_token_for_asset(&self, underlying: Address) -> Result<Option<Address>, ParseError> {
        let row = DegenbotDb::lookup_asset_by_underlying_address_on_conn(
            self.conn,
            self.market_id,
            &addr_to_hex(underlying),
        )?;
        Ok(row.and_then(|a| parse_address(&a.v_token_address)))
    }

    // ── the liquidation engine (HQF5NQ-B) ────────────────────────────────

    // ── the liquidation engine fns (HQF5NQ-B) ──────────────────────────

    /// Mirrors Python `_analyze_liquidation_scenarios` — pre-analyze
    /// liquidations to detect multi-liquidation scenarios. Returns a mapping
    /// of `(user, debt_v_token_address)` → `liquidation_count`. This allows
    /// proper disambiguation when the same user is liquidated multiple times
    /// with the same debt asset in one transaction.
    ///
    /// §4.2-parity note: the Python reads `decode_address(ev["topics"][3])`
    /// (user) + `decode_address(ev["topics"][2])` (`debt_asset`) per
    /// `LiquidationCall` event → resolves `debt_asset` → vToken via
    /// `_get_v_token_for_asset`; we mirror the same per-event vToken resolution.
    #[allow(clippy::missing_panics_doc)] // assert on missing vToken substrate
    fn analyze_liquidation_scenarios(
        &self,
        all_events: &[&Log],
    ) -> Result<HashMap<(Address, Address), usize>, ParseError> {
        let mut counts: HashMap<(Address, Address), usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            let debt_asset = Address::from_word(topics[2]);
            let v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
                ParseError::Substrate(format!(
                    "no vToken for debt_asset {debt_asset} in market {}",
                    self.market_id
                ))
            })?;
            *counts.entry((user, v_token_address)).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// Mirrors Python `_analyze_user_liquidation_count` — count total
    /// liquidations per user (not per user+asset pair). Returns `user` →
    /// `total_liquidations`.
    ///
    /// When a user has exactly 1 liquidation, ALL debt burns for that user
    /// belong to that single liquidation. This handles bad-debt liquidations
    /// where the protocol burns multiple debt positions via `_burnBadDebt()`.
    /// When a user has multiple liquidations, use asset-specific matching to
    /// disambiguate which burns belong to which liquidation.
    fn analyze_user_liquidation_count(all_events: &[&Log]) -> HashMap<Address, usize> {
        let mut counts: HashMap<Address, usize> = HashMap::new();
        for ev in all_events {
            let topics = ev.topics();
            if topics.first() != Some(&aave_event_decoder::LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if topics.len() < 4 {
                continue;
            }
            let user = Address::from_word(topics[3]);
            *counts.entry(user).or_insert(0) += 1;
        }
        counts
    }

    /// Mirrors Python `_collect_debt_burns` — the §4.2-critical
    /// `SINGLE/COMBINED_BURN/SEPARATE_BURNS` pattern-detection debt-burn
    /// collector.
    ///
    /// Collection strategy (port-EXACT):
    /// - Single liquidation per user: collect ALL debt burns (no asset filter)
    ///   → handles bad-debt liquidations where `_burnBadDebt()` burns all debt positions.
    /// - Multiple liquidations per user: use asset filter + sequential matching
    ///   to disambiguate which burns belong to which liquidation.
    ///   - **`SEPARATE_BURNS`** (`liquidation_count_for_asset == total_burn_count`):
    ///     burn `i` belongs to liquidation `i`; return the burn at
    ///     `liquidation_position`.
    ///   - **`COMBINED_BURN`** (`liquidation_count_for_asset > total_burn_count` +
    ///     `liquidation_position == 0`): all burns go to the first liquidation.
    ///
    /// # §4.2-parity note (the `assigned_indices.add(ev.index)` quirk)
    ///
    /// The Python also calls `assigned_indices.add(ev.index)` to store the
    /// Burn event's `index` field (a ray-scale u256 liquidity-index value) in
    /// `assigned_indices`. This storage is **dead-code** — every
    /// `in assigned_indices` query is `ev.event["logIndex"] in assigned_indices`
    /// (small-int logIndex), never `ev.index`. The `add(ev.index)` stores
    /// ray-scale values that are never matched; we omit the assignment (a u64
    /// set can't hold ray values anyway) but preserve the assertions
    /// `assert ev.index is not None; assert ev.index > 0` (the runtime panic on
    /// ill-formed events) as `debug_assert!`. Output parity verified: the
    /// `assigned_indices` state never affects the parsed Operation output.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // mirror's intrinsic shape
    fn collect_debt_burns<'b>(
        user: Address,
        debt_v_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
        liquidation_analysis: &HashMap<(Address, Address), usize>,
        user_liquidation_count: usize,
        liquidation_position: usize,
    ) -> Vec<&'b ScaledTokenEvent<'a>>
    where
        'a: 'b,
    {
        let mut burns: Vec<&ScaledTokenEvent<'a>> = Vec::new();
        if user_liquidation_count == 1 {
            // Single liquidation per user: collect ALL debt burns (no asset filter).
            let mut candidate_burns: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    !assigned_indices.contains(&ev.log_index)
                        && ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                })
                .collect();
            candidate_burns.sort_by_key(|e| e.log_index);
            for ev in candidate_burns {
                burns.push(ev);
                assigned_indices.insert(ev.log_index);
                let idx = ev.index.unwrap_or(U256::ZERO);
                debug_assert!(
                    !idx.is_zero(),
                    "DebtBurn index must be > 0 (parity: assert ev.index > 0)"
                );
                // NB: the Python's `assigned_indices.add(ev.index)` here is
                // dead-code (no later `in assigned_indices` query hits the
                // ray-scale `index` field); omitted (u64 set can't hold it).
            }
        } else {
            let debt_v =
                debt_v_token_address.expect("multi-liquidation requires debt_v_token_address");
            let liquidation_count_for_asset = liquidation_analysis
                .get(&(user, debt_v))
                .copied()
                .unwrap_or(1);
            let is_multi_liquidation = liquidation_count_for_asset > 1;
            // Get ALL burns for this (user, debt_asset) to determine pattern.
            // Don't filter by assigned_indices yet — we need total count for
            // pattern detection (parity: Python lines 1580-1586).
            let mut all_burns_for_asset: Vec<&ScaledTokenEvent<'a>> = scaled_events
                .iter()
                .filter(|ev| {
                    ev.user_address == user
                        && (ev.event_type == ScaledTokenEventType::DebtBurn
                            || ev.event_type == ScaledTokenEventType::GhoDebtBurn)
                        && ev.token_address == debt_v
                })
                .collect();
            all_burns_for_asset.sort_by_key(|e| e.log_index);
            // Now get only unassigned burns for assignment.
            let candidate_burns: Vec<&ScaledTokenEvent<'a>> = all_burns_for_asset
                .iter()
                .copied()
                .filter(|ev| !assigned_indices.contains(&ev.log_index))
                .collect();
            // Determine pattern: COMBINED_BURN vs SEPARATE_BURNS.
            let total_burn_count = all_burns_for_asset.len();
            if is_multi_liquidation && !candidate_burns.is_empty() {
                debug_assert!(
                    liquidation_count_for_asset >= total_burn_count,
                    "parity: assert liquidation_count_for_asset >= total_burn_count"
                );
                if liquidation_count_for_asset == total_burn_count {
                    // SEPARATE_BURNS pattern: each liquidation gets exactly one burn.
                    debug_assert!(
                        liquidation_position < total_burn_count,
                        "parity: assert liquidation_position < total_burn_count"
                    );
                    let target_burn = all_burns_for_asset[liquidation_position];
                    debug_assert!(
                        !assigned_indices.contains(&target_burn.log_index),
                        "SEPARATE_BURNS target burn already assigned"
                    );
                    burns.push(target_burn);
                    assigned_indices.insert(target_burn.log_index);
                    let idx = target_burn.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                } else if liquidation_position == 0 {
                    // COMBINED_BURN pattern: more liquidations than burns.
                    // All burns go to the first liquidation.
                    for ev in candidate_burns {
                        burns.push(ev);
                        assigned_indices.insert(ev.log_index);
                        let idx = ev.index.unwrap_or(U256::ZERO);
                        debug_assert!(!idx.is_zero());
                    }
                }
            } else {
                // Single liquidation or no burns: collect all available burns.
                for ev in candidate_burns {
                    burns.push(ev);
                    assigned_indices.insert(ev.log_index);
                    let idx = ev.index.unwrap_or(U256::ZERO);
                    debug_assert!(!idx.is_zero());
                }
            }
        }
        burns
    }

    /// Mirrors Python `_collect_collateral_events` — collect collateral
    /// events (burns and transfers) for the liquidation. During liquidations
    /// a borrower may have BOTH collateral burned AND multiple transfers.
    /// Collateral may be burned OR transferred to treasury (`BalanceTransfer`).
    ///
    /// Returns `(collateral_burn, collateral_transfers)`.
    ///
    /// # EIWEPM bug class — the burn-side / mint-side pair ERC20 Transfer
    ///
    /// By Aave V3 protocol, the aToken emits the `Burn` event AND the paired
    /// ERC20 `Transfer user→0x0` (the burn-side companion) as ONE operational
    /// action (the same for `Mint` + `Transfer 0x0→user`). Pre-fix this fn
    /// pushed the burn-side ERC20 Transfer into `collateral_transfers`
    /// unfiltered (no `is_part_of_burn` / `is_part_of_mint` call — those filters
    /// lived only on the standalone Transfer path at line 1471/1476). The
    /// Liquidation op ended up with BOTH the `CollateralBurn` AND its paired
    /// ERC20 `Transfer user→0x0` in `op.scaled_events`. `dispatch_liquidation`
    /// then applied both with no `override_transfer_with_paired_bt` (the
    /// Liquidation path's intentional no-op for that override) → the user's
    /// collateral balance was debited TWICE by the Burn delta (the p1198-class
    /// 4.33× divergence — root-caused + math-verified on mainnet).
    ///
    /// Fix (per the orchestrator's directive): wire the already-ported
    /// `is_part_of_burn` (line 2522) + `is_part_of_mint` (line 2548) filters
    /// INTO this fn — match the standalone Transfer path's exact semantics
    /// (same `user_address` + `token_address` matching, no value heuristic).
    /// Mark the filtered events into `assigned_indices` so the standalone
    /// Step-4e `create_transfer_operations` skips them (it would re-apply the
    /// filter from `scaled_events` and skip — but pre-marking is the safest
    /// mirror of the standalone pattern's `local_assigned` write-back).
    fn collect_collateral_events<'b>(
        user: Address,
        collateral_a_token_address: Option<Address>,
        scaled_events: &'b [ScaledTokenEvent<'a>],
        assigned_indices: &mut HashSet<u64>,
    ) -> (
        Option<&'b ScaledTokenEvent<'a>>,
        Vec<&'b ScaledTokenEvent<'a>>,
    )
    where
        'a: 'b,
    {
        let mut collateral_transfers: Vec<&ScaledTokenEvent<'a>> = Vec::new();
        let mut collateral_burn: Option<&ScaledTokenEvent<'a>> = None;
        for ev in scaled_events {
            if assigned_indices.contains(&ev.log_index) {
                continue;
            }
            // Match collateral events only if they belong to this liquidation's
            // collateral asset (parity: prevents incorrect matching when a user
            // is liquidated multiple times with different collateral assets).
            if let Some(expected) = collateral_a_token_address {
                if ev.token_address != expected {
                    continue;
                }
            }
            if ev.event_type == ScaledTokenEventType::CollateralBurn && ev.user_address == user {
                collateral_burn = Some(ev);
            } else if (ev.event_type == ScaledTokenEventType::CollateralTransfer
                || ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer)
                && ev.user_address == user
            {
                // TYS5MS: mirror Python's `_should_skip_collateral_transfer`
                // (`transfers.py:25`) Liquidation-op filter — in Liquidation
                // ops, ALL ERC20 CollateralTransfers (`Erc20CollateralTransfer`
                // variant — index=None, the standard ERC20 Transfer event emitted
                // by the aToken) are SKIPPED. The user's collateral debit is
                // captured by the Burn event + the protocol-fee balance move by
                // the paired BalanceTransfer (`CollateralTransfer` variant —
                // index=non-None, preserved below). Without this guard the
                // `Erc20CollateralTransfer(user→treasury, fee)` emitted alongside
                // the LiquidationCall over-debits the user / over-credits the
                // treasury by exactly the `liquidationProtocolFee × bonus /
                // (1+bonus)` share — empirically verified on Rust-written
                // 16591070 prod DB for users 0x23dB (id 134) + 0x87A6 (id 1018)
                // WETH positions (residuals byte-match the on-chain ERC20
                // Transfer amounts; ratio = 0.476% = 0.10×0.05/1.05).
                //
                // Context confirmation (per orchestrator reminder (a)):
                // `collect_collateral_events` is LC-only — sole caller is
                // `create_liquidation_operation`; the unconditional skip CANNOT
                // over-filter non-LC ERC20 Transfers.
                //
                // Marked `assigned` so the standalone Step-4e Transfer path
                // (`create_transfer_operations`) doesn't re-collect it — same
                // write-back as the 1cf6578d `is_part_of_burn` / `is_part_of_mint`
                // guards below. The 1cf6578d ZERO-address guards below remain
                // ACTIVE for the `CollateralTransfer` (BalanceTransfer) variant
                // (target/from == ZERO is degenerate for BT events but defensive).
                if ev.event_type == ScaledTokenEventType::Erc20CollateralTransfer {
                    assigned_indices.insert(ev.log_index);
                    continue;
                }
                // EIWEPM: skip the burn-side pair ERC20 Transfer to ZERO (the
                // Burn event itself is the sole operational debit), and the
                // mint-side pair ERC20 Transfer from ZERO (the Mint event is
                // the sole operational credit). Mirrors the standalone Transfer
                // path's `is_part_of_burn` / `is_part_of_mint` filter exact
                // semantics (user-address + token-address match — no value
                // heuristic; the reference's matching contract).
                if ev.target_address == Some(Address::ZERO)
                    && is_part_of_burn(ev, scaled_events, assigned_indices)
                {
                    continue;
                }
                if ev.from_address == Some(Address::ZERO)
                    && is_part_of_mint(ev, scaled_events, assigned_indices)
                {
                    continue;
                }
                collateral_transfers.push(ev);
            }
        }
        (collateral_burn, collateral_transfers)
    }

    /// Mirrors Python `_create_liquidation_operation` — the gnarly ~160-LoC
    /// fn that builds an `Operation { operation_type: Liquidation, ... }` (or
    /// `GhoLiquidation` if the debt asset is GHO). Coordinates:
    /// - `analyze_liquidation_scenarios` + `analyze_user_liquidation_count` (the
    ///   pre-analysis dicts over `all_events`).
    /// - `collect_debt_burns` (the `SINGLE/COMBINED_BURN/SEPARATE_BURNS` detection).
    /// - `collect_collateral_events` (the `collateral_burn` vs `collateral_transfers` split).
    /// - the `debt_mint` net-debt-increase branch (accrued interest > repayment).
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Substrate`] if either the aToken or vToken sibling
    /// lookup fails (parity: `assert debt_v_token_address is not None`).
    #[allow(clippy::too_many_lines)] // mirror of Python's 5-nested-branch fn — intrinsic
    fn create_liquidation_operation(
        &self,
        operation_id: u32,
        liquidation_event: &'a Log,
        scaled_events: &[ScaledTokenEvent<'a>],
        all_events: &[&'a Log],
        assigned_indices: &mut HashSet<u64>,
    ) -> Result<Operation<'a>, ParseError> {
        let topics = liquidation_event.topics();
        let collateral_asset = Address::from_word(topics[1]);
        let debt_asset = Address::from_word(topics[2]);
        let user = Address::from_word(topics[3]);
        // Extract debtToCover from LiquidationCall event data (word 0).
        let data = liquidation_event.data().data.as_ref();
        let mut buf = [0u8; 32];
        if data.len() >= 32 {
            buf.copy_from_slice(&data[0..32]);
        }
        let debt_to_cover = U256::from_be_bytes::<32>(buf);
        let is_gho = self.gho_token_address == Some(debt_asset);
        let collateral_a_token_address = self.get_a_token_for_asset(collateral_asset)?;
        let debt_v_token_address = self.get_v_token_for_asset(debt_asset)?.ok_or_else(|| {
            ParseError::Substrate(format!(
                "no vToken for debt_asset {debt_asset} in market {}",
                self.market_id
            ))
        })?;
        // Pre-analyze liquidations to detect multi-liquidation scenarios.
        let liquidation_analysis = self.analyze_liquidation_scenarios(all_events)?;
        let user_liquidation_analysis = Self::analyze_user_liquidation_count(all_events);
        let user_liquidation_count = user_liquidation_analysis.get(&user).copied().unwrap_or(1);
        // Calculate this liquidation's position among all liquidations for
        // this (user, debt_asset) — sequential matching; burn[i] belongs to
        // liquidation[i]. Count LiquidationCall events before this one with
        // the same (user, debt_asset).
        let this_log_index = log_idx_value(liquidation_event);
        let mut liquidation_position = 0usize;
        for ev in all_events {
            if ev.topics().first() != Some(&aave_event_decoder::LIQUIDATION_CALL_TOPIC) {
                continue;
            }
            if ev.topics().len() < 4 {
                continue;
            }
            if Address::from_word(ev.topics()[3]) != user {
                continue;
            }
            if Address::from_word(ev.topics()[2]) != debt_asset {
                continue;
            }
            if log_idx_value(ev) < this_log_index {
                liquidation_position += 1;
            }
        }
        // Collect debt burns (SINGLE/COMBINED_BURN/SEPARATE_BURNS detection).
        let debt_burns = Self::collect_debt_burns(
            user,
            Some(debt_v_token_address),
            scaled_events,
            assigned_indices,
            &liquidation_analysis,
            user_liquidation_count,
            liquidation_position,
        );
        // Collect collateral events (burns + transfers).
        let (collateral_burn, collateral_transfers) = Self::collect_collateral_events(
            user,
            collateral_a_token_address,
            scaled_events,
            assigned_indices,
        );
        // A liquidation requires at least one collateral event.
        if collateral_burn.is_none() && collateral_transfers.is_empty() {
            return Err(ParseError::NoMatch(format!(
                "Expected at least 1 collateral event (burn or transfer) for liquidation. \
                 User: {user}, scaled_events log indices: {}",
                scaled_events
                    .iter()
                    .map(|e| e.log_index.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // Find debt mint events that represent net debt increase during
        // liquidation (when accrued interest > debt repayment:
        // balance_increase > amount).
        let mut debt_mint: Option<&ScaledTokenEvent<'a>> = None;
        for scaled_event in scaled_events {
            if assigned_indices.contains(&scaled_event.log_index) {
                continue;
            }
            if scaled_event.user_address != user {
                continue;
            }
            if is_gho && scaled_event.event_type != ScaledTokenEventType::GhoDebtMint {
                continue;
            }
            if !is_gho && scaled_event.event_type != ScaledTokenEventType::DebtMint {
                continue;
            }
            // Match debt mint events only if they belong to this liquidation's
            // debt asset (parity: assert event_token_address == debt_v_token_address).
            if scaled_event.token_address != debt_v_token_address {
                continue;
            }
            let bal_inc = scaled_event.balance_increase.unwrap_or(U256::ZERO);
            debug_assert!(
                bal_inc > scaled_event.amount,
                "parity: balance_increase > amount for net-debt-increase mint"
            );
            debt_mint = Some(scaled_event);
            break;
        }
        // Assemble scaled_token_events + balance_transfer_events.
        let mut scaled_token_events: Vec<ScaledTokenEvent<'a>> = Vec::new();
        let mut balance_transfer_events: Vec<&'a Log> = Vec::new();
        // Note: debt_burns may be empty for flash loan liquidations or when
        // interest > repayment; debt_mint is set when interest > repayment;
        // collateral_burn may be None when collateral is transferred to treasury.
        if let Some(burn) = collateral_burn {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        // Add all debt burns (primary and secondary).
        for burn in &debt_burns {
            scaled_token_events.push(clone_scaled_event(burn));
        }
        if let Some(mint) = debt_mint {
            scaled_token_events.push(clone_scaled_event(mint));
        }
        if !collateral_transfers.is_empty() {
            // Add all collateral transfers to scaled_token_events. Both
            // ERC20 Transfers (index=0) and BalanceTransfer events (index>0)
            // are collateral events that should be validated together.
            for transfer in &collateral_transfers {
                scaled_token_events.push(clone_scaled_event(transfer));
                // Track BalanceTransfer events separately so ERC20 Transfers
                // can use them for proper scaling during processing.
                let idx = transfer.index.unwrap_or(U256::ZERO);
                if !idx.is_zero() {
                    balance_transfer_events.push(transfer.log);
                }
            }
        }
        let op_type = if is_gho {
            OperationType::GhoLiquidation
        } else {
            OperationType::Liquidation
        };
        Ok(Operation {
            operation_id,
            operation_type: op_type,
            pool_revision: self.pool_revision,
            pool_event: Some(liquidation_event),
            scaled_events: scaled_token_events,
            transfer_events: Vec::new(),
            balance_transfer_events,
            minted_to_treasury_amount: None,
            debt_to_cover: Some(debt_to_cover),
            validation_errors: Vec::new(),
        })
    }

    // ── the validators (`_validate_operation` + per-op) ───────────────────

    /// Mirrors `_validate_operation` dispatch. Mutates `op.validation_errors`
    /// (the validators are token-fillers here; the Python's strict top-level
    /// `TransactionOperations.validate` pass is HQF5NQ-C).
    /// Dispatch-style match over `OperationType` — one arm per variant. Kept as
    /// a single fn (rather than per-op validator helpers) because each arm is
    /// 2–8 lines of predicate checks; splitting would fragment the validation
    /// logic across ~10 helpers without improving clarity (the Python uses a
    /// validators dict, not per-op fns here).
    #[allow(clippy::too_many_lines)]
    fn validate_operation(op: &mut Operation<'_>) {
        // The API dispatch mirrors the Python validators dictionary; the
        // actual validation is per-op (`_validate_*` in Python). For A's
        // parse() integrity, the validators are minimal — they fill
        // `validation_errors` with descriptive messages on miss-pattern.
        // The Python's strict top-level pass is HQF5NQ-C's `validate()`.
        match op.operation_type {
            OperationType::Supply => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("SUPPLY expects 1 collateral event".into());
                }
            }
            OperationType::Withdraw => {
                let burns: usize = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral() && e.event_type.is_burn())
                    .count();
                if burns > 1 {
                    op.validation_errors
                        .push(format!("WITHDRAW expects ≤1 collateral burn, got {burns}"));
                }
            }
            OperationType::Borrow | OperationType::GhoBorrow => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("BORROW expects 1 debt event".into());
                }
            }
            OperationType::Repay | OperationType::GhoRepay => {
                if op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count()
                    != 1
                {
                    op.validation_errors
                        .push("REPAY expects 1 debt event".into());
                }
            }
            OperationType::RepayWithAtokens => {
                let debt = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_debt())
                    .count();
                if debt != 1 {
                    op.validation_errors.push(format!(
                        "REPAY_WITH_ATOKENS expects 1 debt event, got {debt}"
                    ));
                }
                let burns = op
                    .scaled_events
                    .iter()
                    .filter(|e| e.event_type.is_collateral() && e.event_type.is_burn())
                    .count();
                if burns > 1 {
                    op.validation_errors.push(format!(
                        "REPAY_WITH_ATOKENS expects ≤1 collateral burn, got {burns}"
                    ));
                }
            }
            OperationType::InterestAccrual => {
                if op.scaled_events.len() != 1 {
                    op.validation_errors
                        .push("INTEREST_ACCRUAL expects 1 scaled event".into());
                }
            }
            OperationType::BalanceTransfer => {
                if op.scaled_events.len() != 1 {
                    op.validation_errors
                        .push("BALANCE_TRANSFER expects 1 scaled event".into());
                }
            }
            OperationType::DeficitCoverage => {
                if op.scaled_events.len() < 2 {
                    op.validation_errors
                        .push("DEFICIT_COVERAGE expects ≥2 (transfer + burn)".into());
                }
            }
            OperationType::MintToTreasury => {
                if op.scaled_events.len() != 1
                    || !matches!(
                        op.scaled_events[0].event_type,
                        ScaledTokenEventType::CollateralMint
                    )
                {
                    op.validation_errors
                        .push("MINT_TO_TREASURY expects 1 CollateralMint".into());
                }
            }
            OperationType::StkAaveTransfer
                if (op.scaled_events.len() != 1
                    || op.scaled_events[0].event_type
                        != ScaledTokenEventType::DiscountTransfer) =>
            {
                op.validation_errors
                    .push("STKAAVE_TRANSFER expects 1 DiscountTransfer".into());
            }
            // Liquidation / GhoLiquidation / GhoFlashLoan / Unknown — minimal
            // validation in A (B owns the LiquidationCall builder + its
            // validators).
            _ => {}
        }
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

/// Extract the pool events from a `&[&Log]` slice (mirror of
/// `_extract_pool_events`). Sorted by logIndex.
fn extract_pool_events<'a>(events: &[&'a Log]) -> Vec<&'a Log> {
    let mut pool: Vec<&Log> = events
        .iter()
        .copied()
        .filter(|l| is_pool_event_log(l))
        .collect();
    pool.sort_by_key(|l| log_idx_value(l));
    pool
}

/// `true` if the log is one of the 6 pool-event anchors (Supply/Withdraw/
/// Borrow/Repay/LiquidationCall/DeficitCreated).
fn is_pool_event_log(log: &Log) -> bool {
    let Some(t) = log.topics().first() else {
        return false;
    };
    [
        aave_event_decoder::SUPPLY_TOPIC,
        aave_event_decoder::WITHDRAW_TOPIC,
        aave_event_decoder::BORROW_TOPIC,
        aave_event_decoder::REPAY_TOPIC,
        aave_event_decoder::LIQUIDATION_CALL_TOPIC,
        aave_event_decoder::DEFICIT_CREATED_TOPIC,
    ]
    .contains(t)
}

/// `true` if the log is a `MintedToTreasury` event.
fn is_minted_to_treasury_log(log: &Log) -> bool {
    log.topics().first() == Some(&aave_event_decoder::MINTED_TO_TREASURY_TOPIC)
}

/// `true` if the log is a plain ERC20 `Transfer` event.
fn is_erc20_transfer_log(log: &Log) -> bool {
    log.topics().first() == Some(&aave_event_decoder::ERC20_TRANSFER_TOPIC)
}

/// Read the `logIndex` of a `Log` as a `u64` (`0` if absent — every RPC-fetched
/// receipt has one).
pub(crate) fn log_idx_value(log: &Log) -> u64 {
    log.log_index.map_or(0, |i| i)
}

/// Convert an alloy `Address` to the EIP-55 checksummed hex string the
/// `erc20_tokens.address` / `aave_v3_users.address` VARCHAR(42) columns
/// store — mirrors the Python's `get_checksum_address` + the config path's
/// `checksum()` (`config_dispatch.rs`). MUST be consistent across both
/// paths: the `SQLite` columns use BINARY collation (case-sensitive), so a
/// lowercase lookup against a checksummed-seeded row would miss + create a
/// duplicate (the RCDJPH §4.2 byte-divergence — duplicate lowercase users).
pub(crate) fn addr_to_hex(addr: Address) -> String {
    address_to_checksum_string(&addr)
}

/// Parse a hex string (with or without `0x` prefix) to an alloy `Address`.
/// Returns `None` on malformed input.
pub(crate) fn parse_address(s: &str) -> Option<Address> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.len() != 40 {
        return None;
    }
    Address::parse_checksummed(s, None).ok().or_else(|| {
        // Fall back to case-insensitive parse (the DB-stored address may not
        // be EIP-55 checksum-encoded — the Python stores lowercase, so the
        // `parse_checksummed` strict check would fail; alloy's
        // `Address::from_str` accepts the lowercase form).
        let bytes = alloy::hex::decode(trimmed).ok()?;
        if bytes.len() != 20 {
            return None;
        }
        let arr: [u8; 20] = bytes.try_into().ok()?;
        Some(Address::from(arr))
    })
}

/// Mark an `event`'s `log_index` assigned (helper used by the supply/withdraw/
/// borrow/repay builders).
fn assigned_log_idx_for_event(assigned: &mut HashSet<u64>, ev: &ScaledTokenEvent<'_>) {
    assigned.insert(ev.log_index);
}

/// Clone a `ScaledTokenEvent` reference into an owned `ScaledTokenEvent` (the
/// `Operation.scaled_events` vec owns by-value; the source lifetime still
/// ties to the input `&[&Log]` slice — the `log: &'a Log` field is
/// preserved across the clone).
fn clone_scaled_event<'a>(ev: &ScaledTokenEvent<'a>) -> ScaledTokenEvent<'a> {
    ScaledTokenEvent {
        log: ev.log,
        decoded: ev.decoded.clone(),
        event_type: ev.event_type,
        token_address: ev.token_address,
        user_address: ev.user_address,
        caller_address: ev.caller_address,
        from_address: ev.from_address,
        target_address: ev.target_address,
        amount: ev.amount,
        balance_increase: ev.balance_increase,
        index: ev.index,
        log_index: ev.log_index,
    }
}

/// Same as `clone_scaled_event`, taking a reference (used to alias-borrow).
fn clone_scaled_event_from_ref<'a>(ev: &ScaledTokenEvent<'a>) -> ScaledTokenEvent<'a> {
    clone_scaled_event(ev)
}

/// The amount-match tolerance (mirror of `operations.py:: TOKEN_AMOUNT_MATCH_TOLERANCE`
/// applied to a `u64`-magnitude comparison).
#[must_use]
pub fn amounts_match_with_tolerance(calculated: U256, expected: U256, tolerance: u64) -> bool {
    if calculated == expected {
        return true;
    }
    let tol = U256::from(tolerance);
    if calculated > expected {
        calculated - expected <= tol
    } else {
        expected - calculated <= tol
    }
}

/// Bare-bytes comparison helper — repeated in a handful of test expressions;
/// kept here for parity (the Python field-tests on a similar helper).
#[allow(dead_code)]
fn _amounts_match_eq(a: U256, b: U256) -> bool {
    a == b
}

// ── pool-event decode helpers ─────────────────────────────────────────────
//
// Tiny structure-only decoders for Pool-event fields the parser-matching needs
// (user + reserve address + amount). These bypass ECFB5C's full decode (which
// returns many fields the parser doesn't use) — direct topic/data slicing for
// the literal fields every builder needs.

struct SupplyPoolDecoded {
    reserve: Address,
    on_behalf_of: Address,
    amount: U256,
}
struct WithdrawPoolDecoded {
    reserve: Address,
    user: Address,
    amount: U256,
}
struct BorrowPoolDecoded {
    reserve: Address,
    on_behalf_of: Address,
    amount: U256,
}
struct RepayPoolDecoded {
    reserve: Address,
    user: Address,
    amount: U256,
    use_a_tokens: bool,
}

fn decode_supply_pool_event(log: &Log) -> Option<SupplyPoolDecoded> {
    // Real Aave V3 Supply: 4 topics [sig, reserve, onBehalfOf, referralCode].
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let on_behalf_of = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    // data = abi.encode(address user, uint256 amount) — amount is word 1.
    if data.len() < 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[32..64]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(SupplyPoolDecoded {
        reserve,
        on_behalf_of,
        amount,
    })
}

fn decode_withdraw_pool_event(log: &Log) -> Option<WithdrawPoolDecoded> {
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let user = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(WithdrawPoolDecoded {
        reserve,
        user,
        amount,
    })
}

fn decode_borrow_pool_event(log: &Log) -> Option<BorrowPoolDecoded> {
    // Real Aave V3 Borrow: 4 topics [sig, reserve, onBehalfOf, referralCode].
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let on_behalf_of = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    // data = abi.encode(address user, uint256 amount, uint8 mode, uint256 rate)
    if data.len() < 128 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[32..64]);
    let amount = U256::from_be_bytes::<32>(buf);
    Some(BorrowPoolDecoded {
        reserve,
        on_behalf_of,
        amount,
    })
}

fn decode_repay_pool_event(log: &Log) -> Option<RepayPoolDecoded> {
    let topics = log.topics();
    if topics.len() < 4 {
        return None;
    }
    let reserve = Address::from_word(topics[1]);
    let user = Address::from_word(topics[2]);
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&data[0..32]);
    let amount = U256::from_be_bytes::<32>(buf);
    // bool use_a_tokens at byte 31 of word 1.
    let use_a_tokens = data[63] != 0;
    Some(RepayPoolDecoded {
        reserve,
        user,
        amount,
        use_a_tokens,
    })
}

/// `is_part_of_burn` mirror (a Transfer to ZERO is part of some burn).
fn is_part_of_burn(
    ev: &ScaledTokenEvent<'_>,
    scaled_events: &[ScaledTokenEvent<'_>],
    local_assigned: &mut HashSet<u64>,
) -> bool {
    let ev_token = ev.token_address;
    for other in scaled_events {
        if other.event_type != ScaledTokenEventType::CollateralBurn
            && other.event_type != ScaledTokenEventType::DebtBurn
            && other.event_type != ScaledTokenEventType::GhoDebtBurn
        {
            continue;
        }
        if other.user_address != ev.from_address.unwrap_or(Address::ZERO) {
            continue;
        }
        if other.token_address != ev_token {
            continue;
        }
        local_assigned.insert(ev.log_index);
        return true;
    }
    false
}

/// `is_part_of_mint` mirror (a Transfer from ZERO is part of some mint).
fn is_part_of_mint(
    ev: &ScaledTokenEvent<'_>,
    scaled_events: &[ScaledTokenEvent<'_>],
    local_assigned: &mut HashSet<u64>,
) -> bool {
    let ev_token = ev.token_address;
    for other in scaled_events {
        if other.event_type != ScaledTokenEventType::CollateralMint
            && other.event_type != ScaledTokenEventType::DebtMint
            && other.event_type != ScaledTokenEventType::GhoDebtMint
        {
            continue;
        }
        if other.user_address != ev.target_address.unwrap_or(Address::ZERO) {
            continue;
        }
        if other.token_address != ev_token {
            continue;
        }
        local_assigned.insert(ev.log_index);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{LiquidationGroup, LiquidationPattern};

    #[test]
    fn amounts_match_exact_below_threshold() {
        // pool_revision < 9 → exact match required.
        assert!(TransactionOperationsParser::amounts_match(
            U256::from(100),
            U256::from(100),
            5
        ));
        assert!(!TransactionOperationsParser::amounts_match(
            U256::from(99),
            U256::from(100),
            5
        ));
    }

    #[test]
    fn amounts_match_tolerance_at_or_above_threshold() {
        // pool_revision >= 9 → ±TOKEN_AMOUNT_MATCH_TOLERANCE (2).
        assert!(TransactionOperationsParser::amounts_match(
            U256::from(100),
            U256::from(102),
            9
        ));
        assert!(TransactionOperationsParser::amounts_match(
            U256::from(100),
            U256::from(98),
            9
        ));
        assert!(!TransactionOperationsParser::amounts_match(
            U256::from(100),
            U256::from(103),
            9
        ));
    }

    #[test]
    fn are_compatible_transfer_types_pairings() {
        use ScaledTokenEventType::*;
        assert!(TransactionOperationsParser::are_compatible_transfer_types(
            CollateralTransfer,
            Erc20CollateralTransfer,
        ));
        assert!(TransactionOperationsParser::are_compatible_transfer_types(
            DebtTransfer,
            Erc20DebtTransfer,
        ));
        // cross-token kinds are NOT compatible.
        assert!(!TransactionOperationsParser::are_compatible_transfer_types(
            CollateralTransfer,
            Erc20DebtTransfer,
        ));
    }

    #[test]
    fn scaled_token_event_type_predicates() {
        use ScaledTokenEventType::*;
        assert!(CollateralMint.is_collateral());
        assert!(CollateralBurn.is_burn());
        assert!(DebtMint.is_debt());
        assert!(Erc20CollateralTransfer.is_collateral());
        assert!(!Erc20CollateralTransfer.is_burn());
        assert!(GhoDebtMint.is_debt());
        assert!(CollateralInterestBurn.is_collateral());
    }

    #[test]
    fn operation_event_log_indices_dedupes() {
        // Operation.event_log_indices returns the unique log-index set across
        // pool_event + scaled_events + transfer_events + balance_transfer_events
        // — the parse() loop uses this to track which logs have been
        // consumed so subsequent operations skip them.
        let pool_log = test_log(1, &aave_event_decoder::SUPPLY_TOPIC, &[]);
        let mut op = Operation {
            operation_id: 0,
            operation_type: OperationType::Supply,
            pool_revision: 9,
            pool_event: Some(&pool_log),
            scaled_events: Vec::new(),
            transfer_events: vec![&pool_log], // duplicate — must be deduped.
            balance_transfer_events: Vec::new(),
            minted_to_treasury_amount: None,
            debt_to_cover: None,
            validation_errors: Vec::new(),
        };
        op.transfer_events = vec![&pool_log];
        let indices = op.event_log_indices();
        assert_eq!(indices, vec![1]);
    }

    // helper: construct a minimal Log for tests.
    fn test_log(idx: u64, topic0: &alloy::primitives::B256, _data: &[u8]) -> Log {
        use alloy::primitives::{Bytes, Log as AlloyLog};
        let inner = AlloyLog::new_unchecked(Address::ZERO, vec![*topic0], Bytes::default());
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(idx),
            removed: false,
        }
    }

    // ── HQF5NQ-B liquidation engine tests (§4.2 zero-drift surface) ─────
    //
    // The static pattern-detection fns (`analyze_user_liquidation_count`,
    // `collect_debt_burns`, `collect_collateral_events`) are the §4.2-drift
    // critical bit — covered here. The full `create_liquidation_operation`
    // builder needs an in-memory DB substrate (the `get_a_token_for_asset` /
    // `get_v_token_for_asset` sibling lookups); an integration-test fixture
    // is HQF5NQ-C's concern (the apply dispatch glue + the §4.2 cross-check
    // (U5YIBG) is the consumer).

    /// Construct a LiquidationCall-shape Log for tests: 4 topics
    /// (`LIQUIDATION_CALL_TOPIC`, `collateralAsset`, `debtAsset`, `user`) + a 4-word
    /// data payload whose first word is `debt_to_cover`.
    fn liquidation_call_log(
        idx: u64,
        collateral: Address,
        debt: Address,
        user: Address,
        debt_to_cover: U256,
    ) -> Log {
        use alloy::primitives::{Bytes, Log as AlloyLog, B256};
        let topics = vec![
            aave_event_decoder::LIQUIDATION_CALL_TOPIC,
            B256::left_padding_from(collateral.as_slice()),
            B256::left_padding_from(debt.as_slice()),
            B256::left_padding_from(user.as_slice()),
        ];
        let mut data = vec![0u8; 128];
        debt_to_cover
            .to_be_bytes::<32>()
            .iter()
            .enumerate()
            .for_each(|(i, b)| {
                data[32 + i] = *b;
            });
        let inner = AlloyLog::new_unchecked(Address::ZERO, topics, Bytes::from(data));
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(idx),
            removed: false,
        }
    }

    /// Construct a minimal `ScaledTokenEvent` for the liquidation tests. The
    /// `token_address` discriminator (= `debt_v_token` / `collateral_a_token`)
    /// drives the asset-specific filtering.
    fn scaled_event(
        log_idx: u64,
        event_type: ScaledTokenEventType,
        user: Address,
        token_address: Address,
        amount: U256,
        index: Option<U256>,
    ) -> ScaledTokenEvent<'static> {
        // The Log field isn't read by the static collectors (`collect_debt_burns`/
        // `collect_collateral_events`); pass a synthetic placeholder for the lifetime.
        let log = Box::leak(Box::new(test_log(
            log_idx,
            &aave_event_decoder::SUPPLY_TOPIC,
            &[],
        )));
        ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Burn {
                from: user,
                target: Address::ZERO,
                value: amount,
                balance_increase: U256::ZERO,
                index: index.unwrap_or(U256::from(1_000_000_000u64)),
            },
            event_type,
            token_address,
            user_address: user,
            caller_address: None,
            from_address: Some(user),
            target_address: Some(Address::ZERO),
            amount,
            balance_increase: Some(U256::ZERO),
            index,
            log_index: log_idx,
        }
    }

    /// Variant of `scaled_event` for a Transfer whose `target_address` is NOT
    /// the ZERO address — i.e. a collateral movement to a liquidator (not the
    /// burn-side pair ERC20 Transfer-to-ZERO). Used by `collect_collateral_events`
    /// tests to assert the EIWEPM filter distinguishes the two cases.
    fn transfer_to_event(
        log_idx: u64,
        event_type: ScaledTokenEventType,
        from: Address,
        target: Address,
        token_address: Address,
        amount: U256,
    ) -> ScaledTokenEvent<'static> {
        let log = Box::leak(Box::new(test_log(
            log_idx,
            &aave_event_decoder::SUPPLY_TOPIC,
            &[],
        )));
        ScaledTokenEvent {
            log,
            decoded: ScaledTokenEventData::Burn {
                from,
                target,
                value: amount,
                balance_increase: U256::ZERO,
                index: U256::from(1_000_000_000u64),
            },
            event_type,
            token_address,
            user_address: from,
            caller_address: None,
            from_address: Some(from),
            target_address: Some(target),
            amount,
            balance_increase: Some(U256::ZERO),
            index: Some(U256::from(1_000_000_000u64)),
            log_index: log_idx,
        }
    }

    /// `SINGLE` pattern: one `LiquidationCall` → one debt Burn for the same user.
    /// `collect_debt_burns` with `user_liquidation_count == 1` collects ALL
    /// debt burns for the user (no asset filter) — handles bad-debt
    /// multi-asset burns from `_burnBadDebt()`.
    #[test]
    fn collect_debt_burns_single_user_one_burn() {
        let user = Address::from([0xA0; 20]);
        let v_token = Address::from([0xB0; 20]);
        let burn = scaled_event(
            10,
            ScaledTokenEventType::DebtBurn,
            user,
            v_token,
            U256::from(1_000),
            Some(U256::from(1_000_000_000u64)),
        );
        let events = vec![burn];
        let mut assigned = HashSet::new();
        let analysis = HashMap::new();
        let burns = TransactionOperationsParser::collect_debt_burns(
            user,
            Some(v_token),
            &events,
            &mut assigned,
            &analysis,
            1,
            0,
        );
        assert_eq!(burns.len(), 1);
        assert_eq!(burns[0].log_index, 10);
        assert!(assigned.contains(&10));
    }

    /// `COMBINED_BURN` pattern: N liquidations share 1 burn — when
    /// `liquidation_count_for_asset > total_burn_count` + `liquidation_position == 0`,
    /// all burns go to the first liquidation.
    #[test]
    fn collect_debt_burns_combined_burn_pattern() {
        let user = Address::from([0xA0; 20]);
        let v_token = Address::from([0xB0; 20]);
        // 1 burn covering 2 liquidations (the COMBINED_BURN case).
        let combined_burn = scaled_event(
            100,
            ScaledTokenEventType::DebtBurn,
            user,
            v_token,
            U256::from(2_000),
            Some(U256::from(1_000_000_000u64)),
        );
        // An unrelated burn on a different vToken — must NOT be collected
        // (asset-specific filter for multi-liquidation users).
        let other_vtoken = Address::from([0xC0; 20]);
        let other_burn = scaled_event(
            200,
            ScaledTokenEventType::DebtBurn,
            user,
            other_vtoken,
            U256::from(999),
            Some(U256::from(1_000_000_000u64)),
        );
        let events = vec![combined_burn, other_burn];
        let mut assigned = HashSet::new();
        let mut analysis = HashMap::new();
        analysis.insert((user, v_token), 2usize); // 2 liquidations for this asset
                                                  // First liquidation (position 0): collects the single burn.
        let burns0 = TransactionOperationsParser::collect_debt_burns(
            user,
            Some(v_token),
            &events,
            &mut assigned,
            &analysis,
            2,
            0,
        );
        assert_eq!(
            burns0.len(),
            1,
            "COMBINED_BURN: first liquidation gets the burn"
        );
        assert_eq!(burns0[0].log_index, 100);
        // Second liquidation (position 1): no burns left (combined burn goes to position 0).
        let burns1 = TransactionOperationsParser::collect_debt_burns(
            user,
            Some(v_token),
            &events,
            &mut assigned,
            &analysis,
            2,
            1,
        );
        assert!(
            burns1.is_empty(),
            "second liquidation gets no burn (already taken)"
        );
        // Unrelated-vToken burn must NEVER have been touched.
        assert!(!assigned.contains(&200));
    }

    /// `SEPARATE_BURNS` pattern: N liquidations → N burns, one per liquidation.
    /// `liquidation_count_for_asset == total_burn_count`; burn[i] belongs to
    /// liquidation[i] (sequential matching via `liquidation_position`).
    #[test]
    fn collect_debt_burns_separate_burns_pattern() {
        let user = Address::from([0xA0; 20]);
        let v_token = Address::from([0xB0; 20]);
        let b0 = scaled_event(
            10,
            ScaledTokenEventType::DebtBurn,
            user,
            v_token,
            U256::from(1000),
            Some(U256::from(1_000_000_000u64)),
        );
        let b1 = scaled_event(
            20,
            ScaledTokenEventType::DebtBurn,
            user,
            v_token,
            U256::from(2000),
            Some(U256::from(1_000_000_000u64)),
        );
        let events = vec![b0, b1];
        let mut assigned = HashSet::new();
        let mut analysis = HashMap::new();
        analysis.insert((user, v_token), 2usize);
        // Position 0 → first burn (log_index 10).
        let burns0 = TransactionOperationsParser::collect_debt_burns(
            user,
            Some(v_token),
            &events,
            &mut assigned,
            &analysis,
            2,
            0,
        );
        assert_eq!(burns0.len(), 1);
        assert_eq!(burns0[0].log_index, 10);
        // Position 1 → second burn (log_index 20).
        let burns1 = TransactionOperationsParser::collect_debt_burns(
            user,
            Some(v_token),
            &events,
            &mut assigned,
            &analysis,
            2,
            1,
        );
        assert_eq!(burns1.len(), 1);
        assert_eq!(burns1[0].log_index, 20);
    }

    /// `analyze_user_liquidation_count`: counts `LiquidationCall` events per
    /// user (not per asset-pair). When `user_liquidation_count == 1`, ALL
    /// debt burns for that user are collected (no asset filter — bad-debt
    /// `_burnBadDebt()` across multiple debt positions).
    #[test]
    fn analyze_user_liquidation_count_counts_per_user() {
        let user_a = Address::from([0xA0; 20]);
        let user_b = Address::from([0xB0; 20]);
        let debt_weth = Address::from([0xC0; 20]);
        let debt_dai = Address::from([0xD0; 20]);
        let collateral = Address::from([0xE0; 20]);
        // User A: 2 liquidations (multi-asset pair).
        let a1 = liquidation_call_log(10, collateral, debt_weth, user_a, U256::from(1000));
        let a2 = liquidation_call_log(20, collateral, debt_dai, user_a, U256::from(2000));
        // User B: 1 liquidation (single, bad-debt path).
        let b1 = liquidation_call_log(30, collateral, debt_weth, user_b, U256::from(500));
        let events: Vec<&Log> = vec![&a1, &a2, &b1];
        let counts = TransactionOperationsParser::analyze_user_liquidation_count(&events);
        assert_eq!(counts.get(&user_a), Some(&2));
        assert_eq!(counts.get(&user_b), Some(&1));
    }

    /// `collect_collateral_events`: the `collateral_burn` vs `collateral_transfers`
    /// split. When collateral is moved to the liquidator (not burned), the
    /// burn is `None` + `collateral_transfers` is non-empty.
    #[test]
    fn collect_collateral_events_burn_vs_transfers_split() {
        let user = Address::from([0xA0; 20]);
        let a_token = Address::from([0xB0; 20]);
        let other_atoken = Address::from([0xC0; 20]);
        let liquidator = Address::from([0xD0; 20]);
        // The burn for this liquidation's aToken.
        let burn = scaled_event(
            10,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        );
        // The transfer to a non-ZERO liquidator (NOT the burn-side pair).
        //
        // TYS5MS: this Erc20CollateralTransfer is now SKIPPED by the new mirror
        // of Python's `_should_skip_collateral_transfer` LC-op filter — in
        // Liquidation ops, ALL ERC20 CollateralTransfers are filtered at
        // collect-time (Python's filter has the same behavior: `index is None
        // AND op=LC` → skip). The liquidator's net credit comes from the Mint
        // event (when `receiveAToken=true`) or from a paired `CollateralTransfer`
        // (BalanceTransfer variant — index=non-None) — NOT via the standard
        // ERC20 Transfer event. Marked assigned so the standalone Step-4e
        // Transfer path doesn't re-collect it.
        let transfer = transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            liquidator,
            a_token,
            U256::from(500),
        );
        // A burn on a different aToken (user multi-liquidated) — must NOT be collected.
        let other_burn = scaled_event(
            12,
            ScaledTokenEventType::CollateralBurn,
            user,
            other_atoken,
            U256::from(999),
            None,
        );
        let events = vec![burn, transfer, other_burn];
        let mut assigned = HashSet::new();
        let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
            user,
            Some(a_token),
            &events,
            &mut assigned,
        );
        assert!(
            cb.is_some(),
            "collateral_burn found for this liquidation's aToken"
        );
        assert_eq!(cb.unwrap().log_index, 10);
        assert_eq!(
            ct.len(),
            0,
            "TYS5MS: Erc20CollateralTransfer to liquidator is now SKIPPED at \
             collect-time (mirrors Python's `_should_skip_collateral_transfer` \
             LC-op filter — ERC20 Transfers in Liquidation ops are never applied; \
             the liquidator's credit comes via the Mint event or via the \
             `CollateralTransfer` BalanceTransfer variant, not via the standard \
             ERC20 Transfer event)"
        );
        assert!(
            assigned.contains(&11),
            "TYS5MS: filtered ERC20 transfer marked assigned so the standalone \
             Step-4e Transfer path doesn't re-collect it"
        );
    }

    /// TYS5MS regression guard — `collect_collateral_events` MUST skip
    /// `Erc20CollateralTransfer` to a NON-ZERO recipient in LC ops (the
    /// Aave V3 treasury protocol-fee Transfer shape). Aave V3 `LiquidationCall`
    /// transfers the protocol-fee portion from the liquidated user to the
    /// treasury via `transferOnLiquidation`, emitting BOTH:
    ///   - `Transfer(user, treasury, fee_underlying)` (Erc20CollateralTransfer,
    ///     index=None) — the standard ERC20 Transfer event
    ///   - `BalanceTransfer(user, treasury, fee_scaled, liquidity_index)`
    ///     (CollateralTransfer variant, index=non-None) — Aave V3's native
    ///     scaled-balance move event
    /// Python's apply-path filter `_should_skip_collateral_transfer` skips the
    /// ERC20 variant in LC ops so the user is debited + treasury credited
    /// ONCE via the BT event's `value`. Rust mirrors the filter at
    /// collect-time here. RED-verified: stripping the skip-guard causes
    /// the assertion to fail (Erc20 collected, ct.len() == 1).
    #[test]
    fn collect_collateral_events_skips_erc20_fee_transfer_to_treasury_in_liquidation() {
        let user = Address::from([0xA0; 20]);
        let a_token = Address::from([0xB0; 20]);
        // The Aave V3 treasury (verified on mainnet as 0x464C71f6...e18c —
        // recipient of all protocol-fee Transfers in LiquidationCall logs).
        let treasury = Address::from([0xC0; 20]);
        // Collateral Burn (user's full liquidation scaled amount minus fee).
        let burn = scaled_event(
            10,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500_000),
            Some(U256::from(1_000_500_000u64)),
        );
        // Standard ERC20 Transfer(user→treasury, fee_underlying) emitted
        // alongside `LiquidationCall` by `transferOnLiquidation`. Without the
        // TYS5MS skip-guard, this would be DOUBLE-APPLIED (once here as the
        // fee_underlying amount, AND once via the paired BT.value below),
        // over-debiting the user + over-crediting the treasury by exactly
        // the fee_underlying amount.
        let fee_underlying_amount = U256::from(5_815_314_991_815_639u64);
        let erc20_fee_transfer = transfer_to_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            treasury,
            a_token,
            fee_underlying_amount,
        );
        // Paired Aave V3 BalanceTransfer(user→treasury, fee_scaled, index) —
        // the SCALED-balance move that Python (and Rust post-TYS5MS) applies.
        // value × index / RAY ≈ erc20_fee_transfer.amount (within tiny
        // ray-floor rounding). Modeled here via the CollateralTransfer variant
        // (index is non-None, indicating a BalanceTransfer event_type).
        let fee_scaled_amount = U256::from(5_812_238_924_384_490u64);
        let bt_fee_transfer = transfer_to_event(
            12,
            ScaledTokenEventType::CollateralTransfer,
            user,
            treasury,
            a_token,
            fee_scaled_amount,
        );
        let events = vec![burn, erc20_fee_transfer, bt_fee_transfer];
        let mut assigned = HashSet::new();
        let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
            user,
            Some(a_token),
            &events,
            &mut assigned,
        );
        // Burn must be found.
        assert_eq!(cb.unwrap().log_index, 10);
        // ONLY the BT event is collected — the ERC20 fee Transfer is skipped.
        assert_eq!(ct.len(), 1, "TYS5MS: only the BT fee transfer is collected; \
            the Erc20CollateralTransfer(user→treasury, fee) is SKIPPED at \
            collect-time (mirrors Python's `_should_skip_collateral_transfer`)");
        assert_eq!(ct[0].log_index, 12, "the collected transfer is the BT event");
        // The skipped ERC20 fee Transfer MUST be marked assigned so the
        // standalone Step-4e Transfer path doesn't re-collect it (which
        // would re-instate the double-application the TYS5MS fix prevents).
        assert!(
            assigned.contains(&11),
            "TYS5MS: the skipped Erc20CollateralTransfer to treasury IS marked \
             assigned so the standalone Step-4e Transfer path skips it; the \
             paired BT event also IS marked assigned by being collected"
        );
        // The BT event is marked assigned by being collected into collateral_transfers
        // (the caller writes assigned.insert for collected items elsewhere).
    }

    /// `collect_collateral_events` EIWEPM filter (per the orchestrator's
    /// fix directive): a burn-side pair ERC20 Transfer-to-ZERO (the Burn
    /// event's operational companion) MUST be excluded from
    /// `collateral_transfers` so the Liquidation path doesn't double-debit
    /// the user (the p1198-class 4.33× divergence root cause).
    #[test]
    fn collect_collateral_events_filters_burn_side_pair_transfer_to_zero() {
        let user = Address::from([0xA0; 20]);
        let a_token = Address::from([0xB0; 20]);
        // The CollateralBurn event.
        let burn = scaled_event(
            10,
            ScaledTokenEventType::CollateralBurn,
            user,
            a_token,
            U256::from(500),
            None,
        );
        // The paired ERC20 Transfer user→0x0 (burn-side companion — same user +
        // token, value matches). MUST be filtered by `is_part_of_burn`.
        let pair_transfer = scaled_event(
            11,
            ScaledTokenEventType::Erc20CollateralTransfer,
            user,
            a_token,
            U256::from(500),
            None,
        );
        let events = vec![burn, pair_transfer];
        let mut assigned = HashSet::new();
        let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
            user,
            Some(a_token),
            &events,
            &mut assigned,
        );
        assert!(cb.is_some(), "burn collected");
        assert_eq!(cb.unwrap().log_index, 10);
        assert_eq!(
            ct.len(),
            0,
            "burn-side pair Transfer-to-ZERO filtered (EIWEPM fix)"
        );
        assert!(
            assigned.contains(&11),
            "filtered pair-transfer marked assigned so it isn't re-collected by standalone Step-4e"
        );
    }

    /// `collect_collateral_events`: when no events match (collateral was fully
    /// transferred, no burn), the split is `(None, [...])`. The Python's caller
    /// asserts at least one event exists; tested variant separately.
    #[test]
    fn collect_collateral_events_no_burn_only_transfers() {
        let user = Address::from([0xA0; 20]);
        let a_token = Address::from([0xB0; 20]);
        let transfer = scaled_event(
            11,
            ScaledTokenEventType::CollateralTransfer,
            user,
            a_token,
            U256::from(500),
            Some(U256::from(1_000_000_000u64)),
        );
        let events = vec![transfer];
        let mut assigned = HashSet::new();
        let (cb, ct) = TransactionOperationsParser::collect_collateral_events(
            user,
            Some(a_token),
            &events,
            &mut assigned,
        );
        assert!(cb.is_none(), "no burn — collateral moved via transfer");
        assert_eq!(ct.len(), 1);
    }

    /// `LiquidationGroup::detect_pattern` (mirror of `pattern_types.py`) —
    /// the §4.2-critical pattern classifier.
    #[test]
    fn liquidation_group_detect_pattern() {
        // SINGLE: 1 liquidation, 1 burn.
        let mut single = LiquidationGroup::default();
        single.liquidations.push((0, U256::from(100), 0));
        single.burn_events.push((1, U256::from(100)));
        assert_eq!(single.detect_pattern(), LiquidationPattern::Single);

        // COMBINED_BURN: 3 liquidations, 1 burn.
        let combined = LiquidationGroup {
            liquidations: vec![
                (0, U256::from(100), 0),
                (1, U256::from(200), 10),
                (2, U256::from(300), 20),
            ],
            burn_events: vec![(5, U256::from(600))],
        };
        assert_eq!(combined.detect_pattern(), LiquidationPattern::CombinedBurn);

        // SEPARATE_BURNS: 3 liquidations, 3 burns.
        let separate = LiquidationGroup {
            liquidations: vec![(0, U256::from(100), 0), (1, U256::from(200), 10)],
            burn_events: vec![(5, U256::from(100)), (6, U256::from(200))],
        };
        assert_eq!(separate.detect_pattern(), LiquidationPattern::SeparateBurns);
    }
}
