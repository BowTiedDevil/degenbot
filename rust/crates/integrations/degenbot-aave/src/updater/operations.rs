//! Domain types for the Aave V3 transaction operations parser.
//!
//! The parser [`crate::operations_parser::TransactionOperationsParser`] is a
//! per-Ethereum-tx grouping engine: it matches Pool events (Supply/Borrow/
//! Repay/Withdraw/MintToTreasury/Deficit) to their constituent `ScaledToken`
//! events (aToken/vToken Mint/Burn/Transfer + plain ERC20 Transfer) by user +
//! amount +/- a pool-revision ray-flooring tolerance
//! (`TransactionOperationsParser::amounts_match`), then
//! emits typed [`Operation`] objects the apply dispatch glue consumes
//! to construct canonical delta effects and dispatch via
//! [`crate::run::apply_aave_chunk_writes_on_conn`].
//!
//! This module is the **pure-data contract** the parser fills and the apply
//! dispatch consumes, including the [`LiquidationPatternContext`] that
//! `transaction_processor::build_liquidation_patterns` builds and the
//! liquidation dispatch reads. No DB / RPC / processor calls live here — the
//! parser struct holds the stateful behavior in `operations_parser/mod.rs`;
//! this file owns the data types.
//!
//! # Lifetime contract
//!
//! [`ScaledTokenEvent`] + [`Operation`] borrow their source `&Log` (alloy's
//! `alloy::rpc::types::Log`) — the parser is fed a `&[&Log]` slice and all
//! `Operation`/`ScaledTokenEvent` references stay valid for the lifetime of
//! that slice. The orchestrator owns the underlying `Vec<Log>` (the
//! RPC-fetched receipt list) and hands a `&[&Log]` view to the parser.

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use std::collections::HashSet;

// ── parity tolerance constants ────────────────────────────────────────────

/// The ±wei tolerance the matching gates allow on Pool revisions that floor
/// scaled amounts (`revision::scaled_amount_flooring`), where the Pool's
/// flooring ray division can deviate when it scales the underlying amount
/// before handing it to the aToken/vToken contract.
pub const TOKEN_AMOUNT_MATCH_TOLERANCE: u64 = 2;

// ── enums ─────────────────────────────────────────────────────────────────

/// The discriminated kind of Aave V3 `Operation` produced by the parser.
/// Variants are declared 1:1 with the committed Python gold fixture's
/// declaration order so the parity cross-check stays trivial.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// pattern detection). The liquidation-builder body lives in the apply
    /// dispatch glue; this variant is defined here so the `OperationType`
    /// enum is the full contract surface it can pattern-match against.
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
    /// matching downstream: a `DEFICIT_CREATED` event is always UNKNOWN.
    Unknown,
}

/// A token classification for an emitter contract address (the
/// aToken-or-vToken-or-gho-discount-token discriminator the parser uses to
/// classify each decoded `Mint`/`Burn`/`BalanceTransfer`/`Transfer` event
/// into a [`ScaledTokenEventType`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
/// as seen by the parser: the emission variants (Mint/Burn/Transfer) + the 6
/// interest-accrual derived variants applied AFTER matching (the parser
/// derives them when classifying the net effect of a `Mint` emitted on a
/// repayment/withdrawal path where interest exceeded the principal).
///
/// This enum lives in the **parser** crate (not the **decoder** crate).
/// The decoder emits raw
/// `Address`/`U256` fields; the parser classifies the event by the emitter
/// `Address` (aToken vs vToken vs GHO-vToken vs GHO-discount-token) + the
/// operation context (interest-exceeds-principal → derived interest variant).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

// ── ScaledTokenEvent ──────────────────────────────────────────────────────

/// The parser-side wrapper around a decoded scaled-token (or plain ERC20
/// Transfer) event. Carries the emitter `Address` (so the apply dispatch
/// glue can re-resolve position ids), the classification [`ScaledTokenEventType`],
/// the user/from/target addresses (decoded by
/// [`degenbot_decoders::aave_event_decoder`]), and the raw `value`/
/// `balance_increase`/`index` fields the matching helpers
/// ([`crate::operations_parser::TransactionOperationsParser`]) use.
///
/// Borrows the source `&Log` so downstream `Operation`s can re-reference the
/// raw event for `transfer_events` / `balance_transfer_events` arrays.
#[derive(Clone, Debug)]
pub struct ScaledTokenEvent<'a> {
    pub log: &'a Log,
    /// Field-by-field decoded event data (the Mint/Burn/BalanceTransfer/
    /// `Erc20Transfer` struct from `degenbot_decoders::aave_event_decoder`).
    pub decoded: ScaledTokenEventData,
    pub event_type: ScaledTokenEventType,
    /// The contract that emitted the event (the aToken/vToken/Discount-token).
    pub token_address: Address,
    /// The user address the event credits/debits (`onBehalfOf` for Mint;
    /// `from` for Burn/BalanceTransfer/Transfer).
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
    /// The logIndex — used by the parser for assigned-index tracking +
    /// chronological ordering.
    pub log_index: u64,
}

/// Field-by-field decoded scaled-token event data (the discriminator for the
/// 4 emission shapes — Mint/Burn/BalanceTransfer/Transfer — re-extracted from
/// the upstream `DecodedAaveEvent` variants). Owned data (no lifetime).
#[derive(Clone, Debug, PartialEq, Eq)]
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

// ── Operation ────────────────────────────────────────────────────────────

/// A single logical operation with complete asset-flow context.
/// `validation_errors` is filled only by the validators that run at the end
/// of [`crate::operations_parser::TransactionOperationsParser::parse`].
///
/// Borrows source logs for the lifetime of the input `&[&Log]` slice.
#[derive(Clone, Debug)]
pub struct Operation<'a> {
    /// The sequential operation id (assigned by `parse()`).
    pub operation_id: u32,
    /// The discriminated operation type.
    pub operation_type: OperationType,
    /// The Pool contract revision at parse-start (read once per tx;
    /// mid-tx `PoolUpdated` config events are the orchestrator's concern).
    pub pool_revision: u32,
    /// The Pool event triggering the operation (raw log); `None` for the
    /// phase-4 post-loop operations (MintToTreasury/DeficitCoverage/
    /// InterestAccrual/Transfer/StkAaveTransfer).
    pub pool_event: Option<&'a Log>,
    /// The matched scaled-token events (the parser grouped these into the
    /// operation).
    pub scaled_events: Vec<ScaledTokenEvent<'a>>,
    /// Supporting aToken/vToken Transfer events (raw logs).
    pub transfer_events: Vec<&'a Log>,
    /// Supporting aToken `BalanceTransfer` events (raw logs).
    pub balance_transfer_events: Vec<&'a Log>,
    /// For `MintToTreasury`: the `MintedToTreasury` event's `amountMinted` field
    /// (raw UNDERLYING amount, regardless of `pool_revision`) — NOT pre-scaled;
    /// the single-conversion rule is pinned by
    /// `mint_to_treasury_dp3_does_not_pre_scale_then_dispatch_single_converts`.
    pub minted_to_treasury_amount: Option<U256>,
    /// For Liquidation: the `LiquidationCall` `debtToCover` field (surfaces
    /// the burn-amount's accuracy edge — the Burn `amount + balance_increase`
    /// is off by 1 wei; the liquidation builder consumes it.
    pub debt_to_cover: Option<U256>,
    pub validation_errors: Vec<String>,
}

impl Operation<'_> {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.validation_errors.is_empty()
    }

    /// Mark every log this operation owns as claimed. The parser claims only
    /// through here, so an operation's ownership has one definition.
    pub fn claim_into(&self, claimed: &mut HashSet<u64>) {
        claimed.extend(self.claimed_log_indices());
    }

    /// Every log index this operation consumed: its Pool anchor, its
    /// scaled-token events, and its transfer reports. The receipt's ownership
    /// ledger claims these, so one operation is the one owner of every log it
    /// reads (ADR-105 D5).
    pub fn claimed_log_indices(&self) -> impl Iterator<Item = u64> + '_ {
        self.pool_event
            .map(log_idx)
            .into_iter()
            .chain(self.scaled_events.iter().map(|ev| ev.log_index))
            .chain(self.transfer_events.iter().map(|log| log_idx(log)))
            .chain(self.balance_transfer_events.iter().map(|log| log_idx(log)))
    }
}

/// Read the `logIndex` from a log; `0` if absent (matches the parser's
/// pre-existing `ev["logIndex"]` access — every log from a real RPC receipt
/// has one, but the field is `Option` on alloy's `Log`).
fn log_idx(log: &Log) -> u64 {
    log.log_index.unwrap_or(0)
}

// ── liquidation-pattern types ─────────────────────────────────────────────
//
// Defined here so the apply dispatch glue can extend them and so the
// parser's `parse()` scaffold (which builds but does not exercise the
// pattern context) stays wired.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LiquidationPattern {
    /// 1 liquidation, 1 burn (standard).
    Single,
    /// N liquidations, 1 combined burn.
    CombinedBurn,
    /// N liquidations, N burns.
    SeparateBurns,
}

///
/// Built per-tx by `crate::transaction_processor::build_liquidation_patterns`;
/// consumed by the liquidation
/// dispatch (`dispatch_liquidation`) to apply the `COMBINED_BURN`
/// aggregated-burn + Mint-skip behavior.
#[derive(Clone, Debug, Default)]
pub struct LiquidationPatternContext {
    /// `(user, debt_v_token)` → detected pattern.
    pub patterns: std::collections::HashMap<(Address, Address), LiquidationPattern>,
    /// `(user, debt_v_token)` → group state.
    pub groups: std::collections::HashMap<(Address, Address), LiquidationGroup>,
    /// Groups already fully processed (`COMBINED_BURN` handling).
    pub processed: std::collections::HashSet<(Address, Address)>,
}

impl LiquidationPatternContext {
    /// Get the detected pattern for a `(user, debt_v_token)`. `None` for a
    /// pair with no liquidation group (the dispatch falls through to the
    /// per-op path).
    #[must_use]
    pub fn get_pattern(&self, user: Address, debt_v_token: Address) -> Option<LiquidationPattern> {
        self.patterns.get(&(user, debt_v_token)).copied()
    }

    /// Get the liquidation group for a `(user, debt_v_token)`.
    #[must_use]
    pub fn get_group(&self, user: Address, debt_v_token: Address) -> Option<&LiquidationGroup> {
        self.groups.get(&(user, debt_v_token))
    }

    /// Whether a group has been fully processed (`COMBINED_BURN`
    /// aggregated-burn has already fired).
    #[must_use]
    pub fn is_processed(&self, user: Address, debt_v_token: Address) -> bool {
        self.processed.contains(&(user, debt_v_token))
    }

    /// Mark a group as fully processed (`COMBINED_BURN` handling).
    pub fn mark_processed(&mut self, user: Address, debt_v_token: Address) {
        self.processed.insert((user, debt_v_token));
    }
}

#[derive(Clone, Debug, Default)]
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
