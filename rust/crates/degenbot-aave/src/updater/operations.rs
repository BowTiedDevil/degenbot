//! Domain types for the Aave V3 transaction operations parser (port of
//! `src/degenbot/aave/operations.py` + `operation_types.py` + `pattern_types.py`).
//!
//! The parser [`crate::operations_parser::TransactionOperationsParser`] is a
//! per-Ethereum-tx grouping engine: it matches Pool events (Supply/Borrow/
//! Repay/Withdraw/MintToTreasury/Deficit) to their constituent `ScaledToken`
//! events (aToken/vToken Mint/Burn/Transfer + plain ERC20 Transfer) by user +
//! amount +/- a pool-revision ray-flooring tolerance (`_amounts_match`), then
//! emits typed [`Operation`] objects the apply dispatch glue (HQF5NQ-C) consumes
//! to construct `AaveChunkEvent` variants and dispatch via
//! [`crate::run::apply_aave_chunk_writes_on_conn`].
//!
//! This module is the **pure-data contract** between A (this task) + B
//! (the liquidation engine, [`LiquidationPatternContext`] is a stub here so
//! B can extend it) + C (the apply dispatch glue, consumes `Operation`s).
//! No DB / RPC / processor calls live here — the parser struct holds the
//! stateful behavior in `operations_parser.rs`; this file owns the data types.
//!
//! # Lifetime contract
//!
//! [`ScaledTokenEvent`] + [`Operation`] borrow their source `&Log` (alloy's
//! `alloy::rpc::types::Log`) — the parser is fed a `&[&Log]` slice and all
//! `Operation`/`ScaledTokenEvent` references stay valid for the lifetime of
//! that slice. The orchestrator (6SWY4R) owns the underlying `Vec<Log>` (the
//! RPC-fetched receipt list) and hands a `&[&Log]` view to the parser.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;

// ── the ω-shape constants (`operations.py:L19-L23`) ────────────────────────

/// Pool revisions at-or-above this use flooring ray division which can cause
/// ±2 wei deviations when the Pool scales the underlying amount before
/// handing it to the aToken/vToken contract. The matching gate
/// [`amounts_match`] widens the equality by [`TOKEN_AMOUNT_MATCH_TOLERANCE`]
/// wei on those revisions.
pub const SCALED_AMOUNT_POOL_REVISION: u32 = 9;
/// The ±wei tolerance applied on pool revisions >=
/// [`SCALED_AMOUNT_POOL_REVISION`]. Mirrors `operations.py::TOKEN_AMOUNT_MATCH_TOLERANCE`.
pub const TOKEN_AMOUNT_MATCH_TOLERANCE: u64 = 2;

// ── enums (`operation_types.py` + `operations.py::ScaledTokenEventType`) ──

/// The discriminated kind of Aave V3 `Operation` produced by the parser.
/// Mirrors `aave/operation_types.py::OperationType` (1:1, in declaration order
/// to make the parity cross-check trivial).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub enum OperationType {
    /// `Supply` → `CollateralMint`.
    Supply,
    /// `Withdraw` → `CollateralBurn`.
    Withdraw,
    /// `Borrow` → `DebtMint`.
    Borrow,
    /// `Repay` → `DebtBurn`.
    Repay,
    /// `Repay` (useATokens=true) → `DebtBurn` + `CollateralBurn`.
    RepayWithAtokens,
    /// `LiquidationCall` → `DebtBurn` + `CollateralBurn` (+ the multi-burn
    /// pattern detection). The body of the liquidation builder is HQF5NQ-B;
    /// this variant is defined here so the `OperationType` enum is the full
    /// contract surface B can pattern-match against.
    Liquidation,
    /// GHO `Borrow` → `GhoDebtMint`.
    GhoBorrow,
    /// GHO `Repay` → `GhoDebtBurn`.
    GhoRepay,
    /// GHO `LiquidationCall` → `GhoDebtBurn` + `CollateralBurn`.
    GhoLiquidation,
    /// GHO `DeficitCreated` → `GhoDebtBurn` (flash-loan payoff path).
    GhoFlashLoan,
    /// Mint/Burn with no matching Pool event (amount == `balance_increase`).
    InterestAccrual,
    /// Standalone aToken/vToken `BalanceTransfer` (no Pool event).
    BalanceTransfer,
    /// Paired `BalanceTransfer + Burn` (Umbrella deficit coverage).
    DeficitCoverage,
    /// Pool `mintToTreasury()` (no SUPPLY event).
    MintToTreasury,
    /// stkAAVE (GHO discount-token) transfer.
    StkAaveTransfer,
    /// Only `DeficitCreated` falls here — placeholder awaiting liquidation
    /// matching downstream. Mirrors the Python's "`DEFICIT_CREATED` is always
    /// marked UNKNOWN" treatment.
    Unknown,
}

/// A token classification for an emitter contract address (the
/// aToken-or-vToken-or-gho-discount-token discriminator the parser uses to
/// classify each decoded `Mint`/`Burn`/`BalanceTransfer`/`Transfer` event
/// into a [`ScaledTokenEventType`]). Mirrors `aave/types.py::TokenType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub enum TokenType {
    /// An aToken (collateral) contract.
    Atoken,
    /// A vToken (variable debt) contract.
    Vtoken,
    /// The GHO-discount-token (the stkAAVE-equivalent discount token if the
    /// mechanism is active). Plain `Transfer`s of this contract emit
    /// [`ScaledTokenEventType::DiscountTransfer`] operations.
    GhoDiscount,
}

/// The discriminated kind of a scaled-token (or plain ERC20 Transfer) event,
/// as seen by the parser. Mirrors `aave/events.py::ScaledTokenEventType`:
/// the emission variants (`*Mint`/`*Burn`/`*Transfer`) + the 6 interest-accrual
/// derived variants applied AFTER matching (the parser flourishes the Python
/// applies when classifying the net effect of a `Mint` emitted on a
/// repayment/withdrawal path where interest exceeded the principal).
///
/// Per HQF5NQ (ecfb5c-flag-3-resolution): this enum lives in the **parser**
/// crate (not the **decoder** crate). The decoder emits raw
/// `Address`/`U256` fields; the parser classifies the event by the emitter
/// `Address` (aToken vs vToken vs GHO-vToken vs GHO-discount-token) + the
/// operation context (interest-exceeds-principal → derived interest variant).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub enum ScaledTokenEventType {
    // ── the 12 emission variants ──────────────────────────────────────
    CollateralMint,
    CollateralBurn,
    CollateralTransfer,
    DebtMint,
    DebtBurn,
    DebtTransfer,
    GhoDebtMint,
    GhoDebtBurn,
    GhoDebtTransfer,
    /// Plain ERC20 `Transfer` on an aToken (collateral) contract — paired with
    /// a `BalanceTransfer` for the same transfer when both exist.
    Erc20CollateralTransfer,
    /// Plain ERC20 `Transfer` on a vToken (debt) contract.
    Erc20DebtTransfer,
    /// Plain ERC20 `Transfer` on the GHO-discount-token contract (a stkAAVE
    /// movement tracked as a discount-token operation).
    DiscountTransfer,
    // ── the 6 interest-accrual derived variants (parser flourishes) ────
    /// An aToken `Mint` where `amount == balance_increase` (pure interest).
    CollateralInterestMint,
    /// An aToken `Burn` derived as the interest-exceeds-principal pair of a
    /// `Mint` on the WITHDRAW / `RepayWithAtokens` path.
    CollateralInterestBurn,
    /// A vToken `Mint` where `amount == balance_increase`.
    DebtInterestMint,
    /// A vToken `Burn` derived (the `_create_interest_accrual_operations`
    /// path also handles unassigned debt burns to surface debt-reduction).
    DebtInterestBurn,
    /// GHO-vToken `Mint` where `amount == balance_increase`.
    GhoDebtInterestMint,
    /// GHO-vToken `Burn` (unassigned, surfaced as interest accrual).
    GhoDebtInterestBurn,
}

impl ScaledTokenEventType {
    /// Mirrors `operations.py::ScaledTokenEvent.is_collateral`.
    #[must_use]
    pub fn is_collateral(self) -> bool {
        matches!(
            self,
            Self::CollateralBurn
                | Self::CollateralMint
                | Self::CollateralTransfer
                | Self::CollateralInterestBurn
                | Self::CollateralInterestMint
                | Self::Erc20CollateralTransfer
        )
    }
    /// Mirrors `operations.py::ScaledTokenEvent.is_debt`.
    #[must_use]
    pub fn is_debt(self) -> bool {
        matches!(
            self,
            Self::DebtBurn
                | Self::DebtMint
                | Self::DebtTransfer
                | Self::DebtInterestBurn
                | Self::DebtInterestMint
                | Self::GhoDebtBurn
                | Self::GhoDebtMint
                | Self::GhoDebtTransfer
                | Self::GhoDebtInterestBurn
                | Self::GhoDebtInterestMint
                | Self::Erc20DebtTransfer
        )
    }
    /// Mirrors `operations.py::ScaledTokenEvent.is_burn`.
    #[must_use]
    pub fn is_burn(self) -> bool {
        matches!(
            self,
            Self::CollateralBurn
                | Self::CollateralInterestBurn
                | Self::DebtBurn
                | Self::DebtInterestBurn
                | Self::GhoDebtBurn
                | Self::GhoDebtInterestBurn
        )
    }
}

// ── ScaledTokenEvent (`operations.py::ScaledTokenEvent`) ───────────────────

/// The parser-side wrapper around a decoded scaled-token (or plain ERC20
/// Transfer) event. Carries the emitter `Address` (so the apply dispatch
/// glue can re-resolve position ids), the classification [`ScaledTokenEventType`],
/// the user/from/target addresses (decoded by
/// [`degenbot_decoders::aave_event_decoder`]), and the raw `value`/
/// `balance_increase`/`index` fields the matching helpers
/// ([`crate::operations_parser::TransactionOperationsParser`]) use.
///
/// Borrows the source `&Log` so downstream `Operation`s can re-reference the
/// raw event for `transfer_events` / `balance_transfer_events` arrays (mirror
/// of `operations.py::ScaledTokenEvent.event`).
#[derive(Clone, Debug)]
#[allow(clippy::module_name_repetitions)]
pub struct ScaledTokenEvent<'a> {
    /// The source log (mirror of `ScaledTokenEvent.event`).
    pub log: &'a Log,
    /// Field-by-field decoded event data (the Mint/Burn/BalanceTransfer/
    /// `Erc20Transfer` struct from ECFB5C's `aave_event_decoder`).
    pub decoded: ScaledTokenEventData,
    /// The classification (mirror of `ScaledTokenEvent.event_type`).
    pub event_type: ScaledTokenEventType,
    /// The contract that emitted the event (the aToken/vToken/Discount-token).
    pub token_address: Address,
    /// The user address the event credits/debits (`onBehalfOf` for Mint;
    /// `from` for Burn/BalanceTransfer/Transfer). Mirrors
    /// `ScaledTokenEvent.user_address`.
    pub user_address: Address,
    /// `caller` for Mint; `None` for Burn/BalanceTransfer/Transfer.
    pub caller_address: Option<Address>,
    /// `from` for Burn/BalanceTransfer/Transfer; `None` for Mint.
    pub from_address: Option<Address>,
    /// `to` for Burn/BalanceTransfer/Transfer; `None` for Mint.
    pub target_address: Option<Address>,
    /// The `value` field for Mint/Burn; the scaled amount for `BalanceTransfer`;
    /// the `value` for plain ERC20 Transfer.
    pub amount: U256,
    /// `balance_increase` for Mint/Burn; `0` for `BalanceTransfer`; `None`
    /// for plain ERC20 Transfer.
    pub balance_increase: Option<U256>,
    /// The event's `index`; `None` for plain ERC20 Transfer.
    pub index: Option<U256>,
    /// The logIndex (mirror of `event["logIndex"]`) — used by the parser for
    /// assigned-index tracking + chronological ordering.
    pub log_index: u64,
}

/// Field-by-field decoded scaled-token event data (the discriminator for the
/// 4 emission shapes — Mint/Burn/BalanceTransfer/Transfer — re-extracted from
/// the ECFB5C `DecodedAaveEvent` variants). Owned data (no lifetime).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::module_name_repetitions)]
pub enum ScaledTokenEventData {
    /// `ScaledTokenMint` raw fields.
    Mint {
        caller: Address,
        on_behalf_of: Address,
        value: U256,
        balance_increase: U256,
        index: U256,
    },
    /// `ScaledTokenBurn` raw fields.
    Burn {
        from: Address,
        target: Address,
        value: U256,
        balance_increase: U256,
        index: U256,
    },
    /// `ScaledTokenBalanceTransfer` raw fields.
    BalanceTransfer {
        from: Address,
        to: Address,
        value: U256,
        index: U256,
    },
    /// Plain ERC20 `Transfer` raw fields.
    Transfer {
        from: Address,
        to: Address,
        value: U256,
    },
}

// ── Operation (`operations.py::Operation`) ───────────────────────────────

/// A single logical operation with complete asset-flow context. Mirrors
/// `operations.py::Operation` (dataclass by dataclass; the Python `validation_errors`
/// list is preserved verbatim — validators run at the end of
/// [`crate::operations_parser::TransactionOperationsParser::parse`]).
///
/// Borrows source logs for the lifetime of the input `&[&Log]` slice.
#[derive(Clone, Debug)]
pub struct Operation<'a> {
    /// The sequential operation id (assigned by `parse()`).
    pub operation_id: u32,
    /// The discriminated operation type.
    pub operation_type: OperationType,
    /// The Pool contract revision at parse-start (DP4 — read once per tx;
    /// mid-tx `PoolUpdated` config events are the orchestrator's concern).
    pub pool_revision: u32,
    /// The Pool event triggering the operation (raw log); `None` for the
    /// phase-4 post-loop operations (MintToTreasury/DeficitCoverage/
    /// InterestAccrual/Transfer/StkAaveTransfer).
    pub pool_event: Option<&'a Log>,
    /// The matched scaled-token events (the parser grouped these into the
    /// operation).
    pub scaled_events: Vec<ScaledTokenEvent<'a>>,
    /// Supporting aToken/vToken Transfer events (raw logs; mirror of
    /// `Operation.transfer_events`).
    pub transfer_events: Vec<&'a Log>,
    /// Supporting aToken `BalanceTransfer` events (raw logs; mirror of
    /// `Operation.balance_transfer_events`).
    pub balance_transfer_events: Vec<&'a Log>,
    /// For `MintToTreasury`: the `MintedToTreasury` event's `amountMinted` field
    /// (raw UNDERLYING amount, regardless of `pool_revision`). Mirrors Python's
    /// `operation.minted_to_treasury_amount` semantics — NOT pre-scaled.
    /// The scaling conversion (`ray_div` / `ray_div_ceil` per `pool_revision`) happens
    /// EXACTLY ONCE in `dispatch_mint_to_treasury` (mirrors Python's
    /// `PoolMath::underlying_to_scaled_collateral` flow). Previously DP3
    /// pre-converted for rev < 9 → dispatch would re-convert → SB3XJF
    /// DOUBLE-conversion divergence.
    pub minted_to_treasury_amount: Option<U256>,
    /// For Liquidation: the `LiquidationCall` `debtToCover` field (surfaces
    /// the burn-amount's accuracy edge — the Burn `amount + balance_increase`
    /// is off by 1 wei). HQF5NQ-B's concern.
    pub debt_to_cover: Option<U256>,
    /// Validation errors filled by the `_validate_*` fns (`operations_parser.py`).
    pub validation_errors: Vec<String>,
}

impl Operation<'_> {
    /// Mirrors `operations.py::Operation.is_valid`.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.validation_errors.is_empty()
    }

    /// The set of log-indices involved in this operation (mirror of
    /// `Operation.get_event_log_indices`). Used by [`crate::operations_parser`]
    /// to track which logs have been consumed by some Operation (so subsequent
    /// operations skip them).
    #[must_use]
    pub fn event_log_indices(&self) -> Vec<u64> {
        let mut indices: Vec<u64> = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        if let Some(pool_ev) = self.pool_event {
            let idx = pool_ev_idx(pool_ev);
            indices.push(idx);
            seen.insert(idx);
        }
        for ev in &self.scaled_events {
            if seen.insert(ev.log_index) {
                indices.push(ev.log_index);
            }
        }
        for ev in &self.transfer_events {
            let idx = log_idx(ev);
            if seen.insert(idx) {
                indices.push(idx);
            }
        }
        for ev in &self.balance_transfer_events {
            let idx = log_idx(ev);
            if seen.insert(idx) {
                indices.push(idx);
            }
        }
        indices
    }
}

/// Read the `logIndex` from a log; `0` if absent (matches the parser's
/// pre-existing `ev["logIndex"]` access — every log from a real RPC receipt
/// has one, but the field is `Option` on alloy's `Log`).
fn log_idx(log: &Log) -> u64 {
    log.log_index.map_or(0, |i| i)
}
/// The Pool-event variant's `log_index` equivalent (Pool events are raw logs
/// held by reference in `Operation.pool_event`).
fn pool_ev_idx(log: &Log) -> u64 {
    log_idx(log)
}

// ── the liquidation-pattern stubs (pattern_types.py) ──────────────────────
//
// The full body lives in HQF5NQ-B; the type definitions land here so B can
// extend them and so the parser's `parse()` scaffold (which builds but does
// not exercise the pattern context) is wired.

/// Mirrors `aave/pattern_types.py::LiquidationPattern`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(clippy::module_name_repetitions)]
pub enum LiquidationPattern {
    /// 1 liquidation, 1 burn (standard).
    Single,
    /// N liquidations, 1 combined burn (issue 0056).
    CombinedBurn,
    /// N liquidations, N burns (issue 0065).
    SeparateBurns,
}

/// Mirrors `aave/pattern_types.py::LiquidationPatternContext`.
///
/// Built per-tx by [`crate::transaction_processor::build_liquidation_patterns`]
/// (the port of `detect_liquidation_patterns`); consumed by the liquidation
/// dispatch (`dispatch_liquidation`) to apply the `COMBINED_BURN`
/// aggregated-burn + Mint-skip behavior (Issue 0056).
#[derive(Clone, Debug, Default)]
#[allow(clippy::module_name_repetitions)]
pub struct LiquidationPatternContext {
    /// `(user, debt_v_token)` → detected pattern.
    pub patterns: std::collections::HashMap<(Address, Address), LiquidationPattern>,
    /// `(user, debt_v_token)` → group state (mirrors
    /// `LiquidationGroup::liquidations` / `burn_events`).
    pub groups: std::collections::HashMap<(Address, Address), LiquidationGroup>,
    /// Groups already fully processed (`COMBINED_BURN` handling).
    pub processed: std::collections::HashSet<(Address, Address)>,
}

impl LiquidationPatternContext {
    /// Get the detected pattern for a `(user, debt_v_token)`. Mirrors
    /// `LiquidationPatternContext.get_pattern`. `None` for a pair with no
    /// liquidation group (the dispatch falls through to the per-op path).
    #[must_use]
    pub fn get_pattern(&self, user: Address, debt_v_token: Address) -> Option<LiquidationPattern> {
        self.patterns.get(&(user, debt_v_token)).copied()
    }

    /// Get the liquidation group for a `(user, debt_v_token)`. Mirrors
    /// `LiquidationPatternContext.get_group`.
    #[must_use]
    pub fn get_group(&self, user: Address, debt_v_token: Address) -> Option<&LiquidationGroup> {
        self.groups.get(&(user, debt_v_token))
    }

    /// Whether a group has been fully processed (`COMBINED_BURN`
    /// aggregated-burn has already fired). Mirrors
    /// `LiquidationPatternContext.is_processed`.
    #[must_use]
    pub fn is_processed(&self, user: Address, debt_v_token: Address) -> bool {
        self.processed.contains(&(user, debt_v_token))
    }

    /// Mark a group as fully processed (`COMBINED_BURN` handling). Mirrors
    /// `LiquidationPatternContext.mark_processed`.
    pub fn mark_processed(&mut self, user: Address, debt_v_token: Address) {
        self.processed.insert((user, debt_v_token));
    }
}

/// Mirrors `aave/pattern_types.py::LiquidationGroup`.
#[derive(Clone, Debug, Default)]
#[allow(clippy::module_name_repetitions)]
pub struct LiquidationGroup {
    /// `(operation_id, debt_to_cover, pool_event_log_index)` tuples.
    pub liquidations: Vec<(u32, U256, u64)>,
    /// `(burn_event_log_index, burn_amount)` tuples.
    pub burn_events: Vec<(u64, U256)>,
}

impl LiquidationGroup {
    #[must_use]
    pub fn liquidation_count(&self) -> usize {
        self.liquidations.len()
    }
    #[must_use]
    pub fn burn_event_count(&self) -> usize {
        self.burn_events.len()
    }
    #[must_use]
    pub fn total_debt_to_cover(&self) -> U256 {
        self.liquidations
            .iter()
            .fold(U256::ZERO, |acc, &(_, debt, _)| acc + debt)
    }
    /// Mirrors `LiquidationGroup::detect_pattern`.
    #[must_use]
    pub fn detect_pattern(&self) -> LiquidationPattern {
        if self.liquidation_count() == 1 {
            LiquidationPattern::Single
        } else if self.burn_event_count() == 1 {
            LiquidationPattern::CombinedBurn
        } else {
            LiquidationPattern::SeparateBurns
        }
    }
}
