//! The family-blind door contract vocabulary — the [`super::balance_vector`]
//! pattern (ADR-059 D6: what the families genuinely share lives beside them,
//! never between their projections) applied to the doors themselves. ONE
//! swap-read request, ONE read outcome, ONE observe probe pair, ONE apply
//! contract; a family's design bits ride as ADDITIVE variants of these shapes
//! (the concentrated-liquidity tree's canonical signed request and its rich
//! miss-recovery verdicts, the balance-vector pair reads' scaling-factor
//! override), never as forks of the shared shape.
//!
//! The per-family math, projections, and refusal conventions stay in the
//! family read modules — the doors merely speak this vocabulary at their
//! edges, and the substrate swap-simulation row adapts the outcome onto its
//! own typed contract. The kernels stay monomorphic and family-blind
//! (ADR-059 D6).

use alloy::primitives::{aliases::U112, I256, U256};

use crate::v3_state::{PoolTickCoverage, SimulateSwapError, V3SwapOutcome};

/// The ONE swap-read request vocabulary — EVERY family read variant, keyed by
/// direction (and, for the pair reads, token selection). The token0/1
/// exact-input row is the seat engine's hop universe; the pair rows are the
/// standalone-driver N-token surface; the concentrated-liquidity variant is
/// the CL family's canonical signed request.
#[derive(Clone, Copy, Debug)]
pub enum SwapReadRequest<'a> {
    /// Exact-input swap (on-chain `getAmountOut`); a zero request answers the
    /// legacy `Ok(U256::ZERO)` sentinel (retired by C6, not here).
    ExactIn {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// The input amount (native decimals).
        amount_in: U256,
    },
    /// Exact-output swap — answered only where a family carries the closed
    /// form (the reserve-pair V2 design); every other door refuses it.
    ExactOut {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// The requested output amount (native decimals).
        amount_out: U256,
    },
    /// Exact-input swap across an explicit token pair (the standalone-driver
    /// N-token surface). Overrides carry the caller's view of the balances —
    /// and, additively, the stable family's `MetaStable` scaling-factor rates.
    PairExactIn {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The input amount (native decimals).
        amount_in: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
        /// Replacement scaling factors (caller's view); `None` reads the
        /// identity. The stable family's additive bit — a family whose
        /// scaling factors are registration-immutable refuses a `Some`
        /// rather than guess what the caller meant.
        override_scaling_factors: Option<&'a [U256]>,
    },
    /// Exact-output swap across an explicit token pair (the `GIVEN_OUT` arm).
    /// Overrides carry the caller's view of the balances and, additively, the
    /// stable family's scaling-factor rates.
    PairExactOut {
        /// Input token index.
        idx_in: usize,
        /// Output token index.
        idx_out: usize,
        /// The requested output amount (native decimals).
        amount_out: U256,
        /// Replacement balances (caller's view); `None` reads the slice.
        override_balances: Option<&'a [U256]>,
        /// Replacement scaling factors (caller's view); `None` reads the
        /// identity (see [`SwapReadRequest::PairExactIn`]).
        override_scaling_factors: Option<&'a [U256]>,
    },
    /// The concentrated-liquidity family's canonical request (ADR-037 sign
    /// convention: positive `amount_specified` = exact output, negative =
    /// exact input). The CL design files map it onto their engine
    /// conventions — callers never do.
    ConcentratedLiquidity {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// Signed canonical amount — see the variant-level convention.
        amount_specified: I256,
        /// Price-limit bound; `None` selects the shared CL default.
        sqrt_price_limit: Option<U256>,
    },
}

/// The ONE swap-read outcome vocabulary — the family doors' verdicts, which
/// the substrate swap-simulation row adapts onto its typed `SwapRead`
/// contract. The simple families (reserve-pair, Solidly, balance-vector)
/// answer [`ReadOutcome::Amount`] / [`ReadOutcome::NotComputable`]; the
/// concentrated-liquidity tree's richer miss-recovery verdicts ride as
/// additive variants.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// A completed simple read: the math's `U256` output.
    Amount(U256),
    /// Arithmetic overflow / invariant violation / non-positive amount / a
    /// typed family refusal — the simple doors' only error verdict.
    NotComputable,
    /// A completed CL read: the raw engine outcome (V3-shaped; both CL
    /// designs return it) + the words fetched during miss recovery, in fetch
    /// order.
    Computed {
        /// The engine swap outcome.
        outcome: V3SwapOutcome,
        /// Tick-bitmap words fetched during miss recovery.
        fetched_words: Vec<i32>,
    },
    /// The stateless Pattern-B verdict: the raw compute result, with a
    /// sparse-map miss surfaced as DATA for the caller's own fetch + retry
    /// shell.
    Stateless(Result<V3SwapOutcome, SimulateSwapError>),
    /// Miss recovery ran but the fetch itself failed for `word`.
    FetchFailed {
        /// The word whose recovery failed.
        word: i32,
    },
    /// Miss recovery was impossible or exhausted for `word` (no fetcher
    /// registered, disarmed, or the same word missed twice after a merge).
    FetchExhausted {
        /// The word whose recovery was exhausted.
        word: i32,
    },
    /// The discovery verdict: every missing word on the crossing, in
    /// first-miss order.
    MissingWords(Vec<i32>),
}

impl ReadOutcome {
    /// The simple families' door constructor: a completed read is
    /// [`ReadOutcome::Amount`], every refusal is
    /// [`ReadOutcome::NotComputable`] (the simple doors carry no sparse-map
    /// miss — their slices have no tick words to miss).
    #[must_use]
    pub fn simple(read: Result<U256, SimulateSwapError>) -> Self {
        match read {
            Ok(amount) => Self::Amount(amount),
            Err(_) => Self::NotComputable,
        }
    }

    /// The simple families' verdict as the stateless dispatch's `Result`
    /// contract (the inverse of [`ReadOutcome::simple`]).
    ///
    /// # Errors
    ///
    /// [`SimulateSwapError::NotComputable`] for the refusal verdict — the
    /// simple doors' only error verdict.
    ///
    /// # Panics
    ///
    /// Never for a verdict a simple door produced; the CL-only variants are
    /// unreachable here.
    pub fn amount(self) -> Result<U256, SimulateSwapError> {
        match self {
            Self::Amount(amount) => Ok(amount),
            Self::NotComputable => Err(SimulateSwapError::NotComputable),
            _ => unreachable!("the simple doors answer only Amount/NotComputable"),
        }
    }

    /// The discovery word list (empty for every other variant — call only on
    /// a discovery read).
    #[must_use]
    pub fn missing_words(self) -> Vec<i32> {
        match self {
            Self::MissingWords(words) => words,
            _ => Vec::new(),
        }
    }
}

/// The ONE observe probe vocabulary — the family-local GET probes the
/// observe doors answer over their slices. Each family answers its own
/// probes; a probe a family doesn't carry is a caller bug, and the door
/// refuses to answer it rather than defaulting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observe {
    /// The block of the last state mutation (the family's price clock).
    UpdateBlock,
    /// The per-mutation state nonce (the staleness gate).
    StateNonce,
    /// The token count (the balance vector's arity).
    TokenCount,
    /// Registration coverage (`Tracked` / `Sparse`) — the CL family's probe.
    Coverage,
    /// Whether the design's hook surface can modify swap amounts (V3: always
    /// `false` — no hook concept; V4: the amount-modifying permission bits).
    Hooked,
    /// The tick-data fingerprint `(update_block, tick_count, sum_of_blocks)`
    /// the engine verifier summarizes state with.
    TickFingerprint,
}

/// One observed fact, keyed by the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// The slice's price clock.
    UpdateBlock(u64),
    /// The slice's per-mutation nonce.
    StateNonce(u64),
    /// The slice's balance-vector arity.
    TokenCount(usize),
    /// The slice's registration coverage.
    Coverage(PoolTickCoverage),
    /// Whether the design's hook surface can modify swap amounts.
    Hooked(bool),
    /// The tick-data fingerprint; `Some` for every CL slice (every CL probe
    /// is answerable over the family's slices).
    TickFingerprint(Option<(u64, usize, u64)>),
}

/// One buffered `Sync(uint112, uint112)` event — the reserve-pair slices'
/// shared event. The reserve-pair family's designs (V2, Aerodrome V2
/// volatile) and the Solidly family's designs (Aerodrome V2 stable, Camelot
/// `stable_swap`) land on the same state slices, whose apply bodies are the
/// ADR-017 D3 collapse (full-state [`crate::state_history::V2BlockDelta`]
/// push + reserve overwrite + clock advance) — so one event and one
/// [`FamilyApply`] impl serve both families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncEvent {
    /// The new token0 reserve (on-chain `uint112`).
    pub reserve0: U112,
    /// The new token1 reserve (on-chain `uint112`).
    pub reserve1: U112,
    /// The block the update landed at (advances the price clock).
    pub block_number: u64,
}

/// The ONE apply contract: one buffered family event lands on the family's
/// state slice. The event type is the slice's associated type, so the
/// event/slice pairing is compiler-enforced for every family — a V4 event on
/// a V3 slice (or any cross-family pairing) does not compile, which is the
/// property the former per-family apply doors encoded by hand.
pub trait FamilyApply {
    /// The buffered event this slice accepts.
    type Event;
    /// One buffered event lands on the slice; the journal + clock discipline
    /// lives on the slice itself (ADR-017).
    fn apply_event(&mut self, event: Self::Event);
}
